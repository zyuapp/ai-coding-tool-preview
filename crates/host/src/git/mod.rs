//! Every git invocation in the app goes through here: no shell, one error shape, a bounded timeout.

pub mod changes;
pub mod diff;
pub mod github;
pub mod status;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::process::{Exec, RunError};

pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Checkout-scale work — populating or removing a worktree, committing a whole tree — takes as long as the tree is big.
pub const LONG_TIMEOUT: Duration = Duration::from_secs(600);
/// A worktree checkout can carry a large binary patch, so the pipe has to be bigger than the default.
const MAX_BUFFER: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct GitError {
    pub args: Vec<String>,
    pub message: String,
    pub exit_code: Option<i32>,
}

impl GitError {
    fn new(args: &[&str], message: impl Into<String>, exit_code: Option<i32>) -> Self {
        Self { args: args.iter().map(|arg| arg.to_string()).collect(), message: message.into(), exit_code }
    }
}

pub async fn git(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    git_with(cwd, args, None, TIMEOUT).await
}

pub async fn git_with(cwd: &Path, args: &[&str], input: Option<&[u8]>, timeout: Duration) -> Result<String, GitError> {
    match Exec::new("git").args(args.iter().copied()).cwd(cwd).input(input).timeout(timeout).max_buffer(MAX_BUFFER).run().await {
        Ok(output) => Ok(output.stdout_text()),
        Err(RunError::NotFound { .. }) => Err(GitError::new(args, "Git is not installed or is not on the PATH.", None)),
        Err(error) => {
            let stderr = String::from_utf8_lossy(error.stderr()).trim().to_string();
            let message = if !stderr.is_empty() {
                stderr
            } else if let RunError::Exit { .. } = error {
                format!("Command failed: git {}", args.join(" "))
            } else {
                error.to_string()
            };
            Err(GitError::new(args, message, error.exit_code()))
        }
    }
}

async fn try_git(cwd: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    git_with(cwd, args, None, timeout).await.ok()
}

/// Refuses a caller-supplied name git would read as an option instead of a ref.
fn ref_argument(name: &str) -> Result<&str, GitError> {
    if name.starts_with('-') {
        return Err(GitError::new(&[name], format!("Invalid ref name: {name}"), None));
    }
    Ok(name)
}

pub async fn head_commit(root: &Path, reference: Option<&str>) -> Result<String, GitError> {
    let reference = ref_argument(reference.unwrap_or("HEAD"))?;
    Ok(git(root, &["rev-parse", reference]).await?.trim().to_string())
}

pub async fn short_commit(root: &Path, commit: &str) -> Result<String, GitError> {
    Ok(git(root, &["rev-parse", "--short", commit]).await?.trim().to_string())
}

/// True while the checkout has no branch, which is how a worktree starts and often stays.
pub async fn is_detached(root: &Path) -> bool {
    try_git(root, &["symbolic-ref", "--quiet", "HEAD"], TIMEOUT).await.is_none()
}

/// The branch this checkout holds, or None while it is detached.
pub async fn current_branch(root: &Path) -> Option<String> {
    non_empty(try_git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"], TIMEOUT).await)
}

/// Where this checkout pushes, or None when it has no origin. A worktree reports its repository's.
pub async fn origin_url(root: &Path) -> Option<String> {
    non_empty(try_git(root, &["config", "--get", "remote.origin.url"], TIMEOUT).await)
}

fn non_empty(output: Option<String>) -> Option<String> {
    output.map(|text| text.trim().to_string()).filter(|text| !text.is_empty())
}

pub async fn is_dirty(root: &Path) -> Result<bool, GitError> {
    Ok(!git(root, &["status", "--porcelain", "-z"]).await?.is_empty())
}

pub async fn repository_root(root: &Path) -> Result<PathBuf, GitError> {
    Ok(PathBuf::from(git(root, &["rev-parse", "--show-toplevel"]).await?.trim()))
}

pub async fn add_worktree(repository: &Path, worktree: &Path, commit: &str) -> Result<(), GitError> {
    let worktree = worktree.to_string_lossy();
    git_with(repository, &["worktree", "add", "--detach", &worktree, commit], None, LONG_TIMEOUT).await?;
    Ok(())
}

pub async fn remove_worktree(repository: &Path, worktree: &Path) {
    let worktree = worktree.to_string_lossy();
    try_git(repository, &["worktree", "remove", "--force", &worktree], LONG_TIMEOUT).await;
    try_git(repository, &["worktree", "prune"], TIMEOUT).await;
}

/// Local branches can be moved onto; remote ones are only ever compared against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branches {
    pub branches: Vec<String>,
    pub remotes: Vec<String>,
    /// None while the checkout is detached.
    pub current: Option<String>,
}

/// What the branch picker reads, failure included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum BranchesResult {
    Available {
        branches: Vec<String>,
        remotes: Vec<String>,
        current: Option<String>,
    },
    Error {
        message: String,
    },
}

impl From<Result<Branches, GitError>> for BranchesResult {
    fn from(result: Result<Branches, GitError>) -> Self {
        match result {
            Ok(Branches { branches, remotes, current }) => BranchesResult::Available { branches, remotes, current },
            Err(error) => BranchesResult::Error { message: error.message },
        }
    }
}

/// Branches newest first, with the one the checkout is on.
pub async fn list_branches(root: &Path) -> Result<Branches, GitError> {
    let (local, remote, current) = tokio::join!(
        git(root, &["for-each-ref", "--sort=-committerdate", "--format=%(refname:short)", "refs/heads"]),
        git(root, &["for-each-ref", "--sort=-committerdate", "--format=%(refname:short)", "refs/remotes"]),
        try_git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"], TIMEOUT),
    );
    let names = |output: String| output.lines().map(str::trim).filter(|line| !line.is_empty()).map(String::from).collect::<Vec<_>>();
    Ok(Branches {
        branches: names(local?),
        // `origin/HEAD` is a pointer at another of these rather than a branch of its own.
        remotes: names(remote?).into_iter().filter(|name| !name.ends_with("/HEAD")).collect(),
        current: non_empty(current),
    })
}

/// Which of these hashes name a commit the checkout's repository holds, read in one go.
pub async fn known_commits(root: &Path, hashes: &[String]) -> Result<Vec<String>, GitError> {
    let input: String = hashes.iter().map(|hash| format!("{hash}^{{commit}}\n")).collect();
    let output = git_with(root, &["cat-file", "--batch-check=%(objecttype)"], Some(input.as_bytes()), TIMEOUT).await?;
    let answers: Vec<&str> = output.split('\n').collect();
    Ok(hashes.iter().enumerate().filter(|(index, _)| answers.get(*index) == Some(&"commit")).map(|(_, hash)| hash.clone()).collect())
}

/// Makes `branch` at the checkout's HEAD. Git refuses a name the repository already has.
pub async fn create_branch(root: &Path, branch: &str) -> Result<(), GitError> {
    git(root, &["branch", ref_argument(branch)?]).await?;
    Ok(())
}

/// Moves the checkout onto `branch`. Never forced: uncommitted work that would be overwritten stops it.
pub async fn checkout_branch(root: &Path, branch: &str) -> Result<(), GitError> {
    git(root, &["checkout", ref_argument(branch)?]).await?;
    Ok(())
}

pub async fn update_ref(root: &Path, reference: &str, commit: &str) -> Result<(), GitError> {
    git(root, &["update-ref", reference, commit]).await?;
    Ok(())
}

/// Commits everything in the checkout, hooks skipped, so a hook can never throw a released
/// worktree's work away. None when there was nothing to commit.
pub async fn snapshot_commit(root: &Path, message: &str) -> Result<Option<String>, GitError> {
    if !is_dirty(root).await? {
        return Ok(None);
    }
    git_with(root, &["add", "-A"], None, LONG_TIMEOUT).await?;
    // A machine with no git identity still gets its snapshot; the app signs it instead.
    let identity: &[&str] = if try_git(root, &["var", "GIT_AUTHOR_IDENT"], TIMEOUT).await.is_some() {
        &[]
    } else {
        &["-c", concat!("user.name=", act_core::app_name!()), "-c", "user.email=aic@localhost"]
    };
    let mut args: Vec<&str> = identity.to_vec();
    args.extend(["commit", "--no-verify", "-m", message]);
    git_with(root, &args, None, LONG_TIMEOUT).await?;
    head_commit(root, None).await.map(Some)
}

/// The `HEAD` no branch, remote or tag holds, if removing this checkout would orphan it.
pub async fn unreachable_head(root: &Path) -> Result<Option<String>, GitError> {
    let output = git(root, &["rev-list", "-n", "1", "HEAD", "--not", "--branches", "--remotes", "--tags"]).await?;
    Ok(Some(output.trim().to_string()).filter(|head| !head.is_empty()))
}

/// Ignored files present in the checkout. `--directory` collapses `node_modules/` into one entry.
pub async fn ignored_paths(root: &Path) -> Result<Vec<String>, GitError> {
    let output = git(root, &["ls-files", "--others", "--ignored", "--exclude-standard", "--directory", "-z"]).await?;
    Ok(split_nul(&output))
}

pub async fn untracked_paths(root: &Path) -> Result<Vec<String>, GitError> {
    let output = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).await?;
    Ok(split_nul(&output))
}

pub(crate) fn split_nul(output: &str) -> Vec<String> {
    output.split('\0').filter(|part| !part.is_empty()).map(String::from).collect()
}

/// Which of `candidates` match the patterns in `pattern_file`, decided by Git's own ignore matcher
/// in an empty scratch repository, so only those patterns apply.
pub async fn match_ignore_patterns(pattern_file: &Path, candidates: &[String]) -> Result<Vec<String>, GitError> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let scratch = std::env::temp_dir().join(format!("aic-match-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&scratch).map_err(|error| GitError::new(&["init"], error.to_string(), None))?;
    let result = async {
        git(&scratch, &["init", "-q", "."]).await?;
        let excludes = format!("core.excludesFile={}", pattern_file.to_string_lossy());
        let mut input = candidates.join("\0");
        input.push('\0');
        match git_with(&scratch, &["-c", &excludes, "check-ignore", "--no-index", "--stdin", "-z"], Some(input.as_bytes()), TIMEOUT).await {
            Ok(output) => Ok(split_nul(&output)),
            // `check-ignore` exits 1 when no candidate matches, which is an answer rather than a failure.
            Err(error) if error.exit_code == Some(1) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    result
}

/// The uncommitted work in a checkout, as a patch that applies onto the same commit elsewhere.
pub async fn tracked_diff(root: &Path) -> Result<String, GitError> {
    git_with(root, &["diff", "--binary", "HEAD", "--"], None, LONG_TIMEOUT).await
}

pub async fn apply_patch(root: &Path, patch: &str) -> Result<(), GitError> {
    git_with(root, &["apply", "--whitespace=nowarn", "-"], Some(patch.as_bytes()), LONG_TIMEOUT).await?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_repo {
    use std::path::Path;
    use std::process::Command;

    pub fn run(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// A repository on `main` with one commit holding `file.txt`.
    pub fn init(dir: &Path) {
        run(dir, &["init", "-q", "-b", "main"]);
        run(dir, &["config", "user.name", "Test"]);
        run(dir, &["config", "user.email", "test@example.com"]);
        run(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("file.txt"), "one\ntwo\n").unwrap();
        run(dir, &["add", "."]);
        run(dir, &["commit", "-q", "-m", "first"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_a_ref_that_reads_as_an_option() {
        let dir = tempfile::tempdir().unwrap();
        test_repo::init(dir.path());
        assert!(create_branch(dir.path(), "--force").await.is_err());
        assert!(checkout_branch(dir.path(), "-b").await.is_err());
    }

    #[tokio::test]
    async fn lists_local_and_remote_branches_apart() {
        let dir = tempfile::tempdir().unwrap();
        test_repo::init(dir.path());
        test_repo::run(dir.path(), &["branch", "feature"]);
        test_repo::run(dir.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let branches = list_branches(dir.path()).await.unwrap();
        assert_eq!(branches.current.as_deref(), Some("main"));
        assert!(branches.branches.contains(&"feature".to_string()));
        assert_eq!(branches.remotes, vec!["origin/main".to_string()]);
    }

    #[tokio::test]
    async fn known_commits_keeps_only_commits_the_repository_holds() {
        let dir = tempfile::tempdir().unwrap();
        test_repo::init(dir.path());
        let head = head_commit(dir.path(), None).await.unwrap();
        let missing = "0123456789abcdef0123456789abcdef01234567".to_string();
        assert_eq!(known_commits(dir.path(), &[missing, head.clone()]).await.unwrap(), vec![head]);
    }

    #[tokio::test]
    async fn snapshot_commits_only_a_dirty_tree() {
        let dir = tempfile::tempdir().unwrap();
        test_repo::init(dir.path());
        assert_eq!(snapshot_commit(dir.path(), "snap").await.unwrap(), None);
        std::fs::write(dir.path().join("new.txt"), "x").unwrap();
        let commit = snapshot_commit(dir.path(), "snap").await.unwrap().unwrap();
        assert_eq!(commit, head_commit(dir.path(), None).await.unwrap());
        assert!(!is_dirty(dir.path()).await.unwrap());
    }

    #[tokio::test]
    async fn matches_include_patterns_with_gitignore_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let patterns = dir.path().join(".worktreeinclude");
        std::fs::write(&patterns, ".env*\n!.env.example\n").unwrap();
        let candidates = vec![".env".to_string(), ".env.example".to_string(), "node_modules/".to_string()];
        assert_eq!(match_ignore_patterns(&patterns, &candidates).await.unwrap(), vec![".env".to_string()]);
        let none = vec!["other".to_string()];
        assert!(match_ignore_patterns(&patterns, &none).await.unwrap().is_empty());
    }
}
