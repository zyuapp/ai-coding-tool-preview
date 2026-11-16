//! The local server a phone or another computer reaches. It trades pairing codes for tokens and holds
//! one socket per session. Nothing here decides anything about the workspace: a command goes to the
//! window and is acknowledged with what the window said.
//!
//! Sessions outlive their sockets. Every message a session sends is numbered and kept, so a device
//! that drops resumes by naming the last number it saw and is handed the rest; one that fell further
//! behind is handed a fresh snapshot. What a device has run is remembered against the device, so a
//! resent command is never run twice.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_core::domain::mobile::{MobileConnectionState, MobileServerStatus, MobileSessionView, PairedDeviceKind};
use act_core::util::Map as Ordered;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use super::addresses::{health_path, mobile_instance, socket_path, workspace_socket_path, MOBILE_HEALTH_RESPONSE, MOBILE_INSTANCE_HEADER};
use super::messages::{computer_capabilities, is_computer_client_message, is_computer_transfer, is_mobile_client_message, rejected_request, supports_command, supports_query, MOBILE_PROTOCOL_VERSION, OLDEST_PHONE_PROTOCOL_VERSION};
use super::pairing::{EditOutcome, PairingStore, PhonePush};
use super::relay::MobileRelay;
use super::view::apply_mobile_patch;
use super::{now, COMPUTER_TRANSFER_TIMEOUT_MS, MAX_COMPUTER_MESSAGE_BYTES, MAX_PENDING_SOCKETS, MAX_SESSIONS_PER_DEVICE, MOBILE_BUFFER_BYTES, MOBILE_CONGESTED_BYTES, MOBILE_DEAD_AFTER_MS, MOBILE_EVENT_BUFFER, MOBILE_PING_INTERVAL_MS};
use crate::notify::ThreadNotice;
use crate::workspace_patches::{handed_over_patches, handed_over_state, WorkspacePatch};

/// How long a socket that has said nothing is given to name itself.
const AUTH_DEADLINE: Duration = Duration::from_secs(10);
/// How long a dropped session waits to be resumed before its buffer is thrown away.
pub const SESSION_GRACE: Duration = Duration::from_secs(5 * 60);
const MAX_HANDLED_REQUESTS: usize = 256;
const MAX_COMMANDS_IN_FLIGHT: usize = 64;
const MAX_QUERIES_IN_FLIGHT: usize = 8;
/// How long a hung-up socket is given to answer the close handshake before it is cut.
const CLOSE_GRACE: Duration = Duration::from_millis(250);
/// How often a phone behind a congested line is looked at again.
const CATCH_UP: Duration = Duration::from_millis(250);
const PHONE_UNREADABLE: &str = "Your computer could not read that request.";

/// A workspace update for another computer: `{revision, state}` or `{revision, patches}`.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceUpdate {
    State { revision: u64, state: Value },
    Patches { revision: u64, patches: Vec<WorkspacePatch> },
}

impl WorkspaceUpdate {
    /// As another computer is handed it: without the states of the computers this one pairs with.
    pub fn handed_over(self) -> Value {
        match self {
            WorkspaceUpdate::State { revision, state } => json!({ "revision": revision, "state": handed_over_state(state) }),
            WorkspaceUpdate::Patches { revision, patches } => json!({ "revision": revision, "patches": handed_over_patches(patches) }),
        }
    }
}

/// The workspace as another computer reads and drives it: whole, and in the window's own inputs.
pub trait WorkspaceHooks: Send + Sync + 'static {
    /// What this computer calls itself to the computers it hands the workspace to.
    fn name(&self) -> Option<String>;
    fn snapshot(&self) -> WorkspaceUpdate;
    /// Calls `listener` with every update; the returned closure stops it.
    fn subscribe(&self, listener: Arc<dyn Fn(WorkspaceUpdate) + Send + Sync>) -> Box<dyn FnOnce() + Send>;
    /// Runs the inputs one after another; answers the last one's result with the revision it left.
    fn input(&self, inputs: Vec<Value>) -> BoxFuture<'static, Result<Value, String>>;
    fn query(&self, query: Value) -> BoxFuture<'static, Result<Value, String>>;
}

pub struct ServerOptions {
    pub devices: Arc<Mutex<PairingStore>>,
    pub port: u16,
    /// The origins a browser may open a socket from.
    pub allowed_origins: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub relay: Arc<MobileRelay>,
    /// Something changed what settings should say.
    pub on_change: Arc<dyn Fn() + Send + Sync>,
    pub workspace: Option<Arc<dyn WorkspaceHooks>>,
    pub session_grace: Duration,
    /// What this build calls its engines, modes, models and efforts, for a phone not built beside it.
    pub vocabulary: Value,
}

enum Outgoing {
    Text(Arc<str>, Option<Box<dyn FnOnce() + Send>>),
    Close(u16, String),
    Terminate,
}

#[derive(Clone)]
struct Socket {
    id: u64,
    tx: mpsc::UnboundedSender<Outgoing>,
    buffered: Arc<AtomicUsize>,
    open: Arc<AtomicBool>,
}

impl Socket {
    fn write(&self, text: Arc<str>, sent: Option<Box<dyn FnOnce() + Send>>) {
        if self.open.load(Ordering::SeqCst) {
            self.buffered.fetch_add(text.len(), Ordering::SeqCst);
            if let Err(mpsc::error::SendError(Outgoing::Text(text, sent))) = self.tx.send(Outgoing::Text(text, sent)) {
                self.buffered.fetch_sub(text.len(), Ordering::SeqCst);
                if let Some(sent) = sent {
                    sent();
                }
            }
        } else if let Some(sent) = sent {
            sent();
        }
    }

    /// Hangs up, and cuts the line shortly after if the other side ignores the close.
    fn hang_up(&self, code: u16, reason: &str) {
        let _ = self.tx.send(Outgoing::Close(code, reason.to_string()));
    }

    fn terminate(&self) {
        let _ = self.tx.send(Outgoing::Terminate);
    }

    /// A refusal is the last thing a socket hears: it carries no session, so it is numbered zero.
    fn refuse(&self, code: &str, message: &str) {
        self.write(json!({ "kind": "error", "sequence": 0, "code": code, "message": message }).to_string().into(), None);
        self.hang_up(4001, code);
    }

    fn congested(&self) -> bool {
        self.buffered.load(Ordering::SeqCst) > MOBILE_CONGESTED_BYTES
    }
}

struct Session {
    id: String,
    kind: PairedDeviceKind,
    device_id: String,
    device_name: String,
    started_at: i64,
    last_seen_at: i64,
    sequence: u64,
    connection: MobileConnectionState,
    socket: Option<Socket>,
    buffer: VecDeque<(u64, Arc<str>)>,
    buffer_bytes: usize,
    awaiting_snapshot: bool,
    expires_at: Option<i64>,
    queries: usize,
    transfers: HashMap<String, i64>,
    catching_up: bool,
}

/// Per device: every request id it has sent and what came of it, None while one is in flight.
type Handled = Ordered<Option<Value>>;

#[derive(Default)]
struct ServerState {
    sessions: Vec<Session>,
    instance: String,
    handled: HashMap<String, Handled>,
    listening: Option<u16>,
    failure: Option<String>,
    pending: usize,
    view: Option<Value>,
    sockets: HashMap<u64, Socket>,
    next_socket: u64,
    stop: Option<CancellationToken>,
    stop_workspace: Option<Box<dyn FnOnce() + Send>>,
}

impl ServerState {
    fn session(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|session| session.id == id)
    }

    fn forget(&mut self, id: &str) {
        self.sessions.retain(|session| session.id != id);
        if self.sessions.is_empty() {
            self.view = None;
        }
    }

    /// Numbers the message, keeps it for a resume, and writes it to the session's socket.
    fn emit(&mut self, session_id: &str, mut message: Value, sent: Option<Box<dyn FnOnce() + Send>>) {
        let Some(session) = self.session(session_id) else {
            if let Some(sent) = sent {
                sent();
            }
            return;
        };
        session.sequence += 1;
        message["sequence"] = json!(session.sequence);
        let text: Arc<str> = message.to_string().into();
        session.buffer.push_back((session.sequence, text.clone()));
        session.buffer_bytes += text.len();
        while session.buffer.len() > MOBILE_EVENT_BUFFER || (session.buffer_bytes > MOBILE_BUFFER_BYTES && session.buffer.len() > 1) {
            let Some((_, dropped)) = session.buffer.pop_front() else { break };
            session.buffer_bytes -= dropped.len();
        }
        match &session.socket {
            Some(socket) => socket.write(text, sent),
            None => {
                if let Some(sent) = sent {
                    sent();
                }
            }
        }
    }

    /// The session a request's outcome goes to: the one that asked while its line is up, else the
    /// device's newest session with a line.
    fn answering(&self, session_id: &str) -> String {
        let Some(session) = self.sessions.iter().find(|session| session.id == session_id) else { return session_id.to_string() };
        if session.socket.is_some() {
            return session.id.clone();
        }
        self.sessions.iter().filter(|entry| entry.device_id == session.device_id && entry.socket.is_some()).max_by_key(|entry| entry.started_at).map(|entry| entry.id.clone()).unwrap_or_else(|| session.id.clone())
    }

    fn ack(&mut self, session_id: &str, request_id: &str, outcome: &Value) {
        let mut message = json!({ "kind": "ack", "requestId": request_id, "ok": outcome.get("ok").and_then(Value::as_bool).unwrap_or(false) });
        if message["ok"] == json!(false) {
            message["message"] = outcome.get("message").cloned().unwrap_or(json!(""));
        }
        self.emit(session_id, message, None);
    }
}

struct Inner {
    options: ServerOptions,
    state: Mutex<ServerState>,
}

/// One phone bridge server, started and stopped with phone access.
#[derive(Clone)]
pub struct MobileServer {
    inner: Arc<Inner>,
}

fn plain(status: StatusCode, text: &str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(text.to_string())));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    for (name, value) in [("content-type", "text/plain; charset=utf-8"), ("cache-control", "no-store"), ("x-content-type-options", "nosniff"), ("x-frame-options", "DENY"), ("content-security-policy", "frame-ancestors 'none'"), ("referrer-policy", "no-referrer")] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

fn refuse_upgrade(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response.headers_mut().insert("connection", HeaderValue::from_static("close"));
    response
}

fn is_loopback(address: &IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
    }
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).and_then(|value| value.to_str().ok()).map(str::trim).filter(|value| !value.is_empty()).map(str::to_string)
}

/// Who sent a request, for counting wrong pairing codes. Tailscale Serve proxies from the loopback and
/// names the real peer in the forwarded header, which is trusted only from the loopback.
fn source_of(headers: &HeaderMap, remote: SocketAddr) -> String {
    if !is_loopback(&remote.ip()) {
        return remote.ip().to_string();
    }
    header(headers, "x-forwarded-for").and_then(|forwarded| forwarded.split(',').next().map(|first| first.trim().to_string())).filter(|first| !first.is_empty()).unwrap_or_else(|| remote.ip().to_string())
}

fn forwarded_host(headers: &HeaderMap, remote: SocketAddr) -> Option<String> {
    if !is_loopback(&remote.ip()) {
        return header(headers, "host");
    }
    header(headers, "x-forwarded-host").or_else(|| header(headers, "host"))
}

fn forwarded_scheme(headers: &HeaderMap, remote: SocketAddr) -> &'static str {
    if is_loopback(&remote.ip()) && header(headers, "x-forwarded-proto").as_deref() == Some("https") { "https" } else { "http" }
}

/// Whether the session's buffer still reaches back to what the device says it saw.
fn resumable(session: &Session, last_sequence: u64) -> bool {
    if last_sequence > session.sequence {
        return false;
    }
    if last_sequence == session.sequence {
        return true;
    }
    session.buffer.front().is_some_and(|(oldest, _)| *oldest <= last_sequence + 1)
}

fn trim(handled: &mut Handled) {
    while handled.len() > MAX_HANDLED_REQUESTS {
        let Some(index) = handled.values().position(Option::is_some) else { return };
        handled.shift_remove_index(index);
    }
}

/// A whole number as JSON carries it, which a client may have written as a float.
fn count(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_f64).filter(|number| number.is_finite() && *number >= 0.0).map(|number| number as u64).unwrap_or(0)
}

fn outcome_value(outcome: &Result<(), String>) -> Value {
    match outcome {
        Ok(()) => json!({ "ok": true }),
        Err(message) => json!({ "ok": false, "message": message }),
    }
}

/// The phone app's vocabulary, from this build's engines, modes, models, efforts and roles.
pub fn mobile_vocabulary() -> Value {
    use act_core::domain::engine::{built_in_models, capabilities_for, efforts, engine_label, offered_policies, AGENT_EFFORTS, AGENT_ENGINES};
    let catalog = built_in_models();
    let engines: Vec<Value> = AGENT_ENGINES
        .iter()
        .map(|engine| {
            let models: Vec<Value> = catalog
                .models
                .get(engine)
                .map(|models| {
                    models
                        .iter()
                        .map(|spec| {
                            let mut model = json!({ "id": spec.id, "label": spec.label, "description": spec.description, "efforts": spec.efforts.iter().map(|effort| effort.id).collect::<Vec<_>>() });
                            if let Some(provider) = &spec.provider {
                                model["provider"] = json!(provider);
                            }
                            model
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "id": engine,
                "label": engine_label(*engine),
                "fastMode": capabilities_for(*engine).fast_mode,
                "policies": offered_policies(*engine).iter().map(|policy| json!({ "id": policy, "label": policy.label(), "description": policy.description() })).collect::<Vec<_>>(),
                "models": models,
            })
        })
        .collect();
    json!({
        "engines": engines,
        "efforts": efforts(&AGENT_EFFORTS),
        "roles": act_core::domain::thread_role::THREAD_ROLES.iter().map(|role| json!({ "id": role, "label": role.label() })).collect::<Vec<_>>(),
    })
}

impl MobileServer {
    pub fn new(options: ServerOptions) -> Self {
        Self { inner: Arc::new(Inner { options, state: Mutex::new(ServerState { instance: uuid::Uuid::new_v4().to_string(), ..ServerState::default() }) }) }
    }

    pub fn port(&self) -> Option<u16> {
        self.inner.state.lock().unwrap().listening
    }

    pub fn status(&self) -> MobileServerStatus {
        let state = self.inner.state.lock().unwrap();
        if state.failure.is_some() {
            MobileServerStatus::Error
        } else if state.listening.is_none() {
            MobileServerStatus::Off
        } else {
            MobileServerStatus::Listening
        }
    }

    pub fn error(&self) -> Option<String> {
        self.inner.state.lock().unwrap().failure.clone()
    }

    pub fn session_views(&self) -> Vec<MobileSessionView> {
        self.inner
            .state
            .lock()
            .unwrap()
            .sessions
            .iter()
            .map(|session| MobileSessionView { id: session.id.clone(), kind: session.kind, device_id: session.device_id.clone(), started_at: session.started_at, last_seen_at: session.last_seen_at, sequence: session.sequence as i64, connection: session.connection, device_name: session.device_name.clone() })
            .collect()
    }

    /// Binds `host` on the configured port, or any free port when that one is taken.
    pub async fn start(&self, host: &str) -> Result<(), String> {
        if self.inner.state.lock().unwrap().stop.is_some() {
            return Ok(());
        }
        let ip: IpAddr = host.parse().map_err(|_| format!("{host} is not an address."))?;
        let listener = match tokio::net::TcpListener::bind(SocketAddr::new(ip, self.inner.options.port)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && self.inner.options.port != 0 => tokio::net::TcpListener::bind(SocketAddr::new(ip, 0)).await.map_err(|error| error.to_string())?,
            Err(error) => {
                self.inner.state.lock().unwrap().failure = Some(error.to_string());
                return Err(error.to_string());
            }
        };
        let port = listener.local_addr().map_err(|error| error.to_string())?.port();
        let stop = CancellationToken::new();
        {
            let mut state = self.inner.state.lock().unwrap();
            state.failure = None;
            state.instance = uuid::Uuid::new_v4().to_string();
            state.listening = Some(port);
            state.stop = Some(stop.clone());
        }
        let inner = self.inner.clone();
        let accepting = stop.clone();
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    _ = accepting.cancelled() => break,
                };
                let Ok((stream, remote)) = accepted else { continue };
                let inner = inner.clone();
                let closing = accepting.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                        let inner = inner.clone();
                        async move { Ok::<_, Infallible>(inner.serve(request, remote)) }
                    });
                    let connection = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).with_upgrades();
                    tokio::select! {
                        _ = connection => {}
                        _ = closing.cancelled() => {}
                    }
                });
            }
        });
        let ticking = stop.clone();
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(MOBILE_PING_INTERVAL_MS as u64));
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = interval.tick() => inner.tick(),
                    _ = ticking.cancelled() => break,
                }
            }
        });
        if let Some(workspace) = self.inner.options.workspace.clone() {
            let inner = Arc::downgrade(&self.inner);
            let stop_workspace = workspace.subscribe(Arc::new(move |update: WorkspaceUpdate| {
                let Some(inner) = inner.upgrade() else { return };
                let handed = update.handed_over();
                let mut state = inner.state.lock().unwrap();
                let computers: Vec<String> = state.sessions.iter().filter(|session| session.kind == PairedDeviceKind::Computer && !session.awaiting_snapshot).map(|session| session.id.clone()).collect();
                for id in computers {
                    state.emit(&id, json!({ "kind": "workspace", "update": handed }), None);
                }
            }));
            self.inner.state.lock().unwrap().stop_workspace = Some(stop_workspace);
        }
        Ok(())
    }

    pub async fn stop(&self) {
        let (stop, stop_workspace, sockets) = {
            let mut state = self.inner.state.lock().unwrap();
            let sockets: Vec<Socket> = state.sockets.drain().map(|(_, socket)| socket).collect();
            state.sessions.clear();
            state.handled.clear();
            state.view = None;
            state.listening = None;
            (state.stop.take(), state.stop_workspace.take(), sockets)
        };
        if let Some(stop_workspace) = stop_workspace {
            stop_workspace();
        }
        for socket in sockets {
            socket.hang_up(1001, "The bridge was turned off.");
        }
        if let Some(stop) = stop {
            stop.cancel();
        }
    }

    /// What every phone should see now: `{kind: "snapshot", view}` or `{kind: "patch", patch}`.
    pub fn publish(&self, update: &Value) {
        self.inner.publish(update);
    }

    /// A notice this workspace raised, handed to every computer on the line.
    pub fn notice(&self, notice: &ThreadNotice) {
        let mut state = self.inner.state.lock().unwrap();
        let ids: Vec<String> = state.sessions.iter().filter(|session| session.kind == PairedDeviceKind::Computer && !session.awaiting_snapshot).map(|session| session.id.clone()).collect();
        for id in ids {
            state.emit(&id, json!({ "kind": "notice", "notice": { "taskId": notice.task_id, "title": notice.title, "headline": notice.headline } }), None);
        }
    }

    /// What this computer now calls itself, told to every computer on the line.
    pub fn announce_name(&self) {
        let ids: Vec<String> = self.inner.state.lock().unwrap().sessions.iter().map(|session| session.id.clone()).collect();
        for id in ids {
            self.inner.send_name(&id);
        }
    }

    /// A revoked device loses its sessions with its token.
    pub fn drop_device(&self, device_id: &str) {
        let mut state = self.inner.state.lock().unwrap();
        let dropped: Vec<(String, PairedDeviceKind, Option<Socket>)> = state.sessions.iter().filter(|session| session.device_id == device_id).map(|session| (session.id.clone(), session.kind, session.socket.clone())).collect();
        for (id, kind, socket) in dropped {
            state.forget(&id);
            if let Some(socket) = socket {
                socket.hang_up(4003, if kind == PairedDeviceKind::Computer { "This computer was unpaired." } else { "This phone was unpaired." });
            }
        }
        state.handled.remove(device_id);
    }
}

impl Inner {
    fn serve(self: &Arc<Self>, mut request: Request<Incoming>, remote: SocketAddr) -> Response<Full<Bytes>> {
        let path = request.uri().path().to_string();
        let upgrade = request.headers().get("upgrade").and_then(|value| value.to_str().ok()).is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
        if upgrade {
            let kind = if path == socket_path() {
                Some(PairedDeviceKind::Phone)
            } else if path == workspace_socket_path() && self.options.workspace.is_some() {
                Some(PairedDeviceKind::Computer)
            } else {
                None
            };
            let Some(kind) = kind else { return refuse_upgrade(StatusCode::BAD_REQUEST) };
            let Some(key) = request.headers().get("sec-websocket-key").map(|key| key.as_bytes().to_vec()) else { return refuse_upgrade(StatusCode::BAD_REQUEST) };
            if !self.origin_allowed(request.headers(), remote) {
                return refuse_upgrade(StatusCode::FORBIDDEN);
            }
            if self.state.lock().unwrap().pending >= MAX_PENDING_SOCKETS {
                return refuse_upgrade(StatusCode::SERVICE_UNAVAILABLE);
            }
            let source = source_of(request.headers(), remote);
            let upgrading = hyper::upgrade::on(&mut request);
            let inner = self.clone();
            tokio::spawn(async move {
                let Ok(upgraded) = upgrading.await else { return };
                let config = WebSocketConfig::default().max_message_size(Some(MAX_COMPUTER_MESSAGE_BYTES)).max_frame_size(Some(MAX_COMPUTER_MESSAGE_BYTES));
                let stream = tokio_tungstenite::WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config)).await;
                inner.accept(stream, source, kind).await;
            });
            let mut response = Response::new(Full::new(Bytes::new()));
            *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            let headers = response.headers_mut();
            headers.insert("connection", HeaderValue::from_static("Upgrade"));
            headers.insert("upgrade", HeaderValue::from_static("websocket"));
            if let Ok(accept) = HeaderValue::from_str(&tokio_tungstenite::tungstenite::handshake::derive_accept_key(&key)) {
                headers.insert("sec-websocket-accept", accept);
            }
            return response;
        }
        if request.method() != http::Method::GET {
            return plain(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
        }
        if path == socket_path() {
            return plain(StatusCode::BAD_REQUEST, "This address is a WebSocket.");
        }
        if path == health_path() {
            let mut response = plain(StatusCode::OK, MOBILE_HEALTH_RESPONSE);
            if let Ok(instance) = HeaderValue::from_str(mobile_instance()) {
                response.headers_mut().insert(MOBILE_INSTANCE_HEADER, instance);
            }
            return response;
        }
        plain(StatusCode::NOT_FOUND, "Not found")
    }

    /// A browser may open a socket only from one of this server's own addresses; a request with no
    /// origin is not one a browser made, and is left to the token to judge.
    fn origin_allowed(&self, headers: &HeaderMap, remote: SocketAddr) -> bool {
        let Some(origin) = headers.get("origin").and_then(|value| value.to_str().ok()) else { return true };
        if (self.options.allowed_origins)().iter().any(|allowed| allowed == origin) {
            return true;
        }
        forwarded_host(headers, remote).is_some_and(|host| origin == format!("{}://{host}", forwarded_scheme(headers, remote)))
    }

    async fn accept<S>(self: Arc<Self>, stream: tokio_tungstenite::WebSocketStream<S>, source: String, kind: PairedDeviceKind)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut sink, mut reader) = stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Outgoing>();
        let ended = CancellationToken::new();
        let socket = {
            let mut state = self.state.lock().unwrap();
            state.next_socket += 1;
            state.pending += 1;
            let socket = Socket { id: state.next_socket, tx, buffered: Arc::new(AtomicUsize::new(0)), open: Arc::new(AtomicBool::new(true)) };
            state.sockets.insert(socket.id, socket.clone());
            socket
        };
        let writer_socket = socket.clone();
        let writer_ended = ended.clone();
        tokio::spawn(async move {
            while let Some(outgoing) = rx.recv().await {
                match outgoing {
                    Outgoing::Text(text, sent) => {
                        let length = text.len();
                        let result = sink.send(Message::text(text.to_string())).await;
                        writer_socket.buffered.fetch_sub(length, Ordering::SeqCst);
                        if let Some(sent) = sent {
                            sent();
                        }
                        if result.is_err() {
                            break;
                        }
                    }
                    Outgoing::Close(code, reason) => {
                        writer_socket.open.store(false, Ordering::SeqCst);
                        let _ = tokio::time::timeout(CLOSE_GRACE, sink.send(Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.into() })))).await;
                        tokio::time::sleep(CLOSE_GRACE).await;
                        break;
                    }
                    Outgoing::Terminate => break,
                }
            }
            writer_socket.open.store(false, Ordering::SeqCst);
            writer_ended.cancel();
            // Whatever was still queued is never written; its senders hear it went nowhere.
            rx.close();
            while let Ok(outgoing) = rx.try_recv() {
                if let Outgoing::Text(text, Some(sent)) = outgoing {
                    writer_socket.buffered.fetch_sub(text.len(), Ordering::SeqCst);
                    sent();
                }
            }
        });
        let mut session: Option<String> = None;
        let deadline = tokio::time::sleep(AUTH_DEADLINE);
        tokio::pin!(deadline);
        loop {
            let frame = tokio::select! {
                frame = reader.next() => frame,
                _ = ended.cancelled() => break,
                _ = &mut deadline, if session.is_none() => {
                    socket.hang_up(4008, "This phone never said who it was.");
                    deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(3600));
                    continue;
                }
            };
            let text = match frame {
                Some(Ok(Message::Text(text))) => text.to_string(),
                Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => continue,
            };
            let named = session.is_some();
            self.on_message(&socket, &source, kind, &text, &mut session);
            if !named && session.is_some() {
                let mut state = self.state.lock().unwrap();
                state.pending = state.pending.saturating_sub(1);
            }
        }
        socket.open.store(false, Ordering::SeqCst);
        socket.terminate();
        let mut state = self.state.lock().unwrap();
        state.sockets.remove(&socket.id);
        if session.is_none() {
            state.pending = state.pending.saturating_sub(1);
        }
        drop(state);
        if let Some(id) = session {
            self.socket_closed(&id, socket.id);
        }
    }

    fn socket_closed(self: &Arc<Self>, session_id: &str, socket_id: u64) {
        let computer = {
            let mut state = self.state.lock().unwrap();
            let Some(session) = state.session(session_id) else { return };
            if session.socket.as_ref().map(|socket| socket.id) != Some(socket_id) {
                return;
            }
            session.socket = None;
            session.transfers.clear();
            session.connection = MobileConnectionState::Offline;
            session.expires_at = Some(now() + self.options.session_grace.as_millis() as i64);
            session.kind == PairedDeviceKind::Computer
        };
        if computer {
            if let Some(workspace) = self.options.workspace.clone() {
                tokio::spawn(async move {
                    let _ = workspace.input(vec![json!({ "type": "view.set-focused", "focused": false })]).await;
                });
            }
        }
        (self.options.on_change)();
    }

    fn on_message(self: &Arc<Self>, socket: &Socket, source: &str, kind: PairedDeviceKind, text: &str, session: &mut Option<String>) {
        let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let valid = match kind {
            PairedDeviceKind::Computer => is_computer_client_message(&parsed),
            PairedDeviceKind::Phone => is_mobile_client_message(&parsed),
        };
        let current = session.as_deref().filter(|id| self.state.lock().unwrap().sessions.iter().any(|entry| entry.id == *id)).map(str::to_string);
        if !valid {
            if let Some(id) = &current {
                let mut state = self.state.lock().unwrap();
                match kind {
                    PairedDeviceKind::Computer => {
                        if let Some((rejected, request_id)) = rejected_request(&parsed, ["input", "query"]) {
                            if let Some(held) = state.session(id) {
                                held.transfers.remove(&request_id);
                            }
                            let message = if rejected == "input" {
                                json!({ "kind": "result", "requestId": request_id, "result": { "ok": false, "message": act_core::validation::REMOTE_UNSUPPORTED, "revision": 0 } })
                            } else {
                                json!({ "kind": "answer", "requestId": request_id, "ok": false, "message": act_core::validation::REMOTE_UNSUPPORTED })
                            };
                            return state.emit(id, message, None);
                        }
                    }
                    PairedDeviceKind::Phone => {
                        if let Some((rejected, request_id)) = rejected_request(&parsed, ["command", "query"]) {
                            if rejected == "command" {
                                return state.ack(id, &request_id, &json!({ "ok": false, "message": PHONE_UNREADABLE }));
                            }
                            return state.emit(id, json!({ "kind": "answer", "requestId": request_id, "ok": false, "message": PHONE_UNREADABLE }), None);
                        }
                    }
                }
            }
            return socket.refuse(
                "unreadable",
                if kind == PairedDeviceKind::Computer { "The remote computer could not read this request. Updating AI Coding Tool on the remote computer may help." } else { "That message could not be read." },
            );
        }
        if session.is_some() {
            let Some(id) = current else { return };
            if let Some(held) = self.state.lock().unwrap().session(&id) {
                held.last_seen_at = now();
            }
            return match kind {
                PairedDeviceKind::Computer => self.on_computer_message(&id, parsed),
                PairedDeviceKind::Phone => self.on_phone_message(&id, parsed),
            };
        }
        let opened = match parsed.get("kind").and_then(Value::as_str) {
            Some("pair") => self.on_pair(socket, &parsed, source, kind),
            Some("resume") => self.on_resume(socket, &parsed, kind),
            _ => None,
        };
        if let Some(opened) = opened {
            *session = Some(opened);
            (self.options.on_change)();
        }
    }

    /// Computers connect best effort across versions; a phone app and this computer must read each other.
    fn version_accepted(request: &Value, kind: PairedDeviceKind, socket: &Socket) -> bool {
        if kind == PairedDeviceKind::Computer {
            return true;
        }
        if count(request.get("version")) < OLDEST_PHONE_PROTOCOL_VERSION {
            socket.refuse("version", "This app is out of date for your computer. Update the app to carry on.");
            return false;
        }
        if MOBILE_PROTOCOL_VERSION < count(request.get("oldestComputer")) {
            socket.refuse("version", "AI Coding Tool on your computer is out of date for this app. Update it on your computer to carry on.");
            return false;
        }
        true
    }

    fn open_session(&self, state: &mut ServerState, kind: PairedDeviceKind, device_id: &str, device_name: &str, at: i64) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        state.sessions.push(Session {
            id: id.clone(),
            kind,
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            started_at: at,
            last_seen_at: at,
            sequence: 0,
            connection: MobileConnectionState::Live,
            socket: None,
            buffer: VecDeque::new(),
            buffer_bytes: 0,
            awaiting_snapshot: true,
            expires_at: None,
            queries: 0,
            transfers: HashMap::new(),
            catching_up: false,
        });
        state.handled.entry(device_id.to_string()).or_default();
        let held: Vec<(String, Option<Socket>, PairedDeviceKind)> = state.sessions.iter().filter(|session| session.device_id == device_id).map(|session| (session.id.clone(), session.socket.clone(), session.kind)).collect();
        let stale = held.len().saturating_sub(MAX_SESSIONS_PER_DEVICE);
        for (stale_id, socket, _) in &held[..stale] {
            state.forget(stale_id);
            if let Some(socket) = socket {
                socket.hang_up(4002, "This phone connected again.");
            }
        }
        // A phone that opens a session has given up on the ones it dropped.
        if kind == PairedDeviceKind::Phone {
            for (dropped_id, socket, dropped_kind) in &held {
                if *dropped_id != id && *dropped_kind == PairedDeviceKind::Phone && socket.is_none() {
                    state.forget(dropped_id);
                }
            }
        }
        id
    }

    /// One socket per session: a device that opens a second one takes the session, and the first is closed.
    fn attach(&self, state: &mut ServerState, session_id: &str, socket: &Socket) {
        let Some(session) = state.session(session_id) else { return };
        if let Some(previous) = session.socket.as_ref().filter(|previous| previous.id != socket.id) {
            previous.hang_up(4002, "This phone connected again.");
        }
        session.socket = Some(socket.clone());
        session.transfers.clear();
        session.connection = MobileConnectionState::Live;
        session.expires_at = None;
        session.last_seen_at = now();
    }

    fn send_capabilities(&self, state: &mut ServerState, session_id: &str) {
        let instance = state.instance.clone();
        if state.session(session_id).is_some_and(|session| session.kind == PairedDeviceKind::Computer) {
            state.emit(session_id, json!({ "kind": "capabilities", "capabilities": computer_capabilities(), "instance": instance }), None);
        }
    }

    fn send_name(&self, session_id: &str) {
        let Some(name) = self.options.workspace.as_ref().and_then(|workspace| workspace.name()) else { return };
        let mut state = self.state.lock().unwrap();
        if state.session(session_id).is_some_and(|session| session.kind == PairedDeviceKind::Computer) {
            state.emit(session_id, json!({ "kind": "name", "name": name }), None);
        }
    }

    fn on_pair(self: &Arc<Self>, socket: &Socket, request: &Value, source: &str, kind: PairedDeviceKind) -> Option<String> {
        if !Self::version_accepted(request, kind, socket) || !socket.open.load(Ordering::SeqCst) {
            return None;
        }
        let code = request.get("code").and_then(Value::as_str).unwrap_or("");
        let device_name = request.get("deviceName").and_then(Value::as_str).unwrap_or("");
        let outcome = self.options.devices.lock().unwrap().redeem(code, device_name, source, now(), kind);
        let (device, token) = match outcome {
            Ok(paired) => paired,
            Err(refusal) => {
                socket.refuse(refusal.code, &refusal.message);
                return None;
            }
        };
        let id = {
            let mut state = self.state.lock().unwrap();
            let id = self.open_session(&mut state, kind, &device.id, &device.name, now());
            self.attach(&mut state, &id, socket);
            state.emit(&id, json!({ "kind": "paired", "deviceId": device.id, "deviceName": device.name, "token": token }), None);
            if !socket.open.load(Ordering::SeqCst) {
                state.forget(&id);
                drop(state);
                self.options.devices.lock().unwrap().revoke(&device.id);
                return None;
            }
            self.send_capabilities(&mut state, &id);
            id
        };
        self.send_name(&id);
        self.spawn_snapshot(id.clone());
        Some(id)
    }

    /// Device tokens are 256 bits, so an unknown one is not counted against anyone.
    fn on_resume(self: &Arc<Self>, socket: &Socket, request: &Value, kind: PairedDeviceKind) -> Option<String> {
        if !Self::version_accepted(request, kind, socket) {
            return None;
        }
        let at = now();
        let token = request.get("token").and_then(Value::as_str).unwrap_or("");
        let (device, unreadable) = {
            let mut devices = self.options.devices.lock().unwrap();
            let device = devices.authenticate(token, at);
            (device, devices.unreadable())
        };
        if device.is_none() && unreadable {
            socket.refuse("internal", "This computer could not read its paired devices. Try again in a moment.");
            return None;
        }
        let Some(device) = device.filter(|device| device.kind == kind) else {
            socket.refuse("unauthorized", if kind == PairedDeviceKind::Computer { "This computer is not paired with the other one." } else { "This phone is not paired with this computer." });
            return None;
        };
        self.options.devices.lock().unwrap().mark_seen(&device.id, at);
        let last_sequence = count(request.get("lastSequence"));
        let wanted = request.get("sessionId").and_then(Value::as_str).map(str::to_string);
        let mut state = self.state.lock().unwrap();
        let resumed = wanted.filter(|wanted| state.sessions.iter().any(|session| session.id == *wanted && session.device_id == device.id && resumable(session, last_sequence)));
        if let Some(id) = resumed {
            self.attach(&mut state, &id, socket);
            if let Some(session) = state.session(&id) {
                for (sequence, text) in &session.buffer {
                    if *sequence > last_sequence {
                        socket.write(text.clone(), None);
                    }
                }
            }
            state.emit(&id, json!({ "kind": "ping", "at": at }), None);
            self.send_capabilities(&mut state, &id);
            drop(state);
            self.send_name(&id);
            return Some(id);
        }
        let id = self.open_session(&mut state, kind, &device.id, &device.name, at);
        self.attach(&mut state, &id, socket);
        self.send_capabilities(&mut state, &id);
        drop(state);
        self.send_name(&id);
        self.spawn_snapshot(id.clone());
        Some(id)
    }

    fn snapshot_message(&self, state: &ServerState, session_id: &str, view: &Value) -> Value {
        let mut message = json!({ "kind": "snapshot", "sessionId": session_id, "view": view, "instance": state.instance, "vocabulary": self.options.vocabulary, "push": true });
        if let Some(name) = self.options.workspace.as_ref().and_then(|workspace| workspace.name()) {
            message["computer"] = json!(name);
        }
        message
    }

    fn spawn_snapshot(self: &Arc<Self>, session_id: String) {
        let inner = self.clone();
        tokio::spawn(async move { inner.send_snapshot(&session_id).await });
    }

    async fn send_snapshot(self: &Arc<Self>, session_id: &str) {
        let kind = self.state.lock().unwrap().session(session_id).map(|session| session.kind);
        let Some(kind) = kind else { return };
        let result: Result<(), String> = async {
            if kind == PairedDeviceKind::Computer {
                let workspace = self.options.workspace.clone().ok_or("This computer takes no other computers.")?;
                let update = workspace.snapshot().handed_over();
                let mut state = self.state.lock().unwrap();
                if let Some(session) = state.session(session_id) {
                    session.awaiting_snapshot = false;
                }
                state.emit(session_id, json!({ "kind": "workspace", "sessionId": session_id, "update": update }), None);
                return Ok(());
            }
            let held = self.state.lock().unwrap().view.clone();
            let view = match held {
                Some(view) => view,
                None => {
                    let view = self.options.relay.snapshot(session_id).await?;
                    let mut state = self.state.lock().unwrap();
                    if !state.session(session_id).is_some_and(|session| session.awaiting_snapshot) {
                        return Ok(());
                    }
                    state.view = Some(view.clone());
                    view
                }
            };
            let mut state = self.state.lock().unwrap();
            let Some(session) = state.session(session_id) else { return Ok(()) };
            session.awaiting_snapshot = false;
            let message = self.snapshot_message(&state, session_id, &view);
            state.emit(session_id, message, None);
            Ok(())
        }
        .await;
        if let Err(message) = result {
            let mut state = self.state.lock().unwrap();
            state.emit(session_id, json!({ "kind": "error", "code": "internal", "message": message }), None);
            let socket = state.session(session_id).and_then(|session| session.socket.clone());
            state.forget(session_id);
            drop(state);
            if let Some(socket) = socket {
                socket.hang_up(1011, if kind == PairedDeviceKind::Computer { &message } else { "The view could not be read." });
            }
            (self.options.on_change)();
        }
    }

    fn publish(self: &Arc<Self>, update: &Value) {
        let snapshot = update.get("kind").and_then(Value::as_str) == Some("snapshot");
        let mut behind = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            state.view = if snapshot { update.get("view").cloned() } else { state.view.as_ref().and_then(|view| update.get("patch").and_then(|patch| apply_mobile_patch(view, patch))) };
            let phones: Vec<String> = state.sessions.iter().filter(|session| session.kind == PairedDeviceKind::Phone).map(|session| session.id.clone()).collect();
            for id in phones {
                let Some(session) = state.session(&id) else { continue };
                if snapshot {
                    session.awaiting_snapshot = false;
                    let message = self.snapshot_message(&state, &id, update.get("view").unwrap_or(&Value::Null));
                    state.emit(&id, message, None);
                } else if !session.awaiting_snapshot {
                    if session.socket.as_ref().is_some_and(Socket::congested) {
                        behind.push(id);
                    } else {
                        state.emit(&id, json!({ "kind": "patch", "patch": update.get("patch").cloned().unwrap_or(Value::Null) }), None);
                    }
                }
            }
        }
        for id in behind {
            self.fall_behind(id);
        }
    }

    /// A phone whose socket still holds what it was handed is handed no more patches; it is owed the
    /// view whole once its line drains.
    fn fall_behind(self: &Arc<Self>, session_id: String) {
        {
            let mut state = self.state.lock().unwrap();
            let Some(session) = state.session(&session_id) else { return };
            session.awaiting_snapshot = true;
            if session.catching_up {
                return;
            }
            session.catching_up = true;
        }
        let inner = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CATCH_UP).await;
            let congested = {
                let mut state = inner.state.lock().unwrap();
                let Some(session) = state.session(&session_id) else { return };
                session.catching_up = false;
                if !session.awaiting_snapshot {
                    return;
                }
                session.socket.as_ref().is_some_and(Socket::congested)
            };
            if congested {
                inner.fall_behind(session_id);
            } else {
                inner.send_snapshot(&session_id).await;
            }
        });
    }

    fn on_phone_message(self: &Arc<Self>, session_id: &str, message: Value) {
        let kind = message.get("kind").and_then(Value::as_str).unwrap_or("");
        match kind {
            "query" => return self.on_phone_query(session_id, message),
            "ping" => return self.state.lock().unwrap().emit(session_id, json!({ "kind": "ping", "at": now() }), None),
            "push-token" => {
                let device = self.state.lock().unwrap().session(session_id).map(|session| session.device_id.clone());
                if let Some(device) = device {
                    let push = message.get("push").filter(|push| !push.is_null()).and_then(|push| serde_json::from_value::<PhonePush>(push.clone()).ok());
                    self.options.devices.lock().unwrap().set_push(&device, push);
                }
                return;
            }
            "command" => {}
            _ => return,
        }
        let request_id = message.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
        let command = message.get("command").cloned().unwrap_or(Value::Null);
        let edit = command.get("type").and_then(Value::as_str) == Some("task.edit-queued");
        let mut state = self.state.lock().unwrap();
        let Some(device_id) = state.session(session_id).map(|session| session.device_id.clone()) else { return };
        let stored = if edit && !state.handled.get(&device_id).is_some_and(|handled| handled.contains_key(&request_id)) {
            self.options.devices.lock().unwrap().edit_outcome(&device_id, &request_id).map(|outcome| match outcome {
                EditOutcome::Ok => json!({ "ok": true }),
                EditOutcome::Failed(message) => json!({ "ok": false, "message": message }),
            })
        } else {
            None
        };
        let Some(handled) = state.handled.get_mut(&device_id) else { return };
        if let Some(stored) = stored {
            handled.insert(request_id.clone(), Some(stored));
        }
        if let Some(settled) = handled.get(&request_id) {
            if let Some(settled) = settled.clone() {
                state.ack(session_id, &request_id, &settled);
            }
            return;
        }
        if handled.values().filter(|outcome| outcome.is_none()).count() >= MAX_COMMANDS_IN_FLIGHT {
            return state.ack(session_id, &request_id, &json!({ "ok": false, "message": "This phone has too many commands still waiting on the computer." }));
        }
        handled.insert(request_id.clone(), None);
        trim(handled);
        drop(state);
        let inner = self.clone();
        let session_id = session_id.to_string();
        tokio::spawn(async move {
            let outcome = inner.options.relay.command(&session_id, command).await;
            if edit {
                let stored = match &outcome {
                    Ok(()) => EditOutcome::Ok,
                    Err(message) => EditOutcome::Failed(message.clone()),
                };
                inner.options.devices.lock().unwrap().remember_edit(&device_id, &request_id, stored);
            }
            let outcome = outcome_value(&outcome);
            let mut state = inner.state.lock().unwrap();
            if let Some(handled) = state.handled.get_mut(&device_id) {
                if handled.contains_key(&request_id) {
                    handled.insert(request_id.clone(), Some(outcome.clone()));
                }
            }
            let answering = state.answering(&session_id);
            state.ack(&answering, &request_id, &outcome);
        });
    }

    /// A read is answered once and never remembered; too many at once are refused rather than queued.
    fn on_phone_query(self: &Arc<Self>, session_id: &str, message: Value) {
        let request_id = message.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
        {
            let mut state = self.state.lock().unwrap();
            let Some(session) = state.session(session_id) else { return };
            if session.queries >= MAX_QUERIES_IN_FLIGHT {
                return state.emit(session_id, json!({ "kind": "answer", "requestId": request_id, "ok": false, "message": "This phone is asking for too much at once." }), None);
            }
            session.queries += 1;
        }
        let inner = self.clone();
        let session_id = session_id.to_string();
        let query = message.get("query").cloned().unwrap_or(Value::Null);
        tokio::spawn(async move {
            let answer = match inner.options.relay.query(&session_id, query).await {
                Ok(result) => json!({ "kind": "answer", "requestId": request_id, "ok": true, "result": result }),
                Err(message) => json!({ "kind": "answer", "requestId": request_id, "ok": false, "message": message }),
            };
            let mut state = inner.state.lock().unwrap();
            state.emit(&session_id, answer, None);
            if let Some(session) = state.session(&session_id) {
                session.queries = session.queries.saturating_sub(1);
            }
        });
    }

    /// Another computer's inputs run one after another and are answered once, with the last one's
    /// result. A request already run is answered from what it decided.
    fn on_computer_message(self: &Arc<Self>, session_id: &str, message: Value) {
        let Some(workspace) = self.options.workspace.clone() else { return };
        let Some(fields) = message.as_object() else { return };
        let kind = fields.get("kind").and_then(Value::as_str).unwrap_or("");
        if matches!(kind, "pair" | "resume" | "pong") {
            return;
        }
        let request_id = fields.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
        let capabilities = computer_capabilities();
        let mut state = self.state.lock().unwrap();
        let Some(session) = state.session(session_id) else { return };
        if kind == "input" && fields.get("inputs").and_then(Value::as_array).is_some_and(|inputs| inputs.iter().any(|input| !supports_command(Some(&capabilities), input))) {
            session.transfers.remove(&request_id);
            return state.emit(session_id, json!({ "kind": "result", "requestId": request_id, "result": { "ok": false, "message": act_core::validation::REMOTE_UNSUPPORTED, "revision": 0 } }), None);
        }
        if kind == "query" && !fields.get("query").is_some_and(|query| supports_query(Some(&capabilities), query)) {
            session.transfers.remove(&request_id);
            return state.emit(session_id, json!({ "kind": "answer", "requestId": request_id, "ok": false, "message": act_core::validation::REMOTE_UNSUPPORTED }), None);
        }
        if kind == "transfer" {
            if !session.transfers.contains_key(&request_id) && session.transfers.len() < MAX_COMMANDS_IN_FLIGHT {
                session.transfers.insert(request_id, now() + COMPUTER_TRANSFER_TIMEOUT_MS);
            }
            return;
        }
        let transferring = is_computer_transfer(fields);
        if !transferring {
            session.transfers.remove(&request_id);
        } else if !session.transfers.contains_key(&request_id) {
            session.transfers.insert(request_id.clone(), now() + COMPUTER_TRANSFER_TIMEOUT_MS);
        }
        let sent = {
            let inner = Arc::downgrade(self);
            let session_id = session_id.to_string();
            let request_id = request_id.clone();
            move || -> Box<dyn FnOnce() + Send> {
                Box::new(move || {
                    // Written with the state held, so the transfer is let go once it is free.
                    tokio::spawn(async move {
                        if let Some(inner) = inner.upgrade() {
                            if let Some(session) = inner.state.lock().unwrap().session(&session_id) {
                                session.transfers.remove(&request_id);
                            }
                        }
                    });
                })
            }
        };
        if kind == "query" {
            if session.queries >= MAX_QUERIES_IN_FLIGHT {
                return state.emit(session_id, json!({ "kind": "answer", "requestId": request_id, "ok": false, "message": "That computer is asking for too much at once." }), Some(sent()));
            }
            session.queries += 1;
            drop(state);
            let inner = self.clone();
            let session_id = session_id.to_string();
            let query = fields.get("query").cloned().unwrap_or(Value::Null);
            tokio::spawn(async move {
                let answer = match workspace.query(query).await {
                    Ok(result) => json!({ "kind": "answer", "requestId": request_id, "ok": true, "result": result }),
                    Err(message) => json!({ "kind": "answer", "requestId": request_id, "ok": false, "message": message }),
                };
                let mut state = inner.state.lock().unwrap();
                let answering = state.answering(&session_id);
                state.emit(&answering, answer, Some(sent()));
                if let Some(session) = state.session(&session_id) {
                    session.queries = session.queries.saturating_sub(1);
                }
            });
            return;
        }
        let device_id = session.device_id.clone();
        let Some(handled) = state.handled.get_mut(&device_id) else { return };
        if let Some(settled) = handled.get(&request_id) {
            if let Some(mut settled) = settled.clone() {
                settled["revision"] = json!(0);
                state.emit(session_id, json!({ "kind": "result", "requestId": request_id, "result": settled }), Some(sent()));
            }
            return;
        }
        if handled.values().filter(|outcome| outcome.is_none()).count() >= MAX_COMMANDS_IN_FLIGHT {
            return state.emit(session_id, json!({ "kind": "result", "requestId": request_id, "result": { "ok": false, "message": "That computer has too much still waiting.", "revision": 0 } }), Some(sent()));
        }
        handled.insert(request_id.clone(), None);
        trim(handled);
        drop(state);
        let inputs = fields.get("inputs").and_then(Value::as_array).cloned().unwrap_or_default();
        let inner = self.clone();
        let session_id = session_id.to_string();
        // Asked now, so inputs reach the workspace in the order their messages arrived.
        let running = workspace.input(inputs);
        tokio::spawn(async move {
            let result = match running.await {
                Ok(result) => result,
                Err(message) => json!({ "ok": false, "message": message, "revision": 0 }),
            };
            let mut outcome = result.clone();
            if let Some(outcome) = outcome.as_object_mut() {
                outcome.remove("revision");
            }
            let mut state = inner.state.lock().unwrap();
            if let Some(handled) = state.handled.get_mut(&device_id) {
                if handled.contains_key(&request_id) {
                    handled.insert(request_id.clone(), Some(outcome));
                }
            }
            let answering = state.answering(&session_id);
            state.emit(&answering, json!({ "kind": "result", "requestId": request_id, "result": result }), Some(sent()));
        });
    }

    /// Pings what is connected, hangs up on what has gone quiet, and forgets what will not come back.
    fn tick(self: &Arc<Self>) {
        let at = now();
        let mut moved = false;
        {
            let mut state = self.state.lock().unwrap();
            let ids: Vec<String> = state.sessions.iter().map(|session| session.id.clone()).collect();
            for id in ids {
                let Some(session) = state.session(&id) else { continue };
                if let Some(socket) = session.socket.clone() {
                    session.transfers.retain(|_, deadline| *deadline > at);
                    if session.transfers.is_empty() && at - session.last_seen_at > MOBILE_DEAD_AFTER_MS {
                        socket.terminate();
                        continue;
                    }
                    state.emit(&id, json!({ "kind": "ping", "at": at }), None);
                    continue;
                }
                if session.expires_at.is_some_and(|expires| at >= expires) {
                    state.forget(&id);
                    moved = true;
                }
            }
        }
        if moved {
            (self.options.on_change)();
        }
    }
}

#[cfg(test)]
#[path = "server_spec.rs"]
mod spec;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobile::relay::MobileResponse;

    async fn next_json<S>(socket: &mut S) -> Value
    where
        S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), socket.next()).await.expect("a frame in time").expect("an open socket").expect("a readable frame");
            if let Message::Text(text) = frame {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[(self.next() % items.len() as u64) as usize]
        }
    }

    fn noise(rng: &mut Rng, depth: u32) -> Value {
        let leaves = [json!(null), json!(true), json!(0), json!(-1), json!(1.5), json!(1e300), json!(""), json!("t1"), json!("é😀\u{0}"), json!("x".repeat(3000)), json!("uncommitted"), json!("commit"), json!("abcdef1234")];
        let keys = ["kind", "requestId", "command", "query", "type", "taskId", "version", "token", "sessionId", "lastSequence", "at", "range", "path", "audio", "before", "prefix", "push", "code", "deviceName", "base", "compare", "messageId", "href"];
        match if depth == 0 { 0 } else { rng.next() % 4 } {
            0 => rng.pick(&leaves).clone(),
            1 => Value::Array((0..rng.next() % 4).map(|_| noise(rng, depth - 1)).collect()),
            _ => Value::Object((0..rng.next() % 6).map(|_| ((*rng.pick(&keys)).to_string(), noise(rng, depth - 1))).collect()),
        }
    }

    fn phone_message(rng: &mut Rng, token: &str, session: &str) -> String {
        let kinds = ["thread-read", "history", "directories", "thread", "archived", "commands", "transcribe", "image", "diff-summary", "diff-patch"];
        let commands: Vec<&str> = act_core::validation::DECLARATIONS.iter().filter(|declaration| declaration.phone.is_some()).map(|declaration| declaration.kind).collect();
        let mut message = match rng.next() % 6 {
            0 => json!({ "kind": "resume", "version": 8, "token": token, "sessionId": session, "lastSequence": rng.next() % 5 }),
            1 => json!({ "kind": "command", "requestId": format!("r{}", rng.next() % 9), "command": { "type": rng.pick(&commands) } }),
            2 => json!({ "kind": "query", "requestId": format!("r{}", rng.next() % 9), "query": { "kind": rng.pick(&kinds) } }),
            3 => json!({ "kind": "ping", "at": rng.next() % 1000 }),
            4 => json!({ "kind": "push-token", "push": noise(rng, 2) }),
            _ => noise(rng, 4),
        };
        let keys = ["taskId", "range", "path", "before", "prefix", "audio", "messageId", "href", "text", "prompt", "id", "open", "lastSequence", "sessionId", "version"];
        for _ in 0..rng.next() % 4 {
            let (key, value) = ((*rng.pick(&keys)).to_string(), noise(rng, 2));
            let inner = if rng.next() % 2 == 0 { ["command", "query"].into_iter().find(|name| message.get(*name).is_some_and(Value::is_object)) } else { None };
            let target = match inner {
                Some(name) => &mut message[name],
                None => &mut message,
            };
            if let Value::Object(fields) = target {
                fields.insert(key, value);
            }
        }
        message.to_string()
    }

    #[tokio::test]
    async fn malformed_phone_and_computer_messages_never_panic_the_bridge() {
        let panics = Arc::new(AtomicUsize::new(0));
        let counted = panics.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            counted.fetch_add(1, Ordering::SeqCst);
            previous(info);
        }));
        let dir = tempfile::tempdir().unwrap();
        let devices = Arc::new(Mutex::new(PairingStore::open(dir.path().join("devices.json"), now())));
        let relay_cell: Arc<std::sync::OnceLock<Arc<MobileRelay>>> = Arc::new(std::sync::OnceLock::new());
        let answering = relay_cell.clone();
        let relay = MobileRelay::new(Arc::new(move |request| {
            let relay = answering.get().unwrap().clone();
            tokio::spawn(async move { relay.answer(MobileResponse { request_id: request.request_id, result: Ok(json!({ "groups": [], "thread": null })) }) });
            true
        }));
        relay_cell.set(relay.clone()).ok();
        let server = MobileServer::new(ServerOptions { devices: devices.clone(), port: 0, allowed_origins: Arc::new(Vec::new), relay, on_change: Arc::new(|| {}), workspace: None, session_grace: SESSION_GRACE, vocabulary: json!({}) });
        server.start("127.0.0.1").await.unwrap();
        let port = server.port().unwrap();
        let url = format!("ws://127.0.0.1:{port}/m/socket");
        let code = devices.lock().unwrap().mint(now()).code;
        let (mut phone, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        phone.send(Message::text(json!({ "kind": "pair", "version": 8, "code": code, "deviceName": "iPhone" }).to_string())).await.unwrap();
        let paired = next_json(&mut phone).await;
        let token = paired["token"].as_str().unwrap().to_string();
        let session = next_json(&mut phone).await["sessionId"].as_str().unwrap().to_string();
        drop(phone);

        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        for round in 0..120 {
            let path = if round % 5 == 4 { workspace_socket_path() } else { socket_path() };
            let Ok((mut socket, _)) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}{path}")).await else { continue };
            if round % 2 == 0 {
                let _ = socket.send(Message::text(json!({ "kind": "resume", "version": 8, "token": token, "sessionId": session, "lastSequence": 0 }).to_string())).await;
            }
            for _ in 0..8 {
                let frame = if rng.next() % 7 == 0 { Message::binary((0..rng.next() % 64).map(|_| rng.next() as u8).collect::<Vec<u8>>()) } else { Message::text(phone_message(&mut rng, &token, &session)) };
                if socket.send(frame).await.is_err() {
                    break;
                }
            }
            let _ = tokio::time::timeout(Duration::from_millis(20), socket.next()).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;

        let (mut phone, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        phone.send(Message::text(json!({ "kind": "resume", "version": 8, "token": token, "lastSequence": 0 }).to_string())).await.unwrap();
        let mut answered = false;
        for _ in 0..20 {
            if next_json(&mut phone).await["kind"] == "snapshot" {
                answered = true;
                break;
            }
        }
        assert!(answered, "the bridge still answers a phone");
        server.stop().await;
        assert_eq!(panics.load(Ordering::SeqCst), 0, "a malformed message panicked the bridge");
    }

    #[tokio::test]
    async fn a_phone_pairs_then_resumes_where_it_left_off() {
        let dir = tempfile::tempdir().unwrap();
        let devices = Arc::new(Mutex::new(PairingStore::open(dir.path().join("devices.json"), now())));
        let relay_cell: Arc<std::sync::OnceLock<Arc<MobileRelay>>> = Arc::new(std::sync::OnceLock::new());
        let answering = relay_cell.clone();
        let relay = MobileRelay::new(Arc::new(move |request| {
            let relay = answering.get().unwrap().clone();
            tokio::spawn(async move { relay.answer(MobileResponse { request_id: request.request_id, result: Ok(json!({ "groups": [], "thread": null })) }) });
            true
        }));
        relay_cell.set(relay.clone()).ok();
        let server = MobileServer::new(ServerOptions { devices: devices.clone(), port: 0, allowed_origins: Arc::new(Vec::new), relay, on_change: Arc::new(|| {}), workspace: None, session_grace: SESSION_GRACE, vocabulary: json!({}) });
        server.start("127.0.0.1").await.unwrap();
        let url = format!("ws://127.0.0.1:{}/m/socket", server.port().unwrap());
        let code = devices.lock().unwrap().mint(now()).code;

        let (mut phone, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        phone.send(Message::text(json!({ "kind": "pair", "version": 8, "code": code, "deviceName": "iPhone" }).to_string())).await.unwrap();
        let paired = next_json(&mut phone).await;
        assert_eq!((paired["kind"].as_str(), paired["sequence"].as_u64()), (Some("paired"), Some(1)));
        let snapshot = next_json(&mut phone).await;
        assert_eq!(snapshot["kind"], "snapshot");
        let session = snapshot["sessionId"].as_str().unwrap().to_string();
        let token = paired["token"].as_str().unwrap().to_string();
        drop(phone);

        server.publish(&json!({ "kind": "patch", "patch": { "dictation": true } }));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (mut phone, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        phone.send(Message::text(json!({ "kind": "resume", "version": 8, "token": token, "sessionId": session, "lastSequence": 2 }).to_string())).await.unwrap();
        let missed = next_json(&mut phone).await;
        assert_eq!((missed["kind"].as_str(), missed["sequence"].as_u64()), (Some("patch"), Some(3)));
        assert_eq!(next_json(&mut phone).await["kind"], "ping");

        let (mut stranger, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        stranger.send(Message::text(json!({ "kind": "resume", "version": 8, "token": "nope", "lastSequence": 0 }).to_string())).await.unwrap();
        assert_eq!(next_json(&mut stranger).await["code"], "unauthorized");
        server.stop().await;
    }
}
