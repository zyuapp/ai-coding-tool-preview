//! Where the app keeps its data, and the housekeeping it does there at launch.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const WINDOW_STORAGE_FILE: &str = "window.v1.json";
const LAUNCHED_VERSION: &str = "launched-version";
const CONTEXT_SUFFIX: &str = ".context.json";

/// How long an attachment nothing names stays: a composer or a paired device may still be about to send it.
pub const ORPHAN_ATTACHMENT_MIN_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppProfile {
    pub name: String,
    pub user_data: PathBuf,
    pub worktrees_root: PathBuf,
}

/// The platform's per-user application data folder, as Electron's `appData` names it.
pub fn app_data_dir() -> PathBuf {
    let home = crate::paths::home();
    if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|path| path.is_absolute()).unwrap_or_else(|| home.join(".config"))
    }
}

/// Dotted versions compared part by part, a missing part counting as zero.
pub fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let parts = |version: &str| version.split('.').map(|part| part.parse::<f64>().unwrap_or(f64::NAN)).collect::<Vec<_>>();
    let (left, right) = (parts(left), parts(right));
    for index in 0..left.len().max(right.len()) {
        let difference = left.get(index).copied().unwrap_or(0.0) - right.get(index).copied().unwrap_or(0.0);
        // A part that is not a number ends the comparison without either side winning.
        if difference.is_nan() {
            return std::cmp::Ordering::Equal;
        }
        if difference > 0.0 {
            return std::cmp::Ordering::Greater;
        }
        if difference < 0.0 {
            return std::cmp::Ordering::Less;
        }
    }
    std::cmp::Ordering::Equal
}

/// The version this launch updated the app to, or None when it is not the first launch since an
/// update. A profile from before this record was kept counts as updated when its window storage
/// shows it has launched before. Read once at startup, before the window writes there.
pub fn read_app_launch(user_data: &Path, version: &str) -> Option<String> {
    let record = user_data.join(LAUNCHED_VERSION);
    let previous = fs::read_to_string(&record).ok().map(|text| text.trim().to_string()).filter(|text| !text.is_empty());
    if previous.as_deref() == Some(version) {
        return None;
    }
    // Going back to an older build is not an update, so it is recorded without a word.
    let updated = match &previous {
        None => user_data.join(WINDOW_STORAGE_FILE).exists(),
        Some(previous) => compare_versions(version, previous) == std::cmp::Ordering::Greater,
    };
    let _ = fs::write(&record, version);
    updated.then(|| version.to_string())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepResult {
    pub files: u64,
    pub bytes: u64,
}

/// Removes attachments no stored message names, with their context sidecars, once they are older than
/// anything still in flight could be. A sidecar whose image is gone goes the same way.
pub fn sweep_orphan_attachments(directory: &Path, referenced: &HashSet<String>, now: SystemTime, min_age: Duration) -> std::io::Result<SweepResult> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(SweepResult::default()),
        Err(error) => return Err(error),
    };
    let mut result = SweepResult::default();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let image = name.strip_suffix(CONTEXT_SUFFIX).unwrap_or(&name);
        if !crate::attachments::is_saved_attachment_name(image) || referenced.contains(image) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else { continue };
        let fresh = metadata.modified().ok().is_none_or(|modified| modified > now - min_age);
        if !metadata.is_file() || fresh {
            continue;
        }
        if fs::remove_file(entry.path()).is_ok() {
            result.files += 1;
            result.bytes += metadata.len();
        }
    }
    Ok(result)
}

/// What each stored message names, by file name, so the sweep can compare against the directory.
pub fn attachment_names<'a>(paths: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
    paths.into_iter().filter_map(|file| Path::new(file).file_name().map(|name| name.to_string_lossy().into_owned())).collect()
}

/// A reverted build once gave Codex a home under the app's data folder. Nothing reads it since, so
/// it goes wherever `discard` sends it, such as the Trash. True when there was one.
pub fn retire_legacy_codex_home(user_data: &Path, discard: impl FnOnce(&Path) -> std::io::Result<()>) -> std::io::Result<bool> {
    let legacy = user_data.join("codex");
    if !fs::metadata(&legacy).is_ok_and(|metadata| metadata.is_dir()) {
        return Ok(false);
    }
    discard(&legacy)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_is_named_once_and_a_downgrade_never() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_app_launch(dir.path(), "1.0.0"), None);
        assert_eq!(read_app_launch(dir.path(), "1.1.0").as_deref(), Some("1.1.0"));
        assert_eq!(read_app_launch(dir.path(), "1.1.0"), None);
        assert_eq!(read_app_launch(dir.path(), "1.0.9"), None);
        assert_eq!(compare_versions("0.10.27", "0.9.30"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn sweeps_only_old_unreferenced_attachments() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["kept.png", "orphan.png", "orphan.png.context.json", "notes.txt"] {
            fs::write(dir.path().join(name), "x").unwrap();
        }
        let referenced: HashSet<String> = ["kept.png".to_string()].into();
        let now = SystemTime::now() + Duration::from_secs(10);
        assert_eq!(sweep_orphan_attachments(dir.path(), &referenced, now, Duration::from_secs(60)).unwrap(), SweepResult::default());
        let swept = sweep_orphan_attachments(dir.path(), &referenced, now, Duration::from_secs(1)).unwrap();
        assert_eq!(swept, SweepResult { files: 2, bytes: 2 });
        assert!(dir.path().join("kept.png").exists() && dir.path().join("notes.txt").exists());
    }
}
