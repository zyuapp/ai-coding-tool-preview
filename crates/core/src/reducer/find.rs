//! The find bar's target: what it asks of the view holding the text, and when that view is gone.

use crate::domain::find::FindTarget;
use crate::effects::{Effect, EffectWork};
use crate::state::{dock_holds_tab, dock_side_chats, front_dock, owner_of_browser_tab, owner_of_terminal, FindState, WorkspaceState, DIFF_PANEL};

/// What a search asks of whoever holds the text. A thread and a review are counted where they are drawn.
pub fn search_effects(find: &FindState, find_next: bool, forward: bool) -> Vec<Effect> {
    let query = find.query.trim().to_string();
    if query.is_empty() {
        return Vec::new();
    }
    match &find.target {
        FindTarget::Browser { tab_id } => vec![EffectWork::FindInPage { tab_id: tab_id.clone(), query, forward, find_next }.into()],
        FindTarget::Terminal { terminal_id } => vec![EffectWork::FindInTerminal { terminal_id: terminal_id.clone(), query, forward }.into()],
        _ => Vec::new(),
    }
}

/// A page and a shell keep highlighting what was found until they are told to stop.
pub fn stop_search_effects(find: Option<&FindState>) -> Vec<Effect> {
    match find.map(|find| &find.target) {
        Some(FindTarget::Browser { tab_id }) => vec![EffectWork::StopFindInPage { tab_id: tab_id.clone() }.into()],
        Some(FindTarget::Terminal { terminal_id }) => vec![EffectWork::StopFindInTerminal { terminal_id: terminal_id.clone() }.into()],
        _ => Vec::new(),
    }
}

fn find_view_gone(state: &WorkspaceState, target: &FindTarget) -> bool {
    let (owner, dock) = front_dock(state);
    match target {
        FindTarget::Thread { task_id } => task_id != &state.current_id && !dock_side_chats(state, &owner).iter().any(|chat| Some(&chat.id) == task_id.as_ref()),
        FindTarget::Browser { tab_id } => owner_of_browser_tab(state, tab_id).is_none(),
        FindTarget::Terminal { terminal_id } => {
            let shown = state.selected_computer().and_then(|computer| computer.state.as_deref()).unwrap_or(state);
            owner_of_terminal(shown, terminal_id).is_none()
        }
        FindTarget::Review { owner: target } => target != &owner || !dock.panels.iter().any(|panel| panel == DIFF_PANEL),
        FindTarget::Panel { owner: target, panel } => target != &owner || !dock.panels.contains(panel),
    }
}

/// Find belongs to the view it is searching, so it goes when that view does; so do the keys of a gone tab.
pub fn pruned_find(state: &mut WorkspaceState) {
    if let Some(tab) = state.keyboard_tab.clone() {
        if !dock_holds_tab(state, &tab) {
            state.keyboard_tab = None;
        }
    }
    let gone = state.find.as_ref().is_some_and(|find| find_view_gone(state, &find.target));
    if gone {
        state.find = None;
        state.find_results = None;
    }
}
