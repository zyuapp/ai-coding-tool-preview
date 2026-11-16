//! What the workspace puts on the desktop when a thread needs the user, and what a coordinator hears.

use crate::domain::coordination::{coordinator_of, is_coordinator, remote_lead_of, works_under_coordinator, CoordinationNote, CoordinationReport, CoordinationState, Decision};
use crate::domain::conversation::MessageKind;
use crate::domain::thread::Thread;
use crate::domain::tool_call::{reply_line, tool_label};
use crate::effects::{Effect, EffectWork};
use crate::events::ThreadNotice;
use crate::state::WorkspaceState;
use crate::util::{js_len, js_slice_to, random_id};

/// Why a thread wants the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeReason {
    Finished,
    Failed,
    Waiting,
    Decision,
    Request,
    Blocked,
    Finding,
}

const POSTED: usize = 120;

fn owner_of<'a>(state: &'a WorkspaceState, thread: &'a Thread) -> Option<&'a Thread> {
    match state.side_chats.iter().find(|chat| chat.id == thread.id) {
        Some(chat) => state.thread(&chat.source_thread_id),
        None => Some(thread),
    }
}

fn lead_of<'a>(state: &'a WorkspaceState, thread: Option<&'a Thread>) -> Option<&'a Thread> {
    if is_coordinator(thread) {
        return thread;
    }
    match remote_lead_of(thread) {
        None => coordinator_of(&state.threads, thread),
        Some(remote) => state.thread(&remote.thread_id).filter(|lead| is_coordinator(Some(lead))),
    }
}

/// What a notice calls the thread: a side chat and a coordinator's thread name both.
pub fn notice_title(state: &WorkspaceState, thread: &Thread) -> String {
    let owner = owner_of(state, thread);
    if owner.map(|owner| owner.id.as_str()) != Some(thread.id.as_str()) {
        return match owner {
            Some(owner) => format!("{} · {}", owner.title, thread.title),
            None => thread.title.clone(),
        };
    }
    let lead = if is_coordinator(Some(thread)) { None } else { lead_of(state, Some(thread)).map(|lead| lead.title.clone()).or_else(|| remote_lead_of(Some(thread)).map(|lead| lead.title.clone())) };
    match lead {
        Some(lead) => format!("{lead} · {}", thread.title),
        None => thread.title.clone(),
    }
}

fn heard_elsewhere(state: &WorkspaceState, thread: &Thread) -> bool {
    remote_lead_of(Some(thread)).is_some_and(|remote| !state.threads.iter().any(|item| item.id == remote.thread_id))
}

fn latest_user_origin_is_users(thread: &Thread) -> bool {
    thread.messages.iter().rev().find(|message| message.kind == MessageKind::User && !message.withdrawn).is_none_or(|message| message.origin.is_none())
}

fn wanted(state: &WorkspaceState, thread: &Thread, reason: NoticeReason) -> bool {
    if thread.role == Some(crate::domain::thread_role::ThreadRole::Reviewer) {
        return false;
    }
    if heard_elsewhere(state, thread) {
        return false;
    }
    let lead = lead_of(state, owner_of(state, thread));
    if lead.is_some_and(|lead| lead.muted) {
        return false;
    }
    if reason != NoticeReason::Finished || !works_under_coordinator(&state.threads, Some(thread)) {
        return true;
    }
    latest_user_origin_is_users(thread)
}

/// What a run waiting on the user says on the desktop.
pub fn waiting_headline_tool(tool_name: &str) -> String {
    format!("Waiting for your permission to use {}", tool_label(tool_name))
}

/// A line a thread wants in front of the user, kept back while notifications are off or unwanted.
pub fn announced(state: &WorkspaceState, thread: &Thread, headline: &str, reason: NoticeReason) -> Vec<Effect> {
    if !wanted(state, thread, reason) {
        return Vec::new();
    }
    announced_notice(state, ThreadNotice { task_id: thread.id.clone(), title: notice_title(state, thread), headline: headline.into() })
}

pub fn announced_notice(state: &WorkspaceState, notice: ThreadNotice) -> Vec<Effect> {
    if !state.notifications {
        return Vec::new();
    }
    vec![EffectWork::AnnounceThread { notice }.into()]
}

/// The newest assistant reply in a thread, from `from` on.
pub fn last_reply(thread: &Thread, from: usize) -> Option<String> {
    thread.messages.iter().skip(from).rev().find(|message| message.kind == MessageKind::Assistant && !message.text.trim().is_empty()).map(|message| message.text.trim().to_string())
}

/// The line a coordinator's reply puts on the desktop.
pub fn posted_headline(thread: &Thread, from: usize) -> Option<String> {
    last_reply(thread, from).and_then(|reply| reply_line(&reply, POSTED))
}

/// Why a turn reaches the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunSurfacing {
    Failed,
    Cancelled,
    Attended,
    Loud,
    Reported,
    NoAnswer,
}

/// The line a settling run puts on the desktop, or None.
pub fn settled_headline(surfacing: Option<RunSurfacing>, message: Option<&str>) -> Option<String> {
    match surfacing {
        Some(RunSurfacing::Failed) => Some(message.map(str::trim).filter(|message| !message.is_empty()).unwrap_or("The run failed.").to_string()),
        Some(RunSurfacing::Attended) => Some("The run finished.".into()),
        _ => None,
    }
}

fn clipped(text: &str, max: usize) -> String {
    if js_len(text) > max {
        format!("{}…", js_slice_to(text, max - 1))
    } else {
        text.to_string()
    }
}

pub fn coordination_note(thread_id: &str, text: String, at: i64, urgent: bool, run_id: Option<String>, quiet: bool) -> CoordinationNote {
    CoordinationNote { id: random_id(), thread_id: thread_id.into(), text, at, urgent, run_id, quiet }
}

/// What a coordinator hears when one of its threads ends a turn.
pub fn turn_note(thread: &Thread, status: crate::domain::run::RunStatus, at: i64, quiet: bool) -> CoordinationNote {
    use crate::domain::run::RunStatus;
    let ended = match status {
        RunStatus::Succeeded => "ended its turn",
        RunStatus::Failed => "failed",
        _ => "was stopped",
    };
    let reply = last_reply(thread, 0).map(|reply| format!(" It last said: {}", clipped(&reply, 600))).unwrap_or_default();
    coordination_note(&thread.id, format!("\"{}\" {ended}.{reply}", thread.title), at, status == RunStatus::Failed && !quiet, None, quiet)
}

pub fn report_note(thread: &Thread, report: &CoordinationReport, at: i64, run_id: Option<String>) -> CoordinationNote {
    let state = serde_json::to_value(report.state).ok().and_then(|value| value.as_str().map(String::from)).unwrap_or_default();
    coordination_note(&thread.id, format!("\"{}\" reported {state}: {}", thread.title, report.summary), at, report.state != CoordinationState::Done, run_id, false)
}

pub fn decision_note(thread: &Thread, decision: &Decision, at: i64, run_id: Option<String>) -> CoordinationNote {
    coordination_note(&thread.id, format!("\"{}\" asked the user to decide: {}", thread.title, decision.question), at, false, run_id, false)
}

pub fn report_headline(report: &CoordinationReport) -> String {
    format!("{}: {}", if report.state == CoordinationState::Blocked { "Blocked" } else { "Failed" }, report.summary)
}

pub fn decision_headline(decision: &Decision) -> String {
    format!("Needs you: {}", decision.question)
}
