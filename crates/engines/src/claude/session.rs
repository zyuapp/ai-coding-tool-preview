//! One live Claude session, kept across turns. The process, its MCP servers and everything it left
//! running in the background belong to the session, so a second turn neither pays for a new process
//! nor replays the transcript. The input stays open between turns; each turn ends on its own result.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use futures::future::{BoxFuture, FutureExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::background::BackgroundWork;
use crate::brief::{needs_side_chat_boundary, SIDE_CHAT_BOUNDARY};
use crate::catalog::{claude_effort, context_window_limit, model_takes_effort};
use crate::markdown::{append_complete_markdown, MarkdownBuffer};
use crate::provider::{AgentTurn, Emit, ProviderEvent, ProviderResult, RunInput, ToolDecision};
use crate::running_totals::RunningTotals;
use crate::session_pool::{PooledSession, SessionHooks};
use crate::session_turn::{answer_of, spawn_abort_watch, TurnSlot};
use crate::steer::SteerChannel;
use crate::tools::{grants_tool, tool_reach, ApprovalScope, ServedToolSet};
use crate::types::{
    ActiveGoal, ActivityKind, AgentEffort, AgentEngine, BackgroundProcess, BackgroundProcessKind,
    CompactionTrigger, Continuation, ExecutionPolicy, GoalReport, GoalStatus, LimitWindow,
    RetryNotice, SubagentMetadata, SubagentReport, SubagentTerminalStatus, ToolIntent, UsageLimit,
    WorkflowEndStatus, WorkflowReport,
};
use crate::workflow::parse_workflow_progress;

use super::query::{CanUseTool, ClaudeOptions, ClaudeQuery, HookCallback, QueryOutput};

/// The model the agent process stamps on replies it produced itself.
const SYNTHETIC_MODEL: &str = "<synthetic>";
/// Claude Code's own command for summarizing the session so far.
const COMPACT_COMMAND: &str = "/compact";
const METADATA_LIMIT: usize = 1_000;

fn number(value: &Value) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|n| n as i64))
        .unwrap_or(0)
}

/// What a response spent: all it read, cached or not, and all it wrote.
fn tokens_of(usage: &Value) -> i64 {
    number(&usage["input_tokens"])
        + number(&usage["cache_creation_input_tokens"])
        + number(&usage["cache_read_input_tokens"])
        + number(&usage["output_tokens"])
}

fn retry_reason(message: &Value) -> String {
    match message["error"].as_str() {
        Some("overloaded") => "Claude is overloaded.".into(),
        Some("rate_limit") => "Claude is rate limited.".into(),
        _ if message["error_status"].is_null() => "Cannot reach Claude.".into(),
        _ => format!("Claude API error {}.", message["error_status"]),
    }
}

/// The plan limit the account is held at, if the latest report says requests are being turned away.
pub fn usage_limit_of(info: Option<&Value>) -> Option<UsageLimit> {
    let info = info?;
    if info["status"] != "rejected" {
        return None;
    }
    let resets_at = info["resetsAt"].as_f64().filter(|value| *value != 0.0)?;
    if matches!(
        info["overageStatus"].as_str(),
        Some("allowed" | "allowed_warning")
    ) {
        return None;
    }
    let window = match info["rateLimitType"].as_str() {
        None | Some("five_hour") => LimitWindow::Session,
        Some(_) => LimitWindow::Weekly,
    };
    Some(UsageLimit {
        resets_at: resets_at * 1_000.0,
        window,
    })
}

fn background_processes(tasks: &[Value]) -> Vec<BackgroundProcess> {
    tasks
        .iter()
        .filter_map(|task| {
            let kind = match task["task_type"].as_str()? {
                "local_bash" => BackgroundProcessKind::Shell,
                "monitor_mcp" | "monitor_ws" => BackgroundProcessKind::Monitor,
                _ => return None,
            };
            Some(BackgroundProcess {
                id: task["task_id"].as_str()?.to_string(),
                kind,
                description: task["description"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// The effort a run asks the CLI to hold, or None to clear it for a model that takes none.
fn effort_flag(model: &str, effort: AgentEffort) -> Option<AgentEffort> {
    model_takes_effort(AgentEngine::Claude, model).then(|| claude_effort(effort))
}

pub fn claude_permission_mode(policy: ExecutionPolicy) -> &'static str {
    match policy {
        ExecutionPolicy::Confirm => "default",
        ExecutionPolicy::Plan => "plan",
        ExecutionPolicy::AllowEdits => "acceptEdits",
        ExecutionPolicy::Autonomous => "auto",
        ExecutionPolicy::Bypass => "bypassPermissions",
    }
}

/// What the run says, as the agent process takes it. A steered message asks to be folded into the
/// turn already going.
pub fn user_message(prompt: &str, now: bool) -> Value {
    let mut message = json!({ "type": "user", "message": { "role": "user", "content": prompt }, "parent_tool_use_id": null, "session_id": "" });
    if now {
        message["priority"] = json!("now");
    }
    message
}

fn write_path_for(tool_name: &str, input: &Value) -> Option<String> {
    if !matches!(tool_name, "Write" | "Edit" | "NotebookEdit") {
        return None;
    }
    input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))
        .and_then(Value::as_str)
        .map(String::from)
}

/// Every skill runs through one tool, so the name carries the skill that was asked for.
pub fn tool_display_name(tool_name: &str, input: &Value) -> String {
    match input["skill"].as_str() {
        Some(skill) if tool_name == "Skill" && !skill.is_empty() => format!("Skill: {skill}"),
        _ => tool_name.to_string(),
    }
}

pub fn normalize_tool_intent(tool_name: &str, input: &Value, tool_use_id: &str) -> ToolIntent {
    ToolIntent {
        tool_id: tool_use_id.to_string(),
        name: tool_display_name(tool_name, input),
        input: input.clone(),
        write_path: write_path_for(tool_name, input),
    }
}

/// Everything one turn builds up as it streams, and where what it produces goes.
struct Stream {
    emit: Emit,
    /// What the turn answers with, which its context meter is measured against.
    model: Option<String>,
    streamed_text: HashMap<String, MarkdownBuffer>,
    active_main_stream_id: Option<String>,
}

impl Stream {
    fn new(emit: Emit, model: Option<String>) -> Self {
        Stream {
            emit,
            model,
            streamed_text: HashMap::new(),
            active_main_stream_id: None,
        }
    }

    fn emit(&self, event: ProviderEvent) {
        (self.emit)(event)
    }
}

/// Whether a run is owed a turn past the one going: steered messages end the turn they interrupt and
/// are answered together in the one after it.
struct TurnData {
    steered: bool,
    stream: Stream,
}

struct OpenAgentTurn {
    id: u64,
    turn: AgentTurn,
    steered: bool,
    stream: Stream,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Run,
    Agent,
}

/// A bounded insertion-ordered map, dropping the oldest entry past the limit.
#[derive(Default)]
struct Bounded<V> {
    map: HashMap<String, V>,
    order: VecDeque<String>,
}

impl<V> Bounded<V> {
    fn get(&self, key: &str) -> Option<&V> {
        self.map.get(key)
    }

    fn insert(&mut self, key: String, value: V) {
        if self.map.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        }
        if self.map.len() > METADATA_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            }
        }
    }

    fn remove(&mut self, key: &str) {
        if self.map.remove(key).is_some() {
            self.order.retain(|entry| entry != key);
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

struct State {
    side_chat_boundary_pending: bool,
    query: Option<ClaudeQuery>,
    turns: TurnSlot<TurnData>,
    ended: bool,
    outcome: Option<ProviderResult>,
    agent_turn: Option<OpenAgentTurn>,
    next_agent_turn: u64,
    begin_agent_turn: Arc<dyn Fn() -> Option<AgentTurn> + Send + Sync>,
    workflows: RunningTotals,
    report_workflow: Arc<dyn Fn(WorkflowReport) + Send + Sync>,
    report_subagent: Arc<dyn Fn(SubagentReport) + Send + Sync>,
    report_goal: Arc<dyn Fn(GoalReport) + Send + Sync>,
    subagent_ids: Vec<String>,
    subagent_by_tool_use: Vec<(String, String)>,
    subagent_metadata: Bounded<SubagentMetadata>,
    subagent_efforts: Bounded<String>,
    has_goal: bool,
    rate_limit: Option<Value>,
    spent_by_id: RunningTotals,
    open_responses: HashMap<String, (String, Value)>,
    background: BackgroundWork,
    session_id: Option<String>,
    model: Option<String>,
    effort: Option<Option<AgentEffort>>,
    tool_sets: Vec<ServedToolSet>,
}

/// Work to do once the session lock is let go.
#[derive(Default)]
struct After {
    close: bool,
    rested: bool,
    reports: Vec<SubagentReport>,
}

pub struct ClaudeSession {
    key: String,
    hooks: SessionHooks,
    state: Mutex<State>,
    me: Weak<ClaudeSession>,
}

pub type SessionOpener = Box<
    dyn FnOnce(CanUseTool, Vec<(String, Vec<HookCallback>)>) -> anyhow::Result<ClaudeOptions>
        + Send,
>;

impl ClaudeSession {
    pub fn new(key: String, hooks: SessionHooks) -> Arc<Self> {
        Arc::new_cyclic(|me| ClaudeSession {
            key,
            hooks,
            me: me.clone(),
            state: Mutex::new(State {
                side_chat_boundary_pending: false,
                query: None,
                turns: TurnSlot::default(),
                ended: false,
                outcome: None,
                agent_turn: None,
                next_agent_turn: 1,
                begin_agent_turn: Arc::new(|| None),
                workflows: RunningTotals::default(),
                report_workflow: Arc::new(|_| {}),
                report_subagent: Arc::new(|_| {}),
                report_goal: Arc::new(|_| {}),
                subagent_ids: Vec::new(),
                subagent_by_tool_use: Vec::new(),
                subagent_metadata: Bounded::default(),
                subagent_efforts: Bounded::default(),
                has_goal: false,
                rate_limit: None,
                spent_by_id: RunningTotals::default(),
                open_responses: HashMap::new(),
                background: BackgroundWork::default(),
                session_id: None,
                model: None,
                effort: None,
                tool_sets: Vec::new(),
            }),
        })
    }

    /// What the agent process called this session. A later run resumes it by this id.
    pub fn session_id(&self) -> Option<String> {
        self.state.lock().unwrap().session_id.clone()
    }

    /// Starts the process for the run that seeded the session. `build` turns the session's callbacks
    /// into the process options.
    pub fn open(
        self: &Arc<Self>,
        seed: &RunInput,
        tool_sets: Vec<ServedToolSet>,
        build: SessionOpener,
    ) {
        {
            let mut state = self.state.lock().unwrap();
            state.side_chat_boundary_pending = needs_side_chat_boundary(seed);
            state.model = Some(seed.model.clone());
            state.effort = Some(effort_flag(&seed.model, seed.effort));
            state.report_workflow = seed.hooks.report_workflow.clone();
            state.report_subagent = seed.hooks.report_subagent.clone();
            state.report_goal = seed.hooks.report_goal.clone();
            state.begin_agent_turn = seed.hooks.begin_agent_turn.clone();
            state
                .background
                .open_with(seed.hooks.report_background.clone());
            (state.report_goal)(GoalReport::Changed { goal: None });
            state.has_goal = false;
            state.tool_sets = tool_sets;
        }
        let weak = self.me.clone();
        let can_use_tool: CanUseTool = Arc::new(move |name, input, tool_use_id, cancel| {
            let weak = weak.clone();
            async move {
                match weak.upgrade() {
                    Some(session) => session.can_use_tool(name, input, tool_use_id, cancel).await,
                    None => json!({ "behavior": "deny", "message": "The run this call belongs to is over." }),
                }
            }
            .boxed()
        });
        let weak = self.me.clone();
        let effort_hook: HookCallback = Arc::new(move |input, _| {
            if let Some(session) = weak.upgrade() {
                session.receive_subagent_effort(&input);
            }
            async { json!({}) }.boxed()
        });
        let hooks = vec![
            ("PreToolUse".to_string(), vec![effort_hook.clone()]),
            ("SubagentStop".to_string(), vec![effort_hook]),
        ];
        let spawned = build(can_use_tool, hooks).and_then(ClaudeQuery::spawn);
        match spawned {
            Ok((query, output)) => {
                self.state.lock().unwrap().query = Some(query);
                let session = self.clone();
                tokio::spawn(async move { session.pump(output).await });
            }
            Err(error) => {
                {
                    let mut state = self.state.lock().unwrap();
                    state.outcome = Some(ProviderResult::failed(error.to_string()));
                }
                self.close_now();
            }
        }
    }

    pub fn stop_process(&self, process_id: &str) {
        let query = self.state.lock().unwrap().query.clone();
        if let Some(query) = query {
            let id = process_id.to_string();
            tokio::spawn(async move {
                let _ = query.stop_task(&id).await;
            });
        }
    }

    fn run_turn(self: Arc<Self>, input: RunInput) -> BoxFuture<'static, ProviderResult> {
        let mut state = self.state.lock().unwrap();
        let Some(query) = state.query.clone().filter(|_| !state.ended) else {
            let outcome = state.outcome.clone().unwrap_or_else(|| {
                ProviderResult::failed("The agent session ended before the run could start.")
            });
            return async move { outcome }.boxed();
        };
        Self::end_agent_turn(&mut state, ProviderResult::cancelled());
        let emit = input.hooks.emit.clone();
        let data = TurnData {
            steered: false,
            stream: Stream::new(emit, Some(input.model.clone())),
        };
        let (taken, _) = state.turns.take(input.clone(), data);
        if taken.begin {
            let id = taken.id;
            let weak = self.me.clone();
            let give_up = self.me.clone();
            let interrupt_query = query.clone();
            let watcher = spawn_abort_watch(
                &input,
                move || {
                    tokio::spawn(async move {
                        let _ = interrupt_query.interrupt().await;
                    });
                },
                move || {
                    if let Some(session) = give_up.upgrade() {
                        let settled = session
                            .state
                            .lock()
                            .unwrap()
                            .turns
                            .settle_if(id, ProviderResult::cancelled())
                            .is_some();
                        if settled {
                            session.close_now();
                        }
                    }
                },
            );
            state.turns.watch(id, watcher);
            drop(state);
            tokio::spawn(async move {
                if let Some(session) = weak.upgrade() {
                    session.begin(id, input, query).await;
                }
            });
        }
        answer_of(taken.answer).boxed()
    }

    async fn begin(self: Arc<Self>, id: u64, input: RunInput, query: ClaudeQuery) {
        self.retune(&input, &query).await;
        let prompt = {
            let mut state = self.state.lock().unwrap();
            if !state.turns.is_current(id) {
                return;
            }
            if matches!(
                input.operation,
                Some(crate::types::RunOperation::Compact { .. })
            ) {
                drop(state);
                query.send_user(&user_message(COMPACT_COMMAND, false));
                return;
            }
            let prompt = if state.side_chat_boundary_pending {
                format!(
                    "{SIDE_CHAT_BOUNDARY}\n\nSide-chat request:\n{}",
                    input.prompt
                )
            } else {
                input.prompt.clone()
            };
            state.side_chat_boundary_pending = false;
            prompt
        };
        query.send_user(&user_message(&prompt, false));
        self.drain_steering(
            input.steering.clone(),
            input.hooks.emit.clone(),
            Which::Run,
            id,
        )
        .await;
    }

    /// A live session takes its settings as changes rather than as a reason to start over.
    async fn retune(&self, input: &RunInput, query: &ClaudeQuery) {
        let model_changed = {
            let mut state = self.state.lock().unwrap();
            let changed = state.model.as_deref() != Some(input.model.as_str());
            state.model = Some(input.model.clone());
            changed
        };
        if model_changed {
            let _ = query.set_model(&input.model).await;
        }
        let _ = query
            .set_permission_mode(claude_permission_mode(input.policy))
            .await;
        let effort = effort_flag(&input.model, input.effort);
        let effort_changed = {
            let mut state = self.state.lock().unwrap();
            let changed = state.effort != Some(effort);
            state.effort = Some(effort);
            changed
        };
        if effort_changed {
            let _ = query
                .apply_flag_settings(json!({ "effortLevel": effort.map(AgentEffort::as_str) }))
                .await;
        }
    }

    /// Feeds one run's steered messages into the session for as long as that run is the one going.
    async fn drain_steering(&self, steering: SteerChannel, emit: Emit, which: Which, id: u64) {
        while let Some(steer) = steering.next().await {
            let query = {
                let mut state = self.state.lock().unwrap();
                let owed = match which {
                    Which::Run => state
                        .turns
                        .turn_mut()
                        .filter(|turn| turn.id == id)
                        .map(|turn| &mut turn.data.steered),
                    Which::Agent => state
                        .agent_turn
                        .as_mut()
                        .filter(|turn| turn.id == id)
                        .map(|turn| &mut turn.steered),
                };
                let Some(owed) = owed else { return };
                *owed = true;
                state.query.clone()
            };
            if query.is_some_and(|query| query.send_user(&user_message(&steer.prompt, true))) {
                emit(ProviderEvent::Steered {
                    message_id: steer.message_id,
                });
            }
        }
    }

    async fn pump(self: Arc<Self>, mut output: mpsc::UnboundedReceiver<QueryOutput>) {
        let mut ended = Ok(());
        while let Some(item) = output.recv().await {
            match item {
                QueryOutput::Message(message) => {
                    let after = {
                        let mut state = self.state.lock().unwrap();
                        if state.ended {
                            break;
                        }
                        let mut after = After::default();
                        self.receive(&mut state, &message, &mut after);
                        after
                    };
                    self.after(after);
                }
                QueryOutput::End(result) => {
                    ended = result;
                    break;
                }
            }
        }
        {
            let mut state = self.state.lock().unwrap();
            let result = match ended {
                Ok(()) => ProviderResult::succeeded(),
                Err(message) => ProviderResult::failed(message)
                    .with_limit(usage_limit_of(state.rate_limit.as_ref())),
            };
            state.outcome = Some(result.clone());
            state.turns.settle(result.clone());
            Self::end_agent_turn(&mut state, result);
        }
        self.close_now();
    }

    fn after(&self, after: After) {
        let report = self.state.lock().unwrap().report_subagent.clone();
        for event in after.reports {
            report(event);
        }
        if after.rested {
            (self.hooks.rested)();
        }
        if after.close {
            self.close_now();
        }
    }

    fn busy_of(state: &State) -> bool {
        state.turns.answering() || state.agent_turn.is_some() || state.background.running()
    }

    /// Ends the session: settles what is in flight, reports what it held as stopped, and ends the
    /// process. Hooks run after the lock is let go.
    fn close_now(&self) {
        let (query, agent_turn, reports, workflow_reports, report_subagent, report_workflow) = {
            let mut state = self.state.lock().unwrap();
            if state.ended {
                return;
            }
            state.ended = true;
            state.background.clear();
            if state.has_goal {
                (state.report_goal)(GoalReport::Changed { goal: None });
            }
            state.has_goal = false;
            let agent_turn = state.agent_turn.take();
            let reports: Vec<SubagentReport> = state
                .subagent_ids
                .drain(..)
                .map(|id| SubagentReport::Finished {
                    id,
                    status: SubagentTerminalStatus::Stopped,
                    summary: "The session ended before this subagent finished.".into(),
                })
                .collect();
            state.subagent_by_tool_use.clear();
            state.subagent_metadata.clear();
            state.subagent_efforts.clear();
            let workflow_reports: Vec<WorkflowReport> = state
                .workflows
                .keys()
                .into_iter()
                .map(|id| WorkflowReport::Finished {
                    id,
                    status: WorkflowEndStatus::Stopped,
                    summary: String::new(),
                })
                .collect();
            state.workflows.clear();
            state.turns.settle(ProviderResult::cancelled());
            (
                state.query.take(),
                agent_turn,
                reports,
                workflow_reports,
                state.report_subagent.clone(),
                state.report_workflow.clone(),
            )
        };
        if let Some(open) = agent_turn {
            (open.turn.end)(ProviderResult::cancelled());
        }
        for report in reports {
            report_subagent(report);
        }
        for report in workflow_reports {
            report_workflow(report);
        }
        if let Some(query) = query {
            query.close();
        }
        (self.hooks.ended)();
    }

    fn end_agent_turn(state: &mut State, result: ProviderResult) {
        if let Some(open) = state.agent_turn.take() {
            (open.turn.end)(result);
        }
    }

    /// Ends whichever turn is in flight: the one a run asked for, or the one the agent started itself.
    fn conclude(state: &mut State, result: ProviderResult) {
        if state.turns.answering() {
            state.turns.settle(result);
        } else {
            Self::end_agent_turn(state, result);
        }
    }

    /// Whether this result ends a turn a steered message cut short rather than the run.
    fn absorbed_steering(state: &mut State) -> bool {
        let owing = match state.turns.turn_mut() {
            Some(turn) => &mut turn.data.steered,
            None => match state.agent_turn.as_mut() {
                Some(open) => &mut open.steered,
                None => return false,
            },
        };
        if !*owing {
            return false;
        }
        *owing = false;
        true
    }

    fn stream_mut(state: &mut State, which: Which) -> Option<&mut Stream> {
        match which {
            Which::Run => state.turns.turn_mut().map(|turn| &mut turn.data.stream),
            Which::Agent => state.agent_turn.as_mut().map(|open| &mut open.stream),
        }
    }

    fn current_stream(state: &State) -> Option<Which> {
        if state.turns.answering() {
            Some(Which::Run)
        } else if state.agent_turn.is_some() {
            Some(Which::Agent)
        } else {
            None
        }
    }

    /// Where the message goes. Between runs the agent still speaks, so a turn nobody asked for is
    /// given a run of its own rather than dropped.
    fn stream_for(&self, state: &mut State, message: &Value) -> Option<Which> {
        let kind = message["type"].as_str().unwrap_or_default();
        let streamed = kind == "assistant" || kind == "stream_event";
        if streamed && message["parent_tool_use_id"].is_string() {
            return None;
        }
        if let Some(which) = Self::current_stream(state) {
            return Some(which);
        }
        if state.ended || !streamed {
            return None;
        }
        self.open_agent_turn(state).then_some(Which::Agent)
    }

    fn open_agent_turn(&self, state: &mut State) -> bool {
        if state.agent_turn.is_some() {
            return true;
        }
        let Some(turn) = (state.begin_agent_turn)() else {
            return false;
        };
        let id = state.next_agent_turn;
        state.next_agent_turn += 1;
        let steering = turn.steering.clone();
        let emit = turn.emit.clone();
        state.agent_turn = Some(OpenAgentTurn {
            id,
            stream: Stream::new(turn.emit.clone(), state.model.clone()),
            turn,
            steered: false,
        });
        let weak = self.me.clone();
        tokio::spawn(async move {
            if let Some(session) = weak.upgrade() {
                session
                    .drain_steering(steering, emit, Which::Agent, id)
                    .await;
            }
        });
        true
    }

    fn receive(&self, state: &mut State, message: &Value, after: &mut After) {
        let kind = message["type"].as_str().unwrap_or_default();
        let subtype = message["subtype"].as_str().unwrap_or_default();
        if kind == "active_goal" {
            let value = &message["value"];
            state.has_goal = !value.is_null();
            let goal = (!value.is_null()).then(|| ActiveGoal {
                objective: value["condition"].as_str().unwrap_or_default().to_string(),
                status: GoalStatus::Active,
                iterations: value["iterations"].as_u64(),
                last_reason: value["last_reason"]
                    .as_str()
                    .filter(|r| !r.is_empty())
                    .map(String::from),
            });
            (state.report_goal)(GoalReport::Changed { goal });
            return;
        }
        if kind == "system" && subtype == "init" {
            state.session_id = message["session_id"].as_str().map(String::from);
        }
        if kind == "rate_limit_event" {
            state.rate_limit = Some(message["rate_limit_info"].clone());
            return;
        }
        if kind == "system" && subtype == "background_tasks_changed" {
            let tasks = message["tasks"].as_array().cloned().unwrap_or_default();
            let ids = tasks
                .iter()
                .filter_map(|task| task["task_id"].as_str().map(String::from))
                .collect();
            let was_running = state.background.replace(background_processes(&tasks), ids);
            if was_running && !Self::busy_of(state) {
                after.rested = true;
            }
        }
        self.receive_spend(state, message);
        if self.receive_workflow(state, message) {
            return;
        }
        self.receive_subagent_metadata(state, message, after);
        if self.receive_subagent(state, message, after) {
            return;
        }
        let Some(which) = self.stream_for(state, message) else {
            return;
        };
        if kind == "system" && subtype == "init" {
            let id = message["session_id"].as_str().unwrap_or_default();
            if let Some(stream) = Self::stream_mut(state, which) {
                stream.emit(ProviderEvent::Continuation(Continuation::new(
                    AgentEngine::Claude,
                    id,
                )));
            }
        } else if kind == "system" && subtype == "compact_boundary" {
            let metadata = &message["compact_metadata"];
            let trigger = if metadata["trigger"] == "manual" {
                CompactionTrigger::Manual
            } else {
                CompactionTrigger::Auto
            };
            if let Some(stream) = Self::stream_mut(state, which) {
                stream.emit(ProviderEvent::Compaction {
                    trigger,
                    pre_tokens: metadata["pre_tokens"].as_u64().unwrap_or(0),
                    post_tokens: metadata["post_tokens"].as_u64(),
                });
            }
        } else if kind == "system" && subtype == "api_retry" {
            let notice = RetryNotice {
                message: retry_reason(message),
                attempt: message["attempt"].as_u64(),
                max_retries: message["max_retries"].as_u64(),
            };
            if let Some(stream) = Self::stream_mut(state, which) {
                stream.emit(ProviderEvent::Retry(notice));
            }
        } else if kind == "system"
            && subtype == "status"
            && (message["status"] == "compacting"
                || message["compact_result"]
                    .as_str()
                    .is_some_and(|r| !r.is_empty()))
        {
            let error = (message["compact_result"] == "failed").then(|| {
                message["compact_error"]
                    .as_str()
                    .unwrap_or("Context compaction failed.")
                    .to_string()
            });
            if let Some(stream) = Self::stream_mut(state, which) {
                stream.emit(ProviderEvent::CompactionStatus {
                    compacting: message["status"] == "compacting",
                    error,
                });
            }
        } else if kind == "stream_event"
            && message["parent_tool_use_id"].is_null()
            && message["event"]["type"] == "message_start"
        {
            if let (Some(stream), Some(id)) = (
                Self::stream_mut(state, which),
                message["event"]["message"]["id"].as_str(),
            ) {
                stream.active_main_stream_id = Some(id.to_string());
                stream
                    .streamed_text
                    .insert(id.to_string(), MarkdownBuffer::default());
            }
        } else if kind == "stream_event"
            && message["parent_tool_use_id"].is_null()
            && message["event"]["type"] == "content_block_delta"
            && message["event"]["delta"]["type"] == "text_delta"
        {
            if let Some(stream) = Self::stream_mut(state, which) {
                if let Some(active) = stream.active_main_stream_id.clone() {
                    if let Some(buffered) = stream.streamed_text.get_mut(&active) {
                        let complete = append_complete_markdown(
                            buffered,
                            message["event"]["delta"]["text"]
                                .as_str()
                                .unwrap_or_default(),
                        );
                        let tail = buffered.text.clone();
                        if !complete.is_empty() {
                            stream.emit(ProviderEvent::Assistant {
                                message_id: active.clone(),
                                text: complete,
                                append: true,
                                artifact: false,
                            });
                        }
                        stream.emit(ProviderEvent::AssistantTail {
                            message_id: active,
                            text: tail,
                        });
                    }
                }
            }
        } else if kind == "assistant" {
            let inner = &message["message"];
            let model = inner["model"].as_str().unwrap_or_default();
            let Some(stream) = Self::stream_mut(state, which) else {
                return;
            };
            let stream_id = inner["id"].as_str().unwrap_or_default().to_string();
            let streamed = stream.streamed_text.remove(&stream_id);
            if let Some(buffer) = &streamed {
                if !buffer.text.is_empty() {
                    stream.emit(ProviderEvent::Assistant {
                        message_id: stream_id.clone(),
                        text: buffer.text.clone(),
                        append: true,
                        artifact: false,
                    });
                }
                if stream.active_main_stream_id.as_deref() == Some(stream_id.as_str()) {
                    stream.active_main_stream_id = None;
                }
            }
            let synthetic_error = !message["error"].is_null()
                && message.get("error").is_some()
                && model == SYNTHETIC_MODEL;
            for block in inner["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text")
                        if streamed.is_none()
                            && !synthetic_error
                            && !block["text"].as_str().unwrap_or_default().trim().is_empty() =>
                    {
                        stream.emit(ProviderEvent::Assistant {
                            message_id: message["uuid"].as_str().unwrap_or_default().to_string(),
                            text: block["text"].as_str().unwrap_or_default().to_string(),
                            append: false,
                            artifact: false,
                        });
                    }
                    Some("tool_use") => {
                        let intent = normalize_tool_intent(
                            block["name"].as_str().unwrap_or_default(),
                            &block["input"],
                            block["id"].as_str().unwrap_or_default(),
                        );
                        stream.emit(ProviderEvent::Tool { intent });
                    }
                    _ => {}
                }
            }
            if model != SYNTHETIC_MODEL {
                if let Some(turn_model) = stream.model.clone() {
                    let usage = &inner["usage"];
                    let tokens = number(&usage["input_tokens"])
                        + number(&usage["cache_creation_input_tokens"])
                        + number(&usage["cache_read_input_tokens"]);
                    stream.emit(ProviderEvent::Usage {
                        tokens: tokens.max(0) as u64,
                        limit: context_window_limit(AgentEngine::Claude, &turn_model),
                        model: model.to_string(),
                    });
                }
            }
        } else if kind == "result" {
            if Self::absorbed_steering(state) {
                return;
            }
            if subtype != "success" || message["is_error"] == true {
                let text = if subtype == "success" {
                    message["result"].as_str().unwrap_or_default().to_string()
                } else {
                    message["errors"]
                        .as_array()
                        .map(|errors| {
                            errors
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default()
                };
                let limit = usage_limit_of(state.rate_limit.as_ref());
                Self::conclude(state, ProviderResult::failed(text).with_limit(limit));
                after.close = true;
                return;
            }
            Self::conclude(state, ProviderResult::succeeded());
        }
    }

    /// Counts what a model response spent on the run in flight, whether the main loop or a subagent
    /// asked for it. A response adds only what it grew by since it was last seen.
    fn receive_spend(&self, state: &mut State, message: &Value) {
        let kind = message["type"].as_str().unwrap_or_default();
        if kind == "result" {
            state.spent_by_id.clear();
            state.open_responses.clear();
        } else if kind == "assistant"
            && message["message"]["model"] != SYNTHETIC_MODEL
            && message["message"]["usage"].is_object()
        {
            let id = message["message"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            self.spend(state, message, &id, tokens_of(&message["message"]["usage"]));
        } else if kind == "stream_event"
            && message["event"]["type"] == "message_start"
            && message["event"]["message"]["model"] != SYNTHETIC_MODEL
        {
            let started = &message["event"]["message"];
            if let (Some(id), true) = (started["id"].as_str(), started["usage"].is_object()) {
                let key = message["parent_tool_use_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                state
                    .open_responses
                    .insert(key, (id.to_string(), started["usage"].clone()));
            }
        } else if kind == "stream_event" && message["event"]["type"] == "message_delta" {
            let key = message["parent_tool_use_id"].as_str().unwrap_or_default();
            let Some((id, opened)) = state.open_responses.remove(key) else {
                return;
            };
            let usage = &message["event"]["usage"];
            let pick = |field: &str| {
                if usage[field].is_null() {
                    opened[field].clone()
                } else {
                    usage[field].clone()
                }
            };
            let merged = json!({
                "input_tokens": pick("input_tokens"),
                "cache_creation_input_tokens": pick("cache_creation_input_tokens"),
                "cache_read_input_tokens": pick("cache_read_input_tokens"),
                "output_tokens": usage["output_tokens"],
            });
            self.spend(state, message, &id, tokens_of(&merged));
        }
    }

    /// Adds what a response grew to since it was last counted. A subagent's spend is the run's.
    fn spend(&self, state: &mut State, message: &Value, response_id: &str, total: i64) {
        let which = if message["parent_tool_use_id"].is_string() {
            Self::current_stream(state)
        } else {
            self.stream_for(state, message)
        };
        let Some(which) = which else { return };
        let grown = state.spent_by_id.grow(response_id, total, 0);
        if grown > 0 {
            if let Some(stream) = Self::stream_mut(state, which) {
                stream.emit(ProviderEvent::Spend { tokens: grown });
            }
        }
    }

    /// Counts what a workflow spent on the run in flight, read off its running total.
    fn spend_workflow(state: &mut State, id: &str, total: i64) {
        let grown = state.workflows.grow(id, total, 0);
        if grown > 0 {
            if let Some(which) = Self::current_stream(state) {
                if let Some(stream) = Self::stream_mut(state, which) {
                    stream.emit(ProviderEvent::Spend { tokens: grown });
                }
            }
        }
    }

    /// Hooks report the child's effective effort, including model downgrades and agent overrides.
    fn receive_subagent_effort(&self, input: &Value) {
        let (Some(id), Some(effort)) = (
            input["agent_id"].as_str(),
            input["effort"]["level"].as_str(),
        ) else {
            return;
        };
        let report = {
            let mut state = self.state.lock().unwrap();
            if state.ended
                || state
                    .subagent_efforts
                    .get(id)
                    .is_some_and(|known| known == effort)
            {
                return;
            }
            state
                .subagent_efforts
                .insert(id.to_string(), effort.to_string());
            state
                .subagent_ids
                .iter()
                .any(|known| known == id)
                .then(|| state.report_subagent.clone())
        };
        if let Some(report) = report {
            report(SubagentReport::Metadata {
                id: id.to_string(),
                metadata: SubagentMetadata {
                    effort: Some(effort.to_string()),
                    ..Default::default()
                },
            });
        }
    }

    fn subagent_for_tool_use(state: &State, tool_use_id: &str) -> Option<String> {
        state
            .subagent_by_tool_use
            .iter()
            .find(|(tool, _)| tool == tool_use_id)
            .map(|(_, id)| id.clone())
    }

    /// The child reply identifies its actual model; the parent's settings cannot account for overrides.
    fn receive_subagent_metadata(&self, state: &mut State, message: &Value, after: &mut After) {
        let mut remember = |state: &mut State, tool_id: &str, metadata: SubagentMetadata| {
            let previous = state
                .subagent_metadata
                .get(tool_id)
                .cloned()
                .unwrap_or_default();
            let same = metadata
                .model
                .as_ref()
                .is_none_or(|m| previous.model.as_ref() == Some(m))
                && metadata
                    .effort
                    .as_ref()
                    .is_none_or(|e| previous.effort.as_ref() == Some(e))
                && metadata
                    .prompt
                    .as_ref()
                    .is_none_or(|p| previous.prompt.as_ref() == Some(p));
            if same && state.subagent_metadata.get(tool_id).is_some() {
                return;
            }
            let merged = SubagentMetadata {
                model: metadata.model.clone().or(previous.model),
                effort: metadata.effort.clone().or(previous.effort),
                prompt: metadata.prompt.clone().or(previous.prompt),
            };
            state.subagent_metadata.insert(tool_id.to_string(), merged);
            if let Some(id) = Self::subagent_for_tool_use(state, tool_id) {
                after
                    .reports
                    .push(SubagentReport::Metadata { id, metadata });
            }
        };
        let kind = message["type"].as_str().unwrap_or_default();
        if kind == "assistant" {
            for block in message["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if block["type"] != "tool_use"
                    || !(block["name"] == "Agent" || block["name"] == "Task")
                {
                    continue;
                }
                if let (Some(prompt), Some(id)) =
                    (block["input"]["prompt"].as_str(), block["id"].as_str())
                {
                    remember(
                        state,
                        id,
                        SubagentMetadata {
                            prompt: Some(prompt.to_string()),
                            ..Default::default()
                        },
                    );
                }
            }
            let model = message["message"]["model"].as_str().unwrap_or_default();
            if let Some(parent) = message["parent_tool_use_id"].as_str() {
                if !model.is_empty() && model != SYNTHETIC_MODEL {
                    remember(
                        state,
                        parent,
                        SubagentMetadata {
                            model: Some(model.to_string()),
                            ..Default::default()
                        },
                    );
                }
            }
        } else if kind == "stream_event" && message["event"]["type"] == "message_start" {
            if let Some(parent) = message["parent_tool_use_id"].as_str() {
                let model = message["event"]["message"]["model"]
                    .as_str()
                    .unwrap_or_default();
                if !model.is_empty() && model != SYNTHETIC_MODEL {
                    remember(
                        state,
                        parent,
                        SubagentMetadata {
                            model: Some(model.to_string()),
                            ..Default::default()
                        },
                    );
                }
            }
        }
    }

    /// Whether the message belonged to a subagent. Read before the stream guard, so one running in
    /// the background reports between turns too.
    fn receive_subagent(&self, state: &mut State, message: &Value, after: &mut After) -> bool {
        let kind = message["type"].as_str().unwrap_or_default();
        if kind == "assistant" {
            let Some(parent) = message["parent_tool_use_id"].as_str() else {
                return false;
            };
            let Some(id) = Self::subagent_for_tool_use(state, parent) else {
                return false;
            };
            for block in message["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
            {
                match block["type"].as_str() {
                    Some("text")
                        if !block["text"].as_str().unwrap_or_default().trim().is_empty() =>
                    {
                        after.reports.push(SubagentReport::Activity {
                            id: id.clone(),
                            activity_id: format!(
                                "{}:text",
                                message["uuid"].as_str().unwrap_or_default()
                            ),
                            kind: ActivityKind::Text,
                            title: None,
                            text: block["text"].as_str().unwrap_or_default().to_string(),
                        })
                    }
                    Some("tool_use") => after.reports.push(SubagentReport::Activity {
                        id: id.clone(),
                        activity_id: block["id"].as_str().unwrap_or_default().to_string(),
                        kind: ActivityKind::Tool,
                        title: Some(tool_display_name(
                            block["name"].as_str().unwrap_or_default(),
                            &block["input"],
                        )),
                        text: serde_json::to_string_pretty(&block["input"]).unwrap_or_default(),
                    }),
                    _ => {}
                }
            }
            return true;
        }
        if kind != "system" {
            return false;
        }
        let task_id = message["task_id"].as_str().unwrap_or_default().to_string();
        let subagent_type = message["subagent_type"].as_str().filter(|t| !t.is_empty());
        match message["subtype"].as_str().unwrap_or_default() {
            "task_started" if subagent_type.is_some() => {
                if !state.subagent_ids.contains(&task_id) {
                    state.subagent_ids.push(task_id.clone());
                }
                let tool_use = message["tool_use_id"].as_str().filter(|t| !t.is_empty());
                if let Some(tool_use) = tool_use {
                    state
                        .subagent_by_tool_use
                        .retain(|(tool, _)| tool != tool_use);
                    state
                        .subagent_by_tool_use
                        .push((tool_use.to_string(), task_id.clone()));
                }
                let mut metadata = tool_use
                    .and_then(|tool| state.subagent_metadata.get(tool).cloned())
                    .unwrap_or_default();
                if let Some(prompt) = message["prompt"].as_str() {
                    metadata.prompt = Some(prompt.to_string());
                }
                if let Some(effort) = state.subagent_efforts.get(&task_id) {
                    metadata.effort = Some(effort.clone());
                }
                after.reports.push(SubagentReport::Started {
                    id: task_id,
                    description: message["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    agent_type: subagent_type.map(String::from),
                    session_scoped: true,
                    metadata,
                });
                true
            }
            "task_progress" if subagent_type.is_some() || state.subagent_ids.contains(&task_id) => {
                after.reports.push(SubagentReport::Progress {
                    id: task_id,
                    description: message["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    agent_type: None,
                    last_tool_name: message["last_tool_name"]
                        .as_str()
                        .filter(|t| !t.is_empty())
                        .map(String::from),
                    summary: message["summary"]
                        .as_str()
                        .filter(|t| !t.is_empty())
                        .map(String::from),
                    total_tokens: message["usage"]["total_tokens"].as_u64().unwrap_or(0),
                });
                true
            }
            "task_notification" if state.subagent_ids.contains(&task_id) => {
                state.subagent_ids.retain(|id| *id != task_id);
                state.subagent_efforts.remove(&task_id);
                let tools: Vec<String> = state
                    .subagent_by_tool_use
                    .iter()
                    .filter(|(_, id)| *id == task_id)
                    .map(|(tool, _)| tool.clone())
                    .collect();
                state.subagent_by_tool_use.retain(|(_, id)| *id != task_id);
                for tool in tools {
                    state.subagent_metadata.remove(&tool);
                }
                let status = match message["status"].as_str() {
                    Some("completed") => SubagentTerminalStatus::Completed,
                    Some("failed") => SubagentTerminalStatus::Failed,
                    _ => SubagentTerminalStatus::Stopped,
                };
                after.reports.push(SubagentReport::Finished {
                    id: task_id,
                    status,
                    summary: message["summary"].as_str().unwrap_or_default().to_string(),
                });
                true
            }
            _ => false,
        }
    }

    /// Whether the message belonged to a workflow. A workflow reports between turns too.
    fn receive_workflow(&self, state: &mut State, message: &Value) -> bool {
        if message["type"] != "system" {
            return false;
        }
        let task_id = message["task_id"].as_str().unwrap_or_default().to_string();
        match message["subtype"].as_str().unwrap_or_default() {
            "task_started" if message["task_type"] == "local_workflow" => {
                state.workflows.set(&task_id, 0);
                let description = message["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                (state.report_workflow)(WorkflowReport::Started {
                    id: task_id,
                    name: message["workflow_name"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| description.clone()),
                    description,
                });
                true
            }
            "task_progress" if state.workflows.contains(&task_id) => {
                let total = message["usage"]["total_tokens"].as_i64().unwrap_or(0);
                Self::spend_workflow(state, &task_id, total);
                if let Some(progress) = parse_workflow_progress(&message["workflow_progress"]) {
                    (state.report_workflow)(WorkflowReport::Progress {
                        id: task_id,
                        phases: progress.phases,
                        agents: progress.agents,
                        total_tokens: total.max(0) as u64,
                        total_tool_calls: message["usage"]["tool_uses"].as_u64().unwrap_or(0),
                    });
                }
                true
            }
            "task_notification" if state.workflows.contains(&task_id) => {
                if message["usage"].is_object() {
                    Self::spend_workflow(
                        state,
                        &task_id,
                        message["usage"]["total_tokens"].as_i64().unwrap_or(0),
                    );
                }
                state.workflows.remove(&task_id);
                let status = match message["status"].as_str() {
                    Some("completed") => WorkflowEndStatus::Completed,
                    Some("failed") => WorkflowEndStatus::Failed,
                    _ => WorkflowEndStatus::Stopped,
                };
                (state.report_workflow)(WorkflowReport::Finished {
                    id: task_id,
                    status,
                    summary: message["summary"].as_str().unwrap_or_default().to_string(),
                });
                true
            }
            _ => false,
        }
    }

    async fn can_use_tool(
        self: Arc<Self>,
        tool_name: String,
        input: Value,
        tool_use_id: String,
        _cancel: CancellationToken,
    ) -> Value {
        let allow = json!({ "behavior": "allow", "updatedInput": input });
        let answered = |decision: ToolDecision| match decision {
            ToolDecision::Allow => json!({ "behavior": "allow", "updatedInput": input }),
            denial => json!({ "behavior": "deny", "message": denial.denial_reason() }),
        };
        let intent = normalize_tool_intent(&tool_name, &input, &tool_use_id);
        let authorize = {
            let mut state = self.state.lock().unwrap();
            let scope: Option<ApprovalScope> = state.turns.turn().map(|turn| turn.input.scope());
            if grants_tool(tool_reach(&tool_name, &state.tool_sets), scope) {
                return allow;
            }
            let run = state
                .turns
                .turn()
                .map(|turn| turn.input.hooks.authorize.clone());
            match run {
                Some(authorize) => Some(authorize),
                None if self.open_agent_turn(&mut state) => state
                    .agent_turn
                    .as_ref()
                    .map(|open| open.turn.authorize.clone()),
                None => None,
            }
        };
        match authorize {
            Some(authorize) => answered(authorize(intent).await),
            None => {
                json!({ "behavior": "deny", "message": "The run this call belongs to is over." })
            }
        }
    }
}

impl PooledSession for ClaudeSession {
    fn key(&self) -> String {
        self.key.clone()
    }

    fn live(&self) -> bool {
        !self.state.lock().unwrap().ended
    }

    fn busy(&self) -> bool {
        Self::busy_of(&self.state.lock().unwrap())
    }

    fn answering(&self) -> bool {
        self.state.lock().unwrap().turns.answering()
    }

    fn continues(&self, continuation: Option<&str>) -> bool {
        continuation
            .is_none_or(|value| self.state.lock().unwrap().session_id.as_deref() == Some(value))
    }

    fn run(self: Arc<Self>, input: RunInput) -> BoxFuture<'static, ProviderResult> {
        self.run_turn(input)
    }

    fn close(&self) -> BoxFuture<'static, ()> {
        self.close_now();
        async {}.boxed()
    }

    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_plan_names_when_it_lifts() {
        let limit = usage_limit_of(Some(
            &json!({ "status": "rejected", "resetsAt": 100, "rateLimitType": "seven_day" }),
        ))
        .unwrap();
        assert_eq!(limit.resets_at, 100_000.0);
        assert_eq!(limit.window, LimitWindow::Weekly);
        assert!(usage_limit_of(Some(
            &json!({ "status": "rejected", "resetsAt": 100, "overageStatus": "allowed" })
        ))
        .is_none());
        assert!(usage_limit_of(Some(&json!({ "status": "allowed", "resetsAt": 100 }))).is_none());
    }

    #[test]
    fn intents_name_skills_and_written_files() {
        let intent = normalize_tool_intent("Edit", &json!({ "file_path": "/a" }), "t");
        assert_eq!(intent.write_path.as_deref(), Some("/a"));
        assert_eq!(
            tool_display_name("Skill", &json!({ "skill": "review" })),
            "Skill: review"
        );
    }
}
