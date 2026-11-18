//! macOS.

use std::path::{Path, PathBuf};

use crate::process::launch_detached;
use crate::Platform;

pub const PLATFORM: Platform = Platform::Darwin;

/// An app started from the Dock has no terminal for the shell to take.
pub const LOGIN_SHELL_OWN_SESSION: bool = false;

pub const OSASCRIPT: Option<&str> = Some("/usr/bin/osascript");
pub const CODESIGN: Option<&str> = Some("/usr/bin/codesign");
pub const DITTO: Option<&str> = Some("/usr/bin/ditto");
pub const PBCOPY: Option<&str> = Some("/usr/bin/pbcopy");
pub const DEFAULTS: Option<&str> = Some("/usr/bin/defaults");
pub const SOUND_PLAYER: Option<&str> = Some("/usr/bin/afplay");

pub fn app_data_dir(home: Option<&Path>) -> Option<PathBuf> {
    Some(home?.join("Library").join("Application Support"))
}

pub fn open(target: &str) -> bool {
    launch_detached("open", &[target.to_string()])
}

pub fn reveal(path: &Path) -> bool {
    launch_detached("open", &["-R".into(), path.to_string_lossy().into_owned()])
}

pub fn sleep_blocker(pid: u32, _who: &str, _why: &str) -> std::process::Command {
    let mut command = crate::process::std_command("caffeinate");
    command.args(["-i", "-w", &pid.to_string()]);
    command
}

pub fn available_memory() -> Option<u64> {
    let output = crate::process::std_command("/usr/bin/vm_stat").env("LC_ALL", "C").stdin(std::process::Stdio::null()).output().ok()?;
    output.status.success().then(|| available_memory_from_vm_stat(&String::from_utf8_lossy(&output.stdout))).flatten()
}

/// Free, inactive and speculative pages, which the system hands out without swapping.
fn available_memory_from_vm_stat(output: &str) -> Option<u64> {
    let page = output.split("page size of ").nth(1).and_then(|rest| rest.split_whitespace().next()).and_then(|size| size.parse::<u64>().ok())?;
    let mut pages = 0;
    for name in ["free", "inactive", "speculative"] {
        let prefix = format!("Pages {name}:");
        pages += output.lines().find_map(|line| line.strip_prefix(&prefix)).and_then(|rest| rest.trim().strip_suffix('.')).and_then(|count| count.trim().parse::<u64>().ok())?;
    }
    Some(pages * page)
}

pub fn total_memory() -> Option<u64> {
    let mut size: u64 = 0;
    let mut length = std::mem::size_of::<u64>();
    let name = c"hw.memsize";
    // SAFETY: hw.memsize is a u64 and `length` names its size.
    let status = unsafe { libc::sysctlbyname(name.as_ptr(), (&mut size as *mut u64).cast(), &mut length, std::ptr::null_mut(), 0) };
    (status == 0).then_some(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_memory_counts_reclaimable_pages() {
        let output = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:  10.\nPages active: 5.\nPages inactive: 2.\nPages speculative: 1.\n";
        assert_eq!(available_memory_from_vm_stat(output), Some(13 * 16384));
    }
}
