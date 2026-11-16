//! A private OpenCode server on the loopback interface, on a port of the system's choosing, behind
//! the password it makes up for itself, and the client that speaks to it over HTTP and its event feed.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use futures::future::{BoxFuture, FutureExt, Shared};
use futures::StreamExt;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::{ChildStdin, Command};
use tokio::task::AbortHandle;

use crate::binary::{engine_binary_path, OPENCODE_BINARY};

/// Long enough for a cold start that loads the model catalogue, short enough that a stuck one is noticed.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a server asked to stop has before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// No request the app makes waits on a model, so one that takes this long is stuck.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const STDERR_LIMIT: usize = 8_192;
const LOADING_RETRY: Duration = Duration::from_millis(500);
const LOADING_ATTEMPTS: u32 = 10;

/// Told to every listener when the feed ends while the server is still meant to be up.
pub const FEED_ENDED: &str = "aicodingtool.feed.ended";

/// The server does not notice the app going away, so it runs under a shell that stops it once the
/// app's end of stdin closes, whether the app closed it or died, and kills it if it will not stop.
pub const WATCHED_SERVE: &str = "exec 3<&0\n\"$0\" serve --hostname 127.0.0.1 --port 0 </dev/null &\nserver=$!\n( read _ <&3; kill $server 2>/dev/null; sleep 3; kill -9 $server 2>/dev/null ) &\nwatcher=$!\nwait $server\nstatus=$?\nkill $watcher 2>/dev/null\nexit $status";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub stderr: String,
}

impl ExitStatus {
    /// Why the server went, in one line.
    pub fn describe(&self) -> String {
        let line = self
            .stderr
            .trim()
            .split('\n')
            .next()
            .unwrap_or_default()
            .to_string();
        if !line.is_empty() {
            return line;
        }
        match (self.signal, self.code) {
            (Some(signal), _) => {
                format!("signal {}", crate::codex::app_server::signal_name(signal))
            }
            (None, Some(code)) => format!("exit code {code}"),
            _ => "exit code null".into(),
        }
    }
}

/// A request the server answered with an error, kept with the tag OpenCode gave it.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct OpenCodeError {
    pub status: u16,
    pub tag: Option<String>,
    pub message: String,
}

/// One frame of the server's event feed.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenCodeEvent {
    pub kind: String,
    pub data: Value,
}

#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    /// Folder the server starts in, which is the location a request names none for.
    pub cwd: String,
    /// Inline configuration, merged over the user's own.
    pub config: Option<Value>,
}

fn listening(output: &str) -> Option<String> {
    let lower = output.to_lowercase();
    let at = lower.find("listening on ")? + "listening on ".len();
    let rest = &output[at..];
    let url: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
    (url.starts_with("http://") || url.starts_with("https://")).then_some(url)
}

fn password(output: &str) -> Option<String> {
    let lower = output.to_lowercase();
    let at = lower.find("server password ")? + "server password ".len();
    let word: String = output[at..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    (!word.is_empty()).then_some(word)
}

/// For a server that never said how to reach it in words this build reads: the last line naming
/// what is still missing. None for a slow start.
pub fn unread_line(output: &str) -> Option<String> {
    let mut missing = Vec::new();
    if listening(output).is_none() {
        missing.push("listening");
    }
    if password(output).is_none() {
        missing.push("password");
    }
    output
        .split('\n')
        .rfind(|line| {
            missing
                .iter()
                .any(|word| line.to_lowercase().contains(word))
        })
        .map(|line| line.trim().to_string())
}

type Listener = Arc<dyn Fn(OpenCodeEvent) + Send + Sync>;

struct Inner {
    url: String,
    authorization: String,
    http: reqwest::Client,
    listeners: Mutex<Vec<(u64, Listener)>>,
    next_listener: AtomicU64,
    feed: Mutex<Option<AbortHandle>>,
    closing: AtomicBool,
    stdin: Mutex<Option<ChildStdin>>,
    pid: Option<u32>,
    exited: Shared<BoxFuture<'static, ExitStatus>>,
    loaded: Mutex<HashSet<String>>,
}

#[derive(Clone)]
pub struct OpenCodeServer {
    inner: Arc<Inner>,
}

pub type OpenCodeConnect =
    Arc<dyn Fn(ServerOptions) -> BoxFuture<'static, anyhow::Result<OpenCodeServer>> + Send + Sync>;

pub fn connect_open_code() -> OpenCodeConnect {
    Arc::new(|options| start_open_code_server(options).boxed())
}

/// Starts a private OpenCode server and connects to it.
pub async fn start_open_code_server(options: ServerOptions) -> anyhow::Result<OpenCodeServer> {
    let binary = engine_binary_path(&OPENCODE_BINARY)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "opencode".into());
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", WATCHED_SERVE, &binary])
        .current_dir(&options.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(config) = &options.config {
        command.env("OPENCODE_CONFIG_CONTENT", config.to_string());
    }
    let mut child = command.spawn()?;
    crate::session_resources::register_session_process(child.id());
    let stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let pid = child.id();
    let stderr_text = Arc::new(Mutex::new(String::new()));
    let tail = stderr_text.clone();
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        while let Ok(read) = stderr.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            let mut text = tail.lock().unwrap();
            text.push_str(&String::from_utf8_lossy(&buffer[..read]));
            if text.len() > STDERR_LIMIT {
                let cut = text.len() - STDERR_LIMIT;
                let cut = (cut..text.len())
                    .find(|index| text.is_char_boundary(*index))
                    .unwrap_or(text.len());
                text.drain(..cut);
            }
        }
    });
    let exited: Shared<BoxFuture<'static, ExitStatus>> = async move {
        use std::os::unix::process::ExitStatusExt;
        let status = child.wait().await.ok();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let stderr = stderr_text.lock().unwrap().clone();
        ExitStatus {
            code: status.and_then(|s| s.code()),
            signal: status.and_then(|s| s.signal()),
            stderr,
        }
    }
    .boxed()
    .shared();
    tokio::spawn(exited.clone());

    let mut output = String::new();
    let reading = async {
        let mut buffer = [0u8; 4096];
        loop {
            let read = stdout.read(&mut buffer).await.unwrap_or(0);
            if read == 0 {
                return None;
            }
            output.push_str(&String::from_utf8_lossy(&buffer[..read]));
            if let (Some(url), Some(password)) = (listening(&output), password(&output)) {
                return Some((url, password));
            }
        }
    };
    let found = tokio::select! {
        found = tokio::time::timeout(START_TIMEOUT, reading) => found,
        status = exited.clone() => {
            drop(stdin);
            anyhow::bail!("{}", status.describe());
        }
    };
    let (url, password) = match found {
        Ok(Some(found)) => found,
        Ok(None) => {
            let status = exited.clone().await;
            anyhow::bail!("{}", status.describe());
        }
        Err(_) => {
            drop(stdin);
            if let Some(pid) = pid {
                // SAFETY: ends the shell that watches a server that never came up.
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }
            match unread_line(&output) {
                Some(line) => anyhow::bail!("OpenCode started, but this version of the app cannot read how to reach it (\"{line}\"). Update AI Coding Tool, or use the OpenCode version it was built for."),
                None => anyhow::bail!("OpenCode did not start in time."),
            }
        }
    };
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        while stdout.read(&mut buffer).await.is_ok_and(|read| read > 0) {}
    });
    let authorization = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
    );
    Ok(OpenCodeServer {
        inner: Arc::new(Inner {
            url: url.trim_end_matches('/').to_string(),
            authorization,
            http: reqwest::Client::new(),
            listeners: Mutex::new(Vec::new()),
            next_listener: AtomicU64::new(1),
            feed: Mutex::new(None),
            closing: AtomicBool::new(false),
            stdin: Mutex::new(stdin),
            pid,
            exited,
            loaded: Mutex::new(HashSet::new()),
        }),
    })
}

impl OpenCodeServer {
    pub fn exited(&self) -> Shared<BoxFuture<'static, ExitStatus>> {
        self.inner.exited.clone()
    }

    /// One request; most answers wrap their value with the location they were read in, which is unwrapped.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        timeout: Option<Duration>,
    ) -> anyhow::Result<Value> {
        let method = reqwest::Method::from_bytes(method.as_bytes())?;
        let mut request = self
            .inner
            .http
            .request(method, format!("{}{path}", self.inner.url))
            .timeout(timeout.unwrap_or(REQUEST_TIMEOUT))
            .header("authorization", &self.inner.authorization);
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        let parsed: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        if !status.is_success() {
            let tag = parsed["_tag"].as_str().map(String::from);
            let message = parsed["message"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| {
                    format!(
                        "{} {}",
                        status.as_u16(),
                        status.canonical_reason().unwrap_or_default()
                    )
                });
            return Err(OpenCodeError {
                status: status.as_u16(),
                tag,
                message,
            }
            .into());
        }
        Ok(match parsed {
            Value::Object(mut record) if record.contains_key("data") => {
                record.remove("data").unwrap_or(Value::Null)
            }
            other => other,
        })
    }

    /// Calls `listener` for every event until the server goes; `unsubscribe` with the id stops it.
    pub fn subscribe(&self, listener: impl Fn(OpenCodeEvent) + Send + Sync + 'static) -> u64 {
        let id = self.inner.next_listener.fetch_add(1, Ordering::Relaxed);
        self.inner
            .listeners
            .lock()
            .unwrap()
            .push((id, Arc::new(listener)));
        let mut feed = self.inner.feed.lock().unwrap();
        if feed.is_none() {
            let server = self.clone();
            *feed = Some(tokio::spawn(async move { server.listen().await }).abort_handle());
        }
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.inner
            .listeners
            .lock()
            .unwrap()
            .retain(|(known, _)| *known != id);
    }

    /// One feed per server, shared by every listener, read until the server goes.
    async fn listen(self) {
        let response = self
            .inner
            .http
            .get(format!("{}/api/event", self.inner.url))
            .header("authorization", &self.inner.authorization)
            .header("accept", "text/event-stream")
            .send()
            .await;
        if let Ok(response) = response {
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            while let Some(Ok(chunk)) = stream.next().await {
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = buffer.find("\n\n") {
                    let frame = buffer[..end].to_string();
                    buffer.drain(..end + 2);
                    self.dispatch(&frame);
                }
            }
        }
        self.inner.feed.lock().unwrap().take();
        if !self.inner.closing.load(Ordering::SeqCst) {
            for listener in self.listeners() {
                listener(OpenCodeEvent {
                    kind: FEED_ENDED.into(),
                    data: Value::Object(Default::default()),
                });
            }
        }
    }

    fn listeners(&self) -> Vec<Listener> {
        self.inner
            .listeners
            .lock()
            .unwrap()
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect()
    }

    fn dispatch(&self, frame: &str) {
        let data = frame
            .split('\n')
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(&data) else {
            return;
        };
        let Some(kind) = event["type"].as_str() else {
            return;
        };
        let parsed = OpenCodeEvent {
            kind: kind.to_string(),
            data: if event["data"].is_object() {
                event["data"].clone()
            } else {
                Value::Object(Default::default())
            },
        };
        for listener in self.listeners() {
            listener(parsed.clone());
        }
    }

    /// Stops listening and closes the server's stdin, at once, which its watcher answers by
    /// stopping it; `close` then waits it out.
    pub fn begin_close(&self) -> bool {
        if self.inner.closing.swap(true, Ordering::SeqCst) {
            return false;
        }
        if let Some(feed) = self.inner.feed.lock().unwrap().take() {
            feed.abort();
        }
        self.inner.listeners.lock().unwrap().clear();
        self.inner.stdin.lock().unwrap().take();
        true
    }

    pub async fn close(&self) {
        self.begin_close();
        if tokio::time::timeout(STOP_GRACE, self.inner.exited.clone())
            .await
            .is_err()
        {
            if let Some(pid) = self.inner.pid {
                // SAFETY: kills the watching shell that would not stop.
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }
            self.inner.exited.clone().await;
        }
    }

    /// A list read in a folder the server has not loaded yet comes back empty, then grows while it
    /// loads, so it is read again until two answers agree, for a few seconds at most.
    pub async fn read_loaded(&self, path: &str) -> anyhow::Result<Vec<Value>> {
        let mut previous: Option<usize> = None;
        let mut attempt = 1;
        loop {
            let list = self
                .request("GET", path, None, None)
                .await?
                .as_array()
                .cloned()
                .unwrap_or_default();
            if self.inner.loaded.lock().unwrap().contains(path) {
                return Ok(list);
            }
            if !list.is_empty() && Some(list.len()) == previous {
                self.inner.loaded.lock().unwrap().insert(path.to_string());
                return Ok(list);
            }
            if attempt >= LOADING_ATTEMPTS {
                return Ok(list);
            }
            previous = Some(list.len());
            attempt += 1;
            tokio::time::sleep(LOADING_RETRY).await;
        }
    }
}

/// `location[directory]=<folder>`, as the server's list endpoints take it.
pub fn location_query(directory: &str) -> String {
    let encoded: String = directory
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    format!("location[directory]={encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_where_the_server_listens() {
        let output = "Server listening on http://127.0.0.1:5000/\nserver password abc123\n";
        assert_eq!(listening(output).as_deref(), Some("http://127.0.0.1:5000/"));
        assert_eq!(password(output).as_deref(), Some("abc123"));
        assert_eq!(
            unread_line("listening at port 5\n"),
            Some("listening at port 5".into())
        );
        assert_eq!(unread_line("loading\n"), None);
    }
}
