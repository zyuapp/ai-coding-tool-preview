//! One line to a paired computer. It dials, pairs or resumes, mirrors the other computer's state as it
//! arrives, and carries inputs there. A dropped line is dialled again on a growing pause, and what was
//! still owed an answer goes again once it is back: the other computer remembers what it has run by
//! request, so nothing runs twice. A line the other computer refuses is left down with the reason.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_core::domain::platform::ComputerStatus;
use act_core::validation::REMOTE_UNSUPPORTED;
use act_core::WorkspaceState;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use super::replica::{RemoteWorkspaceReplica, ReplicaUpdate};
use crate::attachments::MAX_ATTACHMENT_ENCODED_BYTES;
use crate::mobile::addresses::workspace_socket_path;
use crate::mobile::messages::{computer_capabilities, is_computer_server_message, is_computer_transfer, supports_command, supports_query, COMPUTER_PROTOCOL_VERSION};
use crate::mobile::{COMPUTER_TRANSFER_TIMEOUT_MS, MAX_COMPUTER_MESSAGE_BYTES, MOBILE_PING_INTERVAL_MS};
use crate::notify::ThreadNotice;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY: [Duration; 5] = [Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(10), Duration::from_secs(15)];
/// How long a line the other computer refused waits before asking again.
const REFUSED_RETRY: Duration = Duration::from_secs(5 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a request waits in all while its line is down, to be carried again once it is back.
const OUTAGE_WAIT: Duration = Duration::from_secs(2 * 60);
/// Silence past this means the line is gone, though no one said so.
pub const COMPUTER_SILENT_AFTER: Duration = Duration::from_millis(MOBILE_PING_INTERVAL_MS as u64 + 5_000);

pub const COMPUTER_OFFLINE: &str = "That computer cannot be reached right now.";
pub const COMPUTER_RESTARTED: &str = "That computer restarted before it answered. Check whether it went through.";
pub const COMPUTER_SEND_TOO_LARGE: &str = "This message is too large to send to another computer. Use smaller images or send fewer attachments.";

/// The socket a host is dialled on: the tailnet name over TLS, or plain to this machine's own loopback.
pub fn computer_socket_url(host: &str) -> String {
    let bare = host.split(':').next().unwrap_or(host);
    let loopback = matches!(bare, "127.0.0.1" | "localhost") || host.starts_with("[::1]");
    format!("{}://{host}{}", if loopback { "ws" } else { "wss" }, workspace_socket_path())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    Token(String),
    Code(String),
}

pub struct ComputerClientOptions {
    pub host: String,
    /// What this computer calls itself, which is how the other lists it.
    pub device_name: String,
    pub credential: Credential,
    pub url: Option<String>,
    pub on_status: Arc<dyn Fn(ComputerStatus, Option<String>) + Send + Sync>,
    /// The device id, name and token the other computer handed out.
    pub on_paired: Arc<dyn Fn(String, String, String) + Send + Sync>,
    pub on_state: Arc<dyn Fn(Arc<WorkspaceState>) + Send + Sync>,
    pub on_capabilities: Arc<dyn Fn(Option<Vec<String>>) + Send + Sync>,
    pub on_notice: Arc<dyn Fn(ThreadNotice) + Send + Sync>,
    pub on_name: Arc<dyn Fn(String) + Send + Sync>,
}

/// A request still owed an answer, kept as it was sent so a line that comes back can carry it again.
struct Waiting {
    settle: oneshot::Sender<Result<Value, String>>,
    payload: Arc<str>,
    transferring: bool,
    line: u64,
    instance: Option<String>,
}

#[derive(Default)]
struct Owed {
    results: HashMap<String, Waiting>,
    answers: HashMap<String, Waiting>,
}

struct ClientState {
    writer: Option<mpsc::UnboundedSender<Message>>,
    credential: Credential,
    session_id: Option<String>,
    last_sequence: u64,
    displayed: Option<Arc<WorkspaceState>>,
    capabilities: Option<Vec<String>>,
    replica: RemoteWorkspaceReplica,
    attempt: usize,
    dials: u64,
    instance: Option<String>,
    stopped: bool,
    refused: bool,
    status: ComputerStatus,
    last_error: Option<String>,
    heard_at: tokio::time::Instant,
    owed: Owed,
}

struct ClientInner {
    options: ComputerClientOptions,
    url: String,
    state: Mutex<ClientState>,
    wake: Notify,
    cut: Notify,
    stop: CancellationToken,
}

#[derive(Clone)]
pub struct ComputerClient {
    inner: Arc<ClientInner>,
}

fn request_payload(message: &Value) -> Option<String> {
    let oversized = message.get("inputs").and_then(Value::as_array).is_some_and(|inputs| {
        inputs.iter().any(|input| {
            input.get("type").and_then(Value::as_str) == Some("attachments.send")
                && input.get("attachments").and_then(Value::as_array).is_some_and(|attachments| {
                    attachments.iter().any(|attachment| {
                        let source = attachment.get("source").and_then(Value::as_str).unwrap_or("");
                        let data = if source.starts_with("data:") { source.split_once(',').map(|(_, data)| data).unwrap_or("") } else { source };
                        data.len() > MAX_ATTACHMENT_ENCODED_BYTES
                    })
                })
        })
    });
    if oversized {
        return None;
    }
    let payload = message.to_string();
    (payload.len() <= MAX_COMPUTER_MESSAGE_BYTES).then_some(payload)
}

impl ComputerClient {
    pub fn connect(options: ComputerClientOptions) -> Self {
        let url = options.url.clone().unwrap_or_else(|| computer_socket_url(&options.host));
        let credential = options.credential.clone();
        let inner = Arc::new(ClientInner {
            options,
            url,
            state: Mutex::new(ClientState {
                writer: None,
                credential,
                session_id: None,
                last_sequence: 0,
                displayed: None,
                capabilities: None,
                replica: RemoteWorkspaceReplica::default(),
                attempt: 0,
                dials: 0,
                instance: None,
                stopped: false,
                refused: false,
                status: ComputerStatus::Connecting,
                last_error: None,
                heard_at: tokio::time::Instant::now(),
                owed: Owed::default(),
            }),
            wake: Notify::new(),
            cut: Notify::new(),
            stop: CancellationToken::new(),
        });
        let driver = inner.clone();
        tokio::spawn(async move { driver.drive().await });
        Self { inner }
    }

    pub fn status(&self) -> ComputerStatus {
        self.inner.state.lock().unwrap().status
    }

    pub fn state(&self) -> Option<Arc<WorkspaceState>> {
        self.inner.state.lock().unwrap().displayed.clone()
    }

    /// Carries the window's inputs there, answered with the last one's result.
    pub async fn send(&self, inputs: Vec<Value>) -> Result<Value, String> {
        let own = computer_capabilities();
        let theirs = self.inner.state.lock().unwrap().capabilities.clone();
        let supported = inputs.iter().all(|input| act_core::validation::parse_command(input).is_ok() && supports_command(Some(&own), input) && supports_command(theirs.as_deref(), input));
        if !supported {
            return Err(REMOTE_UNSUPPORTED.into());
        }
        self.inner.ask(true, json!({ "kind": "input", "requestId": uuid::Uuid::new_v4().to_string(), "inputs": inputs })).await
    }

    pub async fn query(&self, query: Value) -> Result<Value, String> {
        let theirs = self.inner.state.lock().unwrap().capabilities.clone();
        if !supports_query(theirs.as_deref(), &query) {
            return Err(REMOTE_UNSUPPORTED.into());
        }
        self.inner.ask(false, json!({ "kind": "query", "requestId": uuid::Uuid::new_v4().to_string(), "query": query })).await
    }

    /// Dials now rather than at the end of the pause, and asks a computer that refused again. A line
    /// that only looks up, after the network moved under it, is cut first so the dial is a real one.
    pub fn reconnect(&self) {
        let (open, cut) = {
            let mut state = self.inner.state.lock().unwrap();
            if state.stopped || (state.refused && matches!(state.credential, Credential::Code(_))) {
                return;
            }
            state.refused = false;
            state.attempt = 0;
            let quiet = state.heard_at.elapsed() >= COMPUTER_SILENT_AFTER;
            (state.writer.is_some(), state.writer.is_some() && state.status == ComputerStatus::Connected && quiet)
        };
        if cut {
            self.inner.cut.notify_one();
        } else if !open {
            self.inner.wake.notify_one();
        }
    }

    pub fn stop(&self) {
        let waiting = {
            let mut state = self.inner.state.lock().unwrap();
            state.stopped = true;
            if let Some(writer) = state.writer.take() {
                let _ = writer.send(Message::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame { code: 1000.into(), reason: "stopped".into() })));
            }
            std::mem::take(&mut state.owed)
        };
        refuse_all(waiting, COMPUTER_OFFLINE);
        self.inner.stop.cancel();
    }
}

fn refuse_all(owed: Owed, message: &str) {
    for (_, waiting) in owed.results.into_iter().chain(owed.answers) {
        let _ = waiting.settle.send(Err(message.to_string()));
    }
}

impl ClientInner {
    fn report(&self, status: ComputerStatus, error: Option<String>) {
        self.state.lock().unwrap().status = status;
        (self.options.on_status)(status, error);
    }

    fn live(state: &ClientState) -> bool {
        state.writer.is_some() && state.status == ComputerStatus::Connected
    }

    /// Writes a request, announcing a large one first so the other computer allows for it.
    fn deliver(state: &ClientState, request_id: &str, waiting: &Waiting) {
        let Some(writer) = &state.writer else { return };
        if waiting.transferring {
            let _ = writer.send(Message::text(json!({ "kind": "transfer", "requestId": request_id }).to_string()));
        }
        let _ = writer.send(Message::text(waiting.payload.to_string()));
    }

    async fn ask(self: &Arc<Self>, input: bool, message: Value) -> Result<Value, String> {
        let request_id = message.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(payload) = request_payload(&message) else { return Err(COMPUTER_SEND_TOO_LARGE.into()) };
        let transferring = message.as_object().is_some_and(is_computer_transfer);
        let (settle, settled) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if !Self::live(&state) {
                return Err(COMPUTER_OFFLINE.into());
            }
            let waiting = Waiting { settle, payload: payload.into(), transferring, line: state.dials, instance: state.instance.clone() };
            Self::deliver(&state, &request_id, &waiting);
            if input {
                state.owed.results.insert(request_id.clone(), waiting);
            } else {
                state.owed.answers.insert(request_id.clone(), waiting);
            }
        }
        let first = if transferring { Duration::from_millis(COMPUTER_TRANSFER_TIMEOUT_MS as u64) } else { REQUEST_TIMEOUT };
        let asked_at = tokio::time::Instant::now();
        let deadline = tokio::time::sleep(first);
        tokio::pin!(deadline);
        tokio::pin!(settled);
        loop {
            tokio::select! {
                answer = &mut settled => return answer.unwrap_or_else(|_| Err(COMPUTER_OFFLINE.into())),
                _ = &mut deadline => {
                    // A request whose line is down waits out the outage; one on a live line that went unanswered is refused.
                    let waited = asked_at.elapsed();
                    let mut state = self.state.lock().unwrap();
                    let live = Self::live(&state);
                    if !live && waited < OUTAGE_WAIT {
                        deadline.as_mut().reset(tokio::time::Instant::now() + (OUTAGE_WAIT - waited));
                        continue;
                    }
                    state.owed.results.remove(&request_id);
                    state.owed.answers.remove(&request_id);
                    return Err(if live { "That computer did not answer in time.".into() } else { COMPUTER_OFFLINE.into() });
                }
            }
        }
    }

    /// What was written on an earlier line goes again only to the same running app, which remembers
    /// what it ran; an app that restarted since would run it twice, so the request is refused.
    fn carry(state: &mut ClientState) {
        let current = state.dials;
        let instance = state.instance.clone();
        for held in [&mut state.owed.results, &mut state.owed.answers] {
            let stale: Vec<String> = held.iter().filter(|(_, waiting)| waiting.line != current && (instance.is_none() || waiting.instance != instance)).map(|(id, _)| id.clone()).collect();
            for id in stale {
                if let Some(waiting) = held.remove(&id) {
                    let _ = waiting.settle.send(Err(COMPUTER_RESTARTED.into()));
                }
            }
        }
        let resend: Vec<(String, bool)> = state.owed.results.iter().map(|(id, waiting)| (id.clone(), waiting.line != current)).chain(state.owed.answers.iter().map(|(id, waiting)| (id.clone(), waiting.line != current))).collect();
        for (id, again) in resend {
            let waiting = state.owed.results.get(&id).or_else(|| state.owed.answers.get(&id));
            if let (true, Some(waiting)) = (again, waiting) {
                Self::deliver(state, &id, waiting);
            }
            if let Some(waiting) = state.owed.results.get_mut(&id).or(state.owed.answers.get_mut(&id)) {
                waiting.line = current;
                waiting.instance = instance.clone();
            }
        }
    }

    fn connected(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.attempt = 0;
            state.refused = false;
            state.last_error = None;
        }
        self.report(ComputerStatus::Connected, None);
    }

    /// The line answered something. Returns what ends the line, if anything did.
    fn receive(&self, message: Map<String, Value>) -> Option<(Option<String>, bool)> {
        let kind = message.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
        let resumed_live = {
            let mut state = self.state.lock().unwrap();
            state.last_sequence = message.get("sequence").and_then(Value::as_f64).unwrap_or(0.0) as u64;
            state.heard_at = tokio::time::Instant::now();
            state.displayed.is_some() && state.session_id.is_some() && state.status != ComputerStatus::Connected && kind != "workspace" && kind != "error"
        };
        if resumed_live {
            self.connected();
        }
        let text = |key: &str| message.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        match kind.as_str() {
            "paired" => {
                self.state.lock().unwrap().credential = Credential::Token(text("token"));
                (self.options.on_paired)(text("deviceId"), text("deviceName"), text("token"));
            }
            "workspace" => {
                let applied = {
                    let mut state = self.state.lock().unwrap();
                    if let Some(session) = message.get("sessionId").and_then(Value::as_str) {
                        state.session_id = Some(session.to_string());
                    }
                    let update = message.get("update").and_then(Value::as_object).cloned().unwrap_or_default();
                    state.replica.apply(&update)
                };
                match applied {
                    Err(_) => return Some((Some("This computer sent workspace data this app cannot read. Updating AI Coding Tool may help.".into()), true)),
                    Ok(ReplicaUpdate::Resync) => {
                        self.state.lock().unwrap().session_id = None;
                        return Some((None, false));
                    }
                    Ok(ReplicaUpdate::Unchanged) => {}
                    Ok(ReplicaUpdate::State(next)) => {
                        let next: Arc<WorkspaceState> = Arc::from(next);
                        let status = {
                            let mut state = self.state.lock().unwrap();
                            state.displayed = Some(next.clone());
                            state.status
                        };
                        if status != ComputerStatus::Connected {
                            self.connected();
                        }
                        (self.options.on_state)(next);
                    }
                }
            }
            "result" | "answer" => {
                let request_id = text("requestId");
                let mut state = self.state.lock().unwrap();
                if kind == "result" {
                    if let Some(waiting) = state.owed.results.remove(&request_id) {
                        let _ = waiting.settle.send(Ok(message.get("result").cloned().unwrap_or(Value::Null)));
                    }
                } else if let Some(waiting) = state.owed.answers.remove(&request_id) {
                    let answer = if message.get("ok") == Some(&Value::Bool(true)) { Ok(message.get("result").cloned().unwrap_or(Value::Null)) } else { Err(text("message")) };
                    let _ = waiting.settle.send(answer);
                }
            }
            "error" => {
                let refusal = matches!(text("code").as_str(), "unauthorized" | "version" | "expired-code" | "rate-limited");
                return Some((Some(text("message")), refusal));
            }
            "notice" => {
                if let Some(notice) = message.get("notice") {
                    (self.options.on_notice)(ThreadNotice { task_id: notice.get("taskId").and_then(Value::as_str).unwrap_or("").into(), title: notice.get("title").and_then(Value::as_str).unwrap_or("").into(), headline: notice.get("headline").and_then(Value::as_str).unwrap_or("").into() });
                }
            }
            "capabilities" => {
                let capabilities: Vec<String> = message.get("capabilities").and_then(Value::as_array).map(|names| names.iter().filter_map(|name| name.as_str().map(str::to_string)).collect()).unwrap_or_default();
                {
                    let mut state = self.state.lock().unwrap();
                    state.capabilities = Some(capabilities.clone());
                    state.instance = message.get("instance").and_then(Value::as_str).map(str::to_string);
                    Self::carry(&mut state);
                }
                (self.options.on_capabilities)(Some(capabilities));
            }
            "name" => (self.options.on_name)(text("name")),
            "ping" => {
                if let Some(writer) = &self.state.lock().unwrap().writer {
                    let _ = writer.send(Message::text(json!({ "kind": "pong", "at": message.get("at").cloned().unwrap_or(json!(0)) }).to_string()));
                }
            }
            _ => {}
        }
        None
    }

    /// A refused line is left down a long while; every other drop is dialled again soon.
    fn dropped(&self, error: Option<String>, refusal: bool) -> Option<Duration> {
        let (refused_owed, delay, last_error) = {
            let mut state = self.state.lock().unwrap();
            state.writer = None;
            if state.stopped {
                return None;
            }
            if error.is_some() {
                state.last_error = error.clone();
            }
            let mut refused_owed = None;
            if refusal {
                refused_owed = Some(std::mem::take(&mut state.owed));
                state.refused = true;
                if matches!(state.credential, Credential::Code(_)) {
                    state.stopped = true;
                }
            }
            let delay = if state.refused { REFUSED_RETRY } else { RETRY[state.attempt.min(RETRY.len() - 1)] };
            state.attempt += 1;
            (refused_owed, (!state.stopped).then_some(delay), state.last_error.clone())
        };
        if let Some(owed) = refused_owed {
            refuse_all(owed, error.as_deref().unwrap_or(COMPUTER_OFFLINE));
        }
        self.report(ComputerStatus::Offline, last_error);
        delay
    }

    async fn drive(self: Arc<Self>) {
        loop {
            if self.stop.is_cancelled() || self.state.lock().unwrap().stopped {
                return;
            }
            let last_error = {
                let mut state = self.state.lock().unwrap();
                state.dials += 1;
                state.instance = None;
                state.last_error.clone()
            };
            self.report(ComputerStatus::Connecting, last_error);
            let (error, refusal) = self.line().await;
            let Some(delay) = self.dropped(error, refusal) else { return };
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.wake.notified() => {}
                _ = self.stop.cancelled() => return,
            }
        }
    }

    /// One dial, until it drops: what ended it, and whether the other computer refused it.
    async fn line(self: &Arc<Self>) -> (Option<String>, bool) {
        let config = WebSocketConfig::default().max_message_size(Some(256 * 1024 * 1024)).max_frame_size(Some(256 * 1024 * 1024));
        let connecting = tokio_tungstenite::connect_async_with_config(&self.url, Some(config), true);
        let socket = tokio::select! {
            socket = tokio::time::timeout(CONNECT_TIMEOUT, connecting) => socket,
            _ = self.stop.cancelled() => return (None, false),
        };
        let Ok(Ok((socket, _))) = socket else { return (None, false) };
        let (mut sink, mut stream) = socket.split();
        let (writer, mut outgoing) = mpsc::unbounded_channel::<Message>();
        let hello = {
            let mut state = self.state.lock().unwrap();
            state.writer = Some(writer);
            state.capabilities = None;
            state.heard_at = tokio::time::Instant::now();
            match &state.credential {
                Credential::Token(token) => {
                    let mut resume = json!({ "kind": "resume", "version": COMPUTER_PROTOCOL_VERSION, "token": token, "lastSequence": state.last_sequence });
                    if let Some(session) = &state.session_id {
                        resume["sessionId"] = json!(session);
                    }
                    resume
                }
                Credential::Code(code) => json!({ "kind": "pair", "version": COMPUTER_PROTOCOL_VERSION, "code": code, "deviceName": self.options.device_name }),
            }
        };
        (self.options.on_capabilities)(None);
        if sink.send(Message::text(hello.to_string())).await.is_err() {
            return (None, false);
        }
        loop {
            let heard_at = self.state.lock().unwrap().heard_at;
            tokio::select! {
                frame = stream.next() => {
                    let text = match frame {
                        Some(Ok(Message::Text(text))) => text.to_string(),
                        Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                        Some(Ok(Message::Close(frame))) => {
                            let said = frame.map(|frame| (u16::from(frame.code), frame.reason.to_string()));
                            return match said {
                                Some((code, reason)) if code != 1000 && !reason.is_empty() && reason != "unauthorized" => (Some(reason), false),
                                _ => (None, false),
                            };
                        }
                        Some(Ok(_)) => { self.state.lock().unwrap().heard_at = tokio::time::Instant::now(); continue }
                        Some(Err(_)) | None => return (None, false),
                    };
                    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    if !is_computer_server_message(&parsed) {
                        continue;
                    }
                    if let Some(ended) = self.receive(parsed.as_object().cloned().unwrap_or_default()) {
                        let _ = sink.close().await;
                        return ended;
                    }
                }
                message = outgoing.recv() => {
                    let Some(message) = message else { return (None, false) };
                    let closing = matches!(message, Message::Close(_));
                    if sink.send(message).await.is_err() || closing {
                        return (None, false);
                    }
                }
                _ = tokio::time::sleep_until(heard_at + COMPUTER_SILENT_AFTER) => {
                    if self.state.lock().unwrap().heard_at.elapsed() >= COMPUTER_SILENT_AFTER {
                        return (None, false);
                    }
                }
                _ = self.cut.notified() => return (None, false),
                _ = self.stop.cancelled() => return (None, false),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobile::pairing::PairingStore;
    use crate::mobile::relay::MobileRelay;
    use crate::mobile::server::{MobileServer, ServerOptions, WorkspaceHooks, WorkspaceUpdate, SESSION_GRACE};
    use futures::future::BoxFuture;
    use futures::FutureExt;

    struct Desk {
        inputs: Mutex<Vec<Value>>,
    }

    impl WorkspaceHooks for Desk {
        fn name(&self) -> Option<String> {
            Some("Studio".into())
        }
        fn snapshot(&self) -> WorkspaceUpdate {
            WorkspaceUpdate::State { revision: 1, state: json!({ "threads": [{ "id": "t1", "engine": "claude", "title": "Remote work", "executionPolicy": "confirm", "updatedAt": 1 }], "computers": { "paired": [{ "id": "x", "name": "Other", "host": "other.ts.net", "status": "connected", "error": null, "pairedAt": 1, "state": { "secret": true } }] } }) }
        }
        fn subscribe(&self, _listener: Arc<dyn Fn(WorkspaceUpdate) + Send + Sync>) -> Box<dyn FnOnce() + Send> {
            Box::new(|| {})
        }
        fn input(&self, inputs: Vec<Value>) -> BoxFuture<'static, Result<Value, String>> {
            self.inputs.lock().unwrap().extend(inputs);
            async { Ok(json!({ "ok": true, "taskId": "t2", "revision": 2 })) }.boxed()
        }
        fn query(&self, _query: Value) -> BoxFuture<'static, Result<Value, String>> {
            async { Ok(json!(["/Users/me/code/"])) }.boxed()
        }
    }

    #[tokio::test]
    async fn a_computer_pairs_mirrors_the_others_threads_and_drives_it() {
        let dir = tempfile::tempdir().unwrap();
        let devices = Arc::new(Mutex::new(PairingStore::open(dir.path().join("devices.json"), crate::mobile::now())));
        let desk = Arc::new(Desk { inputs: Mutex::new(Vec::new()) });
        let server = MobileServer::new(ServerOptions {
            devices: devices.clone(),
            port: 0,
            allowed_origins: Arc::new(Vec::new),
            relay: MobileRelay::new(Arc::new(|_| false)),
            on_change: Arc::new(|| {}),
            workspace: Some(desk.clone()),
            session_grace: SESSION_GRACE,
            vocabulary: json!({}),
        });
        server.start("127.0.0.1").await.unwrap();
        let code = devices.lock().unwrap().mint(crate::mobile::now()).code;
        let (states, mut arrived) = mpsc::unbounded_channel();
        let (paired_tx, mut paired) = mpsc::unbounded_channel();
        let client = ComputerClient::connect(ComputerClientOptions {
            host: format!("127.0.0.1:{}", server.port().unwrap()),
            device_name: "Laptop".into(),
            credential: Credential::Code(code),
            url: None,
            on_status: Arc::new(|_, _| {}),
            on_paired: Arc::new(move |_, _, token| {
                let _ = paired_tx.send(token);
            }),
            on_state: Arc::new(move |state| {
                let _ = states.send(state);
            }),
            on_capabilities: Arc::new(|_| {}),
            on_notice: Arc::new(|_| {}),
            on_name: Arc::new(|_| {}),
        });
        let token = tokio::time::timeout(Duration::from_secs(5), paired.recv()).await.unwrap().unwrap();
        assert_eq!(token.len(), 64);
        let state = tokio::time::timeout(Duration::from_secs(5), arrived.recv()).await.unwrap().unwrap();
        assert_eq!(state.threads[0].title, "Remote work");
        assert!(state.computers.paired.iter().all(|computer| computer.state.is_none()));
        for _ in 0..50 {
            if client.inner.state.lock().unwrap().capabilities.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let result = client.send(vec![json!({ "type": "task.select", "taskId": "t1" })]).await.unwrap();
        assert_eq!(result["taskId"], "t2");
        assert_eq!(desk.inputs.lock().unwrap().len(), 1);
        assert_eq!(client.query(json!({ "kind": "directories", "prefix": "~/c" })).await.unwrap(), json!(["/Users/me/code/"]));
        client.stop();
        server.stop().await;
    }
}
