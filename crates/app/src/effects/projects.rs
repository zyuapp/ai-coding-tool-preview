//! The project's folder, its checkouts, and what Git says about them (src/host/project-effects.ts).

use std::path::Path;
use std::sync::Arc;

use act_core::domain::diff::DiffRange;
use act_core::events::WorkspaceEvent;
use act_host::workspaces::WorkspaceRecord;
use act_host::worktrees::{CreateWorktreeRequest, ReleaseWorktreeRequest, WorktreeRelease};
use serde_json::json;

use super::{convert, Host};

/// A folder the user named, refused when it lies under a checkout the app keeps, then registered.
pub async fn register_folder(host: &Host, root: &str) -> Result<WorkspaceRecord, String> {
    let folder = act_host::files::project_folder(root, &host.services.worktrees_roots()).await.map_err(|error| error.to_string())?;
    host.services.workspaces.register_project(&folder).await.map_err(|error| error.to_string())
}

pub async fn add_project(host: &Arc<Host>, root: String, request: Option<i64>, start: bool) {
    match register_folder(host, &root).await {
        Ok(workspace) => host.sink.json(json!({ "type": "project.added", "workspace": workspace, "request": request, "start": start })),
        Err(error) => host.sink.event(WorkspaceEvent::ProjectAddFinished { request, error: Some(error) }),
    }
}

pub async fn register_project(host: &Arc<Host>, project_id: String, root: String) {
    match register_folder(host, &root).await {
        Ok(workspace) => host.sink.json(json!({ "type": "project.registered", "projectId": project_id, "workspace": workspace })),
        Err(message) => host.sink.event(WorkspaceEvent::ProjectRegisterFailed { project_id, message }),
    }
}

/// A folder picked in the system's panel, which every way of adding a project ends in.
pub async fn opened_folder(host: &Arc<Host>, root: String) {
    match register_folder(host, &root).await {
        Ok(workspace) => host.sink.json(json!({ "type": "project.opened", "workspace": workspace })),
        Err(error) => host.sink.failed(error),
    }
}

pub async fn list_directories(host: &Arc<Host>, prefix: String, request: i64) {
    match act_host::files::list_directories(&prefix).await {
        Ok(directories) => host.sink.event(WorkspaceEvent::ProjectDirectories { request, directories, error: None }),
        Err(error) => host.sink.event(WorkspaceEvent::ProjectDirectories { request, directories: Vec::new(), error: Some(error.to_string()) }),
    }
}

pub async fn create_worktree(host: &Arc<Host>, task_id: String, project_root: String, moving: Option<bool>, name: Option<String>, project_id: Option<String>, then: Option<String>) {
    let request = CreateWorktreeRequest { project_root, carry_changes: !moving.unwrap_or(false), branch: None };
    match host.services.worktrees.create(&request).await {
        Ok(created) => {
            let mut worktree = serde_json::to_value(&created).unwrap_or_default();
            if let Some(name) = name {
                worktree["name"] = json!(name);
            }
            host.sink.json(json!({ "type": "worktree.created", "taskId": task_id, "worktree": worktree, "move": moving, "projectId": project_id, "then": then }));
        }
        Err(error) => host.sink.event(WorkspaceEvent::WorktreeFailed { task_id, message: format!("Could not create the worktree: {error}") }),
    }
}

pub async fn release_worktree(host: &Arc<Host>, task_id: String, worktree_id: String, root: String, title: String) {
    let request = ReleaseWorktreeRequest { worktree_id, root, task_id: Some(task_id.clone()), title, release: WorktreeRelease::ReturnedToLocal, missing_only: None };
    match host.services.worktrees.release(&request).await {
        Ok(snapshot) => host.sink.json(json!({ "type": "worktree.released", "taskId": task_id, "snapshot": snapshot })),
        Err(error) => host.sink.event(WorkspaceEvent::WorktreeReleaseFailed { task_id, message: error.to_string() }),
    }
}

pub async fn delete_worktree(host: &Arc<Host>, worktree_id: String, root: String, title: String, missing_only: Option<bool>) {
    let request = ReleaseWorktreeRequest { worktree_id: worktree_id.clone(), root: root.clone(), task_id: None, title, release: WorktreeRelease::Deleted, missing_only };
    match host.services.worktrees.release(&request).await {
        Ok(snapshot) => host.sink.json(json!({ "type": "worktree.deleted", "worktreeId": worktree_id, "root": root, "snapshot": snapshot, "missingOnly": missing_only })),
        Err(error) => host.sink.event(WorkspaceEvent::WorktreesFailed { message: error.to_string(), root: Some(root) }),
    }
}

pub async fn list_worktrees(host: &Arc<Host>) {
    match host.services.worktrees.list().await {
        Ok(worktrees) => host.sink.json(json!({ "type": "worktrees.loaded", "worktrees": worktrees })),
        Err(error) => host.sink.event(WorkspaceEvent::WorktreesFailed { message: error.to_string(), root: None }),
    }
}

pub async fn reveal_worktree(host: &Arc<Host>, root: String) {
    match host.services.worktrees.owned_path(Path::new(&root)).await {
        Ok(path) => {
            act_host::open::reveal_in_file_manager(&path);
        }
        Err(error) => host.sink.event(WorkspaceEvent::WorktreesFailed { message: error.to_string(), root: None }),
    }
}

pub async fn checkout_branch(host: &Arc<Host>, workspace_id: String, branch: String, create: Option<bool>) {
    let outcome: Result<(), String> = async {
        let root = host.services.workspaces.available_root(&workspace_id).await.map_err(|error| error.to_string())?;
        if create.unwrap_or(false) {
            act_host::git::create_branch(&root, &branch).await.map_err(|error| error.to_string())?;
        }
        act_host::git::checkout_branch(&root, &branch).await.map_err(|error| error.to_string())
    }
    .await;
    host.sink.event(WorkspaceEvent::CheckoutFinished { message: outcome.err() });
}

pub async fn read_diff(host: &Arc<Host>, owner: String, workspace_id: String, range: DiffRange, ignore_whitespace: bool) {
    let result = match convert::<_, act_host::git::diff::DiffRange>(&range) {
        Ok(host_range) => serde_json::to_value(act_host::git::diff::diff_summary(&workspace_id, &host_range, &host.services.workspaces, ignore_whitespace).await).unwrap_or_default(),
        Err(message) => json!({ "status": "error", "message": message }),
    };
    host.sink.json(json!({ "type": "diff.loaded", "owner": owner, "workspaceId": workspace_id, "range": range, "result": result }));
}

/// A checkout GitHub cannot be asked about keeps the answer it has; the next ask tries again.
pub async fn read_pull_request(host: &Arc<Host>, workspace_id: String, branch: Option<String>, read: i64, members: bool) {
    let Ok(root) = host.services.workspaces.available_root(&workspace_id).await else { return };
    let answer = act_host::git::github::pull_request_for(&root).await;
    host.sink.json(json!({ "type": "pull-request.answered", "workspaceId": workspace_id, "branch": branch, "read": read, "members": members, "answer": answer }));
}
