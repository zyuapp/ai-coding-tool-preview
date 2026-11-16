//! The checkout a run happens in: made ready before the run starts, or why it could not be.

use std::future::Future;
use std::path::PathBuf;

use act_core::domain::platform::{WorkspaceKind, WorkspaceRecord};
use act_core::domain::worktree::CreatedWorktree;
use act_core::effects::{BranchSpec, CreateWorktreeSpec};
use act_core::reducer::errors::{REOPEN_PROJECT_ERROR, SAME_PROJECT_ERROR};
use act_core::WorkspaceEvent;

use crate::git;
use crate::services::HostServices;
use crate::worktrees::CreateWorktreeRequest;

/// The `ResolveRunWorkspace` effect's fields.
#[derive(Debug, Clone)]
pub struct RunWorkspaceRequest {
    pub pending_id: String,
    pub picker: bool,
    pub workspace: Option<WorkspaceRecord>,
    pub root: Option<String>,
    pub create_worktree: Option<CreateWorktreeSpec>,
    pub create_branch: Option<BranchSpec>,
    pub checkout: Option<BranchSpec>,
}

fn convert<T: serde::de::DeserializeOwned>(value: impl serde::Serialize) -> Result<T, String> {
    serde_json::to_value(value).and_then(serde_json::from_value).map_err(|error| error.to_string())
}

/// Git says what went wrong but not what was being attempted, so each step names itself.
async fn step<T, E: std::fmt::Display>(attempt: &str, work: impl Future<Output = Result<T, E>>) -> Result<T, String> {
    work.await.map_err(|error| format!("Could not {attempt}: {error}"))
}

async fn available_root(services: &HostServices, workspace_id: &str) -> Result<PathBuf, String> {
    services.workspaces.available_root(workspace_id).await.map_err(|error| error.to_string())
}

/// Turns a resolution request into the event that answers it. A workspace the reducer already named
/// is handed straight back, so nothing here decides what kind of checkout a run happens in.
/// `pick_folder` asks the user for a project folder and registers it, or answers None when cancelled.
pub async fn resolve_run_workspace<Pick, Picked>(request: RunWorkspaceRequest, services: &HostServices, pick_folder: Pick) -> WorkspaceEvent
where
    Pick: FnOnce() -> Picked,
    Picked: Future<Output = Result<Option<WorkspaceRecord>, String>>,
{
    let pending_id = request.pending_id.clone();
    match resolve(request, services, pick_folder).await {
        Ok(event) => event,
        Err(message) => WorkspaceEvent::RunUnresolved { pending_id, message },
    }
}

async fn resolve<Pick, Picked>(request: RunWorkspaceRequest, services: &HostServices, pick_folder: Pick) -> Result<WorkspaceEvent, String>
where
    Pick: FnOnce() -> Picked,
    Picked: Future<Output = Result<Option<WorkspaceRecord>, String>>,
{
    let pending_id = request.pending_id;
    if let Some(BranchSpec { workspace_id, branch }) = &request.create_branch {
        step(&format!("create the branch {branch}"), async { git::create_branch(&available_root(services, workspace_id).await?, branch).await.map_err(|error| error.to_string()) }).await?;
    }
    if let Some(BranchSpec { workspace_id, branch }) = &request.checkout {
        step(&format!("check out {branch}"), async { git::checkout_branch(&available_root(services, workspace_id).await?, branch).await.map_err(|error| error.to_string()) }).await?;
    }
    if let Some(making) = request.create_worktree {
        let created = step("create the worktree", services.worktrees.create(&CreateWorktreeRequest { project_root: making.project_root, carry_changes: making.carry_changes, branch: making.branch })).await?;
        let worktree: CreatedWorktree = convert(&created)?;
        let workspace = WorkspaceRecord { id: worktree.workspace_id.clone(), kind: WorkspaceKind::Worktree, root: worktree.root.clone() };
        return Ok(WorkspaceEvent::RunResolved { pending_id, workspace, worktree: Some(worktree) });
    }
    if let Some(workspace) = request.workspace {
        return Ok(WorkspaceEvent::RunResolved { pending_id, workspace, worktree: None });
    }
    if !request.picker {
        let projectless = services.workspaces.projectless().await.map_err(|error| error.to_string())?;
        return Ok(WorkspaceEvent::RunResolved { pending_id, workspace: convert(projectless)?, worktree: None });
    }
    let Some(selected) = pick_folder().await? else {
        return Ok(WorkspaceEvent::RunUnresolved { pending_id, message: REOPEN_PROJECT_ERROR.into() });
    };
    if request.root.as_deref() != Some(selected.root.as_str()) {
        return Ok(WorkspaceEvent::RunUnresolved { pending_id, message: SAME_PROJECT_ERROR.into() });
    }
    Ok(WorkspaceEvent::RunResolved { pending_id, workspace: selected, worktree: None })
}
