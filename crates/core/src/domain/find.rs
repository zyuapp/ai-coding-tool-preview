//! Find, as data: matching runs over the messages themselves, and the view is told which to reveal.

use crate::shared::Shared;
use serde::{Deserialize, Serialize};

use super::conversation::ConversationMessage;
use super::diff::DiffFile;

/// Where a search is pointed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum FindTarget {
    Thread { task_id: Option<String> },
    Browser { tab_id: String },
    Terminal { terminal_id: String },
    Review { owner: String },
    Panel { owner: String, panel: String },
}

impl FindTarget {
    /// Every field flattened, so two are the same when they name the same view.
    pub fn key(&self) -> String {
        match self {
            FindTarget::Thread { task_id } => format!("thread:{}", task_id.as_deref().unwrap_or("")),
            FindTarget::Browser { tab_id } => format!("browser:{tab_id}"),
            FindTarget::Terminal { terminal_id } => format!("terminal:{terminal_id}"),
            FindTarget::Review { owner } => format!("review:{owner}"),
            FindTarget::Panel { owner, panel } => format!("panel:{owner}:{panel}"),
        }
    }

    /// Whether the view holds its own text and keeps its own place in what it found.
    pub fn searches_itself(&self) -> bool {
        matches!(self, FindTarget::Browser { .. } | FindTarget::Terminal { .. })
    }
}

pub fn same_find_target(one: &FindTarget, other: &FindTarget) -> bool {
    one.key() == other.key()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindField {
    Text,
    Detail,
}

/// Where a match is. `start` is a byte offset into the field; `occurrence` counts within the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindHit {
    pub message_id: String,
    pub field: FindField,
    pub start: usize,
    pub occurrence: usize,
}

/// What the find bar shows, and what a view that counts itself reports back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FindResults {
    pub matches: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counting: Option<bool>,
}

pub const MAX_FIND_HITS: usize = 500;

/// Byte offsets of every case-insensitive, non-overlapping match of `needle` (already lowered) in `haystack`.
pub fn match_offsets(haystack: &str, needle: &str, limit: usize) -> Vec<usize> {
    let mut found = Vec::new();
    if needle.is_empty() || limit == 0 {
        return found;
    }
    let lowered: Vec<(usize, char)> = haystack.char_indices().map(|(at, ch)| (at, ch.to_lowercase().next().unwrap_or(ch))).collect();
    let pattern: Vec<char> = needle.chars().collect();
    let mut index = 0;
    while index + pattern.len() <= lowered.len() {
        if lowered[index..index + pattern.len()].iter().map(|(_, ch)| *ch).eq(pattern.iter().copied()) {
            found.push(lowered[index].0);
            if found.len() == limit {
                break;
            }
            index += pattern.len();
        } else {
            index += 1;
        }
    }
    found
}

fn hits_in_message(message: &ConversationMessage, needle: &str, limit: usize) -> Vec<FindHit> {
    let mut hits = Vec::new();
    for (field, haystack) in [(FindField::Text, Some(message.text.as_str())), (FindField::Detail, message.detail.as_deref())] {
        let Some(haystack) = haystack.filter(|text| !text.is_empty()) else { continue };
        for start in match_offsets(haystack, needle, limit - hits.len()) {
            hits.push(FindHit { message_id: message.id.clone(), field, start, occurrence: hits.len() });
        }
        if hits.len() == limit {
            break;
        }
    }
    hits
}

/// Every match in a thread, oldest first, capped.
pub fn find_hits(messages: &[Shared<ConversationMessage>], query: &str) -> Vec<FindHit> {
    let needle = query.trim().to_lowercase();
    let mut hits = Vec::new();
    if needle.is_empty() {
        return hits;
    }
    for message in messages {
        let found = hits_in_message(message, &needle, MAX_FIND_HITS);
        let room = MAX_FIND_HITS - hits.len();
        hits.extend(found.into_iter().take(room));
        if hits.len() == MAX_FIND_HITS {
            break;
        }
    }
    hits
}

/// The match `delta` away, wrapping at both ends.
pub fn step_match(index: i64, delta: i64, matches: i64) -> i64 {
    if matches <= 0 {
        return 0;
    }
    ((index + delta) % matches + matches) % matches
}

/// Where a match is in a review. A None key is a match in the file's own name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewHit {
    pub path: String,
    pub key: Option<String>,
    pub occurrence: usize,
}

fn hits_in_text(path: &str, key: Option<&str>, text: &str, needle: &str, room: usize) -> Vec<ReviewHit> {
    match_offsets(text, needle, room).into_iter().enumerate().map(|(occurrence, _)| ReviewHit { path: path.into(), key: key.map(String::from), occurrence }).collect()
}

/// Every match in one file, in the order the review draws it: its name first, then its lines.
pub fn file_review_hits(path: &str, file: Option<&DiffFile>, needle: &str, limit: usize) -> Vec<ReviewHit> {
    if needle.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut hits = hits_in_text(path, None, path, needle, limit);
    let Some(file) = file else { return hits };
    for hunk in &file.hunks {
        for row in &hunk.rows {
            hits.extend(hits_in_text(path, Some(&row.key), &row.text, needle, limit - hits.len()));
            if hits.len() >= limit {
                return hits;
            }
        }
    }
    hits
}

/// One file as a search sees it.
pub struct ReviewFile<'a> {
    pub path: String,
    pub version: String,
    pub file: Option<&'a DiffFile>,
    pub coming: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ReviewCount {
    pub hits: Vec<ReviewHit>,
    pub counting: bool,
}

/// Every match in a review, in the order it is drawn. `scanned` caches each read file's hits by version.
pub fn review_hits(files: &[ReviewFile], needle: &str, scanned: &mut std::collections::HashMap<String, Vec<ReviewHit>>) -> ReviewCount {
    let mut count = ReviewCount::default();
    if needle.is_empty() {
        return count;
    }
    for entry in files {
        if count.hits.len() >= MAX_FIND_HITS {
            break;
        }
        if entry.file.is_none() && entry.coming {
            count.counting = true;
        }
        let key = format!("{}|{needle}", entry.version);
        let found = match entry.file.and_then(|_| scanned.get(&key).cloned()) {
            Some(found) => found,
            None => {
                let found = file_review_hits(&entry.path, entry.file, needle, MAX_FIND_HITS);
                if entry.file.is_some() {
                    scanned.insert(key, found.clone());
                }
                found
            }
        };
        let room = MAX_FIND_HITS - count.hits.len();
        count.hits.extend(found.into_iter().take(room));
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_case_insensitive_and_non_overlapping() {
        assert_eq!(match_offsets("Aaaa", "aa", 10), vec![0, 2]);
        assert_eq!(match_offsets("héllo HÉLLO", "héllo", 10), vec![0, 7]);
    }

    #[test]
    fn step_wraps_both_ways() {
        assert_eq!(step_match(0, -1, 3), 2);
        assert_eq!(step_match(2, 1, 3), 0);
        assert_eq!(step_match(5, 1, 0), 0);
    }
}
