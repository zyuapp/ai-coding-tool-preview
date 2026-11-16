//! Projects, checkouts, subagents and automations as their rows hold them.

use serde::{Deserialize, Serialize};

use super::enums::{ActivityKind, AutomationRunStatus, ExecutionPolicy, SubagentStatus};
use super::js;
use super::message::Extra;

/// A folder the user opened (`src/domain/project.ts`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub root: String,
    /// What the user chose to call the folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Sidebar position. Only the user moves it.
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub sort_index: Option<f64>,
    /// Opened only to start a thread; the sidebar leaves it out until a thread is started there.
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub unlisted: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A checkout of a project any number of threads can work in (`src/domain/worktree.ts`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub project_id: String,
    pub root: String,
    pub workspace_id: String,
    /// The commit the worktree was created detached at.
    pub base_commit: String,
    #[serde(serialize_with = "js::num")]
    pub created_at: f64,
    #[serde(serialize_with = "js::num")]
    pub last_used_at: f64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A delegated agent without its activity, as the `subagents` table holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedSubagent {
    pub id: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// A session-owned agent can continue after the parent run returns.
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub session_scoped: bool,
    pub status: SubagentStatus,
    /// Set from the moment a stop is asked for until the subagent stops working.
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub stopping: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub total_tokens: Option<f64>,
    #[serde(serialize_with = "js::num")]
    pub started_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub finished_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "js::opt_string")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "js::opt_string")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "js::opt_string")]
    pub prompt: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A subagent with its activity, as a loaded thread carries it. A summary load leaves `activity`
/// empty; it is read on demand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subagent {
    #[serde(flatten)]
    pub record: PersistedSubagent,
    pub activity: Vec<SubagentActivity>,
}

/// One line of a subagent's log, as the `subagent_activity` table holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentActivity {
    pub id: String,
    pub kind: ActivityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub text: String,
    #[serde(serialize_with = "js::num")]
    pub at: f64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A thread's recurring prompt (`src/domain/automation.ts`). One per thread.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub id: String,
    pub task_id: String,
    pub prompt: String,
    /// A five-field cron expression, or an ISO 8601 timestamp for a single run.
    pub schedule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ExecutionPolicy>,
    /// Present makes the schedule quiet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_when: Option<String>,
    /// Present makes the automation a watch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_when: Option<String>,
    pub paused: bool,
    #[serde(serialize_with = "js::num")]
    pub created_at: f64,
    #[serde(serialize_with = "js::num")]
    pub updated_at: f64,
    pub run_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub last_run_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<AutomationRunStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub last_status_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_declines: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrun_count: Option<u64>,
    #[serde(flatten)]
    pub extra: Extra,
}
