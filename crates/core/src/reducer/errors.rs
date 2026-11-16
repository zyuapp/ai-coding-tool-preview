//! The refusals the workspace reports, and the names the view knows them by.

use crate::domain::coordination::{is_coordinator, MAX_OPEN_DECISIONS};
use crate::domain::thread::Thread;
use crate::domain::thread_role::ThreadRole;
use crate::domain::worktree::WorktreeDestination;

pub const REOPEN_PROJECT_ERROR: &str = "Reopen this project folder before running a task.";
pub const SAME_PROJECT_ERROR: &str = "Choose the same project folder to continue this task.";
pub const MISSING_PROJECT_ERROR: &str = "This task's project is unavailable. Reopen the project folder before running it.";
pub const RUNNING_PROJECT_ERROR: &str = "Stop the running tasks before removing this project.";
pub const PROJECT_WORKTREES_ERROR: &str = "Delete this project's worktrees before removing the project.";
pub const BUSY_AUTOMATION_ERROR: &str = "This task is already running. The automation will run on its next tick.";
pub const WORKTREE_PROJECT_ERROR: &str = "Open this thread in a project folder before giving it a worktree.";
pub const WORKTREE_MISSING_ERROR: &str = "That worktree is not one this app is keeping.";
pub const WORKTREE_ELSEWHERE_ERROR: &str = "That worktree is a checkout of another project.";
pub const TERMINAL_FOLDER_ERROR: &str = "Open a project folder before starting a terminal.";
pub const WORKTREE_RUNNING_ERROR: &str = "Stop this thread's run before changing where it works.";
pub const WORKTREE_CREATING_ERROR: &str = "This thread's worktree is still being created.";
pub const WORKTREE_RELEASING_ERROR: &str = "This thread's worktree is still being removed.";
pub const ARCHIVED_SEND_ERROR: &str = "This thread is archived, so only the user can pick it back up, by writing to it or restoring it from Archived threads in Settings.";
pub const WORKTREE_COORDINATOR_ERROR: &str = "A coordinator stays in its project's checkout, since it changes nothing itself. Start a thread with start_thread and worktree: true to do the work in a worktree.";
pub const COORDINATOR_ROLE_ERROR: &str = "A coordinator keeps its role until the user changes it from the thread menu.";
pub const WORKTREE_COORDINATOR_ROLE_ERROR: &str = "A coordinator stays in its project's checkout, since it changes nothing itself, and this thread works in a worktree. Start a coordinator with start_thread and role \"coordinator\" instead.";

pub const NO_COORDINATOR: &str = "This thread works under no coordinator, so there is no one to report to and nothing was recorded. Say it in your reply instead.";
pub const OUTSIDE_COORDINATION: &str = "Only a coordinator and the threads working under it can raise decisions, so nothing was recorded. Ask the user in your reply instead.";
pub const NO_MEMORY: &str = "Only a coordinator keeps a memory.";

pub fn decisions_full_error() -> String {
    format!("This thread already has {MAX_OPEN_DECISIONS} decisions waiting on the user, so this one was not raised. Wait for their answers, or ask in your reply instead.")
}

pub const CHECKOUT_RUNNING_ERROR: &str = "Stop the threads running in this project before starting one on another branch.";
pub const SWITCH_RUNNING_ERROR: &str = "Stop the threads running in this checkout before switching it to another branch.";
pub const SWITCH_PROJECT_ERROR: &str = "Open this thread in a project folder before switching branches.";
pub const FILE_FOLDER_ERROR: &str = "Open this thread in a project folder before opening a file from it.";
pub const APP_FOLDER_ERROR: &str = "Open this thread in a project folder before opening it in another application.";

pub const BROWSER_URL_ERROR: &str = "That is not a page the browser can open.";
pub const BROWSER_TAB_ERROR: &str = "The browser has no page open to act on.";

/// What a thread asks of a coordinator that no thread may: moving one into a worktree, taking its role,
/// or making one in a worktree a coordinator.
pub enum CoordinatorAsk<'a> {
    MoveWorktree(&'a WorktreeDestination),
    SetRole(Option<ThreadRole>),
}

pub fn coordinator_ask_refusal(subject: &Thread, ask: CoordinatorAsk, creating_worktree: bool) -> Option<&'static str> {
    match ask {
        CoordinatorAsk::MoveWorktree(destination) => (!matches!(destination, WorktreeDestination::Local) && is_coordinator(Some(subject))).then_some(WORKTREE_COORDINATOR_ERROR),
        CoordinatorAsk::SetRole(role) => {
            if role != Some(ThreadRole::Coordinator) {
                return (subject.role == Some(ThreadRole::Coordinator)).then_some(COORDINATOR_ROLE_ERROR);
            }
            (subject.worktree_id.is_some() || creating_worktree).then_some(WORKTREE_COORDINATOR_ROLE_ERROR)
        }
    }
}
