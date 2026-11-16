//! The menus and confirmations open over the window, as they are drawn.

use crate::domain::project::Project;
use crate::events::ChangedFilesResult;
use crate::state::WorkspaceState;
use crate::view::computers::computer_of_thread;
use crate::workspace::location::{thread_workspace_id, worktree_claimants, worktree_for};

/// The folder editor as the dialog draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectEditorView {
    pub project: Project,
    pub checkouts: usize,
    pub saving: bool,
    pub error: Option<String>,
}

pub fn project_editor_view(state: &WorkspaceState) -> Option<ProjectEditorView> {
    let edit = state.project_edit.as_ref()?;
    let project = state.project(&edit.project_id)?.clone();
    let checkouts = state.worktrees.iter().filter(|worktree| worktree.project_id == project.id).count();
    Some(ProjectEditorView { project, checkouts, saving: edit.saving, error: edit.error.clone() })
}

/// The pending move as the confirmation draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeMoveView {
    pub worktree: bool,
    /// Uncommitted files in the checkout the thread is leaving, which the move commits first.
    pub changes: usize,
    /// Threads left in the worktree once this one goes.
    pub others: usize,
}

pub fn worktree_move_view(state: &WorkspaceState) -> Option<WorktreeMoveView> {
    let pending = state.worktree_move.as_ref()?;
    let thread = state.thread(&pending.task_id)?;
    let changes = match thread_workspace_id(state, Some(thread)).and_then(|id| state.environments.get(&id)) {
        Some(ChangedFilesResult::Available { files, .. }) => files.len(),
        _ => 0,
    };
    let others = worktree_for(state, Some(thread)).map(|worktree| worktree_claimants(state, &worktree.id).len().saturating_sub(1)).unwrap_or(0);
    Some(WorktreeMoveView { worktree: pending.worktree, changes, others })
}

/// The thread the archive confirmation asks about, wherever it is held, and whether archiving stops a run.
#[derive(Clone, Debug, PartialEq)]
pub struct ArchivePromptView {
    pub task_id: String,
    pub title: String,
    pub running: bool,
}

/// None once the thread is gone or already archived.
pub fn archive_prompt_view(state: &WorkspaceState, task_id: Option<&str>) -> Option<ArchivePromptView> {
    let task_id = task_id.or(state.archive_prompt.as_deref())?;
    let holder = computer_of_thread(state, Some(task_id)).and_then(|computer| computer.state.as_deref()).unwrap_or(state);
    let thread = holder.thread(task_id)?;
    if thread.archived_at.is_some() {
        return None;
    }
    Some(ArchivePromptView { task_id: task_id.into(), title: thread.title.clone(), running: holder.active_runs.contains_key(task_id) })
}

pub const ARCHIVE_RETENTION_DAYS: i64 = 5;

pub fn archive_question(title: &str) -> String {
    format!("Archive “{}”?", if title.is_empty() { "Untitled thread" } else { title })
}

pub fn archive_consequence(running: bool, phone: bool) -> String {
    let place = format!("Settings › {}{}", crate::domain::settings_catalog::settings_page_label(crate::domain::sidebar::SettingsSection::Archive), if phone { " on your computer" } else { "" });
    format!("{} leaves the {}. Restore it from {place} within {ARCHIVE_RETENTION_DAYS} days.", if running { "Its run stops and it" } else { "It" }, if phone { "list" } else { "sidebar" })
}
