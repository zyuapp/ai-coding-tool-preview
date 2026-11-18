//! What differs between operating systems: starting and stopping child processes and the search path
//! they get, the local socket a later launch reaches the first one through, private files, the user's
//! folders, memory and the process table, and the system tools the app calls.
//!
//! `sys` is what every Unix shares and `os` what one system does its own way; each public module goes
//! through them. A Windows port adds `windows.rs` as both.

pub mod fs;
pub mod ipc;
pub mod paths;
pub mod process;
pub mod search_path;
pub mod system;
pub mod tools;

#[cfg(unix)]
#[path = "unix.rs"]
mod sys;

#[cfg(target_os = "macos")]
#[path = "mac.rs"]
mod os;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod os;

/// An operating system, for catalogues that list each one's own entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Darwin,
    Windows,
    Linux,
}

impl Platform {
    /// The system this build runs on.
    pub const fn current() -> Self {
        os::PLATFORM
    }
}
