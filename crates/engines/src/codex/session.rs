//! One live Codex thread, kept across turns. The app-server process belongs to the session, so a
//! second turn neither pays for a new process nor resumes the thread from disk. Each turn ends on
//! the server's own `turn/completed`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use futures::future::{join_all, BoxFuture, FutureExt, Shared};
use serde_json::{json, Value};

use crate::background::BackgroundWork;
use crate::brief::{
    needs_side_chat_boundary, run_brief, BriefDialect, RunBrief, SIDE_CHAT_BOUNDARY,
};
use crate::catalog::context_window_limit;
use crate::markdown::{end_markdown, stream_markdown, MarkdownBuffer};
use crate::provider::{ProviderEvent, ProviderResult, RunInput, ToolDecision};
use crate::session_pool::SessionHooks;
use crate::session_turn::{
    drain_steering, first_line, opened, Compacting, OpenFailure, SessionCore, TurnSession, TurnSlot,
};
use crate::tools::{grants_tool, ServedTools, ToolHost, ToolReach};
use crate::types::{
    ActiveGoal, AgentEngine, BackgroundProcess, BackgroundProcessKind, CompactionTrigger,
    Continuation, CoordinationRole, GoalReport, GoalStatus, RetryNotice, RunOperation, ToolIntent,
};

use super::app_server::{
    client_info, signal_name, AppServerClient, AppServerError, CodexConnect, ExitStatus,
    IncomingRequest,
};
use super::config::{codex_config, TOOL_TOKEN_ENV};
use super::plan_usage::read_codex_usage_limit;
use super::policy::{codex_policy, sandbox_policy};
use super::questions::CodexQuestions;
use super::server::{codex_app_server, ServerOptions};
use super::skills::{adopt_app_skills, CodexSkills};
use super::spend::CodexSpend;
use super::subagents::{CodexSubagents, StopTarget};
use super::thread_record::{resume_thread, CodexThreadRecord, ReadOrigin};

const SIGN_IN: &str = "Sign in to Codex to run this thread.";
const ENDED: &str = "The Codex session ended before the run could start.";

pub const CODEX_DIALECT: BriefDialect = BriefDialect {
    native_work: "Codex sessions or subagents",
    native_worktree: None,
    coordinator_reads: "Your shell is read-only: use it to read and search, and never ask to run a command outside its sandbox.",
    own_checkout_commits: Some("If your sandbox keeps a git command such as git add or git commit from writing the git data your checkout keeps in the main checkout's .git folder, ask to run it outside the sandbox, and report blocked only if that is refused. That is your checkout's own history, not the user's files."),
};

/// What a Codex thread is told beyond its prompt, and the app tools it is served.
pub fn codex_brief(input: &RunInput) -> RunBrief {
    run_brief(input, &CODEX_DIALECT)
}

/// Saves a generated image and answers with what the thread shows for it.
pub type ImageOutput =
    Arc<dyn Fn(Value, String) -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Goal {
    Clear,
    Get,
    Set(String),
}

fn goal_command(prompt: &str) -> Option<Goal> {
    let rest = prompt.trim().strip_prefix("/goal")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let argument = rest.trim();
    Some(if argument.eq_ignore_ascii_case("clear") {
        Goal::Clear
    } else if argument.is_empty() {
        Goal::Get
    } else {
        Goal::Set(argument.to_string())
    })
}

/// Why the server is retrying a request, in the user's terms.
fn retry_reason(error: &Value) -> String {
    let info = &error["codexErrorInfo"];
    match info.as_str() {
        Some("serverOverloaded") => return "Codex is overloaded.".into(),
        Some("rateLimitExceeded") => return "Codex is rate limited.".into(),
        Some("internalServerError") => return "Codex API error.".into(),
        _ => {}
    }
    if [
        "httpConnectionFailed",
        "responseStreamConnectionFailed",
        "responseStreamDisconnected",
    ]
    .iter()
    .any(|key| info.get(key).is_some())
    {
        return "Cannot reach Codex.".into();
    }
    first_line(error["message"].as_str().unwrap_or_default()).to_string()
}

fn describe_exit(exit: &ExitStatus) -> String {
    let line = first_line(&exit.stderr);
    if !line.is_empty() {
        return line.to_string();
    }
    match (exit.signal, exit.code) {
        (Some(signal), _) => format!("signal {}", signal_name(signal)),
        (None, Some(code)) => format!("exit code {code}"),
        _ => "exit code null".to_string(),
    }
}

fn start_failure(exit: &ExitStatus) -> String {
    format!("Codex could not start: {}", describe_exit(exit))
}

/// What an item the agent started is about, in the shape the thread records tools in.
pub fn intent_of(item: &Value) -> Option<ToolIntent> {
    let id = item["id"].as_str().unwrap_or_default().to_string();
    match item["type"].as_str()? {
        "commandExecution" => Some(ToolIntent {
            tool_id: id,
            name: "command_execution".into(),
            input: json!({ "command": item["command"], "cwd": item["cwd"] }),
            write_path: None,
        }),
        "fileChange" => {
            let changes = item["changes"].as_array().cloned().unwrap_or_default();
            let path = changes
                .first()
                .and_then(|change| change["path"].as_str())
                .map(String::from);
            let listed: Vec<Value> = changes.iter().map(|change| json!({ "path": change["path"], "kind": change["kind"]["type"], "diff": change["diff"] })).collect();
            Some(ToolIntent {
                tool_id: id,
                name: "file_change".into(),
                input: json!({ "path": path.clone().unwrap_or_default(), "changes": listed }),
                write_path: path,
            })
        }
        "mcpToolCall" => Some(ToolIntent {
            tool_id: id,
            name: item["tool"].as_str().unwrap_or_default().to_string(),
            input: if item["arguments"].is_object() {
                item["arguments"].clone()
            } else {
                json!({ "arguments": item["arguments"] })
            },
            write_path: None,
        }),
        "webSearch" => Some(ToolIntent {
            tool_id: id,
            name: "web_search".into(),
            input: json!({ "query": item["query"] }),
            write_path: None,
        }),
        "imageGeneration" => Some(ToolIntent {
            tool_id: id,
            name: "image_generation".into(),
            input: json!({ "description": "Generating image" }),
            write_path: None,
        }),
        _ => None,
    }
}

/// Grants what was asked for, whole: the question is yes or no, not which part.
fn granted(requested: &Value) -> Value {
    let mut grant = json!({});
    for key in ["network", "fileSystem"] {
        if !requested[key].is_null() && requested.get(key).is_some() {
            grant[key] = requested[key].clone();
        }
    }
    grant
}

/// The tool an MCP approval prompt names, which the prompt carries only in its wording.
fn tool_named(message: &str) -> Option<String> {
    let start = message.find("run tool \"")? + "run tool \"".len();
    let end = message[start..].find('"')?;
    (end > 0).then(|| message[start..start + end].to_string())
}

#[derive(Default)]
pub(crate) struct TurnData {
    compacting: Compacting,
    /// Named by the server once turn/start returns; an interrupt asked for before then waits on it.
    turn_id: Option<String>,
    interrupt_wanted: bool,
    streamed: HashMap<String, MarkdownBuffer>,
    /// Tool items started and not yet completed: what an approval for one of them is about.
    items: HashMap<String, Value>,
    images: Vec<Shared<BoxFuture<'static, ()>>>,
    image_ids: HashSet<String>,
    failure: Option<Value>,
    review_output: Option<String>,
}

type Opening = Shared<BoxFuture<'static, Result<(), OpenFailure>>>;

pub(crate) struct State {
    client: Option<AppServerClient>,
    served: Option<ServedTools>,
    opening: Option<Opening>,
    core: SessionCore<TurnData>,
    subagents: Option<CodexSubagents>,
    closed: Option<Shared<BoxFuture<'static, ()>>>,
    last_tokens: u64,
    goal_active: bool,
    report_goal: Arc<dyn Fn(GoalReport) + Send + Sync>,
    background: BackgroundWork,
    background_read: u64,
    skills: Option<CodexSkills>,
    thread_id: Option<String>,
    /// Server turns whose run already settled before they completed, as a manual compaction does.
    retired_turns: VecDeque<String>,
    /// Retired turns the server still runs; it takes no other input until they complete.
    unfinished_retired: Vec<String>,
}

pub struct CodexSession {
    key: String,
    hooks: SessionHooks,
    connect: CodexConnect,
    host: Option<Arc<dyn ToolHost>>,
    image_output: ImageOutput,
    questions: CodexQuestions,
    record: CodexThreadRecord,
    spend: CodexSpend,
    state: Mutex<State>,
    retired_finished: tokio::sync::Notify,
    me: Weak<CodexSession>,
}

impl CodexSession {
    pub fn new(
        key: String,
        connect: CodexConnect,
        host: Option<Arc<dyn ToolHost>>,
        hooks: SessionHooks,
        read_origin: ReadOrigin,
        image_output: ImageOutput,
    ) -> Arc<Self> {
        let questions = CodexQuestions::default();
        let released = questions.clone();
        Arc::new_cyclic(|me: &Weak<CodexSession>| {
            let spend_target = me.clone();
            CodexSession {
                key,
                hooks,
                connect,
                host,
                image_output,
                questions,
                record: CodexThreadRecord::new(read_origin),
                spend: CodexSpend::new(Arc::new(move |event| {
                    if let Some(session) = spend_target.upgrade() {
                        session.emit_to_turn(event);
                    }
                })),
                me: me.clone(),
                state: Mutex::new(State {
                    client: None,
                    served: None,
                    opening: None,
                    core: SessionCore::with_turns(TurnSlot::releasing(move || released.close())),
                    subagents: None,
                    closed: None,
                    last_tokens: 0,
                    goal_active: false,
                    report_goal: Arc::new(|_| {}),
                    background: BackgroundWork::default(),
                    background_read: 0,
                    skills: None,
                    thread_id: None,
                    retired_turns: VecDeque::new(),
                    unfinished_retired: Vec::new(),
                }),
                retired_finished: tokio::sync::Notify::new(),
            }
        })
    }

    /// What Codex calls this thread. A later run resumes it by this id.
    pub fn thread_id(&self) -> Option<String> {
        self.state.lock().unwrap().thread_id.clone()
    }

    fn emit_to_turn(&self, event: ProviderEvent) {
        let emit = self
            .state
            .lock()
            .unwrap()
            .core
            .turns
            .turn()
            .map(|turn| turn.input.hooks.emit.clone());
        if let Some(emit) = emit {
            emit(event);
        }
    }

    fn busy_of(state: &State) -> bool {
        state.core.turns.answering()
            || state
                .subagents
                .as_ref()
                .is_some_and(|subagents| subagents.busy())
            || state.background.running()
    }

    fn settle_if(&self, id: u64, result: ProviderResult) {
        self.state.lock().unwrap().core.turns.settle_if(id, result);
    }

    fn current(&self, id: u64) -> bool {
        self.state.lock().unwrap().core.turns.is_current(id)
    }

    fn with_turn<T>(
        &self,
        id: u64,
        change: impl FnOnce(&mut TurnData, &RunInput) -> T,
    ) -> Option<T> {
        let mut state = self.state.lock().unwrap();
        let turn = state.core.turns.turn_mut().filter(|turn| turn.id == id)?;
        let input = turn.input.clone();
        Some(change(&mut turn.data, &input))
    }

    async fn begin_turn(self: Arc<Self>, id: u64, input: RunInput) {
        self.record.label(&input.title);
        let opening = {
            let mut state = self.state.lock().unwrap();
            state
                .opening
                .get_or_insert_with(|| self.clone().open(input.clone()).boxed().shared())
                .clone()
        };
        if !opened(&self, id, opening).await {
            return;
        }
        let (client, thread_id, skills) = {
            let mut state = self.state.lock().unwrap();
            let (Some(client), Some(thread_id), Some(skills)) = (
                state.client.clone(),
                state.thread_id.clone(),
                state.skills.clone(),
            ) else {
                return;
            };
            if !state.core.turns.is_current(id) {
                return;
            }
            state
                .core
                .turns
                .announce(Continuation::new(AgentEngine::Codex, &thread_id));
            (client, thread_id, skills)
        };
        self.await_retired_turns().await;
        match &input.operation {
            Some(RunOperation::Compact { .. }) => {
                return self.begin_compaction(id, &client, &thread_id).await
            }
            Some(RunOperation::Review { target }) => {
                return self
                    .begin_review(id, &input, &client, &thread_id, target.clone())
                    .await
            }
            None => {}
        }
        let goal = goal_command(&input.prompt);
        if let Some(goal) = &goal {
            if !self.begin_goal(id, &input, &client, &thread_id, goal).await {
                return;
            }
        }
        let policy = codex_policy(input.policy, input.coordination_role);
        let prompt = match &goal {
            Some(Goal::Set(objective)) => objective.clone(),
            _ => input.prompt.clone(),
        };
        let params = json!({
            "threadId": thread_id,
            "input": skills.input(&prompt).await,
            "model": input.model,
            "effort": input.effort.as_str(),
            "serviceTier": if input.fast_mode == Some(true) { "priority" } else { "default" },
            "approvalPolicy": policy.approval_policy,
            "approvalsReviewer": policy.approvals_reviewer,
            "sandboxPolicy": sandbox_policy(policy.sandbox, input.project_root.as_deref(), None),
        });
        let started = match client.request("turn/start", params).await {
            Ok(started) => started,
            Err(error) => {
                self.settle_if(
                    id,
                    ProviderResult::failed(format!(
                        "Codex could not start the turn: {}",
                        error.reason()
                    )),
                );
                return;
            }
        };
        self.after_start(id, &input, started).await;
    }

    /// Records the turn the server started, interrupting it at once when the run already went away.
    async fn after_start(self: &Arc<Self>, id: u64, input: &RunInput, started: Value) {
        let turn_id = started["turn"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let interrupt_wanted = self.with_turn(id, |turn, _| {
            turn.turn_id = Some(turn_id.clone());
            turn.interrupt_wanted
        });
        self.record.began();
        match interrupt_wanted {
            None => {}
            Some(true) => self.interrupt_turn(id),
            Some(false) => self.drain(id, input).await,
        }
    }

    async fn begin_goal(
        &self,
        id: u64,
        input: &RunInput,
        client: &AppServerClient,
        thread_id: &str,
        goal: &Goal,
    ) -> bool {
        let result = match goal {
            Goal::Clear => client
                .request("thread/goal/clear", json!({ "threadId": thread_id }))
                .await
                .map(|_| {
                    self.state.lock().unwrap().goal_active = false;
                    (input.hooks.report_goal)(GoalReport::Changed { goal: None });
                    self.settle_if(id, ProviderResult::succeeded());
                    false
                }),
            Goal::Get => client
                .request("thread/goal/get", json!({ "threadId": thread_id }))
                .await
                .map(|result| {
                    self.report_codex_goal(&result["goal"]);
                    self.settle_if(id, ProviderResult::succeeded());
                    false
                }),
            Goal::Set(objective) => client
                .request(
                    "thread/goal/set",
                    json!({ "threadId": thread_id, "objective": objective }),
                )
                .await
                .map(|result| {
                    self.report_codex_goal(&result["goal"]);
                    true
                }),
        };
        result.unwrap_or_else(|error| {
            self.settle_if(
                id,
                ProviderResult::failed(format!(
                    "Codex could not update the goal: {}",
                    error.reason()
                )),
            );
            false
        })
    }

    fn report_codex_goal(&self, goal: &Value) {
        let report = {
            let mut state = self.state.lock().unwrap();
            state.goal_active = goal["status"] == "active";
            state.report_goal.clone()
        };
        let shown = (!goal.is_null() && goal["status"] != "complete").then(|| ActiveGoal {
            objective: goal["objective"].as_str().unwrap_or_default().to_string(),
            status: if goal["status"] == "active" {
                GoalStatus::Active
            } else {
                GoalStatus::Blocked
            },
            iterations: None,
            last_reason: None,
        });
        report(GoalReport::Changed { goal: shown });
    }

    /// Manual compaction is a thread operation, not an empty model turn.
    async fn begin_compaction(&self, id: u64, client: &AppServerClient, thread_id: &str) {
        self.set_compacting(id, true, None);
        if let Err(error) = client
            .request("thread/compact/start", json!({ "threadId": thread_id }))
            .await
        {
            if !self.current(id) {
                return;
            }
            self.set_compacting(
                id,
                false,
                Some(format!("Could not compact context: {}", error.reason())),
            );
            self.settle_if(id, ProviderResult::failed_silently());
        }
    }

    /// Inline delivery keeps the isolated native reviewer compatible with paginated history.
    async fn begin_review(
        self: &Arc<Self>,
        id: u64,
        input: &RunInput,
        client: &AppServerClient,
        thread_id: &str,
        target: Value,
    ) {
        match client
            .request(
                "review/start",
                json!({ "threadId": thread_id, "target": target, "delivery": "inline" }),
            )
            .await
        {
            Ok(started) => self.after_start(id, input, started).await,
            Err(error) => self.settle_if(
                id,
                ProviderResult::failed(format!(
                    "Codex could not start the review: {}",
                    error.reason()
                )),
            ),
        }
    }

    /// Spawns the server and puts the thread on it: a new one, the one the run continues, or a fork.
    async fn open(self: Arc<Self>, seed: RunInput) -> Result<(), OpenFailure> {
        {
            let mut state = self.state.lock().unwrap();
            state
                .background
                .open_with(seed.hooks.report_background.clone());
            state.report_goal = seed.hooks.report_goal.clone();
        }
        (seed.hooks.report_goal)(GoalReport::Changed { goal: None });
        let fail = OpenFailure::new;
        let brief = codex_brief(&seed);
        let tools: Vec<_> = brief
            .tools
            .iter()
            .flat_map(|set| set.tools.clone())
            .collect();
        if !tools.is_empty() {
            let Some(host) = self.host.clone() else {
                return Err(fail(
                    "Codex could not start: the app's tool service is unavailable.".into(),
                ));
            };
            let served = host.serve(tools).await.map_err(|error| {
                fail(format!(
                    "Codex could not start: {}",
                    first_line(&error.to_string())
                ))
            })?;
            let mut state = self.state.lock().unwrap();
            if state.core.ended {
                drop(state);
                served.release();
                return Err(fail(ENDED.into()));
            }
            state.served = Some(served);
        }
        let (args, token) = {
            let state = self.state.lock().unwrap();
            (
                codex_config(&seed, state.served.as_ref(), &brief.tools),
                state.served.as_ref().map(|served| served.token.clone()),
            )
        };
        let env = token.map(|token| {
            let mut env: HashMap<String, String> = std::env::vars().collect();
            env.insert(TOOL_TOKEN_ENV.into(), token);
            env
        });
        let command = codex_app_server(
            &args,
            ServerOptions {
                cwd: Some(seed.workspace_root.clone()),
                env,
                shared_home: false,
            },
        )
        .await
        .map_err(|error| {
            fail(format!(
                "Codex could not start: {}",
                first_line(&error.to_string())
            ))
        })?;
        if self.state.lock().unwrap().core.ended {
            return Err(fail(ENDED.into()));
        }
        let client = (self.connect)(command);
        let skills = CodexSkills::new(client.clone(), seed.workspace_root.clone());
        let read_client = client.clone();
        let rested = self.me.clone();
        let subagents = CodexSubagents::new(
            seed.hooks.report_subagent.clone(),
            Arc::new(move |busy| {
                if let Some(session) = rested.upgrade() {
                    if !busy && !session.state.lock().unwrap().core.turns.answering() {
                        (session.hooks.rested)();
                    }
                }
            }),
            Some(Arc::new(move |thread_id: String| {
                let client = read_client.clone();
                async move {
                    client
                        .request(
                            "thread/read",
                            json!({ "threadId": thread_id, "includeTurns": false }),
                        )
                        .await
                        .ok()
                        .map(|read| read["thread"].clone())
                }
                .boxed()
            })),
            Some(self.spend.clone()),
        );
        {
            let mut state = self.state.lock().unwrap();
            state.client = Some(client.clone());
            state.skills = Some(skills.clone());
            state.subagents = Some(subagents.clone());
        }
        self.listen(&client, &subagents, &skills);
        let exited = self.me.clone();
        let exit = client.exited();
        tokio::spawn(async move {
            let status = exit.await;
            if let Some(session) = exited.upgrade() {
                session.exited(status).await;
            }
        });
        let opened = async {
            client.initialize(client_info()).await?;
            adopt_app_skills(&client)
                .await
                .map_err(|error| AppServerError::Rpc {
                    method: "skills/extraRoots/set".into(),
                    code: 0,
                    message: error.to_string(),
                    data: Value::Null,
                })?;
            skills.refresh(true).await;
            let account = client
                .request("account/read", json!({ "refreshToken": false }))
                .await?;
            Ok::<_, AppServerError>(account)
        }
        .await;
        let describe = |error: AppServerError| match &error {
            AppServerError::Exited { exit, .. } => fail(start_failure(exit)),
            AppServerError::Rpc { .. } => {
                fail(format!("Codex could not start: {}", error.reason()))
            }
        };
        let account = opened.map_err(describe)?;
        if account["account"].is_null() {
            return Err(fail(SIGN_IN.into()));
        }
        let policy = codex_policy(seed.policy, seed.coordination_role);
        let settings = json!({
            "cwd": seed.workspace_root,
            "model": seed.model,
            "serviceTier": if seed.fast_mode == Some(true) { "priority" } else { "default" },
            "approvalPolicy": policy.approval_policy,
            "sandbox": policy.sandbox.id(),
            "approvalsReviewer": policy.approvals_reviewer,
            "config": { "model_reasoning_effort": seed.effort.as_str() },
            "developerInstructions": brief.instructions.join("\n\n"),
        });
        let continuation = seed.continuation_value().map(String::from);
        let started = match &continuation {
            None => client
                .request("thread/start", settings)
                .await
                .map_err(describe)?,
            Some(thread) if seed.fork_continuation => {
                let mut params = settings;
                params["threadId"] = json!(thread);
                params["excludeTurns"] = json!(true);
                client
                    .request("thread/fork", params)
                    .await
                    .map_err(describe)?
            }
            Some(thread) => match resume_thread(&client, thread, &settings).await {
                Ok(resumed) => resumed,
                Err(error @ AppServerError::Rpc { .. }) => return Err(OpenFailure::lost(format!(
                    "Codex could not continue this thread ({}). Start a new thread to keep going.",
                    error.reason()
                ))),
                Err(error) => return Err(describe(error)),
            },
        };
        let thread_id = started["thread"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if needs_side_chat_boundary(&seed) {
            client
                .request("thread/inject_items", json!({ "threadId": thread_id, "items": [{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": SIDE_CHAT_BOUNDARY }] }] }))
                .await
                .map_err(describe)?;
        }
        self.state.lock().unwrap().thread_id = Some(thread_id.clone());
        self.record
            .opened(client.clone(), &thread_id, &seed.workspace_root);
        if continuation.is_some() {
            self.record.began();
        }
        subagents.set_root_thread_id(&thread_id);
        Ok(())
    }

    fn is_thread(&self, params: &Value) -> bool {
        let state = self.state.lock().unwrap();
        state.thread_id.is_some() && params["threadId"].as_str() == state.thread_id.as_deref()
    }

    fn listen(&self, client: &AppServerClient, subagents: &CodexSubagents, skills: &CodexSkills) {
        let me = self.me.clone();
        let on = |method: &str, handler: Box<dyn Fn(&Arc<CodexSession>, Value) + Send + Sync>| {
            let me = me.clone();
            client.on(method, move |params| {
                if let Some(session) = me.upgrade() {
                    handler(&session, params);
                }
            });
        };
        let children = subagents.clone();
        on(
            "thread/started",
            Box::new(move |_, params| {
                children.thread_started(&params);
            }),
        );
        let children = subagents.clone();
        on(
            "thread/status/changed",
            Box::new(move |_, params| {
                children.thread_status_changed(&params);
            }),
        );
        let children = subagents.clone();
        on(
            "thread/closed",
            Box::new(move |_, params| {
                children.thread_closed(&params);
            }),
        );
        let children = subagents.clone();
        on(
            "turn/started",
            Box::new(move |session, params| {
                let thread = params["threadId"].as_str().unwrap_or_default().to_string();
                if children.turn_started(&params) {
                    if let Some(held) = children.take_held_stop(&thread) {
                        session.request_quietly(
                            "turn/interrupt",
                            json!({ "threadId": held.thread_id, "turnId": held.turn_id }),
                        );
                    }
                } else if session.is_thread(&params) {
                    let mut state = session.state.lock().unwrap();
                    let goal_active = state.goal_active;
                    if let Some(turn) = state.core.turns.turn_mut() {
                        let compacting = turn.data.turn_id.is_none()
                            && matches!(turn.input.operation, Some(RunOperation::Compact { .. }));
                        if goal_active || compacting {
                            turn.data.turn_id = params["turn"]["id"].as_str().map(String::from);
                        }
                    }
                }
            }),
        );
        on(
            "thread/goal/updated",
            Box::new(|session, params| {
                if session.is_thread(&params) {
                    session.report_codex_goal(&params["goal"]);
                }
            }),
        );
        on(
            "thread/goal/cleared",
            Box::new(|session, params| {
                if !session.is_thread(&params) {
                    return;
                }
                let report = {
                    let mut state = session.state.lock().unwrap();
                    state.goal_active = false;
                    state.report_goal.clone()
                };
                report(GoalReport::Changed { goal: None });
            }),
        );
        let refreshing = skills.clone();
        on(
            "skills/changed",
            Box::new(move |_, _| {
                drop(refreshing.refresh(true));
            }),
        );
        let children = subagents.clone();
        on(
            "item/started",
            Box::new(move |session, params| {
                if !children.item_started(&params) && session.is_thread(&params) {
                    session.receive_started(&params["item"]);
                }
            }),
        );
        on(
            "item/agentMessage/delta",
            Box::new(|session, params| {
                if session.is_thread(&params) {
                    session.receive_delta(
                        params["itemId"].as_str().unwrap_or_default(),
                        params["delta"].as_str().unwrap_or_default(),
                    );
                }
            }),
        );
        let children = subagents.clone();
        on(
            "item/completed",
            Box::new(move |session, params| {
                let child = children.item_completed(&params);
                let own = session.is_thread(&params);
                if !child && own {
                    session.receive_completed(&params["item"]);
                }
                if own
                    && params["item"]["type"] == "commandExecution"
                    && session.state.lock().unwrap().background.running()
                {
                    tokio::spawn(session.clone().refresh_background_processes());
                }
            }),
        );
        let children = subagents.clone();
        on(
            "thread/tokenUsage/updated",
            Box::new(move |session, params| {
                if children.token_usage_updated(&params) || !session.is_thread(&params) {
                    return;
                }
                let usage = &params["tokenUsage"];
                session
                    .spend
                    .report(params["threadId"].as_str().unwrap_or_default(), usage);
                session.receive_usage(
                    usage["last"]["totalTokens"].as_u64().unwrap_or(0),
                    usage["modelContextWindow"].as_u64(),
                );
            }),
        );
        let children = subagents.clone();
        on(
            "error",
            Box::new(move |session, params| {
                if children.error(&params) || !session.is_thread(&params) {
                    return;
                }
                let mut state = session.state.lock().unwrap();
                let Some(turn) = state.core.turns.turn_mut() else {
                    return;
                };
                if params["willRetry"] == true {
                    turn.input.emit(ProviderEvent::Retry(RetryNotice {
                        message: retry_reason(&params["error"]),
                        attempt: None,
                        max_retries: None,
                    }));
                } else {
                    turn.data.failure = Some(params["error"].clone());
                }
            }),
        );
        let children = subagents.clone();
        on(
            "turn/completed",
            Box::new(move |session, params| {
                if !children.turn_completed(&params) && session.is_thread(&params) {
                    session.receive_turn_completed(params["turn"].clone());
                }
            }),
        );
        on(
            "serverRequest/resolved",
            Box::new(|session, params| session.questions.resolved(&params["requestId"])),
        );
        let me = self.me.clone();
        client.on_request(move |request| {
            if let Some(session) = me.upgrade() {
                session.answer(request);
            }
        });
    }

    fn request_quietly(&self, method: &str, params: Value) {
        let client = self.state.lock().unwrap().client.clone();
        if let Some(client) = client {
            let request = client.request(method, params);
            tokio::spawn(async move {
                let _ = request.await;
            });
        }
    }

    fn interrupt_turn(&self, id: u64) {
        let (client, thread_id, turn_id, children) = {
            let mut state = self.state.lock().unwrap();
            let Some(turn) = state.core.turns.turn_mut().filter(|turn| turn.id == id) else {
                return;
            };
            let Some(turn_id) = turn.data.turn_id.clone() else {
                turn.data.interrupt_wanted = true;
                return;
            };
            let (Some(client), Some(thread_id)) = (state.client.clone(), state.thread_id.clone())
            else {
                return;
            };
            let children = state
                .subagents
                .as_ref()
                .map(|subagents| subagents.live_turns())
                .unwrap_or_default();
            (client, thread_id, turn_id, children)
        };
        for child in children {
            let request = client.request(
                "turn/interrupt",
                json!({ "threadId": child.thread_id, "turnId": child.turn_id }),
            );
            tokio::spawn(async move {
                let _ = request.await;
            });
        }
        let request = client.request(
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": turn_id }),
        );
        tokio::spawn(async move {
            let _ = request.await;
        });
    }

    /// Feeds one run's steered messages into the turn already going.
    async fn drain(self: &Arc<Self>, id: u64, input: &RunInput) {
        let target = || {
            let state = self.state.lock().unwrap();
            let turn = state.core.turns.turn().filter(|turn| turn.id == id)?;
            Some((
                state.client.clone()?,
                state.thread_id.clone()?,
                turn.data.turn_id.clone()?,
                state.skills.clone()?,
            ))
        };
        let session = self.clone();
        drain_steering(
            &input.steering,
            &input.hooks.emit,
            target,
            |(client, thread_id, turn_id, skills): (AppServerClient, String, String, CodexSkills), prompt: String| {
                let session = session.clone();
                let report_goal = input.hooks.report_goal.clone();
                async move {
                    if goal_command(&prompt) == Some(Goal::Clear) {
                        client.request("thread/goal/clear", json!({ "threadId": thread_id })).await.map_err(anyhow::Error::new)?;
                        session.state.lock().unwrap().goal_active = false;
                        report_goal(GoalReport::Changed { goal: None });
                        return Ok(());
                    }
                    client.request("turn/steer", json!({ "threadId": thread_id, "input": skills.input(&prompt).await, "expectedTurnId": turn_id })).await.map_err(anyhow::Error::new)?;
                    Ok(())
                }
            },
            |error| match error.downcast_ref::<AppServerError>() {
                Some(error) => error.reason(),
                None => first_line(&error.to_string()).to_string(),
            },
        )
        .await;
    }

    async fn exited(self: Arc<Self>, exit: ExitStatus) {
        {
            let mut state = self.state.lock().unwrap();
            if state.core.ended {
                return;
            }
            let outcome = ProviderResult::failed(if state.thread_id.is_some() {
                format!("Codex stopped: {}", describe_exit(&exit))
            } else {
                start_failure(&exit)
            });
            state.core.outcome = Some(outcome.clone());
            state.core.turns.settle(outcome);
        }
        self.close_now().await;
    }

    fn receive_started(&self, item: &Value) {
        let mut state = self.state.lock().unwrap();
        let last_tokens = state.last_tokens;
        let Some(turn) = state.core.turns.turn_mut() else {
            return;
        };
        if item["type"] == "contextCompaction" {
            turn.data
                .compacting
                .report(&turn.input, true, last_tokens, None);
            return;
        }
        let Some(intent) = intent_of(item) else {
            return;
        };
        turn.data.items.insert(intent.tool_id.clone(), item.clone());
        turn.input.emit(ProviderEvent::Tool { intent });
    }

    fn receive_delta(&self, item_id: &str, delta: &str) {
        let mut state = self.state.lock().unwrap();
        let Some(turn) = state.core.turns.turn_mut() else {
            return;
        };
        if turn.data.review_output.is_some() {
            return;
        }
        let emit = turn.input.hooks.emit.clone();
        stream_markdown(&mut turn.data.streamed, &*emit, item_id, delta);
    }

    fn receive_completed(&self, item: &Value) {
        let mut settle = None;
        let mut retired = None;
        let mut state = self.state.lock().unwrap();
        let last_tokens = state.last_tokens;
        let (client, thread_id) = (state.client.clone(), state.thread_id.clone());
        let Some(turn) = state.core.turns.turn_mut() else {
            return;
        };
        let id = item["id"].as_str().unwrap_or_default().to_string();
        let turn_number = turn.id;
        match item["type"].as_str() {
            Some("agentMessage") => {
                if turn.data.review_output.is_some() {
                    return;
                }
                let questions = item["questions"].as_array().cloned().unwrap_or_default();
                if item["delivery"] == "async" && !questions.is_empty() {
                    if let (Some(client), Some(thread_id)) = (client, thread_id) {
                        let input = turn.input.clone();
                        let me = self.me.clone();
                        let session_questions = self.questions.clone();
                        tokio::spawn(async move {
                            let active = || {
                                me.upgrade().and_then(|session| {
                                    session
                                        .state
                                        .lock()
                                        .unwrap()
                                        .core
                                        .turns
                                        .turn()
                                        .filter(|turn| turn.id == turn_number)
                                        .and_then(|turn| turn.data.turn_id.clone())
                                })
                            };
                            if let Err(error) = session_questions
                                .answer_async(&questions, &input, &client, &thread_id, active)
                                .await
                            {
                                if let Some(session) = me.upgrade() {
                                    let reason = error
                                        .downcast_ref::<AppServerError>()
                                        .map(AppServerError::reason)
                                        .unwrap_or_else(|| {
                                            first_line(&error.to_string()).to_string()
                                        });
                                    session.settle_if(
                                        turn_number,
                                        ProviderResult::failed(format!(
                                            "Codex could not receive your answer: {reason}"
                                        )),
                                    );
                                }
                            }
                        });
                    }
                }
                let emit = turn.input.hooks.emit.clone();
                end_markdown(
                    &mut turn.data.streamed,
                    &*emit,
                    &id,
                    item["text"].as_str().unwrap_or_default(),
                );
            }
            Some("exitedReviewMode")
                if matches!(turn.input.operation, Some(RunOperation::Review { .. })) =>
            {
                let output = item["review"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !output.is_empty() && turn.data.review_output.is_none() {
                    turn.data.review_output = Some(output.clone());
                    turn.input.emit(ProviderEvent::Assistant {
                        message_id: id,
                        text: output,
                        append: false,
                        artifact: false,
                    });
                }
            }
            Some("imageGeneration") => {
                turn.data.items.remove(&id);
                if !turn.data.image_ids.insert(id.clone()) {
                    return;
                }
                let output = (self.image_output)(item.clone(), turn.input.workspace_root.clone());
                let me = self.me.clone();
                let saving = async move {
                    let text = output.await.unwrap_or_else(|error| {
                        format!(
                            "The generated image could not be saved or displayed: {}",
                            first_line(&error.to_string())
                        )
                    });
                    let Some(session) = me.upgrade() else { return };
                    let emit = {
                        let state = session.state.lock().unwrap();
                        state
                            .core
                            .turns
                            .turn()
                            .filter(|turn| turn.id == turn_number && !turn.input.is_cancelled())
                            .map(|turn| turn.input.hooks.emit.clone())
                    };
                    if let Some(emit) = emit {
                        emit(ProviderEvent::Assistant {
                            message_id: id,
                            text,
                            append: false,
                            artifact: true,
                        });
                    }
                }
                .boxed()
                .shared();
                turn.data.images.push(saving.clone());
                tokio::spawn(saving);
            }
            Some("contextCompaction") => {
                let manual = matches!(turn.input.operation, Some(RunOperation::Compact { .. }));
                let pre_tokens = turn.data.compacting.pre_tokens.unwrap_or(last_tokens);
                turn.input.emit(ProviderEvent::Compaction {
                    trigger: if manual {
                        CompactionTrigger::Manual
                    } else {
                        CompactionTrigger::Auto
                    },
                    pre_tokens,
                    post_tokens: None,
                });
                turn.data
                    .compacting
                    .report(&turn.input, false, last_tokens, None);
                if manual {
                    settle = Some(ProviderResult::succeeded());
                    retired = turn.data.turn_id.clone();
                }
            }
            _ => {
                turn.data.items.remove(&id);
            }
        }
        if let Some(turn_id) = retired {
            state.unfinished_retired.push(turn_id.clone());
            state.retired_turns.push_back(turn_id);
            if state.retired_turns.len() > 16 {
                state.retired_turns.pop_front();
            }
        }
        if let Some(result) = settle {
            state.core.turns.settle(result);
        }
    }

    fn set_compacting(&self, id: u64, compacting: bool, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        let last_tokens = state.last_tokens;
        if let Some(turn) = state.core.turns.turn_mut().filter(|turn| turn.id == id) {
            turn.data
                .compacting
                .report(&turn.input, compacting, last_tokens, error);
        }
    }

    /// The window the server reports is the model's own; the catalogue's stands in while it reports none.
    fn receive_usage(&self, tokens: u64, context_window: Option<u64>) {
        let mut state = self.state.lock().unwrap();
        if !state.core.turns.answering() {
            return;
        }
        state.last_tokens = tokens;
        let turn = state.core.turns.turn().unwrap();
        let limit = context_window
            .unwrap_or_else(|| context_window_limit(AgentEngine::Codex, &turn.input.model));
        turn.input.emit(ProviderEvent::Usage {
            tokens,
            limit,
            model: turn.input.model.clone(),
        });
    }

    /// Waits, at most a while, for turns the server still runs after their run settled.
    async fn await_retired_turns(&self) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let finished = self.retired_finished.notified();
            if self.state.lock().unwrap().unfinished_retired.is_empty() {
                return;
            }
            if tokio::time::timeout_at(deadline, finished).await.is_err() {
                return;
            }
        }
    }

    fn receive_turn_completed(&self, completed: Value) {
        if let Some(finished) = completed["id"].as_str() {
            let mut state = self.state.lock().unwrap();
            let before = state.unfinished_retired.len();
            state.unfinished_retired.retain(|turn| turn != finished);
            if state.unfinished_retired.len() != before {
                self.retired_finished.notify_waiters();
            }
        }
        let id = {
            let state = self.state.lock().unwrap();
            let Some(turn) = state.core.turns.turn() else {
                return;
            };
            if completed["id"]
                .as_str()
                .is_some_and(|id| state.retired_turns.iter().any(|retired| retired == id))
            {
                return;
            }
            if turn
                .data
                .turn_id
                .as_deref()
                .is_some_and(|turn_id| Some(turn_id) != completed["id"].as_str())
            {
                return;
            }
            turn.id
        };
        if let Some(session) = self.me.upgrade() {
            tokio::spawn(session.finish_turn(id, completed));
        }
    }

    /// Reads terminals before settling, so the session cannot be reclaimed while new work is running.
    async fn finish_turn(self: Arc<Self>, id: u64, completed: Value) {
        let images = self
            .with_turn(id, |turn, _| turn.images.clone())
            .unwrap_or_default();
        join_all(images).await;
        self.clone().refresh_background_processes().await;
        let (failure, client) = {
            let state = self.state.lock().unwrap();
            let Some(turn) = state.core.turns.turn().filter(|turn| turn.id == id) else {
                return;
            };
            let review = matches!(turn.input.operation, Some(RunOperation::Review { .. }));
            if completed["status"] == "completed" && state.goal_active && !review {
                return;
            }
            (turn.data.failure.clone(), state.client.clone())
        };
        match completed["status"].as_str() {
            Some("completed") => self.settle_if(id, ProviderResult::succeeded()),
            Some("interrupted") => self.settle_if(id, ProviderResult::cancelled()),
            Some("failed") => {
                let limited = [
                    &completed["error"],
                    failure.as_ref().unwrap_or(&Value::Null),
                ]
                .iter()
                .any(|error| error["codexErrorInfo"] == "usageLimitExceeded");
                let limit = match (limited, client) {
                    (true, Some(client)) => read_codex_usage_limit(&client).await,
                    _ => None,
                };
                let message = completed["error"]["message"]
                    .as_str()
                    .or_else(|| {
                        failure
                            .as_ref()
                            .and_then(|failure| failure["message"].as_str())
                    })
                    .unwrap_or("Codex could not finish the turn.")
                    .to_string();
                self.settle_if(id, ProviderResult::failed(message).with_limit(limit));
            }
            _ => {}
        }
    }

    /// Every page of terminals the server still runs: the panel treats each report as the whole set.
    async fn list_terminals(client: &AppServerClient, thread_id: &str) -> Option<Vec<Value>> {
        let mut found = Vec::new();
        let mut cursors = HashSet::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = client
                .request(
                    "thread/backgroundTerminals/list",
                    json!({ "threadId": thread_id, "cursor": cursor, "limit": 100 }),
                )
                .await
                .ok()?;
            found.extend(page["data"].as_array().cloned().unwrap_or_default());
            cursor = page["nextCursor"].as_str().map(String::from);
            match &cursor {
                None => return Some(found),
                Some(next) if !cursors.insert(next.clone()) => return Some(found),
                Some(_) => {}
            }
        }
    }

    /// Interrupting a child's turn leaves the commands it started running, so its stop ends them too.
    async fn terminate_terminals(client: AppServerClient, thread_id: String) {
        let Some(terminals) = Self::list_terminals(&client, &thread_id).await else {
            return;
        };
        let stops = terminals
            .iter()
            .filter_map(|terminal| terminal["processId"].as_str())
            .map(|process| {
                client.request(
                    "thread/backgroundTerminals/terminate",
                    json!({ "threadId": thread_id, "processId": process }),
                )
            });
        join_all(stops).await;
    }

    async fn refresh_background_processes(self: Arc<Self>) {
        let (client, thread_id, read) = {
            let mut state = self.state.lock().unwrap();
            let (Some(client), Some(thread_id)) = (state.client.clone(), state.thread_id.clone())
            else {
                return;
            };
            if state.core.ended {
                return;
            }
            state.background_read += 1;
            (client, thread_id, state.background_read)
        };
        let mut found = Vec::new();
        let mut cursors = HashSet::new();
        let mut cursor: Option<String> = None;
        loop {
            let Ok(page) = client
                .request(
                    "thread/backgroundTerminals/list",
                    json!({ "threadId": thread_id, "cursor": cursor, "limit": 100 }),
                )
                .await
            else {
                return;
            };
            {
                let state = self.state.lock().unwrap();
                if read != state.background_read
                    || state.core.ended
                    || state.thread_id.as_deref() != Some(thread_id.as_str())
                {
                    return;
                }
            }
            for terminal in page["data"].as_array().into_iter().flatten() {
                let command = terminal["command"]
                    .as_str()
                    .filter(|command| !command.is_empty())
                    .unwrap_or("Background shell");
                found.push(BackgroundProcess {
                    id: terminal["processId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    kind: BackgroundProcessKind::Shell,
                    description: command.to_string(),
                });
            }
            cursor = page["nextCursor"].as_str().map(String::from);
            match &cursor {
                None => break,
                Some(next) if !cursors.insert(next.clone()) => return,
                Some(_) => {}
            }
        }
        let rested = {
            let mut state = self.state.lock().unwrap();
            let ids = found.iter().map(|process| process.id.clone()).collect();
            let was_running = state.background.replace(found, ids);
            was_running && !Self::busy_of(&state)
        };
        if rested {
            (self.hooks.rested)();
        }
    }

    /// Whether the run allows what the server is asking. Nothing is allowed with no run to ask.
    async fn allowed(&self, intent: ToolIntent, workspace: bool) -> bool {
        let (input, id) = {
            let state = self.state.lock().unwrap();
            let Some(turn) = state.core.turns.turn() else {
                return false;
            };
            (turn.input.clone(), turn.id)
        };
        if workspace && input.coordination_role == Some(CoordinationRole::Coordinator) {
            return false;
        }
        if grants_tool(ToolReach::Workspace, Some(input.scope())) {
            return true;
        }
        let decision = (input.hooks.authorize)(intent).await;
        if let ToolDecision::DenyWith(reason) = &decision {
            self.explain_denial(id, reason);
        }
        decision.is_allow()
    }

    /// A decline carries no wording on the wire, so a denial that says why is steered into the turn.
    fn explain_denial(&self, id: u64, reason: &str) {
        let (client, thread_id, turn_id) = {
            let state = self.state.lock().unwrap();
            let Some(turn) = state.core.turns.turn().filter(|turn| turn.id == id) else {
                return;
            };
            let (Some(client), Some(thread_id), Some(turn_id)) = (
                state.client.clone(),
                state.thread_id.clone(),
                turn.data.turn_id.clone(),
            ) else {
                return;
            };
            (client, thread_id, turn_id)
        };
        let request = client.request("turn/steer", json!({ "threadId": thread_id, "input": [{ "type": "text", "text": reason, "text_elements": [] }], "expectedTurnId": turn_id }));
        tokio::spawn(async move {
            let _ = request.await;
        });
    }

    /// The pending item a server request addresses, kept apart across concurrent threads.
    fn pending_item(&self, thread_id: &str, item_id: &str) -> Option<Value> {
        let (own, subagents) = {
            let state = self.state.lock().unwrap();
            if state.thread_id.as_deref() == Some(thread_id) {
                (
                    Some(
                        state
                            .core
                            .turns
                            .turn()
                            .and_then(|turn| turn.data.items.get(item_id).cloned()),
                    ),
                    None,
                )
            } else {
                (None, state.subagents.clone())
            }
        };
        match own {
            Some(item) => item,
            None => subagents.and_then(|subagents| subagents.pending_item(thread_id, item_id)),
        }
    }

    /// The newest call one thread has going against one MCP server, which is what its prompt is about.
    fn pending_mcp_call(&self, thread_id: &str, server: &str) -> Option<Value> {
        let (own, subagents) = {
            let state = self.state.lock().unwrap();
            if state.thread_id.as_deref() == Some(thread_id) {
                let found = state.core.turns.turn().and_then(|turn| {
                    turn.data
                        .items
                        .values()
                        .filter(|item| item["type"] == "mcpToolCall" && item["server"] == server)
                        .last()
                        .cloned()
                });
                (Some(found), None)
            } else {
                (None, state.subagents.clone())
            }
        };
        match own {
            Some(item) => item,
            None => subagents.and_then(|subagents| subagents.pending_mcp_call(thread_id, server)),
        }
    }

    /// Every question the server asks goes to the run's user; a denial declines rather than cancels.
    fn answer(self: &Arc<Self>, request: IncomingRequest) {
        let params = request.params.clone();
        let session = self.clone();
        match request.method.as_str() {
            "item/commandExecution/requestApproval" => {
                let item_id = params["itemId"].as_str().unwrap_or_default().to_string();
                let started =
                    self.pending_item(params["threadId"].as_str().unwrap_or_default(), &item_id);
                let command = params["command"]
                    .as_str()
                    .map(String::from)
                    .or_else(|| {
                        started
                            .filter(|item| item["type"] == "commandExecution")
                            .and_then(|item| item["command"].as_str().map(String::from))
                    })
                    .unwrap_or_default();
                let mut input = json!({ "command": command });
                for key in ["cwd", "reason"] {
                    if params[key].as_str().is_some_and(|value| !value.is_empty()) {
                        input[key] = params[key].clone();
                    }
                }
                tokio::spawn(async move {
                    let allow = session
                        .allowed(
                            ToolIntent {
                                tool_id: item_id,
                                name: "command_execution".into(),
                                input,
                                write_path: None,
                            },
                            true,
                        )
                        .await;
                    request
                        .respond(json!({ "decision": if allow { "accept" } else { "decline" } }));
                });
            }
            "item/fileChange/requestApproval" => {
                let item_id = params["itemId"].as_str().unwrap_or_default().to_string();
                let started =
                    self.pending_item(params["threadId"].as_str().unwrap_or_default(), &item_id);
                let mut intent = started.as_ref().and_then(intent_of).unwrap_or(ToolIntent {
                    tool_id: item_id,
                    name: "file_change".into(),
                    input: json!({}),
                    write_path: None,
                });
                if !intent.input.is_object() {
                    intent.input = json!({});
                }
                if params["reason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty())
                {
                    intent.input["reason"] = params["reason"].clone();
                }
                tokio::spawn(async move {
                    let allow = session.allowed(intent, true).await;
                    request
                        .respond(json!({ "decision": if allow { "accept" } else { "decline" } }));
                });
            }
            "item/permissions/requestApproval" => {
                let mut input =
                    json!({ "cwd": params["cwd"], "permissions": params["permissions"] });
                if params["reason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty())
                {
                    input["reason"] = params["reason"].clone();
                }
                let intent = ToolIntent {
                    tool_id: params["itemId"].as_str().unwrap_or_default().to_string(),
                    name: "permissions".into(),
                    input,
                    write_path: None,
                };
                tokio::spawn(async move {
                    let allow = session.allowed(intent, true).await;
                    request.respond(json!({ "permissions": if allow { granted(&params["permissions"]) } else { json!({}) }, "scope": "turn" }));
                });
            }
            "item/tool/requestUserInput" => {
                let input = {
                    let state = self.state.lock().unwrap();
                    let own = state.thread_id.as_deref() == params["threadId"].as_str();
                    state
                        .core
                        .turns
                        .turn()
                        .filter(|turn| {
                            !(own
                                && turn.data.turn_id.is_some()
                                && turn.data.turn_id.as_deref() != params["turnId"].as_str())
                        })
                        .map(|turn| turn.input.clone())
                };
                let secret = params["questions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|question| question["isSecret"] == true);
                match input {
                    Some(input) if !secret => self.questions.answer(request, &input),
                    _ => request.fail(-32000, "This question cannot be answered in chat."),
                }
            }
            "mcpServer/elicitation/request" => {
                let meta = &params["_meta"];
                if meta["codex_approval_kind"] != "mcp_tool_call" {
                    request.respond(json!({ "action": "decline", "content": null, "_meta": null }));
                    return;
                }
                let server = params["serverName"].as_str().unwrap_or_default();
                let started =
                    self.pending_mcp_call(params["threadId"].as_str().unwrap_or_default(), server);
                let intent = started
                    .as_ref()
                    .and_then(intent_of)
                    .unwrap_or_else(|| ToolIntent {
                        tool_id: format!(
                            "{server}:{}",
                            match &request.id {
                                Value::String(id) => id.clone(),
                                other => other.to_string(),
                            }
                        ),
                        name: tool_named(params["message"].as_str().unwrap_or_default())
                            .unwrap_or_else(|| "mcp_tool_call".into()),
                        input: if meta["tool_params"].is_object() {
                            meta["tool_params"].clone()
                        } else {
                            json!({})
                        },
                        write_path: None,
                    });
                tokio::spawn(async move {
                    let allow = session.allowed(intent, false).await;
                    request.respond(if allow {
                        json!({ "action": "accept", "content": {}, "_meta": null })
                    } else {
                        json!({ "action": "decline", "content": null, "_meta": null })
                    });
                });
            }
            method => request.fail(-32601, &format!("{method} is not supported.")),
        }
    }

    /// Names this thread in Codex's own history, so it reads there as it reads here.
    pub fn label(&self, title: &str) {
        self.record.label(title);
    }

    /// Stops one subagent turn or terminal owned by this thread.
    pub fn stop_process(self: &Arc<Self>, process_id: &str) {
        let (client, thread_id, subagents) = {
            let state = self.state.lock().unwrap();
            let (Some(client), Some(thread_id)) = (state.client.clone(), state.thread_id.clone())
            else {
                return;
            };
            (client, thread_id, state.subagents.clone())
        };
        let process = process_id.to_string();
        match subagents.and_then(|subagents| subagents.stop(process_id)) {
            Some(StopTarget::Turn(child)) => {
                tokio::spawn(async move {
                    let _ = client
                        .request(
                            "turn/interrupt",
                            json!({ "threadId": child.thread_id, "turnId": child.turn_id }),
                        )
                        .await;
                    Self::terminate_terminals(client, process).await;
                });
            }
            Some(StopTarget::Held) => {}
            None => {
                let session = self.clone();
                tokio::spawn(async move {
                    let _ = client
                        .request(
                            "thread/backgroundTerminals/terminate",
                            json!({ "threadId": thread_id, "processId": process }),
                        )
                        .await;
                    session.refresh_background_processes().await;
                });
            }
        }
    }

    /// Ends the session: settles its turn, stops its children and its process. Settles once the
    /// process has exited.
    fn close_now(&self) -> BoxFuture<'static, ()> {
        let (client, served, subagents, report_goal) = {
            let mut state = self.state.lock().unwrap();
            if state.core.ended {
                return state
                    .closed
                    .clone()
                    .map(|closed| closed.boxed())
                    .unwrap_or_else(|| async {}.boxed());
            }
            state.core.ended = true;
            state.background_read += 1;
            state.background.clear();
            state.core.turns.settle(ProviderResult::cancelled());
            (
                state.client.take(),
                state.served.take(),
                state.subagents.take(),
                state.report_goal.clone(),
            )
        };
        report_goal(GoalReport::Changed { goal: None });
        if let Some(subagents) = subagents {
            subagents.close();
        }
        if let Some(client) = &client {
            client.terminate();
        }
        let closed: Shared<BoxFuture<'static, ()>> = match client {
            Some(client) => async move {
                client.close().await;
            }
            .boxed()
            .shared(),
            None => async {}.boxed().shared(),
        };
        tokio::spawn(closed.clone());
        self.state.lock().unwrap().closed = Some(closed.clone());
        if let Some(served) = served {
            served.release();
        }
        (self.hooks.ended)();
        closed.boxed()
    }
}

impl TurnSession for CodexSession {
    type Turn = TurnData;
    type State = State;
    const ENDED: &'static str = ENDED;

    fn key(&self) -> &str {
        &self.key
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn core(state: &mut State) -> &mut SessionCore<TurnData> {
        &mut state.core
    }

    fn handle(state: &State) -> Option<&str> {
        state.thread_id.as_deref()
    }

    fn busy(state: &State) -> bool {
        Self::busy_of(state)
    }

    fn open_turn(&self, _state: &mut State, _input: &RunInput) -> TurnData {
        TurnData::default()
    }

    fn begin(self: Arc<Self>, id: u64, input: RunInput) -> BoxFuture<'static, ()> {
        self.begin_turn(id, input).boxed()
    }

    fn interrupt(&self, id: u64) {
        self.interrupt_turn(id);
    }

    fn close(&self) -> BoxFuture<'static, ()> {
        self.close_now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_goal_commands() {
        assert_eq!(goal_command("/goal"), Some(Goal::Get));
        assert_eq!(goal_command("  /goal CLEAR "), Some(Goal::Clear));
        assert_eq!(
            goal_command("/goal ship it\nnow"),
            Some(Goal::Set("ship it\nnow".into()))
        );
        assert_eq!(goal_command("/goals"), None);
        assert_eq!(
            tool_named("Allow the server to run tool \"search\"?").as_deref(),
            Some("search")
        );
    }

    #[test]
    fn file_changes_name_the_file_they_write() {
        let intent = intent_of(&json!({ "type": "fileChange", "id": "i", "changes": [{ "path": "/a.rs", "kind": { "type": "update" }, "diff": "+" }] })).unwrap();
        assert_eq!(intent.write_path.as_deref(), Some("/a.rs"));
        assert_eq!(intent.input["changes"][0]["kind"], "update");
        assert!(intent_of(&json!({ "type": "reasoning", "id": "r" })).is_none());
    }
}
