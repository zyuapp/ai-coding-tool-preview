//! What crosses the boundary around a shell: reads, snapshots, updates, and their limits.

use serde::{Deserialize, Serialize};

pub const MAX_TERMINAL_INPUT: usize = 64 * 1024;
pub const MAX_TERMINAL_DIMENSION: u16 = 1_000;
/// How many lines a read returns when the caller does not say, kept small so a read is cheap.
pub const DEFAULT_TERMINAL_LINES: usize = 100;
/// The most any read returns, however much the caller asks for.
pub const MAX_TERMINAL_LINES: usize = 2_000;
/// How much text a read returns at most, whatever the line count works out to.
pub const MAX_TERMINAL_BYTES: usize = 100_000;

pub fn is_terminal_dimension(value: u32) -> bool {
    (1..=MAX_TERMINAL_DIMENSION as u32).contains(&value)
}

pub fn terminal_line_limit(lines: Option<f64>) -> usize {
    match lines {
        None => DEFAULT_TERMINAL_LINES,
        Some(lines) if lines.is_nan() => 1,
        Some(lines) => (lines.floor().clamp(1., MAX_TERMINAL_LINES as f64)) as usize,
    }
}

/// The folder a session is named after when the shell says nothing.
pub fn terminal_title(cwd: &str) -> String {
    cwd.split('/').filter(|part| !part.is_empty()).next_back().unwrap_or(cwd).to_owned()
}

/// Trims a read to the byte ceiling from the end, since the newest lines are the ones worth having.
pub fn within_read_budget(lines: Vec<String>) -> Vec<String> {
    let mut bytes = 0;
    for index in (0..lines.len()).rev() {
        bytes += utf16_len(&lines[index]) + 1;
        if bytes > MAX_TERMINAL_BYTES {
            return lines[index + 1..].to_vec();
        }
    }
    lines
}

/// Lengths are counted the way the original measured strings, in UTF-16 units.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalReadOptions {
    pub lines: usize,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "match")]
    pub matching: Option<String>,
}

/// What a shell holds: its lines, with no escape sequences left in them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalText {
    pub lines: Vec<String>,
    /// How many lines the terminal holds that the limit left out.
    pub omitted: usize,
    /// Set when a filter was applied, counting the lines it kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalStatus {
    Running,
    Exited,
}

/// What the host reports about a session. Output never comes through here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalUpdate {
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TerminalStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TerminalUpdate {
    pub(crate) fn new(terminal_id: &str) -> Self {
        Self { terminal_id: terminal_id.to_owned(), title: None, status: None, exit_code: None, error: None }
    }
}

/// Resolved screen contents and the last live-output flush included in them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalScreenSnapshot {
    pub data: String,
    pub cols: u16,
    pub rows: u16,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalReadKind {
    Snapshot,
    Output,
}

/// A remote viewer first restores the screen, then reads only output after its watermark.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOutputRead {
    pub kind: TerminalReadKind,
    pub data: String,
    pub cols: u16,
    pub rows: u16,
    pub sequence: u64,
}

impl TerminalOutputRead {
    pub(crate) fn snapshot(snapshot: TerminalScreenSnapshot) -> Self {
        Self { kind: TerminalReadKind::Snapshot, data: snapshot.data, cols: snapshot.cols, rows: snapshot.rows, sequence: snapshot.sequence }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalError {
    InvalidInput,
    InvalidSize,
    InvalidSequence,
}

impl std::fmt::Display for TerminalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "Invalid terminal input.",
            Self::InvalidSize => "Invalid terminal size.",
            Self::InvalidSequence => "Invalid terminal sequence.",
        })
    }
}

impl std::error::Error for TerminalError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_budget_keeps_the_newest_lines() {
        let line = "x".repeat(40_000);
        let over = within_read_budget(vec!["first".into(), line.clone(), line.clone(), line.clone()]);
        assert_eq!(over, vec![line.clone(), line]);
    }
}
