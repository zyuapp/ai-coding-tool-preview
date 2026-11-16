//! The dock itself: its tabs, its panels, how much of the window it takes, the review drawn in it,
//! the shells it holds, and the side chats it shows.

use std::collections::HashSet;

use crate::commands::AppCommand;
use crate::domain::coordination::{is_coordinator, OverviewFold};
use crate::domain::diff::{default_branch_range, file_fingerprint, is_commit_hash, range_key, DiffMode, DiffRange};
use crate::domain::terminal::{TerminalSession, TerminalStatus};
use crate::domain::thread::read_attention;
use crate::effects::{Effect, EffectWork};
use crate::events::{ChangedFilesResult, CommandResult, DiffSummaryResult, WorkspaceEvent};
use crate::reducer::errors::TERMINAL_FOLDER_ERROR;
use crate::reducer::{follow_ups, take_keys, Step};
use crate::state::{
    diff_for, diff_matches, diff_mut, dock_for, dock_mut, dock_owner, dock_subject, dock_tab_after_closing, dock_tab_ids, dock_tab_kind, folded_on_load, front_dock, mode_for_range, owner_of_browser_tab, owner_of_terminal, retained_views,
    workflow_by_id, DiffStateMode, DockFocus, DockTabKind, SideChat, WorkspaceState, DIFF_PANEL, DOCK_PICKER, DRAFT_DOCK, OVERVIEW_PANEL, WORKFLOW_PANEL,
};
use crate::util::{now, random_id, Map};
use crate::workspace::location::{current_folder, thread_workspace_id};

/// Hands a dock tab the keyboard. The view watches the count rather than being told to focus.
pub fn focused_tab(state: &mut WorkspaceState, owner: &str, tab: &str) {
    let count = state.dock_focus.as_ref().map(|focus| focus.count).unwrap_or(0) + 1;
    state.dock_focus = Some(DockFocus { owner: owner.into(), tab: tab.into(), count });
}

/// `focused_tab`, and the keys back to the window unless the tab is a page that already holds them.
pub fn focus_dock_tab(state: &mut WorkspaceState, owner: &str, tab: &str) -> Vec<Effect> {
    focused_tab(state, owner, tab);
    let page = dock_for(state, owner).browser_tabs.iter().find(|item| item.id == tab);
    if page.is_some_and(|page| !page.url.is_empty()) {
        Vec::new()
    } else {
        take_keys()
    }
}

pub fn show_dock_tab(state: &mut WorkspaceState, owner: &str, tab: &str) {
    let dock = dock_mut(state, owner);
    dock.open = true;
    dock.tab = tab.into();
}

/// Bringing a page to the front is what gives a restored one its view, and only then.
pub fn browser_effects_for_tab(state: &WorkspaceState, owner: &str, dock_tab: &str) -> Vec<Effect> {
    let Some(tab) = dock_for(state, owner).browser_tabs.iter().find(|page| page.id == dock_tab) else { return Vec::new() };
    vec![
        EffectWork::BrowserOpen { task_id: None, tab_id: tab.id.clone(), url: (!tab.url.is_empty()).then(|| tab.url.clone()) }.into(),
        EffectWork::BrowserShow { tab_id: Some(tab.id.clone()) }.into(),
    ]
}

/// Which page the panel draws once the dock on screen changes.
pub fn shown_page_effects(state: &WorkspaceState) -> Vec<Effect> {
    let (owner, dock) = front_dock(state);
    let effects = browser_effects_for_tab(state, &owner, &dock.tab.clone());
    if effects.is_empty() {
        vec![EffectWork::BrowserShow { tab_id: None }.into()]
    } else {
        effects
    }
}

/// The dock a draft was composed in belongs to the thread that send creates.
pub fn hand_over_draft_dock(state: &mut WorkspaceState, task_id: &str) {
    if let Some(dock) = state.docks.shift_remove(DRAFT_DOCK) {
        state.docks.insert(task_id.to_string(), dock);
    }
    if let Some(diff) = state.diffs.shift_remove(DRAFT_DOCK) {
        state.diffs.insert(task_id.to_string(), diff);
    }
}

/// A thread gone for good takes its dock with it: its pages close and its shells stop.
pub fn docks_disposed<'a>(state: &mut WorkspaceState, owners: impl IntoIterator<Item = &'a String>) -> Vec<Effect> {
    let mut effects = Vec::new();
    for owner in owners {
        state.diffs.shift_remove(owner);
        if let Some(dock) = state.docks.shift_remove(owner) {
            effects.extend(dock.browser_tabs.iter().map(|tab| Effect::from(EffectWork::BrowserClose { tab_id: tab.id.clone() })));
            effects.extend(dock.terminals.iter().map(|terminal| Effect::from(EffectWork::TerminalClose { terminal_id: terminal.id.clone() })));
        }
    }
    effects
}

/// Threads gone for good leave no session-only record holding their data.
pub fn pruned(state: &mut WorkspaceState, removed: &HashSet<String>) {
    if removed.is_empty() {
        return;
    }
    state.creating_worktrees.retain(|id| !removed.contains(id));
    state.releasing_worktrees.retain(|id| !removed.contains(id));
    state.moves_after_run.retain(|id, _| !removed.contains(id));
    let history: Vec<String> = state.history.clone();
    let mut index = -1;
    for (position, id) in history.iter().enumerate() {
        if position as i64 > state.history_index {
            break;
        }
        if !removed.contains(id) {
            index += 1;
        }
    }
    state.history.retain(|id| !removed.contains(id));
    state.history_index = index.min(state.history.len() as i64 - 1);
    state.prompts.retain(|key, _| !removed.contains(key));
    state.annotations.retain(|key, _| !removed.contains(key));
    state.pastes.retain(|key, _| !removed.contains(key));
    state.images.retain(|key, _| !removed.contains(key));
    state.reading_points.retain(|key, _| !removed.contains(key));
    state.side_chats.retain(|chat| !removed.contains(&chat.id) && !removed.contains(&chat.source_thread_id));
    state.docks.retain(|owner, _| !removed.contains(owner));
    for dock in state.docks.values_mut() {
        if dock.thread_tabs.iter().any(|id| removed.contains(id)) {
            dock.thread_tabs.retain(|id| !removed.contains(id));
            if removed.contains(&dock.tab) {
                dock.tab = DOCK_PICKER.into();
            }
        }
    }
    state.diffs.retain(|owner, _| !removed.contains(owner));
    if state.dock_focus.as_ref().is_some_and(|focus| removed.contains(&focus.owner) || removed.contains(&focus.tab)) {
        state.dock_focus = None;
    }
    if let Some(tab) = state.keyboard_tab.clone() {
        let kept = state.side_chats.iter().any(|chat| chat.id == tab)
            || state.docks.values().any(|dock| dock.panels.contains(&tab) || dock.browser_tabs.iter().any(|page| page.id == tab) || dock.terminals.iter().any(|terminal| terminal.id == tab) || dock.thread_tabs.contains(&tab));
        if !kept {
            state.keyboard_tab = None;
        }
    }
    state.pending_runs.retain(|_, pending| pending.task_id.as_ref().is_none_or(|id| !removed.contains(id)));
    state.queued_messages.retain(|id, _| !removed.contains(id));
    state.last_run_ids.retain(|id, _| !removed.contains(id));
    state.active_runs.retain(|id, _| !removed.contains(id));
    state.run_statuses.retain(|id, _| !removed.contains(id));
    state.approvals.retain(|_, approval| !removed.contains(&approval.task_id));
    state.streaming_tails.retain(|id, _| !removed.contains(id));
    state.background_processes.retain(|id, _| !removed.contains(id));
    state.workflows.retain(|id, _| !removed.contains(id));
    state.subagents.retain(|id, _| !removed.contains(id));
    state.automations.retain(|view| !removed.contains(&view.automation.task_id));
    if state.browser_approval.as_ref().is_some_and(|approval| removed.contains(&approval.task_id)) {
        state.browser_approval = None;
    }
}

/// Retires the schedules of threads that are going.
/// One delete for each of `task_ids` with a schedule, in the order given.
pub fn retire_automations<'a>(state: &WorkspaceState, task_ids: impl IntoIterator<Item = &'a String>) -> Vec<Effect> {
    let scheduled: HashSet<&str> = state.automations.iter().map(|view| view.automation.task_id.as_str()).collect();
    task_ids.into_iter().filter(|id| scheduled.contains(id.as_str())).map(|task_id| EffectWork::AutomationDelete { task_id: task_id.clone() }.into()).collect()
}

/// Side chats closing: their drafts go, their runs stop, their threads go, and a dock showing one moves on.
pub fn side_chats_closed(state: &mut WorkspaceState, closing: &[SideChat]) -> Step {
    let closed: HashSet<String> = closing.iter().map(|chat| chat.id.clone()).collect();
    let mut effects: Vec<Effect> = Vec::new();
    for chat in closing {
        crate::reducer::composer::cleared_draft(state, &chat.id);
        if let Some(active) = state.active_runs.get(&chat.id) {
            effects.push(EffectWork::SendRunCommand { command: crate::effects::RunCommand::Cancel { task_id: chat.id.clone(), run_id: active.run_id.clone() } }.into());
        }
    }
    state.threads.retain(|thread| !closed.contains(&thread.id));
    let owners: Vec<String> = state.docks.keys().cloned().collect();
    for owner in owners {
        let tab = dock_for(state, &owner).tab.clone();
        if closed.contains(&tab) {
            let after = dock_tab_after_closing(state, &owner, &tab);
            dock_mut(state, &owner).tab = after;
        }
    }
    effects.extend(retire_automations(state, closing.iter().map(|chat| &chat.id)));
    pruned(state, &closed);
    Step::effects(effects)
}

/// A coordinator's dock leads with its Overview; a thread that stops coordinating loses the tab.
pub fn with_overview_panels(state: &mut WorkspaceState) {
    let leads: Vec<String> = state.threads.iter().filter(|thread| is_coordinator(Some(thread))).map(|thread| thread.id.clone()).collect();
    for owner in &leads {
        let dock = dock_for(state, owner);
        if dock.panels.iter().any(|panel| panel == OVERVIEW_PANEL) {
            continue;
        }
        let tab = if !dock.open && dock.tab == DOCK_PICKER { OVERVIEW_PANEL.to_string() } else { dock.tab.clone() };
        let dock = dock_mut(state, owner);
        dock.panels.insert(0, OVERVIEW_PANEL.into());
        dock.tab = tab;
    }
    let owners: Vec<String> = state.docks.keys().cloned().collect();
    for owner in owners {
        if leads.contains(&owner) || !dock_for(state, &owner).panels.iter().any(|panel| panel == OVERVIEW_PANEL) {
            continue;
        }
        let tab = if dock_for(state, &owner).tab == OVERVIEW_PANEL { dock_tab_after_closing(state, &owner, OVERVIEW_PANEL) } else { dock_for(state, &owner).tab.clone() };
        let dock = dock_mut(state, &owner);
        dock.panels.retain(|panel| panel != OVERVIEW_PANEL);
        dock.tab = tab;
    }
}

/// A dock follows a workflow only while its record is there.
pub fn pruned_workflow_panels(state: &mut WorkspaceState) {
    let owners: Vec<String> = state.docks.iter().filter(|(_, dock)| dock.workflow_id.is_some()).map(|(owner, _)| owner.clone()).collect();
    for owner in owners {
        let workflow_id = dock_for(state, &owner).workflow_id.clone();
        if workflow_by_id(state, workflow_id.as_deref()).is_some() {
            continue;
        }
        let tab = if dock_for(state, &owner).tab == WORKFLOW_PANEL { dock_tab_after_closing(state, &owner, WORKFLOW_PANEL) } else { dock_for(state, &owner).tab.clone() };
        let dock = dock_mut(state, &owner);
        dock.workflow_id = None;
        dock.panels.retain(|panel| panel != WORKFLOW_PANEL);
        dock.tab = tab;
    }
}

/// The checkout the thread in front works in.
pub fn current_workspace_id(state: &WorkspaceState) -> Option<String> {
    match state.current_thread() {
        Some(thread) => thread_workspace_id(state, Some(thread)),
        None => state.draft_project_id.as_deref().and_then(|id| state.project(id)).and_then(|project| project.workspace_id.clone()),
    }
}

/// The checkout a dock's review reads: the thread tab it is about, else the thread in front.
pub fn subject_workspace_id(state: &WorkspaceState, owner: &str) -> Option<String> {
    match dock_subject(state, owner, DIFF_PANEL) {
        Some(subject) => thread_workspace_id(state, state.thread(&subject)),
        None => current_workspace_id(state),
    }
}

/// Asks Git about one thread's checkout, naming the run its answer follows.
pub fn refresh_thread_environment(state: &WorkspaceState, task_id: &str) -> Vec<Effect> {
    let Some(workspace_id) = thread_workspace_id(state, state.thread(task_id)) else { return Vec::new() };
    vec![EffectWork::RefreshEnvironment { workspace_id, task_id: Some(task_id.into()), run_id: state.last_run_ids.get(task_id).cloned() }.into()]
}

/// Asks Git about the checkout the thread in front works in, and those of the threads open as its tabs.
pub fn refresh_environment(state: &WorkspaceState) -> Vec<Effect> {
    let owner = dock_owner(state, None);
    let tabs: Vec<Effect> = dock_for(state, &owner).thread_tabs.iter().flat_map(|task_id| refresh_thread_environment(state, task_id)).collect();
    if let Some(current) = state.current_thread() {
        let mut effects = refresh_thread_environment(state, &current.id);
        effects.extend(tabs);
        return effects;
    }
    let mut effects: Vec<Effect> = current_workspace_id(state).map(|workspace_id| EffectWork::RefreshEnvironment { workspace_id, task_id: None, run_id: None }.into()).into_iter().collect();
    effects.extend(tabs);
    effects
}

/// What Git last said about a checkout, or None while none of it has been read yet.
pub fn environment_for<'a>(state: &'a WorkspaceState, workspace_id: Option<&str>) -> Option<&'a ChangedFilesResult> {
    state.environments.get(workspace_id?)
}

fn default_range_for(state: &WorkspaceState, owner: &str) -> DiffRange {
    match environment_for(state, subject_workspace_id(state, owner).as_deref()) {
        Some(ChangedFilesResult::Available { baseline: Some(baseline), .. }) => DiffRange::Branches { base: baseline.clone(), compare: None },
        _ => default_branch_range(),
    }
}

fn initial_range(state: &WorkspaceState, owner: &str) -> DiffRange {
    let diff = diff_for(state, owner);
    if diff.mode != DiffStateMode::Branch || diff.workspace_id.is_some() || diff.result.is_some() {
        return diff.range.clone();
    }
    default_range_for(state, owner)
}

/// What a fresh review read starts over with.
#[derive(Clone, Default)]
pub struct DiffReset {
    pub mode: Option<DiffStateMode>,
    pub clear: bool,
    pub ignore_whitespace: Option<bool>,
}

/// Reads a dock's review of `workspace_id` over `range`.
pub fn diff_read_from(state: &mut WorkspaceState, owner: &str, workspace_id: Option<&str>, range: &DiffRange, reset: DiffReset) -> Vec<Effect> {
    let previous = diff_for(state, owner).clone();
    // A review with no checkout yet is never the same one as an absent checkout.
    let same_workspace = previous.workspace_id.is_some() && previous.workspace_id.as_deref() == workspace_id;
    let ignore_whitespace = reset.ignore_whitespace.unwrap_or(previous.ignore_whitespace);
    let diff = diff_mut(state, owner);
    if let Some(mode) = reset.mode {
        diff.mode = mode;
    }
    if reset.clear {
        diff.result = None;
        diff.collapsed.clear();
        diff.viewed = Map::new();
    }
    diff.ignore_whitespace = ignore_whitespace;
    diff.mode = mode_for_range(range);
    diff.branch_range = match range {
        DiffRange::Branches { .. } => Some(range.clone()),
        _ if same_workspace => previous.branch_range.clone(),
        _ => None,
    };
    diff.range = range.clone();
    let Some(workspace_id) = workspace_id else {
        diff.workspace_id = None;
        diff.result = None;
        diff.loading = false;
        return Vec::new();
    };
    diff.workspace_id = Some(workspace_id.into());
    diff.loading = true;
    vec![EffectWork::ReadDiff { owner: owner.into(), workspace_id: workspace_id.into(), range: range.clone(), ignore_whitespace }.into()]
}

fn diff_read(state: &mut WorkspaceState, owner: &str, range: &DiffRange, reset: DiffReset) -> Vec<Effect> {
    let workspace_id = match range {
        DiffRange::Commit { .. } => diff_for(state, owner).workspace_id.clone().or_else(|| subject_workspace_id(state, owner)),
        _ => subject_workspace_id(state, owner),
    };
    diff_read_from(state, owner, workspace_id.as_deref(), range, reset)
}

/// Reads the review for the checkout it is about now, starting over when that is another checkout.
fn review_subject(state: &mut WorkspaceState, owner: &str) -> Vec<Effect> {
    let diff = diff_for(state, owner).clone();
    if diff.workspace_id.is_none() || diff.workspace_id == subject_workspace_id(state, owner) {
        let range = initial_range(state, owner);
        return diff_read(state, owner, &range, DiffReset::default());
    }
    let range = if diff.mode == DiffStateMode::Uncommitted { diff.range.clone() } else { default_range_for(state, owner) };
    diff_read(state, owner, &range, DiffReset { clear: true, ..DiffReset::default() })
}

/// Every review whose subject is this thread, read again against its checkout now.
pub fn reviews_reread(state: &mut WorkspaceState, task_id: &str, workspace_id: &str) -> Vec<Effect> {
    let owners: Vec<(String, DiffRange)> = state.diffs.iter().filter(|(owner, _)| dock_subject(state, owner, DIFF_PANEL).unwrap_or_else(|| owner.to_string()) == task_id).map(|(owner, diff)| (owner.clone(), diff.range.clone())).collect();
    let mut effects = Vec::new();
    for (owner, range) in owners {
        effects.extend(diff_read_from(state, &owner, Some(workspace_id), &range, DiffReset::default()));
    }
    follow_ups(effects)
}

/// A thread's own review read again once it works in another checkout.
pub fn diff_reread(state: &mut WorkspaceState, task_id: &str) -> Vec<Effect> {
    let Some(diff) = state.diffs.get(task_id).cloned() else { return Vec::new() };
    let subject = dock_subject(state, task_id, DIFF_PANEL).unwrap_or_else(|| task_id.to_string());
    let workspace_id = thread_workspace_id(state, state.thread(&subject));
    if diff.workspace_id == workspace_id {
        return Vec::new();
    }
    follow_ups(diff_read_from(state, task_id, workspace_id.as_deref(), &diff.range, DiffReset { clear: true, ..DiffReset::default() }))
}

/// ⌘W: whatever is in front closes, a dialog before the settings, the settings before the dock.
fn close_front(state: &mut WorkspaceState) -> Step {
    let (owner, dock) = front_dock(state);
    let dock = dock.clone();
    if state.worktree_move.is_some() {
        state.worktree_move = None;
        return Step::none();
    }
    if state.fork_name.is_some() {
        state.fork_name = None;
        return Step::none();
    }
    if state.archive_prompt.is_some() {
        state.archive_prompt = None;
        return Step::none();
    }
    if state.project_edit.is_some() {
        state.project_edit = None;
        return Step::none();
    }
    if state.settings_open || state.computer_use_setup {
        return crate::reducer::settings::settings_shown(state, false, None, None);
    }
    if !dock.open {
        return Step::effect(EffectWork::CloseWindow);
    }
    if dock.expanded {
        dock_mut(state, &owner).expanded = false;
        return Step::none();
    }
    let kind = dock_tab_kind(state, &owner, &dock.tab);
    let closed = if kind == DockTabKind::Picker || dock.tab == OVERVIEW_PANEL {
        dock_mut(state, &owner).open = false;
        Step::none()
    } else {
        match kind {
            DockTabKind::Browser => {
                let holder = owner_of_browser_tab(state, &dock.tab).unwrap_or_else(|| dock_owner(state, None));
                crate::reducer::misc::browser_tab_closed(state, &holder, &dock.tab)
            }
            DockTabKind::Terminal => reduce_terminal(state, &AppCommand::TerminalClose { terminal_id: dock.tab.clone() }),
            DockTabKind::SideChat => {
                let closing: Vec<SideChat> = state.side_chats.iter().filter(|chat| chat.id == dock.tab).cloned().collect();
                side_chats_closed(state, &closing)
            }
            DockTabKind::Thread => reduce_dock(state, &AppCommand::CloseThreadTab { task_id: dock.tab.clone() }),
            _ => reduce_dock(state, &AppCommand::CloseDockPanel { panel: dock.tab.clone() }),
        }
    };
    let mut closed = closed;
    closed.effects.extend(take_keys());
    closed
}

/// A dock just shown or widened hands its tab the keys, unless that tab is the picker.
fn focused_unless_picker(state: &mut WorkspaceState, owner: &str, tab: &str) -> Step {
    if dock_tab_kind(state, owner, tab) == DockTabKind::Picker {
        return Step::none();
    }
    Step::effects(focus_dock_tab(state, owner, tab))
}

pub fn reduce_dock(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::CloseTab => close_front(state),
        AppCommand::NewTab => {
            if state.settings_open {
                return Step::none();
            }
            let (owner, dock) = front_dock(state);
            let kind = if dock.open { dock_tab_kind(state, &owner, &dock.tab.clone()) } else { DockTabKind::Picker };
            if kind == DockTabKind::Browser {
                crate::reducer::misc::new_browser_tab(state)
            } else {
                reduce_terminal(state, &AppCommand::TerminalOpen { cwd: None })
            }
        }
        AppCommand::SelectDockIndex { index } => {
            let owner = dock_owner(state, None);
            let tabs = dock_tab_ids(state, &owner);
            let tab = if *index == -1 { tabs.last().cloned() } else { usize::try_from(*index).ok().and_then(|index| tabs.get(index).cloned()) }.filter(|tab| !tab.is_empty());
            match tab {
                Some(tab) => reduce_dock(state, &AppCommand::SelectDockTab { tab }),
                None => Step::none(),
            }
        }
        AppCommand::SetDockOpen { open } => {
            let (owner, dock) = front_dock(state);
            if dock.open == *open {
                return Step::none();
            }
            let tab = dock.tab.clone();
            dock_mut(state, &owner).open = *open;
            if !open {
                dock_mut(state, &owner).expanded = false;
                return Step::effects(take_keys());
            }
            focused_unless_picker(state, &owner, &tab)
        }
        AppCommand::SetDockExpanded { expanded } => {
            let (owner, dock) = front_dock(state);
            if dock.expanded == *expanded {
                return Step::none();
            }
            let tab = dock.tab.clone();
            let open = *expanded || dock.open;
            let dock = dock_mut(state, &owner);
            dock.expanded = *expanded;
            dock.open = open;
            focused_unless_picker(state, &owner, &tab)
        }
        AppCommand::OpenDockPanel { panel, task_id } => {
            let (owner, dock) = front_dock(state);
            let subject = task_id.clone().filter(|id| dock.thread_tabs.contains(id));
            let dock = dock_mut(state, &owner);
            if !dock.panels.contains(panel) {
                dock.panels.push(panel.clone());
            }
            dock.subjects.shift_remove(panel);
            if let Some(subject) = subject {
                dock.subjects.insert(panel.clone(), subject);
            }
            dock.open = true;
            dock.tab = panel.clone();
            let focus = focus_dock_tab(state, &owner, panel);
            let mut effects = browser_effects_for_tab(state, &owner, panel);
            effects.extend(focus);
            if panel != DIFF_PANEL {
                return Step::effects(effects);
            }
            effects.extend(review_subject(state, &owner));
            Step::effects(effects)
        }
        AppCommand::OpenWorkflow { workflow_id } => {
            let listed = state.current_id.as_deref().and_then(|id| state.workflows.get(id)).is_some_and(|workflows| workflows.iter().any(|workflow| &workflow.id == workflow_id));
            if !listed {
                return Step::none();
            }
            let owner = dock_owner(state, None);
            dock_mut(state, &owner).workflow_id = Some(workflow_id.clone());
            reduce_dock(state, &AppCommand::OpenDockPanel { panel: WORKFLOW_PANEL.into(), task_id: None })
        }
        AppCommand::CloseDockPanel { panel } => {
            let (owner, dock) = front_dock(state);
            if !dock.panels.contains(panel) || panel == OVERVIEW_PANEL {
                return Step::none();
            }
            let tab = if &dock.tab == panel { dock_tab_after_closing(state, &owner, panel) } else { dock.tab.clone() };
            let dock = dock_mut(state, &owner);
            dock.panels.retain(|item| item != panel);
            dock.tab = tab.clone();
            if panel == WORKFLOW_PANEL {
                dock.workflow_id = None;
            }
            Step::effects(browser_effects_for_tab(state, &owner, &tab))
        }
        AppCommand::CloseThreadTab { task_id } => {
            let (owner, dock) = front_dock(state);
            let dock = dock.clone();
            if !dock.thread_tabs.contains(task_id) {
                return Step::none();
            }
            let last = dock.thread_tabs.len() == 1 && dock.panels.iter().any(|panel| panel == OVERVIEW_PANEL);
            let tab = if &dock.tab != task_id {
                dock.tab.clone()
            } else if last {
                OVERVIEW_PANEL.to_string()
            } else {
                dock_tab_after_closing(state, &owner, task_id)
            };
            let open = dock.open && tab != DOCK_PICKER;
            {
                let held = dock_mut(state, &owner);
                held.thread_tabs.retain(|id| id != task_id);
                held.tab = tab.clone();
                held.open = open;
                held.subjects.retain(|_, id| id != task_id);
                if !open {
                    held.expanded = false;
                }
            }
            let mut effects = if open { browser_effects_for_tab(state, &owner, &tab) } else { take_keys() };
            if dock.subjects.get(DIFF_PANEL) == Some(task_id) && dock.panels.iter().any(|panel| panel == DIFF_PANEL) {
                let reread = review_subject(state, &owner);
                effects.extend(follow_ups(reread));
            }
            Step::effects(effects)
        }
        AppCommand::SetOverviewGroup { task_id, group, open, all } => {
            let fold = state.overview_folds.get(task_id).and_then(|folds| folds.get(group)).cloned().unwrap_or_default();
            let next = OverviewFold { open: open.or(fold.open), all: fold.all || *all == Some(true) };
            if next == fold {
                return Step::none();
            }
            state.overview_folds.entry(task_id.clone()).or_default().insert(*group, next);
            Step::none()
        }
        AppCommand::SelectDockTab { tab } => {
            let owner = dock_owner(state, None);
            let kind = dock_tab_kind(state, &owner, tab);
            {
                let dock = dock_mut(state, &owner);
                dock.tab = tab.clone();
                dock.open = true;
                if kind == DockTabKind::Browser {
                    dock.browser_tab_id = Some(tab.clone());
                }
                if kind == DockTabKind::Terminal {
                    dock.terminal_id = Some(tab.clone());
                }
            }
            let focus = focus_dock_tab(state, &owner, tab);
            if matches!(kind, DockTabKind::SideChat | DockTabKind::Thread) {
                read_attention(&mut state.threads, Some(tab));
            }
            let mut effects = browser_effects_for_tab(state, &owner, tab);
            effects.extend(focus);
            Step::effects(effects)
        }
        _ => Step::none(),
    }
}

/// The diff panel: which comparison it shows, which file is open, and which files are ticked off.
pub fn reduce_diffs(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::DiffOpenCommit { commit, task_id } => {
            if !is_commit_hash(commit) {
                return Step::refuse("Invalid commit hash.");
            }
            let thread_id = task_id.clone().or_else(|| state.current_id.clone());
            let Some(workspace_id) = thread_workspace_id(state, thread_id.as_deref().and_then(|id| state.thread(id))) else { return Step::refuse("Open this thread's project to inspect the commit.") };
            let owner = dock_owner(state, None);
            {
                let dock = dock_mut(state, &owner);
                if !dock.panels.iter().any(|panel| panel == DIFF_PANEL) {
                    dock.panels.push(DIFF_PANEL.into());
                }
            }
            show_dock_tab(state, &owner, DIFF_PANEL);
            let mut effects = focus_dock_tab(state, &owner, DIFF_PANEL);
            let read = diff_read_from(state, &owner, Some(&workspace_id), &DiffRange::Commit { commit: commit.clone() }, DiffReset { mode: Some(DiffStateMode::Commit), clear: true, ignore_whitespace: None });
            effects.extend(browser_effects_for_tab(state, &owner, DIFF_PANEL));
            effects.extend(read);
            Step::effects(effects)
        }
        AppCommand::RefreshEnvironment => Step::effects(refresh_environment(state)),
        AppCommand::DiffToggle { task_id } => {
            let owner = dock_owner(state, None);
            let dock = dock_for(state, &owner);
            let showing = dock.open && dock.tab == DIFF_PANEL && dock_subject(state, &owner, DIFF_PANEL) == *task_id;
            if showing {
                reduce_dock(state, &AppCommand::CloseDockPanel { panel: DIFF_PANEL.into() })
            } else {
                reduce_dock(state, &AppCommand::OpenDockPanel { panel: DIFF_PANEL.into(), task_id: task_id.clone() })
            }
        }
        AppCommand::DiffRefresh => {
            let owner = dock_owner(state, None);
            let range = diff_for(state, &owner).range.clone();
            Step::effects(diff_read(state, &owner, &range, DiffReset::default()))
        }
        AppCommand::DiffSetMode { mode } => {
            state.open_menu = None;
            let owner = dock_owner(state, None);
            let diff = diff_for(state, &owner).clone();
            if diff.mode == DiffStateMode::from(*mode) {
                return Step::none();
            }
            let range = match mode {
                DiffMode::Uncommitted => DiffRange::Uncommitted,
                DiffMode::Branch => (if diff.workspace_id == subject_workspace_id(state, &owner) { diff.branch_range.clone() } else { None }).unwrap_or_else(|| default_range_for(state, &owner)),
            };
            Step::effects(diff_read(state, &owner, &range, DiffReset { mode: Some((*mode).into()), clear: true, ignore_whitespace: None }))
        }
        AppCommand::DiffSetRange { range } => {
            let owner = dock_owner(state, None);
            let diff = diff_for(state, &owner);
            if diff.mode == mode_for_range(range) && range_key(&diff.range) == range_key(range) {
                return Step::none();
            }
            Step::effects(diff_read(state, &owner, range, DiffReset { mode: Some(mode_for_range(range)), clear: true, ignore_whitespace: None }))
        }
        AppCommand::DiffSetCollapsed { path, collapsed } => {
            let owner = dock_owner(state, None);
            let diff = diff_mut(state, &owner);
            if *collapsed {
                if !diff.collapsed.contains(path) {
                    diff.collapsed.push(path.clone());
                }
            } else {
                diff.collapsed.retain(|item| item != path);
            }
            Step::none()
        }
        AppCommand::DiffSetViewed { path, viewed } => {
            let owner = dock_owner(state, None);
            let fingerprint = match &diff_for(state, &owner).result {
                Some(DiffSummaryResult::Available { files, .. }) => files.iter().find(|file| &file.path == path).map(file_fingerprint),
                _ => None,
            };
            let Some(fingerprint) = fingerprint else { return Step::none() };
            let diff = diff_mut(state, &owner);
            diff.viewed.shift_remove(path);
            if *viewed {
                diff.viewed.insert(path.clone(), fingerprint);
                if !diff.collapsed.contains(path) {
                    diff.collapsed.push(path.clone());
                }
            } else {
                diff.collapsed.retain(|item| item != path);
            }
            Step::none()
        }
        AppCommand::DiffSetSplit { split } => {
            let owner = dock_owner(state, None);
            diff_mut(state, &owner).split = *split;
            Step::none()
        }
        AppCommand::DiffSetIgnoreWhitespace { ignore } => {
            let owner = dock_owner(state, None);
            let diff = diff_for(state, &owner);
            if diff.ignore_whitespace == *ignore {
                return Step::none();
            }
            let range = diff.range.clone();
            Step::effects(diff_read(state, &owner, &range, DiffReset { ignore_whitespace: Some(*ignore), ..DiffReset::default() }))
        }
        _ => Step::none(),
    }
}

pub fn reduce_diff_event(state: &mut WorkspaceState, event: &WorkspaceEvent) -> Step {
    match event {
        WorkspaceEvent::DiffLoaded { owner, workspace_id, range, result } => {
            let diff = diff_for(state, owner).clone();
            if !diff_matches(&diff, workspace_id, range) {
                return Step::none();
            }
            if let DiffSummaryResult::Available { ignore_whitespace, .. } = result {
                if *ignore_whitespace != diff.ignore_whitespace {
                    return Step::none();
                }
            }
            let viewed = retained_views(&diff.viewed, result, diff.result.as_ref());
            let collapsed = match result {
                DiffSummaryResult::Available { files, .. } => Some(folded_on_load(&diff, files, result)),
                _ => None,
            };
            let held = diff_mut(state, owner);
            held.result = Some(result.clone());
            held.loading = false;
            held.viewed = viewed;
            if let Some(collapsed) = collapsed {
                held.collapsed = collapsed;
            }
            match result {
                DiffSummaryResult::Error { message } => Step::result(CommandResult::failed(message.clone())),
                _ => Step::none(),
            }
        }
        WorkspaceEvent::EnvironmentUpdated { workspace_id, task_id, run_id, result } => {
            if state.environments.get(workspace_id) != Some(result) {
                let live: HashSet<String> = std::iter::once(workspace_id.clone())
                    .chain(state.projects.iter().filter_map(|project| project.workspace_id.clone()))
                    .chain(state.worktrees.iter().map(|worktree| worktree.workspace_id.clone()))
                    .collect();
                state.environments.retain(|id, _| live.contains(id));
                state.environments.insert(workspace_id.clone(), result.clone());
            }
            if let ChangedFilesResult::Error { message } = result {
                return Step::result(CommandResult::failed(message.clone()));
            }
            if let (Some(run_id), Some(task_id)) = (run_id, task_id) {
                if state.last_run_ids.get(task_id) != Some(run_id) {
                    return Step::none();
                }
            }
            let (Some(task_id), ChangedFilesResult::Available { files, .. }) = (task_id, result) else { return Step::none() };
            let Some(thread) = state.thread(task_id) else { return Step::none() };
            if &thread.last_change_snapshot.files == files {
                return Step::none();
            }
            if thread_workspace_id(state, Some(thread)).is_some_and(|id| &id != workspace_id) {
                return Step::none();
            }
            if let Some(thread) = state.thread_mut(task_id) {
                thread.last_change_snapshot = crate::domain::thread::ChangeSnapshot { files: files.clone(), captured_at: now() };
                thread.updated_at = now();
            }
            Step::none()
        }
        _ => Step::none(),
    }
}

fn terminal_title(cwd: &str) -> String {
    cwd.split('/').filter(|part| !part.is_empty()).next_back().unwrap_or(cwd).to_string()
}

/// The terminal panel. Every command here is the user's own.
pub fn reduce_terminal(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::TerminalOpen { cwd } => {
            let owner = dock_owner(state, None);
            let Some(cwd) = cwd.clone().or_else(|| current_folder(state)).filter(|cwd| !cwd.is_empty()) else { return Step::refuse(TERMINAL_FOLDER_ERROR) };
            let terminal = TerminalSession { id: random_id(), title: terminal_title(&cwd), cwd: cwd.clone(), task_id: state.current_id.clone(), status: TerminalStatus::Running, exit_code: None, error: None };
            let terminal_id = terminal.id.clone();
            let dock = dock_mut(state, &owner);
            dock.open = true;
            dock.tab = terminal_id.clone();
            dock.terminals.push(terminal);
            dock.terminal_id = Some(terminal_id.clone());
            state.action_error = None;
            let mut effects: Vec<Effect> = vec![EffectWork::TerminalStart { terminal_id: terminal_id.clone(), cwd }.into()];
            effects.extend(focus_dock_tab(state, &owner, &terminal_id));
            Step::effects(effects)
        }
        AppCommand::TerminalSelect { terminal_id } => {
            let Some(owner) = owner_of_terminal(state, terminal_id) else { return Step::none() };
            show_dock_tab(state, &owner, terminal_id);
            dock_mut(state, &owner).terminal_id = Some(terminal_id.clone());
            Step::effects(focus_dock_tab(state, &owner, terminal_id))
        }
        AppCommand::TerminalClose { terminal_id } => {
            let Some(owner) = owner_of_terminal(state, terminal_id) else { return Step::none() };
            let dock = dock_for(state, &owner).clone();
            let index = dock.terminals.iter().position(|terminal| &terminal.id == terminal_id).unwrap_or(0);
            let tab = if &dock.tab == terminal_id { dock_tab_after_closing(state, &owner, terminal_id) } else { dock.tab.clone() };
            let terminals: Vec<TerminalSession> = dock.terminals.iter().filter(|terminal| &terminal.id != terminal_id).cloned().collect();
            let next = if dock.terminal_id.as_ref() == Some(terminal_id) {
                index.checked_sub(1).and_then(|left| terminals.get(left)).or(terminals.get(index)).map(|terminal| terminal.id.clone())
            } else {
                terminals.iter().find(|terminal| Some(&terminal.id) == dock.terminal_id.as_ref()).map(|terminal| terminal.id.clone())
            };
            let held = dock_mut(state, &owner);
            held.terminals = terminals;
            held.terminal_id = next;
            held.tab = tab;
            Step::effect(EffectWork::TerminalClose { terminal_id: terminal_id.clone() })
        }
        AppCommand::TerminalRestart { terminal_id } => {
            let Some(owner) = owner_of_terminal(state, terminal_id) else { return Step::none() };
            let Some(old) = dock_for(state, &owner).terminals.iter().find(|terminal| &terminal.id == terminal_id).cloned() else { return Step::none() };
            let terminal = TerminalSession { id: random_id(), title: terminal_title(&old.cwd), cwd: old.cwd.clone(), task_id: old.task_id.clone(), status: TerminalStatus::Running, exit_code: None, error: None };
            let new_id = terminal.id.clone();
            let held = dock_mut(state, &owner);
            for item in held.terminals.iter_mut() {
                if item.id == old.id {
                    *item = terminal.clone();
                }
            }
            held.terminal_id = Some(new_id.clone());
            show_dock_tab(state, &owner, &new_id);
            state.action_error = None;
            let mut effects: Vec<Effect> = vec![EffectWork::TerminalClose { terminal_id: old.id }.into(), EffectWork::TerminalStart { terminal_id: new_id.clone(), cwd: old.cwd }.into()];
            effects.extend(focus_dock_tab(state, &owner, &new_id));
            Step::effects(effects)
        }
        AppCommand::TerminalInput { terminal_id, data } => Step::effect(EffectWork::TerminalWrite { terminal_id: terminal_id.clone(), data: data.clone() }),
        AppCommand::TerminalResize { terminal_id, cols, rows } => Step::effect(EffectWork::TerminalResize { terminal_id: terminal_id.clone(), cols: *cols, rows: *rows }),
        _ => Step::none(),
    }
}

pub fn terminal_updated(state: &mut WorkspaceState, update: &crate::domain::terminal::TerminalUpdate) -> Step {
    let Some(owner) = owner_of_terminal(state, &update.terminal_id) else { return Step::none() };
    if let Some(terminal) = dock_mut(state, &owner).terminals.iter_mut().find(|terminal| terminal.id == update.terminal_id) {
        if let Some(title) = &update.title {
            terminal.title = title.clone();
        }
        if let Some(status) = update.status {
            terminal.status = status;
        }
        if update.exit_code.is_some() {
            terminal.exit_code = update.exit_code;
        }
        if update.error.is_some() {
            terminal.error = update.error.clone();
        }
    }
    Step::none()
}

/// Opens a side chat on the thread on screen, as a tab of its dock.
pub fn side_chat_open(state: &mut WorkspaceState, chat_id: &str) -> Step {
    let Some(source) = state.current_thread().cloned() else { return Step::none() };
    let sequence = state.side_chat_sequence + 1;
    let at = now();
    let mut thread = crate::domain::thread::Thread::new(chat_id, format!("Chat {sequence}"), source.engine, source.execution_policy, at);
    thread.project_id = source.project_id.clone();
    thread.model = source.model.clone();
    thread.effort = source.effort;
    if crate::domain::engine::capabilities_for(source.engine).fast_mode {
        thread.fast_mode = Some(source.fast_mode.unwrap_or(false));
    }
    thread.last_change_snapshot.captured_at = at;
    state.side_chats.push(SideChat { id: chat_id.into(), source_thread_id: source.id.clone(), error: None });
    state.side_chat_sequence = sequence;
    state.threads.push(thread.into());
    show_dock_tab(state, &source.id, chat_id);
    Step::effects(focus_dock_tab(state, &source.id, chat_id))
}

pub fn side_chat_close(state: &mut WorkspaceState, chat_id: &str) -> Step {
    let closing: Vec<SideChat> = state.side_chats.iter().filter(|chat| chat.id == chat_id).cloned().collect();
    if closing.is_empty() {
        return Step::none();
    }
    side_chats_closed(state, &closing)
}
