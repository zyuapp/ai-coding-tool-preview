//! The environment poll's answer: which files changed, and how many lines, measured from where HEAD
//! left the origin default branch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use super::git_with;
use crate::workspaces::{UnavailableReason, WorkspaceError, WorkspaceResolution, WorkspaceService};

const TIMEOUT: Duration = Duration::from_secs(5);
/// Long enough for the two-second environment poll to reuse, short enough to follow fetched refs.
const BASE_TTL: Duration = Duration::from_secs(30);
/// File reads are independent, but an untracked directory must not open everything at once.
const UNTRACKED_CONCURRENCY: usize = 8;
/// Bounds the memory. A count only saves work, so throwing the lot away costs one slower scan.
const UNTRACKED_CACHE_LIMIT: usize = 4_096;
const UNTRACKED_SIZE_LIMIT: u64 = 5_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ChangedFilesResult {
    /// `baseline` is the ref the counts are measured from, or None when only the working tree is counted.
    Available { files: Vec<String>, branch: Option<String>, baseline: Option<String>, additions: u64, deletions: u64 },
    Unavailable { reason: UnavailableReason },
    #[serde(rename_all = "camelCase")]
    Unknown { workspace_id: String },
    Error { message: String },
}

pub fn summarize_numstat(output: &str) -> (u64, u64) {
    let mut additions = 0;
    let mut deletions = 0;
    for record in output.split('\0') {
        let mut fields = record.split('\t');
        let (Some(added), Some(deleted)) = (fields.next(), fields.next()) else { continue };
        if let (Ok(added), Ok(deleted)) = (digits(added), digits(deleted)) {
            additions += added;
            deletions += deleted;
        }
    }
    (additions, deletions)
}

fn digits(text: &str) -> Result<u64, ()> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    text.parse().map_err(|_| ())
}

/// What one untracked file counted, against the size and modification time it counted.
#[derive(Clone, Copy)]
struct CountedFile {
    size: u64,
    modified: Option<SystemTime>,
    lines: u64,
}

fn untracked_counts() -> &'static Mutex<HashMap<PathBuf, CountedFile>> {
    static COUNTS: std::sync::OnceLock<Mutex<HashMap<PathBuf, CountedFile>>> = std::sync::OnceLock::new();
    COUNTS.get_or_init(Default::default)
}

pub fn count_lines(contents: &[u8]) -> u64 {
    if contents.contains(&0) {
        return 0;
    }
    let newlines = contents.iter().filter(|byte| **byte == b'\n').count() as u64;
    newlines + u64::from(!contents.is_empty() && contents.last() != Some(&b'\n'))
}

async fn untracked_file_lines(root: &Path, file: &str) -> u64 {
    if !crate::paths::stays_inside(root, file) {
        return 0;
    }
    let candidate = crate::paths::resolve_from(root, Path::new(file));
    let Ok(metadata) = tokio::fs::symlink_metadata(&candidate).await else { return 0 };
    if !metadata.is_file() || metadata.len() > UNTRACKED_SIZE_LIMIT {
        return 0;
    }
    let modified = metadata.modified().ok();
    if let Some(counted) = untracked_counts().lock().unwrap().get(&candidate) {
        if counted.size == metadata.len() && counted.modified == modified {
            return counted.lines;
        }
    }
    // The file may disappear while Git and the filesystem are being sampled.
    let Ok(contents) = tokio::fs::read(&candidate).await else { return 0 };
    let lines = count_lines(&contents);
    let mut counts = untracked_counts().lock().unwrap();
    if counts.len() >= UNTRACKED_CACHE_LIMIT {
        counts.clear();
    }
    counts.insert(candidate, CountedFile { size: metadata.len(), modified, lines });
    lines
}

async fn untracked_lines(root: &Path, files: &[String]) -> u64 {
    let mut total = 0;
    for chunk in files.chunks(UNTRACKED_CONCURRENCY) {
        let counts = futures::future::join_all(chunk.iter().map(|file| untracked_file_lines(root, file))).await;
        total += counts.into_iter().sum::<u64>();
    }
    total
}

async fn read_git(root: &Path, args: &[&str]) -> Option<String> {
    git_with(root, args, None, TIMEOUT).await.ok().map(|output| output.trim().to_string())
}

/// The commit at which HEAD left the origin default branch, so work a thread already committed keeps
/// counting. None when there is no origin to compare against. Nothing is fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ComparisonBase {
    reference: String,
    commit: String,
}

async fn resolve_comparison_base(root: &Path) -> Option<ComparisonBase> {
    let remote_head = read_git(root, &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]).await;
    let mut candidates: Vec<String> = Vec::new();
    for candidate in [remote_head.map(|head| head.strip_prefix("refs/remotes/").map(String::from).unwrap_or(head)), Some("origin/main".into()), Some("origin/master".into())].into_iter().flatten() {
        if !candidate.is_empty() && !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    for reference in candidates {
        if let Some(commit) = read_git(root, &["merge-base", &reference, "HEAD"]).await.filter(|commit| !commit.is_empty()) {
            return Some(ComparisonBase { reference, commit });
        }
    }
    None
}

struct CachedBase {
    head: String,
    at: Instant,
    base: Option<ComparisonBase>,
}

fn comparison_bases() -> &'static Mutex<HashMap<PathBuf, CachedBase>> {
    static BASES: std::sync::OnceLock<Mutex<HashMap<PathBuf, CachedBase>>> = std::sync::OnceLock::new();
    BASES.get_or_init(Default::default)
}

/// A poll asks for the same HEAD repeatedly, so one repository shares its settled read for a while.
async fn comparison_base(root: &Path, head: &str) -> Option<ComparisonBase> {
    let key = crate::paths::resolve(root);
    {
        let mut bases = comparison_bases().lock().unwrap();
        bases.retain(|_, cached| cached.at.elapsed() < BASE_TTL);
        if let Some(cached) = bases.get(&key).filter(|cached| cached.head == head) {
            return cached.base.clone();
        }
    }
    let base = resolve_comparison_base(root).await;
    comparison_bases().lock().unwrap().insert(key, CachedBase { head: head.to_string(), at: Instant::now(), base: base.clone() });
    base
}

async fn branch_name(root: &Path) -> Result<Option<String>, super::GitError> {
    if let Ok(branch) = git_with(root, &["symbolic-ref", "--quiet", "--short", "HEAD"], None, TIMEOUT).await {
        return Ok(Some(branch.trim().to_string()).filter(|branch| !branch.is_empty()));
    }
    let short = git_with(root, &["rev-parse", "--short", "HEAD"], None, TIMEOUT).await?;
    let short = short.trim();
    Ok((!short.is_empty()).then(|| format!("detached@{short}")))
}

pub async fn changed_files(workspace_id: &str, workspaces: &WorkspaceService) -> ChangedFilesResult {
    let root = match workspaces.resolve(workspace_id).await {
        Ok(WorkspaceResolution::Available { workspace }) => PathBuf::from(workspace.root),
        Ok(WorkspaceResolution::Unavailable { reason, .. }) => return ChangedFilesResult::Unavailable { reason },
        Err(WorkspaceError::Unknown(_)) => return ChangedFilesResult::Unknown { workspace_id: workspace_id.to_string() },
        Err(error) => return ChangedFilesResult::Error { message: error.to_string() },
    };
    match read_changes(&root).await {
        Ok(result) => result,
        Err(error) => ChangedFilesResult::Error { message: error.message },
    }
}

async fn read_changes(root: &Path) -> Result<ChangedFilesResult, super::GitError> {
    let (head, branch) = tokio::join!(git_with(root, &["rev-parse", "--verify", "HEAD"], None, TIMEOUT), branch_name(root));
    let head = head?.trim().to_string();
    let branch = branch?;
    let base = comparison_base(root, &head).await;
    let compared = base.as_ref().map(|base| base.commit.as_str()).unwrap_or("HEAD");
    let numstat_args = ["diff", "--numstat", "-z", compared, "--"];
    let (status, numstat) = tokio::join!(
        git_with(root, &["--no-optional-locks", "status", "--porcelain=v1", "-z"], None, TIMEOUT),
        git_with(root, &numstat_args, None, TIMEOUT),
    );
    let records = super::split_nul(&status?);
    let untracked: Vec<String> = records.iter().filter_map(|record| record.strip_prefix("?? ").map(String::from)).collect();
    let (mut additions, deletions) = summarize_numstat(&numstat?);
    additions += untracked_lines(root, &untracked).await;
    Ok(ChangedFilesResult::Available { files: records, branch, baseline: base.map(|base| base.reference), additions, deletions })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_repo;

    #[test]
    fn numstat_skips_binary_records() {
        assert_eq!(summarize_numstat("3\t1\ta.txt\0-\t-\tb.png\0"), (3, 1));
    }

    #[test]
    fn counts_a_last_line_without_a_newline() {
        assert_eq!(count_lines(b"a\nb"), 2);
        assert_eq!(count_lines(b"a\n"), 1);
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"a\0b"), 0);
    }

    #[tokio::test]
    async fn counts_untracked_lines_as_additions() {
        let dir = tempfile::tempdir().unwrap();
        test_repo::init(dir.path());
        std::fs::write(dir.path().join("file.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "a\nb\nc").unwrap();
        let data = tempfile::tempdir().unwrap();
        let workspaces = WorkspaceService::open(data.path().join("w.json"), data.path().join("p")).await.unwrap();
        let record = workspaces.register_project(dir.path()).await.unwrap();
        let ChangedFilesResult::Available { files, branch, baseline, additions, deletions } = changed_files(&record.id, &workspaces).await else { panic!() };
        assert_eq!(files.len(), 2);
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(baseline, None);
        assert_eq!((additions, deletions), (3, 1));
    }
}
