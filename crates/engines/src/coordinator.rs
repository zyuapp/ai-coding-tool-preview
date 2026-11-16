//! Runs, one live per thread: starts them on an engine, turns what the engine reports into
//! sequenced `AgentEvent`s, and holds the approvals and questions a run waits on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::future::BoxFuture;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

use crate::brief::RunToolkit;
use crate::catalog::capabilities;
use crate::provider::{
    AgentProvider, AgentTurn, ProviderEvent, ProviderResult, ResultStatus, RunHooks, RunInput,
    ToolDecision,
};
use crate::steer::{SteerChannel, SteerMessage};
use crate::types::{
    AgentEffort, AgentEngine, BackgroundReport, CompactionTrigger, ComputerUseRunConfig,
    Continuation, CoordinationRole, ExecutionPolicy, GoalReport, QuestionAnswers, QuestionRequest,
    RetryNotice, RunChannel, RunOperation, RunStatus, SubagentReport, ToolIntent, UsageLimit,
    WorkflowReport,
};

/// Tails arrive per token; this is often enough to read as typing without flooding the renderer.
const TAIL_INTERVAL: Duration = Duration::from_millis(40);
/// Long enough that a person who is around still decides; short enough that the next tick is not lost.
const UNATTENDED_APPROVAL: Duration = Duration::from_secs(10 * 60);
const UNATTENDED_DENIAL: &str = "Nobody is watching this scheduled run, so this action was denied rather than left waiting. Take a route that needs no approval, or stop and report what you could not do.";

/// One run event, as the renderer's reducer reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RunEventKind {
    #[serde(rename = "question.requested", rename_all = "camelCase")]
    QuestionRequested {
        request_id: String,
        request: QuestionRequest,
    },
    #[serde(rename = "question.answered", rename_all = "camelCase")]
    QuestionAnswered {
        request_id: String,
        question_id: String,
        text: String,
    },
    #[serde(rename = "question.closed", rename_all = "camelCase")]
    QuestionClosed { request_id: String },
    #[serde(rename = "run.started", rename_all = "camelCase")]
    RunStarted {
        #[serde(skip_serializing_if = "std::ops::Not::not", default)]
        agent_initiated: bool,
    },
    #[serde(rename = "run.status")]
    RunStatus {
        status: RunStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<UsageLimit>,
    },
    #[serde(rename = "assistant.delta", rename_all = "camelCase")]
    AssistantDelta {
        message_id: String,
        text: String,
        #[serde(skip_serializing_if = "std::ops::Not::not", default)]
        append: bool,
        #[serde(skip_serializing_if = "std::ops::Not::not", default)]
        artifact: bool,
    },
    #[serde(rename = "assistant.tail", rename_all = "camelCase")]
    AssistantTail { message_id: String, text: String },
    #[serde(rename = "context.usage")]
    ContextUsage {
        tokens: u64,
        limit: u64,
        model: String,
    },
    #[serde(rename = "context.spent")]
    ContextSpent { tokens: u64 },
    #[serde(rename = "context.compaction-status")]
    CompactionStatus {
        compacting: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "run.retrying")]
    Retrying(RetryNotice),
    #[serde(rename = "context.compacted", rename_all = "camelCase")]
    Compacted {
        trigger: CompactionTrigger,
        pre_tokens: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        post_tokens: Option<u64>,
    },
    #[serde(rename = "tool.intent")]
    ToolIntent { intent: ToolIntent },
    #[serde(rename = "computer-use.setup-required")]
    ComputerUseSetupRequired,
    #[serde(rename = "approval.requested", rename_all = "camelCase")]
    ApprovalRequested {
        approval_id: String,
        intent: ToolIntent,
        title: String,
        description: String,
    },
    #[serde(rename = "continuation.updated")]
    ContinuationUpdated { continuation: Continuation },
    #[serde(rename = "continuation.lost")]
    ContinuationLost,
    #[serde(rename = "queued.delivered", rename_all = "camelCase")]
    QueuedDelivered { message_id: String },
    #[serde(rename = "queued.steer-failed", rename_all = "camelCase")]
    QueuedSteerFailed { message_id: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    pub task_id: String,
    pub run_id: String,
    pub sequence: u64,
    #[serde(flatten)]
    pub event: RunEventKind,
}

/// A report that answers to the thread rather than to a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadScoped<T> {
    pub task_id: String,
    #[serde(flatten)]
    pub report: T,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReloadStatus {
    Pending,
    Reloaded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SettingsReloadEvent {
    #[serde(rename = "engine.settings-reload-status")]
    Status {
        status: ReloadStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

/// Everything the engine layer pushes back, on one channel so a workflow cannot overtake its own run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentEvent {
    Run(RunEvent),
    Workflow(ThreadScoped<WorkflowReport>),
    Background(ThreadScoped<BackgroundReport>),
    Subagent(ThreadScoped<SubagentReport>),
    Goal(ThreadScoped<GoalReport>),
    SettingsReload(SettingsReloadEvent),
}

/// What starts a run.
#[derive(Clone)]
pub struct StartRun {
    pub channel: RunChannel,
    pub task_id: String,
    pub title: String,
    pub run_id: String,
    pub prompt: String,
    pub workspace_root: String,
    pub project_root: Option<String>,
    pub projectless: bool,
    pub computer_use: ComputerUseRunConfig,
    pub policy: ExecutionPolicy,
    pub engine: AgentEngine,
    pub model: String,
    pub effort: AgentEffort,
    pub fast_mode: Option<bool>,
    pub operation: Option<RunOperation>,
    pub chrome_browser: bool,
    pub continuation: Option<Continuation>,
    pub fork_continuation: bool,
    pub fresh_session: bool,
    /// Started by the scheduler with nobody present: an approval nobody answers is denied for them.
    pub unattended: bool,
    pub coordination_role: Option<CoordinationRole>,
    /// The app's tools for this run, already short of what the user turned off.
    pub toolkit: Option<Arc<dyn RunToolkit>>,
}

pub type Emitter = Arc<dyn Fn(AgentEvent) + Send + Sync>;
pub type WritePathCheck = Arc<dyn Fn(String, String) -> BoxFuture<'static, bool> + Send + Sync>;

#[derive(Clone, Default)]
pub struct CoordinatorOptions {
    /// Whether a write path lies inside the run's workspace; a write outside it is denied unasked.
    pub is_write_path_inside: Option<WritePathCheck>,
    /// How a person reads a tool name in an approval title.
    pub tool_label: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
    pub tail_interval: Option<Duration>,
    pub unattended_approval: Option<Duration>,
}

/// `server · tool` for an MCP tool, the name itself otherwise.
pub fn default_tool_label(name: &str) -> String {
    static MCP: OnceLock<Regex> = OnceLock::new();
    let mcp = MCP.get_or_init(|| Regex::new(r"^mcp__(.+?)__(.+)$").unwrap());
    match mcp.captures(name) {
        Some(captures) => format!("{} · {}", &captures[1], captures[2].replace('_', " ")),
        None => name.to_string(),
    }
}

struct PendingApproval {
    id: String,
    intent: ToolIntent,
    answer: Option<oneshot::Sender<ToolDecision>>,
    deadline: Option<AbortHandle>,
}

struct PendingQuestions {
    request: QuestionRequest,
    answers: QuestionAnswers,
    close: Option<oneshot::Sender<Option<QuestionAnswers>>>,
    deadline: Option<AbortHandle>,
}

struct RunState {
    sequence: u64,
    terminal: bool,
    unattended: bool,
    approvals: Vec<PendingApproval>,
    questions: HashMap<String, PendingQuestions>,
    pending_tail: Option<(String, String)>,
    tail_timer: Option<AbortHandle>,
}

struct ActiveRun {
    task_id: String,
    run_id: String,
    workspace_root: String,
    cancel: CancellationToken,
    steering: SteerChannel,
    state: Mutex<RunState>,
}

impl ActiveRun {
    fn new(task_id: String, run_id: String, workspace_root: String, unattended: bool) -> Arc<Self> {
        Arc::new(ActiveRun {
            task_id,
            run_id,
            workspace_root,
            cancel: CancellationToken::new(),
            steering: SteerChannel::new(),
            state: Mutex::new(RunState {
                sequence: 0,
                terminal: false,
                unattended,
                approvals: Vec::new(),
                questions: HashMap::new(),
                pending_tail: None,
                tail_timer: None,
            }),
        })
    }
}

struct Inner {
    provider: Arc<dyn AgentProvider>,
    emit: Emitter,
    options: CoordinatorOptions,
    runs: Mutex<HashMap<String, Arc<ActiveRun>>>,
}

/// One live run per thread; different threads run concurrently. `emit` must not call back into
/// the coordinator synchronously.
#[derive(Clone)]
pub struct RunCoordinator {
    inner: Arc<Inner>,
}

impl RunCoordinator {
    pub fn new(
        provider: Arc<dyn AgentProvider>,
        emit: Emitter,
        options: CoordinatorOptions,
    ) -> Self {
        RunCoordinator {
            inner: Arc::new(Inner {
                provider,
                emit,
                options,
                runs: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn start(&self, command: StartRun) {
        let previous = self
            .inner
            .runs
            .lock()
            .unwrap()
            .get(&command.task_id)
            .cloned();
        if let Some(previous) = previous {
            self.cancel_active(&previous);
        }
        let active = ActiveRun::new(
            command.task_id.clone(),
            command.run_id.clone(),
            command.workspace_root.clone(),
            command.unattended,
        );
        self.inner
            .runs
            .lock()
            .unwrap()
            .insert(command.task_id.clone(), active.clone());
        self.publish(
            &active,
            RunEventKind::RunStarted {
                agent_initiated: false,
            },
        );
        self.publish(
            &active,
            RunEventKind::RunStatus {
                status: RunStatus::Running,
                message: None,
                limit: None,
            },
        );
        let input = self.run_input(&active, command);
        let this = self.clone();
        tokio::spawn(async move {
            let result = this.inner.provider.execute(input).await;
            if !this.is_current(&active) || active.state.lock().unwrap().terminal {
                return;
            }
            this.finish(&active, result.status, result.message, result.limit);
        });
    }

    fn run_input(&self, active: &Arc<ActiveRun>, command: StartRun) -> RunInput {
        let emit_target = (self.clone(), active.clone());
        let authorize_target = (self.clone(), active.clone());
        let question_target = (self.clone(), active.clone());
        let task = active.task_id.clone();
        let (workflow, background, subagent, goal) =
            (self.clone(), self.clone(), self.clone(), self.clone());
        let (workflow_task, background_task, subagent_task, goal_task) =
            (task.clone(), task.clone(), task.clone(), task.clone());
        let seed = (self.clone(), active.clone());
        let hooks = RunHooks {
            emit: Arc::new(move |event| emit_target.0.handle_provider_event(&emit_target.1, event)),
            authorize: Arc::new(move |intent| {
                let (this, active) = authorize_target.clone();
                Box::pin(async move { this.authorize(&active, intent).await })
            }),
            ask_question: Arc::new(move |request, cancel| {
                let (this, active) = question_target.clone();
                Box::pin(async move { this.ask_question(&active, request, cancel).await })
            }),
            report_workflow: Arc::new(move |report| {
                (workflow.inner.emit)(AgentEvent::Workflow(ThreadScoped {
                    task_id: workflow_task.clone(),
                    report,
                }))
            }),
            report_background: Arc::new(move |report| {
                (background.inner.emit)(AgentEvent::Background(ThreadScoped {
                    task_id: background_task.clone(),
                    report,
                }))
            }),
            report_subagent: Arc::new(move |report| {
                (subagent.inner.emit)(AgentEvent::Subagent(ThreadScoped {
                    task_id: subagent_task.clone(),
                    report,
                }))
            }),
            report_goal: Arc::new(move |report| {
                (goal.inner.emit)(AgentEvent::Goal(ThreadScoped {
                    task_id: goal_task.clone(),
                    report,
                }))
            }),
            begin_agent_turn: Arc::new(move || seed.0.begin_agent_turn(&seed.1)),
        };
        RunInput {
            channel: command.channel,
            task_id: command.task_id,
            title: command.title,
            prompt: command.prompt,
            workspace_root: command.workspace_root,
            project_root: command.project_root,
            projectless: command.projectless,
            computer_use: command.computer_use,
            policy: command.policy,
            engine: command.engine,
            model: command.model,
            effort: command.effort,
            fast_mode: capabilities(command.engine)
                .fast_mode
                .then(|| command.fast_mode.unwrap_or(false)),
            operation: command.operation,
            chrome_browser: command.chrome_browser,
            continuation: command.continuation,
            fork_continuation: command.fork_continuation,
            fresh_session: command.fresh_session,
            coordination_role: command.coordination_role,
            toolkit: command.toolkit,
            steering: active.steering.clone(),
            cancel: active.cancel.clone(),
            hooks,
        }
    }

    /// Only the run the message was queued against can take it; anything else leaves it for the next run.
    pub fn steer(&self, task_id: &str, run_id: &str, message_id: &str, prompt: &str) -> bool {
        let Some(active) = self.run(task_id).filter(|active| active.run_id == run_id) else {
            return false;
        };
        {
            let mut state = active.state.lock().unwrap();
            if state.terminal {
                return false;
            }
            state.unattended = false;
        }
        active.steering.push(SteerMessage {
            message_id: message_id.to_string(),
            prompt: prompt.to_string(),
        })
    }

    pub fn cancel(&self, task_id: &str, run_id: &str) -> bool {
        let Some(active) = self.run(task_id).filter(|active| active.run_id == run_id) else {
            return false;
        };
        self.cancel_active(&active);
        true
    }

    /// Kills one background process of the thread's session; work outlives runs, so it is addressed to the session.
    pub fn stop_process(&self, task_id: &str, process_id: &str) -> bool {
        self.inner.provider.stop_process(task_id, process_id)
    }

    pub fn retire_out_of_role(&self, task_id: &str, role: Option<CoordinationRole>) {
        self.inner.provider.retire_out_of_role(task_id, role)
    }

    pub fn label_thread(&self, task_id: &str, title: &str) -> bool {
        self.inner.provider.label_thread(task_id, title)
    }

    pub fn archive_thread(
        &self,
        task_id: &str,
        continuation: Option<Continuation>,
        archived: bool,
    ) -> bool {
        self.inner
            .provider
            .archive_thread(task_id, continuation, archived)
    }

    pub fn close_all(&self) {
        self.inner.provider.close_all()
    }

    pub fn decide_approval(
        &self,
        task_id: &str,
        run_id: &str,
        approval_id: &str,
        allow: bool,
    ) -> bool {
        let Some(active) = self.run(task_id).filter(|active| active.run_id == run_id) else {
            return false;
        };
        {
            let mut state = active.state.lock().unwrap();
            if state.terminal
                || !state
                    .approvals
                    .iter()
                    .any(|pending| pending.id == approval_id)
            {
                return false;
            }
            state.unattended = false;
            if !Self::settle_approval(
                &mut state,
                approval_id,
                if allow {
                    ToolDecision::Allow
                } else {
                    ToolDecision::Deny
                },
            ) {
                return false;
            }
        }
        self.show_approvals(&active);
        true
    }

    pub fn answer_question(
        &self,
        task_id: &str,
        run_id: &str,
        request_id: &str,
        question_id: &str,
        text: &str,
    ) -> bool {
        let Some(active) = self.run(task_id).filter(|active| active.run_id == run_id) else {
            return false;
        };
        if text.trim().is_empty() {
            return false;
        }
        let complete = {
            let mut state = active.state.lock().unwrap();
            if state.terminal {
                return false;
            }
            let Some(pending) = state.questions.get_mut(request_id) else {
                return false;
            };
            if !pending
                .request
                .questions
                .iter()
                .any(|question| question.id == question_id)
                || pending.answers.contains_key(question_id)
            {
                return false;
            }
            pending
                .answers
                .insert(question_id.to_string(), text.to_string());
            let complete = pending
                .request
                .questions
                .iter()
                .all(|question| pending.answers.contains_key(&question.id));
            state.unattended = false;
            complete
        };
        self.publish(
            &active,
            RunEventKind::QuestionAnswered {
                request_id: request_id.into(),
                question_id: question_id.into(),
                text: text.into(),
            },
        );
        if complete {
            self.close_question(&active, request_id, true);
        }
        true
    }

    fn run(&self, task_id: &str) -> Option<Arc<ActiveRun>> {
        self.inner.runs.lock().unwrap().get(task_id).cloned()
    }

    fn is_current(&self, active: &Arc<ActiveRun>) -> bool {
        self.inner
            .runs
            .lock()
            .unwrap()
            .get(&active.task_id)
            .is_some_and(|current| Arc::ptr_eq(current, active))
    }

    fn handle_provider_event(&self, active: &Arc<ActiveRun>, event: ProviderEvent) {
        if !self.is_current(active) || active.state.lock().unwrap().terminal {
            return;
        }
        let published = match event {
            ProviderEvent::Subagent(report) => {
                (self.inner.emit)(AgentEvent::Subagent(ThreadScoped {
                    task_id: active.task_id.clone(),
                    report,
                }));
                return;
            }
            ProviderEvent::Assistant {
                message_id,
                text,
                append,
                artifact,
            } => {
                if !artifact {
                    active.state.lock().unwrap().pending_tail = None;
                }
                RunEventKind::AssistantDelta {
                    message_id,
                    text,
                    append,
                    artifact,
                }
            }
            ProviderEvent::AssistantTail { message_id, text } => {
                self.queue_tail(active, message_id, text);
                return;
            }
            ProviderEvent::Usage {
                tokens,
                limit,
                model,
            } => RunEventKind::ContextUsage {
                tokens,
                limit,
                model,
            },
            ProviderEvent::Spend { tokens } => RunEventKind::ContextSpent { tokens },
            ProviderEvent::Retry(notice) => RunEventKind::Retrying(notice),
            ProviderEvent::CompactionStatus { compacting, error } => {
                RunEventKind::CompactionStatus { compacting, error }
            }
            ProviderEvent::Compaction {
                trigger,
                pre_tokens,
                post_tokens,
            } => RunEventKind::Compacted {
                trigger,
                pre_tokens,
                post_tokens,
            },
            ProviderEvent::Tool { intent } => RunEventKind::ToolIntent { intent },
            ProviderEvent::ComputerUseSetupRequired => RunEventKind::ComputerUseSetupRequired,
            ProviderEvent::Continuation(continuation) => {
                RunEventKind::ContinuationUpdated { continuation }
            }
            ProviderEvent::ContinuationLost => RunEventKind::ContinuationLost,
            ProviderEvent::Steered { message_id } => RunEventKind::QueuedDelivered { message_id },
            ProviderEvent::SteerFailed {
                message_id,
                message,
            } => RunEventKind::QueuedSteerFailed {
                message_id,
                message,
            },
        };
        self.publish(active, published);
    }

    /// Gives a turn the agent started itself somewhere to land: a run like any other.
    fn begin_agent_turn(&self, seed: &Arc<ActiveRun>) -> Option<AgentTurn> {
        let active = {
            let mut runs = self.inner.runs.lock().unwrap();
            if runs.contains_key(&seed.task_id) {
                return None;
            }
            let active = ActiveRun::new(
                seed.task_id.clone(),
                uuid::Uuid::new_v4().to_string(),
                seed.workspace_root.clone(),
                false,
            );
            runs.insert(active.task_id.clone(), active.clone());
            active
        };
        self.publish(
            &active,
            RunEventKind::RunStarted {
                agent_initiated: true,
            },
        );
        self.publish(
            &active,
            RunEventKind::RunStatus {
                status: RunStatus::Running,
                message: None,
                limit: None,
            },
        );
        let (emit_this, emit_run) = (self.clone(), active.clone());
        let (authorize_this, authorize_run) = (self.clone(), active.clone());
        let (end_this, end_run) = (self.clone(), active.clone());
        Some(AgentTurn {
            emit: Arc::new(move |event| emit_this.handle_provider_event(&emit_run, event)),
            authorize: Arc::new(move |intent| {
                let (this, active) = (authorize_this.clone(), authorize_run.clone());
                Box::pin(async move { this.authorize(&active, intent).await })
            }),
            steering: active.steering.clone(),
            end: Arc::new(move |result: ProviderResult| {
                end_this.finish(&end_run, result.status, result.message, result.limit)
            }),
        })
    }

    fn queue_tail(&self, active: &Arc<ActiveRun>, message_id: String, text: String) {
        {
            let mut state = active.state.lock().unwrap();
            state.pending_tail = Some((message_id, text));
            if state.tail_timer.is_some() {
                return;
            }
        }
        self.flush_tail(active);
        let this = self.clone();
        let run = active.clone();
        let interval = self.inner.options.tail_interval.unwrap_or(TAIL_INTERVAL);
        let timer = tokio::spawn(async move {
            tokio::time::sleep(interval).await;
            let pending = {
                let mut state = run.state.lock().unwrap();
                state.tail_timer = None;
                state.pending_tail.clone()
            };
            if let Some((message_id, text)) = pending {
                this.queue_tail(&run, message_id, text);
            }
        });
        active.state.lock().unwrap().tail_timer = Some(timer.abort_handle());
    }

    fn flush_tail(&self, active: &Arc<ActiveRun>) {
        let pending = {
            let mut state = active.state.lock().unwrap();
            if state.terminal {
                return;
            }
            state.pending_tail.take()
        };
        if let Some((message_id, text)) = pending {
            self.publish(active, RunEventKind::AssistantTail { message_id, text });
        }
    }

    async fn ask_question(
        &self,
        active: &Arc<ActiveRun>,
        request: QuestionRequest,
        cancel: CancellationToken,
    ) -> Option<QuestionAnswers> {
        if !self.is_current(active)
            || active.state.lock().unwrap().terminal
            || cancel.is_cancelled()
            || !request.is_valid()
        {
            return None;
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        let (close, answer) = oneshot::channel();
        {
            let mut state = active.state.lock().unwrap();
            let deadline = state.unattended.then(|| {
                let (this, run, id) = (self.clone(), active.clone(), request_id.clone());
                let wait = self
                    .inner
                    .options
                    .unattended_approval
                    .unwrap_or(UNATTENDED_APPROVAL);
                tokio::spawn(async move {
                    tokio::time::sleep(wait).await;
                    if run.state.lock().unwrap().unattended {
                        this.close_question(&run, &id, false);
                    }
                })
                .abort_handle()
            });
            state.questions.insert(
                request_id.clone(),
                PendingQuestions {
                    request: request.clone(),
                    answers: QuestionAnswers::new(),
                    close: Some(close),
                    deadline,
                },
            );
        }
        let (this, run, id) = (self.clone(), active.clone(), request_id.clone());
        let withdraw = tokio::spawn(async move {
            cancel.cancelled().await;
            this.close_question(&run, &id, false);
        });
        self.publish(
            active,
            RunEventKind::QuestionRequested {
                request_id,
                request,
            },
        );
        let answers = answer.await.ok().flatten();
        withdraw.abort();
        answers
    }

    /// Closes one pending question, answered with what it collected or with nothing.
    fn close_question(&self, active: &Arc<ActiveRun>, request_id: &str, answered: bool) {
        let pending = active.state.lock().unwrap().questions.remove(request_id);
        let Some(mut pending) = pending else { return };
        if let Some(deadline) = pending.deadline.take() {
            deadline.abort();
        }
        self.publish(
            active,
            RunEventKind::QuestionClosed {
                request_id: request_id.to_string(),
            },
        );
        if let Some(close) = pending.close.take() {
            let _ = close.send(answered.then_some(pending.answers));
        }
    }

    async fn authorize(&self, active: &Arc<ActiveRun>, intent: ToolIntent) -> ToolDecision {
        let closed = |this: &Self| {
            !this.is_current(active)
                || active.state.lock().unwrap().terminal
                || active.cancel.is_cancelled()
        };
        if closed(self) {
            return ToolDecision::Deny;
        }
        if let (Some(path), Some(inside)) =
            (&intent.write_path, &self.inner.options.is_write_path_inside)
        {
            if !inside(active.workspace_root.clone(), path.clone()).await || closed(self) {
                return ToolDecision::Deny;
            }
        }
        let approval_id = uuid::Uuid::new_v4().to_string();
        let (sender, decision) = oneshot::channel();
        let first = {
            let mut state = active.state.lock().unwrap();
            let deadline = state.unattended.then(|| {
                let (this, run, id) = (self.clone(), active.clone(), approval_id.clone());
                let wait = self
                    .inner
                    .options
                    .unattended_approval
                    .unwrap_or(UNATTENDED_APPROVAL);
                tokio::spawn(async move {
                    tokio::time::sleep(wait).await;
                    let settled = {
                        let mut state = run.state.lock().unwrap();
                        state.unattended
                            && Self::settle_approval(
                                &mut state,
                                &id,
                                ToolDecision::DenyWith(UNATTENDED_DENIAL.into()),
                            )
                    };
                    if settled {
                        this.show_approvals(&run);
                    }
                })
                .abort_handle()
            });
            state.approvals.push(PendingApproval {
                id: approval_id.clone(),
                intent,
                answer: Some(sender),
                deadline,
            });
            state.approvals.len() == 1
        };
        let (run, id) = (active.clone(), approval_id.clone());
        let deny_on_abort = tokio::spawn(async move {
            run.cancel.cancelled().await;
            Self::settle_approval(&mut run.state.lock().unwrap(), &id, ToolDecision::Deny);
        });
        if first {
            self.show_approvals(active);
        }
        let answer = decision.await.unwrap_or(ToolDecision::Deny);
        deny_on_abort.abort();
        answer
    }

    /// A run shows one approval at a time, oldest first, and stays waiting until none is left.
    fn show_approvals(&self, active: &Arc<ActiveRun>) {
        let next = {
            let state = active.state.lock().unwrap();
            if state.terminal {
                return;
            }
            state
                .approvals
                .first()
                .map(|pending| (pending.id.clone(), pending.intent.clone()))
        };
        if !self.is_current(active) {
            return;
        }
        match next {
            None => self.publish(
                active,
                RunEventKind::RunStatus {
                    status: RunStatus::Running,
                    message: None,
                    limit: None,
                },
            ),
            Some((approval_id, intent)) => {
                let label = match &self.inner.options.tool_label {
                    Some(label) => label(&intent.name),
                    None => default_tool_label(&intent.name),
                };
                self.publish(
                    active,
                    RunEventKind::ApprovalRequested {
                        approval_id,
                        intent,
                        title: format!("{label} needs approval"),
                        description: "Review this action before it runs.".into(),
                    },
                );
                self.publish(
                    active,
                    RunEventKind::RunStatus {
                        status: RunStatus::AwaitingApproval,
                        message: None,
                        limit: None,
                    },
                );
            }
        }
    }

    fn cancel_active(&self, active: &Arc<ActiveRun>) {
        if !active.state.lock().unwrap().terminal {
            self.finish(active, ResultStatus::Cancelled, None, None);
        }
        active.cancel.cancel();
    }

    fn finish(
        &self,
        active: &Arc<ActiveRun>,
        status: ResultStatus,
        message: Option<String>,
        limit: Option<UsageLimit>,
    ) {
        let questions: Vec<String> = {
            let mut state = active.state.lock().unwrap();
            if state.terminal {
                return;
            }
            state.terminal = true;
            if let Some(timer) = state.tail_timer.take() {
                timer.abort();
            }
            state.pending_tail = None;
            let ids: Vec<String> = state
                .approvals
                .iter()
                .map(|pending| pending.id.clone())
                .collect();
            for id in ids {
                Self::settle_approval(&mut state, &id, ToolDecision::Deny);
            }
            state.questions.keys().cloned().collect()
        };
        active.steering.close();
        for request_id in questions {
            self.close_question(active, &request_id, false);
        }
        if self.is_current(active) {
            let status = match status {
                ResultStatus::Succeeded => RunStatus::Succeeded,
                ResultStatus::Failed => RunStatus::Failed,
                ResultStatus::Cancelled => RunStatus::Cancelled,
            };
            let limit = if status == RunStatus::Failed {
                limit
            } else {
                None
            };
            self.publish(
                active,
                RunEventKind::RunStatus {
                    status,
                    message,
                    limit,
                },
            );
            let mut runs = self.inner.runs.lock().unwrap();
            if runs
                .get(&active.task_id)
                .is_some_and(|current| Arc::ptr_eq(current, active))
            {
                runs.remove(&active.task_id);
            }
        }
    }

    /// Answers one pending approval exactly once, and stops any deadline it was under.
    fn settle_approval(state: &mut RunState, approval_id: &str, decision: ToolDecision) -> bool {
        let Some(index) = state
            .approvals
            .iter()
            .position(|pending| pending.id == approval_id)
        else {
            return false;
        };
        let mut pending = state.approvals.remove(index);
        if let Some(deadline) = pending.deadline.take() {
            deadline.abort();
        }
        match pending.answer.take() {
            Some(answer) => {
                let _ = answer.send(decision);
                true
            }
            None => false,
        }
    }

    fn publish(&self, active: &Arc<ActiveRun>, event: RunEventKind) {
        if !self.is_current(active) {
            return;
        }
        let mut state = active.state.lock().unwrap();
        state.sequence += 1;
        let sequence = state.sequence;
        (self.inner.emit)(AgentEvent::Run(RunEvent {
            task_id: active.task_id.clone(),
            run_id: active.run_id.clone(),
            sequence,
            event,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Continuation;
    use futures::FutureExt;

    struct Scripted;

    impl AgentProvider for Scripted {
        fn execute(&self, input: RunInput) -> BoxFuture<'static, ProviderResult> {
            async move {
                input.emit(ProviderEvent::Continuation(Continuation::new(
                    AgentEngine::Claude,
                    "s1",
                )));
                let intent = ToolIntent {
                    tool_id: "t".into(),
                    name: "mcp__srv__do_it".into(),
                    input: serde_json::json!({}),
                    write_path: None,
                };
                let decision = (input.hooks.authorize)(intent).await;
                if decision.is_allow() {
                    ProviderResult::succeeded()
                } else {
                    ProviderResult::failed("denied")
                }
            }
            .boxed()
        }
        fn stop_process(&self, _: &str, _: &str) -> bool {
            false
        }
        fn label_thread(&self, _: &str, _: &str) -> bool {
            false
        }
        fn archive_thread(&self, _: &str, _: Option<Continuation>, _: bool) -> bool {
            false
        }
        fn retire_out_of_role(&self, _: &str, _: Option<CoordinationRole>) {}
        fn close_all(&self) {}
    }

    fn command() -> StartRun {
        StartRun {
            channel: RunChannel::Main,
            task_id: "t1".into(),
            title: String::new(),
            run_id: "r1".into(),
            prompt: "go".into(),
            workspace_root: "/tmp".into(),
            project_root: None,
            projectless: false,
            computer_use: ComputerUseRunConfig::SetupRequired,
            policy: ExecutionPolicy::Confirm,
            engine: AgentEngine::Claude,
            model: "opus".into(),
            effort: AgentEffort::High,
            fast_mode: None,
            operation: None,
            chrome_browser: false,
            continuation: None,
            fork_continuation: false,
            fresh_session: false,
            unattended: false,
            coordination_role: None,
            toolkit: None,
        }
    }

    #[test]
    fn thread_events_and_retries_keep_the_wire_shape() {
        let subagent = AgentEvent::Subagent(ThreadScoped {
            task_id: "t".into(),
            report: SubagentReport::Status {
                id: "a".into(),
                status: crate::types::SubagentLiveStatus::Idle,
                summary: None,
            },
        });
        assert_eq!(
            serde_json::to_value(&subagent).unwrap(),
            serde_json::json!({ "taskId": "t", "type": "subagent.status", "id": "a", "status": "idle" })
        );
        let retry = RunEvent {
            task_id: "t".into(),
            run_id: "r".into(),
            sequence: 3,
            event: RunEventKind::Retrying(RetryNotice {
                message: "busy".into(),
                attempt: Some(1),
                max_retries: Some(5),
            }),
        };
        assert_eq!(
            serde_json::to_value(&retry).unwrap(),
            serde_json::json!({ "taskId": "t", "runId": "r", "sequence": 3, "type": "run.retrying", "message": "busy", "attempt": 1, "maxRetries": 5 })
        );
    }

    #[tokio::test]
    async fn an_approval_waits_for_the_user_and_the_run_ends_once() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let coordinator = RunCoordinator::new(
            Arc::new(Scripted),
            Arc::new(move |event| sink.lock().unwrap().push(event)),
            CoordinatorOptions::default(),
        );
        coordinator.start(command());
        let approval_id = loop {
            tokio::task::yield_now().await;
            let found = events.lock().unwrap().iter().find_map(|event| match event {
                AgentEvent::Run(RunEvent {
                    event:
                        RunEventKind::ApprovalRequested {
                            approval_id, title, ..
                        },
                    ..
                }) => Some((approval_id.clone(), title.clone())),
                _ => None,
            });
            if let Some(found) = found {
                break found;
            }
        };
        assert_eq!(approval_id.1, "srv · do it needs approval");
        assert!(coordinator.decide_approval("t1", "r1", &approval_id.0, true));
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        let events = events.lock().unwrap();
        let statuses: Vec<RunStatus> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Run(RunEvent {
                    event: RunEventKind::RunStatus { status, .. },
                    ..
                }) => Some(*status),
                _ => None,
            })
            .collect();
        assert_eq!(
            statuses,
            [
                RunStatus::Running,
                RunStatus::AwaitingApproval,
                RunStatus::Running,
                RunStatus::Succeeded
            ]
        );
        let sequences: Vec<u64> = events
            .iter()
            .filter_map(|event| {
                if let AgentEvent::Run(run) = event {
                    Some(run.sequence)
                } else {
                    None
                }
            })
            .collect();
        assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1));
        let json = serde_json::to_value(&events[0]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "taskId": "t1", "runId": "r1", "sequence": 1, "type": "run.started" })
        );
    }
}
