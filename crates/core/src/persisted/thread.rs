//! A thread as the `tasks.data` column holds it: `Thread` without its messages or history summary.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::enums::{
    AgentEffort, AgentEngine, ContinuationStatus, CoordinationState, Delivery, ExecutionPolicy, LimitWindow,
    PullRequestChecks, PullRequestReview, PullRequestState, ThreadOutcome, ThreadRole,
};
use super::js;
use super::message::{Annotation, AttachedFile, Extra, MessageOrigin, PastedText};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedTask {
    // ThreadCore
    pub id: String,
    pub title: String,
    /// Set once the user names the thread themselves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_by_user: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ThreadRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub execution_policy: ExecutionPolicy,
    pub engine: AgentEngine,
    /// Absent means the engine's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<AgentEffort>,
    /// Only on an engine with fast mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub sort_index: Option<f64>,
    /// Absent on threads saved before threads were ranked.
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub ranked_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub snoozed_until: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub created_at: Option<f64>,
    #[serde(serialize_with = "js::num")]
    pub updated_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub archived_at: Option<f64>,

    // CoordinationThread
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_lead: Option<HeldLead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<ThreadBrief>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<CoordinationReport>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub resolved_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decisions: Option<Vec<Decision>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination_notes: Option<Vec<CoordinationNote>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_model: Option<ThreadModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<CoordinatorMemory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposals: Option<Vec<ThreadProposal>>,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub muted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num_map")]
    pub remote_heard: Option<BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_working: Option<BTreeMap<String, HeardWork>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_stops: Option<BTreeMap<String, RemoteStop>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caught_starts: Option<BTreeMap<String, CaughtStart>>,

    // PullRequestThread
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PullRequestLink>,

    // FindingsThread
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<AutomationFinding>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handled_issues: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub last_finding_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<LastChecked>,

    // RunThread
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextUsage>,
    /// Tokens spent, by model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<BTreeMap<String, u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Continuation>,
    pub continuation_status: ContinuationStatus,
    pub last_change_snapshot: ChangeSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ThreadOutcome>,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub outcome_unread: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_pause: Option<LimitPause>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub run_ended_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_ended_as: Option<ThreadOutcome>,
    /// Set while a run is going; a thread saved with it was cut off when the app quit.
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub run_open: bool,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub inherited_continuation: bool,

    // WorktreeThread
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub worktree_entered_at: Option<f64>,

    #[serde(flatten)]
    pub extra: Extra,
}

/// What a coordinator hands a thread with its first message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadBrief {
    pub intent: String,
    pub done_when: String,
    pub delivers: Delivery,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Where a thread last said its work stands.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoordinationReport {
    pub state: CoordinationState,
    pub summary: String,
    #[serde(serialize_with = "js::num")]
    pub at: f64,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub recommended: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A choice only the user can make.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub options: Vec<DecisionOption>,
    #[serde(serialize_with = "js::num")]
    pub raised_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub answered_at: Option<f64>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Something a coordinator has yet to hear about one of its threads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinationNote {
    pub id: String,
    pub thread_id: String,
    pub text: String,
    #[serde(serialize_with = "js::num")]
    pub at: f64,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub urgent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub quiet: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

/// What a coordinator's threads run on unless one is started on something else. A thread from
/// another build may name an engine or effort this one does not know, so both stay strings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadModelChoice {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "js::opt_string")]
    pub engine: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "js::opt_string")]
    pub effort: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryNote {
    pub id: String,
    pub text: String,
    #[serde(serialize_with = "js::num")]
    pub at: f64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// What a coordinator keeps for the user: its purpose and notes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorMemory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose_cleared: Option<i64>,
    pub notes: Vec<MemoryNote>,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub heard: Option<f64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProposalComputer {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Where a suggested thread starts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalPlace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<ProposalComputer>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A thread a coordinator suggested, waiting for the user to start, allow or dismiss it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProposal {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub named: bool,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<ThreadBrief>,
    pub place: ProposalPlace,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "js::opt_string")]
    pub engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "js::opt_string")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "js::opt_string")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ThreadRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(serialize_with = "js::num")]
    pub proposed_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub carried: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The coordinator on another computer a thread works under, away or not.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldLead {
    pub thread_id: String,
    pub title: String,
    pub computer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub away: bool,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub back_at: Option<f64>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A thread elsewhere last heard working.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeardWork {
    pub computer: String,
    pub title: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStopAnswer {
    #[serde(serialize_with = "js::num")]
    pub at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// What Stop all stopped of a thread elsewhere.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStop {
    pub computer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<RemoteStopAnswer>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A start on a paired computer Stop all caught on its way.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaughtStart {
    pub computer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The pull request a thread's work was last seen on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PullRequestLink {
    pub number: u64,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PullRequestState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<PullRequestChecks>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PullRequestReview>,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub conflicts: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Something a run said it found, on purpose.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationFinding {
    pub id: String,
    pub headline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(serialize_with = "js::num")]
    pub at: f64,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub read: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

/// What the last silent scheduled run looked at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastChecked {
    #[serde(serialize_with = "js::num")]
    pub at: f64,
    pub note: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextUsage {
    #[serde(serialize_with = "js::num")]
    pub tokens: f64,
    #[serde(serialize_with = "js::num")]
    pub limit: f64,
    /// The id the engine reported on the wire.
    pub model: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// An engine's own handle on a thread, named by the engine that made it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Continuation {
    pub provider: String,
    pub value: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSnapshot {
    pub files: Vec<String>,
    #[serde(serialize_with = "js::num")]
    pub captured_at: f64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A message written to a paused thread, kept until the thread resumes with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldMessage {
    pub id: String,
    pub text: String,
    pub prompt: String,
    pub attachments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<Annotation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pastes: Option<Vec<PastedText>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<AttachedFile>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The decision this message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<String>,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub users_own: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A thread waiting out its account's usage limit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitPause {
    #[serde(serialize_with = "js::num")]
    pub resets_at: f64,
    pub window: LimitWindow,
    #[serde(serialize_with = "js::num")]
    pub paused_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub nudged_at: Option<f64>,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub workflow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<Vec<HeldMessage>>,
    #[serde(flatten)]
    pub extra: Extra,
}
