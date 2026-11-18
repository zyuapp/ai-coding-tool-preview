//! The store answering: threads restored on launch, a conversation loaded on demand, preferences landing.

use crate::shared::Shared;
use std::collections::HashSet;

use crate::domain::coordination::{coordinator_of, may_lead_elsewhere};
use crate::domain::engine::DEFAULT_ENGINE;
use crate::domain::platform::{browser_url, BrowserTab};
use crate::domain::project::{legacy_project_id, Project};
use crate::domain::run::{Subagent, SubagentStatus, DEFAULT_EFFORT};
use crate::domain::thread::{thread_last_output_at, Thread, ThreadOutcome};
use crate::domain::thread_role::ThreadRole;
use crate::domain::worktree::Worktree;
use crate::events::WorkspaceEvent;
use crate::persisted::{StoredThread, ThreadStoreData};
use crate::reducer::run_queue::resolve_workspace_effect;
use crate::reducer::settlement::{settle_turn, turn_settled, StopIntent, TurnEnd};
use crate::reducer::{finish, Step};
use crate::state::{dock_for, PendingOperation, PendingRun, ResumeAfter, RunOrigin, WorkspaceState, MAX_DISCARDED_LEADS};
use crate::util::{now, random_id, Map};
use crate::view::sidebar::backfill_project_sort_index;
use crate::workspace::activity::thread_busy;
use crate::workspace::location::{project_for, worktree_for};
use crate::workspace::order::backfill_sort_index;
use crate::workspace::preferences::apply_view_preferences;

pub const ARCHIVE_RETENTION_MS: i64 = 5 * 24 * 60 * 60 * 1000;
pub const UPDATE_RESUME_PROMPT: &str = "The app restarted to install an update, which cut off your last turn. Check whether anything it was doing was left half-done, then continue where you left off.";

/// A stored thread read into the workspace's own shape. The persisted record writes the same JSON.
pub fn thread_from_stored(stored: &StoredThread) -> Option<(Thread, Vec<Subagent>, bool)> {
    let mut value = serde_json::to_value(stored).ok()?;
    let subagents = value.as_object_mut().and_then(|fields| fields.remove("subagents"));
    let ranked = value.get("rankedAt").is_some();
    let mut thread = Thread::from_json(&value).ok()?;
    if !ranked {
        thread.ranked_at = thread_last_output_at(&thread);
    }
    let subagents: Vec<Subagent> = subagents.and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default();
    Some((thread, subagents, ranked))
}

fn project_from_stored(project: &crate::persisted::Project) -> Option<Project> {
    serde_json::from_value(serde_json::to_value(project).ok()?).ok()
}

fn worktree_from_stored(worktree: &crate::persisted::Worktree) -> Option<Worktree> {
    serde_json::from_value(serde_json::to_value(worktree).ok()?).ok()
}

/// A subagent stored mid-work stopped with the app.
fn restored_subagent(mut subagent: Subagent) -> Subagent {
    if subagent.status == SubagentStatus::Working {
        subagent.stopping = false;
        subagent.status = SubagentStatus::Stopped;
    }
    subagent
}

/// A thread saved mid-turn was stopped with the app, a moment after the last thing it did.
fn restored_thread(thread: &mut Thread) -> bool {
    if let Some(stop) = thread.resource_stop.as_mut() {
        stop.cleanup = crate::domain::session_resources::ResourceCleanup::Complete;
    }
    if !thread.run_open {
        return false;
    }
    thread.run_open = false;
    let decisions = thread.decisions.iter().flatten().map(|decision| decision.raised_at).max().unwrap_or(0);
    let moved = thread.updated_at.max(thread_last_output_at(thread)).max(thread.report.as_ref().map(|report| report.at).unwrap_or(0)).max(thread.last_finding_at.unwrap_or(0)).max(thread.remote_lead.as_ref().and_then(|lead| lead.back_at).unwrap_or(0)).max(decisions);
    thread.run_ended_at = Some(moved + 1);
    thread.ranked_at = moved + 1;
    thread.run_ended_as = Some(ThreadOutcome::Stopped);
    true
}

/// The workspace a store hands over, with nothing of the session around it yet.
pub fn state_from_data(data: &ThreadStoreData, engine_status: Option<crate::domain::engine::EngineStatus>, resuming: &HashSet<String>) -> WorkspaceState {
    let mut projects: Vec<Project> = data.projects.iter().filter_map(project_from_stored).collect();
    if let Some(last) = &data.last_folder {
        if !projects.iter().any(|project| &project.root == last) {
            projects.push(Project { id: legacy_project_id(last), root: last.clone(), name: None, workspace_id: None, sort_index: None, unlisted: false });
        }
    }
    let at = now();
    let mut threads = Vec::new();
    let mut subagents: Map<Vec<Subagent>> = Map::new();
    let mut cut_off = Vec::new();
    for stored in &data.tasks {
        let Some((mut thread, delegated, _)) = thread_from_stored(stored) else { continue };
        if thread.archived_at.is_some_and(|archived| at - archived >= ARCHIVE_RETENTION_MS) {
            continue;
        }
        if !delegated.is_empty() {
            subagents.insert(thread.id.clone(), delegated.into_iter().map(restored_subagent).collect());
        }
        if restored_thread(&mut thread) {
            cut_off.push(thread.id.clone());
        }
        threads.push(Shared::new(thread));
    }
    let first = threads.iter().find(|thread| coordinator_of(&threads, Some(thread)).is_none()).cloned();
    let draft_thread = threads.iter().find(|thread| coordinator_of(&threads, Some(thread)).is_none() && thread.role != Some(ThreadRole::Reviewer)).cloned();
    let first_project = match &first {
        Some(first) => first.project_id.clone(),
        None => projects.iter().find(|project| Some(&project.root) == data.last_folder.as_ref()).map(|project| project.id.clone()),
    };
    backfill_sort_index(&mut threads);
    backfill_project_sort_index(&mut projects);
    let mut state = WorkspaceState::empty(None);
    state.engine_status = engine_status;
    state.threads = threads;
    state.subagents = subagents;
    state.projects = projects;
    state.worktrees = data.worktrees.iter().filter_map(worktree_from_stored).collect();
    state.last_folder = data.last_folder.clone();
    state.current_id = first.as_ref().map(|thread| thread.id.clone());
    state.history = first.as_ref().map(|thread| vec![thread.id.clone()]).unwrap_or_default();
    state.history_index = if first.is_some() { 0 } else { -1 };
    state.draft_project_id = first_project;
    state.with_draft(|draft| {
        draft.engine = first.as_ref().map(|thread| thread.engine).unwrap_or(DEFAULT_ENGINE);
        draft.model = first.as_ref().and_then(|thread| thread.model.clone());
        draft.effort = first.as_ref().and_then(|thread| thread.effort).unwrap_or(DEFAULT_EFFORT);
        draft.policy = draft_thread.as_ref().map(|thread| thread.execution_policy).unwrap_or_default();
        draft.fast_mode = first.as_ref().and_then(|thread| thread.fast_mode).unwrap_or(false);
    });
    let settlement = settle_turn(TurnEnd { status: crate::domain::run::RunStatus::Cancelled, message: None, limit: None, run: None, stop: Some(StopIntent::Quit) });
    for task_id in cut_off.iter().filter(|id| !resuming.contains(*id)) {
        let _ = turn_settled(&mut state, task_id, &settlement, None, at);
    }
    state
}

fn session_started(state: &WorkspaceState) -> bool {
    state.current_id.as_ref().is_some_and(|id| state.thread(id).is_some())
        || state.draft_project_id.is_some()
        || !state.prompts.is_empty()
        || !state.annotations.is_empty()
        || !state.pastes.is_empty()
        || !state.images.is_empty()
        || !state.files.is_empty()
        || !state.pending_runs.is_empty()
}

/// The store's threads landing in a session that may already be under way.
pub fn with_store_data(state: &mut WorkspaceState, data: &ThreadStoreData, resuming: &HashSet<String>) {
    let landing = state_from_data(data, state.engine_status.clone(), resuming);
    let mut held = state.side_chat_ids();
    held.extend(state.active_runs.keys().cloned());
    held.extend(state.pending_runs.values().filter_map(|pending| pending.task_id.clone()));
    held.extend(state.creating_worktrees.iter().cloned());
    let stored: HashSet<String> = landing.threads.iter().map(|thread| thread.id.clone()).collect();
    let mut threads = landing.threads.clone();
    threads.extend(state.threads.iter().filter(|thread| held.contains(&thread.id) && !stored.contains(&thread.id)).cloned());
    let landing_worktrees: HashSet<String> = landing.worktrees.iter().map(|worktree| worktree.id.clone()).collect();
    let claimed: HashSet<String> = threads.iter().filter_map(|thread| thread.worktree_id.clone()).collect();
    let mut worktrees = landing.worktrees.clone();
    worktrees.extend(state.worktrees.iter().filter(|worktree| !landing_worktrees.contains(&worktree.id) && claimed.contains(&worktree.id)).cloned());
    let mut subagents = landing.subagents.clone();
    for (id, held) in &state.subagents {
        subagents.insert(id.clone(), held.clone());
    }
    state.threads = threads;
    state.subagents = subagents;
    state.worktrees = worktrees;
    state.projects = landing.projects.clone();
    state.last_folder = landing.last_folder.clone();
    state.storage_error = landing.storage_error.clone();
    state.writable = landing.writable;
    if session_started(state) {
        return;
    }
    state.current_id = landing.current_id.clone();
    state.history = landing.history.clone();
    state.history_index = landing.history_index;
    state.draft_project_id = landing.draft_project_id.clone();
    let draft = landing.draft();
    state.with_draft(|change| {
        change.engine = draft.engine;
        change.model = Some(draft.model.clone());
        change.effort = draft.effort;
        change.policy = draft.policy;
        change.fast_mode = draft.fast_mode;
    });
}

/// Threads whose turn an update's restart cut off pick up where they were.
fn resumed_after_update(state: &mut WorkspaceState, task_ids: &[String]) -> Vec<crate::effects::Effect> {
    let mut effects = Vec::new();
    for task_id in task_ids {
        let Some(thread) = state.thread(task_id).cloned() else { continue };
        if thread.archived_at.is_some() || thread_busy(state, task_id) {
            continue;
        }
        let project = project_for(state, Some(&thread)).cloned();
        let mut pending = PendingRun::new(random_id(), random_id(), RunOrigin::Composer, "", UPDATE_RESUME_PROMPT);
        pending.operation = Some(PendingOperation::Resume { after: ResumeAfter::Update });
        pending.task_id = Some(task_id.clone());
        pending.project_id = project.as_ref().map(|project| project.id.clone());
        let effect = resolve_workspace_effect(&pending.id, Some(&thread), project.as_ref(), worktree_for(state, Some(&thread)), false, None);
        state.pending_runs.insert(pending.id.clone(), pending);
        effects.push(effect.into());
    }
    effects
}

/// Coordinators gone from here whose threads on paired computers may still work under them.
pub fn leads_discarded(state: &mut WorkspaceState, threads: &[Shared<Thread>]) -> Step {
    let held: HashSet<String> = state.computers.discarded_leads.iter().cloned().collect();
    let leads: Vec<String> = threads.iter().filter(|thread| may_lead_elsewhere(thread) && !held.contains(&thread.id)).map(|thread| thread.id.clone()).collect();
    if leads.is_empty() {
        return Step::none();
    }
    state.computers.discarded_leads.extend(leads);
    let excess = state.computers.discarded_leads.len().saturating_sub(MAX_DISCARDED_LEADS);
    state.computers.discarded_leads.drain(..excess);
    Step::persisting()
}

pub fn reduce_store(state: &mut WorkspaceState, event: &WorkspaceEvent) -> crate::reducer::Applied {
    match event {
        WorkspaceEvent::StoreLoaded { data, hidden_tasks, resuming } => {
            let resuming_ids: HashSet<String> = data.tasks.iter().filter(|stored| stored.task.run_open && resuming.as_ref().is_some_and(|ids| ids.contains(&stored.task.id))).map(|stored| stored.task.id.clone()).collect();
            with_store_data(state, data, &resuming_ids);
            let kept: HashSet<String> = state.threads.iter().map(|thread| thread.id.clone()).collect();
            let dropped: Vec<Shared<Thread>> = data.tasks.iter().filter(|stored| !kept.contains(&stored.task.id)).filter_map(|stored| thread_from_stored(stored).map(|(thread, _, _)| Shared::new(thread))).collect();
            state.hidden_threads = hidden_tasks.unwrap_or(0);
            state.restored = true;
            let step = leads_discarded(state, &dropped);
            let mut transition = finish(state, step);
            let ordered: Vec<String> = resuming_ids.into_iter().collect();
            transition.effects.extend(resumed_after_update(state, &ordered));
            transition.into()
        }
        WorkspaceEvent::PreferencesLoaded { preferences } => {
            for (owner, urls) in preferences.browser_tabs.clone().unwrap_or_default() {
                let tabs: Vec<BrowserTab> = urls.iter().filter_map(|url| browser_url(url)).map(|url| BrowserTab { id: random_id(), url, title: String::new(), loading: false, can_go_back: false, can_go_forward: false, error: None }).collect();
                if !tabs.is_empty() {
                    let mut dock = dock_for(state, &owner).clone();
                    dock.browser_tab_id = Some(tabs[0].id.clone());
                    dock.browser_tabs = tabs;
                    state.docks.insert(owner.clone(), dock);
                }
            }
            state.computers.discarded_leads = preferences.discarded_leads.clone().unwrap_or_default();
            apply_view_preferences(state, preferences);
            state.browser_import.last = preferences.browser_import.clone();
            Step::none().into()
        }
        WorkspaceEvent::StoreThreadLoaded { task_id, messages } => {
            if let Some(thread) = state.thread_mut(task_id) {
                if thread.history_summary.is_some() {
                    thread.history_summary = None;
                    thread.messages = messages.clone();
                }
            }
            Step::none().into()
        }
        WorkspaceEvent::StoreAbsent => {
            state.restored = true;
            Step::none().into()
        }
        WorkspaceEvent::StoreFailed { message } => {
            state.writable = false;
            state.storage_error = Some(message.clone());
            state.restored = true;
            Step::none().into()
        }
        WorkspaceEvent::ViewClosed => {
            state.focused = false;
            state.viewing_image = None;
            state.viewing_gallery.clear();
            for dock in state.docks.values_mut() {
                if dock.terminals.iter().any(|terminal| terminal.id == dock.tab) {
                    dock.tab = crate::state::DOCK_PICKER.into();
                }
                dock.terminals.clear();
                dock.terminal_id = None;
                for page in dock.browser_tabs.iter_mut() {
                    page.error = None;
                    page.loading = false;
                    page.can_go_back = false;
                    page.can_go_forward = false;
                }
            }
            state.keyboard_tab = None;
            Step::none().into()
        }
        _ => Step::none().into(),
    }
}
