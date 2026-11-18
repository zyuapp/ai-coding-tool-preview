//! What macOS and Linux share.

use std::fs::{Metadata, OpenOptions, Permissions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;

use crate::process::Signal;

pub use std::os::unix::net::{UnixListener as Listener, UnixStream as Stream};
pub use tokio::net::{UnixListener as AsyncListener, UnixStream as AsyncStream};

fn number(signal: Signal) -> libc::c_int {
    match signal {
        Signal::Interrupt => libc::SIGINT,
        Signal::Terminate => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    }
}

pub fn signal(pid: u32, signal: Signal) -> bool {
    // SAFETY: kill(2) takes any pid and signal number.
    unsafe { libc::kill(pid as libc::pid_t, number(signal)) == 0 }
}

pub fn signal_group(pid: u32, signal: Signal) -> bool {
    // SAFETY: a negative pid names the process group it leads.
    unsafe { libc::kill(-(pid as libc::pid_t), number(signal)) == 0 }
}

pub fn alive(pid: u32) -> bool {
    // Signal 0 checks the process exists; EPERM means it does, under another user.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    status.signal()
}

pub fn signal_name(signal: i32) -> Option<&'static str> {
    let names = [
        (libc::SIGHUP, "SIGHUP"),
        (libc::SIGINT, "SIGINT"),
        (libc::SIGQUIT, "SIGQUIT"),
        (libc::SIGILL, "SIGILL"),
        (libc::SIGTRAP, "SIGTRAP"),
        (libc::SIGABRT, "SIGABRT"),
        (libc::SIGBUS, "SIGBUS"),
        (libc::SIGFPE, "SIGFPE"),
        (libc::SIGKILL, "SIGKILL"),
        (libc::SIGUSR1, "SIGUSR1"),
        (libc::SIGSEGV, "SIGSEGV"),
        (libc::SIGUSR2, "SIGUSR2"),
        (libc::SIGPIPE, "SIGPIPE"),
        (libc::SIGALRM, "SIGALRM"),
        (libc::SIGTERM, "SIGTERM"),
        (libc::SIGXCPU, "SIGXCPU"),
        (libc::SIGXFSZ, "SIGXFSZ"),
    ];
    names.iter().find(|(number, _)| *number == signal).map(|(_, name)| *name)
}

pub fn terminal_shell() -> (String, &'static [&'static str]) {
    let shell = std::env::var("SHELL").ok().filter(|shell| !shell.is_empty()).unwrap_or_else(|| "/bin/bash".into());
    (shell, &["-l"])
}

pub fn own_group(command: &mut std::process::Command) {
    command.process_group(0);
}

pub fn new_session(command: &mut std::process::Command) {
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

pub fn hostname() -> String {
    let mut buffer = [0u8; 256];
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len()) };
    if result != 0 {
        return String::new();
    }
    let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

pub fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

pub fn is_executable(metadata: &Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

pub fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    std::fs::set_permissions(path, Permissions::from_mode(mode))
}

pub fn create_mode(options: &mut OpenOptions, mode: u32) {
    options.mode(mode);
}

pub fn create_mode_async(options: &mut tokio::fs::OpenOptions, mode: u32) {
    options.mode(mode);
}

pub fn create_dir_all_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(mode).create(path)
}

pub fn owned_by_current_user(metadata: &Metadata) -> bool {
    // SAFETY: getuid cannot fail.
    metadata.uid() == unsafe { libc::getuid() }
}

pub fn file_identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}
