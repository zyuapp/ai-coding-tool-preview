//! What a run asks of an engine and what the engine reports back, whichever engine runs it.

use std::sync::Arc;

use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use crate::brief::RunToolkit;
use crate::steer::SteerChannel;
use crate::tools::ApprovalScope;
use crate::types::{
    AgentEffort, AgentEngine, BackgroundReport, CompactionTrigger, ComputerUseRunConfig,
    Continuation, CoordinationRole, ExecutionPolicy, GoalReport, QuestionAnswers, QuestionRequest,
    RetryNotice, RunChannel, RunOperation, SubagentReport, ToolIntent, UsageLimit, WorkflowReport,
};

/// What an engine reports while a turn runs.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    /// A complete block of assistant text, or a whole message when `append` is false. `artifact`
    /// marks text the engine produced as a file, such as a generated image.
    Assistant {
        message_id: String,
        text: String,
        append: bool,
        artifact: bool,
    },
    /// The buffered remainder that has not formed a complete block yet.
    AssistantTail {
        message_id: String,
        text: String,
    },
    /// `model` is the id the engine reported on the wire.
    Usage {
        tokens: u64,
        limit: u64,
        model: String,
    },
    /// Tokens a model response spent beyond what an earlier event already counted for it.
    Spend {
        tokens: u64,
    },
    CompactionStatus {
        compacting: bool,
        error: Option<String>,
    },
    Retry(RetryNotice),
    Compaction {
        trigger: CompactionTrigger,
        pre_tokens: u64,
        post_tokens: Option<u64>,
    },
    Tool {
        intent: ToolIntent,
    },
    ComputerUseSetupRequired,
    Continuation(Continuation),
    /// The continuation the run was given resumes nothing any more.
    ContinuationLost,
    Steered {
        message_id: String,
    },
    SteerFailed {
        message_id: String,
        message: String,
    },
    Subagent(SubagentReport),
}

impl ProviderEvent {
    /// The event in the app's `ProviderEvent` JSON shape.
    pub fn to_wire(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            ProviderEvent::Assistant {
                message_id,
                text,
                append,
                artifact,
            } => {
                let mut event =
                    json!({ "type": "assistant", "messageId": message_id, "text": text });
                if *append {
                    event["append"] = json!(true);
                }
                if *artifact {
                    event["artifact"] = json!(true);
                }
                event
            }
            ProviderEvent::AssistantTail { message_id, text } => {
                json!({ "type": "assistant-tail", "messageId": message_id, "text": text })
            }
            ProviderEvent::Usage {
                tokens,
                limit,
                model,
            } => json!({ "type": "usage", "tokens": tokens, "limit": limit, "model": model }),
            ProviderEvent::Spend { tokens } => json!({ "type": "spend", "tokens": tokens }),
            ProviderEvent::CompactionStatus { compacting, error } => {
                let mut event = json!({ "type": "compaction-status", "compacting": compacting });
                if let Some(error) = error {
                    event["error"] = json!(error);
                }
                event
            }
            ProviderEvent::Retry(notice) => {
                let mut event = serde_json::to_value(notice).unwrap_or_default();
                event["type"] = json!("retry");
                event
            }
            ProviderEvent::Compaction {
                trigger,
                pre_tokens,
                post_tokens,
            } => {
                let mut event =
                    json!({ "type": "compaction", "trigger": trigger, "preTokens": pre_tokens });
                if let Some(post) = post_tokens {
                    event["postTokens"] = json!(post);
                }
                event
            }
            ProviderEvent::Tool { intent } => json!({ "type": "tool", "intent": intent }),
            ProviderEvent::ComputerUseSetupRequired => {
                json!({ "type": "computer-use.setup-required" })
            }
            ProviderEvent::Continuation(continuation) => {
                json!({ "type": "continuation", "continuation": continuation })
            }
            ProviderEvent::ContinuationLost => json!({ "type": "continuation-lost" }),
            ProviderEvent::Steered { message_id } => {
                json!({ "type": "steered", "messageId": message_id })
            }
            ProviderEvent::SteerFailed {
                message_id,
                message,
            } => json!({ "type": "steer-failed", "messageId": message_id, "message": message }),
            ProviderEvent::Subagent(report) => serde_json::to_value(report).unwrap_or_default(),
        }
    }
}

impl ProviderResult {
    /// The result in the app's `ProviderResult` JSON shape.
    pub fn to_wire(&self) -> serde_json::Value {
        let mut result = serde_json::json!({ "status": match self.status {
            ResultStatus::Succeeded => "succeeded",
            ResultStatus::Failed => "failed",
            ResultStatus::Cancelled => "cancelled",
        } });
        if let Some(message) = &self.message {
            result["message"] = serde_json::json!(message);
        }
        if let Some(limit) = &self.limit {
            result["limit"] = serde_json::to_value(limit).unwrap_or_default();
        }
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderResult {
    pub status: ResultStatus,
    pub message: Option<String>,
    /// Set on a failure caused by the account reaching its plan's usage limit.
    pub limit: Option<UsageLimit>,
}

impl ProviderResult {
    pub fn succeeded() -> Self {
        ProviderResult {
            status: ResultStatus::Succeeded,
            message: None,
            limit: None,
        }
    }

    pub fn cancelled() -> Self {
        ProviderResult {
            status: ResultStatus::Cancelled,
            message: None,
            limit: None,
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        ProviderResult {
            status: ResultStatus::Failed,
            message: Some(message.into()),
            limit: None,
        }
    }

    pub fn failed_silently() -> Self {
        ProviderResult {
            status: ResultStatus::Failed,
            message: None,
            limit: None,
        }
    }

    pub fn with_limit(mut self, limit: Option<UsageLimit>) -> Self {
        self.limit = limit;
        self
    }
}

/// A plain denial reads as the user's; one that carries wording says why, for a run with no user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDecision {
    Allow,
    Deny,
    DenyWith(String),
}

impl ToolDecision {
    pub fn is_allow(&self) -> bool {
        matches!(self, ToolDecision::Allow)
    }

    /// What the agent is told of a denial, which every engine words the same.
    pub fn denial_reason(&self) -> String {
        match self {
            ToolDecision::DenyWith(reason) => reason.clone(),
            _ => "The user denied this action.".to_string(),
        }
    }
}

pub type Emit = Arc<dyn Fn(ProviderEvent) + Send + Sync>;
pub type Authorize = Arc<dyn Fn(ToolIntent) -> BoxFuture<'static, ToolDecision> + Send + Sync>;
/// Resolves `None` when the request expired; cancelling the token withdraws unanswered questions.
pub type AskQuestion = Arc<
    dyn Fn(QuestionRequest, CancellationToken) -> BoxFuture<'static, Option<QuestionAnswers>>
        + Send
        + Sync,
>;

/// A turn the agent started itself after the run that seeded the session had ended: a workflow
/// reporting what it produced, or a tool call from work that outlived its run.
#[derive(Clone)]
pub struct AgentTurn {
    pub emit: Emit,
    pub authorize: Authorize,
    pub steering: SteerChannel,
    pub end: Arc<dyn Fn(ProviderResult) + Send + Sync>,
}

/// Where a run reports, and how it asks. Workflows, background work, subagents and goals belong to
/// the thread and outlast the run, so they report apart from `emit`.
#[derive(Clone)]
pub struct RunHooks {
    pub emit: Emit,
    pub authorize: Authorize,
    pub ask_question: AskQuestion,
    pub report_workflow: Arc<dyn Fn(WorkflowReport) + Send + Sync>,
    pub report_background: Arc<dyn Fn(BackgroundReport) + Send + Sync>,
    pub report_subagent: Arc<dyn Fn(SubagentReport) + Send + Sync>,
    pub report_goal: Arc<dyn Fn(GoalReport) + Send + Sync>,
    /// Opens a run for a turn nobody asked for. None when the thread already has a run of its own.
    pub begin_agent_turn: Arc<dyn Fn() -> Option<AgentTurn> + Send + Sync>,
}

impl RunHooks {
    /// Hooks that drop every report and deny every request.
    pub fn silent() -> Self {
        RunHooks {
            emit: Arc::new(|_| {}),
            authorize: Arc::new(|_| Box::pin(async { ToolDecision::Deny })),
            ask_question: Arc::new(|_, _| Box::pin(async { None })),
            report_workflow: Arc::new(|_| {}),
            report_background: Arc::new(|_| {}),
            report_subagent: Arc::new(|_| {}),
            report_goal: Arc::new(|_| {}),
            begin_agent_turn: Arc::new(|| None),
        }
    }
}

/// Everything one run asks of an engine.
#[derive(Clone)]
pub struct RunInput {
    pub channel: RunChannel,
    /// Which thread is asking, which is what a warm session belongs to.
    pub task_id: String,
    pub title: String,
    pub prompt: String,
    pub workspace_root: String,
    /// Set for a member working in a checkout of its own: the project's checkout, the user's to change.
    pub project_root: Option<String>,
    pub projectless: bool,
    pub computer_use: ComputerUseRunConfig,
    pub policy: ExecutionPolicy,
    pub engine: AgentEngine,
    pub model: String,
    pub effort: AgentEffort,
    /// Read only by an engine with fast mode. None means standard speed.
    pub fast_mode: Option<bool>,
    pub operation: Option<RunOperation>,
    /// Read only by an engine that can drive the user's Chrome.
    pub chrome_browser: bool,
    pub continuation: Option<Continuation>,
    pub fork_continuation: bool,
    /// The run starts a session of its own even where the thread has a warm one.
    pub fresh_session: bool,
    pub coordination_role: Option<CoordinationRole>,
    /// The app's tools for this run and what their servers tell it. None offers no app tools.
    pub toolkit: Option<Arc<dyn RunToolkit>>,
    pub steering: SteerChannel,
    /// Cancelled when the user cancels the run.
    pub cancel: CancellationToken,
    pub hooks: RunHooks,
}

impl RunInput {
    /// The run's engine's own handle on the thread.
    pub fn continuation_value(&self) -> Option<&str> {
        crate::types::handle_for(self.continuation.as_ref(), self.engine)
    }

    pub fn scope(&self) -> ApprovalScope {
        ApprovalScope {
            policy: self.policy,
            channel: self.channel,
        }
    }

    pub fn emit(&self, event: ProviderEvent) {
        (self.hooks.emit)(event)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// An engine's provider: runs turns on the thread's warm session and reaches that session between runs.
pub trait AgentProvider: Send + Sync {
    fn execute(&self, input: RunInput) -> BoxFuture<'static, ProviderResult>;
    /// Kills one background process of the thread's session, whether or not a run is going.
    fn stop_process(&self, task_id: &str, process_id: &str) -> bool;
    /// Offers the thread's title to the engine's own record of it. Engines that keep none say so.
    fn label_thread(&self, task_id: &str, title: &str) -> bool;
    /// Files the thread away in the engine's own record, or brings it back.
    fn archive_thread(
        &self,
        task_id: &str,
        continuation: Option<Continuation>,
        archived: bool,
    ) -> bool;
    /// Lets the thread's session go once its turn has answered, when it was built for another role.
    fn retire_out_of_role(&self, task_id: &str, role: Option<CoordinationRole>);
    /// Lets every session go, which ends the processes they hold.
    fn close_all(&self);
}
