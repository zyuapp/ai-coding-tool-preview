//! The system's own tools, each named by what it does. A tool a system lacks answers
//! [`RunError::NotFound`].

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::process::{std_command, Detach, Exec, Output, RunError};

fn tool(program: Option<&'static str>, name: &str) -> Result<&'static str, RunError> {
    program.ok_or_else(|| RunError::NotFound { program: name.into() })
}

/// Opens a URL or path with the desktop's own handler.
pub fn open(target: &str) -> bool {
    crate::os::open(target)
}

/// Shows a path in the file manager.
pub fn reveal(path: &Path) -> bool {
    crate::os::reveal(path)
}

/// A process that holds off idle sleep while it runs, for this process's `pid`; `who` and `why` are
/// what the system shows for it.
pub fn sleep_blocker(pid: u32, who: &str, why: &str) -> std::process::Command {
    crate::os::sleep_blocker(pid, who, why)
}

/// Plays a sound file without waiting for it. Does nothing where the system has no player.
pub fn play_sound(file: &Path) {
    let Some(player) = crate::os::SOUND_PLAYER else { return };
    let mut command = std_command(player);
    command.arg(file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).own_group();
    let _ = command.spawn();
}

/// Runs a JavaScript for Automation script and returns what it printed.
pub async fn run_automation_script(script: &str, timeout: Duration) -> Result<Output, RunError> {
    Exec::new(tool(crate::os::OSASCRIPT, "osascript")?).args(["-l", "JavaScript", "-e", script]).timeout(timeout).run().await
}

/// Runs a shell command as an administrator, after the system asks for the password.
pub async fn run_as_administrator(shell_command: &str) -> Result<Output, RunError> {
    let script = format!("do shell script {} with administrator privileges", serde_json::to_string(shell_command).unwrap_or_default());
    Exec::new(tool(crate::os::OSASCRIPT, "osascript")?).args(["-e", script.as_str()]).run().await
}

/// What the code signature of an app bundle says, its designated requirement among it.
pub async fn signature_details(bundle: &Path) -> Result<Output, RunError> {
    Exec::new(tool(crate::os::CODESIGN, "codesign")?).args([OsStr::new("-d"), OsStr::new("-r-"), bundle.as_os_str()]).run().await
}

/// Checks a bundle's whole signature against a requirement.
pub async fn verify_signature(bundle: &Path, requirement: &str) -> Result<Output, RunError> {
    Exec::new(tool(crate::os::CODESIGN, "codesign")?).args([OsStr::new("--verify"), OsStr::new("--deep"), OsStr::new("--strict"), OsStr::new("-R"), OsStr::new(requirement), bundle.as_os_str()]).run().await
}

/// Unpacks a zip archive into a folder, keeping what the archive records about each file.
pub async fn extract_zip(archive: &Path, into: &Path) -> Result<Output, RunError> {
    Exec::new(tool(crate::os::DITTO, "ditto")?).args([OsStr::new("-x"), OsStr::new("-k"), archive.as_os_str(), into.as_os_str()]).run().await
}

/// Puts text on the system clipboard.
pub async fn copy_text(text: &str, timeout: Duration) -> Result<Output, RunError> {
    Exec::new(tool(crate::os::PBCOPY, "pbcopy")?).env("LANG", "en_US.UTF-8").input(Some(text.as_bytes())).timeout(timeout).run().await
}

/// The identifier an app bundle's `Info.plist` gives it.
pub fn bundle_identifier(plist: &Path) -> Option<String> {
    let output = std_command(crate::os::DEFAULTS?).arg("read").arg(plist).arg("CFBundleIdentifier").stdin(Stdio::null()).output().ok()?;
    let identifier = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!identifier.is_empty()).then_some(identifier)
}
