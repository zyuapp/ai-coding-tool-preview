//! Checkouts the app makes for threads, under the profile's worktree root (`act_store::profile`),
//! and the snapshot each one leaves behind when a thread lets go of it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::git::status::{read_worktree_status, WorktreeStatus};
use crate::git::{self, GitError};
use crate::workspaces::{WorkspaceError, WorkspaceService};

pub const WORKTREE_INCLUDE_FILE: &str = ".worktreeinclude";
pub const SNAPSHOT_REF_NAMESPACE: &str = "refs/aicodingtool";
const SLUG_LIMIT: usize = 24;

/// What this service makes: the checkout on disk, without the project link only workspace state has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorktree {
    pub id: String,
    pub root: String,
    pub workspace_id: String,
    pub base_commit: String,
    pub created_at: i64,
    pub last_used_at: i64,
}

/// One directory under a root the app owns, read from disk for manual management in Settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedWorktree {
    pub id: String,
    pub root: String,
    pub repository: Option<String>,
    /// None means detached, unless `repository` is also None and the directory is no longer a worktree.
    pub branch: Option<String>,
    pub status: WorktreeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorktreeRequest {
    pub project_root: String,
    /// Copies the project checkout's uncommitted work into the new worktree, leaving the original alone.
    pub carry_changes: bool,
    /// Which branch to detach from. The checkout's own HEAD when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Why a worktree let go of its thread. A snapshot commit records this in its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeRelease {
    ReturnedToLocal,
    Deleted,
}

impl WorktreeRelease {
    pub fn reason(self) -> &'static str {
        match self {
            WorktreeRelease::ReturnedToLocal => "returned to local",
            WorktreeRelease::Deleted => "deleted manually",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseWorktreeRequest {
    pub worktree_id: String,
    pub root: String,
    /// The thread handing the checkout back, or None when nothing claims it any more.
    pub task_id: Option<String>,
    pub title: String,
    pub release: WorktreeRelease,
    /// Only forget a missing folder; an existing folder must remain untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_only: Option<bool>,
}

/// What a released worktree left behind, so the thread can say where its work went.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSnapshot {
    pub commit: Option<String>,
    pub short_commit: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Refused(String),
}

pub fn worktree_ref(worktree_id: &str) -> String {
    format!("{SNAPSHOT_REF_NAMESPACE}/{worktree_id}")
}

/// The subject is prefixed so these never read as hand-written work and `git log --grep=aicodingtool` finds them all.
pub fn snapshot_message(title: &str, task_id: Option<&str>, release: WorktreeRelease, reference: Option<&str>) -> String {
    let thread = task_id.map(|task| format!("Thread {task}")).unwrap_or_else(|| "No thread".into());
    let mut lines = vec![format!("aicodingtool: snapshot \"{title}\""), String::new(), format!("{thread} · {}", release.reason())];
    if let Some(reference) = reference {
        lines.push(format!("Ref {reference}"));
    }
    lines.join("\n")
}

/// The project's name, so `git worktree list` stays readable, plus the worktree's id.
pub fn worktree_directory_name(project_root: &str, worktree_id: &str) -> String {
    let name = project_root.split('/').rfind(|part| !part.is_empty()).unwrap_or("project").to_lowercase();
    let slug: String = replace_runs(&name, |character| character.is_ascii_lowercase() || character.is_ascii_digit()).trim_matches('-').chars().take(SLUG_LIMIT).collect();
    format!("{}-{worktree_id}", if slug.is_empty() { "project" } else { &slug })
}

/// The id inside a directory the app made. A name from anywhere else stands in for itself.
pub fn worktree_id_from_directory_name(name: &str) -> String {
    if let Some((_, id)) = name.rsplit_once('-') {
        if id.len() == 8 && id.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
            return id.to_string();
        }
    }
    replace_runs(name, |character| character.is_ascii_alphanumeric() || character == '-')
}

/// Every run of characters `keep` refuses, replaced by one hyphen.
fn replace_runs(text: &str, keep: impl Fn(char) -> bool) -> String {
    let mut replaced = String::with_capacity(text.len());
    let mut in_run = false;
    for character in text.chars() {
        if keep(character) {
            replaced.push(character);
            in_run = false;
        } else if !in_run {
            replaced.push('-');
            in_run = true;
        }
    }
    replaced
}

pub struct WorktreeService {
    worktrees_root: PathBuf,
    /// Every root the app owns checkouts in, newest first. Only the first one is created in.
    owned_roots: Vec<PathBuf>,
    workspaces: Arc<WorkspaceService>,
}

impl WorktreeService {
    pub fn new(worktrees_root: impl Into<PathBuf>, legacy_roots: Vec<PathBuf>, workspaces: Arc<WorkspaceService>) -> Self {
        let worktrees_root = worktrees_root.into();
        let mut owned_roots = vec![worktrees_root.clone()];
        for root in legacy_roots {
            if !owned_roots.contains(&root) {
                owned_roots.push(root);
            }
        }
        Self { worktrees_root, owned_roots, workspaces }
    }

    pub fn owned_roots(&self) -> &[PathBuf] {
        &self.owned_roots
    }

    /// A worktree detached at whatever the project checkout has checked out right now, carrying the
    /// gitignored files `.worktreeinclude` names across. No branch is created.
    pub async fn create(&self, request: &CreateWorktreeRequest) -> Result<CreatedWorktree, WorktreeError> {
        let project = Path::new(&request.project_root);
        let (repository, base_commit) = tokio::join!(git::repository_root(project), git::head_commit(project, request.branch.as_deref()));
        let (repository, base_commit) = (repository?, base_commit?);
        let id = format!("{:08x}", rand_u32());
        let root = self.worktrees_root.join(worktree_directory_name(&repository.to_string_lossy(), &id));
        tokio::fs::create_dir_all(&self.worktrees_root).await?;
        git::add_worktree(&repository, &root, &base_commit).await?;

        copy_included_files(&repository, &root).await?;
        if request.carry_changes {
            carry_changes(&repository, &root).await?;
        }

        let registration = self.workspaces.register_worktree(&root).await?;
        let at = chrono::Utc::now().timestamp_millis();
        Ok(CreatedWorktree { id, root: registration.root, workspace_id: registration.id, base_commit, created_at: at, last_used_at: at })
    }

    /// Reads every directory the app owns without changing it, including roots used by older builds.
    pub async fn list(&self) -> Result<Vec<ManagedWorktree>, WorktreeError> {
        let mut roots = Vec::new();
        for base in &self.owned_roots {
            let mut entries = match tokio::fs::read_dir(base).await {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            while let Some(entry) = entries.next_entry().await? {
                if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
                    roots.push(base.join(entry.file_name()));
                }
            }
        }
        let mut worktrees = Vec::new();
        for chunk in roots.chunks(4) {
            worktrees.extend(futures::future::join_all(chunk.iter().map(|root| read_managed(root))).await);
        }
        let mut unique: Vec<ManagedWorktree> = Vec::new();
        for worktree in worktrees {
            match unique.iter_mut().find(|known| known.root == worktree.root) {
                Some(known) => *known = worktree,
                None => unique.push(worktree),
            }
        }
        unique.sort_by(|left, right| crate::text::locale_compare(&left.root, &right.root));
        Ok(unique)
    }

    /// Resolves a caller-supplied path only after proving it sits under a root the app owns.
    pub async fn owned_path(&self, root: &Path) -> Result<PathBuf, WorktreeError> {
        self.assert_owned(root).await?;
        Ok(canonical_path(root).await)
    }

    /// Commits everything left in the worktree so a thread never loses work by walking away from it,
    /// then gives the directory back. A detached worktree's commits are kept reachable by a ref under
    /// `refs/aicodingtool`. The snapshot lands before the directory goes, so a failure keeps both.
    pub async fn release(&self, request: &ReleaseWorktreeRequest) -> Result<WorktreeSnapshot, WorktreeError> {
        let root = Path::new(&request.root);
        self.assert_owned(root).await?;
        if request.missing_only == Some(true) {
            match tokio::fs::symlink_metadata(root).await {
                Ok(_) => return Err(WorktreeError::Refused("This folder exists again. Refresh the worktree list before deleting it.".into())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            self.workspaces.forget_worktree(&canonical_path(root).await).await?;
            return Ok(WorktreeSnapshot::default());
        }
        let snapshot = self.snapshot(request).await?;
        self.delete(root).await?;
        Ok(snapshot)
    }

    /// Throws the directory away along with anything uncommitted in it. Branches are never touched.
    pub async fn delete(&self, root: &Path) -> Result<(), WorktreeError> {
        self.assert_owned(root).await?;
        let canonical = canonical_path(root).await;
        if let Some(repository) = parent_repository(root).await {
            git::remove_worktree(&repository, root).await;
        }
        match tokio::fs::remove_dir_all(root).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.workspaces.forget_worktree(&canonical).await?;
        Ok(())
    }

    async fn snapshot(&self, request: &ReleaseWorktreeRequest) -> Result<WorktreeSnapshot, WorktreeError> {
        let root = Path::new(&request.root);
        if !tokio::fs::metadata(root).await.is_ok_and(|metadata| metadata.is_dir()) || parent_repository(root).await.is_none() {
            return Ok(WorktreeSnapshot::default());
        }
        let reference = git::is_detached(root).await.then(|| worktree_ref(&request.worktree_id));
        let message = snapshot_message(&request.title, request.task_id.as_deref(), request.release, reference.as_deref());
        let commit = git::snapshot_commit(root, &message).await?;
        let kept = match (&commit, &reference) {
            (Some(commit), _) => Some(commit.clone()),
            (None, Some(_)) => git::unreachable_head(root).await?,
            (None, None) => None,
        };
        let Some(kept) = kept else { return Ok(WorktreeSnapshot::default()) };
        if let Some(reference) = &reference {
            git::update_ref(root, reference, &kept).await?;
        }
        let short_commit = match &commit {
            Some(commit) => Some(git::short_commit(root, commit).await?),
            None => None,
        };
        Ok(WorktreeSnapshot { commit, short_commit, reference })
    }

    /// Everything here acts only inside a root the app owns; any other path is refused outright.
    async fn assert_owned(&self, root: &Path) -> Result<(), WorktreeError> {
        let mut bases = Vec::new();
        for owned in &self.owned_roots {
            bases.push(canonical_path(owned).await);
            bases.push(crate::paths::resolve(owned));
        }
        let candidates = [canonical_path(root).await, crate::paths::resolve(root)];
        let owned = bases.iter().any(|base| candidates.iter().any(|candidate| candidate != base && candidate.starts_with(base)));
        if owned {
            Ok(())
        } else {
            Err(WorktreeError::Refused(format!("Not an AI Coding Tool worktree: {}", root.to_string_lossy())))
        }
    }
}

fn rand_u32() -> u32 {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

async fn read_managed(root: &Path) -> ManagedWorktree {
    let canonical = canonical_path(root).await;
    let repository = parent_repository(&canonical).await;
    let (branch, status) = match &repository {
        Some(_) => tokio::join!(git::current_branch(&canonical), read_worktree_status(&canonical)),
        None => (None, WorktreeStatus { changed_files: None, comparison: None }),
    };
    let name = canonical.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    ManagedWorktree {
        id: worktree_id_from_directory_name(&name),
        root: canonical.to_string_lossy().into_owned(),
        repository: repository.map(|repository| repository.to_string_lossy().into_owned()),
        branch,
        status,
    }
}

/// Resolved through symlinks while the path exists, so the registry, git and the disk agree on it.
async fn canonical_path(target: &Path) -> PathBuf {
    tokio::fs::canonicalize(target).await.unwrap_or_else(|_| crate::paths::resolve(target))
}

/// A worktree's `.git` is a file pointing into the checkout it was linked from.
async fn parent_repository(root: &Path) -> Option<PathBuf> {
    let common = git::git(root, &["rev-parse", "--git-common-dir"]).await.ok()?;
    let common = crate::paths::resolve_from(root, Path::new(common.trim()));
    common.parent().map(Path::to_path_buf)
}

/// The files `.worktreeinclude` names, copied out of the project checkout. Only paths Git already
/// ignores are eligible, so a pattern can never duplicate a tracked file into the worktree.
async fn copy_included_files(repository: &Path, worktree: &Path) -> Result<(), WorktreeError> {
    let patterns = repository.join(WORKTREE_INCLUDE_FILE);
    if tokio::fs::metadata(&patterns).await.is_err() {
        return Ok(());
    }
    let included = git::match_ignore_patterns(&patterns, &git::ignored_paths(repository).await?).await?;
    for relative in included {
        let from = repository.join(&relative);
        let to = worktree.join(&relative);
        // The includes are conveniences, not the thread's work; one that will not copy is not fatal.
        let copied = tokio::task::spawn_blocking(move || crate::fs_copy::copy_recursive(&from, &to)).await;
        if let Ok(Err(error)) | Err(error) = copied.map_err(std::io::Error::other) {
            tracing::warn!("Could not copy {relative} into the worktree: {error}");
        }
    }
    Ok(())
}

/// Uncommitted work, copied across: tracked edits as a patch, untracked files and symlinks verbatim.
async fn carry_changes(repository: &Path, worktree: &Path) -> Result<(), WorktreeError> {
    let patch = git::tracked_diff(repository).await?;
    if !patch.trim().is_empty() {
        git::apply_patch(worktree, &patch).await?;
    }
    for relative in git::untracked_paths(repository).await? {
        let from = repository.join(&relative);
        let to = worktree.join(&relative);
        // A path deleted while this runs is gone from the checkout too, so there is nothing to carry.
        let Ok(metadata) = tokio::fs::symlink_metadata(&from).await else { continue };
        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            continue;
        }
        tokio::task::spawn_blocking(move || crate::fs_copy::copy_recursive(&from, &to)).await.map_err(std::io::Error::other)??;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_repo;

    #[test]
    fn directory_names_slug_the_project_and_read_back_their_id() {
        assert_eq!(worktree_directory_name("/Users/me/My Project!", "0a1b2c3d"), "my-project-0a1b2c3d");
        assert_eq!(worktree_directory_name("/", "0a1b2c3d"), "project-0a1b2c3d");
        assert_eq!(worktree_directory_name("/x/a-very-long-repository-name-indeed", "0a1b2c3d"), "a-very-long-repository-n-0a1b2c3d");
        assert_eq!(worktree_id_from_directory_name("my-project-0a1b2c3d"), "0a1b2c3d");
        assert_eq!(worktree_id_from_directory_name("other dir"), "other-dir");
    }

    #[test]
    fn snapshot_message_names_thread_reason_and_ref() {
        let message = snapshot_message("Fix", Some("t1"), WorktreeRelease::Deleted, Some("refs/aicodingtool/abc"));
        assert_eq!(message, "aicodingtool: snapshot \"Fix\"\n\nThread t1 · deleted manually\nRef refs/aicodingtool/abc");
    }

    async fn setup() -> (tempfile::TempDir, PathBuf, WorktreeService) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let project = base.join("project");
        std::fs::create_dir(&project).unwrap();
        test_repo::init(&project);
        std::fs::write(project.join(".gitignore"), ".env\nnode_modules/\n").unwrap();
        std::fs::write(project.join(WORKTREE_INCLUDE_FILE), ".env\n").unwrap();
        std::fs::write(project.join(".env"), "SECRET=1").unwrap();
        std::fs::create_dir(project.join("node_modules")).unwrap();
        std::fs::write(project.join("node_modules/x"), "x").unwrap();
        test_repo::run(&project, &["add", ".gitignore", WORKTREE_INCLUDE_FILE]);
        test_repo::run(&project, &["commit", "-qm", "ignore"]);
        let workspaces = Arc::new(WorkspaceService::open(base.join("w.json"), base.join("p")).await.unwrap());
        let service = WorktreeService::new(base.join("worktrees"), Vec::new(), workspaces);
        (dir, project, service)
    }

    #[tokio::test]
    async fn creates_with_includes_and_carried_work_then_snapshots_on_release() {
        let (_dir, project, service) = setup().await;
        std::fs::write(project.join("file.txt"), "changed\n").unwrap();
        std::fs::write(project.join("fresh.txt"), "new\n").unwrap();
        let request = CreateWorktreeRequest { project_root: project.to_string_lossy().into_owned(), carry_changes: true, branch: None };
        let created = service.create(&request).await.unwrap();
        let root = PathBuf::from(&created.root);
        assert_eq!(std::fs::read_to_string(root.join(".env")).unwrap(), "SECRET=1");
        assert!(!root.join("node_modules").exists());
        assert_eq!(std::fs::read_to_string(root.join("file.txt")).unwrap(), "changed\n");
        assert_eq!(std::fs::read_to_string(root.join("fresh.txt")).unwrap(), "new\n");
        assert_eq!(std::fs::read_to_string(project.join("file.txt")).unwrap(), "changed\n");

        let listed = service.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);
        assert_eq!(listed[0].branch, None);

        let release = ReleaseWorktreeRequest { worktree_id: created.id.clone(), root: created.root.clone(), task_id: Some("t".into()), title: "Work".into(), release: WorktreeRelease::Deleted, missing_only: None };
        let snapshot = service.release(&release).await.unwrap();
        assert!(!root.exists());
        let reference = snapshot.reference.unwrap();
        assert_eq!(reference, worktree_ref(&created.id));
        let kept = test_repo::run(&project, &["rev-parse", &reference]);
        assert_eq!(kept.trim(), snapshot.commit.unwrap());
    }

    async fn made(service: &WorktreeService, project: &Path) -> (CreatedWorktree, PathBuf) {
        let request = CreateWorktreeRequest { project_root: project.to_string_lossy().into_owned(), carry_changes: false, branch: None };
        let created = service.create(&request).await.unwrap();
        let root = PathBuf::from(&created.root);
        (created, root)
    }

    fn release_of(created: &CreatedWorktree, missing_only: Option<bool>) -> ReleaseWorktreeRequest {
        ReleaseWorktreeRequest { worktree_id: created.id.clone(), root: created.root.clone(), task_id: Some("t".into()), title: "Work".into(), release: WorktreeRelease::ReturnedToLocal, missing_only }
    }

    #[tokio::test]
    async fn release_keeps_detached_commits_by_ref_and_a_branch_needs_none() {
        let (_dir, project, service) = setup().await;
        let (clean, _) = made(&service, &project).await;
        assert_eq!(service.release(&release_of(&clean, None)).await.unwrap(), WorktreeSnapshot::default());

        let (detached, root) = made(&service, &project).await;
        std::fs::write(root.join("file.txt"), "committed detached\n").unwrap();
        test_repo::run(&root, &["commit", "-qam", "detached work"]);
        let head = test_repo::run(&root, &["rev-parse", "HEAD"]);
        let snapshot = service.release(&release_of(&detached, None)).await.unwrap();
        assert_eq!(snapshot.commit, None);
        let reference = snapshot.reference.unwrap();
        assert_eq!(test_repo::run(&project, &["rev-parse", &reference]), head);

        let (branched, root) = made(&service, &project).await;
        test_repo::run(&root, &["checkout", "-q", "-b", "thread-work"]);
        std::fs::write(root.join("file.txt"), "on a branch\n").unwrap();
        let snapshot = service.release(&release_of(&branched, None)).await.unwrap();
        assert_eq!(snapshot.reference, None);
        assert_eq!(test_repo::run(&project, &["rev-parse", "thread-work"]).trim(), snapshot.commit.unwrap());
    }

    #[tokio::test]
    async fn a_snapshot_survives_a_hook_that_rejects_commits() {
        let (_dir, project, service) = setup().await;
        let hook = project.join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let (created, root) = made(&service, &project).await;
        std::fs::write(root.join("file.txt"), "must not be lost\n").unwrap();
        assert!(service.release(&release_of(&created, None)).await.unwrap().commit.is_some());
    }

    #[tokio::test]
    async fn a_gone_directory_or_repository_still_releases_and_deletes() {
        let (_dir, project, service) = setup().await;
        let (gone, root) = made(&service, &project).await;
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(service.release(&release_of(&gone, None)).await.unwrap(), WorktreeSnapshot::default());

        let (orphaned, root) = made(&service, &project).await;
        std::fs::remove_dir_all(&project).unwrap();
        service.delete(&root).await.unwrap();
        assert!(!root.exists());
        assert!(!service.workspaces.records().await.iter().any(|record| record.root == orphaned.root || record.root == gone.root));
    }

    #[tokio::test]
    async fn forgetting_refuses_a_folder_that_exists() {
        let (_dir, project, service) = setup().await;
        let (created, root) = made(&service, &project).await;
        let request = release_of(&created, Some(true));
        assert!(service.release(&request).await.unwrap_err().to_string().contains("folder exists again"));
        assert!(root.join("file.txt").exists());
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(service.release(&request).await.unwrap(), WorktreeSnapshot::default());
        assert!(!service.workspaces.records().await.iter().any(|record| record.root == created.root));
    }

    #[tokio::test]
    async fn includes_honour_negation_and_never_duplicate_tracked_files() {
        let (_dir, project, service) = setup().await;
        std::fs::write(project.join(".gitignore"), ".env\n*.log\n").unwrap();
        std::fs::write(project.join("keep.log"), "keep").unwrap();
        std::fs::write(project.join("drop.log"), "drop").unwrap();
        std::fs::write(project.join(WORKTREE_INCLUDE_FILE), "*.log\n!drop.log\nfile.txt\n").unwrap();
        test_repo::run(&project, &["commit", "-qam", "includes"]);
        std::fs::write(project.join("file.txt"), "dirty\n").unwrap();
        std::os::unix::fs::symlink("file.txt", project.join("link.txt")).unwrap();
        let (_, root) = made(&service, &project).await;
        assert!(root.join("keep.log").exists());
        assert!(!root.join("drop.log").exists());
        assert!(!root.join(".env").exists());
        assert_eq!(std::fs::read_to_string(root.join("file.txt")).unwrap(), "one\ntwo\n");
        assert!(!root.join("link.txt").exists());

        let request = CreateWorktreeRequest { project_root: project.to_string_lossy().into_owned(), carry_changes: true, branch: None };
        let carried = PathBuf::from(service.create(&request).await.unwrap().root);
        assert_eq!(std::fs::read_link(carried.join("link.txt")).unwrap(), Path::new("file.txt"));
    }

    #[tokio::test]
    async fn listing_tells_unknown_comparisons_and_orphans_from_clean_worktrees() {
        let (_dir, project, service) = setup().await;
        test_repo::run(&project, &["branch", "-m", "trunk"]);
        made(&service, &project).await;
        let orphan = service.worktrees_root.join("orphan");
        std::fs::create_dir(&orphan).unwrap();
        let listed = service.list().await.unwrap();
        let valid = listed.iter().find(|item| item.repository.is_some()).unwrap();
        assert_eq!(valid.status, WorktreeStatus { changed_files: Some(0), comparison: None });
        let invalid = listed.iter().find(|item| item.root.ends_with("/orphan")).unwrap();
        assert_eq!(invalid.status, WorktreeStatus { changed_files: None, comparison: None });

        let file = service.worktrees_root.join("file");
        std::fs::write(&file, "not a directory").unwrap();
        let broken = WorktreeService::new(file, Vec::new(), service.workspaces.clone());
        assert!(broken.list().await.is_err());
    }

    #[tokio::test]
    async fn refuses_a_folder_outside_its_roots() {
        let (_dir, project, service) = setup().await;
        assert!(service.delete(&project).await.is_err());
        assert!(project.exists());
    }
}
