//! How a turn ends: the facts of its end go in and one settlement comes out.

use crate::domain::coordination::{coordinator_of, ends_as_stopped, is_coordinator, reopened_if_stuck, with_coordination_note};
use crate::domain::run::RunStatus;
use crate::domain::thread::{dismissed, run_outcome, with_closed_issues, ThreadOutcome};
use crate::domain::usage_limit::{resumes_on_its_own, with_engine_limit, LimitPause, LimitWindow, UsageLimit};
use crate::domain::workflow::WorkflowStatus;
use crate::reducer::run_queue::{drained, QueueIntent};
use crate::reducer::run_state::{restored_testimony, turn_end_status, with_run_status, withdraw_run, ended_turn};
use crate::reducer::Step;
use crate::state::{ActiveRun, RunOperationKind, RunOrigin, WorkspaceState};
use crate::workspace::activity::thread_on_screen;
use crate::workspace::notices::{turn_note, RunSurfacing};

/// Who asked a turn to end before it ended by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopIntent {
    User,
    StopAll,
    Quit,
}

/// What a turn's end is settled on.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnEnd<'a> {
    pub status: RunStatus,
    pub message: Option<String>,
    pub limit: Option<UsageLimit>,
    pub run: Option<&'a ActiveRun>,
    pub stop: Option<StopIntent>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settlement {
    pub status: RunStatus,
    pub queue: QueueIntent,
    pub message: Option<String>,
    pub surfacing: Option<RunSurfacing>,
    pub verdict: Option<ThreadOutcome>,
    pub dismisses: bool,
    pub run_ended_as: Option<ThreadOutcome>,
    pub restores_testimony: bool,
    pub withdraws: bool,
    pub closes_issues: bool,
    /// The usage limit the turn reached, and whether its thread waits in line for it.
    pub limit: Option<(UsageLimit, bool)>,
    pub reopens: bool,
    /// What its coordinator here hears: None for nothing, Some(quiet) otherwise.
    pub heard: Option<bool>,
    pub answers_stop_all: bool,
    pub hushes_news: bool,
}

/// Whether a thread's turn ending is heard only with its coordinator's next wake.
pub fn ends_quietly(status: RunStatus, stop: Option<StopIntent>, answers_stop: bool, users_own: bool) -> bool {
    answers_stop || stop == Some(StopIntent::Quit) || (stop == Some(StopIntent::StopAll) && ends_as_stopped(run_outcome(status))) || (users_own && status != RunStatus::Failed)
}

/// Why a turn reaches the user, or None when it earned its silence.
pub fn why_turn_surfaces(run: Option<&ActiveRun>, status: RunStatus) -> Option<RunSurfacing> {
    match status {
        RunStatus::Succeeded => {}
        RunStatus::Failed => return Some(RunSurfacing::Failed),
        _ => return Some(RunSurfacing::Cancelled),
    }
    let Some(run) = run else { return Some(RunSurfacing::Attended) };
    if run.origin != RunOrigin::Automation {
        return Some(RunSurfacing::Attended);
    }
    if !run.quiet {
        return Some(RunSurfacing::Loud);
    }
    if !run.acknowledged {
        return Some(RunSurfacing::NoAnswer);
    }
    run.notified.then_some(RunSurfacing::Reported)
}

/// Settles a turn's end.
pub fn settle_turn(end: TurnEnd) -> Settlement {
    let stopping = end.stop.is_some() || end.run.is_some_and(|run| run.answers_stop);
    let stopped_first = stopping && end.status == RunStatus::Failed && end.limit.as_ref().is_some_and(|limit| limit.window == LimitWindow::Session);
    let status = if stopped_first { RunStatus::Cancelled } else { end.status };
    let message = if stopped_first { None } else { end.message.clone() };
    let ended = run_outcome(status);
    let queue = if status == RunStatus::Cancelled { QueueIntent::HandBack } else { QueueIntent::Deliver };
    if end.run.is_some_and(|run| run.operation == Some(RunOperationKind::Compact)) {
        return Settlement { status, queue, message, surfacing: None, verdict: None, dismisses: false, run_ended_as: ended, restores_testimony: true, withdraws: false, closes_issues: false, limit: None, reopens: false, heard: None, answers_stop_all: false, hushes_news: false };
    }
    let surfacing = why_turn_surfaces(end.run, status);
    let unseen = surfacing.is_none();
    let reached = if end.status == RunStatus::Failed { end.limit.clone() } else { None };
    let carries_on = reached.as_ref().is_some_and(|limit| !stopping && resumes_on_its_own(limit.window));
    let finished = !matches!(surfacing, Some(RunSurfacing::Failed) | Some(RunSurfacing::Cancelled));
    Settlement {
        status,
        queue,
        message,
        surfacing,
        verdict: if unseen || carries_on { None } else { ended },
        dismisses: status == RunStatus::Succeeded && end.run.is_some_and(|run| run.dismissed),
        run_ended_as: if carries_on { None } else { ended },
        restores_testimony: false,
        withdraws: unseen,
        closes_issues: finished && end.run.is_some_and(|run| run.origin == RunOrigin::Automation && run.acknowledged),
        limit: reached.map(|limit| (limit, !stopping)),
        reopens: status == RunStatus::Failed && !carries_on,
        heard: if unseen || carries_on { None } else { Some(ends_quietly(status, end.stop, end.run.is_some_and(|run| run.answers_stop), end.run.is_some_and(|run| run.users_own))) },
        answers_stop_all: !unseen && end.stop == Some(StopIntent::StopAll),
        hushes_news: end.stop == Some(StopIntent::Quit),
    }
}

/// What the turn leaves on its own thread: its verdict, and for a tick, the run withdrawn or its issues closed.
pub fn turn_testimony(state: &mut WorkspaceState, task_id: &str, settlement: &Settlement, run: Option<&ActiveRun>) {
    if settlement.restores_testimony {
        return;
    }
    if let Some(verdict) = settlement.verdict {
        let unread = !thread_on_screen(state, task_id);
        if let Some(thread) = state.thread_mut(task_id) {
            if thread.archived_at.is_none() {
                thread.outcome = Some(verdict);
                if unread {
                    thread.outcome_unread = true;
                }
            }
        }
    }
    if settlement.dismisses {
        dismissed(&mut state.threads, &[task_id.to_string()].into());
    }
    if let Some(run) = run {
        if settlement.withdraws || settlement.closes_issues {
            if let Some(thread) = state.thread_mut(task_id) {
                if settlement.withdraws {
                    withdraw_run(thread, run.messages_before, &run.before);
                }
                if settlement.closes_issues {
                    with_closed_issues(thread, &run.reported_issues);
                }
            }
        }
    }
}

/// What follows from the turn's end beyond its testimony: its usage limit, a reopening, Stop all's answer, and its coordinator.
fn turn_aftermath(state: &mut WorkspaceState, task_id: &str, settlement: &Settlement, run: Option<&ActiveRun>, at: i64) {
    if let Some((limit, waits)) = &settlement.limit {
        if *waits {
            paused_for_limit(state, task_id, limit, settlement.run_ended_as.is_none(), at);
        } else if let Some(engine) = state.thread(task_id).map(|thread| thread.engine) {
            with_engine_limit(&mut state.threads, engine, limit);
        }
    }
    if settlement.reopens {
        if let Some(thread) = state.thread_mut(task_id) {
            reopened_if_stuck(thread);
        }
    }
    if settlement.answers_stop_all {
        if let Some(run) = run {
            state.quiet_stops.retain(|id| id != &run.run_id);
        }
    }
    if let Some(quiet) = settlement.heard {
        if let Some(thread) = state.thread(task_id).cloned() {
            if let Some(lead_id) = coordinator_of(&state.threads, Some(&thread)).map(|lead| lead.id.clone()) {
                let note = turn_note(&thread, settlement.status, at, quiet);
                if let Some(lead) = state.thread_mut(&lead_id) {
                    with_coordination_note(lead, note);
                }
            }
        }
    }
    if settlement.hushes_news && is_coordinator(state.thread(task_id)) {
        if let Some(notes) = state.thread_mut(task_id).and_then(|thread| thread.coordination_notes.as_mut()) {
            for note in notes.iter_mut() {
                note.quiet = true;
            }
        }
    }
}

/// Puts a thread whose run hit the limit in line. One that carries on by itself has its verdict and end taken off.
fn paused_for_limit(state: &mut WorkspaceState, task_id: &str, limit: &UsageLimit, carries_on: bool, at: i64) {
    let Some(engine) = state.thread(task_id).map(|thread| thread.engine) else { return };
    let workflow = state.workflows.get(task_id).is_some_and(|workflows| workflows.iter().any(|workflow| workflow.status == WorkflowStatus::Stopped));
    with_engine_limit(&mut state.threads, engine, limit);
    let pause = LimitPause { resets_at: limit.resets_at, window: limit.window, paused_at: at, nudged_at: None, workflow, held: None };
    if let Some(thread) = state.thread_mut(task_id) {
        if carries_on {
            thread.outcome = None;
            thread.outcome_unread = false;
            thread.run_ended_as = None;
        }
        thread.limit_pause = Some(pause);
    }
}

/// Applies a settlement to a thread whose turn has already closed, then takes up its queue.
pub fn turn_settled(state: &mut WorkspaceState, task_id: &str, settlement: &Settlement, run: Option<&ActiveRun>, at: i64) -> Step {
    if settlement.restores_testimony {
        if let (Some(run), Some(thread)) = (run, state.thread_mut(task_id)) {
            restored_testimony(thread, &run.before);
        }
    } else {
        turn_testimony(state, task_id, settlement, run);
        turn_aftermath(state, task_id, settlement, run, at);
    }
    let queue = settlement.queue;
    drained(state, task_id, queue)
}

/// Takes up the thread's queue once its testimony is already left, as the turn's end says.
pub fn settled_after_testimony(state: &mut WorkspaceState, task_id: &str, settlement: &Settlement, run: Option<&ActiveRun>, at: i64) -> Step {
    turn_aftermath(state, task_id, settlement, run, at);
    drained(state, task_id, settlement.queue)
}

/// A thread's turn ending with `status`: it reads as stopped or idle, and keeps how and when it ended.
pub fn turn_ended(state: &mut WorkspaceState, task_id: &str, status: RunStatus) {
    with_run_status(state, task_id, turn_end_status(status));
    if let Some(thread) = state.thread_mut(task_id) {
        ended_turn(thread, status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_beats_a_session_limit() {
        let settlement = settle_turn(TurnEnd { status: RunStatus::Failed, message: Some("limit".into()), limit: Some(UsageLimit { resets_at: 10, window: LimitWindow::Session }), run: None, stop: Some(StopIntent::User) });
        assert_eq!(settlement.status, RunStatus::Cancelled);
        assert_eq!(settlement.queue, QueueIntent::HandBack);
        assert!(settlement.message.is_none());
    }

    #[test]
    fn a_session_limit_carries_on_without_a_verdict() {
        let settlement = settle_turn(TurnEnd { status: RunStatus::Failed, message: None, limit: Some(UsageLimit { resets_at: 10, window: LimitWindow::Session }), run: None, stop: None });
        assert_eq!(settlement.verdict, None);
        assert_eq!(settlement.run_ended_as, None);
        assert_eq!(settlement.limit.map(|(_, waits)| waits), Some(true));
    }
}
