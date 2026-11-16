//! One newline-delimited JSON-RPC 2.0 conversation with a `codex app-server` process. Client requests
//! are answered by id; server requests reach request handlers and must be answered once;
//! notifications reach handlers by method. The child exiting rejects everything still pending.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use super::shared_auth::{acquire_shared_auth, SharedAuth};

const STDERR_KEEP: usize = 16 * 1024;
/// How long after `exit` the stdio pipes get to drain before the client stops waiting for them.
const DRAIN: Duration = Duration::from_millis(250);
const KILL_GRACE: Duration = Duration::from_millis(2_000);
const METHOD_NOT_FOUND: i64 = -32601;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub stderr: String,
}

impl ExitStatus {
    fn how(&self) -> String {
        match (self.signal, self.code) {
            (Some(signal), _) => format!("signal {}", signal_name(signal)),
            (None, Some(code)) => format!("code {code}"),
            (None, None) => "code null".to_string(),
        }
    }
}

pub fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGTERM => "SIGTERM".into(),
        libc::SIGKILL => "SIGKILL".into(),
        libc::SIGINT => "SIGINT".into(),
        libc::SIGHUP => "SIGHUP".into(),
        libc::SIGSEGV => "SIGSEGV".into(),
        libc::SIGABRT => "SIGABRT".into(),
        other => other.to_string(),
    }
}

/// How an app-server failed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AppServerError {
    /// The server answered a request with a JSON-RPC error.
    #[error("{method}: {message}")]
    Rpc {
        method: String,
        code: i64,
        message: String,
        data: Value,
    },
    /// The server process is gone, so no request can be answered.
    #[error("Codex app server exited with {} {context}{}", exit.how(), if exit.stderr.is_empty() { String::new() } else { format!("\n{}", exit.stderr) })]
    Exited { exit: ExitStatus, context: String },
}

impl AppServerError {
    /// What went wrong, without the method the server was answering when it did.
    pub fn reason(&self) -> String {
        match self {
            AppServerError::Rpc { message, .. } => {
                crate::session_turn::first_line(message).to_string()
            }
            other => crate::session_turn::first_line(&other.to_string()).to_string(),
        }
    }
}

/// What a server process is started as.
#[derive(Debug, Clone, Default)]
pub struct AppServerCommand {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    /// Keychain credentials stay with Codex under the user's home; a private conversation home
    /// borrows them from a server started with this command.
    pub shared_auth: Option<Box<AppServerCommand>>,
}

/// A request the server sent. Exactly one of `respond` and `fail` must be called.
pub struct IncomingRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
    reply: Arc<Mutex<Option<Box<dyn FnOnce(Value) + Send>>>>,
}

impl IncomingRequest {
    pub fn respond(&self, result: Value) {
        if let Some(reply) = self.reply.lock().unwrap().take() {
            reply(json!({ "result": result }));
        }
    }

    pub fn fail(&self, code: i64, message: &str) {
        if let Some(reply) = self.reply.lock().unwrap().take() {
            reply(json!({ "error": { "code": code, "message": message } }));
        }
    }

    pub fn clone_handle(&self) -> IncomingRequest {
        IncomingRequest {
            id: self.id.clone(),
            method: self.method.clone(),
            params: self.params.clone(),
            reply: self.reply.clone(),
        }
    }
}

type Pending = (String, oneshot::Sender<Result<Value, AppServerError>>);
type NotificationHandler = Arc<dyn Fn(Value) + Send + Sync>;
type RequestHandler = Arc<dyn Fn(IncomingRequest) + Send + Sync>;

struct Inner {
    writer: Mutex<Option<mpsc::UnboundedSender<String>>>,
    pending: Mutex<HashMap<u64, Pending>>,
    notifications: Mutex<HashMap<String, Vec<(u64, NotificationHandler)>>>,
    requests: Mutex<Vec<(u64, RequestHandler)>>,
    next_id: AtomicU64,
    next_handler: AtomicU64,
    exit: Mutex<Option<ExitStatus>>,
    pid: Option<u32>,
    shared_auth_command: Option<Box<AppServerCommand>>,
    shared_auth: Mutex<Option<SharedAuth>>,
}

/// The client's end of one app-server process.
#[derive(Clone)]
pub struct AppServerClient {
    inner: Arc<Inner>,
    exited: Shared<BoxFuture<'static, ExitStatus>>,
}

/// How the app introduces itself to the server.
pub fn client_info() -> Value {
    json!({ "name": "aicodingtool", "title": "AICodingTool", "version": "1" })
}

impl AppServerClient {
    /// Starts the server in a process group of its own, so closing reaches what it started under it.
    pub fn connect(command: AppServerCommand) -> AppServerClient {
        let mut process = Command::new(&command.executable);
        process
            .args(&command.args)
            .env_clear()
            .envs(&command.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &command.cwd {
            process.current_dir(cwd);
        }
        process.process_group(0);
        let (exit_tx, exit_rx) = oneshot::channel::<ExitStatus>();
        let exited: Shared<BoxFuture<'static, ExitStatus>> = async move {
            exit_rx.await.unwrap_or(ExitStatus {
                code: None,
                signal: None,
                stderr: String::new(),
            })
        }
        .boxed()
        .shared();
        let spawned = process.spawn();
        if let Ok(child) = &spawned {
            crate::session_resources::register_session_process(child.id());
        }
        let (writer, pid) = match &spawned {
            Ok(child) => (None, child.id()),
            Err(_) => (None, None),
        };
        let inner = Arc::new(Inner {
            writer: Mutex::new(writer),
            pending: Mutex::new(HashMap::new()),
            notifications: Mutex::new(HashMap::new()),
            requests: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            next_handler: AtomicU64::new(1),
            exit: Mutex::new(None),
            pid,
            shared_auth_command: command.shared_auth.clone(),
            shared_auth: Mutex::new(None),
        });
        let client = AppServerClient {
            inner: inner.clone(),
            exited,
        };
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                client.finish(
                    ExitStatus {
                        code: None,
                        signal: None,
                        stderr: error.to_string(),
                    },
                    exit_tx,
                );
                return client;
            }
        };
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();
        *inner.writer.lock().unwrap() = Some(write_tx);
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = write_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
            let _ = stdin.shutdown().await;
        });
        let tail = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stderr_tail = tail.clone();
        let stderr_done = tokio::spawn(async move {
            let mut buffer = [0u8; 8192];
            while let Ok(read) = stderr.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                let mut tail = stderr_tail.lock().unwrap();
                tail.extend_from_slice(&buffer[..read]);
                if tail.len() > STDERR_KEEP * 2 {
                    let cut = tail.len() - STDERR_KEEP;
                    tail.drain(..cut);
                }
            }
        });
        let reader = client.clone();
        let stdout_done = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).split(b'\n');
            while let Ok(Some(line)) = lines.next_segment().await {
                reader.handle_line(&line);
            }
        });
        let finisher = client.clone();
        tokio::spawn(async move {
            let status = child.wait().await.ok();
            let _ = tokio::time::timeout(DRAIN, async {
                let _ = stdout_done.await;
                let _ = stderr_done.await;
            })
            .await;
            use std::os::unix::process::ExitStatusExt;
            let stderr = String::from_utf8_lossy(&tail.lock().unwrap())
                .trim()
                .to_string();
            let exit = ExitStatus {
                code: status.and_then(|s| s.code()),
                signal: status.and_then(|s| s.signal()),
                stderr,
            };
            finisher.finish(exit, exit_tx);
        });
        client
    }

    /// The `initialize` / `initialized` handshake the server needs before any other method, and the
    /// shared sign-in a private home borrows.
    pub async fn initialize(&self, client_info: Value) -> Result<Value, AppServerError> {
        let server = self.request("initialize", json!({ "clientInfo": client_info, "capabilities": { "experimentalApi": true, "requestAttestation": false } })).await?;
        self.send(&json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        if let Some(command) = self.inner.shared_auth_command.clone() {
            let borrowed = async {
                let auth = acquire_shared_auth(*command).await?;
                let exited = self.inner.exit.lock().unwrap().clone();
                if let Some(exit) = exited {
                    auth.release().await;
                    return Err(AppServerError::Exited {
                        exit,
                        context: "during authentication".into(),
                    });
                }
                let login = auth.read(false).await;
                *self.inner.shared_auth.lock().unwrap() = Some(auth);
                if let Some(login) = login.map_err(|message| AppServerError::Rpc {
                    method: "getAuthStatus".into(),
                    code: -32603,
                    message,
                    data: Value::Null,
                })? {
                    self.request("account/login/start", login).await?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = borrowed {
                self.close().await;
                return Err(error);
            }
        }
        Ok(server)
    }

    pub fn request(
        &self,
        method: &str,
        params: Value,
    ) -> impl std::future::Future<Output = Result<Value, AppServerError>> + Send + 'static {
        let exit = self.inner.exit.lock().unwrap().clone();
        let (sender, receiver) = oneshot::channel();
        let method_name = method.to_string();
        if exit.is_none() {
            let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
            self.inner
                .pending
                .lock()
                .unwrap()
                .insert(id, (method.to_string(), sender));
            let message = if params.is_null() {
                json!({ "jsonrpc": "2.0", "id": id, "method": method })
            } else {
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
            };
            self.send(&message);
        }
        async move {
            if let Some(exit) = exit {
                return Err(AppServerError::Exited {
                    exit,
                    context: format!("before {method_name}"),
                });
            }
            receiver.await.unwrap_or_else(|_| {
                Err(AppServerError::Exited {
                    exit: ExitStatus {
                        code: None,
                        signal: None,
                        stderr: String::new(),
                    },
                    context: format!("while {method_name} was pending"),
                })
            })
        }
    }

    /// Listens for one notification method until the returned id is passed to `off`.
    pub fn on(&self, method: &str, handler: impl Fn(Value) + Send + Sync + 'static) -> u64 {
        let id = self.inner.next_handler.fetch_add(1, Ordering::Relaxed);
        self.inner
            .notifications
            .lock()
            .unwrap()
            .entry(method.to_string())
            .or_default()
            .push((id, Arc::new(handler)));
        id
    }

    pub fn off(&self, handler: u64) {
        for handlers in self.inner.notifications.lock().unwrap().values_mut() {
            handlers.retain(|(id, _)| *id != handler);
        }
        self.inner
            .requests
            .lock()
            .unwrap()
            .retain(|(id, _)| *id != handler);
    }

    /// Server requests with no handler are refused with method-not-found so the server never waits.
    pub fn on_request(&self, handler: impl Fn(IncomingRequest) + Send + Sync + 'static) -> u64 {
        let id = self.inner.next_handler.fetch_add(1, Ordering::Relaxed);
        self.inner
            .requests
            .lock()
            .unwrap()
            .push((id, Arc::new(handler)));
        id
    }

    pub fn exited(&self) -> Shared<BoxFuture<'static, ExitStatus>> {
        self.exited.clone()
    }

    pub fn has_exited(&self) -> bool {
        self.inner.exit.lock().unwrap().is_some()
    }

    /// Ends the conversation and its process group: a healthy server leaves on its own, the rest is signalled.
    /// Ends the input and asks the process group to stop, at once; `close` then waits it out.
    pub fn terminate(&self) {
        if self.inner.exit.lock().unwrap().is_none() {
            self.inner.writer.lock().unwrap().take();
            self.signal(libc::SIGTERM);
        }
    }

    pub async fn close(&self) -> ExitStatus {
        if self.inner.exit.lock().unwrap().is_none() {
            self.terminate();
            if tokio::time::timeout(KILL_GRACE, self.exited.clone())
                .await
                .is_err()
            {
                self.signal(libc::SIGKILL);
            }
        }
        let exit = self.exited.clone().await;
        let auth = self.inner.shared_auth.lock().unwrap().take();
        if let Some(auth) = auth {
            auth.release().await;
        }
        exit
    }

    fn signal(&self, signal: i32) {
        if let Some(pid) = self.inner.pid {
            // SAFETY: signals the server's own process group, falling back to the process.
            unsafe {
                if libc::kill(-(pid as i32), signal) != 0 {
                    libc::kill(pid as i32, signal);
                }
            }
        }
    }

    fn send(&self, message: &Value) {
        if let Some(writer) = self.inner.writer.lock().unwrap().as_ref() {
            let _ = writer.send(format!("{message}\n"));
        }
    }

    fn handle_line(&self, line: &[u8]) {
        if line.is_empty() {
            return;
        }
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        if !message.is_object() {
            return;
        }
        if let Some(method) = message["method"].as_str() {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            match message.get("id") {
                None => self.dispatch_notification(method, params),
                Some(id) => self.dispatch_request(id.clone(), method.to_string(), params),
            }
        } else if let Some(id) = message["id"].as_u64() {
            let pending = self.inner.pending.lock().unwrap().remove(&id);
            if let Some((method, sender)) = pending {
                let result = match message.get("error") {
                    Some(error) if !error.is_null() => Err(AppServerError::Rpc {
                        method,
                        code: error["code"].as_i64().unwrap_or(0),
                        message: error["message"].as_str().unwrap_or_default().to_string(),
                        data: error.get("data").cloned().unwrap_or(Value::Null),
                    }),
                    _ => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = sender.send(result);
            }
        }
    }

    fn dispatch_notification(&self, method: &str, params: Value) {
        let handlers: Vec<NotificationHandler> = self
            .inner
            .notifications
            .lock()
            .unwrap()
            .get(method)
            .map(|handlers| {
                handlers
                    .iter()
                    .map(|(_, handler)| handler.clone())
                    .collect()
            })
            .unwrap_or_default();
        for handler in handlers {
            handler(params.clone());
        }
    }

    fn dispatch_request(&self, id: Value, method: String, params: Value) {
        let writer = self.clone();
        let reply_id = id.clone();
        let reply: Box<dyn FnOnce(Value) + Send> = Box::new(move |body: Value| {
            if writer.has_exited() {
                return;
            }
            let mut message = json!({ "jsonrpc": "2.0", "id": reply_id });
            if let (Some(target), Some(body)) = (message.as_object_mut(), body.as_object()) {
                for (key, value) in body {
                    target.insert(key.clone(), value.clone());
                }
            }
            writer.send(&message);
        });
        let request = IncomingRequest {
            id,
            method: method.clone(),
            params,
            reply: Arc::new(Mutex::new(Some(reply))),
        };
        if method == "account/chatgptAuthTokens/refresh" {
            let auth = self.inner.shared_auth.lock().unwrap().clone();
            if let Some(auth) = auth {
                tokio::spawn(async move {
                    let previous = request.params["previousAccountId"]
                        .as_str()
                        .map(String::from);
                    match auth.read(true).await {
                        Ok(Some(login)) if login["type"] == "chatgptAuthTokens" => {
                            let account = login["chatgptAccountId"].as_str().unwrap_or_default();
                            if previous.as_deref().is_some_and(|previous| !previous.is_empty() && previous != account) {
                                request.fail(-32603, "Could not refresh the shared Codex account. Check Codex sign-in and start a new run.");
                            } else {
                                request.respond(json!({ "accessToken": login["accessToken"], "chatgptAccountId": login["chatgptAccountId"], "chatgptPlanType": login.get("chatgptPlanType").cloned().unwrap_or(Value::Null) }));
                            }
                        }
                        _ => request.fail(-32603, "Could not refresh the shared Codex account. Check Codex sign-in and start a new run."),
                    }
                });
                return;
            }
        }
        let handlers: Vec<RequestHandler> = self
            .inner
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, handler)| handler.clone())
            .collect();
        if handlers.is_empty() {
            request.fail(METHOD_NOT_FOUND, &format!("No handler for {method}"));
            return;
        }
        for handler in handlers {
            handler(request.clone_handle());
        }
    }

    fn finish(&self, exit: ExitStatus, exit_tx: oneshot::Sender<ExitStatus>) {
        {
            let mut current = self.inner.exit.lock().unwrap();
            if current.is_some() {
                return;
            }
            *current = Some(exit.clone());
        }
        self.inner.writer.lock().unwrap().take();
        let pending: Vec<Pending> = self
            .inner
            .pending
            .lock()
            .unwrap()
            .drain()
            .map(|(_, pending)| pending)
            .collect();
        for (method, sender) in pending {
            let _ = sender.send(Err(AppServerError::Exited {
                exit: exit.clone(),
                context: format!("while {method} was pending"),
            }));
        }
        let _ = exit_tx.send(exit);
    }
}

/// The command a run's own app-server process is started with.
pub type CodexConnect = Arc<dyn Fn(AppServerCommand) -> AppServerClient + Send + Sync>;

pub fn connect_app_server() -> CodexConnect {
    Arc::new(AppServerClient::connect)
}
