//! What Settings shows for a worktree: how many files changed, and how far ahead of the default branch it is.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::git_with;

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeStatus {
    /// None when Git could not read the working tree.
    pub changed_files: Option<u64>,
    /// Compared with a locally available default-branch ref; nothing is fetched.
    pub comparison: Option<Comparison>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    pub branch: String,
    pub ahead: u64,
}

/// A rename occupies two NUL-delimited records but represents one changed file.
pub fn count_changed_files(output: &str) -> u64 {
    let records: Vec<&str> = output.split('\0').collect();
    let mut count = 0;
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if !record.is_empty() {
            count += 1;
            let code: String = record.chars().take(2).collect();
            if code.contains('R') || code.contains('C') {
                index += 1;
            }
        }
        index += 1;
    }
    count
}

async fn comparison(root: &Path) -> Option<Comparison> {
    let remote_head = git_with(root, &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"], None, TIMEOUT).await.ok();
    let mut candidates: Vec<String> = Vec::new();
    for candidate in [remote_head.map(|head| head.trim().to_string()), Some("refs/remotes/origin/main".into()), Some("refs/remotes/origin/master".into()), Some("refs/heads/main".into()), Some("refs/heads/master".into())]
        .into_iter()
        .flatten()
    {
        if !candidate.is_empty() && !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    for reference in candidates {
        let verify = format!("{reference}^{{commit}}");
        let Ok(commit) = git_with(root, &["rev-parse", "--verify", &verify], None, TIMEOUT).await else { continue };
        let output = git_with(root, &["rev-list", "--count", "HEAD", "--not", commit.trim(), "--"], None, TIMEOUT).await.ok()?;
        let ahead = output.trim();
        if ahead.is_empty() || !ahead.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let branch = reference.strip_prefix("refs/heads/").or_else(|| reference.strip_prefix("refs/remotes/")).unwrap_or(&reference).to_string();
        return Some(Comparison { branch, ahead: ahead.parse().ok()? });
    }
    None
}

pub async fn read_worktree_status(root: &Path) -> WorktreeStatus {
    let (changed_files, comparison) = tokio::join!(
        async {
            git_with(root, &["--no-optional-locks", "status", "--porcelain=v1", "-z", "--untracked-files=all"], None, TIMEOUT)
                .await
                .ok()
                .map(|output| count_changed_files(&output))
        },
        comparison(root),
    );
    WorktreeStatus { changed_files, comparison }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_repo;

    #[test]
    fn a_rename_counts_once() {
        assert_eq!(count_changed_files(" M a.txt\0R  new.txt\0old.txt\0?? c.txt\0"), 3);
        assert_eq!(count_changed_files(""), 0);
    }

    #[tokio::test]
    async fn compares_against_the_local_default_branch() {
        let dir = tempfile::tempdir().unwrap();
        test_repo::init(dir.path());
        test_repo::run(dir.path(), &["checkout", "-q", "-b", "work"]);
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        test_repo::run(dir.path(), &["add", "."]);
        test_repo::run(dir.path(), &["commit", "-q", "-m", "a"]);
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        let status = read_worktree_status(dir.path()).await;
        assert_eq!(status.changed_files, Some(1));
        assert_eq!(status.comparison, Some(Comparison { branch: "main".into(), ahead: 1 }));
    }
}
