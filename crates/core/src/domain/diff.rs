//! Unified diff as data: Git's patch text parsed into the rows the review draws.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub use super::conversation::DiffSide;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

impl DiffFileStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DiffFileStatus::Added => "added",
            DiffFileStatus::Modified => "modified",
            DiffFileStatus::Deleted => "deleted",
            DiffFileStatus::Renamed => "renamed",
            DiffFileStatus::Untracked => "untracked",
        }
    }
}

/// One file in a comparison, before its patch is read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFileSummary {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    pub status: DiffFileStatus,
    pub additions: i64,
    pub deletions: i64,
    pub binary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffLineKind {
    Context,
    Add,
    Delete,
    Hunk,
}

/// One drawn line. A hunk header is a row of its own; line numbers are absent on the side the line is not on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffRow {
    pub kind: DiffLineKind,
    pub key: String,
    pub text: String,
    #[serde(default)]
    pub old_line: Option<i64>,
    #[serde(default)]
    pub new_line: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffHunk {
    pub header: String,
    pub old_start: i64,
    pub old_lines: i64,
    pub new_start: i64,
    pub new_lines: i64,
    pub rows: Vec<DiffRow>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    pub hunks: Vec<DiffHunk>,
}

fn parse_hunk_header(line: &str) -> Option<(i64, Option<i64>, i64, Option<i64>, String)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, tail) = rest.split_once(" @@")?;
    let range = |text: &str| -> Option<(i64, Option<i64>)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, Some(count.parse().ok()?))),
            None => Some((text.parse().ok()?, None)),
        }
    };
    let (old_start, old_count) = range(old)?;
    let (new_start, new_count) = range(new)?;
    Some((old_start, old_count, new_start, new_count, tail.to_string()))
}

/// The patch for one file, as `git diff` writes it.
pub fn parse_file_patch(patch: &str, fallback_path: &str) -> DiffFile {
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut path = fallback_path.to_string();
    let mut previous_path: Option<String> = None;
    let mut old_line = 0;
    let mut new_line = 0;
    for line in patch.split('\n') {
        if line.starts_with("--- ") && line != "--- /dev/null" {
            let named = line[4..].strip_prefix("a/").unwrap_or(&line[4..]).to_string();
            if named != path {
                previous_path = Some(named);
            }
            continue;
        }
        if line.starts_with("+++ ") && line != "+++ /dev/null" {
            path = line[4..].strip_prefix("b/").unwrap_or(&line[4..]).to_string();
            continue;
        }
        if let Some((old_start, old_count, new_start, new_count, header)) = parse_hunk_header(line) {
            old_line = old_start;
            new_line = new_start;
            hunks.push(DiffHunk { header: header.trim().to_string(), old_start, old_lines: old_count.unwrap_or(1), new_start, new_lines: new_count.unwrap_or(1), rows: Vec::new() });
            continue;
        }
        let hunk_index = hunks.len().wrapping_sub(1);
        let Some(current) = hunks.last_mut() else { continue };
        if line.is_empty() || line.starts_with('\\') {
            continue;
        }
        let mut chars = line.chars();
        let marker = chars.next().unwrap_or(' ');
        let text = chars.as_str().to_string();
        match marker {
            '+' => {
                current.rows.push(DiffRow { kind: DiffLineKind::Add, key: format!("{hunk_index}:n{new_line}"), text, old_line: None, new_line: Some(new_line) });
                new_line += 1;
            }
            '-' => {
                current.rows.push(DiffRow { kind: DiffLineKind::Delete, key: format!("{hunk_index}:o{old_line}"), text, old_line: Some(old_line), new_line: None });
                old_line += 1;
            }
            ' ' => {
                current.rows.push(DiffRow { kind: DiffLineKind::Context, key: format!("{hunk_index}:c{old_line}:{new_line}"), text, old_line: Some(old_line), new_line: Some(new_line) });
                old_line += 1;
                new_line += 1;
            }
            _ => {}
        }
    }
    let previous_path = previous_path.filter(|previous| *previous != path);
    DiffFile { path, previous_path, hunks }
}

fn hunk_label(hunk: &DiffHunk) -> String {
    let range = format!("@@ -{},{} +{},{} @@", hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines);
    if hunk.header.is_empty() {
        range
    } else {
        format!("{range} {}", hunk.header)
    }
}

/// The flat row list the panel draws, hunk headers included.
pub fn diff_rows(file: &DiffFile) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for (index, hunk) in file.hunks.iter().enumerate() {
        rows.push(DiffRow { kind: DiffLineKind::Hunk, key: format!("h{index}"), text: hunk_label(hunk), old_line: None, new_line: None });
        rows.extend(hunk.rows.iter().cloned());
    }
    rows
}

fn on_side(row: &DiffRow, side: DiffSide) -> bool {
    match side {
        DiffSide::Old => row.kind != DiffLineKind::Add,
        DiffSide::New => row.kind != DiffLineKind::Delete,
    }
}

/// One side of a hunk as the text it was cut from.
pub fn hunk_text(hunk: &DiffHunk, side: DiffSide) -> String {
    hunk.rows.iter().filter(|row| on_side(row, side)).map(|row| row.text.as_str()).collect::<Vec<_>>().join("\n")
}

/// Where each of a hunk's rows lands in `hunk_text`.
pub fn hunk_text_index(hunk: &DiffHunk, side: DiffSide) -> HashMap<String, usize> {
    hunk.rows.iter().filter(|row| on_side(row, side)).enumerate().map(|(line, row)| (row.key.clone(), line)).collect()
}

/// The grammar a path asks for, or None for one nothing here can read.
pub fn language_for_path(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let extension = name.rsplit('.').next().unwrap_or(name).to_lowercase();
    Some(match extension.as_str() {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "json" => "json",
        "css" => "css",
        "html" => "html",
        "md" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" => "shellscript",
        "py" => "python",
        "rs" => "rust",
        "go" => "go",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "sql" => "sql",
        _ => return None,
    })
}

pub fn line_on(row: &DiffRow, side: DiffSide) -> Option<i64> {
    if row.kind == DiffLineKind::Hunk {
        return None;
    }
    match side {
        DiffSide::Old => row.old_line,
        DiffSide::New => row.new_line,
    }
}

/// How a selected range reaches the agent: the file and lines it names, then the lines with markers.
pub fn comment_quote(path: &str, rows: &[DiffRow], side: DiffSide) -> String {
    let numbered: Vec<&DiffRow> = rows.iter().filter(|row| row.kind != DiffLineKind::Hunk).collect();
    let lines: Vec<i64> = numbered.iter().filter_map(|row| line_on(row, side)).collect();
    let range = match (lines.first(), lines.last()) {
        (Some(first), Some(last)) if first == last => format!(":L{first}"),
        (Some(first), Some(last)) => format!(":L{first}-L{last}"),
        _ => String::new(),
    };
    let mut out = vec![format!("{path}{range}")];
    for row in numbered {
        let marker = match row.kind {
            DiffLineKind::Add => "+",
            DiffLineKind::Delete => "-",
            _ => " ",
        };
        out.push(format!("{marker}{}", row.text));
    }
    out.join("\n")
}

/// A file's counts, used to notice that a file marked viewed has changed since it was marked.
pub fn file_fingerprint(file: &DiffFileSummary) -> String {
    format!("{}:{}:{}", file.status.as_str(), file.additions, file.deletions)
}

pub const DRAWN_LINE_BUDGET: i64 = 200_000;
pub const DRAWN_FILE_LIMIT: i64 = 20_000;

/// The files too large to draw.
pub fn folded_for_size(files: &[DiffFileSummary]) -> HashSet<String> {
    let mut folded = HashSet::new();
    let mut drawn = 0;
    for file in files {
        let lines = file.additions + file.deletions;
        if lines > DRAWN_FILE_LIMIT || drawn + lines > DRAWN_LINE_BUDGET {
            folded.insert(file.path.clone());
        } else {
            drawn += lines;
        }
    }
    folded
}

/// What the panel is comparing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DiffRange {
    Uncommitted,
    Commit { commit: String },
    Branches { base: String, compare: Option<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffMode {
    Uncommitted,
    Branch,
}

pub fn default_branch_range() -> DiffRange {
    DiffRange::Branches { base: "HEAD".into(), compare: None }
}

pub fn range_key(range: &DiffRange) -> String {
    match range {
        DiffRange::Commit { commit } => format!("commit:{commit}"),
        DiffRange::Uncommitted => "uncommitted".into(),
        DiffRange::Branches { base, compare } => format!("branches:{base}:{}", compare.as_deref().unwrap_or("")),
    }
}

pub fn is_commit_hash(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

pub fn is_diff_range(range: &DiffRange) -> bool {
    match range {
        DiffRange::Uncommitted => true,
        DiffRange::Commit { commit } => is_commit_hash(commit),
        DiffRange::Branches { base, .. } => !base.is_empty(),
    }
}

/// A line of the two-column view.
#[derive(Clone, Debug, PartialEq)]
pub enum SplitRow {
    Hunk { key: String, text: String },
    Pair { key: String, left: Option<DiffRow>, right: Option<DiffRow> },
}

/// The same rows in two columns, deletions beside the additions that replaced them.
pub fn split_rows(file: &DiffFile) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    for (index, hunk) in file.hunks.iter().enumerate() {
        rows.push(SplitRow::Hunk { key: format!("h{index}"), text: hunk_label(hunk) });
        let mut cursor = 0;
        while cursor < hunk.rows.len() {
            let source = &hunk.rows[cursor];
            if source.kind == DiffLineKind::Context {
                rows.push(SplitRow::Pair { key: source.key.clone(), left: Some(source.clone()), right: Some(source.clone()) });
                cursor += 1;
                continue;
            }
            let mut deletions = Vec::new();
            let mut additions = Vec::new();
            while cursor < hunk.rows.len() && hunk.rows[cursor].kind == DiffLineKind::Delete {
                deletions.push(hunk.rows[cursor].clone());
                cursor += 1;
            }
            while cursor < hunk.rows.len() && hunk.rows[cursor].kind == DiffLineKind::Add {
                additions.push(hunk.rows[cursor].clone());
                cursor += 1;
            }
            if deletions.is_empty() && additions.is_empty() {
                cursor += 1;
                continue;
            }
            for offset in 0..deletions.len().max(additions.len()) {
                let left = deletions.get(offset).cloned();
                let right = additions.get(offset).cloned();
                let key = format!("{}|{}", left.as_ref().map(|row| row.key.as_str()).unwrap_or(""), right.as_ref().map(|row| row.key.as_str()).unwrap_or(""));
                rows.push(SplitRow::Pair { key, left, right });
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/a.ts b/a.ts\n--- a/a.ts\n+++ b/a.ts\n@@ -1,2 +1,2 @@ fn\n-old\n+new\n same\n\\ No newline at end of file\n";

    #[test]
    fn parses_rows_with_line_numbers() {
        let file = parse_file_patch(PATCH, "a.ts");
        assert_eq!(file.hunks.len(), 1);
        let rows = &file.hunks[0].rows;
        assert_eq!(rows[0].key, "0:o1");
        assert_eq!(rows[1].key, "0:n1");
        assert_eq!(rows[2].key, "0:c2:2");
        assert_eq!(diff_rows(&file)[0].text, "@@ -1,2 +1,2 @@ fn");
    }

    #[test]
    fn split_pairs_a_rewrite_across() {
        let file = parse_file_patch(PATCH, "a.ts");
        let split = split_rows(&file);
        assert!(matches!(&split[1], SplitRow::Pair { key, .. } if key == "0:o1|0:n1"));
    }

    #[test]
    fn quotes_name_the_line_range() {
        let file = parse_file_patch(PATCH, "a.ts");
        assert_eq!(comment_quote("a.ts", &file.hunks[0].rows[1..], DiffSide::New), "a.ts:L1-L2\n+new\n same");
    }
}
