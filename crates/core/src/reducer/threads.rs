//! Threads themselves: making one, choosing it, and what the list can do to it; and where the window is.

use std::collections::HashSet;

use crate::commands::AppCommand;
use crate::shared::Shared;
use crate::domain::coordination::{as_coordinator, coordinator_of, effort_role_of};
use crate::domain::engine::{capabilities_for, default_model_in, engine_has_model, model_has_effort, thread_model, AgentEngine};
use crate::domain::run::{AgentEffort, ExecutionPolicy};
use crate::domain::thread::{clamp_title, dismissed, fork_title, has_unread_attention, is_snooze_hours, with_all_read, wants_attention, Thread};
use crate::domain::thread_role::ThreadRole;
use crate::effects::{Effect, EffectWork, RunCommand};
use crate::events::WorkspaceEvent;
use crate::reducer::dock::{docks_disposed, focus_dock_tab, pruned, refresh_thread_environment, show_dock_tab, side_chats_closed};
use crate::reducer::errors::{coordinator_ask_refusal, CoordinatorAsk};
use crate::reducer::run_queue::{cancelling, file_thread};
use crate::reducer::store::leads_discarded;
use crate::reducer::{follow_ups, take_keys, target_id, Step};
use crate::state::{dock_for, dock_mut, front_dock, ForkName, SideChat, WorkspaceState, DRAFT_DOCK};
use crate::util::{now, random_id};
use crate::validation::{supports_computer_command, REMOTE_UNSUPPORTED};
use crate::view::dialogs::archive_prompt_view;
use crate::workspace::activity::{read_row, thread_activity, thread_lists};
use crate::workspace::coordination::{coordinator_roster, dismissed_with, own_coordination_sections, project_activity};
use crate::workspace::location::{project_for, worktree_by_id};
use crate::workspace::order::{move_thread, next_sort_index, order_threads};

/// Puts the user on a thread: what it holds becomes read, and the app follows it to its folder.
pub fn landed(state: &mut WorkspaceState, task_id: &str) {
    let thread = state.thread(task_id).cloned();
    let root = project_for(state, thread.as_ref()).map(|project| project.root.clone());
    state.current_id = Some(task_id.into());
    state.draft_project_id = thread.as_ref().and_then(|thread| thread.project_id.clone());
    if let Some(root) = root {
        state.last_folder = Some(root);
    }
    read_row(&mut state.threads, Some(task_id));
    state.action_error = None;
}

/// Back or forward to the thread at `index` in history, landing on it as any visit does.
pub fn visited(state: &mut WorkspaceState, index: usize) -> Step {
    state.history_index = index as i64;
    let task_id = state.history[index].clone();
    landed(state, &task_id);
    Step::none()
}

/// A side chat, or a thread under a coordinator, opens as a tab in the dock of the thread it belongs to.
pub fn selected(state: &mut WorkspaceState, chosen: &str) -> Step {
    let chat = state.side_chats.iter().find(|item| item.id == chosen).cloned();
    let lead = if chat.is_some() { None } else { coordinator_of(&state.threads, state.thread(chosen)).map(|lead| lead.id.clone()) };
    let task_id = chat.as_ref().map(|chat| chat.source_thread_id.clone()).or(lead.clone()).unwrap_or_else(|| chosen.to_string());
    landed(state, &task_id);
    if task_id == chosen {
        return Step::none();
    }
    if lead.is_some() && !dock_for(state, &task_id).thread_tabs.iter().any(|tab| tab == chosen) {
        dock_mut(state, &task_id).thread_tabs.push(chosen.into());
    }
    show_dock_tab(state, &task_id, chosen);
    let mut effects = focus_dock_tab(state, &task_id, chosen);
    crate::domain::thread::read_attention(&mut state.threads, Some(chosen));
    if lead.is_some() {
        effects.extend(follow_ups(refresh_thread_environment(state, chosen)));
    }
    Step::effects(effects)
}

/// The window on a fresh draft, in the checkout or project named, with the caret in the composer.
pub fn new_thread(state: &mut WorkspaceState, project_named: Option<&str>, worktree_id: Option<&str>) -> Step {
    let worktree = worktree_by_id(state, worktree_id).cloned();
    let project_id = worktree.as_ref().map(|worktree| worktree.project_id.clone()).or_else(|| project_named.map(String::from));
    let root = project_id.as_deref().and_then(|id| state.project(id)).map(|project| project.root.clone());
    state.diffs.shift_remove(DRAFT_DOCK);
    state.current_id = None;
    state.draft_project_id = project_id;
    state.draft_branch = None;
    state.draft_worktree = false;
    state.draft_worktree_id = worktree.map(|worktree| worktree.id);
    state.draft_role = None;
    if let Some(root) = root {
        state.last_folder = Some(root);
    }
    state.composer_focus += 1;
    state.action_error = None;
    state.open_menu = None;
    Step::effects(take_keys())
}

/// Priority as the sidebar draws it, narrowed to one project when `project_id` names one.
fn priority_threads(state: &WorkspaceState, project_id: Option<&str>) -> Vec<String> {
    let lists = thread_lists(state);
    let activity = thread_activity(state);
    let shown = crate::view::computers::shown_computers(state);
    let (sections, threads) = own_coordination_sections(state, &lists.visible_threads, &activity, &shown);
    let priority = match project_id {
        Some(project) => project_activity(&sections, &threads, project).priority,
        None => sections.priority,
    };
    priority.iter().map(|thread| thread.id.clone()).collect()
}

/// The rows of Priority Dismiss all files away.
fn dismissable_priority(state: &WorkspaceState, project_id: Option<&str>) -> Vec<String> {
    let here = thread_activity(state);
    let shown = crate::view::computers::shown_computers(state);
    priority_threads(state, project_id).into_iter().filter(|id| state.thread(id).is_some_and(|row| coordinator_roster(state, row, &here, Some(&shown)).dismissable)).collect()
}

/// Asks each paired computer holding threads under the filed coordinators to file away its own.
fn dismiss_elsewhere(state: &WorkspaceState, rows: &HashSet<String>) -> (Vec<Effect>, Vec<String>) {
    let here = thread_activity(state);
    let shown = crate::view::computers::shown_computers(state);
    let mut inputs: indexmap::IndexMap<String, (crate::state::PairedComputer, Vec<AppCommand>)> = indexmap::IndexMap::new();
    for row in rows {
        let Some(lead) = state.thread(row) else { continue };
        for member in coordinator_roster(state, lead, &here, Some(&shown)).filed {
            let Some(computer) = member.member.computer else { continue };
            if !wants_attention(member.member.thread) {
                continue;
            }
            inputs.entry(computer.link.id.clone()).or_insert_with(|| (computer.clone(), Vec::new())).1.push(AppCommand::TaskDismiss { task_id: Some(member.member.thread.id.clone()) });
        }
    }
    let mut effects = Vec::new();
    let mut errors = Vec::new();
    for (computer, commands) in inputs.into_values() {
        if computer.link.status != crate::domain::platform::ComputerStatus::Connected {
            errors.push(crate::domain::platform::offline_message(&computer.link.name, computer.link.status, computer.link.error.as_deref()));
        } else if !commands.iter().all(|command| supports_computer_command(computer.link.capabilities.as_deref(), command)) {
            errors.push(format!("{}: {REMOTE_UNSUPPORTED}", computer.link.name));
        } else {
            effects.push(EffectWork::ComputerForward { id: computer.link.id.clone(), inputs: commands.into_iter().map(Into::into).collect(), project_add_request: None, draft: None, proposal: None, silent: false }.into());
        }
    }
    (effects, errors)
}

/// The row Priority moves on to once this one leaves it.
fn priority_neighbour(state: &WorkspaceState, task_id: &str) -> Option<String> {
    let narrowed = state.activity_project.as_ref().filter(|id| state.projects.iter().any(|project| &&project.id == id && !project.unlisted)).cloned();
    let priority = priority_threads(state, narrowed.as_deref());
    let index = priority.iter().position(|id| id == task_id)?;
    priority.get(index + 1).or(index.checked_sub(1).and_then(|before| priority.get(before))).cloned()
}

fn dismiss_priority(state: &mut WorkspaceState, local_only: bool, project_id: Option<&str>) -> Step {
    let project_id = project_id.filter(|id| !id.is_empty());
    let own = local_only || state.computers.filter == "all" || state.computers.filter == "this";
    let rows: HashSet<String> = if own { dismissable_priority(state, project_id).into_iter().collect() } else { HashSet::new() };
    let blocked = thread_activity(state).blocked;
    let dotted = dismissed_with(&state.threads, &rows, &blocked);
    if !dotted.is_empty() {
        dismissed(&mut state.threads, &dotted);
    }
    if local_only {
        return Step::none();
    }
    let (mut effects, mut errors) = dismiss_elsewhere(state, &rows);
    for computer in crate::view::computers::shown_computers(state) {
        let Some(holder) = computer.state.as_deref() else { continue };
        if computer.link.status != crate::domain::platform::ComputerStatus::Connected || dismissable_priority(holder, project_id).is_empty() {
            continue;
        }
        let scoped = AppCommand::TaskDismissAll { local_only: Some(true), project_id: project_id.map(String::from) };
        let command = if project_id.is_some() || supports_computer_command(computer.link.capabilities.as_deref(), &scoped) { scoped } else { AppCommand::TaskDismissAll { local_only: None, project_id: None } };
        if !supports_computer_command(computer.link.capabilities.as_deref(), &command) {
            errors.push(format!("{}: {REMOTE_UNSUPPORTED}", computer.link.name));
            continue;
        }
        effects.push(EffectWork::ComputerForward { id: computer.link.id.clone(), inputs: vec![command.into()], project_add_request: None, draft: None, proposal: None, silent: false }.into());
    }
    let mut step = Step::effects(effects);
    if !errors.is_empty() {
        step.refusal = Some(errors.join("\n"));
    }
    step
}

fn read_all(state: &mut WorkspaceState, local_only: bool) -> Step {
    let own = local_only || state.computers.filter == "all" || state.computers.filter == "this";
    if own {
        with_all_read(&mut state.threads);
    }
    if local_only {
        return Step::none();
    }
    let command = AppCommand::TaskReadAll { local_only: Some(true) };
    let mut effects = Vec::new();
    let mut errors = Vec::new();
    for computer in crate::view::computers::shown_computers(state) {
        let unread = computer.state.as_deref().is_some_and(|holder| holder.threads.iter().any(|thread| has_unread_attention(thread)));
        if computer.link.status != crate::domain::platform::ComputerStatus::Connected || !unread {
            continue;
        }
        if supports_computer_command(computer.link.capabilities.as_deref(), &command) {
            effects.push(EffectWork::ComputerForward { id: computer.link.id.clone(), inputs: vec![command.clone().into()], project_add_request: None, draft: None, proposal: None, silent: false }.into());
        } else {
            errors.push(format!("{}: {REMOTE_UNSUPPORTED}", computer.link.name));
        }
    }
    let mut step = Step::effects(effects);
    if !errors.is_empty() {
        step.refusal = Some(errors.join("\n"));
    }
    step
}

/// Archiving a running thread cancels its run; its pages give up their views but keep their records.
fn archive_thread(state: &mut WorkspaceState, task_id: &str) -> Step {
    let active = state.active_runs.contains_key(task_id);
    let dock = dock_for(state, task_id).clone();
    if let Some(thread) = state.thread_mut(task_id) {
        thread.archived_at = Some(now());
    }
    if state.current_id.as_deref() == Some(task_id) {
        state.current_id = None;
    }
    let shown = if front_dock(state).0 == task_id { Some(dock.tab.clone()) } else { None };
    let browsing = state.side_chats.iter().any(|chat| chat.source_thread_id == task_id && state.active_runs.contains_key(&chat.id));
    let closed: Vec<String> = if browsing { Vec::new() } else { dock.browser_tabs.iter().filter(|tab| Some(&tab.id) != shown.as_ref()).map(|tab| tab.id.clone()).collect() };
    let mut step = crate::reducer::runs::pause_cancelled(state, task_id);
    if !closed.is_empty() {
        for tab in dock_mut(state, task_id).browser_tabs.iter_mut() {
            if closed.contains(&tab.id) && tab.loading {
                tab.loading = false;
            }
        }
    }
    step.effects.extend(crate::reducer::dock::retire_automations(state, &[task_id.to_string()]));
    if active {
        step = step.then(cancelling(state, task_id));
    }
    let thread = state.thread(task_id).cloned();
    step.effects.extend(file_thread(state, thread.as_ref(), true));
    step.effects.extend(closed.into_iter().map(|tab_id| Effect::from(EffectWork::BrowserClose { tab_id })));
    step
}

/// Files one row away, with the threads under it, here and on the paired computers holding them.
fn dismiss_thread(state: &mut WorkspaceState, task_id: &str) -> Step {
    let blocked = thread_activity(state).blocked;
    let filing = dismissed_with(&state.threads, &[task_id.to_string()].into(), &blocked);
    let has_attention = state.threads.iter().any(|thread| filing.contains(&thread.id) && (thread.outcome.is_some() || thread.findings.is_some()));
    let running = state.active_runs.get(task_id).is_some_and(|run| !run.dismissed);
    let (effects, errors) = dismiss_elsewhere(state, &[task_id.to_string()].into());
    if !has_attention && !running && effects.is_empty() && errors.is_empty() {
        return Step::none();
    }
    let successor = if state.current_id.as_deref() == Some(task_id) { priority_neighbour(state, task_id) } else { None };
    dismissed(&mut state.threads, &filing);
    if let Some(run) = state.active_runs.get_mut(task_id) {
        run.dismissed = true;
    }
    if let Some(successor) = successor {
        landed(state, &successor);
    }
    let mut step = Step::effects(effects);
    if !errors.is_empty() {
        step.refusal = Some(errors.join("\n"));
    }
    step
}

/// What a setting command changes: the thread it names, or with none named the draft and the thread on screen.
struct SettingsTarget {
    named: bool,
    task_id: Option<String>,
    thread: Option<Thread>,
    engine: AgentEngine,
}

/// Whether `task.set-policy` takes this policy for the thread or draft it names.
/// Whether `task.set-policy` changes anything: a policy the engine takes, written to the thread it names or is on, or to a draft that held another.
pub fn policy_allowed(state: &WorkspaceState, task_id: Option<&str>, policy: ExecutionPolicy) -> bool {
    let target = settings_target(state, task_id);
    capabilities_for(target.engine).policies.contains(&policy) && (target.task_id.is_some() || (!target.named && state.draft_policy != policy))
}

fn settings_target(state: &WorkspaceState, task_id: Option<&str>) -> SettingsTarget {
    let target = target_id(state, task_id);
    let thread = target.as_deref().and_then(|id| state.thread(id)).cloned();
    SettingsTarget { named: task_id.is_some(), engine: thread.as_ref().map(|thread| thread.engine).unwrap_or(state.draft_engine), task_id: target, thread }
}

fn thread_takes_effort(state: &WorkspaceState, thread: &Thread, effort: AgentEffort) -> bool {
    let catalog = state.catalog();
    thread_model(&catalog, thread.engine, thread.model.as_deref()).is_some_and(|model| model_has_effort(&catalog, thread.engine, &model, effort, effort_role_of(Some(thread), None)))
}

fn write_thread(state: &mut WorkspaceState, task_id: Option<&str>, update: impl FnOnce(&mut Thread)) {
    if let Some(thread) = task_id.and_then(|id| state.thread_mut(id)) {
        update(thread);
        thread.updated_at = now();
    }
}

fn label_thread(task_id: &str, title: &str) -> Effect {
    EffectWork::SendRunCommand { command: RunCommand::Label { task_id: task_id.into(), title: title.into() } }.into()
}

/// A copy of `source` placed straight under it in the list.
pub fn forked_in(state: &mut WorkspaceState, source: &Thread, id: &str, at: i64, title: Option<&str>) {
    let taken: Vec<&str> = state.threads.iter().map(|thread| thread.title.as_str()).collect();
    let title = title.map(clamp_title).filter(|title| !title.is_empty()).unwrap_or_else(|| fork_title(&source.title, taken));
    let mut fork = Thread::new(id, title, source.engine, source.execution_policy, at);
    fork.title_by_user = true;
    fork.project_id = source.project_id.clone();
    fork.model = source.model.clone();
    fork.effort = source.effort;
    if capabilities_for(source.engine).fast_mode {
        fork.fast_mode = Some(source.fast_mode.unwrap_or(false));
    }
    fork.messages = source.messages.clone();
    if source.continuation.is_some() {
        fork.continuation = source.continuation.clone();
        fork.inherited_continuation = true;
    }
    fork.continuation_status = source.continuation_status;
    fork.last_change_snapshot.captured_at = at;
    fork.sort_index = Some(next_sort_index(&state.threads));
    let members = crate::domain::coordination::coordinated_thread_ids(&state.threads);
    let anchor = if members.contains(&source.id) { crate::domain::coordination::lead_id_of(source).map(String::from) } else { Some(source.id.clone()) };
    let group: Vec<Shared<Thread>> = order_threads(&state.threads.iter().filter(|thread| thread.archived_at.is_none() && !members.contains(&thread.id) && thread.project_id == source.project_id).cloned().collect::<Vec<_>>());
    let under = group.iter().position(|thread| Some(&thread.id) == anchor.as_ref()).map(|index| index + 1).unwrap_or(0);
    state.threads.push(fork.into());
    move_thread(&mut state.threads, id, &crate::domain::project::ThreadDropTarget { project_id: source.project_id.clone(), index: under as i64 });
}

pub fn reduce_thread_command(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::TaskSnooze { task_id, hours } => {
            if !is_snooze_hours(*hours) || !priority_threads(state, None).contains(task_id) {
                return Step::none();
            }
            let until = now() + *hours as i64 * 60 * 60 * 1000;
            if let Some(thread) = state.thread_mut(task_id) {
                thread.snoozed_until = Some(until);
            }
            state.open_menu = None;
            Step::none()
        }
        AppCommand::TaskUnsnooze { task_id } => {
            let Some(thread) = state.thread_mut(task_id).filter(|thread| thread.snoozed_until.is_some()) else { return Step::none() };
            thread.snoozed_until = None;
            state.open_menu = None;
            Step::none()
        }
        AppCommand::TaskNew { project_id, worktree_id } => new_thread(state, project_id.as_deref(), worktree_id.as_deref()),
        AppCommand::TaskSelect { task_id } => selected(state, task_id),
        AppCommand::TaskDismiss { task_id } => match task_id {
            Some(task_id) => dismiss_thread(state, task_id),
            None => Step::none(),
        },
        AppCommand::TaskDismissAll { local_only, project_id } => dismiss_priority(state, local_only.unwrap_or(false), project_id.as_deref()),
        AppCommand::TaskReadAll { local_only } => read_all(state, local_only.unwrap_or(false)),
        AppCommand::TaskArchive { task_id } => archive_thread(state, task_id),
        AppCommand::TaskRestore { task_id } => {
            let Some(thread) = state.thread(task_id).filter(|thread| thread.archived_at.is_some()).cloned() else { return Step::none() };
            if let Some(thread) = state.thread_mut(task_id) {
                thread.archived_at = None;
            }
            Step::effects(file_thread(state, Some(&thread), false))
        }
        AppCommand::TaskClearArchive => {
            let archived: Vec<Shared<Thread>> = state.threads.iter().filter(|thread| thread.archived_at.is_some()).cloned().collect();
            if archived.is_empty() {
                return Step::none();
            }
            let discarded: HashSet<String> = archived.iter().map(|thread| thread.id.clone()).collect();
            let forks: Vec<SideChat> = state.side_chats.iter().filter(|chat| discarded.contains(&chat.source_thread_id)).cloned().collect();
            let mut step = side_chats_closed(state, &forks);
            step.effects.extend(docks_disposed(state, archived.iter().map(|thread| &thread.id)));
            state.threads.retain(|thread| !discarded.contains(&thread.id));
            if state.current_id.as_ref().is_some_and(|id| !state.threads.iter().any(|thread| &thread.id == id)) {
                state.current_id = None;
            }
            pruned(state, &discarded);
            step.then(leads_discarded(state, &archived))
        }
        AppCommand::TaskRename { task_id, title } => {
            let title = clamp_title(title);
            if title.is_empty() || state.thread(task_id).is_none() {
                return Step::none();
            }
            write_thread(state, Some(task_id), |thread| {
                thread.title = title.clone();
                thread.title_by_user = true;
            });
            Step::effects(vec![label_thread(task_id, &title)])
        }
        AppCommand::TaskSetRole { task_id, role, sender } => {
            let target = target_id(state, task_id.as_deref());
            let Some(thread) = target.as_deref().and_then(|id| state.thread(id)).cloned() else {
                if task_id.is_none() {
                    state.draft_role = *role;
                }
                return Step::none();
            };
            if thread.role == *role {
                return Step::none();
            }
            if sender.is_some() {
                if let Some(refusal) = coordinator_ask_refusal(&thread, CoordinatorAsk::SetRole(*role), state.creating_worktrees.contains(&thread.id)) {
                    return Step::refuse(refusal);
                }
            }
            let catalog = state.catalog();
            if let Some(item) = state.thread_mut(&thread.id) {
                item.role = None;
                item.thread_model = None;
                item.proposals = None;
                item.muted = false;
                if *role == Some(ThreadRole::Coordinator) {
                    as_coordinator(&catalog, item);
                } else {
                    item.role = *role;
                }
                item.updated_at = now();
            }
            Step::none()
        }
        AppCommand::TaskFork { task_id, worktree, title } => {
            let Some(source) = target_id(state, task_id.as_deref()).and_then(|id| state.thread(&id).cloned()) else { return Step::none() };
            if state.side_chat_ids().contains(&source.id) {
                return Step::none();
            }
            let fork_id = random_id();
            forked_in(state, &source, &fork_id, now(), title.as_deref());
            state.fork_name = None;
            state.open_menu = None;
            let opened = selected(state, &fork_id);
            if worktree != &Some(true) {
                return opened;
            }
            let mut moved = crate::reducer::misc::worktree_set(state, &fork_id, true);
            if let Some(refusal) = moved.refusal.take() {
                state.action_error = Some(refusal);
            }
            moved.result = None;
            opened.then(moved)
        }
        AppCommand::ForkName { source_id, worktree } => {
            let Some(source_id) = source_id else {
                state.fork_name = None;
                return Step::none();
            };
            let holder = crate::view::computers::computer_of_thread(state, Some(source_id)).and_then(|computer| computer.state.as_deref()).unwrap_or(state);
            let Some(source) = holder.thread(source_id) else { return Step::none() };
            if holder.side_chat_ids().contains(&source.id) {
                return Step::none();
            }
            let title = fork_title(&source.title, holder.threads.iter().map(|thread| thread.title.as_str()));
            let source_id = source.id.clone();
            state.fork_name = Some(ForkName { task_id: source_id, worktree: *worktree == Some(true), title });
            state.open_menu = None;
            Step::none()
        }
        AppCommand::ArchivePrompt { thread_id } => {
            match thread_id {
                None => state.archive_prompt = None,
                Some(thread_id) => {
                    if archive_prompt_view(state, Some(thread_id)).is_some() {
                        state.archive_prompt = Some(thread_id.clone());
                        state.open_menu = None;
                    }
                }
            }
            Step::none()
        }
        AppCommand::TaskMove { task_id, target } => {
            if move_thread(&mut state.threads, task_id, target) {
                state.open_menu = None;
            }
            Step::none()
        }
        AppCommand::TaskSetPolicy { task_id, policy } => {
            let target = settings_target(state, task_id.as_deref());
            if !capabilities_for(target.engine).policies.contains(policy) {
                return Step::none();
            }
            let policy: ExecutionPolicy = *policy;
            if !target.named {
                state.with_draft(|draft| draft.policy = policy);
            }
            write_thread(state, target.task_id.as_deref(), |thread| thread.execution_policy = policy);
            Step::none()
        }
        AppCommand::TaskSetModel { task_id, engine, model } => {
            let target = settings_target(state, task_id.as_deref());
            let catalog = state.catalog();
            let Some(model) = model.clone().or_else(|| default_model_in(&catalog, *engine)) else { return Step::none() };
            if !engine_has_model(&catalog, *engine, &model) || target.thread.as_ref().is_some_and(|thread| !engine_has_model(&catalog, thread.engine, &model)) {
                return Step::none();
            }
            if !target.named {
                let engine = *engine;
                let chosen = model.clone();
                state.with_draft(|draft| {
                    draft.engine = engine;
                    draft.model = Some(chosen);
                });
            }
            write_thread(state, target.task_id.as_deref(), |thread| thread.model = Some(model));
            Step::none()
        }
        AppCommand::TaskSetFastMode { task_id, fast_mode } => {
            let target = settings_target(state, task_id.as_deref());
            if (target.task_id.is_some() && target.thread.is_none()) || !capabilities_for(target.engine).fast_mode {
                return Step::none();
            }
            let fast = *fast_mode;
            if !target.named {
                state.with_draft(|draft| draft.fast_mode = fast);
            }
            write_thread(state, target.task_id.as_deref(), |thread| thread.fast_mode = Some(fast));
            Step::none()
        }
        AppCommand::TaskSetEffort { task_id, effort, .. } => {
            let target = settings_target(state, task_id.as_deref());
            if target.thread.as_ref().is_some_and(|thread| !thread_takes_effort(state, thread, *effort)) {
                return Step::none();
            }
            let catalog = state.catalog();
            let draft_takes = model_has_effort(&catalog, state.draft_engine, &state.draft_model, *effort, effort_role_of(None, state.draft_role));
            if target.thread.is_none() && !draft_takes {
                return Step::none();
            }
            let effort = *effort;
            if !target.named && draft_takes {
                state.with_draft(|draft| draft.effort = effort);
            }
            write_thread(state, target.task_id.as_deref(), |thread| thread.effort = Some(effort));
            Step::none()
        }
        _ => Step::none(),
    }
}

pub fn reduce_thread_event(state: &mut WorkspaceState, event: &WorkspaceEvent) -> Step {
    match event {
        WorkspaceEvent::TaskResultCollected { task_id } => {
            if let Some(thread) = state.thread_mut(task_id) {
                thread.outcome_unread = false;
            }
            Step::none()
        }
        WorkspaceEvent::TitleSuggested { task_id, title } => {
            let title = clamp_title(title);
            let Some(thread) = state.thread_mut(task_id) else { return Step::none() };
            if thread.title_by_user || title.is_empty() || title == thread.title {
                return Step::none();
            }
            thread.title = title.clone();
            Step::effects(vec![label_thread(task_id, &title)])
        }
        _ => Step::none(),
    }
}
