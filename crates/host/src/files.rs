//! Files the window names: dropped files, folders typed into the project picker, and paths a message
//! links to, each checked before anything opens or reads it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bigger than any path anyone writes, and small enough that nothing chokes on the argument.
pub const MAX_FILE_PATH: usize = 4_096;
/// How many paths one drop may name.
pub const MAX_DESCRIBED_FILES: usize = 20;
/// How many folders a prefix lists.
pub const MAX_DIRECTORIES: usize = 20;
/// How many places one link is allowed to be looked for, so a deep path cannot fan out without end.
const MAX_FILE_CANDIDATES: usize = 256;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct FileError(pub String);

fn refused(message: impl Into<String>) -> FileError {
    FileError(message.into())
}

/// What the window dropped: the name to show, and whether the path is a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedFileDraft {
    pub path: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<bool>,
}

pub async fn describe_files(paths: &[String]) -> Vec<AttachedFileDraft> {
    let named: Vec<&String> = paths.iter().filter(|path| !path.is_empty() && path.len() <= MAX_FILE_PATH).take(MAX_DESCRIBED_FILES).collect();
    let described = futures::future::join_all(named.into_iter().map(|candidate| async move {
        let metadata = tokio::fs::metadata(candidate).await.ok()?;
        if !metadata.is_file() && !metadata.is_dir() {
            return None;
        }
        let resolved = crate::paths::resolve(Path::new(candidate));
        let name = resolved.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        Some(AttachedFileDraft { path: resolved.to_string_lossy().into_owned(), name, folder: metadata.is_dir().then_some(true) })
    }))
    .await;
    described.into_iter().flatten().collect()
}

/// Folders whose path starts with `prefix`, for the project picker. A bounded scan: at most 4096
/// entries read, 40 symlinks followed, 20 folders answered. Hidden folders only when asked for.
pub async fn list_directories(prefix: &str) -> Result<Vec<String>, FileError> {
    if prefix.len() > MAX_FILE_PATH || prefix.contains('\0') {
        return Err(refused("Invalid directory prefix."));
    }
    let tilde = prefix == "~";
    let expanded = if tilde || prefix.starts_with("~/") {
        let mut expanded = crate::paths::home().join(prefix[1.min(prefix.len())..].trim_start_matches('/')).to_string_lossy().into_owned();
        if prefix.ends_with('/') && !expanded.ends_with('/') {
            expanded.push('/');
        }
        expanded
    } else {
        prefix.to_string()
    };
    if expanded.is_empty() || !Path::new(&expanded).is_absolute() {
        return Ok(Vec::new());
    }
    let whole = tilde || expanded.ends_with('/');
    let directory = if whole { PathBuf::from(&expanded) } else { Path::new(&expanded).parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/")) };
    let partial = if whole { String::new() } else { Path::new(&expanded).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default() };
    let mut entries = match tokio::fs::read_dir(&directory).await {
        Ok(entries) => entries,
        Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => return Ok(Vec::new()),
        Err(error) => return Err(refused(error.to_string())),
    };
    let mut matches = Vec::new();
    let mut scanned = 0;
    let mut links = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        scanned += 1;
        if scanned > 4096 {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&partial) || (name.starts_with('.') && !partial.starts_with('.')) {
            continue;
        }
        let root = directory.join(&name);
        let Ok(kind) = entry.file_type().await else { continue };
        let folder = kind.is_dir()
            || (kind.is_symlink() && {
                links += 1;
                links <= 40 && tokio::fs::metadata(&root).await.is_ok_and(|metadata| metadata.is_dir())
            });
        let shown = format!("{}/", root.to_string_lossy());
        if folder && shown.len() <= MAX_FILE_PATH {
            matches.push(shown);
        }
        if matches.len() == MAX_DIRECTORIES {
            break;
        }
    }
    matches.sort_by(|a, b| crate::text::locale_compare(a, b));
    Ok(matches)
}

/// A folder the user typed rather than picked: a directory, and theirs rather than a checkout the app made.
pub async fn project_folder(root: &str, worktrees_roots: &[PathBuf]) -> Result<PathBuf, FileError> {
    let named = root.trim();
    if named.is_empty() {
        return Err(refused("Name a folder to open."));
    }
    let expanded = crate::paths::expand_home(named);
    if !expanded.is_absolute() {
        return Err(refused("Name the folder by its full path."));
    }
    if !project_roots_are_own(&[expanded.as_path()], worktrees_roots) {
        return Err(refused("That folder is a checkout this app made. Name the repository it was cut from."));
    }
    let shown = expanded.to_string_lossy();
    match tokio::fs::metadata(&expanded).await {
        Err(_) => Err(refused(format!("There is no folder at {shown}."))),
        Ok(metadata) if !metadata.is_dir() => Err(refused(format!("{shown} is a file, not a folder."))),
        Ok(_) => Ok(expanded),
    }
}

/// Whether none of these project roots sit inside a folder the app keeps checkouts in.
pub fn project_roots_are_own(projects: &[&Path], worktrees_roots: &[PathBuf]) -> bool {
    let owned: Vec<String> = worktrees_roots.iter().map(|root| format!("{}/", crate::paths::resolve(root).to_string_lossy())).collect();
    !projects.iter().any(|project| {
        let resolved = crate::paths::resolve(project).to_string_lossy().into_owned();
        owned.iter().any(|root| resolved.starts_with(root.as_str()))
    })
}

/// The file a message named, inside the checkout the thread works in, as an absolute path.
pub async fn file_in_checkout(root: &str, candidate: &str) -> Result<PathBuf, FileError> {
    if root.is_empty() {
        return Err(refused("Invalid folder."));
    }
    if candidate.is_empty() || candidate.len() > MAX_FILE_PATH {
        return Err(refused("Invalid file path."));
    }
    let named = match candidate.strip_prefix("~/") {
        Some(rest) => crate::paths::home().join(rest),
        None => PathBuf::from(candidate),
    };
    let (checkout, file) = tokio::join!(tokio::fs::canonicalize(root), tokio::fs::canonicalize(crate::paths::resolve_from(Path::new(root), &named)));
    let missing = |error: std::io::Error| if error.kind() == std::io::ErrorKind::NotFound { refused("That file is not there any more.") } else { refused(error.to_string()) };
    let (checkout, file) = (checkout.map_err(missing)?, file.map_err(missing)?);
    if !crate::paths::is_path_inside(&checkout, &file) {
        return Err(refused("That file is outside this thread's folder."));
    }
    Ok(file)
}

/// The places a written path might really be, nearest the thread first: as written, then its tail
/// re-anchored on each checkout, which finds a file named from the wrong folder or a sibling checkout.
fn file_candidates(roots: &[String], named: &str) -> Vec<PathBuf> {
    let segments: Vec<&str> = named.split(['/', '\\']).filter(|part| !part.is_empty() && *part != ".").collect();
    let absolute = Path::new(named).is_absolute() || is_windows_drive(named);
    let exact: Vec<PathBuf> = if absolute { vec![crate::paths::resolve(Path::new(named))] } else { roots.iter().map(|root| crate::paths::resolve_from(Path::new(root), Path::new(named))).collect() };
    let tails: Vec<String> = (1..segments.len()).map(|index| segments[index..].join("/")).collect();
    let repairs = roots.iter().flat_map(|root| tails.iter().map(move |tail| crate::paths::resolve_from(Path::new(root), Path::new(tail))));
    let mut candidates: Vec<PathBuf> = Vec::new();
    for candidate in exact.into_iter().chain(repairs) {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates.truncate(MAX_FILE_CANDIDATES);
    candidates
}

fn is_windows_drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'/' || bytes[2] == b'\\')
}

/// The file a link names, as an absolute path the desktop can open. Looked for in the thread's
/// checkout, the project's, and the project's other checkouts, before being taken as written.
pub async fn openable_file(roots: &[String], candidate: &str) -> Result<PathBuf, FileError> {
    if roots.iter().any(String::is_empty) {
        return Err(refused("Invalid folder."));
    }
    if candidate.is_empty() || candidate.len() > MAX_FILE_PATH {
        return Err(refused("Invalid file path."));
    }
    let named = match candidate.strip_prefix("~/") {
        Some(rest) => crate::paths::home().join(rest).to_string_lossy().into_owned(),
        None => candidate.to_string(),
    };
    for target in file_candidates(roots, &named) {
        if tokio::fs::metadata(&target).await.is_ok_and(|metadata| metadata.is_file()) {
            return Ok(tokio::fs::canonicalize(&target).await.unwrap_or(target));
        }
    }
    Err(refused(format!("{} could not find {candidate} on this machine.", act_core::identity::APP_NAME)))
}

/// The nearest existing ancestor, canonical, with the rest of the path put back on it.
async fn canonical_write_path(candidate: &Path) -> Option<PathBuf> {
    let mut pending = crate::paths::resolve(candidate);
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match tokio::fs::canonicalize(&pending).await {
            Ok(existing) => return Some(suffix.iter().rev().fold(existing, |current, part| current.join(part))),
            Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => {
                let parent = pending.parent()?.to_path_buf();
                suffix.push(pending.file_name()?.to_os_string());
                pending = parent;
            }
            Err(_) => return None,
        }
    }
}

/// Whether a write to `candidate` lands inside `root`, symlinks included, even if it does not exist yet.
pub async fn is_write_path_inside(root: &Path, candidate: &Path) -> bool {
    let rooted = if candidate.is_absolute() { candidate.to_path_buf() } else { crate::paths::resolve_from(root, candidate) };
    let (root, candidate) = tokio::join!(canonical_write_path(root), canonical_write_path(&rooted));
    matches!((root, candidate), (Some(root), Some(candidate)) if crate::paths::is_path_inside(&root, &candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_link_written_from_the_wrong_folder_is_found_in_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src/app")).unwrap();
        std::fs::write(root.join("src/app/main.rs"), "").unwrap();
        let roots = vec![root.to_string_lossy().into_owned()];
        let found = openable_file(&roots, "/other/checkout/src/app/main.rs").await.unwrap();
        assert_eq!(found, root.join("src/app/main.rs"));
        assert!(openable_file(&roots, "missing.rs").await.is_err());
    }

    #[tokio::test]
    async fn a_checkout_file_must_stay_inside_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(dir.path().join("outside.txt"), "").unwrap();
        let root_text = root.to_string_lossy();
        assert!(file_in_checkout(&root_text, "a.txt").await.is_ok());
        assert_eq!(file_in_checkout(&root_text, "../outside.txt").await.unwrap_err().0, "That file is outside this thread's folder.");
        assert_eq!(file_in_checkout(&root_text, "gone.txt").await.unwrap_err().0, "That file is not there any more.");
    }

    #[tokio::test]
    async fn lists_matching_folders_and_hides_dotted_ones_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["proj-a", "proj-b", ".proj-hidden", "other"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        std::fs::write(dir.path().join("proj-file"), "").unwrap();
        let base = dir.path().to_string_lossy();
        let found = list_directories(&format!("{base}/proj")).await.unwrap();
        assert_eq!(found, vec![format!("{base}/proj-a/"), format!("{base}/proj-b/")]);
        assert_eq!(list_directories(&format!("{base}/.proj")).await.unwrap(), vec![format!("{base}/.proj-hidden/")]);
        assert!(list_directories("relative").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_typed_folder_inside_the_worktrees_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let worktrees = dir.path().join("worktrees");
        std::fs::create_dir_all(worktrees.join("repo-0a1b2c3d")).unwrap();
        let inside = worktrees.join("repo-0a1b2c3d").to_string_lossy().into_owned();
        assert!(project_folder(&inside, std::slice::from_ref(&worktrees)).await.unwrap_err().0.contains("checkout this app made"));
        assert!(project_folder(&dir.path().to_string_lossy(), &[worktrees]).await.is_ok());
        assert_eq!(project_folder("relative/path", &[]).await.unwrap_err().0, "Name the folder by its full path.");
    }

    #[tokio::test]
    async fn a_write_through_a_symlink_out_of_the_root_is_outside() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        act_platform::fs::symlink(dir.path(), root.join("escape")).unwrap();
        assert!(is_write_path_inside(&root, Path::new("new/file.txt")).await);
        assert!(!is_write_path_inside(&root, Path::new("escape/file.txt")).await);
    }
}
