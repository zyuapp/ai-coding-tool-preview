//! Threads as the tools outside the window read them: summaries, listings, transcripts and the
//! thread a reference names.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::domain::conversation::{ConversationMessage, MessageKind};
use crate::domain::message_origin::message_label;
use crate::domain::project::find_project;
use crate::domain::thread::{thread_created_at, thread_last_output_at, Thread};
use crate::domain::thread_role::ThreadRole;
use crate::state::{ThreadRunStatus, WorkspaceState};
use crate::util::{js_len, js_slice_to};
use crate::workspace::activity::{run_status, thread_activity};
use crate::workspace::location::{project_for, worktree_for};

const MESSAGE_TEXT_LIMIT: usize = 2_000;
const DEFAULT_TRANSCRIPT_MESSAGES: usize = 30;

/// Which threads a query covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum ProjectScope {
    All,
    Project { project_id: String },
    Projectless,
}

/// What a caller may narrow a listing by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadFilter {
    pub scope: Option<ProjectScope>,
    pub archived: bool,
    pub idle_for_ms: Option<i64>,
    pub search: Option<String>,
    /// Threads whose unloaded history on disk matches `search`.
    pub stored: Option<HashSet<String>>,
    pub attachments: bool,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryComputer {
    pub id: String,
    pub name: String,
    pub offline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<SummaryComputer>,
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ThreadRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<String>,
    pub status: ThreadRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<i64>,
    pub archived: bool,
    pub created_at: i64,
    pub last_activity_at: i64,
    pub message_count: i64,
    pub attachment_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadMessage {
    pub kind: MessageKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadTranscript {
    pub thread: ThreadSummary,
    pub messages: Vec<ThreadMessage>,
    pub omitted: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadWaitResult {
    pub thread: ThreadSummary,
    pub timed_out: bool,
    pub reply: Option<String>,
}

/// The project scope a caller's `project` argument names.
pub fn resolve_scope(state: &WorkspaceState, caller_thread_id: &str, project: Option<&str>) -> Result<ProjectScope, String> {
    match project {
        Some("all") => Ok(ProjectScope::All),
        None | Some("current") => Ok(match state.thread(caller_thread_id).and_then(|thread| thread.project_id.clone()) {
            Some(project_id) => ProjectScope::Project { project_id },
            None => ProjectScope::Projectless,
        }),
        Some(project) => find_project(&state.projects, project).map(|found| ProjectScope::Project { project_id: found.id.clone() }),
    }
}

fn count_attachments(thread: &Thread) -> i64 {
    match &thread.history_summary {
        Some(summary) => summary.attachment_count,
        None => thread.messages.iter().filter(|message| message.attachments.as_ref().is_some_and(|items| !items.is_empty())).count() as i64,
    }
}

fn summary_of(state: &WorkspaceState, working: &HashSet<String>, thread: &Thread, activity: i64, attachments: Option<i64>) -> ThreadSummary {
    let project = project_for(state, Some(thread));
    let worktree = worktree_for(state, Some(thread));
    ThreadSummary {
        computer: None,
        id: thread.id.clone(),
        title: thread.title.clone(),
        role: thread.role,
        project_id: thread.project_id.clone(),
        project_root: project.map(|project| project.root.clone()),
        worktree_id: worktree.map(|worktree| worktree.id.clone()),
        worktree_root: worktree.map(|worktree| worktree.root.clone()),
        status: if working.contains(&thread.id) { ThreadRunStatus::Running } else { run_status(state, Some(&thread.id)) },
        paused_until: thread.limit_pause.as_ref().map(|pause| pause.resets_at),
        archived: thread.archived_at.is_some(),
        created_at: thread_created_at(thread),
        last_activity_at: activity,
        message_count: thread.history_summary.as_ref().map(|summary| summary.message_count).unwrap_or(thread.messages.len() as i64),
        attachment_count: attachments.unwrap_or_else(|| count_attachments(thread)),
    }
}

pub fn thread_summary(state: &WorkspaceState, thread: &Thread) -> ThreadSummary {
    summary_of(state, &thread_activity(state).working, thread, thread_last_output_at(thread), None)
}

fn in_scope(thread: &Thread, scope: Option<&ProjectScope>) -> bool {
    match scope {
        None | Some(ProjectScope::All) => true,
        Some(ProjectScope::Projectless) => thread.project_id.is_none(),
        Some(ProjectScope::Project { project_id }) => thread.project_id.as_ref() == Some(project_id),
    }
}

fn matches(thread: &Thread, search: &str, stored: Option<&HashSet<String>>) -> bool {
    if thread.title.to_lowercase().contains(search) {
        return true;
    }
    match &thread.history_summary {
        Some(_) => stored.is_some_and(|stored| stored.contains(&thread.id)),
        None => thread.messages.iter().any(|message| message.text.to_lowercase().contains(search)),
    }
}

/// Newest activity first, so a limit keeps the threads worth looking at.
pub fn thread_summaries(state: &WorkspaceState, filter: &ThreadFilter, at: i64) -> Vec<ThreadSummary> {
    let search = filter.search.as_ref().map(|search| search.trim().to_lowercase()).filter(|search| !search.is_empty());
    let forked = state.side_chat_ids();
    let mut matching: Vec<(&Thread, i64, Option<i64>)> = Vec::new();
    for thread in &state.threads {
        if forked.contains(&thread.id) || !in_scope(thread, filter.scope.as_ref()) || thread.archived_at.is_some() != filter.archived {
            continue;
        }
        let activity = thread_last_output_at(thread);
        if filter.idle_for_ms.is_some_and(|idle| at - activity < idle) {
            continue;
        }
        if search.as_deref().is_some_and(|search| !matches(thread, search, filter.stored.as_ref())) {
            continue;
        }
        let attachments = filter.attachments.then(|| count_attachments(thread));
        if filter.attachments && attachments == Some(0) {
            continue;
        }
        matching.push((thread, activity, attachments));
    }
    matching.sort_by_key(|(_, activity, _)| std::cmp::Reverse(*activity));
    if let Some(limit) = filter.limit {
        matching.truncate(limit);
    }
    let working = thread_activity(state).working;
    matching.into_iter().map(|(thread, activity, attachments)| summary_of(state, &working, thread, activity, attachments)).collect()
}

const MATCH_RANKS: usize = 5;

/// The threads a reference could mean, and how closely, closest first.
pub fn thread_matches<'a>(state: &'a WorkspaceState, reference: &str) -> (usize, Vec<&'a Thread>) {
    let wanted = reference.trim().to_lowercase();
    if wanted.is_empty() {
        return (MATCH_RANKS, Vec::new());
    }
    let mut ranked: Vec<Vec<&Thread>> = vec![Vec::new(); MATCH_RANKS];
    for thread in &state.threads {
        let id = thread.id.to_lowercase();
        if id == wanted {
            return (0, vec![thread]);
        }
        let archived = if thread.archived_at.is_none() { 0 } else { 2 };
        if thread.title.trim().to_lowercase() == wanted {
            ranked[1 + archived].push(thread);
        } else if id.starts_with(&wanted) {
            ranked[2 + archived].push(thread);
        }
    }
    match ranked.iter().position(|threads| !threads.is_empty()) {
        Some(rank) => (rank, std::mem::take(&mut ranked[rank])),
        None => (MATCH_RANKS, Vec::new()),
    }
}

/// The thread a reference names: its id, or the one thread it most closely is the title or an id prefix of.
pub fn find_thread<'a>(state: &'a WorkspaceState, reference: &str) -> Option<&'a Thread> {
    let (_, threads) = thread_matches(state, reference);
    (threads.len() == 1).then(|| threads[0])
}

/// Text cut to `limit` UTF-16 units with an ellipsis, never between the halves of a surrogate pair.
pub fn clip_text(text: &str, limit: usize) -> String {
    if js_len(text) <= limit {
        return text.to_string();
    }
    format!("{}…", js_slice_to(text, limit))
}

fn transcript_message(message: &ConversationMessage, text_limit: usize) -> ThreadMessage {
    ThreadMessage {
        kind: message.kind,
        text: clip_text(&message.text, text_limit),
        origin: if message.kind == MessageKind::User { message_label(message.origin.as_ref(), message.detail.as_deref()) } else { None },
        at: message.at,
    }
}

/// Up to `limit` messages ending before index `end`, the newest by default.
pub fn thread_transcript(state: &WorkspaceState, thread_id: &str, limit: Option<usize>, text_limit: Option<usize>, end: Option<usize>) -> Option<ThreadTranscript> {
    let thread = find_thread(state, thread_id)?;
    let limit = limit.unwrap_or(DEFAULT_TRANSCRIPT_MESSAGES);
    let text_limit = text_limit.unwrap_or(MESSAGE_TEXT_LIMIT);
    let stop = end.unwrap_or(thread.messages.len()).min(thread.messages.len());
    let start = stop.saturating_sub(limit);
    let kept = &thread.messages[start..stop];
    Some(ThreadTranscript { thread: thread_summary(state, thread), messages: kept.iter().map(|message| transcript_message(message, text_limit)).collect(), omitted: stop - kept.len() })
}

/// A thread's state for one waiting on it: where it stands, whether the wait ran out, and what it last said.
pub fn thread_wait_result(state: &WorkspaceState, thread_id: &str, timed_out: bool) -> Option<ThreadWaitResult> {
    let thread = find_thread(state, thread_id)?;
    let reply = thread.messages.iter().rev().find(|message| message.kind == MessageKind::Assistant).map(|message| message.text.clone());
    Some(ThreadWaitResult { thread: thread_summary(state, thread), timed_out, reply })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::engine::AgentEngine;
    use crate::domain::run::ExecutionPolicy;

    #[test]
    fn a_live_title_outranks_an_archived_one_and_ties_name_nothing() {
        let mut state = WorkspaceState::default();
        let mut archived = Thread::new("x1", "Fix login", AgentEngine::Claude, ExecutionPolicy::Confirm, 1);
        archived.archived_at = Some(2);
        state.threads.push(archived.into());
        state.threads.push(Thread::new("y1", "Fix login", AgentEngine::Claude, ExecutionPolicy::Confirm, 1).into());
        assert_eq!(find_thread(&state, "fix login").map(|thread| thread.id.as_str()), Some("y1"));
        state.threads.push(Thread::new("y2", "fix login", AgentEngine::Claude, ExecutionPolicy::Confirm, 1).into());
        assert!(find_thread(&state, "Fix login").is_none());
        assert_eq!(find_thread(&state, "x1").map(|thread| thread.id.as_str()), Some("x1"));
    }
}
