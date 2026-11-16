//! The phone bridge server as the TypeScript app's own cases hold it (tests/main/mobile/mobile-server.test.mts).

use std::sync::atomic::AtomicU16;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::*;
use crate::mobile::relay::{MobileOp, MobileRequest, MobileResponse};

const VERSION: u64 = MOBILE_PROTOCOL_VERSION;

fn view(title: &str) -> Value {
    json!({ "groups": [{ "projectId": null, "name": title, "threads": [] }], "activity": { "priority": [], "running": [], "threads": [], "snoozed": [] }, "theme": { "dark": "aicodingtool-dark", "light": "aicodingtool-light", "mode": "dark" }, "engines": {}, "upkeep": [], "dictation": false, "usage": { "engines": [], "composer": null }, "thread": null, "draft": null, "error": null })
}

async fn until<T>(mut check: impl FnMut() -> Option<T>, message: &str) -> T {
    for _ in 0..300 {
        if let Some(value) = check() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{message}");
}

#[derive(Default)]
struct Window {
    commands: Mutex<Vec<Value>>,
    queries: Mutex<Vec<Value>>,
    snapshot: Mutex<Value>,
    refuse_snapshot: Mutex<Option<String>>,
    refuse_query: Mutex<Option<String>>,
    held: Mutex<Option<Arc<Semaphore>>>,
}

struct Harness {
    server: MobileServer,
    devices: Arc<Mutex<PairingStore>>,
    window: Arc<Window>,
    port: Arc<AtomicU16>,
    _folder: tempfile::TempDir,
}

impl Harness {
    async fn start() -> Harness {
        let folder = tempfile::tempdir().unwrap();
        let devices = Arc::new(Mutex::new(PairingStore::open(folder.path().join("mobile-devices.v1.json"), now())));
        let window = Arc::new(Window { snapshot: Mutex::new(view("first")), ..Window::default() });
        let relay_cell: Arc<std::sync::OnceLock<Arc<MobileRelay>>> = Arc::new(std::sync::OnceLock::new());
        let (answering, asked) = (relay_cell.clone(), window.clone());
        let relay = MobileRelay::new(Arc::new(move |request: MobileRequest| {
            let (relay, window) = (answering.get().unwrap().clone(), asked.clone());
            tokio::spawn(async move {
                let result = match request.op {
                    MobileOp::Snapshot => {
                        let held = window.held.lock().unwrap().clone();
                        if let Some(held) = held {
                            let _ = held.acquire().await;
                        }
                        match window.refuse_snapshot.lock().unwrap().clone() {
                            Some(message) => Err(message),
                            None => Ok(window.snapshot.lock().unwrap().clone()),
                        }
                    }
                    MobileOp::Command { command } => {
                        window.commands.lock().unwrap().push(command);
                        Ok(Value::Null)
                    }
                    MobileOp::Query { query } => match window.refuse_query.lock().unwrap().clone() {
                        Some(message) => Err(message),
                        None => {
                            window.queries.lock().unwrap().push(query);
                            Ok(json!({ "status": "available", "patch": "" }))
                        }
                    },
                };
                relay.answer(MobileResponse { request_id: request.request_id, result });
            });
            true
        }));
        relay_cell.set(relay.clone()).ok();
        let port = Arc::new(AtomicU16::new(0));
        let origin_port = port.clone();
        let server = MobileServer::new(ServerOptions {
            devices: devices.clone(),
            port: 0,
            allowed_origins: Arc::new(move || vec![format!("http://127.0.0.1:{}", origin_port.load(Ordering::SeqCst))]),
            relay,
            on_change: Arc::new(|| {}),
            workspace: None,
            session_grace: Duration::ZERO,
            vocabulary: mobile_vocabulary(),
        });
        let harness = Harness { server, devices, window, port, _folder: folder };
        harness.restart().await;
        harness
    }

    async fn restart(&self) {
        self.server.start("127.0.0.1").await.unwrap();
        self.port.store(self.server.port().unwrap(), Ordering::SeqCst);
    }

    fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port())
    }

    fn socket_url(&self) -> String {
        format!("ws://127.0.0.1:{}/m/socket", self.port())
    }

    fn code(&self) -> String {
        self.devices.lock().unwrap().mint(now()).code
    }

    fn commands(&self) -> Vec<Value> {
        self.window.commands.lock().unwrap().clone()
    }

    fn connections(&self) -> Vec<MobileConnectionState> {
        self.server.session_views().iter().map(|session| session.connection).collect()
    }

    async fn offline(&self) {
        until(|| (self.connections().first() == Some(&MobileConnectionState::Offline)).then_some(()), "the session never went offline").await;
    }

    /// Makes the next snapshot wait until the returned closure is called.
    fn hold(&self) -> impl FnOnce() {
        let held = Arc::new(Semaphore::new(0));
        *self.window.held.lock().unwrap() = Some(held.clone());
        let window = self.window.clone();
        move || {
            *window.held.lock().unwrap() = None;
            held.add_permits(16);
        }
    }

    /// A phone that pairs, with the `paired` and `snapshot` it was handed.
    async fn paired_phone(&self) -> (Phone, Value, Value) {
        let phone = Phone::open(&self.socket_url(), &[]).await.unwrap();
        phone.send(json!({ "kind": "pair", "version": VERSION, "code": self.code(), "deviceName": "iPhone" }));
        let paired = phone.wait_for("paired").await;
        let snapshot = phone.wait_for("snapshot").await;
        (phone, paired, snapshot)
    }
}

/// A socket with everything it has been told, so a test waits for a message rather than a moment.
struct Phone {
    messages: Arc<Mutex<Vec<Value>>>,
    closes: Arc<Mutex<Vec<u16>>>,
    out: mpsc::UnboundedSender<Message>,
}

impl Phone {
    /// The socket, or the status its upgrade was refused with.
    async fn open(url: &str, headers: &[(&str, &str)]) -> Result<Phone, u16> {
        let mut request = url.into_client_request().unwrap();
        for (name, value) in headers {
            request.headers_mut().insert(http::HeaderName::from_bytes(name.as_bytes()).unwrap(), HeaderValue::from_str(value).unwrap());
        }
        let socket = match tokio_tungstenite::connect_async(request).await {
            Ok((socket, _)) => socket,
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => return Err(response.status().as_u16()),
            Err(_) => return Err(0),
        };
        let (mut sink, mut reader) = socket.split();
        let (out, mut outgoing) = mpsc::unbounded_channel::<Message>();
        tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                if sink.send(message).await.is_err() {
                    break;
                }
            }
        });
        let (messages, closes) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
        let (heard, closed) = (messages.clone(), closes.clone());
        tokio::spawn(async move {
            let mut code = 1006;
            while let Some(Ok(frame)) = reader.next().await {
                match frame {
                    Message::Text(text) => heard.lock().unwrap().push(serde_json::from_str::<Value>(&text).unwrap()),
                    Message::Close(frame) => code = frame.map(|frame| u16::from(frame.code)).unwrap_or(1005),
                    _ => {}
                }
            }
            closed.lock().unwrap().push(code);
        });
        Ok(Phone { messages, closes, out })
    }

    fn send(&self, message: Value) {
        let _ = self.out.send(Message::text(message.to_string()));
    }

    fn close(&self) {
        let _ = self.out.send(Message::Close(None));
    }

    fn messages(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }

    fn of_kind(&self, kind: &str) -> Vec<Value> {
        self.messages().into_iter().filter(|message| message["kind"] == kind).collect()
    }

    fn closed(&self) -> bool {
        !self.closes.lock().unwrap().is_empty()
    }

    async fn wait_for(&self, kind: &str) -> Value {
        until(|| self.of_kind(kind).into_iter().next(), &format!("no {kind} message arrived")).await
    }
}

/// A plain HTTP request: the status, the headers and the body.
async fn fetch(port: u16, method: &str, path: &str) -> (u16, HeaderMap, String) {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw)).await;
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let status = lines.next().and_then(|line| line.split(' ').nth(1)).and_then(|code| code.parse().ok()).unwrap_or(0);
    let mut headers = HeaderMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(http::HeaderName::from_bytes(name.trim().to_lowercase().as_bytes()).unwrap(), HeaderValue::from_str(value.trim()).unwrap());
        }
    }
    (status, headers, body.to_string())
}

/// A socket that speaks the protocol but never answers a close frame.
struct RudePhone(tokio::net::TcpStream);

impl RudePhone {
    async fn open(port: u16) -> RudePhone {
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let handshake = format!("GET /m/socket HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==\r\nSec-WebSocket-Version: 13\r\n\r\n");
        socket.write_all(handshake.as_bytes()).await.unwrap();
        let mut answer = [0u8; 1024];
        let _ = socket.read(&mut answer).await.unwrap();
        RudePhone(socket)
    }

    /// One masked text frame.
    async fn send(&mut self, message: Value) {
        let body = message.to_string().into_bytes();
        assert!(body.len() < 126, "the test frames are short on purpose");
        let mask = [0x37u8, 0xfa, 0x21, 0x3d];
        let mut frame = vec![0x81, 0x80 | body.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(body.iter().enumerate().map(|(index, byte)| byte ^ mask[index % 4]));
        let _ = self.0.write_all(&frame).await;
    }

    /// Whether the server cut the line: reading ends rather than waiting.
    async fn cut(&mut self) -> bool {
        let mut buffer = [0u8; 4096];
        loop {
            match tokio::time::timeout(Duration::from_secs(3), self.0.read(&mut buffer)).await {
                Ok(Ok(0)) | Ok(Err(_)) => return true,
                Ok(Ok(_)) => continue,
                Err(_) => return false,
            }
        }
    }
}

#[tokio::test]
async fn the_server_answers_its_health_check_and_nothing_else_over_plain_http() {
    let harness = Harness::start().await;
    let port = harness.port();
    assert_eq!(fetch(port, "GET", "/m/health").await.2, MOBILE_HEALTH_RESPONSE);
    for target in ["/", "/m", "/m/", "/m/index.html", "/m/../../package.json", "/package.json"] {
        assert_eq!(fetch(port, "GET", target).await.0, 404, "{target}");
    }
    assert_eq!(fetch(port, "POST", "/m/").await.0, 405);
    assert_eq!(fetch(port, "GET", "/m/socket").await.0, 400, "the socket address is only an upgrade");
    harness.server.stop().await;
}

#[tokio::test]
async fn a_snapshot_names_this_builds_engines_modes_models_and_efforts_for_a_phone_app_built_apart_from_it() {
    let harness = Harness::start().await;
    let (_phone, _, opening) = harness.paired_phone().await;
    let vocabulary = &opening["vocabulary"];
    assert_eq!(vocabulary, &mobile_vocabulary());
    let claude = vocabulary["engines"].as_array().unwrap().iter().find(|engine| engine["id"] == "claude").unwrap();
    assert_eq!(claude["label"], "Claude");
    assert!(claude["models"].as_array().unwrap().iter().any(|model| model["id"] == "opus" && model["efforts"].as_array().unwrap().contains(&json!("high"))));
    assert_eq!(claude["policies"].as_array().unwrap().iter().map(|policy| policy["id"].as_str().unwrap()).collect::<Vec<_>>(), ["autonomous", "bypass", "allow-edits", "confirm"]);
    assert_eq!((vocabulary["efforts"][0]["id"].as_str(), vocabulary["efforts"][0]["delegates"].as_bool()), (Some("ultra"), Some(true)));
    harness.server.stop().await;
}

#[tokio::test]
async fn a_snapshot_says_the_computer_takes_a_push_token_and_a_phones_token_is_kept_on_its_pairing() {
    let harness = Harness::start().await;
    let (phone, _, opening) = harness.paired_phone().await;
    assert_eq!(opening["push"], true);
    let push = json!({ "token": "cd".repeat(32), "environment": "production" });
    phone.send(json!({ "kind": "push-token", "push": push }));
    let kept = until(|| harness.devices.lock().unwrap().push_targets().first().map(|(_, push)| push.clone()), "the token was never kept").await;
    assert_eq!(serde_json::to_value(kept).unwrap(), push);
    assert!(!phone.closed());
    harness.server.stop().await;
}

#[tokio::test]
async fn a_phone_trades_its_code_for_a_token_and_is_handed_the_view() {
    let harness = Harness::start().await;
    let (phone, paired, snapshot) = harness.paired_phone().await;
    assert_eq!(paired["sequence"], 1);
    let token = paired["token"].as_str().unwrap();
    assert!(token.len() == 64 && token.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()));
    assert_eq!(harness.devices.lock().unwrap().authenticate(token, now()).map(|device| device.id), paired["deviceId"].as_str().map(String::from));
    assert_eq!((snapshot["sequence"].as_u64(), &snapshot["view"]), (Some(2), &view("first")));
    assert_eq!(harness.server.session_views().iter().map(|session| (session.device_name.clone(), session.connection)).collect::<Vec<_>>(), [("iPhone".to_string(), MobileConnectionState::Live)]);

    let command = json!({ "type": "task.select", "taskId": "task-1" });
    phone.send(json!({ "kind": "command", "requestId": "request-1", "command": command }));
    assert_eq!(phone.wait_for("ack").await["ok"], true);
    assert_eq!(harness.commands(), [command.clone()]);

    phone.send(json!({ "kind": "command", "requestId": "request-2", "command": { "type": "task.select", "taskId": "" } }));
    let refused = until(|| phone.of_kind("ack").into_iter().find(|ack| ack["requestId"] == "request-2"), "a refusal for the command the guard turned away").await;
    assert_eq!(refused["ok"], false, "a command that fails the guard is refused rather than hung up on");
    assert_eq!(harness.commands(), [command], "and it never reaches the window");
    assert!(!phone.closed());
    harness.server.stop().await;
}

#[tokio::test]
async fn a_phone_that_drops_resumes_where_it_was_and_falls_back_to_a_snapshot_when_it_cannot() {
    let harness = Harness::start().await;
    let (first, paired, opening) = harness.paired_phone().await;
    let (token, session) = (paired["token"].clone(), opening["sessionId"].clone());
    first.close();
    harness.offline().await;
    harness.server.publish(&json!({ "kind": "patch", "patch": { "groups": [{ "projectId": null, "name": "while away", "threads": [] }] } }));

    let back = Phone::open(&harness.socket_url(), &[]).await.unwrap();
    back.send(json!({ "kind": "resume", "version": VERSION, "token": token, "sessionId": session, "lastSequence": opening["sequence"] }));
    let missed = back.wait_for("patch").await;
    assert_eq!(missed["sequence"].as_u64(), opening["sequence"].as_u64().map(|sequence| sequence + 1));
    assert_eq!(missed["patch"]["groups"][0]["name"], "while away");
    assert!(back.of_kind("snapshot").is_empty(), "a resume that worked costs no snapshot");
    assert_eq!(harness.server.session_views().len(), 1, "the session was picked up rather than replaced");

    back.close();
    harness.offline().await;
    *harness.window.snapshot.lock().unwrap() = view("second");
    for _ in 0..=MOBILE_EVENT_BUFFER {
        harness.server.publish(&json!({ "kind": "patch", "patch": { "groups": [] } }));
    }
    let behind = Phone::open(&harness.socket_url(), &[]).await.unwrap();
    behind.send(json!({ "kind": "resume", "version": VERSION, "token": token, "sessionId": session, "lastSequence": missed["sequence"] }));
    let snapshot = behind.wait_for("snapshot").await;
    let mut built = view("first");
    built["groups"] = json!([]);
    assert_eq!(snapshot["view"], built, "the view the published patches built");
    assert_ne!(snapshot["sessionId"], session, "a phone too far behind is given a session of its own");
    harness.server.stop().await;
}

#[tokio::test]
async fn a_socket_is_refused_without_a_token_this_mac_knows_and_without_an_origin_it_served() {
    let harness = Harness::start().await;
    let url = harness.socket_url();
    let stranger = Phone::open(&url, &[]).await.unwrap();
    stranger.send(json!({ "kind": "resume", "version": VERSION, "token": "0".repeat(64), "lastSequence": 0 }));
    assert_eq!(stranger.wait_for("error").await["code"], "unauthorized");
    until(|| stranger.closed().then_some(()), "the refused socket stayed open").await;
    assert!(harness.server.session_views().is_empty());

    let outdated = Phone::open(&url, &[]).await.unwrap();
    outdated.send(json!({ "kind": "pair", "version": OLDEST_PHONE_PROTOCOL_VERSION - 1, "code": harness.code(), "deviceName": "iPhone" }));
    let too_old = outdated.wait_for("error").await;
    assert_eq!(too_old["code"], "version");
    assert!(too_old["message"].as_str().unwrap().contains("Update the app"));

    let ahead = Phone::open(&url, &[]).await.unwrap();
    ahead.send(json!({ "kind": "pair", "version": VERSION + 2, "oldestComputer": VERSION + 1, "code": harness.code(), "deviceName": "iPhone" }));
    let too_new = ahead.wait_for("error").await;
    assert_eq!(too_new["code"], "version");
    assert!(too_new["message"].as_str().unwrap().contains("Update it on your computer"));

    let newer = Phone::open(&url, &[]).await.unwrap();
    newer.send(json!({ "kind": "pair", "version": VERSION + 1, "oldestComputer": VERSION, "code": harness.code(), "deviceName": "iPhone" }));
    newer.wait_for("paired").await;

    assert!(Phone::open(&url, &[("origin", &harness.origin())]).await.is_ok());
    assert_eq!(Phone::open(&url, &[("origin", "https://evil.example")]).await.err(), Some(403));
    harness.server.stop().await;
}

#[tokio::test]
async fn a_command_re_sent_after_a_lost_ack_is_answered_not_run_again() {
    let harness = Harness::start().await;
    let (phone, _, _) = harness.paired_phone().await;
    let command = json!({ "type": "task.send", "taskId": "task-1", "text": "ship it" });
    phone.send(json!({ "kind": "command", "requestId": "request-1", "command": command }));
    assert_eq!(phone.wait_for("ack").await["ok"], true);
    phone.send(json!({ "kind": "command", "requestId": "request-1", "command": command }));
    until(|| (phone.of_kind("ack").len() == 2).then_some(()), "the resend went unanswered").await;
    assert_eq!(harness.commands(), [command], "the window was asked once");
    harness.server.stop().await;
}

#[tokio::test]
async fn an_edits_outcome_outlives_the_server_so_an_edit_re_sent_after_a_restart_is_answered_not_run_again() {
    let harness = Harness::start().await;
    let (phone, paired, _) = harness.paired_phone().await;
    let edit = json!({ "type": "task.edit-queued", "taskId": "task-1", "messageId": "q1" });
    phone.send(json!({ "kind": "command", "requestId": "edit-1", "command": edit }));
    phone.wait_for("ack").await;
    phone.close();

    harness.server.stop().await;
    harness.restart().await;
    let again = Phone::open(&harness.socket_url(), &[("origin", &harness.origin())]).await.unwrap();
    again.send(json!({ "kind": "resume", "version": VERSION, "token": paired["token"], "lastSequence": 0 }));
    again.wait_for("snapshot").await;
    again.send(json!({ "kind": "command", "requestId": "edit-1", "command": edit }));
    assert_eq!(again.wait_for("ack").await["ok"], true);
    assert_eq!(harness.commands(), [edit], "the window was asked once");
    harness.server.stop().await;
}

#[tokio::test]
async fn a_phone_whose_pairing_is_revoked_loses_the_session_with_it() {
    let harness = Harness::start().await;
    let (phone, paired, _) = harness.paired_phone().await;
    let device = paired["deviceId"].as_str().unwrap();
    harness.devices.lock().unwrap().revoke(device);
    harness.server.drop_device(device);
    until(|| phone.closed().then_some(()), "the revoked phone was left connected").await;
    assert!(harness.server.session_views().is_empty());
    harness.server.stop().await;
}

#[tokio::test]
async fn a_view_the_window_will_not_give_up_is_reported_rather_than_dropped() {
    let harness = Harness::start().await;
    *harness.window.refuse_snapshot.lock().unwrap() = Some("The AI Coding Tool window is not open.".into());
    let phone = Phone::open(&harness.socket_url(), &[]).await.unwrap();
    phone.send(json!({ "kind": "pair", "version": VERSION, "code": harness.code(), "deviceName": "iPhone" }));
    let failed = phone.wait_for("error").await;
    assert_eq!(failed["code"], "internal");
    assert!(failed["message"].as_str().unwrap().contains("window is not open"));
    until(|| phone.closed().then_some(()), "the phone was left on a session with no view").await;
    assert!(harness.server.session_views().is_empty());
    harness.server.stop().await;
}

#[tokio::test]
async fn a_command_whose_acknowledgement_was_lost_before_the_session_expired_is_still_only_run_once() {
    let harness = Harness::start().await;
    let (first, paired, opening) = harness.paired_phone().await;
    let command = json!({ "type": "task.send", "taskId": "task-1", "text": "ship it" });
    first.send(json!({ "kind": "command", "requestId": "request-1", "command": command }));
    first.wait_for("ack").await;
    first.close();
    harness.offline().await;
    harness.server.inner.tick();
    assert!(harness.server.session_views().is_empty(), "the session outlived its grace");

    let back = Phone::open(&harness.socket_url(), &[]).await.unwrap();
    back.send(json!({ "kind": "resume", "version": VERSION, "token": paired["token"], "sessionId": opening["sessionId"], "lastSequence": opening["sequence"].as_u64().unwrap() + 1 }));
    back.wait_for("snapshot").await;
    back.send(json!({ "kind": "command", "requestId": "request-1", "command": command }));
    assert_eq!(back.wait_for("ack").await["ok"], true);
    assert_eq!(harness.commands(), [command], "the window ran the resent command once");
    harness.server.stop().await;
}

#[tokio::test]
async fn a_resume_with_nothing_to_replay_is_still_handed_a_frame_so_the_phone_calls_itself_live_at_once() {
    let harness = Harness::start().await;
    let (first, paired, opening) = harness.paired_phone().await;
    first.close();
    let back = Phone::open(&harness.socket_url(), &[]).await.unwrap();
    back.send(json!({ "kind": "resume", "version": VERSION, "token": paired["token"], "sessionId": opening["sessionId"], "lastSequence": opening["sequence"] }));
    let ping = back.wait_for("ping").await;
    assert_eq!(ping["sequence"].as_u64(), opening["sequence"].as_u64().map(|sequence| sequence + 1), "the frame is numbered after what the phone already saw");
    assert!(back.of_kind("snapshot").is_empty());
    harness.server.stop().await;
}

#[tokio::test]
async fn a_page_served_through_a_proxy_is_let_in_by_the_host_it_was_served_on_and_wrong_codes_count_against_the_phone_behind_the_proxy() {
    let harness = Harness::start().await;
    let url = harness.socket_url();
    let proxied = [("origin", "https://mac.tail1234.ts.net"), ("x-forwarded-host", "mac.tail1234.ts.net"), ("x-forwarded-proto", "https"), ("x-forwarded-for", "100.64.0.7")];
    assert!(Phone::open(&url, &proxied).await.is_ok());
    let mut foreign = proxied;
    foreign[0] = ("origin", "https://evil.example");
    assert_eq!(Phone::open(&url, &foreign).await.err(), Some(403));

    harness.code();
    for _ in 0..5 {
        let guess = Phone::open(&url, &proxied).await.unwrap();
        guess.send(json!({ "kind": "pair", "version": VERSION, "code": "WRONGONE", "deviceName": "iPhone" }));
        guess.wait_for("error").await;
    }
    let mut elsewhere = proxied;
    elsewhere[3] = ("x-forwarded-for", "100.64.0.8");
    let other = Phone::open(&url, &elsewhere).await.unwrap();
    other.send(json!({ "kind": "pair", "version": VERSION, "code": harness.code(), "deviceName": "iPad" }));
    other.wait_for("paired").await;
    harness.server.stop().await;
}

#[tokio::test]
async fn a_phones_request_is_answered_by_the_window_or_by_the_wait_running_out() {
    let short = Duration::from_millis(40);
    let sent: Arc<Mutex<Vec<MobileRequest>>> = Arc::default();
    let kept = sent.clone();
    let relay = MobileRelay::waiting(Arc::new(move |request| { kept.lock().unwrap().push(request); true }), short, short, short);
    let asked = relay.clone();
    let answered = tokio::spawn(async move { asked.snapshot("session-1").await });
    let first = until(|| sent.lock().unwrap().first().map(|request| request.request_id.clone()), "the request never reached the window").await;
    relay.answer(MobileResponse { request_id: first.clone(), result: Ok(view("first")) });
    assert_eq!(answered.await.unwrap(), Ok(view("first")));
    relay.answer(MobileResponse { request_id: first, result: Ok(view("first")) });

    let before = now();
    let asked = relay.clone();
    let refused = tokio::spawn(async move { asked.command("session-1", json!({ "type": "task.select", "taskId": "task-1" })).await });
    let second = until(|| sent.lock().unwrap().get(1).cloned(), "the command never reached the window").await;
    assert!(second.answer_by >= before + 40, "the window is told when the relay stops waiting");
    relay.answer(MobileResponse { request_id: second.request_id, result: Err("No such thread.".into()) });
    assert_eq!(refused.await.unwrap(), Err("No such thread.".into()));

    assert!(relay.snapshot("session-1").await.unwrap_err().contains("did not answer"));
    let closed = MobileRelay::new(Arc::new(|_| false));
    assert!(closed.snapshot("session-1").await.unwrap_err().contains("window is not open"));

    let slow: Arc<Mutex<Vec<MobileRequest>>> = Arc::default();
    let held = slow.clone();
    let transcribing = MobileRelay::waiting(Arc::new(move |request| { held.lock().unwrap().push(request); true }), short, Duration::from_millis(400), short);
    let asked = transcribing.clone();
    let heard = tokio::spawn(async move { asked.query("session-1", json!({ "kind": "transcribe", "audio": "UklGRg==" })).await });
    tokio::time::sleep(Duration::from_millis(120)).await;
    let id = slow.lock().unwrap()[0].request_id.clone();
    transcribing.answer(MobileResponse { request_id: id, result: Ok(json!({ "text": "words" })) });
    assert_eq!(heard.await.unwrap(), Ok(json!({ "text": "words" })), "a recording waits longer than any other request");
    assert!(transcribing.query("session-1", json!({ "kind": "directories", "prefix": "/" })).await.unwrap_err().contains("did not answer"));
    let held = slow.clone();
    let discovering = MobileRelay::waiting(Arc::new(move |request| { held.lock().unwrap().push(request); true }), short, short, Duration::from_millis(400));
    let asked = discovering.clone();
    let listed = tokio::spawn(async move { asked.query("session-1", json!({ "kind": "commands", "taskId": "t1" })).await });
    tokio::time::sleep(Duration::from_millis(120)).await;
    let id = slow.lock().unwrap().last().unwrap().request_id.clone();
    discovering.answer(MobileResponse { request_id: id, result: Ok(json!({ "status": "available", "commands": [] })) });
    assert_eq!(listed.await.unwrap(), Ok(json!({ "status": "available", "commands": [] })), "a command list waits out an engine starting to list its skills");

    let abandoned = MobileRelay::new(Arc::new(|_| true));
    let asked = abandoned.clone();
    let waiting = tokio::spawn(async move { asked.snapshot("session-1").await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    abandoned.fail_all("The phone bridge was turned off.");
    assert!(waiting.await.unwrap().unwrap_err().contains("turned off"));
}

#[tokio::test]
async fn every_response_says_it_may_not_be_framed() {
    let harness = Harness::start().await;
    for target in ["/m/", "/m/health", "/m/socket"] {
        let (_, headers, _) = fetch(harness.port(), "GET", target).await;
        let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok()).unwrap_or("").to_string();
        assert_eq!(header("x-frame-options"), "DENY", "{target}");
        assert!(header("content-security-policy").contains("frame-ancestors 'none'"), "{target}");
        assert_eq!(header("x-content-type-options"), "nosniff", "{target}");
    }
    harness.server.stop().await;
}

#[tokio::test]
async fn a_revoked_phone_that_will_not_hang_up_is_cut_off_rather_than_left_issuing_commands() {
    let harness = Harness::start().await;
    let code = harness.code();
    let mut rude = RudePhone::open(harness.port()).await;
    rude.send(json!({ "kind": "pair", "version": VERSION, "code": code, "deviceName": "Stolen" })).await;
    until(|| (harness.server.session_views().len() == 1).then_some(()), "the rude phone never paired").await;
    let device = harness.devices.lock().unwrap().list()[0].id.clone();

    rude.send(json!({ "kind": "command", "requestId": "before", "command": { "type": "task.select", "taskId": "task-1" } })).await;
    until(|| (harness.commands().len() == 1).then_some(()), "the paired phone's command never arrived").await;

    harness.devices.lock().unwrap().revoke(&device);
    harness.server.drop_device(&device);
    rude.send(json!({ "kind": "command", "requestId": "after", "command": { "type": "task.send", "taskId": "task-1", "text": "rm -rf ~" } })).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(harness.commands().len(), 1, "a revoked phone reached the window again");
    assert!(harness.server.session_views().is_empty());
    assert!(rude.cut().await, "the revoked socket was left alive");
    harness.server.stop().await;
}

#[tokio::test]
async fn turning_the_bridge_off_finishes_even_while_a_phone_refuses_to_hang_up() {
    let harness = Harness::start().await;
    let code = harness.code();
    let mut rude = RudePhone::open(harness.port()).await;
    rude.send(json!({ "kind": "pair", "version": VERSION, "code": code, "deviceName": "Stolen" })).await;
    until(|| (harness.server.session_views().len() == 1).then_some(()), "the rude phone never paired").await;
    let started = std::time::Instant::now();
    harness.server.stop().await;
    assert!(started.elapsed() < Duration::from_secs(5), "stop took {:?}", started.elapsed());
    assert_eq!(harness.server.port(), None);
}

#[tokio::test]
async fn a_command_a_phone_re_sends_after_falling_past_the_buffer_is_still_only_run_once() {
    let harness = Harness::start().await;
    let (first, paired, opening) = harness.paired_phone().await;
    let command = json!({ "type": "task.send", "taskId": "task-1", "text": "deploy to prod" });
    first.send(json!({ "kind": "command", "requestId": "request-1", "command": command }));
    until(|| (harness.commands().len() == 1).then_some(()), "the command never reached the window").await;
    first.close();
    harness.offline().await;
    for _ in 0..=MOBILE_EVENT_BUFFER {
        harness.server.publish(&json!({ "kind": "patch", "patch": { "groups": [] } }));
    }

    let behind = Phone::open(&harness.socket_url(), &[]).await.unwrap();
    behind.send(json!({ "kind": "resume", "version": VERSION, "token": paired["token"], "sessionId": opening["sessionId"], "lastSequence": opening["sequence"] }));
    let fresh = behind.wait_for("snapshot").await;
    assert_ne!(fresh["sessionId"], opening["sessionId"], "the phone fell too far behind to resume");
    behind.send(json!({ "kind": "command", "requestId": "request-1", "command": command }));
    behind.wait_for("ack").await;
    assert_eq!(harness.commands(), [command], "the window was asked once across two sessions");
    harness.server.stop().await;
}

#[tokio::test]
async fn what_a_phone_missed_before_its_first_snapshot_is_not_sent_with_the_wrong_numbering() {
    let harness = Harness::start().await;
    let release = harness.hold();
    let phone = Phone::open(&harness.socket_url(), &[]).await.unwrap();
    phone.send(json!({ "kind": "pair", "version": VERSION, "code": harness.code(), "deviceName": "iPhone" }));
    phone.wait_for("paired").await;

    harness.server.publish(&json!({ "kind": "patch", "patch": { "groups": [{ "projectId": null, "name": "too early", "threads": [] }] } }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(phone.of_kind("patch").is_empty(), "a patch outran the snapshot it belongs after");

    release();
    let snapshot = phone.wait_for("snapshot").await;
    harness.server.publish(&json!({ "kind": "patch", "patch": { "groups": [{ "projectId": null, "name": "in order", "threads": [] }] } }));
    let patch = phone.wait_for("patch").await;
    assert_eq!(patch["sequence"].as_u64(), snapshot["sequence"].as_u64().map(|sequence| sequence + 1));
    harness.server.stop().await;
}

#[tokio::test]
async fn sockets_that_never_say_who_they_are_cannot_be_piled_up_without_limit() {
    let harness = Harness::start().await;
    let mut idle = Vec::new();
    for _ in 0..MAX_PENDING_SOCKETS {
        idle.push(Phone::open(&harness.socket_url(), &[]).await.unwrap());
    }
    assert_eq!(Phone::open(&harness.socket_url(), &[]).await.err(), Some(503));
    harness.server.stop().await;
}

#[tokio::test]
async fn a_read_is_answered_with_what_the_window_returned_refused_with_why_and_never_remembered() {
    let harness = Harness::start().await;
    let (phone, _, _) = harness.paired_phone().await;
    let query = json!({ "kind": "diff-patch", "taskId": "task-1", "range": { "kind": "uncommitted" }, "path": "a.ts" });
    phone.send(json!({ "kind": "query", "requestId": "q1", "query": query }));
    let answer = phone.wait_for("answer").await;
    assert_eq!(answer, json!({ "kind": "answer", "sequence": answer["sequence"], "requestId": "q1", "ok": true, "result": { "status": "available", "patch": "" } }));

    phone.send(json!({ "kind": "query", "requestId": "q1", "query": query }));
    until(|| (phone.of_kind("answer").len() == 2).then_some(()), "the same read asked again is read again").await;
    assert_eq!(harness.window.queries.lock().unwrap().len(), 2);

    *harness.window.refuse_query.lock().unwrap() = Some("The checkout is gone.".into());
    phone.send(json!({ "kind": "query", "requestId": "q2", "query": query }));
    let refused = until(|| phone.of_kind("answer").into_iter().find(|answer| answer["requestId"] == "q2"), "a refusal").await;
    assert_eq!(refused, json!({ "kind": "answer", "sequence": refused["sequence"], "requestId": "q2", "ok": false, "message": "The checkout is gone." }));
    harness.server.stop().await;
}
