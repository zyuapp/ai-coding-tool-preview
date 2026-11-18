//! Linux.

use std::path::{Path, PathBuf};

use crate::process::launch_detached;
use crate::Platform;

pub const PLATFORM: Platform = Platform::Linux;

/// An interactive shell can touch the launching terminal and stop its whole process group with
/// SIGTTIN, so it runs as a session of its own.
pub const LOGIN_SHELL_OWN_SESSION: bool = true;

pub const OSASCRIPT: Option<&str> = None;
pub const CODESIGN: Option<&str> = None;
pub const DITTO: Option<&str> = None;
pub const PBCOPY: Option<&str> = None;
pub const DEFAULTS: Option<&str> = None;
pub const SOUND_PLAYER: Option<&str> = None;

pub fn app_data_dir(home: Option<&Path>) -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(home?.join(".config")),
    }
}

pub fn open(target: &str) -> bool {
    launch_detached("xdg-open", &[target.to_string()])
}

/// File managers have no common way to select a file, so its folder opens.
pub fn reveal(path: &Path) -> bool {
    let folder = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
    open(&folder.to_string_lossy())
}

pub fn sleep_blocker(_pid: u32, who: &str, why: &str) -> std::process::Command {
    let mut command = crate::process::std_command("systemd-inhibit");
    command.args(["--what=idle:sleep", &format!("--who={who}"), &format!("--why={why}"), "--mode=block", "sleep", "infinity"]);
    command
}

fn meminfo(name: &str) -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let prefix = format!("{name}:");
    info.lines().find_map(|line| line.strip_prefix(prefix.as_str())).and_then(|rest| rest.trim().strip_suffix("kB")).and_then(|kb| kb.trim().parse::<u64>().ok()).map(|kb| kb * 1024)
}

pub fn available_memory() -> Option<u64> {
    meminfo("MemAvailable")
}

pub fn total_memory() -> Option<u64> {
    meminfo("MemTotal")
}
