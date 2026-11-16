//! Threads stopped for their engine's memory, and resumed by the user.

use crate::domain::run::{SubagentReport, SubagentStatus};
use crate::domain::session_resources::{ResourceCleanup, ResourceStop};
use crate::domain::thread::ThreadOutcome;
use crate::domain::workflow::WorkflowStatus;
use crate::events::{ThreadEvent, ThreadReport};
use crate::reducer::run_queue::{drained, resolve_workspace_effect, QueueIntent};
use crate::reducer::run_state::apply_thread_event;
use crate::reducer::Step;
use crate::state::{PendingOperation, PendingRun, ResumeAfter, RunOrigin, WorkspaceState};
use crate::util::random_id;
use crate::workspace::activity::{thread_busy, thread_on_screen};
use crate::workspace::location::{project_for, worktree_for};
use crate::workspace::notices::{announced, NoticeReason};

pub const MEMORY_STOP_SUMMARY: &str = "Stopped due to high memory usage.";
pub const MEMORY_STOP_ERROR: &str = "Resume this thread after its memory stop before starting another run.";
pub const MEMORY_SEND_ERROR: &str = "Resume this thread after its memory stop before sending another message.";
const MEMORY_RESUME_PROMPT: &str = "Continue after the session was stopped for high memory usage. Inspect the current state before repeating any interrupted action. Avoid the allocation or command that caused the excessive memory use.";

/// A session stop also ends work whose parent turn finished earlier.
pub fn resource_stopped(state: &mut WorkspaceState, task_id: &str, stop: &ResourceStop) -> Step {
    let Some(thread) = state.thread(task_id).cloned() else { return Step::none() };
    let previous = thread.resource_stop.as_ref().map(|stop| stop.at);
    if stop.cleanup != ResourceCleanup::Stopping && previous != Some(stop.at) {
        return Step::none();
    }
    let first = previous != Some(stop.at);
    let said = if first { announced(state, &thread, "Stopped due to high memory usage", NoticeReason::Failed) } else { Vec::new() };
    let unread = !thread_on_screen(state, task_id);
    if let Some(thread) = state.thread_mut(task_id) {
        thread.resource_stop = Some(stop.clone());
        if first && thread.archived_at.is_none() {
            thread.outcome = Some(ThreadOutcome::Stopped);
            if unread {
                thread.outcome_unread = true;
            }
        }
    }
    if first {
        let event = |report: ThreadReport| ThreadEvent { task_id: task_id.to_string(), report };
        apply_thread_event(state, &event(ThreadReport::BackgroundChanged { processes: Vec::new() }));
        apply_thread_event(state, &event(ThreadReport::GoalChanged { goal: None }));
        let children: Vec<String> = state.subagents.get(task_id).into_iter().flatten().filter(|child| matches!(child.status, SubagentStatus::Working | SubagentStatus::Idle)).map(|child| child.id.clone()).collect();
        for id in children {
            apply_thread_event(state, &event(ThreadReport::Subagent(SubagentReport::Finished { id, status: SubagentStatus::Stopped, summary: MEMORY_STOP_SUMMARY.into() })));
        }
        let workflows: Vec<String> = state.workflows.get(task_id).into_iter().flatten().filter(|workflow| workflow.status == WorkflowStatus::Running).map(|workflow| workflow.id.clone()).collect();
        for id in workflows {
            apply_thread_event(state, &event(ThreadReport::WorkflowFinished { id, status: WorkflowStatus::Stopped, summary: MEMORY_STOP_SUMMARY.into() }));
        }
    }
    Step::effects(said).then(drained(state, task_id, QueueIntent::HandBack))
}

/// The user picks a stopped thread back up once its processes have exited.
pub fn resource_resumed(state: &mut WorkspaceState, task_id: &str) -> Step {
    let Some(thread) = state.thread(task_id).cloned() else { return Step::none() };
    if thread.resource_stop.as_ref().is_none_or(|stop| stop.cleanup != ResourceCleanup::Complete) || thread_busy(state, task_id) {
        return Step::none();
    }
    let mut pending = PendingRun::new(random_id(), random_id(), RunOrigin::Composer, "", MEMORY_RESUME_PROMPT);
    pending.task_id = Some(task_id.into());
    pending.operation = Some(PendingOperation::Resume { after: ResumeAfter::Memory });
    let effect = resolve_workspace_effect(&pending.id, Some(&thread), project_for(state, Some(&thread)), worktree_for(state, Some(&thread)), false, None);
    if let Some(thread) = state.thread_mut(task_id) {
        thread.resource_stop = None;
    }
    state.pending_runs.insert(pending.id.clone(), pending);
    Step::effect(effect)
}
