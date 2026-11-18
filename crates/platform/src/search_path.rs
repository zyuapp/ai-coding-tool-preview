//! The search path children get: the one the user's own shell has. macOS starts an app from the Dock
//! with four system folders and nothing else, so without it every command the app runs — `git`, `gh`,
//! and whatever an agent's shell calls — is looked for where the user installed nothing.
//!
//! The path is held here and given to each child as it starts; this process's own environment is
//! never changed.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use tokio::io::AsyncReadExt;

use crate::process::Signal;

/// Room for a shell with heavy start-up files, short enough that the window is not held back on one.
const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT: usize = 1024 * 1024;
/// Printed either side of the environment, so whatever a start-up file prints of its own is skipped.
const MARK: &str = "__aic_environment__";

/// Where a Mac keeps tools that launchd's own search path never lists, in the order a shell looks.
fn tool_folders(home: &Path) -> Vec<String> {
    vec!["/opt/homebrew/bin".into(), "/opt/homebrew/sbin".into(), "/usr/local/bin".into(), home.join(".local").join("bin").to_string_lossy().into_owned()]
}

fn installed_tool_folders() -> String {
    tool_folders(&crate::paths::home()).into_iter().filter(|folder| Path::new(folder).exists()).collect::<Vec<_>>().join(":")
}

/// Until the login shell answers: what the app was started with, then the standard tool folders.
pub(crate) fn held() -> &'static RwLock<String> {
    static HELD: OnceLock<RwLock<String>> = OnceLock::new();
    HELD.get_or_init(|| {
        let inherited = std::env::var("PATH").ok();
        RwLock::new(merge_search_paths([inherited.as_deref(), Some(installed_tool_folders().as_str())]))
    })
}

/// The search path children get.
pub fn get() -> String {
    held().read().unwrap().clone()
}

/// The folders of the search path, in order.
pub fn folders() -> Vec<PathBuf> {
    get().split(':').filter(|folder| !folder.is_empty()).map(PathBuf::from).collect()
}

/// Where `command` is on the search path, as a shell finds it.
pub fn find(command: &str) -> Option<PathBuf> {
    folders().into_iter().map(|folder| folder.join(command)).find(|candidate| crate::fs::is_executable(candidate))
}

/// The search path a shell reported, read off the `PATH=` line of the environment between the marks.
pub fn search_path_from_environment(output: &str) -> Option<String> {
    let start = output.find(MARK)? + MARK.len();
    let end = start + output[start..].find(MARK)?;
    output[start..end].split('\n').find_map(|line| line.strip_prefix("PATH=")).map(str::trim).filter(|path| !path.is_empty()).map(String::from)
}

/// One search path out of several, first mention winning, so the order the shell gave is the order kept.
pub fn merge_search_paths<'a>(paths: impl IntoIterator<Item = Option<&'a str>>) -> String {
    let mut folders: Vec<&str> = Vec::new();
    for value in paths.into_iter().flatten() {
        for folder in value.split(':') {
            if !folder.is_empty() && !folders.contains(&folder) {
                folders.push(folder);
            }
        }
    }
    folders.join(":")
}

/// Login and interactive, because a search path can be written in the files only one reads. A
/// start-up file that fails afterwards has already printed the environment, and the marks find it.
async fn login_shell_environment(shell: &str) -> String {
    let script = format!("printf '{MARK}'; /usr/bin/env; printf '{MARK}'");
    let mut command = crate::process::command(shell);
    command.args(["-ilc", &script]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let own_session = crate::os::LOGIN_SHELL_OWN_SESSION;
    if own_session {
        crate::sys::new_session(command.as_std_mut());
    }
    let Ok(mut child) = command.spawn() else { return String::new() };
    let Some(mut stdout) = child.stdout.take() else { return String::new() };
    let mut output = Vec::new();
    let read = async {
        let mut chunk = [0u8; 8192];
        loop {
            match stdout.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let room = MAX_OUTPUT.saturating_sub(output.len());
                    output.extend_from_slice(&chunk[..count.min(room)]);
                    if count > room {
                        break;
                    }
                }
            }
        }
    };
    let finished = tokio::time::timeout(TIMEOUT, read).await.is_ok();
    if !finished {
        if let Some(pid) = child.id().filter(|_| own_session) {
            crate::process::signal_group(pid, Signal::Kill);
        }
        let _ = child.start_kill();
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
    String::from_utf8_lossy(&output).into_owned()
}

/// The search path to hold: the shell's answer leads, the one held so far follows, and the standard
/// folders last.
pub async fn login_shell_path() -> String {
    let shell = std::env::var("SHELL").ok().filter(|shell| !shell.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let reported = search_path_from_environment(&login_shell_environment(&shell).await);
    let current = get();
    merge_search_paths([reported.as_deref(), Some(current.as_str()), Some(installed_tool_folders().as_str())])
}

/// Asks the login shell and holds its answer; every child started afterwards gets it.
pub async fn adopt_login_shell_path() {
    let path = login_shell_path().await;
    *held().write().unwrap() = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_path_between_the_marks_only() {
        let output = format!("PATH=/noise\n{MARK}HOME=/h\nPATH=/a:/b \nX=1\n{MARK}PATH=/after");
        assert_eq!(search_path_from_environment(&output).as_deref(), Some("/a:/b"));
        assert_eq!(search_path_from_environment("PATH=/a"), None);
        assert_eq!(search_path_from_environment(&format!("{MARK}PATH=/a")), None);
    }

    #[test]
    fn merging_keeps_the_first_mention() {
        assert_eq!(merge_search_paths([Some("/a:/b"), None, Some("/b::/c:/a")]), "/a:/b:/c");
    }
}
