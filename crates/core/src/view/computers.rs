//! Which paired computers the sidebar draws, and which holds a thread, project or checkout.

use crate::domain::platform::ComputerStatus;
use crate::state::{PairedComputer, WorkspaceState};
use crate::view::sidebar::ThreadHost;

/// The paired computers the sidebar's computer filter shows.
pub fn shown_computers(state: &WorkspaceState) -> Vec<&PairedComputer> {
    match state.computers.filter.as_str() {
        "this" => Vec::new(),
        "all" => state.computers.paired.iter().collect(),
        id => state.computers.paired.iter().filter(|computer| computer.link.id == id).collect(),
    }
}

fn holder_of<'a>(state: &'a WorkspaceState, holds: impl Fn(&WorkspaceState) -> bool) -> Option<&'a PairedComputer> {
    if holds(state) {
        return None;
    }
    state.computers.paired.iter().find(|computer| computer.state.as_deref().is_some_and(&holds))
}

/// The paired computer holding a thread, or None for one of this computer's own or one nobody holds.
pub fn computer_of_thread<'a>(state: &'a WorkspaceState, task_id: Option<&str>) -> Option<&'a PairedComputer> {
    let task_id = task_id?;
    holder_of(state, |held| held.threads.iter().any(|thread| thread.id == task_id))
}

pub fn computer_of_project<'a>(state: &'a WorkspaceState, project_id: Option<&str>) -> Option<&'a PairedComputer> {
    let project_id = project_id?;
    holder_of(state, |held| held.projects.iter().any(|project| project.id == project_id))
}

/// The paired computer whose project or checkout a workspace id names.
pub fn computer_of_workspace<'a>(state: &'a WorkspaceState, workspace_id: &str) -> Option<&'a PairedComputer> {
    holder_of(state, |held| held.projects.iter().any(|project| project.workspace_id.as_deref() == Some(workspace_id)) || held.worktrees.iter().any(|worktree| worktree.workspace_id == workspace_id))
}

/// How a paired computer is named on what it holds, and whether its line is up.
pub fn thread_host_of(computer: &PairedComputer) -> ThreadHost {
    ThreadHost { id: computer.link.id.clone(), name: computer.link.name.clone(), offline: computer.link.status != ComputerStatus::Connected, reconnecting: computer.link.status == ComputerStatus::Connecting }
}
