//! File operations whose modes and links differ between systems.

use std::fs::{Metadata, OpenOptions};
use std::path::Path;

const PRIVATE_FILE: u32 = 0o600;
const PRIVATE_DIR: u32 = 0o700;
const EXECUTABLE: u32 = 0o755;
const SHARED_FILE: u32 = 0o644;

/// A symbolic link at `link` naming `target`.
pub fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) -> std::io::Result<()> {
    crate::sys::symlink(target.as_ref(), link.as_ref())
}

/// A regular file anyone may run.
pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && crate::sys::is_executable(&metadata))
}

pub fn set_executable(path: &Path) -> std::io::Result<()> {
    crate::sys::set_mode(path, EXECUTABLE)
}

/// Only this user may read or write the file.
pub fn make_private_file(path: &Path) -> std::io::Result<()> {
    crate::sys::set_mode(path, PRIVATE_FILE)
}

/// Only this user may enter the folder.
pub fn make_private_dir(path: &Path) -> std::io::Result<()> {
    crate::sys::set_mode(path, PRIVATE_DIR)
}

/// A folder and its missing parents, each made private to this user.
pub fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    crate::sys::create_dir_all_mode(path, PRIVATE_DIR)
}

/// A file these options create is private to this user.
pub fn private(options: &mut OpenOptions) -> &mut OpenOptions {
    crate::sys::create_mode(options, PRIVATE_FILE);
    options
}

/// A file these options create anyone may read.
pub fn shared(options: &mut OpenOptions) -> &mut OpenOptions {
    crate::sys::create_mode(options, SHARED_FILE);
    options
}

/// A file these options create anyone may run.
pub fn executable(options: &mut tokio::fs::OpenOptions) -> &mut tokio::fs::OpenOptions {
    crate::sys::create_mode_async(options, EXECUTABLE);
    options
}

pub fn owned_by_current_user(metadata: &Metadata) -> bool {
    crate::sys::owned_by_current_user(metadata)
}

/// The device and file number that tell whether two paths are the same file.
pub fn file_identity(metadata: &Metadata) -> (u64, u64) {
    crate::sys::file_identity(metadata)
}
