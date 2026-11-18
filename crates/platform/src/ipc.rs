//! A socket only processes on this machine reach, named by a path: how a later launch hands its
//! arguments to the first, and how `aic pair` reaches a headless host.

use std::path::Path;

pub use crate::sys::{AsyncListener, AsyncStream, Listener, Stream};

pub fn bind(path: &Path) -> std::io::Result<Listener> {
    Listener::bind(path)
}

pub fn connect(path: &Path) -> std::io::Result<Stream> {
    Stream::connect(path)
}

pub fn bind_async(path: &Path) -> std::io::Result<AsyncListener> {
    AsyncListener::bind(path)
}

pub async fn connect_async(path: &Path) -> std::io::Result<AsyncStream> {
    AsyncStream::connect(path).await
}

/// This machine's name, as the profile lock records it.
pub fn hostname() -> String {
    crate::sys::hostname()
}
