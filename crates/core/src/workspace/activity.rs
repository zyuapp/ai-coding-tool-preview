//! Where each of one computer's threads stands, the lists its threads are drawn from, and what
//! landing on a thread reads.

use crate::shared::Shared;
use std::collections::{HashMap, HashSet};

use crate::domain::automation::with_watched_threads;
use crate::domain::coordination::{coordinated_threads, coordinator_of, lead_id_of};
use crate::domain::thread::{has_unread_attention, read_attention, Thread};
use crate::domain::usage_limit::resumes_on_its_own;
use crate::domain::workflow::WorkflowStatus;
use crate::state::{dock_for, tab_holder_of, PendingRun, RunPhase, ThreadRunStatus, WorkspaceState};
use crate::workspace::location::leaving_thread_ids;

/// What a thread is waiting on before it can work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadWait {
    Worktree,
    WorktreeRelease,
    Run,
}

/// One thread's state, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadStatus {
    Blocked,
    Paused,
    Working,
    Idle,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadActivity {
    /// Threads with work going or about to go without the user.
    pub working: HashSet<String>,
    /// Threads stopped on a question only the user can answer.
    pub blocked: HashSet<String>,
    /// Threads waiting out a usage limit.
    pub paused: HashSet<String>,
    /// Threads not working whose session keeps a background shell or monitor going.
    pub monitoring: HashSet<String>,
    /// What the activity list ranks under Running.
    pub ranked: HashSet<String>,
}

/// Each thread with starts still resolving where their run will happen, and those starts.
pub fn starting_threads(state: &WorkspaceState) -> HashMap<String, Vec<&PendingRun>> {
    let mut grouped: HashMap<String, Vec<&PendingRun>> = HashMap::new();
    for pending in state.pending_runs.values() {
        if let Some(task_id) = &pending.task_id {
            grouped.entry(task_id.clone()).or_default().push(pending);
        }
    }
    grouped
}

pub fn starts_of<'a>(state: &'a WorkspaceState, task_id: &str) -> Vec<&'a PendingRun> {
    state.pending_runs.values().filter(|pending| pending.task_id.as_deref() == Some(task_id)).collect()
}

/// Whether a thread is working, counting the moment between a send and the checkout it resolves to.
pub fn thread_busy(state: &WorkspaceState, task_id: &str) -> bool {
    state.active_runs.contains_key(task_id) || state.pending_runs.values().any(|pending| pending.task_id.as_deref() == Some(task_id))
}

/// Whether the user has a turn of their own going or on its way in this thread.
pub fn users_turn(state: &WorkspaceState, task_id: &str) -> bool {
    use crate::state::RunOrigin;
    state.active_runs.get(task_id).is_some_and(|run| run.origin == RunOrigin::Composer) || starts_of(state, task_id).iter().any(|pending| pending.origin == RunOrigin::Composer)
}

/// What a thread's composer shows of its run.
pub fn run_status(state: &WorkspaceState, task_id: Option<&str>) -> ThreadRunStatus {
    let Some(task_id) = task_id else { return ThreadRunStatus::Idle };
    if state.active_runs.contains_key(task_id) {
        return ThreadRunStatus::Running;
    }
    state.run_statuses.get(task_id).copied().unwrap_or(ThreadRunStatus::Idle)
}

/// The thread a resolved start may begin its run on.
pub fn thread_to_begin<'a>(state: &'a WorkspaceState, task_id: Option<&str>) -> Option<&'a Thread> {
    let thread = state.thread(task_id?)?;
    (thread.archived_at.is_none() && thread.resource_stop.is_none() && !state.active_runs.contains_key(&thread.id)).then_some(thread)
}

/// Whether a run is going in a checkout.
pub fn runs_in_workspace(state: &WorkspaceState, workspace_id: Option<&str>) -> bool {
    let Some(workspace_id) = workspace_id else { return false };
    state.active_runs.keys().any(|task_id| state.thread(task_id).is_some_and(|thread| crate::workspace::location::thread_workspace_id(state, Some(thread)).as_deref() == Some(workspace_id)))
}

/// The one answer the sidebar, thread tools, scheduler, and coordinators all read.
pub fn thread_activity(state: &WorkspaceState) -> ThreadActivity {
    let mut working: HashSet<String> = state.active_runs.keys().cloned().collect();
    let blocked: HashSet<String> = state.active_runs.values().filter(|run| run.phase == RunPhase::AwaitingApproval).map(|run| run.task_id.clone()).collect();
    for pending in state.pending_runs.values() {
        if let Some(task_id) = &pending.task_id {
            working.insert(task_id.clone());
        }
    }
    working.extend(state.creating_worktrees.iter().cloned());
    working.extend(leaving_thread_ids(state));
    for (task_id, workflows) in &state.workflows {
        if workflows.iter().any(|workflow| workflow.status == WorkflowStatus::Running) {
            working.insert(task_id.clone());
        }
    }
    for (task_id, subagents) in &state.subagents {
        if subagents.iter().any(|subagent| subagent.status == crate::domain::run::SubagentStatus::Working) {
            working.insert(task_id.clone());
        }
    }
    let mut paused = HashSet::new();
    for thread in &state.threads {
        let Some(pause) = &thread.limit_pause else { continue };
        paused.insert(thread.id.clone());
        if resumes_on_its_own(pause.window) {
            working.insert(thread.id.clone());
        }
    }
    for (task_id, queued) in &state.queued_messages {
        if !queued.is_empty() && !paused.contains(task_id) {
            working.insert(task_id.clone());
        }
    }
    let mut monitoring = HashSet::new();
    for (task_id, processes) in &state.background_processes {
        if !working.contains(task_id) && processes.iter().any(|process| !process.stopping) {
            monitoring.insert(task_id.clone());
        }
    }
    let ranked: HashSet<String> = working.union(&monitoring).cloned().collect();
    let ranked = with_watched_threads(&ranked, &state.automations);
    ThreadActivity { working, blocked, paused, monitoring, ranked }
}

/// Whether a thread's session holds work a fresh one would silently end.
pub fn session_holds_work(state: &WorkspaceState, task_id: &str) -> bool {
    state.goals.contains_key(task_id)
        || state.workflows.get(task_id).is_some_and(|workflows| workflows.iter().any(|workflow| workflow.status == WorkflowStatus::Running))
        || state.background_processes.get(task_id).is_some_and(|processes| !processes.is_empty())
        || state.subagents.get(task_id).is_some_and(|subagents| subagents.iter().any(|subagent| subagent.status == crate::domain::run::SubagentStatus::Working))
}

/// What a thread's session keeps working on past its run, which a stop of the run leaves going.
pub fn unstopped_session_work(state: &WorkspaceState, task_id: &str) -> Vec<String> {
    let workflows = state.workflows.get(task_id).into_iter().flatten().filter(|workflow| workflow.status == WorkflowStatus::Running && !workflow.stopping).map(|workflow| workflow.id.clone());
    let subagents = state.subagents.get(task_id).into_iter().flatten().filter(|subagent| subagent.status == crate::domain::run::SubagentStatus::Working && !subagent.stopping).map(|subagent| subagent.id.clone());
    workflows.chain(subagents).collect()
}

/// Whether a thread is doing work: it has work going, and is not stopped on a question for the user.
pub fn doing_work(activity: &ThreadActivity, task_id: &str) -> bool {
    activity.working.contains(task_id) && !activity.blocked.contains(task_id)
}

pub fn thread_status(activity: &ThreadActivity, task_id: &str) -> ThreadStatus {
    if activity.blocked.contains(task_id) {
        ThreadStatus::Blocked
    } else if activity.paused.contains(task_id) {
        ThreadStatus::Paused
    } else if activity.working.contains(task_id) {
        ThreadStatus::Working
    } else {
        ThreadStatus::Idle
    }
}

/// What a thread, or the draft typing under `draft_key`, is waiting on before its run can go.
pub fn waiting_on(state: &WorkspaceState, task_id: Option<&str>, draft_key: &str) -> Option<ThreadWait> {
    if let Some(task_id) = task_id {
        if state.creating_worktrees.iter().any(|id| id == task_id) {
            return Some(ThreadWait::Worktree);
        }
        if leaving_thread_ids(state).contains(task_id) {
            return Some(ThreadWait::WorktreeRelease);
        }
    }
    let resolving = state.pending_runs.values().find(|pending| (task_id.is_some() && pending.task_id.as_deref() == task_id) || pending.draft_key.as_deref() == Some(draft_key))?;
    Some(if resolving.creating_worktree { ThreadWait::Worktree } else { ThreadWait::Run })
}

#[derive(Clone, Debug, Default)]
pub struct ThreadLists<'a> {
    /// Every thread with a row of its own, so not a side chat.
    pub listed_threads: Vec<&'a Thread>,
    /// The listed threads not filed away.
    pub visible_threads: Vec<&'a Thread>,
    /// Filed away, most recent first.
    pub archived_threads: Vec<&'a Thread>,
    pub worktree_thread_ids: HashSet<String>,
    /// Threads holding a side chat with an unseen mark.
    pub side_chat_attention: HashSet<String>,
    /// Visible threads carrying an unseen mark, their side chats' included.
    pub unread_count: usize,
}

pub fn thread_lists(state: &WorkspaceState) -> ThreadLists<'_> {
    let forked = state.side_chat_ids();
    let listed_threads: Vec<&Thread> = state.threads.iter().map(|thread| &**thread).filter(|thread| !forked.contains(&thread.id)).collect();
    let mut visible_threads = Vec::new();
    let mut archived_threads = Vec::new();
    let mut worktree_thread_ids = HashSet::new();
    for thread in &listed_threads {
        if thread.archived_at.is_none() {
            visible_threads.push(*thread);
        } else {
            archived_threads.push(*thread);
        }
        if thread.worktree_id.is_some() {
            worktree_thread_ids.insert(thread.id.clone());
        }
    }
    archived_threads.sort_by(|left, right| right.archived_at.cmp(&left.archived_at));
    let (side_chat_attention, unread_count) = unread_view(state, &visible_threads);
    ThreadLists { listed_threads, visible_threads, archived_threads, worktree_thread_ids, side_chat_attention, unread_count }
}

/// The marks the sidebar and the app icon share, side chats and coordinators' threads folded into their rows.
pub fn unread_view(state: &WorkspaceState, listed: &[&Thread]) -> (HashSet<String>, usize) {
    let mut side_chat_attention = HashSet::new();
    for chat in &state.side_chats {
        if state.thread(&chat.id).is_some_and(has_unread_attention) {
            side_chat_attention.insert(chat.source_thread_id.clone());
        }
    }
    let members = crate::domain::coordination::coordinated_ids(listed);
    let mut rows = HashSet::new();
    for thread in listed {
        if has_unread_attention(thread) || side_chat_attention.contains(&thread.id) {
            let row = if members.contains(&thread.id) { lead_id_of(thread).unwrap_or(&thread.id).to_string() } else { thread.id.clone() };
            rows.insert(row);
        }
    }
    let count = rows.len();
    (side_chat_attention, count)
}

/// Whether the user is looking at a thread.
pub fn thread_on_screen(state: &WorkspaceState, task_id: &str) -> bool {
    if state.computers.active.is_some() {
        return false;
    }
    if state.current_id.as_deref() == Some(task_id) {
        return true;
    }
    let thread = state.thread(task_id);
    if let Some(current) = &state.current_id {
        if coordinator_of(&state.threads, thread).is_some_and(|lead| &lead.id == current) {
            return true;
        }
    }
    let Some(holder) = tab_holder_of(state, task_id) else { return false };
    if state.current_id.as_deref() != Some(holder.as_str()) {
        return false;
    }
    let dock = dock_for(state, &holder);
    dock.open && dock.tab == task_id
}

/// Landing on a thread reads it, and landing on a coordinator reads the threads under it too.
pub fn read_row(threads: &mut Vec<Shared<Thread>>, thread_id: Option<&str>) -> bool {
    let mut changed = read_attention(threads, thread_id);
    let Some(thread_id) = thread_id else { return changed };
    let members: Vec<String> = coordinated_threads(threads, thread_id).iter().map(|member| member.id.clone()).collect();
    for member in members {
        changed |= read_attention(threads, Some(&member));
    }
    changed
}
