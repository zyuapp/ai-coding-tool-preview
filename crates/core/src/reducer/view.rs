//! Where the user is looking: history, focus, the find bar, the jump panel and zen mode.

use crate::commands::AppCommand;
use crate::domain::find::{find_hits, same_find_target, step_match, FindTarget};
use crate::domain::shortcuts::ShortcutSurface;
use crate::effects::EffectWork;
use crate::events::WorkspaceEvent;
use crate::reducer::dock::refresh_environment;
use crate::reducer::find::{search_effects, stop_search_effects};
use crate::reducer::settings::{settings_shown, stop_capture};
use crate::reducer::threads::{selected, visited};
use crate::reducer::{follow_ups, reachable_visit, take_keys, zen_allowed, Step};
use crate::state::{dock_holds_tab, FindState, JumpState, WorkspaceState};
use crate::view::find::find_target_for;
use crate::view::jump::jump_view;
use crate::workspace::activity::{read_row, thread_activity};

pub fn reduce_view(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::SetMenu { menu } => {
            state.open_menu = menu.clone();
            Step::none()
        }
        AppCommand::GoBack | AppCommand::GoForward => {
            let step = if matches!(command, AppCommand::GoBack) { -1 } else { 1 };
            match reachable_visit(state, step) {
                Some(index) => visited(state, index),
                None => Step::none(),
            }
        }
        AppCommand::SetFocused { focused } => {
            if !focused {
                let effects = stop_capture(state);
                state.focused = false;
                state.capturing_shortcut = None;
                return Step::effects(effects);
            }
            state.focused = true;
            let asked = crate::reducer::misc::refreshed_engines(state);
            if state.selected_computer().is_none() {
                let current = state.current_id.clone();
                read_row(&mut state.threads, current.as_deref());
            }
            let mut effects = follow_ups(refresh_environment(state));
            effects.extend(asked);
            Step::effects(effects)
        }
        AppCommand::DockKeys { tab } => {
            let tab = tab.clone().filter(|tab| !tab.is_empty() && dock_holds_tab(state, tab));
            if state.keyboard_tab != tab {
                state.keyboard_tab = tab;
            }
            Step::none()
        }
        AppCommand::FindOpen { target } => {
            let target = target.clone().or_else(|| state.find.as_ref().map(|find| find.target.clone())).unwrap_or_else(|| find_target_for(state, ShortcutSurface::Any));
            let previous = state.find.clone();
            let same = previous.as_ref().is_some_and(|previous| same_find_target(&previous.target, &target));
            let find = FindState {
                target: target.clone(),
                query: previous.as_ref().map(|previous| previous.query.clone()).unwrap_or_default(),
                index: if same { previous.as_ref().map(|previous| previous.index).unwrap_or(0) } else { 0 },
                focus: previous.as_ref().map(|previous| previous.focus).unwrap_or(0) + 1,
            };
            let mut effects = Vec::new();
            if !same {
                effects.extend(stop_search_effects(previous.as_ref()));
            }
            if matches!(target, FindTarget::Browser { .. }) {
                effects.push(EffectWork::FocusWindow.into());
            }
            if !same {
                effects.extend(search_effects(&find, false, true));
                state.find_results = None;
            }
            state.find = Some(find);
            Step::effects(effects)
        }
        AppCommand::FindQuery { query } => {
            let Some(mut find) = state.find.clone() else { return Step::none() };
            find.query = query.clone();
            find.index = 0;
            let effects = if find.query.trim().is_empty() { stop_search_effects(Some(&find)) } else { search_effects(&find, false, true) };
            state.find = Some(find);
            state.find_results = None;
            Step::effects(effects)
        }
        AppCommand::FindStep { delta } => {
            let Some(find) = state.find.clone() else { return Step::none() };
            if find.target.searches_itself() {
                return Step::effects(search_effects(&find, true, *delta == 1));
            }
            let matches = match &find.target {
                FindTarget::Thread { task_id } => find_hits(task_id.as_deref().and_then(|id| state.thread(id)).map(|thread| thread.messages.as_slice()).unwrap_or(&[]), &find.query).len() as i64,
                _ => state.find_results.as_ref().map(|results| results.matches).unwrap_or(0),
            };
            if let Some(held) = state.find.as_mut() {
                held.index = step_match(find.index, *delta as i64, matches);
            }
            Step::none()
        }
        AppCommand::FindClose => {
            let mut effects = stop_search_effects(state.find.as_ref());
            if let Some(FindTarget::Browser { tab_id }) = state.find.as_ref().map(|find| &find.target) {
                effects.push(EffectWork::FocusBrowser { tab_id: tab_id.clone() }.into());
            }
            state.find = None;
            state.find_results = None;
            Step::effects(effects)
        }
        AppCommand::JumpOpen => {
            state.jump = Some(JumpState { query: String::new(), index: 0 });
            state.open_menu = None;
            Step::effects(take_keys())
        }
        AppCommand::JumpQuery { query } => {
            if state.jump.is_some() {
                state.jump = Some(JumpState { query: query.clone(), index: 0 });
            }
            Step::none()
        }
        AppCommand::JumpStep { delta } => {
            let busy = thread_activity(state).working;
            let Some(jump) = jump_view(state, &busy) else { return Step::none() };
            state.jump = Some(JumpState { query: jump.query.clone(), index: step_match(jump.index, *delta as i64, jump.options.len() as i64) });
            Step::none()
        }
        AppCommand::JumpChoose { task_id } => {
            state.jump = None;
            selected(state, task_id)
        }
        AppCommand::JumpChooseSetting { section, setting_id } => {
            state.jump = None;
            settings_shown(state, true, Some(*section), setting_id.as_deref())
        }
        AppCommand::JumpClose => {
            state.jump = None;
            Step::none()
        }
        AppCommand::SetZen { zen } => {
            if (*zen && !zen_allowed(state)) || state.zen == *zen {
                return Step::none();
            }
            state.zen = *zen;
            state.keyboard_tab = None;
            state.composer_focus += 1;
            Step::effects(take_keys())
        }
        AppCommand::DismissComputerUseSetup => {
            state.computer_use_setup = false;
            Step::none()
        }
        _ => Step::none(),
    }
}

/// What a page or a shell, or a view that counts itself, reported after searching.
pub fn find_results(state: &mut WorkspaceState, event: &WorkspaceEvent) -> Step {
    let WorkspaceEvent::FindResults { target, results } = event else { return Step::none() };
    let Some(find) = state.find.clone() else { return Step::none() };
    if !same_find_target(&find.target, target) {
        return Step::none();
    }
    let moved = if !find.target.searches_itself() { results.index.filter(|index| *index != find.index) } else { None };
    if moved.is_none() && state.find_results.as_ref() == Some(results) {
        return Step::none();
    }
    state.find_results = Some(results.clone());
    if let Some(index) = moved {
        if let Some(held) = state.find.as_mut() {
            held.index = index;
        }
    }
    Step::none()
}
