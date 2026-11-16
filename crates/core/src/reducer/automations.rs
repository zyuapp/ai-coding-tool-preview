//! Scheduled ticks: whether a tick can run, what a run of one raises, and the watches that end.

use std::collections::HashSet;

use crate::domain::automation::{decline_count, is_watching, AutomationView, DECLINES_BEFORE_SURFACING};
use crate::domain::coordination::{coordinator_of, with_coordination_note};
use crate::domain::finding::LastChecked;
use crate::domain::message_origin::MessageOrigin;
use crate::domain::project::Project;
use crate::domain::thread::{is_news, with_finding, Thread, ThreadOutcome};
use crate::effects::{Effect, EffectWork};
use crate::events::{AutomationAck, AutomationFire};
use crate::reducer::run_queue::{pending_step, resolve_workspace_effect};
use crate::reducer::run_state::automation_run_prompt;
use crate::reducer::Step;
use crate::state::{PendingRun, RunOrigin, WorkspaceState};
use crate::util::{now, random_id};
use crate::workspace::activity::{thread_activity, thread_on_screen, thread_status, users_turn, ThreadStatus};
use crate::workspace::location::{project_for, worktree_for};
use crate::workspace::notices::{announced, coordination_note, NoticeReason};

/// Why a tick has nowhere to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickRefusal {
    NoThread,
    Archived,
    BusyUser,
    BusyAgent,
    NoWorkspace,
}

fn who_is_busy(state: &WorkspaceState, task_id: &str) -> Option<TickRefusal> {
    if thread_status(&thread_activity(state), task_id) == ThreadStatus::Idle {
        return None;
    }
    Some(if users_turn(state, task_id) { TickRefusal::BusyUser } else { TickRefusal::BusyAgent })
}

/// Why this tick cannot run, or None when it can.
pub fn why_tick_cannot_run(state: &WorkspaceState, fire: &AutomationFire, thread: Option<&Thread>, project: Option<&Project>) -> Option<TickRefusal> {
    let Some(thread) = thread else { return Some(TickRefusal::NoThread) };
    if thread.resource_stop.is_some() {
        return Some(TickRefusal::BusyUser);
    }
    if let Some(busy) = who_is_busy(state, &fire.task_id) {
        return Some(busy);
    }
    if thread.archived_at.is_some() {
        return Some(TickRefusal::Archived);
    }
    if thread.project_id.is_some() && project.is_none_or(|project| project.workspace_id.is_none()) {
        return Some(TickRefusal::NoWorkspace);
    }
    None
}

/// The scheduled run a tick is executing, and nothing else.
pub fn scheduled_run(state: &WorkspaceState, task_id: &str) -> bool {
    state.active_runs.get(task_id).is_some_and(|run| run.origin == RunOrigin::Automation)
}

/// A local date and time the way the window's locale writes one.
pub fn locale_date_time(at: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(at).single() {
        Some(moment) => moment.format("%-m/%-d/%Y, %-I:%M:%S %p").to_string(),
        None => String::new(),
    }
}

/// A tick with nowhere to run is acknowledged and dropped; one turned away again and again says so, once.
pub fn declined_tick(state: &mut WorkspaceState, fire: &AutomationFire, thread_id: Option<&str>, refusal: TickRefusal) -> Step {
    let acked: Effect = EffectWork::AutomationAck { ack: AutomationAck { automation_id: fire.automation_id.clone(), run_id: fire.run_id.clone(), started: false } }.into();
    let automation = state.automations.iter().find(|item| item.automation.id == fire.automation_id).map(|view| view.automation.clone());
    let (Some(thread_id), Some(automation)) = (thread_id, automation) else { return Step::effects(vec![acked]) };
    if decline_count(&automation) + 1 < DECLINES_BEFORE_SURFACING || refusal == TickRefusal::BusyUser {
        return Step::effects(vec![acked]);
    }
    let Some(thread) = state.thread(thread_id).cloned() else { return Step::effects(vec![acked]) };
    let key = format!("declined:{}", automation.id);
    let since = automation.last_run_at.unwrap_or(0);
    let said = !is_news(&thread, Some(&key)) || thread.findings.iter().flatten().any(|finding| finding.key.as_deref() == Some(key.as_str()) && finding.at >= since);
    if said {
        return Step::effects(vec![acked]);
    }
    let headline = format!("This automation has not been able to run since {}", locale_date_time(automation.last_run_at.unwrap_or(automation.created_at)));
    let seen = state.focused && thread_on_screen(state, &thread.id);
    let mut effects = vec![acked];
    effects.extend(announced(state, &thread, &headline, NoticeReason::Finding));
    if let Some(thread) = state.thread_mut(thread_id) {
        with_finding(thread, &headline, None, Some(&key), now(), seen);
    }
    Step::effects(effects)
}

/// The scheduler owns the cadence; the workspace decides whether this tick can actually run.
pub fn automation_fired(state: &mut WorkspaceState, fire: &AutomationFire) -> Step {
    let thread = state.thread(&fire.task_id).cloned();
    let project = thread.as_ref().and_then(|thread| project_for(state, Some(thread))).cloned();
    if let Some(refusal) = why_tick_cannot_run(state, fire, thread.as_ref(), project.as_ref()) {
        return declined_tick(state, fire, thread.as_ref().map(|thread| thread.id.as_str()), refusal);
    }
    let mut pending = PendingRun::new(random_id(), fire.run_id.clone(), RunOrigin::Automation, fire.prompt.clone(), automation_run_prompt(&fire.prompt, fire.run_number, fire.surface_when.as_deref(), fire.ends_when.as_deref()));
    pending.task_id = Some(fire.task_id.clone());
    pending.project_id = project.as_ref().map(|project| project.id.clone());
    pending.message_origin = Some(MessageOrigin::Automation { run_number: fire.run_number });
    pending.policy = fire.policy;
    pending.quiet = fire.quiet;
    pending.unattended = fire.unattended;
    pending.automation_id = Some(fire.automation_id.clone());
    let effect = resolve_workspace_effect(&pending.id, thread.as_ref(), project.as_ref(), worktree_for(state, thread.as_ref()), false, None);
    pending_step(state, pending).with(effect)
}

/// Only a scheduled run may raise a finding: a turn the user is present for answers them directly.
pub fn raised_finding(state: &mut WorkspaceState, task_id: &str, headline: &str, detail: Option<&str>, key: Option<&str>) -> Step {
    if !scheduled_run(state, task_id) {
        return Step::none();
    }
    let Some(thread) = state.thread(task_id).cloned() else { return Step::none() };
    let raised = is_news(&thread, key);
    let seen = state.focused && thread_on_screen(state, task_id);
    let effects = if raised { announced(state, &thread, headline, NoticeReason::Finding) } else { Vec::new() };
    if let Some(active) = state.active_runs.get_mut(task_id) {
        active.acknowledged = true;
        active.notified = active.notified || raised;
        if let Some(key) = key {
            if !active.reported_issues.iter().any(|issue| issue == key) {
                active.reported_issues.push(key.into());
            }
        }
    }
    if let Some(thread) = state.thread_mut(task_id) {
        with_finding(thread, headline, detail, key, now(), seen);
    }
    Step::effects(effects)
}

/// A run saying it looked and found nothing. What it says never retracts what it already surfaced.
pub fn nothing_to_report(state: &mut WorkspaceState, task_id: &str, checked: &str, at: i64) -> Step {
    if !scheduled_run(state, task_id) {
        return Step::none();
    }
    if let Some(active) = state.active_runs.get_mut(task_id) {
        active.acknowledged = true;
    }
    if let Some(thread) = state.thread_mut(task_id) {
        thread.last_checked = Some(LastChecked { at, note: checked.into() });
    }
    Step::none()
}

/// A watch that is gone has finished, so its thread leaves Running for Priority as a settled run does.
pub fn automations_changed(state: &mut WorkspaceState, automations: &[AutomationView]) -> Step {
    let before = std::mem::replace(&mut state.automations, automations.to_vec());
    let kept: HashSet<&str> = state.automations.iter().map(|view| view.automation.task_id.as_str()).collect();
    let ended: HashSet<String> = before.iter().filter(|view| is_watching(&view.automation) && !kept.contains(view.automation.task_id.as_str())).map(|view| view.automation.task_id.clone()).collect();
    if ended.is_empty() {
        return Step::none();
    }
    let at = now();
    let finished: Vec<(String, bool)> = state.threads.iter().filter(|thread| ended.contains(&thread.id) && thread.archived_at.is_none()).map(|thread| (thread.id.clone(), thread_on_screen(state, &thread.id))).collect();
    for (id, on_screen) in &finished {
        if let Some(active) = state.active_runs.get_mut(id) {
            active.before.outcome = Some(ThreadOutcome::Finished);
            active.before.run_ended_as = Some(ThreadOutcome::Finished);
            active.before.run_ended_at = Some(at);
            active.before.ranked_at = Some(at);
            if !on_screen {
                active.before.outcome_unread = true;
            }
        }
        if let Some(thread) = state.thread_mut(id) {
            thread.outcome = Some(ThreadOutcome::Finished);
            thread.run_ended_as = Some(ThreadOutcome::Finished);
            thread.run_ended_at = Some(at);
            thread.ranked_at = at;
            if !on_screen {
                thread.outcome_unread = true;
            }
        }
    }
    for (id, _) in &finished {
        let Some(thread) = state.thread(id) else { continue };
        let Some(lead_id) = coordinator_of(&state.threads, Some(thread)).map(|lead| lead.id.clone()) else { continue };
        let note = coordination_note(id, format!("\"{}\" finished watching.", thread.title), at, false, None, false);
        if let Some(lead) = state.thread_mut(&lead_id) {
            with_coordination_note(lead, note);
        }
    }
    Step::none()
}
