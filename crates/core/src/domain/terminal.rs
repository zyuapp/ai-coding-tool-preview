//! Shells the terminal panel holds open, and browser profiles whose sign-ins can be imported.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalStatus {
    Running,
    Exited,
}

/// One shell. The process and everything it printed live in the host; only the record is state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSession {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub task_id: Option<String>,
    pub status: TerminalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What the host reports about a session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalUpdate {
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TerminalStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const MAX_TERMINAL_INPUT: usize = 64 * 1024;
pub const MAX_TERMINAL_DIMENSION: i64 = 1_000;

pub fn is_terminal_dimension(value: i64) -> bool {
    (1..=MAX_TERMINAL_DIMENSION).contains(&value)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserImportSource {
    pub id: String,
    pub browser: String,
    pub profile: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserImportSite {
    pub domain: String,
    pub cookies: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserImportResult {
    pub imported: i64,
    pub skipped: i64,
}

/// The import the user last ran, which "Import again" repeats.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserImportMemory {
    pub source_id: String,
    pub sites: Vec<String>,
}

pub const MAX_IMPORT_SITES: usize = 500;

pub fn is_import_site(value: &str) -> bool {
    !value.is_empty() && value.len() <= 253 && value.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '-') && !value.starts_with('.')
}

pub fn is_import_source_id(value: &str) -> bool {
    !value.is_empty() && crate::util::js_len(value) <= 1024
}
