//! Where a thread works: the branch it starts from, the checkouts the app keeps, moving between
//! them, and the list Settings manages.

use crate::shared::Shared;
use std::collections::HashSet;

use crate::commands::AppCommand;
use crate::domain::conversation::{create_conversation_message, create_failure_message, ConversationMessage, MessageKind};
use crate::domain::message_origin::{origin_fields, MessageOrigin};
use crate::domain::run::RunStatus;
use crate::domain::thread::{dismissed, Thread};
use crate::domain::usage_limit::HeldMessage;
use crate::domain::worktree::{worktree_name, MoveAfterRun, Worktree, WorktreeDestination};
use crate::effects::{Effect, EffectWork};
use crate::events::{ChangedFilesResult, CommandResult, WorkspaceEvent, WorkspaceInput, WorktreeSnapshotResult};
use crate::reducer::dock::{diff_reread, refresh_environment};
use crate::reducer::errors::{
    coordinator_ask_refusal, CoordinatorAsk, SWITCH_PROJECT_ERROR, SWITCH_RUNNING_ERROR, WORKTREE_CREATING_ERROR, WORKTREE_MISSING_ERROR, WORKTREE_PROJECT_ERROR, WORKTREE_RELEASING_ERROR, WORKTREE_RUNNING_ERROR,
};
use crate::reducer::run_queue::{drained, queued_for, set_queued, QueueIntent};
use crate::reducer::{follow_ups, target_id, Step};
use crate::state::{BulkDelete, DraftBranch, QueuedMessage, WorkspaceState, WorktreeMenuList, WorktreeMove, WorktreeSettingsState};
use crate::util::{last_segment, now, random_id};
use crate::view::worktrees::worktree_settings_views;
use crate::workspace::activity::{runs_in_workspace, thread_activity};
use crate::workspace::location::{leaving_thread_ids, project_for, thread_workspace_id, thread_workspace_root, worktree_by_id, worktree_claimants, worktree_for};

fn is_working(state: &WorkspaceState, task_id: &str) -> bool {
    thread_activity(state).working.contains(task_id)
}

fn clears_error(state: &mut WorkspaceState) {
    state.action_error = None;
}

fn refusal_with_management(state: &mut WorkspaceState, message: &str) -> Step {
    state.worktree_management_error = Some(message.into());
    Step::result(CommandResult::failed(message))
}

/// A thread moved to another checkout, or the project's own: it says so, its changes are counted
/// afresh, and a session it had is forked on its next run.
fn relocated_in(state: &mut WorkspaceState, task_id: &str, worktree_id: Option<&str>, note: ConversationMessage) {
    let at = now();
    let Some(thread) = state.thread_mut(task_id) else { return };
    thread.worktree_id = worktree_id.map(String::from);
    thread.worktree_entered_at = None;
    thread.messages.push(Shared::new(note));
    thread.last_change_snapshot = crate::domain::thread::ChangeSnapshot { files: Vec::new(), captured_at: at };
    thread.updated_at = at;
    if thread.continuation.is_some() {
        thread.inherited_continuation = true;
    }
}

/// A location change preserves the transcript and forks its continuation on the next run.
pub fn relocated(state: &mut WorkspaceState, task_id: &str, destination: Option<&Worktree>, asked: bool) -> Step {
    let Some(thread) = state.thread(task_id) else { return Step::none() };
    let label = destination.map(worktree_name).unwrap_or_else(|| "Local".into());
    let root = destination.map(|worktree| worktree.root.clone()).or_else(|| project_for(state, Some(thread)).map(|project| project.root.clone()));
    let note = create_conversation_message(MessageKind::System, format!("Moved to {label}. Existing file changes stay in the previous checkout."), root);
    relocated_in(state, task_id, destination.map(|worktree| worktree.id.as_str()), note);
    if asked && state.current_id.as_deref() == Some(task_id) {
        state.open_menu = None;
    }
    let mut effects = diff_reread(state, task_id);
    if let Some(workspace_id) = thread_workspace_id(state, state.thread(task_id)) {
        effects.push(Effect::follow_up(EffectWork::RefreshEnvironment { workspace_id, task_id: Some(task_id.into()), run_id: None }));
    }
    Step::effects(effects)
}

/// Where a move lands: Ok(None) for the project's own checkout, Err(None) when the thread is already there, or Err(why).
fn arrival(state: &WorkspaceState, thread: &Thread, project_id: &str, destination: &WorktreeDestination) -> Result<Option<Worktree>, Option<String>> {
    let id = match destination {
        WorktreeDestination::Local => return if thread.worktree_id.is_some() { Ok(None) } else { Err(None) },
        WorktreeDestination::New => return Ok(None),
        WorktreeDestination::Worktree { id } => id,
    };
    let Some(worktree) = worktree_by_id(state, Some(id)) else { return Err(Some(WORKTREE_MISSING_ERROR.into())) };
    if worktree.project_id != project_id {
        return Err(Some("Choose a worktree in this thread's project.".into()));
    }
    if thread.worktree_id.as_deref() == Some(worktree.id.as_str()) {
        return Err(None);
    }
    if state.deleting_worktrees.contains(&worktree.root) || state.threads.iter().any(|item| item.worktree_id.as_deref() == Some(worktree.id.as_str()) && state.releasing_worktrees.contains(&item.id)) {
        return Err(Some(WORKTREE_RELEASING_ERROR.into()));
    }
    if let Some(managed) = &state.managed_worktrees {
        if managed.iter().find(|item| item.root == worktree.root).is_none_or(|item| item.repository.is_none()) {
            return Err(Some(WORKTREE_MISSING_ERROR.into()));
        }
    }
    Ok(Some(worktree.clone()))
}

fn creating_worktree(state: &mut WorkspaceState, task_id: &str) {
    state.creating_worktrees.push(task_id.into());
    clears_error(state);
}

fn to_new_worktree(state: &mut WorkspaceState, task_id: &str, project_id: &str, then: Option<String>) -> Step {
    let Some(thread) = state.thread(task_id).cloned() else { return Step::none() };
    let root = thread_workspace_root(state, Some(&thread)).unwrap_or_default();
    creating_worktree(state, task_id);
    Step::effect(EffectWork::CreateWorktree { task_id: task_id.into(), project_root: root, r#move: Some(true), name: Some(thread.title), project_id: Some(project_id.into()), then })
}

/// What a thread that moved itself said it goes on with, first in line in the checkout it moved to.
pub fn follow_up_queued(state: &mut WorkspaceState, task_id: &str, then: Option<&str>) -> Step {
    let (Some(then), Some(thread)) = (then, state.thread(task_id)) else { return Step::none() };
    let (origin, detail) = origin_fields(Some(MessageOrigin::Thread { thread_id: Some(thread.id.clone()), title: thread.title.clone(), computer: None }));
    let follow_up = QueuedMessage {
        held: HeldMessage {
            id: random_id(),
            text: then.into(),
            prompt: format!("You moved to this checkout, as you asked. Go on with what you said you would do next:\n\n{then}"),
            attachments: Vec::new(),
            annotations: None,
            pastes: None,
            files: None,
            origin,
            detail,
            answers: None,
            users_own: false,
        },
        steering: false,
    };
    let mut queued = vec![follow_up];
    queued.extend(queued_for(state, task_id));
    set_queued(state, task_id, queued);
    Step::none()
}

fn move_after_run_dropped(state: &mut WorkspaceState, task_id: &str) {
    state.moves_after_run.shift_remove(task_id);
}

/// `task.move-worktree`: a thread moved to its project's checkout, a new worktree, or an existing one.
pub fn worktree_moved(state: &mut WorkspaceState, task_id: Option<&str>, destination: &WorktreeDestination, after_run: bool, sender: bool, then: Option<&str>) -> Step {
    let Some(thread) = target_id(state, task_id).and_then(|id| state.thread(&id)).cloned() else { return Step::refuse(WORKTREE_PROJECT_ERROR) };
    let Some(project) = project_for(state, Some(&thread)).filter(|project| project.workspace_id.is_some()).cloned() else { return Step::refuse(WORKTREE_PROJECT_ERROR) };
    if state.creating_worktrees.contains(&thread.id) {
        return Step::refuse(WORKTREE_CREATING_ERROR);
    }
    if leaving_thread_ids(state).contains(&thread.id) {
        return Step::refuse(WORKTREE_RELEASING_ERROR);
    }
    if after_run || sender {
        if let Some(refusal) = coordinator_ask_refusal(&thread, CoordinatorAsk::MoveWorktree(destination), false) {
            return Step::refuse(refusal);
        }
    }
    let target = match arrival(state, &thread, &project.id, destination) {
        Ok(target) => target,
        Err(Some(refusal)) => return Step::refuse(refusal),
        Err(None) => {
            move_after_run_dropped(state, &thread.id);
            return Step::none();
        }
    };
    if is_working(state, &thread.id) {
        if !after_run || !state.active_runs.contains_key(&thread.id) {
            return Step::refuse(WORKTREE_RUNNING_ERROR);
        }
        clears_error(state);
        state.moves_after_run.insert(thread.id.clone(), MoveAfterRun { destination: destination.clone(), then: then.map(String::from) });
        return Step::none();
    }
    move_after_run_dropped(state, &thread.id);
    if *destination == WorktreeDestination::New {
        return to_new_worktree(state, &thread.id, &project.id, then.map(String::from));
    }
    clears_error(state);
    let moving = relocated(state, &thread.id, target.as_ref(), true);
    let Some(then) = then else { return moving };
    let queued = follow_up_queued(state, &thread.id, Some(then));
    let delivered = drained(state, &thread.id, QueueIntent::Deliver);
    moving.then(queued).then(delivered)
}

/// A thread that moved itself goes once the run that asked has ended, ahead of anything queued behind it.
pub fn move_after_run_settled(state: &mut WorkspaceState, task_id: &str, status: RunStatus) -> Step {
    let Some(MoveAfterRun { destination, then }) = state.moves_after_run.get(task_id).cloned() else { return Step::none() };
    let then = if status == RunStatus::Cancelled { None } else { then };
    move_after_run_dropped(state, task_id);
    let Some(thread) = state.thread(task_id).filter(|thread| thread.archived_at.is_none()).cloned() else { return Step::none() };
    let project = project_for(state, Some(&thread)).cloned();
    let refusal = coordinator_ask_refusal(&thread, CoordinatorAsk::MoveWorktree(&destination), false);
    let target: Result<Option<Worktree>, Option<String>> = match &project {
        None => Err(Some(WORKTREE_PROJECT_ERROR.into())),
        Some(project) if project.workspace_id.is_none() => Err(Some(WORKTREE_PROJECT_ERROR.into())),
        Some(_) if refusal.is_some() => Err(refusal.map(String::from)),
        Some(_) if leaving_thread_ids(state).contains(task_id) => Err(Some(WORKTREE_RELEASING_ERROR.into())),
        Some(_) if destination == WorktreeDestination::New => Ok(None),
        Some(project) => arrival(state, &thread, &project.id, &destination),
    };
    match target {
        Err(None) => follow_up_queued(state, task_id, then.as_deref()),
        Err(Some(why)) => {
            let note = create_failure_message(format!("Did not move. {why}"));
            if let Some(thread) = state.thread_mut(task_id) {
                thread.messages.push(Shared::new(note));
                thread.updated_at = now();
            }
            Step::none()
        }
        Ok(_) if destination == WorktreeDestination::New => to_new_worktree(state, task_id, &project.expect("project").id, then),
        Ok(target) => {
            let moved = relocated(state, task_id, target.as_ref(), false);
            moved.then(follow_up_queued(state, task_id, then.as_deref()))
        }
    }
}

/// Hands back a checkout only when every linked thread explicitly leaves it.
pub fn release_worktrees(state: &WorkspaceState, leaving: &[&Thread]) -> Vec<EffectWork> {
    let going: HashSet<&str> = leaving.iter().map(|thread| thread.id.as_str()).collect();
    let mut released: HashSet<String> = HashSet::new();
    let mut effects = Vec::new();
    for thread in leaving {
        let Some(worktree) = worktree_for(state, Some(thread)) else { continue };
        if released.contains(&worktree.id) || worktree_claimants(state, &worktree.id).iter().any(|claimant| !going.contains(claimant.id.as_str())) {
            continue;
        }
        released.insert(worktree.id.clone());
        effects.push(EffectWork::ReleaseWorktree { task_id: thread.id.clone(), worktree_id: worktree.id.clone(), root: worktree.root.clone(), title: thread.title.clone() });
    }
    effects
}

/// Moves the thread there and then. Turning it off takes this thread's claim off the checkout; the
/// last claim to go takes the directory with it.
pub fn worktree_set(state: &mut WorkspaceState, task_id: Option<&str>, worktree: bool) -> Step {
    state.worktree_move = None;
    let Some(thread) = target_id(state, task_id).and_then(|id| state.thread(&id)).cloned() else {
        if task_id.is_none() {
            state.draft_worktree = worktree;
            state.draft_worktree_id = None;
        }
        return Step::none();
    };
    if state.creating_worktrees.contains(&thread.id) {
        return Step::refuse(WORKTREE_CREATING_ERROR);
    }
    if leaving_thread_ids(state).contains(&thread.id) {
        return Step::refuse(WORKTREE_RELEASING_ERROR);
    }
    if is_working(state, &thread.id) {
        return Step::refuse(WORKTREE_RUNNING_ERROR);
    }
    if worktree {
        if thread.worktree_id.is_some() {
            return Step::none();
        }
        let Some(project) = project_for(state, Some(&thread)).filter(|project| project.workspace_id.is_some()).cloned() else { return Step::refuse(WORKTREE_PROJECT_ERROR) };
        creating_worktree(state, &thread.id);
        return Step::effect(EffectWork::CreateWorktree { task_id: thread.id.clone(), project_root: project.root, r#move: None, name: None, project_id: None, then: None });
    }
    let Some(leaving) = worktree_for(state, Some(&thread)).cloned() else { return Step::none() };
    let releasing = release_worktrees(state, &[&thread]);
    clears_error(state);
    if releasing.is_empty() {
        let note = create_conversation_message(MessageKind::System, "Returned to the project checkout. The worktree is still there, and other threads are still working in it.", Some(leaving.root.clone()));
        if let Some(thread) = state.thread_mut(&thread.id) {
            thread.worktree_id = None;
            thread.worktree_entered_at = None;
            thread.messages.push(Shared::new(note));
            thread.updated_at = now();
        }
        return Step::none();
    }
    for effect in &releasing {
        if let EffectWork::ReleaseWorktree { task_id, .. } = effect {
            if !state.releasing_worktrees.contains(task_id) {
                state.releasing_worktrees.push(task_id.clone());
            }
        }
    }
    Step::effects(releasing.into_iter().map(Effect::from).collect())
}

/// The checkout itself is gone, so every thread that claimed it is local again and hears why.
fn worktree_dropped(state: &mut WorkspaceState, worktree_id: &str, note: impl Fn() -> ConversationMessage) {
    let gone = state.worktree(worktree_id).cloned();
    let claimants: HashSet<String> = worktree_claimants(state, worktree_id).iter().map(|thread| thread.id.clone()).collect();
    state.releasing_worktrees.retain(|task_id| !claimants.contains(task_id));
    state.worktrees.retain(|worktree| worktree.id != worktree_id);
    if let Some(gone) = gone {
        state.environments.shift_remove(&gone.workspace_id);
    }
    if state.draft_worktree_id.as_deref() == Some(worktree_id) {
        state.draft_worktree_id = None;
    }
    for thread in state.threads.iter_mut().filter(|thread| thread.worktree_id.as_deref() == Some(worktree_id)) {
        thread.worktree_id = None;
        thread.worktree_entered_at = None;
        if thread.continuation.is_some() {
            thread.inherited_continuation = true;
        }
        thread.messages.push(Shared::new(note()));
        thread.updated_at = now();
    }
}

fn snapshot_note(text: String, snapshot: &WorktreeSnapshotResult) -> impl Fn() -> ConversationMessage {
    let detail = snapshot.reference.as_ref().map(|reference| format!("Recover it with git show {reference}"));
    move || create_conversation_message(MessageKind::System, text.clone(), detail.clone())
}

fn settle_bulk(settings: &WorktreeSettingsState, root: &str, deleted: Option<Option<String>>) -> WorktreeSettingsState {
    let mut next = settings.clone();
    let Some(bulk) = &settings.bulk else { return next };
    if !bulk.pending.iter().any(|item| item == root) {
        return next;
    }
    let pending: Vec<String> = bulk.pending.iter().filter(|item| *item != root).cloned().collect();
    let mut recover = bulk.recover.clone();
    if let Some(Some(line)) = &deleted {
        recover.push(line.clone());
    }
    let settled = BulkDelete { deleted: bulk.deleted + i64::from(deleted.is_some()), recover, pending: pending.clone() };
    next.bulk = (!pending.is_empty()).then_some(settled);
    next
}

fn bulk_notice(bulk: &BulkDelete, recover: Option<&str>) -> String {
    let count = bulk.deleted + 1;
    let mut parts = vec![format!("Deleted {count} {}.", if count == 1 { "worktree" } else { "worktrees" })];
    parts.extend(bulk.recover.iter().cloned());
    if let Some(recover) = recover {
        parts.push(recover.into());
    }
    parts.join(" ")
}

fn worktree_deleted(state: &mut WorkspaceState, worktree_id: &str, root: &str, snapshot: &WorktreeSnapshotResult, missing_only: bool) -> Step {
    let worktree = state.worktrees.iter().find(|item| item.id == worktree_id || item.root == root).cloned();
    let short = snapshot.short_commit.clone().or_else(|| snapshot.commit.as_ref().map(|commit| commit.chars().take(7).collect()));
    let text = if missing_only {
        "Missing worktree folder forgotten. Back on the project checkout.".to_string()
    } else if snapshot.commit.is_some() {
        format!("Worktree deleted. Loose work was committed as {} first.", short.clone().unwrap_or_default())
    } else {
        "Worktree deleted. Back on the project checkout.".to_string()
    };
    let recovery = if missing_only {
        String::new()
    } else if let Some(reference) = &snapshot.reference {
        format!("git show {reference}")
    } else if let Some(commit) = &snapshot.commit {
        format!("git show {}", snapshot.short_commit.clone().unwrap_or_else(|| commit.clone()))
    } else {
        String::new()
    };
    let mut notice = if missing_only { format!("Forgot {root}. Thread history kept.") } else { format!("Deleted {root}.") };
    if !missing_only && snapshot.reference.is_some() {
        notice.push_str(&format!(" Recover it with {recovery}."));
    } else if !missing_only && snapshot.commit.is_some() {
        notice.push_str(&format!(" Recover loose work with {recovery}."));
    }
    let bulk = state.worktree_settings.bulk.clone();
    let in_bulk = bulk.as_ref().is_some_and(|bulk| bulk.pending.iter().any(|item| item == root));
    let recover = (!recovery.is_empty()).then(|| format!("Recover {} with {recovery}.", last_segment(root).unwrap_or("")));
    state.worktree_settings = settle_bulk(&state.worktree_settings, root, Some(recover.clone()));
    if let (Some(bulk), true) = (&bulk, in_bulk) {
        notice = bulk_notice(bulk, recover.as_deref());
    }
    if let Some(managed) = state.managed_worktrees.as_mut() {
        managed.retain(|item| item.root != root);
    }
    state.deleting_worktrees.retain(|item| item != root);
    if !in_bulk {
        state.worktree_management_error = None;
    }
    state.worktree_management_notice = Some(notice);
    if let Some(worktree) = worktree {
        let claimants: HashSet<String> = worktree_claimants(state, &worktree.id).iter().map(|thread| thread.id.clone()).collect();
        dismissed(&mut state.threads, &claimants);
        worktree_dropped(state, &worktree.id, snapshot_note(text, snapshot));
    }
    Step::effects(follow_ups(refresh_environment(state)))
}

fn worktree_created(state: &mut WorkspaceState, task_id: &str, worktree: &crate::domain::worktree::CreatedWorktree, moving: bool, project_id: Option<&str>, then: Option<&str>) -> Step {
    let was_creating = state.creating_worktrees.iter().any(|id| id == task_id);
    state.creating_worktrees.retain(|id| id != task_id);
    let Some(thread) = state.thread(task_id).cloned() else { return Step::none() };
    let Some(thread_project) = thread.project_id.clone() else { return Step::none() };
    if thread.worktree_id.is_some() && !moving {
        return Step::none();
    }
    let made = worktree.in_project(project_id.unwrap_or(&thread_project));
    if moving {
        if !was_creating {
            return Step::none();
        }
        state.worktrees.push(made.clone());
        state.managed_worktrees = None;
        state.worktree_management_loading = true;
        let listing = Step::effect(EffectWork::ListWorktrees);
        if thread_project != made.project_id {
            return listing;
        }
        let moved = relocated(state, task_id, Some(&made), true);
        let queued = follow_up_queued(state, task_id, then);
        let delivered = drained(state, task_id, QueueIntent::Deliver);
        return moved.then(queued).then(listing).then(delivered);
    }
    let note = create_conversation_message(MessageKind::System, format!("Moved into a worktree at {}", made.root), Some(format!("Detached at {}", made.base_commit.chars().take(7).collect::<String>())));
    let id = made.id.clone();
    state.worktrees.push(made);
    if let Some(thread) = state.thread_mut(task_id) {
        thread.worktree_id = Some(id);
        thread.messages.push(Shared::new(note));
        thread.updated_at = now();
    }
    Step::effects(diff_reread(state, task_id))
}

fn worktree_released(state: &mut WorkspaceState, task_id: &str, snapshot: &WorktreeSnapshotResult) -> Step {
    let Some(worktree) = worktree_for(state, state.thread(task_id)).cloned() else { return Step::none() };
    let text = match &snapshot.commit {
        Some(commit) => format!(
            "Returned to the project checkout. Uncommitted work was committed as {}, and the worktree was removed.",
            snapshot.short_commit.clone().unwrap_or_else(|| commit.chars().take(7).collect())
        ),
        None => "Returned to the project checkout. The worktree had nothing uncommitted, and was removed.".into(),
    };
    worktree_dropped(state, &worktree.id, snapshot_note(text, snapshot));
    Step::effects(diff_reread(state, task_id))
}

/// Asks before moving, but only where the answer could cost something.
fn move_asked(state: &mut WorkspaceState, worktree: Option<bool>) -> Step {
    let Some(worktree) = worktree else {
        state.worktree_move = None;
        return Step::none();
    };
    let Some(thread) = target_id(state, None).and_then(|id| state.thread(&id)).cloned() else { return worktree_set(state, None, worktree) };
    if worktree == thread.worktree_id.is_some() {
        return Step::none();
    }
    let workspace_id = thread_workspace_id(state, Some(&thread));
    let holding = match workspace_id.as_ref().and_then(|id| state.environments.get(id)) {
        Some(ChangedFilesResult::Available { files, .. }) => files.len(),
        _ => 0,
    };
    if !worktree && holding == 0 {
        return worktree_set(state, None, worktree);
    }
    state.worktree_move = Some(WorktreeMove { task_id: thread.id.clone(), worktree });
    Step::none()
}

fn checkout_branch(state: &mut WorkspaceState, task_id: Option<&str>, branch: &str, create: bool) -> Step {
    let Some(thread) = target_id(state, task_id).and_then(|id| state.thread(&id)).cloned() else {
        clears_error(state);
        state.draft_branch = Some(DraftBranch { name: branch.into(), create });
        return Step::none();
    };
    let Some(workspace_id) = thread_workspace_id(state, Some(&thread)) else { return Step::refuse(SWITCH_PROJECT_ERROR) };
    if state.creating_worktrees.contains(&thread.id) {
        return Step::refuse(WORKTREE_CREATING_ERROR);
    }
    if leaving_thread_ids(state).contains(&thread.id) {
        return Step::refuse(WORKTREE_RELEASING_ERROR);
    }
    if runs_in_workspace(state, Some(&workspace_id)) || is_working(state, &thread.id) {
        return Step::refuse(SWITCH_RUNNING_ERROR);
    }
    clears_error(state);
    Step::effect(EffectWork::CheckoutBranch { workspace_id, branch: branch.into(), create: create.then_some(true) })
}

/// Manual deletion snapshots loose work first and refuses to move the ground under a run.
fn delete_worktree(state: &mut WorkspaceState, task_id: Option<&str>, root: Option<&str>, worktree_id: Option<&str>, missing_only: bool) -> Step {
    let thread = target_id(state, task_id).and_then(|id| state.thread(&id)).cloned();
    let current = worktree_for(state, thread.as_ref()).cloned();
    let recorded = match (worktree_id, root) {
        (Some(id), _) => worktree_by_id(state, Some(id)).cloned(),
        (None, Some(root)) => state.worktrees.iter().find(|item| item.root == root).cloned(),
        (None, None) => current,
    };
    let managed = root.and_then(|root| state.managed_worktrees.as_ref().and_then(|managed| managed.iter().find(|item| item.root == root)).cloned());
    let (id, found_root) = match (recorded, managed) {
        (Some(recorded), _) => (recorded.id, recorded.root),
        (None, Some(managed)) => (managed.id, managed.root),
        (None, None) => return refusal_with_management(state, WORKTREE_MISSING_ERROR),
    };
    if state.deleting_worktrees.contains(&found_root) {
        return Step::none();
    }
    let claimants: Vec<String> = state.threads.iter().filter(|claimant| claimant.worktree_id.as_deref() == Some(id.as_str())).map(|claimant| claimant.id.clone()).collect();
    if claimants.iter().any(|claimant| state.releasing_worktrees.contains(claimant)) {
        return refusal_with_management(state, WORKTREE_RELEASING_ERROR);
    }
    if claimants.iter().any(|claimant| is_working(state, claimant)) {
        state.worktree_management_error = Some(WORKTREE_RUNNING_ERROR.into());
        return Step::refuse(WORKTREE_RUNNING_ERROR);
    }
    let title = last_segment(&found_root).unwrap_or(&id).to_string();
    state.worktree_settings.confirming = None;
    state.deleting_worktrees.push(found_root.clone());
    state.worktree_management_error = None;
    state.worktree_management_notice = None;
    clears_error(state);
    Step::effect(EffectWork::DeleteWorktree { worktree_id: id, root: found_root, title, missing_only: missing_only.then_some(true) })
}

/// Each checkout is judged again here, so one that changed since the confirmation stays.
fn delete_safe(state: &mut WorkspaceState, requested: &[String]) -> Step {
    let working = thread_activity(state).working;
    let safe: HashSet<String> = worktree_settings_views(state, &working).unwrap_or_default().into_iter().filter(|view| view.safe).map(|view| view.root).collect();
    state.worktree_settings.confirming_safe = false;
    let saved_error = state.action_error.clone();
    let mut effects = Vec::new();
    let mut pending = Vec::new();
    let mut seen = HashSet::new();
    for root in requested {
        if !seen.insert(root.clone()) || !safe.contains(root) {
            continue;
        }
        let deleted = delete_worktree(state, None, Some(root), None, false);
        if !state.deleting_worktrees.contains(root) {
            continue;
        }
        effects.extend(deleted.effects);
        pending.push(root.clone());
    }
    if pending.is_empty() {
        state.action_error = saved_error;
    }
    state.worktree_settings.bulk = match state.worktree_settings.bulk.take() {
        Some(mut running) => {
            running.pending.extend(pending);
            Some(running)
        }
        None if !pending.is_empty() => Some(BulkDelete { pending, deleted: 0, recover: Vec::new() }),
        None => None,
    };
    Step::effects(effects)
}

fn confirm_delete(state: &mut WorkspaceState, root: Option<&str>) -> Step {
    let Some(root) = root else {
        state.worktree_settings.confirming = None;
        return Step::none();
    };
    let working = thread_activity(state).working;
    if let Some(worktree) = worktree_settings_views(state, &working).and_then(|views| views.into_iter().find(|item| item.root == root)) {
        if worktree.busy || worktree.deleting {
            return Step::none();
        }
        state.worktree_settings.confirming = Some(root.into());
        return Step::none();
    }
    if state.active_computer().is_some() {
        state.worktree_settings.confirming = Some(root.into());
        return Step::none();
    }
    let Some(recorded) = state.worktrees.iter().find(|item| item.root == root).cloned() else { return Step::none() };
    if state.deleting_worktrees.contains(&recorded.root) || worktree_claimants(state, &recorded.id).iter().any(|thread| working.contains(&thread.id)) {
        return Step::none();
    }
    state.worktree_settings.confirming = Some(root.into());
    let listing = !state.worktree_management_loading;
    state.worktree_management_loading = true;
    state.worktree_management_error = None;
    if listing {
        Step::effect(EffectWork::ListWorktrees)
    } else {
        Step::none()
    }
}

pub fn reduce_worktrees(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    use AppCommand as C;
    use WorkspaceEvent as E;
    match input {
        WorkspaceInput::Command(command) => match command {
            C::TaskMoveWorktree { task_id, destination, after_run, sender, then } => worktree_moved(state, task_id.as_deref(), destination, *after_run, sender.is_some(), then.as_deref()),
            C::WorktreeOpenThread { task_id } => {
                if state.thread(task_id).is_none_or(|thread| thread.worktree_id.is_none()) {
                    return Step::none();
                }
                let shown = crate::reducer::settings::settings_shown(state, false, None, None);
                shown.and_then(state, |state| crate::reducer::threads::selected(state, task_id))
            }
            C::MoveWorktree { worktree } => move_asked(state, *worktree),
            C::TaskSetWorktree { task_id, worktree } => worktree_set(state, task_id.as_deref(), *worktree),
            C::TaskSetBranch { branch, create } => {
                clears_error(state);
                state.draft_branch = branch.as_ref().map(|name| DraftBranch { name: name.clone(), create: *create == Some(true) });
                Step::none()
            }
            C::TaskCheckoutBranch { task_id, branch, create } => checkout_branch(state, task_id.as_deref(), branch, *create == Some(true)),
            C::WorktreeFilterProject { project } => {
                let settings = &mut state.worktree_settings;
                settings.project = project.clone();
                settings.confirming = None;
                settings.confirming_safe = false;
                Step::none()
            }
            C::WorktreeConfirmDeleteSafe { open } => {
                state.worktree_settings.confirming = None;
                state.worktree_settings.confirming_safe = *open;
                Step::none()
            }
            C::WorktreeConfirmDelete { root } => confirm_delete(state, root.as_deref()),
            C::WorktreeSetMissingOpen { open } => {
                state.worktree_settings.missing_open = Some(*open);
                Step::none()
            }
            C::WorktreeSetThreadsOpen { root, open } => {
                let expanded = &mut state.worktree_settings.expanded_threads;
                expanded.retain(|item| item != root);
                if *open {
                    expanded.push(root.clone());
                }
                Step::none()
            }
            C::WorktreeRefresh => {
                if state.worktree_management_loading {
                    return Step::none();
                }
                state.worktree_management_loading = true;
                state.worktree_settings.confirming = None;
                state.worktree_settings.confirming_safe = false;
                state.worktree_management_error = None;
                state.worktree_management_notice = None;
                Step::effect(EffectWork::ListWorktrees)
            }
            C::WorktreeReveal { root } => {
                let found = state.managed_worktrees.as_ref().and_then(|managed| managed.iter().find(|item| &item.root == root)).map(|item| item.root.clone()).or_else(|| state.worktrees.iter().find(|item| &item.root == root).map(|item| item.root.clone()));
                let Some(root) = found else { return refusal_with_management(state, WORKTREE_MISSING_ERROR) };
                state.worktree_management_error = None;
                Step::effect(EffectWork::RevealWorktree { root })
            }
            C::WorktreeDelete { task_id, root, worktree_id, missing_only } => delete_worktree(state, task_id.as_deref(), root.as_deref(), worktree_id.as_deref(), *missing_only == Some(true)),
            C::WorktreeDeleteSafe { roots } => delete_safe(state, roots),
            C::WorktreeMenuOpen { list } | C::WorktreeMenuSearch { list, .. } => {
                let query = match command {
                    C::WorktreeMenuSearch { query, .. } => query.clone(),
                    _ => String::new(),
                };
                state.worktree_menu_search.set(*list, query);
                if matches!(command, C::WorktreeMenuSearch { .. }) || *list == WorktreeMenuList::Threads || state.worktree_management_loading || state.active_computer().is_some() {
                    return Step::none();
                }
                state.worktree_management_loading = true;
                state.worktree_management_error = None;
                Step::effect(EffectWork::ListWorktrees)
            }
            _ => Step::none(),
        },
        WorkspaceInput::Event(event) => match event {
            E::CheckoutFinished { message } => {
                let reads = follow_ups(refresh_environment(state));
                match message {
                    Some(message) => Step { refusal: Some(message.clone()), ..Step::effects(reads) },
                    None => Step::effects(reads),
                }
            }
            E::WorktreeCreated { task_id, worktree, r#move, project_id, then } => worktree_created(state, task_id, worktree, *r#move == Some(true), project_id.as_deref(), then.as_deref()),
            E::WorktreeReleased { task_id, snapshot } => worktree_released(state, task_id, snapshot),
            E::WorktreeDeleted { worktree_id, root, snapshot, missing_only } => worktree_deleted(state, worktree_id, root, snapshot, *missing_only == Some(true)),
            E::WorktreeFailed { task_id, message } => {
                state.creating_worktrees.retain(|id| id != task_id);
                let delivered = drained(state, task_id, QueueIntent::Deliver);
                Step { refusal: Some(message.clone()), ..delivered }
            }
            E::WorktreeReleaseFailed { task_id, message } => {
                state.releasing_worktrees.retain(|id| id != task_id);
                Step::refuse(message.clone())
            }
            E::WorktreesLoaded { worktrees } => {
                state.managed_worktrees = Some(worktrees.clone());
                state.worktree_management_loading = false;
                state.worktree_management_error = None;
                Step::none()
            }
            E::WorktreesFailed { message, root } => {
                if let Some(root) = root {
                    state.deleting_worktrees.retain(|item| item != root);
                    state.worktree_settings = settle_bulk(&state.worktree_settings, root, None);
                } else {
                    state.worktree_management_loading = false;
                }
                state.worktree_management_error = Some(message.clone());
                Step::result(CommandResult::failed(message.clone()))
            }
            _ => Step::none(),
        },
    }
}
