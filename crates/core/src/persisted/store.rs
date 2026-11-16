//! How the workspace and the store talk (`src/contracts/task-store.ts`).

use serde::{Deserialize, Serialize};

use super::js;
use super::message::ConversationMessage;
use super::project::{PersistedSubagent, Project, Subagent, SubagentActivity, Worktree};
use super::thread::PersistedTask;

pub const THREAD_STORE_VERSION: u8 = 2;

/// What a thread loaded without its conversation knows of it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySummary {
    pub message_count: u64,
    pub attachment_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub first_message_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub last_audible_at: Option<f64>,
}

/// A thread as the store hands it over: its record, its conversation (empty while
/// `history_summary` stands in for it), and the subagents it delegated to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredThread {
    #[serde(flatten)]
    pub task: PersistedTask,
    pub messages: Vec<ConversationMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_summary: Option<HistorySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<Vec<Subagent>>,
}

impl StoredThread {
    /// When the thread was made. Threads written before they were timestamped fall back to their
    /// first message, then their last update (`threadCreatedAt`).
    pub fn created_at(&self) -> f64 {
        self.task
            .created_at
            .or_else(|| self.history_summary.as_ref().and_then(|summary| summary.first_message_at))
            .or_else(|| self.messages.first().map(|message| message.at))
            .unwrap_or(self.task.updated_at)
    }

    /// When the thread last said anything a list should count (`threadLastOutputAt`). Withdrawn
    /// messages count for nothing.
    pub fn last_output_at(&self) -> f64 {
        let audible = match &self.history_summary {
            Some(summary) => summary.last_audible_at.unwrap_or(0.0),
            None => self.messages.iter().rev().find(|message| !message.withdrawn).map(|message| message.at).unwrap_or(0.0),
        };
        self.created_at().max(audible).max(self.task.run_ended_at.unwrap_or(0.0))
    }

    /// Where recency lists place the thread. One saved before threads were ranked ranks by when it
    /// last spoke (`rankedThread`).
    pub fn ranked_at(&self) -> f64 {
        self.task.ranked_at.unwrap_or_else(|| self.last_output_at())
    }
}

/// How long an archived thread stays recoverable.
pub const ARCHIVE_RETENTION_MS: f64 = 5.0 * 24.0 * 60.0 * 60.0 * 1000.0;

/// Whether a thread archived at `archived_at` has outlived its retention at `now`.
pub fn archive_expired(archived_at: Option<f64>, now: f64) -> bool {
    archived_at.is_some_and(|archived_at| now - archived_at >= ARCHIVE_RETENTION_MS)
}

/// The threads still within retention at `now` (`retainedThreads`); the next launch drops the rest.
pub fn retained_threads(threads: Vec<StoredThread>, now: f64) -> Vec<StoredThread> {
    threads.into_iter().filter(|thread| !archive_expired(thread.task.archived_at, now)).collect()
}

/// The stored workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStoreData {
    pub version: u8,
    /// Most recently updated first.
    pub tasks: Vec<StoredThread>,
    pub projects: Vec<Project>,
    pub worktrees: Vec<Worktree>,
    pub last_folder: Option<String>,
}

/// The stored workspace, the count of threads this build could not read and left on disk, and,
/// after a summary load, the threads whose conversations are still on disk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedTaskStore {
    #[serde(flatten)]
    pub data: ThreadStoreData,
    pub hidden_tasks: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unloaded_task_ids: Option<Vec<String>>,
    /// The threads the update that started this launch cut off mid-turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_after_update: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IndexedMessage {
    pub index: usize,
    pub message: ConversationMessage,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IndexedSubagent {
    pub index: usize,
    pub subagent: PersistedSubagent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedActivity {
    pub subagent_id: String,
    pub index: usize,
    pub item: SubagentActivity,
}

/// One thread's writes: its record, and the messages, subagents and activity that changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskChange {
    pub task: PersistedTask,
    pub messages: Vec<IndexedMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<IndexedSubagent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activity: Vec<IndexedActivity>,
}

/// The writes that bring the store up to the workspace.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStoreDelta {
    pub tasks: Vec<TaskChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_tasks: Vec<String>,
    /// The whole list, replacing the stored one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<Project>>,
    /// The whole list, as `projects` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees: Option<Vec<Worktree>>,
    /// `Some(None)` records that no folder was last opened; `None` leaves it alone.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub last_folder: Option<Option<String>>,
}

mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<Option<String>>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<String>>, D::Error> {
        Option::<String>::deserialize(deserializer).map(Some)
    }
}

