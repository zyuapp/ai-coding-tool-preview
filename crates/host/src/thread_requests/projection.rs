//! Threads as the tools read them: summaries, transcripts, and which thread a loose reference names.

use std::collections::{HashMap, HashSet};

use act_core::domain::conversation::MessageKind;
use act_core::domain::message_origin::message_label;
use act_core::domain::project::{find_project, Project};
use act_core::domain::thread::{thread_created_at, thread_last_output_at, Thread};
use act_core::domain::worktree::Worktree;
use act_core::state::ThreadRunStatus;
use act_core::util::{js_len, js_slice_to};
use act_core::workspace::activity::{run_status, thread_activity};
use act_core::workspace::location::{project_for, worktree_for};
use act_core::WorkspaceState;
use serde::Serialize;

use crate::tools::types::{ThreadMessage, ThreadStatus, ThreadSummary, ThreadTranscript, ThreadWaitResult};

/// Enough of a message to recognise what happened without carrying a whole transcript.
const MESSAGE_TEXT_LIMIT: usize = 2_000;
pub const DEFAULT_TRANSCRIPT_MESSAGES: usize = 30;
/// How closely threads match a reference, closest first.
pub const MATCH_RANKS: usize = 5;

/// Which threads a query covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectScope {
    All,
    Project(String),
    Projectless,
}

/// What a listing narrows by. `stored` names unloaded threads whose stored messages match the search.
#[derive(Debug, Clone, Default)]
pub struct ThreadFilter {
    pub archived: bool,
    pub idle_for_ms: Option<f64>,
    pub search: Option<String>,
    pub attachments: bool,
    pub limit: Option<f64>,
    pub stored: Option<HashSet<String>>,
}

pub(crate) fn wire_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value).ok().and_then(|value| value.as_str().map(str::to_string)).unwrap_or_default()
}

pub fn resolve_scope(state: &WorkspaceState, caller: &str, project: Option<&str>) -> Result<ProjectScope, String> {
    match project {
        Some("all") => Ok(ProjectScope::All),
        None | Some("current") => Ok(match state.thread(caller).and_then(|thread| thread.project_id.clone()) {
            Some(project_id) => ProjectScope::Project(project_id),
            None => ProjectScope::Projectless,
        }),
        Some(reference) => find_project(&state.projects, reference).map(|project| ProjectScope::Project(project.id.clone())),
    }
}

struct ProjectionIndex<'a> {
    projects: HashMap<&'a str, &'a Project>,
    worktrees: HashMap<&'a str, &'a Worktree>,
    working: HashSet<String>,
}

fn projection_index(state: &WorkspaceState) -> ProjectionIndex<'_> {
    let mut projects = HashMap::new();
    for project in &state.projects {
        projects.entry(project.id.as_str()).or_insert(project);
    }
    let mut worktrees = HashMap::new();
    for worktree in &state.worktrees {
        worktrees.entry(worktree.id.as_str()).or_insert(worktree);
    }
    ProjectionIndex { projects, worktrees, working: thread_activity(state).working }
}

fn count_attachments(thread: &Thread) -> u64 {
    if let Some(summary) = &thread.history_summary {
        return summary.attachment_count.max(0) as u64;
    }
    thread.messages.iter().filter(|message| message.attachments.as_ref().is_some_and(|files| !files.is_empty())).count() as u64
}

fn summarize(state: &WorkspaceState, thread: &Thread, activity: i64, index: &ProjectionIndex, attachments: Option<u64>) -> ThreadSummary {
    let project = thread.project_id.as_deref().and_then(|id| index.projects.get(id).copied());
    let worktree = thread.worktree_id.as_deref().and_then(|id| index.worktrees.get(id).copied());
    let status = if index.working.contains(&thread.id) {
        ThreadStatus::Running
    } else {
        match run_status(state, Some(&thread.id)) {
            ThreadRunStatus::Idle => ThreadStatus::Idle,
            ThreadRunStatus::Running => ThreadStatus::Running,
            ThreadRunStatus::Stopped => ThreadStatus::Stopped,
        }
    };
    ThreadSummary {
        computer: None,
        id: thread.id.clone(),
        title: thread.title.clone(),
        role: thread.role.as_ref().map(wire_name),
        project_id: thread.project_id.clone(),
        project_root: project.map(|project| project.root.clone()),
        worktree_id: worktree.map(|worktree| worktree.id.clone()),
        worktree_root: worktree.map(|worktree| worktree.root.clone()),
        status,
        paused_until: thread.limit_pause.as_ref().map(|pause| pause.resets_at),
        archived: thread.archived_at.is_some(),
        created_at: thread_created_at(thread),
        last_activity_at: activity,
        message_count: thread.history_summary.as_ref().map(|summary| summary.message_count.max(0) as u64).unwrap_or(thread.messages.len() as u64),
        attachment_count: attachments.unwrap_or_else(|| count_attachments(thread)),
    }
}

pub fn thread_summary(state: &WorkspaceState, thread: &Thread) -> ThreadSummary {
    let project = project_for(state, Some(thread));
    let worktree = worktree_for(state, Some(thread));
    let index = ProjectionIndex {
        projects: project.map(|project| (project.id.as_str(), project)).into_iter().collect(),
        worktrees: worktree.map(|worktree| (worktree.id.as_str(), worktree)).into_iter().collect(),
        working: thread_activity(state).working,
    };
    summarize(state, thread, thread_last_output_at(thread), &index, None)
}

fn in_scope(thread: &Thread, scope: &ProjectScope) -> bool {
    match scope {
        ProjectScope::All => true,
        ProjectScope::Projectless => thread.project_id.is_none(),
        ProjectScope::Project(id) => thread.project_id.as_deref() == Some(id.as_str()),
    }
}

fn matches(thread: &Thread, search: &str, stored: Option<&HashSet<String>>) -> bool {
    if thread.title.to_lowercase().contains(search) {
        return true;
    }
    if thread.history_summary.is_some() {
        return stored.is_some_and(|stored| stored.contains(&thread.id));
    }
    thread.messages.iter().any(|message| message.text.to_lowercase().contains(search))
}

/// Newest activity first, so a limit keeps the threads worth looking at.
pub fn thread_summaries(state: &WorkspaceState, scope: &ProjectScope, filter: &ThreadFilter, at: i64) -> Vec<ThreadSummary> {
    let search = filter.search.as_deref().map(|search| search.trim().to_lowercase()).filter(|search| !search.is_empty());
    let forked = state.side_chat_ids();
    let mut matching: Vec<(&Thread, i64, Option<u64>)> = Vec::new();
    for thread in &state.threads {
        if forked.contains(&thread.id) || !in_scope(thread, scope) || thread.archived_at.is_some() != filter.archived {
            continue;
        }
        let activity = thread_last_output_at(thread);
        if filter.idle_for_ms.is_some_and(|idle| ((at - activity) as f64) < idle) {
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
    let limit = filter.limit.map(|limit| if limit.is_nan() { 0 } else { limit.max(0.0).floor() as usize });
    if matching.is_empty() || limit == Some(0) {
        return Vec::new();
    }
    matching.sort_by(|left, right| right.1.cmp(&left.1));
    if let Some(limit) = limit {
        matching.truncate(limit);
    }
    let index = projection_index(state);
    matching.into_iter().map(|(thread, activity, attachments)| summarize(state, thread, activity, &index, attachments)).collect()
}

/// The threads a reference could mean, and how closely: the one whose id it is, else the live threads
/// it is the title of, else the live ones whose id it begins, then the same two among archived threads.
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

/// The thread a reference names, or None when it names none or more than one.
pub fn find_thread<'a>(state: &'a WorkspaceState, reference: &str) -> Option<&'a Thread> {
    let (_, threads) = thread_matches(state, reference);
    (threads.len() == 1).then(|| threads[0])
}

pub fn thread_wait_result(state: &WorkspaceState, thread_id: &str, timed_out: bool) -> Option<ThreadWaitResult> {
    let thread = find_thread(state, thread_id)?;
    let reply = thread.messages.iter().rev().find(|message| message.kind == MessageKind::Assistant).map(|message| message.text.clone());
    Some(ThreadWaitResult { thread: thread_summary(state, thread), timed_out, reply })
}

/// Text cut to `limit` UTF-16 units with an ellipsis, never between the halves of a surrogate pair.
pub fn clip_text(text: &str, limit: usize) -> String {
    if js_len(text) <= limit {
        return text.to_string();
    }
    format!("{}…", js_slice_to(text, limit))
}

/// Up to `limit` messages, the newest.
pub fn thread_transcript(state: &WorkspaceState, thread_id: &str, limit: Option<usize>) -> Option<ThreadTranscript> {
    let thread = find_thread(state, thread_id)?;
    let limit = limit.unwrap_or(DEFAULT_TRANSCRIPT_MESSAGES);
    let stop = thread.messages.len();
    let kept = &thread.messages[stop.saturating_sub(limit)..stop];
    Some(ThreadTranscript {
        thread: thread_summary(state, thread),
        messages: kept
            .iter()
            .map(|message| ThreadMessage {
                kind: wire_name(&message.kind),
                text: clip_text(&message.text, MESSAGE_TEXT_LIMIT),
                origin: (message.kind == MessageKind::User).then(|| message_label(message.origin.as_ref(), message.detail.as_deref())).flatten(),
                at: message.at,
            })
            .collect(),
        omitted: (stop - kept.len()) as u64,
    })
}
