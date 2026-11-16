//! Where a thread's runs happen: its project, its checkout, and the moves between them.

use std::collections::HashSet;

use crate::domain::project::{find_project, Project};
use crate::domain::thread::Thread;
use crate::domain::worktree::Worktree;
use crate::reducer::errors::{WORKTREE_ELSEWHERE_ERROR, WORKTREE_MISSING_ERROR};
use crate::state::WorkspaceState;

#[derive(Clone, Debug, PartialEq)]
pub enum ThreadLocation {
    Local,
    Creating,
    Releasing,
    /// `threads` counts everyone in the checkout, which says whether leaving it takes it away.
    Worktree { worktree: Worktree, threads: usize },
}

pub fn project_for<'a>(state: &'a WorkspaceState, thread: Option<&Thread>) -> Option<&'a Project> {
    let id = thread?.project_id.as_deref()?;
    state.project(id)
}

pub fn worktree_by_id<'a>(state: &'a WorkspaceState, worktree_id: Option<&str>) -> Option<&'a Worktree> {
    state.worktree(worktree_id?)
}

/// The project and checkout a send names. A project that disagrees with the checkout is refused.
pub fn named_place<'a>(state: &'a WorkspaceState, project: Option<&str>, worktree_id: Option<&str>) -> Result<(Option<&'a Project>, Option<&'a Worktree>), String> {
    let worktree = worktree_by_id(state, worktree_id);
    if worktree_id.is_some() && worktree.is_none() {
        return Err(WORKTREE_MISSING_ERROR.into());
    }
    let named = match project {
        None => None,
        Some(reference) => Some(find_project(&state.projects, reference)?),
    };
    if let (Some(worktree), Some(named)) = (worktree, named) {
        if named.id != worktree.project_id {
            return Err(WORKTREE_ELSEWHERE_ERROR.into());
        }
    }
    Ok((named, worktree))
}

pub fn worktree_for<'a>(state: &'a WorkspaceState, thread: Option<&Thread>) -> Option<&'a Worktree> {
    worktree_by_id(state, thread?.worktree_id.as_deref())
}

/// Every thread still linked to a checkout, including archived threads.
pub fn worktree_claimants<'a>(state: &'a WorkspaceState, worktree_id: &str) -> Vec<&'a Thread> {
    state.threads.iter().map(|thread| &**thread).filter(|thread| thread.worktree_id.as_deref() == Some(worktree_id)).collect()
}

/// The folder a thread works in: the checkout it shares once it has one, otherwise its project's.
pub fn thread_workspace_root(state: &WorkspaceState, thread: Option<&Thread>) -> Option<String> {
    worktree_for(state, thread).map(|worktree| worktree.root.clone()).or_else(|| project_for(state, thread).map(|project| project.root.clone()))
}

/// Where a file a message named is looked for, nearest the thread first.
pub fn thread_file_roots(state: &WorkspaceState, thread: Option<&Thread>) -> Vec<String> {
    let project = project_for(state, thread);
    let mut siblings: Vec<&Worktree> = match project {
        Some(project) => state.worktrees.iter().filter(|worktree| worktree.project_id == project.id).collect(),
        None => Vec::new(),
    };
    siblings.sort_by(|left, right| right.last_used_at.cmp(&left.last_used_at));
    let mut roots: Vec<String> = Vec::new();
    let candidates = [worktree_for(state, thread).map(|worktree| worktree.root.clone()), project.map(|project| project.root.clone())].into_iter().flatten().chain(siblings.into_iter().map(|worktree| worktree.root.clone()));
    for root in candidates {
        if !root.is_empty() && !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// Where a thread's runs happen: the checkout it shares once it has one, otherwise its project's.
pub fn thread_workspace_id(state: &WorkspaceState, thread: Option<&Thread>) -> Option<String> {
    worktree_for(state, thread).map(|worktree| worktree.workspace_id.clone()).or_else(|| project_for(state, thread).and_then(|project| project.workspace_id.clone()))
}

/// Threads whose checkout is going: those that asked to leave, and every thread in a checkout being deleted.
pub fn leaving_thread_ids(state: &WorkspaceState) -> HashSet<String> {
    let mut leaving: HashSet<String> = state.releasing_worktrees.iter().cloned().collect();
    if state.deleting_worktrees.is_empty() {
        return leaving;
    }
    let going: HashSet<&str> = state.worktrees.iter().filter(|worktree| state.deleting_worktrees.contains(&worktree.root)).map(|worktree| worktree.id.as_str()).collect();
    for thread in &state.threads {
        if thread.worktree_id.as_deref().is_some_and(|id| going.contains(id)) {
            leaving.insert(thread.id.clone());
        }
    }
    leaving
}

pub fn location_of(state: &WorkspaceState, thread: Option<&Thread>) -> ThreadLocation {
    if let Some(thread) = thread {
        if leaving_thread_ids(state).contains(&thread.id) {
            return ThreadLocation::Releasing;
        }
        if state.creating_worktrees.contains(&thread.id) {
            return ThreadLocation::Creating;
        }
    }
    match worktree_for(state, thread) {
        Some(worktree) => ThreadLocation::Worktree { threads: worktree_claimants(state, &worktree.id).len(), worktree: worktree.clone() },
        None => ThreadLocation::Local,
    }
}

/// The folder the app is pointed at: the thread's own checkout, else its project, else the last one opened.
pub fn current_folder(state: &WorkspaceState) -> Option<String> {
    let thread = state.current_thread();
    let draft = state.draft_project_id.as_deref().and_then(|id| state.project(id)).map(|project| project.root.clone());
    thread_workspace_root(state, thread).or(draft).or_else(|| state.last_folder.clone())
}
