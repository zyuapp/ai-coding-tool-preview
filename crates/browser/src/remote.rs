//! Reading the panel's pages from any thread. WebKit only runs on the main thread, so each read is
//! sent there and its answer sent back.

use act_core::domain::platform::BrowserPermissions;
use act_host::tools::types::{BrowserReadResult, BrowserShot, BrowserSnapshot};
use futures::channel::{mpsc, oneshot};

use crate::types::Inspection;
use crate::Pages;

type Reply<T> = oneshot::Sender<Result<T, String>>;

pub(crate) enum RemoteCall {
    Configure { permissions: BrowserPermissions, reply: Reply<()> },
    Read { tab_id: String, text_limit: f64, timeout_ms: u64, task_id: Option<String>, reply: Reply<Option<BrowserSnapshot>> },
    Inspect { tab_id: String, inspection: Inspection, task_id: Option<String>, reply: Reply<Option<BrowserReadResult>> },
    Capture { tab_id: String, full_page: bool, timeout_ms: u64, task_id: Option<String>, reply: Reply<Option<BrowserShot>> },
}

const UNAVAILABLE: &str = "The browser panel needs the desktop app.";
const GONE: &str = "The browser panel closed.";

impl RemoteCall {
    pub(crate) fn unavailable(self) {
        match self {
            RemoteCall::Configure { reply, .. } => drop(reply.send(Ok(()))),
            RemoteCall::Read { reply, .. } => drop(reply.send(Ok(None))),
            RemoteCall::Inspect { reply, .. } => drop(reply.send(Ok(None))),
            RemoteCall::Capture { reply, .. } => drop(reply.send(Err(UNAVAILABLE.into()))),
        }
    }

    pub(crate) async fn run(self, pages: &Pages) {
        match self {
            RemoteCall::Configure { permissions, reply } => {
                pages.configure_permissions(&permissions);
                let _ = reply.send(Ok(()));
            }
            RemoteCall::Read { tab_id, text_limit, timeout_ms, task_id, reply } => {
                let _ = reply.send(pages.read_page(&tab_id, text_limit, timeout_ms, task_id.as_deref()).await);
            }
            RemoteCall::Inspect { tab_id, inspection, task_id, reply } => {
                let _ = reply.send(pages.inspect_page(&tab_id, inspection, task_id.as_deref()).await);
            }
            RemoteCall::Capture { tab_id, full_page, timeout_ms, task_id, reply } => {
                let _ = reply.send(pages.capture_page(&tab_id, full_page, timeout_ms, task_id.as_deref()).await);
            }
        }
    }
}

/// A handle to the panel's pages that any thread may hold. Every read is made as `task_id`, the
/// run asking, so it reaches only the sites that run may.
#[derive(Clone)]
pub struct BrowserRemote {
    calls: mpsc::UnboundedSender<RemoteCall>,
}

async fn ask<T>(calls: &mpsc::UnboundedSender<RemoteCall>, call: RemoteCall, answer: oneshot::Receiver<Result<T, String>>) -> Result<T, String> {
    calls.unbounded_send(call).map_err(|_| GONE.to_string())?;
    answer.await.map_err(|_| GONE.to_string())?
}

impl BrowserRemote {
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<RemoteCall>) {
        let (calls, receiver) = mpsc::unbounded();
        (Self { calls }, receiver)
    }

    pub async fn configure_permissions(&self, permissions: BrowserPermissions) -> Result<(), String> {
        let (reply, answer) = oneshot::channel();
        ask(&self.calls, RemoteCall::Configure { permissions, reply }, answer).await
    }

    /// Waits for the tab to stop loading, then reads the page. None when that tab is gone.
    pub async fn read_page(&self, tab_id: String, text_limit: f64, timeout_ms: u64, task_id: Option<String>) -> Result<Option<BrowserSnapshot>, String> {
        let (reply, answer) = oneshot::channel();
        ask(&self.calls, RemoteCall::Read { tab_id, text_limit, timeout_ms, task_id, reply }, answer).await
    }

    /// Reads recent diagnostics or waits for a condition in one page.
    pub async fn inspect_page(&self, tab_id: String, inspection: Inspection, task_id: Option<String>) -> Result<Option<BrowserReadResult>, String> {
        let (reply, answer) = oneshot::channel();
        ask(&self.calls, RemoteCall::Inspect { tab_id, inspection, task_id, reply }, answer).await
    }

    /// Waits the same way, then writes a picture of the page to a file. None when that tab is gone.
    pub async fn capture_page(&self, tab_id: String, full_page: bool, timeout_ms: u64, task_id: Option<String>) -> Result<Option<BrowserShot>, String> {
        let (reply, answer) = oneshot::channel();
        ask(&self.calls, RemoteCall::Capture { tab_id, full_page, timeout_ms, task_id, reply }, answer).await
    }
}
