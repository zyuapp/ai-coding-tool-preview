//! Where the native helpers are: beside a packaged app's executable, else where this build put them.

use std::path::{Path, PathBuf};

/// Overrides every other place, for a build that keeps its helpers elsewhere.
pub const NATIVE_DIR_ENV: &str = "ACT_NATIVE_DIR";

/// `<workspace>/target/native`, where `scripts/build-dictation-helper.sh` writes.
pub fn workspace_native_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/native")
}

/// A packaged app keeps its helpers in `Contents/Resources/native`, a loose build beside its binary.
fn packaged_dirs() -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else { return Vec::new() };
    let Some(dir) = exe.parent() else { return Vec::new() };
    vec![dir.join("../Resources/native"), dir.join("native"), dir.to_path_buf()]
}

/// The first place that has `name`, or the place it would be expected.
pub fn helper(name: &str, built: Option<&str>) -> PathBuf {
    if let Some(dir) = std::env::var_os(NATIVE_DIR_ENV) {
        return PathBuf::from(dir).join(name);
    }
    let mut candidates = packaged_dirs().into_iter().map(|dir| dir.join(name)).collect::<Vec<_>>();
    if let Some(built) = built {
        candidates.push(PathBuf::from(built));
    }
    candidates.push(workspace_native_dir().join(name));
    // A path through `..` crashes a Swift helper that finds its resources beside itself, so it is resolved.
    match candidates.iter().find(|path| path.is_file()) {
        Some(found) => std::fs::canonicalize(found).unwrap_or_else(|_| found.clone()),
        None => candidates.last().cloned().unwrap_or_else(|| PathBuf::from(name)),
    }
}

/// Whether this binary runs from inside an app bundle.
pub fn packaged() -> bool {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("../Info.plist").is_file())).unwrap_or(false)
}
