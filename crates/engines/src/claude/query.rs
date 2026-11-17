//! A Claude Code process driven over its stream-json protocol, as the Agent SDK drives it: the
//! command line it is started with, the `initialize` control request, the control requests it
//! sends back (tool permission, hooks, in-process MCP servers), and the control requests the app
//! sends it (interrupt, model, permission mode, flag settings, task stops, usage).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::tools::{BoundTool, ToolResult};

/// The Agent SDK version whose wire behaviour this client reproduces.
pub const SDK_VERSION: &str = "0.3.326";
/// How long a closed process has to leave on its own before it is signalled.
const CLOSE_GRACE: Duration = Duration::from_millis(2_000);
const KILL_GRACE: Duration = Duration::from_millis(5_000);
const STDERR_KEEP: usize = 2_048;

/// Decides one tool call: `(tool_name, input, tool_use_id, cancel)` to the control response body,
/// `{ behavior: "allow", updatedInput }` or `{ behavior: "deny", message }`.
pub type CanUseTool = Arc<
    dyn Fn(String, Value, String, CancellationToken) -> BoxFuture<'static, Value> + Send + Sync,
>;
/// Answers one hook callback with its output object.
pub type HookCallback =
    Arc<dyn Fn(Value, Option<String>) -> BoxFuture<'static, Value> + Send + Sync>;

/// How the session's system prompt is set.
#[derive(Debug, Clone, Default)]
pub enum SystemPrompt {
    /// The SDK's default when none is given: an empty prompt.
    #[default]
    Empty,
    Custom(String),
    /// Claude Code's own prompt with text appended.
    Preset {
        append: Option<String>,
    },
}

/// An MCP server the app serves to Claude in-process, over control messages.
#[derive(Clone)]
pub struct SdkMcpServer {
    pub name: String,
    pub version: String,
    pub always_load: bool,
    pub tools: Vec<BoundTool>,
}

/// The options of one Claude Code process, named as the Agent SDK names them.
#[derive(Clone, Default)]
pub struct ClaudeOptions {
    pub executable: String,
    pub cwd: Option<String>,
    /// The process environment, whole. The SDK's own variables are added to it.
    pub env: HashMap<String, String>,
    /// Restricts the built-in tools; `Some(empty)` offers none.
    pub tools: Option<Vec<String>>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub resume: Option<String>,
    pub fork_session: bool,
    pub permission_mode: Option<String>,
    pub allow_dangerously_skip_permissions: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub betas: Vec<String>,
    /// Extra `--flag[=value]` arguments; a `None` value is a bare flag.
    pub extra_args: Vec<(String, Option<String>)>,
    /// Servers started by Claude itself, as `--mcp-config` entries.
    pub mcp_servers: Map<String, Value>,
    pub sdk_mcp_servers: Vec<SdkMcpServer>,
    pub system_prompt: SystemPrompt,
    pub setting_sources: Option<Vec<String>>,
    /// `skills: "all"`: lets the Skill tool load every skill.
    pub all_skills: bool,
    pub plugin_dirs: Vec<String>,
    pub forward_subagent_text: bool,
    pub include_partial_messages: bool,
    pub json_schema: Option<Value>,
    pub max_turns: Option<u32>,
    pub can_use_tool: Option<CanUseTool>,
    /// Hook event name to callbacks, each registered as one matcher.
    pub hooks: Vec<(String, Vec<HookCallback>)>,
}

impl ClaudeOptions {
    /// The command line the SDK builds for these options, in its order.
    pub fn args(&self) -> Vec<String> {
        let flag = |name: &str, value: String| format!("--{name}={value}");
        let mut args: Vec<String> = [
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
        ]
        .map(String::from)
        .to_vec();
        if let Some(effort) = &self.effort {
            args.push(flag("effort", effort.clone()));
        }
        if let Some(turns) = self.max_turns {
            args.push(flag("max-turns", turns.to_string()));
        }
        if let Some(model) = &self.model {
            args.push(flag("model", model.clone()));
        }
        if !self.betas.is_empty() {
            args.push(flag("betas", self.betas.join(",")));
        }
        if let Some(schema) = &self.json_schema {
            args.push(flag("json-schema", schema.to_string()));
        }
        if self.can_use_tool.is_some() {
            args.push("--permission-prompt-tool=stdio".into());
        }
        if let Some(resume) = &self.resume {
            args.push(flag("resume", resume.clone()));
        }
        let mut allowed = self.allowed_tools.clone();
        if self.all_skills && !allowed.iter().any(|tool| tool == "Skill") {
            allowed.push("Skill".into());
        }
        if !allowed.is_empty() {
            args.push(flag("allowedTools", allowed.join(",")));
        }
        if !self.disallowed_tools.is_empty() {
            args.push(flag("disallowedTools", self.disallowed_tools.join(",")));
        }
        if let Some(tools) = &self.tools {
            args.push(flag("tools", tools.join(",")));
        }
        if !self.mcp_servers.is_empty() {
            args.push("--mcp-config".into());
            args.push(json!({ "mcpServers": self.mcp_servers }).to_string());
        }
        if let Some(sources) = &self.setting_sources {
            args.push(flag("setting-sources", sources.join(",")));
        }
        if let Some(mode) = &self.permission_mode {
            args.push(flag("permission-mode", mode.clone()));
        }
        if self.allow_dangerously_skip_permissions {
            args.push("--allow-dangerously-skip-permissions".into());
        }
        if self.include_partial_messages {
            args.push("--include-partial-messages".into());
        }
        for dir in &self.plugin_dirs {
            args.push(flag("plugin-dir", dir.clone()));
        }
        if self.fork_session {
            args.push("--fork-session".into());
        }
        for (name, value) in &self.extra_args {
            match value {
                None => args.push(format!("--{name}")),
                Some(value) if value.len() > 1 && value.starts_with('-') => {
                    args.push(flag(name, value.clone()))
                }
                Some(value) => {
                    args.push(format!("--{name}"));
                    args.push(value.clone());
                }
            }
        }
        args
    }

    /// The environment the SDK starts Claude Code with.
    pub fn environment(&self) -> HashMap<String, String> {
        let mut env = self.env.clone();
        env.entry("CLAUDE_CODE_ENTRYPOINT".into())
            .or_insert_with(|| "sdk-ts".into());
        env.entry("CLAUDE_AGENT_SDK_VERSION".into())
            .or_insert_with(|| SDK_VERSION.into());
        if !env
            .keys()
            .any(|key| key.eq_ignore_ascii_case("CLAUDE_CODE_SDK_READS_SESSION_STATE"))
        {
            env.insert("CLAUDE_CODE_SDK_READS_SESSION_STATE".into(), "1".into());
        }
        env.remove("NODE_OPTIONS");
        env.remove("DEBUG");
        env
    }

    fn initialize_request(&self) -> Value {
        let mut request = Map::new();
        request.insert("subtype".into(), json!("initialize"));
        if !self.hooks.is_empty() {
            let mut hooks = Map::new();
            let mut next = 0;
            for (event, callbacks) in &self.hooks {
                if callbacks.is_empty() {
                    continue;
                }
                let ids: Vec<String> = callbacks
                    .iter()
                    .map(|_| {
                        let id = format!("hook_{next}");
                        next += 1;
                        id
                    })
                    .collect();
                hooks.insert(event.clone(), json!([{ "hookCallbackIds": ids }]));
            }
            request.insert("hooks".into(), Value::Object(hooks));
        }
        if !self.sdk_mcp_servers.is_empty() {
            request.insert(
                "sdkMcpServers".into(),
                json!(self
                    .sdk_mcp_servers
                    .iter()
                    .map(|server| server.name.clone())
                    .collect::<Vec<_>>()),
            );
            if !env_truthy("CLAUDE_AGENT_SDK_DISABLE_MCP_MANIFESTS") {
                let manifests: Map<String, Value> = self
                    .sdk_mcp_servers
                    .iter()
                    .map(|server| (server.name.clone(), mcp_manifest(server)))
                    .collect();
                request.insert("sdkMcpServerManifests".into(), Value::Object(manifests));
            }
        }
        if let Some(schema) = &self.json_schema {
            request.insert("jsonSchema".into(), schema.clone());
        }
        match &self.system_prompt {
            SystemPrompt::Empty => {
                request.insert("systemPrompt".into(), json!([""]));
            }
            SystemPrompt::Custom(prompt) => {
                request.insert("systemPrompt".into(), json!([prompt]));
            }
            SystemPrompt::Preset { append } => {
                if let Some(append) = append {
                    request.insert("appendSystemPrompt".into(), json!(append));
                }
            }
        }
        if self.forward_subagent_text {
            request.insert("forwardSubagentText".into(), json!(true));
        }
        Value::Object(request)
    }

    fn hook_table(&self) -> HashMap<String, HookCallback> {
        let mut table = HashMap::new();
        let mut next = 0;
        for (_, callbacks) in &self.hooks {
            for callback in callbacks {
                table.insert(format!("hook_{next}"), callback.clone());
                next += 1;
            }
        }
        table
    }
}

/// The error a failed process is reported by: the last error result, or the one before a run of
/// turns that never reached the model, as the SDK reads them.
#[derive(Default)]
struct ErrorResults {
    last: Option<String>,
    before_model_less: Option<String>,
    model_less_ended: bool,
}

impl ErrorResults {
    fn result(&mut self, text: Option<String>, message: &Value) {
        self.last = text;
        self.model_less_ended = message["subtype"] == "success"
            && message["is_error"] != true
            && message["num_turns"] == 0
            && message["result"] == "";
        if !self.model_less_ended {
            self.before_model_less = self.last.clone();
        }
    }

    fn message(&mut self, message: &Value) {
        self.last = None;
        if message["type"] == "system" && message["subtype"] == "init" {
            self.model_less_ended = false;
        } else if message["type"] != "system" || message["subtype"] == "compact_boundary" {
            self.before_model_less = None;
            self.model_less_ended = false;
        }
    }

    fn text(&self) -> Option<String> {
        self.last.clone().or_else(|| {
            self.model_less_ended
                .then(|| self.before_model_less.clone())
                .flatten()
        })
    }
}

fn signal_name(signal: i32) -> String {
    let names = [
        (libc::SIGHUP, "SIGHUP"),
        (libc::SIGINT, "SIGINT"),
        (libc::SIGQUIT, "SIGQUIT"),
        (libc::SIGILL, "SIGILL"),
        (libc::SIGTRAP, "SIGTRAP"),
        (libc::SIGABRT, "SIGABRT"),
        (libc::SIGBUS, "SIGBUS"),
        (libc::SIGFPE, "SIGFPE"),
        (libc::SIGKILL, "SIGKILL"),
        (libc::SIGUSR1, "SIGUSR1"),
        (libc::SIGSEGV, "SIGSEGV"),
        (libc::SIGUSR2, "SIGUSR2"),
        (libc::SIGPIPE, "SIGPIPE"),
        (libc::SIGALRM, "SIGALRM"),
        (libc::SIGTERM, "SIGTERM"),
        (libc::SIGXCPU, "SIGXCPU"),
        (libc::SIGXFSZ, "SIGXFSZ"),
    ];
    names
        .iter()
        .find(|(number, _)| *number == signal)
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| signal.to_string())
}

/// Credentials a stderr tail must not carry into an error message.
fn secret_patterns() -> &'static [(regex::Regex, &'static str)] {
    static PATTERNS: std::sync::OnceLock<Vec<(regex::Regex, &'static str)>> =
        std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (r"(?i)\bBearer\s+[A-Za-z0-9._~+/=%-]{8,}", "Bearer <token>"),
            (r"(?i)(:\s*)Basic\s+[A-Za-z0-9+/=]{8,}", "${1}Basic <token>"),
            (r"\bsk-ant-[A-Za-z0-9_-]{8,}", "<token>"),
            (r"\bsk-(?:proj|svcacct|admin)-[A-Za-z0-9_-]{20,}", "<token>"),
            (r"\bsk-[A-Za-z0-9_-]{20,}", "<token>"),
            (r"(^|[^A-Za-z0-9+])(?:AKIA|ASIA|ABIA|ACCA|A3T[A-Z0-9])[A-Z0-9]{16}\b", "${1}<token>"),
            (r"\bgh[opusr]_[A-Za-z0-9]{20,}", "<token>"),
            (r"github_pat_[A-Za-z0-9_]{82,}", "<token>"),
            (r"sq0(?:atp|csp)-[A-Za-z0-9_-]{22,}", "<token>"),
            (r"(^|[^A-Za-z0-9+/])EAAA[A-Za-z0-9+/=%_-]{56,}", "${1}<token>"),
            (r"\bgl(?:pat|dt|rt|ft|soat|oas|agent|ptt|cbt|imt|ffct)-[A-Za-z0-9_=-]{20,}(?:\.[0-9a-z]{9})?", "<token>"),
            (r"(?i)\b(?:xox[a-z](?:\.xox[a-z])?|xapp|xwfp)-[A-Za-z0-9+/=%_-]{10,}", "<token>"),
            (r"hooks\.slack\.com/[A-Za-z0-9/_-]+", "hooks.slack.com/<redacted>"),
            (r"((?:[A-Za-z0-9_-]*-)?)ey[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}", "${1}<jwt>"),
            (r"(^|[^A-Za-z0-9+_-])AIza[A-Za-z0-9_-]{35}\b", "${1}<token>"),
            (r"(^|[^A-Za-z0-9+_-])GOCSPX-[A-Za-z0-9_-]{28}\b", "${1}<token>"),
            (r"(^|[^A-Za-z0-9+_-])[sr]k_(?:live|test|prod)_[A-Za-z0-9]{24,}", "${1}<token>"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| (regex::Regex::new(pattern).unwrap(), replacement))
        .collect()
    })
}

/// The stderr an exit error carries: credentials replaced, its last 2048 characters, trimmed.
fn stderr_tail_text(tail: &str) -> String {
    let mut text = tail.to_string();
    for (pattern, replacement) in secret_patterns() {
        text = pattern.replace_all(&text, *replacement).into_owned();
    }
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(STDERR_KEEP))
        .collect::<String>()
        .trim()
        .to_string()
}

/// What the process says, in order: every message that is not part of the control protocol, then
/// how the stream ended.
#[derive(Debug, Clone)]
pub enum QueryOutput {
    Message(Value),
    /// The stream ended; an error says why the process failed.
    End(Result<(), String>),
}

type Pending = oneshot::Sender<Result<Value, String>>;

struct Shared_ {
    writer: Mutex<Option<mpsc::UnboundedSender<String>>>,
    pending: Mutex<HashMap<String, Pending>>,
    cancels: Mutex<HashMap<String, CancellationToken>>,
    closed: AtomicBool,
    pid: Option<u32>,
    exited: watch::Receiver<bool>,
    latest_commands: Mutex<Option<Value>>,
    can_use_tool: Option<CanUseTool>,
    hooks: HashMap<String, HookCallback>,
    mcp: HashMap<String, SdkMcpServer>,
}

/// One live Claude Code process.
#[derive(Clone)]
pub struct ClaudeQuery {
    shared: Arc<Shared_>,
    initialization: Shared<BoxFuture<'static, Result<Value, String>>>,
}

fn request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..13].to_string()
}

impl ClaudeQuery {
    /// Starts the process and sends `initialize`. Messages arrive on the returned receiver.
    pub fn spawn(
        options: ClaudeOptions,
    ) -> anyhow::Result<(ClaudeQuery, mpsc::UnboundedReceiver<QueryOutput>)> {
        let mut command = Command::new(&options.executable);
        command.args(options.args());
        command.env_clear();
        command.envs(options.environment());
        if let Some(cwd) = &options.cwd {
            command.current_dir(cwd);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.kill_on_drop(false);
        let mut child = command
            .spawn()
            .map_err(|error| anyhow::anyhow!("Failed to spawn Claude Code process: {error}"))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let pid = child.id();
        crate::session_resources::register_session_process(pid);

        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();
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

        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let tail = stderr_tail.clone();
        let stderr_done = tokio::spawn(async move {
            let mut buffer = [0u8; 4096];
            while let Ok(read) = stderr.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                let mut tail = tail.lock().unwrap();
                tail.push_str(&String::from_utf8_lossy(&buffer[..read]));
                if tail.len() > 2 * STDERR_KEEP {
                    let cut = tail.len() - STDERR_KEEP;
                    let cut = (cut..tail.len())
                        .find(|index| tail.is_char_boundary(*index))
                        .unwrap_or(tail.len());
                    tail.drain(..cut);
                }
            }
        });

        let (exited_tx, exited_rx) = watch::channel(false);
        let (status_tx, status_rx) = oneshot::channel();
        tokio::spawn(async move {
            let status = child.wait().await;
            exited_tx.send_replace(true);
            let _ = status_tx.send(status);
        });

        let mcp = options
            .sdk_mcp_servers
            .iter()
            .map(|server| (server.name.clone(), server.clone()))
            .collect();
        let shared = Arc::new(Shared_ {
            writer: Mutex::new(Some(write_tx)),
            pending: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            pid,
            exited: exited_rx,
            latest_commands: Mutex::new(None),
            can_use_tool: options.can_use_tool.clone(),
            hooks: options.hook_table(),
            mcp,
        });

        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let reader_shared = shared.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut errors = ErrorResults::default();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                match message.get("type").and_then(Value::as_str) {
                    Some("control_response") => {
                        let response = &message["response"];
                        let id = response["request_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        let pending = reader_shared.pending.lock().unwrap().remove(&id);
                        if let Some(pending) = pending {
                            let result = if response["subtype"] == "success" {
                                Ok(response.get("response").cloned().unwrap_or(Value::Null))
                            } else {
                                Err(response["error"]
                                    .as_str()
                                    .unwrap_or("Claude Code control request failed")
                                    .to_string())
                            };
                            let _ = pending.send(result);
                        }
                        continue;
                    }
                    Some("control_request") => {
                        Self::answer_control(reader_shared.clone(), message);
                        continue;
                    }
                    Some("control_cancel_request") => {
                        let id = message["request_id"].as_str().unwrap_or_default();
                        if let Some(cancel) = reader_shared.cancels.lock().unwrap().remove(id) {
                            cancel.cancel();
                        }
                        continue;
                    }
                    Some("keep_alive") | Some("transcript_mirror") => continue,
                    Some("result") => {
                        let last = if message["is_error"] == true {
                            Some(if message["subtype"] == "success" {
                                message["result"].as_str().unwrap_or_default().to_string()
                            } else {
                                message["errors"]
                                    .as_array()
                                    .map(|errors| {
                                        errors
                                            .iter()
                                            .filter_map(Value::as_str)
                                            .map(str::trim)
                                            .filter(|e| !e.is_empty())
                                            .collect::<Vec<_>>()
                                            .join("; ")
                                    })
                                    .unwrap_or_default()
                            })
                            .filter(|text| !text.is_empty())
                        } else {
                            None
                        };
                        errors.result(last, &message);
                    }
                    Some("system")
                        if matches!(
                            message["subtype"].as_str(),
                            Some("post_turn_summary" | "task_summary" | "session_metadata")
                        ) => {}
                    Some("active_goal" | "autocompact_state") => {}
                    Some("system") if message["subtype"] == "session_state_changed" => {
                        if message["sdk_host_only"] == true {
                            continue;
                        }
                    }
                    Some("system") if message["subtype"] == "session_writes_lost" => {}
                    _ => {
                        if message["type"] == "system"
                            && message["subtype"] == "commands_changed"
                            && message["commands"].is_array()
                        {
                            *reader_shared.latest_commands.lock().unwrap() =
                                Some(message["commands"].clone());
                        }
                        errors.message(&message);
                    }
                }
                if out_tx.send(QueryOutput::Message(message)).is_err() {
                    break;
                }
            }
            let _ = stderr_done.await;
            let status = status_rx.await.ok().and_then(Result::ok);
            let tail = stderr_tail_text(&stderr_tail.lock().unwrap());
            let closed = reader_shared.closed.load(Ordering::SeqCst);
            let failure = match status {
                _ if closed => None,
                Some(status) if status.success() => None,
                Some(status) => {
                    let stderr = if tail.is_empty() {
                        String::new()
                    } else {
                        format!(". stderr: {tail}")
                    };
                    Some(match status.code() {
                        Some(code) => {
                            format!("Claude Code process exited with code {code}{stderr}")
                        }
                        None => {
                            use std::os::unix::process::ExitStatusExt;
                            let signal = status.signal().map(signal_name).unwrap_or_default();
                            format!("Claude Code process terminated by signal {signal}{stderr}")
                        }
                    })
                }
                None => Some("Claude Code process ended unexpectedly".to_string()),
            };
            let failure = match (failure, errors.text()) {
                (Some(_), Some(text)) => {
                    Some(format!("Claude Code returned an error result: {text}"))
                }
                (failure, _) => failure,
            };
            for (_, pending) in reader_shared.pending.lock().unwrap().drain() {
                let _ = pending.send(Err("Query closed before response received".into()));
            }
            for (_, cancel) in reader_shared.cancels.lock().unwrap().drain() {
                cancel.cancel();
            }
            let _ = out_tx.send(QueryOutput::End(match failure {
                Some(message) => Err(message),
                None => Ok(()),
            }));
        });

        let initialize = options.initialize_request();
        let query = ClaudeQuery {
            shared: shared.clone(),
            initialization: futures::future::ready(Ok(Value::Null)).boxed().shared(),
        };
        let init = query.raw_request(initialize).boxed().shared();
        let query = ClaudeQuery {
            initialization: init,
            ..query
        };
        Ok((query, out_rx))
    }

    fn write(shared: &Shared_, message: &Value) -> bool {
        let line = format!("{message}\n");
        shared
            .writer
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|writer| writer.send(line).is_ok())
    }

    fn raw_request(
        &self,
        request: Value,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send + 'static {
        let shared = self.shared.clone();
        let id = request_id();
        let (sender, receiver) = oneshot::channel();
        let written = if shared.closed.load(Ordering::SeqCst) {
            false
        } else {
            shared.pending.lock().unwrap().insert(id.clone(), sender);
            Self::write(
                &shared,
                &json!({ "request_id": id, "type": "control_request", "request": request }),
            )
        };
        async move {
            if !written {
                shared.pending.lock().unwrap().remove(&id);
                return Err("Query closed before response received".to_string());
            }
            receiver
                .await
                .unwrap_or_else(|_| Err("Query closed before response received".to_string()))
        }
    }

    /// Sends a control request once `initialize` has been answered.
    pub fn request(
        &self,
        request: Value,
    ) -> impl std::future::Future<Output = anyhow::Result<Value>> + Send + 'static {
        let initialization = self.initialization.clone();
        let this = self.clone();
        async move {
            initialization.await.map_err(anyhow::Error::msg)?;
            this.raw_request(request).await.map_err(anyhow::Error::msg)
        }
    }

    /// What Claude answered `initialize` with: commands, models, account and more.
    pub async fn initialization(&self) -> anyhow::Result<Value> {
        self.initialization
            .clone()
            .await
            .map_err(anyhow::Error::msg)
    }

    /// Writes one user message to the session's input.
    pub fn send_user(&self, message: &Value) -> bool {
        !self.shared.closed.load(Ordering::SeqCst) && Self::write(&self.shared, message)
    }

    pub async fn interrupt(&self) -> anyhow::Result<()> {
        self.request(json!({ "subtype": "interrupt" }))
            .await
            .map(|_| ())
    }

    pub async fn set_permission_mode(&self, mode: &str) -> anyhow::Result<()> {
        self.request(json!({ "subtype": "set_permission_mode", "mode": mode }))
            .await
            .map(|_| ())
    }

    pub async fn set_model(&self, model: &str) -> anyhow::Result<()> {
        self.request(json!({ "subtype": "set_model", "model": model }))
            .await
            .map(|_| ())
    }

    pub async fn apply_flag_settings(&self, settings: Value) -> anyhow::Result<()> {
        self.request(json!({ "subtype": "apply_flag_settings", "settings": settings }))
            .await
            .map(|_| ())
    }

    pub async fn stop_task(&self, task_id: &str) -> anyhow::Result<()> {
        self.request(json!({ "subtype": "stop_task", "task_id": task_id }))
            .await
            .map(|_| ())
    }

    /// The plan's rate-limit report (`get_usage`).
    pub async fn usage(&self) -> anyhow::Result<Value> {
        self.request(json!({ "subtype": "get_usage" })).await
    }

    pub async fn supported_commands(&self) -> anyhow::Result<Value> {
        let init = self.initialization().await?;
        Ok(self
            .shared
            .latest_commands
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| init["commands"].clone()))
    }

    pub async fn supported_models(&self) -> anyhow::Result<Value> {
        Ok(self.initialization().await?["models"].clone())
    }

    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    /// Ends the input. A process that has not left two seconds later is terminated, then killed.
    pub fn close(&self) {
        if self.shared.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shared.writer.lock().unwrap().take();
        for (_, cancel) in self.shared.cancels.lock().unwrap().drain() {
            cancel.cancel();
        }
        let mut exited = self.shared.exited.clone();
        let pid = self.shared.pid;
        tokio::spawn(async move {
            if tokio::time::timeout(CLOSE_GRACE, exited.wait_for(|done| *done))
                .await
                .is_ok()
            {
                return;
            }
            signal(pid, libc::SIGTERM);
            if tokio::time::timeout(KILL_GRACE, exited.wait_for(|done| *done))
                .await
                .is_err()
            {
                signal(pid, libc::SIGKILL);
            }
        });
    }

    /// Settles once the process has exited.
    pub fn exited(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut exited = self.shared.exited.clone();
        async move {
            let _ = exited.wait_for(|done| *done).await;
        }
    }

    fn answer_control(shared: Arc<Shared_>, message: Value) {
        let id = message["request_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if shared.closed.load(Ordering::SeqCst) || shared.cancels.lock().unwrap().contains_key(&id)
        {
            return;
        }
        let cancel = CancellationToken::new();
        shared
            .cancels
            .lock()
            .unwrap()
            .insert(id.clone(), cancel.clone());
        tokio::spawn(async move {
            let request = &message["request"];
            let answer = Self::process_control(&shared, request, cancel.clone()).await;
            let still_asked = shared.cancels.lock().unwrap().remove(&id).is_some();
            if !still_asked || shared.closed.load(Ordering::SeqCst) {
                return;
            }
            let response = match answer {
                Ok(None) => return,
                Ok(Some(body)) => {
                    json!({ "type": "control_response", "response": { "subtype": "success", "request_id": id, "response": body } })
                }
                Err(error) => {
                    json!({ "type": "control_response", "response": { "subtype": "error", "request_id": id, "error": error } })
                }
            };
            Self::write(&shared, &response);
        });
    }

    /// The answer to one control request Claude sent; `None` leaves it unanswered, as the SDK does
    /// for requests meant for another host.
    async fn process_control(
        shared: &Shared_,
        request: &Value,
        cancel: CancellationToken,
    ) -> Result<Option<Value>, String> {
        let answered = match request["subtype"].as_str().unwrap_or_default() {
            "remote_tool_call"
            | "remote_plumbing_call"
            | "remote_tools_probe"
            | "remote_tools_reannounce"
            | "request_user_dialog" => return Ok(None),
            "can_use_tool" => {
                let Some(can_use_tool) = &shared.can_use_tool else {
                    return Err("canUseTool callback is not provided.".into());
                };
                let tool_use_id = request["tool_use_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let mut decision = can_use_tool(
                    request["tool_name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    request.get("input").cloned().unwrap_or(json!({})),
                    tool_use_id.clone(),
                    cancel,
                )
                .await;
                if let Some(object) = decision.as_object_mut() {
                    object.insert("toolUseID".into(), json!(tool_use_id));
                }
                Ok(decision)
            }
            "oauth_token_refresh" => Err("getOAuthToken callback is not provided.".into()),
            "host_auth_token_refresh" => Err("getHostAuthToken callback is not provided.".into()),
            "remote_control_work_secret" => {
                Err("refreshWorkSecret callback is not provided.".into())
            }
            "hook_callback" => {
                let id = request["callback_id"].as_str().unwrap_or_default();
                let Some(hook) = shared.hooks.get(id) else {
                    return Err(format!("No hook callback found for ID: {id}"));
                };
                Ok(hook(
                    request.get("input").cloned().unwrap_or(Value::Null),
                    request["tool_use_id"].as_str().map(String::from),
                )
                .await)
            }
            "mcp_message" => {
                let name = request["server_name"].as_str().unwrap_or_default();
                let Some(server) = shared.mcp.get(name) else {
                    return Err(format!("SDK MCP server not found: {name}"));
                };
                Ok(json!({ "mcp_response": mcp_respond(server, &request["message"]).await }))
            }
            "elicitation" => Ok(json!({ "action": "decline" })),
            other => Err(format!("Unsupported control request subtype: {other}")),
        };
        answered.map(Some)
    }
}

fn signal(pid: Option<u32>, signal: i32) {
    if let Some(pid) = pid {
        // SAFETY: plain signal delivery to a child process id.
        unsafe {
            libc::kill(pid as i32, signal);
        }
    }
}

/// The MCP protocol version an in-process server answers with when the client asks for none it knows.
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_PROTOCOL_VERSIONS: [&str; 5] = [
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

fn env_truthy(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn mcp_initialize_result(server: &SdkMcpServer, requested: Option<&str>) -> Value {
    let version = requested
        .filter(|version| MCP_PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(MCP_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": true } },
        "serverInfo": { "name": server.name, "version": server.version },
    })
}

fn mcp_tools_list(server: &SdkMcpServer) -> Value {
    json!({ "tools": server.tools.iter().map(|tool| {
        let mut listed = json!({
            "name": tool.name,
            "description": tool.description,
            "inputSchema": tool.input_schema,
            "execution": { "taskSupport": "forbidden" },
        });
        if server.always_load {
            listed["_meta"] = json!({ "anthropic/alwaysLoad": true });
        }
        listed
    }).collect::<Vec<_>>() })
}

/// What `initialize` tells Claude of an in-process server up front, so it skips that handshake.
fn mcp_manifest(server: &SdkMcpServer) -> Value {
    json!({
        "initializeResult": mcp_initialize_result(server, Some(MCP_PROTOCOL_VERSION)),
        "toolsListResult": mcp_tools_list(server),
    })
}

/// Answers one JSON-RPC message for an in-process MCP server. Notifications get an empty result.
pub async fn mcp_respond(server: &SdkMcpServer, message: &Value) -> Value {
    let id = message.get("id").cloned().filter(|id| !id.is_null());
    let Some(id) = id.filter(|_| message.get("method").is_some()) else {
        return json!({ "jsonrpc": "2.0", "result": {}, "id": 0 });
    };
    let params = message.get("params").cloned().unwrap_or(json!({}));
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => Ok(mcp_initialize_result(
            server,
            params["protocolVersion"].as_str(),
        )),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(mcp_tools_list(server)),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            match server.tools.iter().find(|tool| tool.name == name) {
                Some(tool) => {
                    let result: ToolResult =
                        (tool.handler)(params.get("arguments").cloned().unwrap_or(json!({}))).await;
                    Ok(serde_json::to_value(result).unwrap_or(json!({})))
                }
                None => Ok(serde_json::to_value(ToolResult::error(format!(
                    "Tool {name} not found"
                )))
                .unwrap_or(json!({}))),
            }
        }
        method => Err(json!({ "code": -32601, "message": format!("Method not found: {method}") })),
    };
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_carries_the_manifest_of_each_in_process_server() {
        let options = ClaudeOptions {
            sdk_mcp_servers: vec![SdkMcpServer {
                name: "aicodingtool-threads".into(),
                version: "1.0.0".into(),
                always_load: true,
                tools: vec![BoundTool {
                    name: "list_threads".into(),
                    description: "List threads.".into(),
                    input_schema: json!({ "type": "object", "properties": {} }),
                    read_only: true,
                    handler: Arc::new(|_| Box::pin(async { ToolResult::text("none") })),
                }],
            }],
            ..Default::default()
        };
        let request = options.initialize_request();
        assert_eq!(request["sdkMcpServers"], json!(["aicodingtool-threads"]));
        let manifest = &request["sdkMcpServerManifests"]["aicodingtool-threads"];
        assert_eq!(
            manifest["initializeResult"]["protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
        let tool = &manifest["toolsListResult"]["tools"][0];
        assert_eq!(tool["name"], "list_threads");
        assert_eq!(tool["execution"]["taskSupport"], "forbidden");
        assert_eq!(tool["_meta"]["anthropic/alwaysLoad"], true);
    }

    #[test]
    fn a_failed_exit_names_the_error_before_turns_that_never_reached_the_model() {
        let mut errors = ErrorResults::default();
        errors.result(
            Some("Prompt is too long".into()),
            &json!({ "subtype": "success", "is_error": true, "num_turns": 1 }),
        );
        errors.message(&json!({ "type": "system", "subtype": "status" }));
        errors.result(
            None,
            &json!({ "subtype": "success", "is_error": false, "num_turns": 0, "result": "" }),
        );
        assert_eq!(errors.text().as_deref(), Some("Prompt is too long"));
        errors.message(&json!({ "type": "assistant" }));
        assert_eq!(errors.text(), None);
    }

    #[test]
    fn a_stderr_tail_carries_no_credentials() {
        let tail = stderr_tail_text(
            "auth failed: Authorization: Bearer abcdefghijklmnop key=sk-ant-api03-secretsecret\n",
        );
        assert!(
            !tail.contains("abcdefghijklmnop") && !tail.contains("secretsecret"),
            "{tail}"
        );
        assert!(tail.ends_with("<token>"));
    }

    #[test]
    fn builds_the_sdk_command_line() {
        let options = ClaudeOptions {
            executable: "claude".into(),
            model: Some("opus".into()),
            effort: Some("high".into()),
            betas: vec!["context-1m-2025-08-07".into()],
            resume: Some("abc".into()),
            disallowed_tools: vec!["AskUserQuestion".into()],
            all_skills: true,
            setting_sources: Some(vec!["user".into(), "project".into()]),
            permission_mode: Some("default".into()),
            include_partial_messages: true,
            extra_args: vec![("chrome".into(), None)],
            can_use_tool: Some(Arc::new(|_, _, _, _| Box::pin(async { json!({}) }))),
            ..Default::default()
        };
        assert_eq!(
            options.args(),
            [
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
                "--effort=high",
                "--model=opus",
                "--betas=context-1m-2025-08-07",
                "--permission-prompt-tool=stdio",
                "--resume=abc",
                "--allowedTools=Skill",
                "--disallowedTools=AskUserQuestion",
                "--setting-sources=user,project",
                "--permission-mode=default",
                "--include-partial-messages",
                "--chrome",
            ]
        );
    }
}
