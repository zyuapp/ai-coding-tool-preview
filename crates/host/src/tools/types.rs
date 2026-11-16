//! What the tools exchange with the window, in the JSON shapes the TypeScript contracts gave them, so
//! the integrator converts to and from `act_core` types by serde.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_ID_LENGTH: usize = 256;
pub const MAX_URL_LENGTH: usize = 8_192;
pub const MAX_SEARCH_TEXT: usize = 1_000;
pub const MAX_THREAD_WAIT_MS: u64 = 15 * 60 * 1_000;
pub const MAX_BROWSER_WAIT_MS: u64 = 2 * 60 * 1_000;
pub const MAX_PROMPT_LENGTH: usize = 1_000_000;
pub const MAX_HEADLINE: usize = 200;
pub const MAX_DETAIL: usize = 10_000;
pub const MAX_FINDING_KEY: usize = 200;
pub const MAX_BRIEF_FIELD: usize = 4_000;
pub const MAX_SUMMARY: usize = 500;
pub const MAX_QUESTION: usize = 300;
pub const MAX_DECISION_CONTEXT: usize = 2_000;
pub const MAX_OPTION_LABEL: usize = 120;
pub const MAX_OPTION_DESCRIPTION: usize = 300;
pub const MAX_OPTIONS: usize = 5;
pub const MAX_PURPOSE: usize = 200;
pub const MAX_MEMORY_NOTE: usize = 500;
pub const MAX_MEMORY_NOTE_ID: usize = 200;
pub const MAX_PROPOSALS: usize = 12;
pub const MAX_PROPOSAL_TITLE: usize = 120;
pub const MAX_PROPOSAL_PROJECT: usize = 4_096;
pub const MAX_PROPOSAL_COMPUTER: usize = 256;
pub const MAX_SIMULATOR_TEXT: usize = 2_000;
pub const MAX_SWIPE_MS: usize = 5_000;
pub const DEFAULT_TERMINAL_LINES: usize = 100;
pub const MAX_TERMINAL_LINES: usize = 2_000;

/// Which of the coordination roles a run plays, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoordinationRole {
    Coordinator,
    Member,
}

/// Which surface a run is read on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunChannel {
    Main,
    Side,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_for_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadComputer {
    pub id: String,
    pub name: String,
    pub offline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadStatus {
    Idle,
    Running,
    Stopped,
}

impl ThreadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadStatus::Idle => "idle",
            ThreadStatus::Running => "running",
            ThreadStatus::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<ThreadComputer>,
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<String>,
    pub status: ThreadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<i64>,
    pub archived: bool,
    pub created_at: i64,
    pub last_activity_at: i64,
    pub message_count: u64,
    pub attachment_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadMessage {
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadTranscript {
    pub thread: ThreadSummary,
    /// The newest messages, oldest first. `omitted` counts the older ones the limit left out.
    pub messages: Vec<ThreadMessage>,
    pub omitted: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadWaitResult {
    pub thread: ThreadSummary,
    pub timed_out: bool,
    pub reply: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCommandResult {
    pub thread: Option<ThreadSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// A command anything outside the window may dispatch, as the `AppCommand` JSON it is.
pub type ExternalCommand = Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingReport {
    pub headline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingResult {
    /// False when nothing was written.
    pub recorded: bool,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoordinationState {
    Working,
    Blocked,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub options: Vec<DecisionOption>,
}

/// A coordinator remembering, forgetting, or setting its purpose. Editing a note is the user's alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MemoryChange {
    Purpose { purpose: Option<String> },
    Remember { text: String },
    #[serde(rename_all = "camelCase")]
    Forget { note_id: String },
}

/// A thread a coordinator suggests rather than starts: what start_thread takes, with a title.
pub type ProposalRequest = Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTab {
    pub id: String,
    pub url: String,
    pub title: String,
    pub loading: bool,
    #[serde(default)]
    pub can_go_back: bool,
    #[serde(default)]
    pub can_go_forward: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserElement {
    #[serde(rename = "ref")]
    pub reference: String,
    pub role: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSnapshot {
    pub tab_id: String,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub text: String,
    pub elements: Vec<BrowserElement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserShot {
    pub tab_id: String,
    pub url: String,
    pub title: String,
    pub path: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserConsoleEntry {
    pub sequence: u64,
    pub at: i64,
    pub level: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserNetworkEntry {
    pub sequence: u64,
    pub started_at: i64,
    pub method: String,
    pub url: String,
    pub resource_type: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_cache: Option<bool>,
}

/// A read of the browser panel. A snapshot waits for the tab to settle first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum BrowserRead {
    Tabs,
    Snapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text_limit: Option<f64>,
        timeout_ms: u64,
    },
    Screenshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        full_page: Option<bool>,
        timeout_ms: u64,
    },
    Console {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum_level: Option<String>,
    },
    Network {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failures_only: Option<bool>,
    },
    Wait {
        condition: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<String>,
        timeout_ms: u64,
    },
}

/// A snapshot of the tab, or the navigation the user has yet to answer instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum BrowserReadResult {
    Tabs { tabs: Vec<BrowserTab> },
    Snapshot { snapshot: BrowserSnapshot },
    Shot { shot: BrowserShot },
    Console { tab_id: String, url: String, title: String, entries: Vec<BrowserConsoleEntry>, latest_sequence: u64, omitted: u64 },
    Network { tab_id: String, url: String, title: String, entries: Vec<BrowserNetworkEntry>, latest_sequence: u64, omitted: u64 },
    Wait { tab_id: String, url: String, title: String, condition: String, #[serde(default, skip_serializing_if = "Option::is_none")] value: Option<String>, matched: bool, elapsed_ms: u64 },
    AwaitingApproval { url: String },
    NoTab,
}

/// A write to the browser panel, as the `browser.*` command JSON minus its task.
pub type BrowserWrite = Value;
/// A write to the simulator panel, as the `simulator.*` command JSON minus its task.
pub type SimulatorWrite = Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulatorDevice {
    pub udid: String,
    pub name: String,
    pub runtime: String,
    pub state: String,
}

impl SimulatorDevice {
    pub fn label(&self) -> String {
        format!("{} · {}", self.name, self.runtime)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulatorElement {
    #[serde(rename = "ref")]
    pub reference: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    pub frame: [f64; 4],
    pub depth: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulatorSnapshot {
    pub udid: String,
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<String>,
    pub width: f64,
    pub height: f64,
    pub elements: Vec<SimulatorElement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulatorShot {
    pub path: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum SimulatorRead {
    /// Waits up to `timeout_ms` for the device to finish booting, then says where it stands.
    Status { timeout_ms: u64 },
    Snapshot { screenshot: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SimulatorReadResult {
    Status {
        device: SimulatorDevice,
        booted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Snapshot {
        snapshot: SimulatorSnapshot,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shot: Option<SimulatorShot>,
    },
    NoSimulator {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalStatus {
    Running,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSession {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub task_id: Option<String>,
    pub status: TerminalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TerminalSession {
    /// How a session reads in a list, for a caller that has no screen.
    pub fn describe(&self) -> String {
        let state = match self.status {
            TerminalStatus::Running => "running".to_string(),
            TerminalStatus::Exited => format!("exited{}", self.exit_code.map(|code| format!(" ({code})")).unwrap_or_default()),
        };
        format!("{} [{}] · {} · {state}", self.title, self.id, self.cwd)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    pub terminal_id: String,
    pub title: String,
    pub cwd: String,
    pub status: TerminalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub lines: Vec<String>,
    pub omitted: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<u64>,
}

/// A read of the terminal panel. There is no write: the panel holds the user's own shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum TerminalRead {
    Terminals,
    Snapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        r#match: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TerminalReadResult {
    Terminals { terminals: Vec<TerminalSession> },
    Snapshot { snapshot: TerminalSnapshot },
    NoTerminal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_serialize_as_the_window_reads_them() {
        let read = BrowserRead::Snapshot { tab_id: Some("t".into()), text_limit: Some(4000.0), timeout_ms: 20_000 };
        assert_eq!(serde_json::to_value(read).unwrap(), json!({ "op": "snapshot", "tabId": "t", "textLimit": 4000.0, "timeoutMs": 20000 }));
        let result: BrowserReadResult = serde_json::from_value(json!({ "kind": "awaiting-approval", "url": "https://x" })).unwrap();
        assert_eq!(result, BrowserReadResult::AwaitingApproval { url: "https://x".into() });
        let terminal = TerminalRead::Snapshot { terminal_id: None, lines: None, r#match: Some("err".into()) };
        assert_eq!(serde_json::to_value(terminal).unwrap(), json!({ "op": "snapshot", "match": "err" }));
        let change = MemoryChange::Forget { note_id: "n3".into() };
        assert_eq!(serde_json::to_value(change).unwrap(), json!({ "kind": "forget", "noteId": "n3" }));
    }
}
