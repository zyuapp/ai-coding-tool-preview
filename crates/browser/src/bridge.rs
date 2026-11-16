//! The browser tools' view of the panel, as one run drives it: writes go through the workspace as
//! that run's commands, and reads come from the pages, limited to what that run may see.

use std::sync::Arc;

use act_core::domain::platform::{BrowserPermissions, BrowserTab};
use act_host::tools::browser::BrowserBridge;
use act_host::tools::types::{BrowserRead, BrowserReadResult, BrowserTab as ToolTab, BrowserWrite};
use futures::future::BoxFuture;
use futures::FutureExt;

use crate::remote::BrowserRemote;
use crate::types::Inspection;

const DEFAULT_PAGE_TEXT: f64 = 4_000.0;

/// What a run sees of the browser.
pub enum BrowserScope {
    /// The run has a navigation waiting on the user, so every read answers that instead.
    AwaitingApproval { url: String },
    /// The run's own thread's dock: its tabs, the tab a read reaches, and what runs may browse.
    Dock { tabs: Vec<BrowserTab>, target: Option<String>, permissions: BrowserPermissions },
}

/// The workspace side of the browser tools, answered from workspace state.
pub trait BrowserWorkspace: Send + Sync {
    /// Runs a `browser.*` command, which already names the run, and answers the panel's own error.
    fn execute(&self, command: serde_json::Value) -> BoxFuture<'_, Result<(), String>>;
    /// The dock `task_id` reaches, whichever dock the user has on screen, and the tab `tab_id`
    /// names there, else the one that dock shows.
    fn scope(&self, task_id: &str, tab_id: Option<&str>) -> BoxFuture<'_, BrowserScope>;
}

/// The browser tools' bridge for one run.
pub struct PanelBridge {
    task_id: String,
    workspace: Arc<dyn BrowserWorkspace>,
    remote: BrowserRemote,
}

impl PanelBridge {
    pub fn new(task_id: impl Into<String>, workspace: Arc<dyn BrowserWorkspace>, remote: BrowserRemote) -> Self {
        Self { task_id: task_id.into(), workspace, remote }
    }
}

fn tool_tab(tab: BrowserTab) -> ToolTab {
    ToolTab { id: tab.id, url: tab.url, title: tab.title, loading: tab.loading, can_go_back: tab.can_go_back, can_go_forward: tab.can_go_forward, error: tab.error }
}

fn named_tab(read: &BrowserRead) -> Option<&str> {
    match read {
        BrowserRead::Tabs => None,
        BrowserRead::Snapshot { tab_id, .. }
        | BrowserRead::Screenshot { tab_id, .. }
        | BrowserRead::Console { tab_id, .. }
        | BrowserRead::Network { tab_id, .. }
        | BrowserRead::Wait { tab_id, .. } => tab_id.as_deref().filter(|tab| !tab.is_empty()),
    }
}

impl BrowserBridge for PanelBridge {
    fn command(&self, write: BrowserWrite) -> BoxFuture<'_, Result<(), String>> {
        let mut command = write;
        if let Some(fields) = command.as_object_mut() {
            fields.insert("taskId".into(), serde_json::Value::String(self.task_id.clone()));
        }
        self.workspace.execute(command)
    }

    fn read(&self, read: BrowserRead) -> BoxFuture<'_, Result<BrowserReadResult, String>> {
        async move {
            let scope = self.workspace.scope(&self.task_id, named_tab(&read)).await;
            let (tabs, target, permissions) = match scope {
                BrowserScope::AwaitingApproval { url } => return Ok(BrowserReadResult::AwaitingApproval { url }),
                BrowserScope::Dock { tabs, target, permissions } => (tabs, target, permissions),
            };
            if matches!(read, BrowserRead::Tabs) {
                return Ok(BrowserReadResult::Tabs { tabs: tabs.into_iter().map(tool_tab).collect() });
            }
            let Some(tab_id) = target else { return Ok(BrowserReadResult::NoTab) };
            self.remote.configure_permissions(permissions).await?;
            let task_id = Some(self.task_id.clone());
            let answer = match read {
                BrowserRead::Tabs => unreachable!("answered above"),
                BrowserRead::Screenshot { full_page, timeout_ms, .. } => {
                    self.remote.capture_page(tab_id, full_page == Some(true), timeout_ms, task_id).await?.map(|shot| BrowserReadResult::Shot { shot })
                }
                BrowserRead::Console { since, limit, minimum_level, .. } => self.remote.inspect_page(tab_id, Inspection::Console { since, limit, minimum_level }, task_id).await?,
                BrowserRead::Network { since, limit, failures_only, .. } => {
                    self.remote.inspect_page(tab_id, Inspection::Network { since, limit, failures_only: failures_only == Some(true) }, task_id).await?
                }
                BrowserRead::Wait { condition, value, timeout_ms, .. } => self.remote.inspect_page(tab_id, Inspection::Wait { condition, value, timeout_ms }, task_id).await?,
                BrowserRead::Snapshot { text_limit, timeout_ms, .. } => {
                    self.remote.read_page(tab_id, text_limit.unwrap_or(DEFAULT_PAGE_TEXT), timeout_ms, task_id).await?.map(|snapshot| BrowserReadResult::Snapshot { snapshot })
                }
            };
            Ok(answer.unwrap_or(BrowserReadResult::NoTab))
        }
        .boxed()
    }
}
