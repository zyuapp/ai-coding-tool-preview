//! Checkouts of a project that any number of threads can work in.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub project_id: String,
    pub root: String,
    pub workspace_id: String,
    pub base_commit: String,
    pub created_at: i64,
    pub last_used_at: i64,
}

/// The checkout the desktop just made, without the project it belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorktree {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub root: String,
    pub workspace_id: String,
    pub base_commit: String,
    pub created_at: i64,
    pub last_used_at: i64,
}

impl CreatedWorktree {
    pub fn in_project(&self, project_id: &str) -> Worktree {
        Worktree {
            id: self.id.clone(),
            name: self.name.clone(),
            project_id: project_id.to_string(),
            root: self.root.clone(),
            workspace_id: self.workspace_id.clone(),
            base_commit: self.base_commit.clone(),
            created_at: self.created_at,
            last_used_at: self.last_used_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorktreeComparison {
    pub branch: String,
    pub ahead: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeStatus {
    pub changed_files: Option<i64>,
    pub comparison: Option<WorktreeComparison>,
}

/// One directory under a root the app owns, read from disk for manual management in Settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManagedWorktree {
    pub id: String,
    pub root: String,
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub status: WorktreeStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeRelease {
    ReturnedToLocal,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WorktreeDestination {
    Local,
    New,
    Worktree { id: String },
}

/// Where a thread that moved itself goes once its run ends, and what its next turn there starts with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MoveAfterRun {
    pub destination: WorktreeDestination,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<String>,
}

pub const SNAPSHOT_REF_NAMESPACE: &str = "refs/aicodingtool";

pub fn worktree_ref(worktree_id: &str) -> String {
    format!("{SNAPSHOT_REF_NAMESPACE}/{worktree_id}")
}

pub fn release_reason(release: WorktreeRelease) -> &'static str {
    match release {
        WorktreeRelease::ReturnedToLocal => "returned to local",
        WorktreeRelease::Deleted => "deleted manually",
    }
}

/// The snapshot a worktree is force-committed to before it lets go of its thread.
pub fn snapshot_message(title: &str, task_id: Option<&str>, release: WorktreeRelease, reference: Option<&str>) -> String {
    let who = match task_id {
        Some(id) => format!("Thread {id}"),
        None => "No thread".into(),
    };
    let mut lines = vec![format!("aicodingtool: snapshot \"{title}\""), String::new(), format!("{who} · {}", release_reason(release))];
    if let Some(reference) = reference {
        lines.push(format!("Ref {reference}"));
    }
    lines.join("\n")
}

const SLUG_LIMIT: usize = 24;

/// The directory a worktree gets: the project's name plus its id.
pub fn worktree_directory_name(project_root: &str, worktree_id: &str) -> String {
    let name = project_root.split('/').filter(|part| !part.is_empty()).next_back().unwrap_or("project");
    let mut slug = String::new();
    let mut dash = false;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            slug.push(ch);
            dash = false;
        } else if !dash {
            slug.push('-');
            dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(SLUG_LIMIT).collect();
    format!("{}-{worktree_id}", if slug.is_empty() { "project" } else { &slug })
}

/// The id inside a directory the app made.
pub fn worktree_id_from_directory_name(name: &str) -> String {
    if name.len() > 9 && name.is_char_boundary(name.len() - 9) {
        let tail = &name[name.len() - 9..];
        if tail.starts_with('-') && tail[1..].chars().all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch)) {
            return tail[1..].to_string();
        }
    }
    let mut out = String::new();
    let mut run = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            out.push(ch);
            run = false;
        } else if !run {
            out.push('-');
            run = true;
        }
    }
    out
}

/// A saved display name, with the directory name as the fallback for older records.
pub fn worktree_name(worktree: &Worktree) -> String {
    worktree.name.clone().unwrap_or_else(|| worktree.root.split('/').filter(|part| !part.is_empty()).next_back().map(String::from).unwrap_or_else(|| worktree.id.clone()))
}

/// How many colours the marks cycle through.
pub const WORKTREE_HUES: i32 = 6;

/// The colour a checkout's mark carries, taken from its id.
pub fn worktree_hue(worktree_id: &str) -> i32 {
    let mut hash: i32 = 0;
    for unit in worktree_id.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(unit as i32);
    }
    (hash.unsigned_abs() % WORKTREE_HUES as u32) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_names_round_trip_their_id() {
        let name = worktree_directory_name("/Users/me/My App", "1a2b3c4d");
        assert_eq!(name, "my-app-1a2b3c4d");
        assert_eq!(worktree_id_from_directory_name(&name), "1a2b3c4d");
        assert_eq!(worktree_id_from_directory_name("odd name"), "odd-name");
        assert_eq!(worktree_id_from_directory_name("projét-éé1a2b3c"), "proj-t--1a2b3c");
    }
}
