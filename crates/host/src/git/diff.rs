//! The review panel's reads: which files a comparison touches, and one file's patch at a time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use act_platform::process::{Exec, RunError};
use crate::workspaces::{UnavailableReason, WorkspaceError, WorkspaceResolution, WorkspaceService};

const TIMEOUT: Duration = Duration::from_secs(10);
/// Patches past this are not worth drawing, and the file is offered to an editor instead.
pub const PATCH_LIMIT: usize = 2_000_000;
/// How much unchanged code surrounds each hunk.
const CONTEXT: &str = "-U3";
/// How long a resolved base is reused for. Long enough for one review's patches, short enough to move.
const BASE_TTL: Duration = Duration::from_secs(30);
/// How many untracked files are measured at once, since each one is a Git process of its own.
const UNTRACKED_CONCURRENCY: usize = 8;
/// Git's own empty tree, which a repository with no commits is compared against.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DiffRange {
    Uncommitted,
    Commit { commit: String },
    /// `compare` None is the working tree.
    Branches { base: String, compare: Option<String> },
}

impl DiffRange {
    pub fn key(&self) -> String {
        match self {
            DiffRange::Commit { commit } => format!("commit:{commit}"),
            DiffRange::Uncommitted => "uncommitted".into(),
            DiffRange::Branches { base, compare } => format!("branches:{base}:{}", compare.as_deref().unwrap_or("")),
        }
    }

    /// Only a comparison that ends at the working tree can have files Git has never seen.
    fn reaches_working_tree(&self) -> bool {
        matches!(self, DiffRange::Uncommitted | DiffRange::Branches { compare: None, .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFileSummary {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    pub status: DiffFileStatus,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum DiffSummaryResult {
    #[serde(rename_all = "camelCase")]
    Available { range: DiffRange, ignore_whitespace: bool, files: Vec<DiffFileSummary>, additions: u64, deletions: u64 },
    Unavailable { reason: UnavailableReason },
    #[serde(rename_all = "camelCase")]
    Unknown { workspace_id: String },
    Error { message: String },
}

/// One file's patch, as Git wrote it. `too-large` names the ceiling rather than truncating a patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum DiffPatchResult {
    Available { patch: String },
    TooLarge { limit: usize },
    Error { message: String },
}

#[derive(Debug)]
enum DiffError {
    TooLarge,
    Failed(String),
}

impl From<RunError> for DiffError {
    fn from(error: RunError) -> Self {
        match error {
            RunError::MaxBuffer { .. } => DiffError::TooLarge,
            RunError::NotFound { .. } => DiffError::Failed("Git is not installed or is not on the PATH.".into()),
            other => {
                let stderr = String::from_utf8_lossy(other.stderr()).trim().to_string();
                DiffError::Failed(if stderr.is_empty() { other.to_string() } else { stderr })
            }
        }
    }
}

impl DiffError {
    fn message(self) -> String {
        match self {
            DiffError::TooLarge => "stdout maxBuffer length exceeded".into(),
            DiffError::Failed(message) => message,
        }
    }
}

/// `--literal-pathspecs` so a path is a path, never a glob.
async fn run(root: &Path, args: &[&str]) -> Result<String, DiffError> {
    let output = Exec::new("git").args(["--literal-pathspecs"]).args(args.iter().copied()).cwd(root).timeout(TIMEOUT).max_buffer(PATCH_LIMIT).run().await?;
    Ok(output.stdout_text())
}

/// `git diff` exits 1 whenever it found differences, which `--no-index` makes the normal case.
async fn run_allowing_differences(root: &Path, args: &[&str]) -> Result<String, DiffError> {
    match Exec::new("git").args(["--literal-pathspecs"]).args(args.iter().copied()).cwd(root).timeout(TIMEOUT).max_buffer(PATCH_LIMIT).run().await {
        Ok(output) => Ok(output.stdout_text()),
        Err(RunError::Exit { code: Some(1), stdout, .. }) => Ok(String::from_utf8_lossy(&stdout).into_owned()),
        Err(error) => Err(error.into()),
    }
}

async fn has_commits(root: &Path) -> bool {
    run(root, &["rev-parse", "--verify", "--quiet", "HEAD"]).await.is_ok()
}

/// Where a comparison starts. Branch comparisons measure from where the two sides last agreed.
async fn resolve_base(root: &Path, range: &DiffRange) -> String {
    match range {
        DiffRange::Commit { .. } => unreachable!("commit comparisons resolve both revisions together"),
        DiffRange::Uncommitted => if has_commits(root).await { "HEAD".into() } else { EMPTY_TREE.into() },
        DiffRange::Branches { base, compare } => {
            let head = compare.as_deref().unwrap_or("HEAD");
            if base == "HEAD" && head == "HEAD" && !has_commits(root).await {
                return EMPTY_TREE.into();
            }
            // A base the checkout does not have is compared against directly, and Git says so if it cannot.
            match run(root, &["merge-base", base, head]).await {
                Ok(merge_base) if !merge_base.trim().is_empty() => merge_base.trim().to_string(),
                _ => base.clone(),
            }
        }
    }
}

fn is_commit_hash(hash: &str) -> bool {
    (7..=40).contains(&hash.len()) && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn commit_revisions(root: &Path, hash: &str) -> Result<Vec<String>, DiffError> {
    if !is_commit_hash(hash) {
        return Err(DiffError::Failed("Invalid commit hash.".into()));
    }
    let verify = format!("{hash}^{{commit}}");
    let commit = match run(root, &["rev-parse", "--verify", &verify]).await {
        Ok(commit) => commit.trim().to_string(),
        Err(_) => return Err(DiffError::Failed(format!("Commit {hash} is not available in this repository."))),
    };
    let parents = run(root, &["rev-list", "--parents", "-n", "1", &commit]).await?;
    let parents: Vec<&str> = parents.split_whitespace().collect();
    // A merge is shown against its first parent, and a root commit against the empty tree.
    Ok(vec![parents.get(1).copied().unwrap_or(EMPTY_TREE).to_string(), commit])
}

/// Resolutions kept so a review of many files asks Git once rather than once a file.
type Resolutions = Mutex<HashMap<String, (Instant, Vec<String>)>>;

fn resolutions() -> &'static Resolutions {
    static RESOLVED: std::sync::OnceLock<Resolutions> = std::sync::OnceLock::new();
    RESOLVED.get_or_init(Default::default)
}

/// The revisions a `git diff` for this range takes, with the working tree left as the absent side.
async fn revisions(root: &Path, range: &DiffRange) -> Result<Vec<String>, DiffError> {
    let key = format!("{}\0{}", root.to_string_lossy(), range.key());
    {
        let mut cached = resolutions().lock().unwrap();
        cached.retain(|_, (at, _)| at.elapsed() < BASE_TTL);
        if let Some((_, revisions)) = cached.get(&key) {
            return Ok(revisions.clone());
        }
    }
    let resolved = match range {
        DiffRange::Commit { commit } => commit_revisions(root, commit).await?,
        DiffRange::Uncommitted => vec![resolve_base(root, range).await],
        DiffRange::Branches { compare, .. } => {
            let mut revisions = vec![resolve_base(root, range).await];
            revisions.extend(compare.clone());
            revisions
        }
    };
    resolutions().lock().unwrap().insert(key, (Instant::now(), resolved.clone()));
    Ok(resolved)
}

async fn untracked_files(root: &Path) -> Result<Vec<String>, DiffError> {
    Ok(super::split_nul(&run(root, &["ls-files", "--others", "--exclude-standard", "-z"]).await?))
}

async fn untracked_summary(root: &Path, file: &str) -> Option<DiffFileSummary> {
    if !crate::paths::stays_inside(root, file) {
        return None;
    }
    let metadata = tokio::fs::symlink_metadata(crate::paths::resolve_from(root, Path::new(file))).await.ok()?;
    if !metadata.is_file() && !metadata.file_type().is_symlink() {
        return None;
    }
    let output = run_allowing_differences(root, &["diff", "--numstat", "-z", "--no-index", "--", "/dev/null", file]).await.ok()?;
    let first = output.split('\0').next().unwrap_or("");
    let mut fields = first.split('\t');
    let added = fields.next().unwrap_or("");
    let deleted = fields.next().unwrap_or("");
    let binary = added == "-" || deleted == "-";
    Some(DiffFileSummary {
        path: file.to_string(),
        previous_path: None,
        status: DiffFileStatus::Untracked,
        additions: if binary { 0 } else { added.parse().unwrap_or(0) },
        deletions: 0,
        binary,
    })
}

async fn measure_untracked(root: &Path, files: &[String]) -> Vec<DiffFileSummary> {
    let mut measured = Vec::new();
    for chunk in files.chunks(UNTRACKED_CONCURRENCY) {
        measured.extend(futures::future::join_all(chunk.iter().map(|file| untracked_summary(root, file))).await.into_iter().flatten());
    }
    measured
}

fn status_of(code: char) -> Option<DiffFileStatus> {
    match code {
        'A' | 'C' => Some(DiffFileStatus::Added),
        'D' => Some(DiffFileStatus::Deleted),
        'R' => Some(DiffFileStatus::Renamed),
        'M' | 'T' => Some(DiffFileStatus::Modified),
        _ => None,
    }
}

/// `--numstat -z` writes counts and paths, and a rename writes both of its paths, so records are read
/// with a cursor rather than split into fixed groups.
pub fn read_numstat(output: &str, statuses: &HashMap<String, DiffFileStatus>) -> Vec<DiffFileSummary> {
    let fields: Vec<&str> = output.split('\0').collect();
    let mut files = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let record = fields[index];
        index += 1;
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(deleted)) = (parts.next(), parts.next()) else { continue };
        // Only the counts are tab-delimited; whatever follows is one path, tabs and all.
        let inline = parts.next().filter(|path| !path.is_empty());
        let (path, previous) = match inline {
            Some(path) => (path.to_string(), None),
            None => {
                let previous = fields.get(index).copied().unwrap_or("");
                let path = fields.get(index + 1).copied().unwrap_or("");
                index += 2;
                if path.is_empty() {
                    continue;
                }
                (path.to_string(), Some(previous.to_string()).filter(|previous| !previous.is_empty()))
            }
        };
        let binary = added == "-" || deleted == "-";
        files.push(DiffFileSummary {
            status: statuses.get(&path).copied().unwrap_or(if previous.is_some() { DiffFileStatus::Renamed } else { DiffFileStatus::Modified }),
            path,
            previous_path: previous,
            additions: if binary { 0 } else { added.parse().unwrap_or(0) },
            deletions: if binary { 0 } else { deleted.parse().unwrap_or(0) },
            binary,
        });
    }
    files
}

/// What happened to each file, which `--numstat` does not say and `--name-status` does.
pub fn read_name_status(output: &str) -> HashMap<String, DiffFileStatus> {
    let fields: Vec<&str> = output.split('\0').collect();
    let mut statuses = HashMap::new();
    let mut index = 0;
    while index < fields.len() {
        let code = fields[index];
        index += 1;
        let Some(first) = code.chars().next() else { continue };
        let Some(status) = status_of(first) else { continue };
        // A rename writes its old path then its new one; the new one is what the list is keyed by.
        let renamed = first == 'R' || first == 'C';
        let target = if renamed { fields.get(index + 1) } else { fields.get(index) };
        index += if renamed { 2 } else { 1 };
        if let Some(target) = target.filter(|target| !target.is_empty()) {
            statuses.insert(target.to_string(), status);
        }
    }
    statuses
}

async fn resolved_root(workspace_id: &str, workspaces: &WorkspaceService) -> Result<PathBuf, Result<WorkspaceResolution, WorkspaceError>> {
    match workspaces.resolve(workspace_id).await {
        Ok(WorkspaceResolution::Available { workspace }) => Ok(PathBuf::from(workspace.root)),
        other => Err(other),
    }
}

pub async fn diff_summary(workspace_id: &str, range: &DiffRange, workspaces: &WorkspaceService, ignore_whitespace: bool) -> DiffSummaryResult {
    let root = match resolved_root(workspace_id, workspaces).await {
        Ok(root) => root,
        Err(Ok(WorkspaceResolution::Unavailable { reason, .. })) => return DiffSummaryResult::Unavailable { reason },
        Err(Err(WorkspaceError::Unknown(_))) => return DiffSummaryResult::Unknown { workspace_id: workspace_id.to_string() },
        Err(Err(error)) => return DiffSummaryResult::Error { message: error.to_string() },
        Err(Ok(WorkspaceResolution::Available { .. })) => unreachable!(),
    };
    match summarize(&root, range, ignore_whitespace).await {
        Ok((files, additions, deletions)) => DiffSummaryResult::Available { range: range.clone(), ignore_whitespace, files, additions, deletions },
        Err(error) => DiffSummaryResult::Error { message: error.message() },
    }
}

async fn summarize(root: &Path, range: &DiffRange, ignore_whitespace: bool) -> Result<(Vec<DiffFileSummary>, u64, u64), DiffError> {
    let revisions = revisions(root, range).await?;
    let mut numstat_args = vec!["diff", "--numstat", "-z", "--relative", "--find-renames"];
    let mut name_args = vec!["diff", "--name-status", "-z", "--relative", "--find-renames"];
    if ignore_whitespace {
        numstat_args.push("-w");
        name_args.push("-w");
    }
    for revision in &revisions {
        numstat_args.push(revision);
        name_args.push(revision);
    }
    numstat_args.push("--");
    name_args.push("--");
    let (numstat, names, untracked) = tokio::join!(run(root, &numstat_args), run(root, &name_args), async {
        if range.reaches_working_tree() { untracked_files(root).await } else { Ok(Vec::new()) }
    });
    let tracked = read_numstat(&numstat?, &read_name_status(&names?));
    let fresh = measure_untracked(root, &untracked?).await;
    let mut files: Vec<DiffFileSummary> = tracked.into_iter().chain(fresh).collect();
    files.sort_by(|a, b| crate::text::locale_compare(&a.path, &b.path));
    let additions = files.iter().map(|file| file.additions).sum();
    let deletions = files.iter().map(|file| file.deletions).sum();
    Ok((files, additions, deletions))
}

pub async fn diff_patch(workspace_id: &str, range: &DiffRange, file_path: &str, workspaces: &WorkspaceService, previous_path: Option<&str>, ignore_whitespace: bool) -> DiffPatchResult {
    let root = match resolved_root(workspace_id, workspaces).await {
        Ok(root) => root,
        Err(Ok(WorkspaceResolution::Unavailable { reason, .. })) => return DiffPatchResult::Error { message: format!("Workspace is {}.", reason.as_str()) },
        Err(Err(error)) => return DiffPatchResult::Error { message: error.to_string() },
        Err(Ok(WorkspaceResolution::Available { .. })) => unreachable!(),
    };
    if !crate::paths::stays_inside(&root, file_path) {
        return DiffPatchResult::Error { message: "Path is outside the workspace.".into() };
    }
    match patch(&root, range, file_path, previous_path, ignore_whitespace).await {
        Ok(patch) => DiffPatchResult::Available { patch },
        Err(DiffError::TooLarge) => DiffPatchResult::TooLarge { limit: PATCH_LIMIT },
        Err(DiffError::Failed(message)) => DiffPatchResult::Error { message },
    }
}

async fn patch(root: &Path, range: &DiffRange, file_path: &str, previous_path: Option<&str>, ignore_whitespace: bool) -> Result<String, DiffError> {
    let revisions = revisions(root, range).await?;
    let mut args = vec!["diff", CONTEXT, "--relative", "--find-renames"];
    if ignore_whitespace {
        args.push("-w");
    }
    args.extend(revisions.iter().map(String::as_str));
    args.push("--");
    // Both sides of a rename, or Git sees only the new path and calls the whole file an addition.
    if let Some(previous) = previous_path.filter(|previous| *previous != file_path) {
        args.push(previous);
    }
    args.push(file_path);
    let patch = run(root, &args).await?;
    if !patch.trim().is_empty() {
        return Ok(patch);
    }
    if !range.reaches_working_tree() {
        return Ok(String::new());
    }
    // Nothing tracked answers for a file Git has never seen, so it is diffed against emptiness.
    let mut fresh = vec!["diff", CONTEXT];
    if ignore_whitespace {
        fresh.push("-w");
    }
    fresh.extend(["--no-index", "--", "/dev/null", file_path]);
    run_allowing_differences(root, &fresh).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_repo;

    #[test]
    fn reads_a_rename_with_both_paths() {
        let numstat = "1\t2\ta.txt\0" .to_string() + "0\t0\t\0old name.txt\0new\tname.txt\0-\t-\timage.png\0";
        let statuses = read_name_status("M\0a.txt\0R100\0old name.txt\0new\tname.txt\0A\0image.png\0");
        let files = read_numstat(&numstat, &statuses);
        assert_eq!(files.len(), 3);
        assert_eq!(files[1].path, "new\tname.txt");
        assert_eq!(files[1].previous_path.as_deref(), Some("old name.txt"));
        assert_eq!(files[1].status, DiffFileStatus::Renamed);
        assert!(files[2].binary);
        assert_eq!(files[2].status, DiffFileStatus::Added);
    }

    #[tokio::test]
    async fn summarizes_tracked_and_untracked_and_patches_both() {
        let dir = tempfile::tempdir().unwrap();
        test_repo::init(dir.path());
        std::fs::write(dir.path().join("file.txt"), "one\nthree\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "a\nb\n").unwrap();
        let data = tempfile::tempdir().unwrap();
        let workspaces = WorkspaceService::open(data.path().join("w.json"), data.path().join("p")).await.unwrap();
        let id = workspaces.register_project(dir.path()).await.unwrap().id;
        let DiffSummaryResult::Available { files, additions, deletions, .. } = diff_summary(&id, &DiffRange::Uncommitted, &workspaces, false).await else { panic!() };
        assert_eq!(files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), vec!["file.txt", "new.txt"]);
        assert_eq!(files[1].status, DiffFileStatus::Untracked);
        assert_eq!((additions, deletions), (3, 1));
        let DiffPatchResult::Available { patch } = diff_patch(&id, &DiffRange::Uncommitted, "new.txt", &workspaces, None, false).await else { panic!() };
        assert!(patch.contains("+a\n+b"));
        assert!(matches!(diff_patch(&id, &DiffRange::Uncommitted, "../x", &workspaces, None, false).await, DiffPatchResult::Error { .. }));
    }

    #[tokio::test]
    async fn a_commit_is_compared_against_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        test_repo::init(dir.path());
        std::fs::write(dir.path().join("file.txt"), "one\ntwo\nthree\n").unwrap();
        test_repo::run(dir.path(), &["commit", "-qam", "second"]);
        let head = test_repo::run(dir.path(), &["rev-parse", "HEAD"]).trim().to_string();
        let data = tempfile::tempdir().unwrap();
        let workspaces = WorkspaceService::open(data.path().join("w.json"), data.path().join("p")).await.unwrap();
        let id = workspaces.register_project(dir.path()).await.unwrap().id;
        let range = DiffRange::Commit { commit: head };
        let DiffSummaryResult::Available { files, .. } = diff_summary(&id, &range, &workspaces, false).await else { panic!() };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].additions, 1);
    }

    async fn registered(root: &Path) -> (tempfile::TempDir, WorkspaceService, String) {
        let data = tempfile::tempdir().unwrap();
        let workspaces = WorkspaceService::open(data.path().join("w.json"), data.path().join("p")).await.unwrap();
        let id = workspaces.register_project(root).await.unwrap().id;
        (data, workspaces, id)
    }

    fn paths(result: DiffSummaryResult) -> Vec<String> {
        let DiffSummaryResult::Available { files, .. } = result else { panic!("{result:?}") };
        files.into_iter().map(|file| file.path).collect()
    }

    #[tokio::test]
    async fn branches_measure_from_the_merge_base_and_merges_from_their_first_parent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        test_repo::init(root);
        test_repo::run(root, &["checkout", "-qb", "topic"]);
        std::fs::write(root.join("mine.txt"), "mine\n").unwrap();
        test_repo::run(root, &["add", "-A"]);
        test_repo::run(root, &["commit", "-qm", "mine"]);
        test_repo::run(root, &["checkout", "-q", "main"]);
        std::fs::write(root.join("theirs.txt"), "theirs\n").unwrap();
        test_repo::run(root, &["add", "-A"]);
        test_repo::run(root, &["commit", "-qm", "theirs"]);
        let (_data, workspaces, id) = registered(root).await;
        let range = DiffRange::Branches { base: "main".into(), compare: Some("topic".into()) };
        assert_eq!(paths(diff_summary(&id, &range, &workspaces, false).await), ["mine.txt"]);

        test_repo::run(root, &["merge", "-q", "--no-ff", "topic", "-m", "merge"]);
        let merge = test_repo::run(root, &["rev-parse", "HEAD"]).trim().to_string();
        assert_eq!(paths(diff_summary(&id, &DiffRange::Commit { commit: merge }, &workspaces, false).await), ["mine.txt"]);
        let missing = diff_summary(&id, &DiffRange::Commit { commit: "0000000".into() }, &workspaces, false).await;
        assert!(matches!(missing, DiffSummaryResult::Error { message } if message.contains("not available in this repository")));
    }

    #[tokio::test]
    async fn an_empty_repository_lists_what_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        test_repo::run(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("first.txt"), "one\ntwo\n").unwrap();
        let (_data, workspaces, id) = registered(dir.path()).await;
        for range in [DiffRange::Uncommitted, DiffRange::Branches { base: "HEAD".into(), compare: None }] {
            assert_eq!(paths(diff_summary(&id, &range, &workspaces, false).await), ["first.txt"]);
            let DiffPatchResult::Available { patch } = diff_patch(&id, &range, "first.txt", &workspaces, None, false).await else { panic!() };
            assert!(patch.contains("+one\n+two"));
        }
    }

    #[tokio::test]
    async fn patches_take_literal_paths_both_sides_of_a_rename_and_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        test_repo::init(root);
        std::fs::write(root.join("spaced.txt"), "one\ntwo\n").unwrap();
        test_repo::run(root, &["add", "-A"]);
        test_repo::run(root, &["commit", "-qm", "spaced"]);
        std::fs::write(root.join("spaced.txt"), "  one\ntwo  \n").unwrap();
        test_repo::run(root, &["mv", "file.txt", "moved.txt"]);
        std::fs::write(root.join("moved.txt"), "one\ntwo\nthree\n").unwrap();
        let (_data, workspaces, id) = registered(root).await;
        let range = DiffRange::Uncommitted;

        let DiffPatchResult::Available { patch } = diff_patch(&id, &range, "f*.txt", &workspaces, None, false).await else { panic!() };
        assert_eq!(patch.trim(), "");
        let DiffPatchResult::Available { patch } = diff_patch(&id, &range, "moved.txt", &workspaces, Some("file.txt"), false).await else { panic!() };
        assert!(patch.contains("+three") && !patch.contains("-one"), "{patch}");

        assert_eq!(paths(diff_summary(&id, &range, &workspaces, false).await), ["moved.txt", "spaced.txt"]);
        assert_eq!(paths(diff_summary(&id, &range, &workspaces, true).await), ["moved.txt"]);
    }

    #[test]
    fn range_serializes_as_the_renderer_names_it() {
        let range: DiffRange = serde_json::from_str(r#"{"kind":"branches","base":"main","compare":null}"#).unwrap();
        assert_eq!(range, DiffRange::Branches { base: "main".into(), compare: None });
    }
}
