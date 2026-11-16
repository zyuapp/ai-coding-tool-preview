//! The find bar as it is drawn, and what it searches.

use crate::domain::find::{find_hits, FindHit, FindTarget};
use crate::domain::shortcuts::ShortcutSurface;
use crate::state::{dock_tab_kind, front_dock, DockTabKind, FindState, WorkspaceState, DIFF_PANEL};

#[derive(Clone, Debug, PartialEq)]
pub struct FindView {
    pub find: FindState,
    pub matches: i64,
    pub counting: bool,
    pub hit: Option<FindHit>,
}

/// The find bar as it is drawn. A thread is counted here; every other view counts its own matches.
pub fn find_view(state: &WorkspaceState) -> Option<FindView> {
    let mut find = state.find.clone()?;
    if let FindTarget::Thread { task_id } = &find.target {
        let thread = task_id.as_deref().and_then(|id| state.thread(id));
        let hits = find_hits(thread.map(|thread| thread.messages.as_slice()).unwrap_or(&[]), &find.query);
        let index = if hits.is_empty() { 0 } else { find.index.min(hits.len() as i64 - 1) };
        find.index = index;
        return Some(FindView { matches: hits.len() as i64, counting: false, hit: hits.get(index as usize).cloned(), find });
    }
    let reported = state.find_results.as_ref();
    let matches = reported.map(|results| results.matches).unwrap_or(0);
    if find.target.searches_itself() {
        find.index = reported.and_then(|results| results.index).unwrap_or(0);
        return Some(FindView { find, matches, counting: false, hit: None });
    }
    let counting = match reported {
        Some(results) => results.counting.unwrap_or(false),
        None => !find.query.trim().is_empty(),
    };
    find.index = if matches > 0 { find.index.min(matches - 1) } else { 0 };
    Some(FindView { find, matches, counting, hit: None })
}

/// What Mod+F searches: the page the keystroke came from, else the dock view holding the keys, else the thread.
pub fn find_target_for(state: &WorkspaceState, surface: ShortcutSurface) -> FindTarget {
    if let Some(remote) = state.selected_computer().and_then(|computer| computer.state.as_deref()) {
        let mut shown = remote.clone();
        shown.computers = Default::default();
        return find_target_for(&shown, surface);
    }
    let (owner, dock) = front_dock(state);
    if surface == ShortcutSurface::Browser {
        if let Some(page) = dock.browser_tabs.iter().find(|tab| tab.id == dock.tab) {
            return FindTarget::Browser { tab_id: page.id.clone() };
        }
    }
    let thread = FindTarget::Thread { task_id: state.current_id.clone() };
    let Some(tab) = state.keyboard_tab.clone() else { return thread };
    match dock_tab_kind(state, &owner, &tab) {
        DockTabKind::Browser => FindTarget::Browser { tab_id: tab },
        DockTabKind::Terminal => FindTarget::Terminal { terminal_id: tab },
        DockTabKind::SideChat | DockTabKind::Thread => FindTarget::Thread { task_id: Some(tab) },
        DockTabKind::Panel if tab == DIFF_PANEL => FindTarget::Review { owner },
        DockTabKind::Panel => FindTarget::Panel { owner, panel: tab },
        DockTabKind::Picker => thread,
    }
}
