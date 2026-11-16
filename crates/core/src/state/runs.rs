//! Runs on their way, the messages waiting for them, and what each live run reports.

use serde::{Deserialize, Serialize};

use super::WorkspaceState;
use crate::domain::conversation::{Annotation, AttachedFile, PastedText};
use crate::domain::coordination::{CoordinationReport, RemoteLead, ThreadBrief};
use crate::domain::engine::{AgentEngine, AgentModel};
use crate::domain::message_origin::{is_users_own, MessageOrigin};
use crate::domain::question::{PendingQuestion, ReviewTarget};
use crate::domain::run::{AgentEffort, ExecutionPolicy, RetryNotice};
use crate::domain::thread::{Thread, ThreadOutcome};
use crate::domain::thread_role::ThreadRole;
use crate::domain::usage_limit::HeldMessage;
use crate::domain::workflow::Workflow;
use crate::util::Map;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunOrigin {
    Composer,
    Automation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunOperationKind {
    Compact,
    Review,
}

/// Where a live run stands. Only a retrying run carries the notice it retries under.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum RunPhase {
    Running,
    Compacting,
    AwaitingApproval,
    Retrying { retry: RetryNotice },
}

/// What a tick has to put back to have said nothing at all.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadMark {
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranked_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_ended_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_ended_as: Option<ThreadOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ThreadOutcome>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub outcome_unread: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<CoordinationReport>,
}

pub fn thread_mark(thread: Option<&Thread>) -> ThreadMark {
    match thread {
        None => ThreadMark::default(),
        Some(thread) => ThreadMark {
            updated_at: thread.updated_at,
            ranked_at: Some(thread.ranked_at),
            run_ended_at: thread.run_ended_at,
            run_ended_as: thread.run_ended_as,
            outcome: thread.outcome,
            outcome_unread: thread.outcome_unread,
            report: thread.report.clone(),
        },
    }
}

/// A run in flight on a thread.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRun {
    pub task_id: String,
    pub run_id: String,
    pub sequence: i64,
    #[serde(flatten)]
    pub phase: RunPhase,
    pub origin: RunOrigin,
    pub quiet: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<RunOperationKind>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub users_own: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<PendingQuestion>>,
    pub notified: bool,
    pub acknowledged: bool,
    pub reported_issues: Vec<String>,
    pub messages_before: usize,
    pub before: ThreadMark,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_parts: Option<Map<i64>>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub warming: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub answers_stop: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub agent_initiated: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub cancelling: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub dismissed: bool,
}

impl ActiveRun {
    pub fn status_name(&self) -> &'static str {
        match self.phase {
            RunPhase::Running => "running",
            RunPhase::Compacting => "compacting",
            RunPhase::AwaitingApproval => "awaiting-approval",
            RunPhase::Retrying { .. } => "retrying",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadRunStatus {
    Idle,
    Running,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalView {
    pub approval_id: String,
    pub task_id: String,
    pub run_id: String,
    pub title: String,
    pub description: String,
    pub tool_name: String,
    pub input: serde_json::Map<String, serde_json::Value>,
}

/// The unfinished end of a streaming message, kept out of the thread so nothing half-written persists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamingTail {
    pub message_id: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResumeAfter {
    Limit,
    Update,
    Memory,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PendingOperation {
    Compact,
    Review { target: ReviewTarget },
    Resume { after: ResumeAfter },
}

/// What a send that starts a thread carries for a coordinator.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinationSend {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<RemoteLead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<ThreadBrief>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Vec<String>>,
}

/// A run asked for that is still resolving its workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRun {
    pub id: String,
    pub run_id: String,
    pub origin: RunOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<PendingOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<AgentEngine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<AgentEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ThreadRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationSend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_text: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub prompt: String,
    pub attachments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<Annotation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pastes: Option<Vec<PastedText>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<AttachedFile>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_origin: Option<MessageOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ExecutionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub quiet: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub unattended: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub creating_worktree: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub warming: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub users_own: bool,
}

impl PendingRun {
    /// A pending run with only what every run has; the rest is filled in by whoever asked for it.
    pub fn new(id: impl Into<String>, run_id: impl Into<String>, origin: RunOrigin, text: impl Into<String>, prompt: impl Into<String>) -> PendingRun {
        PendingRun {
            id: id.into(),
            run_id: run_id.into(),
            origin,
            operation: None,
            task_id: None,
            project_id: None,
            worktree_id: None,
            model: None,
            engine: None,
            effort: None,
            role: None,
            coordination: None,
            draft_key: None,
            draft_text: None,
            text: text.into(),
            title: None,
            prompt: prompt.into(),
            attachments: Vec::new(),
            annotations: None,
            pastes: None,
            files: None,
            message_origin: None,
            answers: None,
            policy: None,
            automation_id: None,
            quiet: false,
            unattended: false,
            queued_ids: None,
            creating_worktree: false,
            warming: false,
            users_own: false,
        }
    }
}

/// Something typed while a run was going. It waits for the run to finish, unless steered into it first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessage {
    #[serde(flatten)]
    pub held: HeldMessage,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub steering: bool,
}

/// A thread whose session work Stop all stopped, and when.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuietSession {
    pub task_id: String,
    pub at: i64,
}

pub const QUEUED_NOT_YOURS: &str = "Only a message you wrote can be edited.";
pub const QUEUED_SENT: &str = "That message has already been sent.";

/// The queued message the user may take back into the composer to rewrite, or why not.
pub fn editable_queued<'a>(queued: &'a [QueuedMessage], message_id: &str) -> Result<&'a QueuedMessage, &'static str> {
    let message = queued.iter().find(|item| item.held.id == message_id).filter(|message| !message.steering).ok_or(QUEUED_SENT)?;
    if is_users_own(message.held.origin.as_ref(), message.held.detail.as_deref(), message.held.answers.as_deref()) {
        Ok(message)
    } else {
        Err(QUEUED_NOT_YOURS)
    }
}

/// Whether a queued message is already on its way into a run that is starting.
pub fn draining_queued(state: &WorkspaceState, task_id: &str, message_id: &str) -> bool {
    state.pending_runs.values().any(|pending| pending.task_id.as_deref() == Some(task_id) && pending.queued_ids.as_ref().is_some_and(|ids| ids.iter().any(|id| id == message_id)))
}

/// A queued message as a composer shows it: `starting` once a run is taking it.
#[derive(Clone, Debug, PartialEq)]
pub struct QueuedMessageView {
    pub message: QueuedMessage,
    pub starting: bool,
}

pub fn queued_view(state: &WorkspaceState, task_id: &str) -> Option<Vec<QueuedMessageView>> {
    let queued = state.queued_messages.get(task_id)?;
    Some(queued.iter().map(|message| QueuedMessageView { starting: draining_queued(state, task_id, &message.held.id), message: message.clone() }).collect())
}

/// A workflow by id, wherever it is kept.
pub fn workflow_by_id<'a>(state: &'a WorkspaceState, id: Option<&str>) -> Option<&'a Workflow> {
    let id = id?;
    state.workflows.values().flatten().find(|workflow| workflow.id == id)
}
