//! The user's own folders.

use std::path::{Path, PathBuf};

/// The home folder, when the environment names one.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from)
}

/// The home folder, or the root when there is none.
pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// The per-user application data folder, as Electron's `appData` names it.
pub fn app_data_dir(home: Option<&Path>) -> Option<PathBuf> {
    crate::os::app_data_dir(home)
}
