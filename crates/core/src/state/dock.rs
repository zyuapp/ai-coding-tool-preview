//! The right dock as data, one per thread, and each thread's review of its own checkout.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{SideChat, WorkspaceState};
use crate::domain::diff::{default_branch_range, file_fingerprint, folded_for_size, range_key, DiffFileSummary, DiffMode, DiffRange};
use crate::domain::platform::BrowserTab;
use crate::domain::terminal::TerminalSession;
use crate::events::DiffSummaryResult;
use crate::util::Map;

/// One thread's right dock: whether it is showing, which panels are open as tabs, which tab is on
/// top, and the pages and shells that thread opened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDock {
    pub open: bool,
    pub expanded: bool,
    pub panels: Vec<String>,
    pub tab: String,
    pub workflow_id: Option<String>,
    pub browser_tabs: Vec<BrowserTab>,
    pub browser_tab_id: Option<String>,
    pub terminals: Vec<TerminalSession>,
    pub terminal_id: Option<String>,
    pub simulator_id: Option<String>,
    pub thread_tabs: Vec<String>,
    pub subjects: Map<String>,
}

impl Default for ThreadDock {
    fn default() -> Self {
        ThreadDock {
            open: false,
            expanded: false,
            panels: Vec::new(),
            tab: DOCK_PICKER.into(),
            workflow_id: None,
            browser_tabs: Vec::new(),
            browser_tab_id: None,
            terminals: Vec::new(),
            terminal_id: None,
            simulator_id: None,
            thread_tabs: Vec::new(),
            subjects: Map::new(),
        }
    }
}

/// The dock tab that offers the panels, shown whenever no panel is on top.
pub const DOCK_PICKER: &str = "home";
/// The dock the app shows while no thread is current.
pub const DRAFT_DOCK: &str = "draft";
pub const DIFF_PANEL: &str = "diff";
pub const AUTOMATION_PANEL: &str = "automation";
pub const WORKFLOW_PANEL: &str = "workflow";
pub const SIMULATOR_PANEL: &str = "simulator";
pub const OVERVIEW_PANEL: &str = "overview";

static EMPTY_DOCK: std::sync::OnceLock<ThreadDock> = std::sync::OnceLock::new();

pub fn empty_dock() -> &'static ThreadDock {
    EMPTY_DOCK.get_or_init(ThreadDock::default)
}

/// Whose dock a command belongs in: the thread it names, else the one on screen. A side chat's dock is its source's.
pub fn dock_owner(state: &WorkspaceState, thread_id: Option<&str>) -> String {
    let Some(id) = thread_id.or(state.current_id.as_deref()).filter(|id| !id.is_empty()) else { return DRAFT_DOCK.into() };
    state.side_chats.iter().find(|chat| chat.id == id).map(|chat| chat.source_thread_id.clone()).unwrap_or_else(|| id.to_string())
}

pub fn dock_for<'a>(state: &'a WorkspaceState, owner: &str) -> &'a ThreadDock {
    state.docks.get(owner).unwrap_or_else(|| empty_dock())
}

/// The dock for `owner`, made when it has none yet.
pub fn dock_mut<'a>(state: &'a mut WorkspaceState, owner: &str) -> &'a mut ThreadDock {
    state.docks.entry(owner.to_string()).or_default()
}

/// The thread tab a dock's panel is about, while that tab is still open.
pub fn dock_subject(state: &WorkspaceState, owner: &str, panel: &str) -> Option<String> {
    let dock = dock_for(state, owner);
    dock.subjects.get(panel).filter(|subject| dock.thread_tabs.contains(subject)).cloned()
}

/// The dock in front, and whose it is.
pub fn front_dock(state: &WorkspaceState) -> (String, &ThreadDock) {
    let owner = dock_owner(state, None);
    let dock = dock_for(state, &owner);
    (owner, dock)
}

/// The thread tab the dock in front is showing, while it shows one.
pub fn shown_thread_tab(state: &WorkspaceState) -> Option<String> {
    let (_, dock) = front_dock(state);
    (dock.open && dock.thread_tabs.contains(&dock.tab)).then(|| dock.tab.clone())
}

pub fn owner_of_browser_tab(state: &WorkspaceState, tab_id: &str) -> Option<String> {
    state.docks.iter().find(|(_, dock)| dock.browser_tabs.iter().any(|tab| tab.id == tab_id)).map(|(owner, _)| owner.clone())
}

pub fn owner_of_terminal(state: &WorkspaceState, terminal_id: &str) -> Option<String> {
    state.docks.iter().find(|(_, dock)| dock.terminals.iter().any(|terminal| terminal.id == terminal_id)).map(|(owner, _)| owner.clone())
}

/// The forks a dock draws as tabs: the ones taken from the thread that owns it.
pub fn dock_side_chats<'a>(state: &'a WorkspaceState, owner: &str) -> Vec<&'a SideChat> {
    state.side_chats.iter().filter(|chat| chat.source_thread_id == owner).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockTabKind {
    Browser,
    Terminal,
    SideChat,
    Thread,
    Panel,
    Picker,
}

/// What a dock tab is showing.
pub fn dock_tab_kind(state: &WorkspaceState, owner: &str, tab: &str) -> DockTabKind {
    let dock = dock_for(state, owner);
    if dock.browser_tabs.iter().any(|page| page.id == tab) {
        return DockTabKind::Browser;
    }
    if dock.terminals.iter().any(|terminal| terminal.id == tab) {
        return DockTabKind::Terminal;
    }
    if dock_side_chats(state, owner).iter().any(|chat| chat.id == tab) {
        return DockTabKind::SideChat;
    }
    if dock.thread_tabs.iter().any(|id| id == tab) {
        return DockTabKind::Thread;
    }
    if dock.panels.iter().any(|panel| panel == tab) {
        DockTabKind::Panel
    } else {
        DockTabKind::Picker
    }
}

/// Every tab in the dock, in the order the strip draws them.
pub fn dock_tab_ids(state: &WorkspaceState, owner: &str) -> Vec<String> {
    let dock = dock_for(state, owner);
    let mut tabs: Vec<String> = dock.panels.clone();
    tabs.extend(dock.browser_tabs.iter().map(|page| page.id.clone()));
    tabs.extend(dock.terminals.iter().map(|terminal| terminal.id.clone()));
    tabs.extend(dock_side_chats(state, owner).iter().map(|chat| chat.id.clone()));
    tabs.extend(dock.thread_tabs.iter().cloned());
    tabs
}

/// A dock tab holds the keyboard only while the dock in front is open and still has that tab.
pub fn dock_holds_tab(state: &WorkspaceState, tab: &str) -> bool {
    let (owner, dock) = front_dock(state);
    dock.open && dock_tab_ids(state, &owner).iter().any(|id| id == tab)
}

/// Which dock holds a thread as a tab of its own: a side chat's source, or a coordinator that has it open.
pub fn tab_holder_of(state: &WorkspaceState, task_id: &str) -> Option<String> {
    if let Some(chat) = state.side_chats.iter().find(|chat| chat.id == task_id) {
        return Some(chat.source_thread_id.clone());
    }
    state.docks.iter().find(|(_, dock)| dock.thread_tabs.iter().any(|tab| tab == task_id)).map(|(owner, _)| owner.clone())
}

/// The thread whose tab holds the keyboard, when the tab holding it is a conversation.
pub fn keyboard_thread_id(state: &WorkspaceState) -> Option<String> {
    let tab = state.keyboard_tab.as_deref()?;
    let kind = dock_tab_kind(state, &dock_owner(state, None), tab);
    matches!(kind, DockTabKind::SideChat | DockTabKind::Thread).then(|| tab.to_string())
}

/// The shell holding the keyboard, when the tab holding it is one.
pub fn keyboard_terminal_id(state: &WorkspaceState) -> Option<String> {
    let tab = state.keyboard_tab.as_deref()?;
    (dock_tab_kind(state, &dock_owner(state, None), tab) == DockTabKind::Terminal).then(|| tab.to_string())
}

/// Which tab takes over when `tab` closes: its neighbour on the left, else on the right, else the picker.
pub fn dock_tab_after_closing(state: &WorkspaceState, owner: &str, tab: &str) -> String {
    let tabs = dock_tab_ids(state, owner);
    let Some(index) = tabs.iter().position(|id| id == tab) else { return dock_for(state, owner).tab.clone() };
    let remaining: Vec<&String> = tabs.iter().filter(|id| *id != tab).collect();
    index.checked_sub(1).and_then(|left| remaining.get(left)).or(remaining.get(index)).map(|id| (*id).clone()).unwrap_or_else(|| DOCK_PICKER.into())
}

pub fn active_browser_tab(dock: &ThreadDock) -> Option<&BrowserTab> {
    dock.browser_tabs.iter().find(|tab| Some(&tab.id) == dock.browser_tab_id.as_ref())
}

/// Which tab a browser command acts on: the one it names, else the one that dock is showing.
pub fn browser_target<'a>(dock: &'a ThreadDock, tab_id: Option<&str>) -> Option<&'a BrowserTab> {
    match tab_id {
        None => active_browser_tab(dock),
        Some(id) => dock.browser_tabs.iter().find(|tab| tab.id == id),
    }
}

pub fn active_terminal(dock: &ThreadDock) -> Option<&TerminalSession> {
    dock.terminals.iter().find(|terminal| Some(&terminal.id) == dock.terminal_id.as_ref())
}

/// Which terminal a read acts on: the one it names, else the asking thread's own, else the one shown.
pub fn terminal_target<'a>(dock: &'a ThreadDock, terminal_id: Option<&str>, thread_id: Option<&str>) -> Option<&'a TerminalSession> {
    if let Some(id) = terminal_id {
        return dock.terminals.iter().find(|terminal| terminal.id == id);
    }
    let own = thread_id.and_then(|thread| dock.terminals.iter().rev().find(|terminal| terminal.task_id.as_deref() == Some(thread)));
    own.or_else(|| active_terminal(dock))
}

/// One thread's review of its own checkout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffState {
    pub mode: DiffStateMode,
    pub range: DiffRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_range: Option<DiffRange>,
    pub workspace_id: Option<String>,
    pub result: Option<DiffSummaryResult>,
    pub loading: bool,
    pub collapsed: Vec<String>,
    pub viewed: Map<String>,
    pub split: bool,
    pub ignore_whitespace: bool,
}

/// A commit link opens a concrete comparison outside the selectable review modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffStateMode {
    Uncommitted,
    Branch,
    Commit,
}

impl From<DiffMode> for DiffStateMode {
    fn from(mode: DiffMode) -> Self {
        match mode {
            DiffMode::Uncommitted => DiffStateMode::Uncommitted,
            DiffMode::Branch => DiffStateMode::Branch,
        }
    }
}

pub fn mode_for_range(range: &DiffRange) -> DiffStateMode {
    match range {
        DiffRange::Branches { .. } => DiffStateMode::Branch,
        DiffRange::Commit { .. } => DiffStateMode::Commit,
        DiffRange::Uncommitted => DiffStateMode::Uncommitted,
    }
}

pub const DIFF_MODE_MENU: &str = "diff:mode";

impl Default for DiffState {
    fn default() -> Self {
        DiffState {
            mode: DiffStateMode::Branch,
            range: default_branch_range(),
            branch_range: None,
            workspace_id: None,
            result: None,
            loading: false,
            collapsed: Vec::new(),
            viewed: Map::new(),
            split: true,
            ignore_whitespace: true,
        }
    }
}

static EMPTY_DIFF: std::sync::OnceLock<DiffState> = std::sync::OnceLock::new();

pub fn diff_for<'a>(state: &'a WorkspaceState, owner: &str) -> &'a DiffState {
    state.diffs.get(owner).unwrap_or_else(|| EMPTY_DIFF.get_or_init(DiffState::default))
}

pub fn diff_mut<'a>(state: &'a mut WorkspaceState, owner: &str) -> &'a mut DiffState {
    state.diffs.entry(owner.to_string()).or_default()
}

/// Whether a fresh list counts the same files a different way, which the whitespace toggle does.
pub fn recounted(previous: Option<&DiffSummaryResult>, result: &DiffSummaryResult) -> bool {
    match (previous, result) {
        (Some(DiffSummaryResult::Available { ignore_whitespace: before, .. }), DiffSummaryResult::Available { ignore_whitespace: after, .. }) => before != after,
        _ => false,
    }
}

/// The ticks that survive a fresh list: a file whose counts moved comes back unread, except on a recount.
pub fn retained_views(viewed: &Map<String>, result: &DiffSummaryResult, previous: Option<&DiffSummaryResult>) -> Map<String> {
    let DiffSummaryResult::Available { files, .. } = result else { return viewed.clone() };
    let fingerprints: HashMap<&str, String> = files.iter().map(|file| (file.path.as_str(), file_fingerprint(file))).collect();
    let before: Option<HashMap<&str, String>> = match previous {
        Some(DiffSummaryResult::Available { files: old, .. }) if recounted(previous, result) => Some(old.iter().map(|file| (file.path.as_str(), file_fingerprint(file))).collect()),
        _ => None,
    };
    viewed
        .iter()
        .filter(|(path, mark)| fingerprints.get(path.as_str()).is_some_and(|now| now == *mark || before.as_ref().and_then(|before| before.get(path.as_str())) == Some(mark)))
        .map(|(path, _)| (path.clone(), fingerprints[path.as_str()].clone()))
        .collect()
}

/// The folds a fresh list lands with: decided files keep their fold; new or changed files fold when too large.
pub fn folded_on_load(diff: &DiffState, files: &[DiffFileSummary], result: &DiffSummaryResult) -> Vec<String> {
    let before: HashMap<&str, String> = match &diff.result {
        Some(DiffSummaryResult::Available { files, .. }) => files.iter().map(|file| (file.path.as_str(), file_fingerprint(file))).collect(),
        _ => HashMap::new(),
    };
    let counted = recounted(diff.result.as_ref(), result);
    let oversized = folded_for_size(files);
    files
        .iter()
        .filter(|file| {
            let known = if counted { before.contains_key(file.path.as_str()) } else { before.get(file.path.as_str()) == Some(&file_fingerprint(file)) };
            if known {
                diff.collapsed.contains(&file.path)
            } else {
                oversized.contains(&file.path)
            }
        })
        .map(|file| file.path.clone())
        .collect()
}

/// Whether a landed list still answers what its dock is asking.
pub fn diff_matches(diff: &DiffState, workspace_id: &str, range: &DiffRange) -> bool {
    diff.workspace_id.as_deref() == Some(workspace_id) && diff.mode == mode_for_range(range) && range_key(&diff.range) == range_key(range)
}
