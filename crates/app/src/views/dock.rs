//! The right dock: its tab strip, the panel launcher, the edge that resizes it, and each panel
//! (RightDock.tsx, DockTabStrip.tsx, DockContent.tsx, dock-registry.tsx).

use std::collections::HashMap;

use act_core::domain::engine::{capabilities_for, engine_label};
use act_core::domain::platform::{browser_tab_title, SimulatorAvailability};
use act_core::domain::run::SubagentStatus;
use act_core::domain::thread::has_unread_attention;
use act_core::domain::thread_role::ThreadRole;
use act_core::events::DiffSummaryResult;
use act_core::state::{diff_for, dock_side_chats, dock_subject, front_dock, WorkspaceState, AUTOMATION_PANEL, DIFF_PANEL, DOCK_PICKER, OVERVIEW_PANEL, SIMULATOR_PANEL, WORKFLOW_PANEL};
use act_core::workspace::location::current_folder;
use act_core::AppCommand;
use act_ui::components::button::{IconButton, IconButtonSize};
use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
use act_ui::overlay::{bounds_cell, bounds_probe, Align, BoundsCell, Placement};
use act_ui::style::{Corners, Raised};
use act_ui::theme::{ActiveTheme, Tokens};
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, relative, AnyElement, App, Context, DragMoveEvent, Empty, FocusHandle, FontWeight, KeyDownEvent, MouseButton, ScrollHandle, ScrollEvent, SharedString, Window};
use serde_json::json;

use super::agents_panel::AgentsState;
use super::common::{engine_icon, icon, ink_icon, with_alpha, Ellipsis};
use super::diff_panel::DiffCache;
use super::root::Root;
use super::sidebar::send;
use super::terminal_panel::{self, Terminals};
use crate::workspace::dispatch;

const MIN_DOCK_WIDTH: f32 = 320.0;
pub const ADD_TAB_MENU: &str = "dock-add";
const TABS_MENU: &str = "dock:tabs";
/// The gap between a head button and the menu it opens, 40px from the button's 34px top.
const MENU_GAP: f32 = 6.0;

/// What a drag on the dock's edge carries.
struct DockEdge;

pub struct DockState {
    pub terminals: Terminals,
    pub diff: DiffCache,
    pub agents: AgentsState,
    pub browser: super::browser_dock::BrowserDock,
    pub simulator: super::simulator_dock::SimulatorDock,
    pub chats: super::side_chat::DockChats,
    pub workflow: super::workflow_panel::WorkflowState,
    pub overview: super::coordination_overview::OverviewState,
    add_probe: BoundsCell,
    tabs_probe: BoundsCell,
    strip: ScrollHandle,
    /// The width the user dragged the dock to; None is the width the layout gives it.
    pub width: Option<f32>,
    /// The workspace's right edge and width at the last layout, which a drag measures against.
    workspace: (f32, f32),
    shown_tab: String,
    /// The width the track was last given.
    track: f32,
    /// Each tab's own keyboard stop; only the selected one is reached by Tab, the arrows walk the rest.
    tab_focus: HashMap<String, FocusHandle>,
    resizer_focus: Option<FocusHandle>,
    /// The thread whose review the dock last held, while it holds one.
    diff_mounted: Option<String>,
    /// The pointer is on the resize strip, which hangs outside the dock's own hover area.
    resizer_hovered: bool,
    /// A tab was closed from the keyboard, so the strip takes the keyboard back once it redraws.
    refocus_tabs: bool,
    bodies: HashMap<String, FocusHandle>,
    /// The picker's launchers, each a keyboard stop the arrows walk.
    picker_focus: Vec<FocusHandle>,
    focus_count: i64,
    reported_keys: Option<Option<String>>,
}

impl Default for DockState {
    fn default() -> Self {
        DockState {
            terminals: Terminals::default(),
            diff: DiffCache::default(),
            agents: AgentsState::default(),
            browser: Default::default(),
            simulator: Default::default(),
            chats: Default::default(),
            workflow: Default::default(),
            overview: Default::default(),
            add_probe: bounds_cell(),
            tabs_probe: bounds_cell(),
            strip: ScrollHandle::new(),
            width: None,
            workspace: (0.0, 0.0),
            shown_tab: String::new(),
            track: 0.0,
            tab_focus: HashMap::new(),
            resizer_focus: None,
            resizer_hovered: false,
            diff_mounted: None,
            refocus_tabs: false,
            bodies: HashMap::new(),
            picker_focus: Vec::new(),
            focus_count: 0,
            reported_keys: None,
        }
    }
}

impl DockState {
    pub fn find_in_terminal(&mut self, terminal_id: &str, query: &str, forward: bool, cx: &mut Context<Root>) {
        if let Some(view) = self.terminals.view(terminal_id) {
            view.update(cx, |view, cx| view.find(query, forward, cx));
        }
    }

    pub fn stop_find_in_terminal(&mut self, terminal_id: &str, cx: &mut Context<Root>) {
        if let Some(view) = self.terminals.view(terminal_id) {
            view.update(cx, |view, cx| view.clear_find(cx));
        }
    }

    /// Hands the keyboard back to `previous` when it lies in a dock tab, or to that tab's composer
    /// when the tab is a chat. False when `previous` is outside the dock.
    pub fn restore_focus(&mut self, previous: &FocusHandle, window: &mut Window, cx: &mut Context<Root>) -> bool {
        let Some(tab) = self.bodies.iter().find(|(_, body)| body.contains(previous, window)).map(|(tab, _)| tab.clone()) else { return false };
        if !self.chats.focus(&tab, window, cx) {
            window.focus(previous, cx);
        }
        true
    }

    /// The width the dock's track takes, remembering where the workspace ends for a drag.
    pub fn track_width(&mut self, viewport: f32, workspace: f32, right: f32) -> f32 {
        self.workspace = (right, workspace);
        self.track = width(viewport, workspace, self.width);
        self.track
    }

    /// Moves the dock's edge from the keyboard, holding the width a drag would.
    fn nudge_width(&mut self, wider: f32) {
        let (_, workspace) = self.workspace;
        self.width = Some((self.track + wider).max(MIN_DOCK_WIDTH).min(workspace - MIN_DOCK_WIDTH));
    }

    /// Follows which thread's review the dock holds, open or closed; another thread's opens anew.
    pub fn hold_review(&mut self, state: &WorkspaceState, cx: &mut App) {
        let (owner, _) = front_dock(state);
        let key = tabs(state, &owner).iter().any(|tab| tab.id == DIFF_PANEL).then(|| dock_subject(state, &owner, DIFF_PANEL).unwrap_or(owner));
        if key.is_some() && key != self.diff_mounted {
            self.diff.remount(cx);
        }
        self.diff_mounted = key;
    }

    /// The dock is off screen: its page leaves the window and its device stops streaming.
    pub fn hidden(&mut self, cx: &mut Context<Root>) {
        self.browser.hide(cx);
        let state = crate::workspace::workspace(cx).read(cx).state.clone();
        self.simulator.hide(&state, cx);
    }

    fn body(&mut self, tab: &str, cx: &mut Context<Root>) -> FocusHandle {
        self.bodies.entry(tab.to_string()).or_insert_with(|| cx.focus_handle()).clone()
    }
}

/// The dragged width, else 40% of the window between 380 and 620, never leaving the conversation
/// narrower than its least.
pub fn width(viewport: f32, workspace: f32, dragged: Option<f32>) -> f32 {
    dragged.unwrap_or_else(|| (0.4 * viewport).clamp(380.0, 620.0)).min(workspace - 480.0).max(0.0)
}

/// One tab in the strip.
pub struct Tab {
    pub id: String,
    pub title: String,
    pub glyph: IconName,
    /// The count, what it counts said in full, and whether it counts work still going.
    pub badge: Option<(usize, String, bool)>,
    pub pinned: bool,
    pub unread: bool,
}

/// An entry in the picker and the add menu: a panel to open, or an action that creates one.
pub struct Launcher {
    pub id: &'static str,
    pub title: String,
    pub description: String,
    pub glyph: IconName,
    pub missing: Option<String>,
    pub command: serde_json::Value,
}

pub fn role_glyph(role: Option<ThreadRole>) -> Option<IconName> {
    match role? {
        ThreadRole::Coordinator => Some(IconName::Waypoints),
        ThreadRole::Implementer => Some(IconName::Hammer),
        ThreadRole::Reviewer => Some(IconName::SearchCheck),
        ThreadRole::Researcher => Some(IconName::FlaskConical),
    }
}

/// Whose thread a coordinator's review or schedule is about, while that tab is open.
fn subject_title(state: &WorkspaceState, owner: &str, panel: &str) -> Option<String> {
    dock_subject(state, owner, panel).and_then(|id| state.thread(&id).map(|thread| thread.title.clone()))
}

pub fn launchers(state: &WorkspaceState) -> Vec<Launcher> {
    let thread = state.current_thread();
    let engine = thread.map(|thread| thread.engine).unwrap_or(state.draft_engine);
    let label = engine_label(engine);
    let panel = |panel: &str| json!({ "type": "view.open-dock-panel", "panel": panel });
    let mut list = Vec::new();
    if capabilities_for(engine).subagents {
        list.push(Launcher { id: "agents", title: "Subagents".into(), description: "View work delegated from this task".into(), glyph: IconName::Bot, missing: None, command: panel("agents") });
    }
    list.push(Launcher { id: DIFF_PANEL, title: "Changes".into(), description: "Review the diff and comment on it".into(), glyph: IconName::FileDiff, missing: None, command: panel(DIFF_PANEL) });
    list.push(Launcher { id: AUTOMATION_PANEL, title: "Automation".into(), description: "Edit the schedule that repeats this task".into(), glyph: IconName::AlarmClock, missing: None, command: panel(AUTOMATION_PANEL) });
    list.push(Launcher { id: "browser", title: "Browser".into(), description: "Browse in one session the whole app shares".into(), glyph: IconName::Globe, missing: None, command: json!({ "type": "browser.new-tab" }) });
    if cfg!(target_os = "macos") {
        let missing = match &state.simulator.availability {
            SimulatorAvailability::Unsupported { reason } => Some(reason.clone()),
            _ => None,
        };
        list.push(Launcher { id: SIMULATOR_PANEL, title: "Simulator".into(), description: format!("Run an iOS app in the background and let {label} drive it"), glyph: IconName::Smartphone, missing, command: json!({ "type": "simulator.open" }) });
    }
    list.push(Launcher {
        id: "terminal",
        title: "Terminal".into(),
        description: format!("Run a shell here and let {label} read what it prints"),
        glyph: IconName::SquareTerminal,
        missing: current_folder(state).is_none().then(|| "Open a project to start a terminal".to_string()),
        command: json!({ "type": "terminal.open" }),
    });
    list.push(Launcher {
        id: "side-chat",
        title: "Side chat".into(),
        description: "Start a focused conversation from this task".into(),
        glyph: IconName::GitFork,
        missing: thread.is_none().then(|| "Open a thread to fork it".to_string()),
        command: json!({ "type": "side-chat.open", "chatId": "new" }),
    });
    list
}

/// Runs a launcher. A side chat gets a fresh id at the moment it opens.
pub fn launch(command: &serde_json::Value, cx: &mut App) {
    let mut command = command.clone();
    if command["type"] == "side-chat.open" {
        command["chatId"] = json!(uuid::Uuid::new_v4().to_string());
    }
    send(command, cx);
}

/// Files in the review not yet ticked off, which the Changes tab counts.
pub fn unreviewed(state: &WorkspaceState, owner: &str) -> usize {
    let diff = diff_for(state, owner);
    match &diff.result {
        Some(DiffSummaryResult::Available { files, .. }) => files.iter().filter(|file| !diff.viewed.contains_key(&file.path)).count(),
        _ => 0,
    }
}

pub fn tabs(state: &WorkspaceState, owner: &str) -> Vec<Tab> {
    let (_, dock) = front_dock(state);
    let mut tabs = Vec::new();
    let working = state.current_id.as_ref().and_then(|id| state.subagents.get(id)).map(|subagents| subagents.iter().filter(|subagent| subagent.status == SubagentStatus::Working).count()).unwrap_or(0);
    let files = unreviewed(state, owner);
    // Panels draw in the registry's order (dock-registry.tsx), whatever order they were opened in.
    const ORDER: [&str; 6] = [OVERVIEW_PANEL, "agents", WORKFLOW_PANEL, DIFF_PANEL, SIMULATOR_PANEL, AUTOMATION_PANEL];
    let mut panels: Vec<&String> = dock.panels.iter().collect();
    panels.sort_by_key(|panel| ORDER.iter().position(|known| known == panel).unwrap_or(ORDER.len()));
    for panel in panels {
        let (title, glyph, badge) = match panel.as_str() {
            "agents" => ("Subagents".to_string(), IconName::Bot, (working > 0).then(|| (working, format!("{working} working"), true))),
            DIFF_PANEL => (
                subject_title(state, owner, DIFF_PANEL).map(|title| format!("Changes · {title}")).unwrap_or_else(|| "Changes".into()),
                IconName::FileDiff,
                (files > 0).then(|| (files, format!("{files} {} unreviewed", if files == 1 { "file" } else { "files" }), false)),
            ),
            AUTOMATION_PANEL => (subject_title(state, owner, AUTOMATION_PANEL).map(|title| format!("Automation · {title}")).unwrap_or_else(|| "Automation".into()), IconName::AlarmClock, None),
            SIMULATOR_PANEL => ("Simulator".to_string(), IconName::Smartphone, None),
            WORKFLOW_PANEL => (act_core::state::workflow_by_id(state, dock.workflow_id.as_deref()).map(|workflow| workflow.name.clone()).unwrap_or_else(|| "Workflow".into()), IconName::Boxes, None),
            OVERVIEW_PANEL => {
                let needs = super::coordination_overview::needs_you(state);
                ("Overview".to_string(), IconName::LayoutGrid, (needs > 0).then(|| (needs, format!("{needs} need{} you", if needs == 1 { "s" } else { "" }), false)))
            }
            other => (other.to_string(), IconName::LayoutGrid, None),
        };
        tabs.push(Tab { id: panel.clone(), title, glyph, badge, pinned: panel == OVERVIEW_PANEL, unread: false });
    }
    for page in &dock.browser_tabs {
        tabs.push(Tab { id: page.id.clone(), title: browser_tab_title(page), glyph: IconName::Globe, badge: None, pinned: false, unread: false });
    }
    for terminal in &dock.terminals {
        tabs.push(Tab { id: terminal.id.clone(), title: terminal.title.clone(), glyph: IconName::SquareTerminal, badge: None, pinned: false, unread: false });
    }
    for chat in dock_side_chats(state, owner) {
        if let Some(thread) = state.thread(&chat.id) {
            tabs.push(Tab { id: chat.id.clone(), title: thread.title.clone(), glyph: IconName::GitFork, badge: None, pinned: false, unread: has_unread_attention(thread) });
        }
    }
    for id in &dock.thread_tabs {
        if let Some(thread) = state.thread(id) {
            tabs.push(Tab { id: id.clone(), title: thread.title.clone(), glyph: role_glyph(thread.role).unwrap_or_else(|| engine_icon(thread.engine)), badge: None, pinned: false, unread: has_unread_attention(thread) });
        }
    }
    tabs
}

pub fn close_command(state: &WorkspaceState, tab: &str) -> serde_json::Value {
    let (_, dock) = front_dock(state);
    if dock.panels.iter().any(|panel| panel == tab) {
        json!({ "type": "view.close-dock-panel", "panel": tab })
    } else if dock.browser_tabs.iter().any(|page| page.id == tab) {
        json!({ "type": "browser.close-tab", "tabId": tab })
    } else if dock.terminals.iter().any(|terminal| terminal.id == tab) {
        json!({ "type": "terminal.close", "terminalId": tab })
    } else if dock.thread_tabs.iter().any(|id| id == tab) {
        json!({ "type": "view.close-thread-tab", "taskId": tab })
    } else {
        json!({ "type": "side-chat.close", "chatId": tab })
    }
}

/// The dot an unread tab carries: info, ringed by its own surface.
pub fn attention_dot(t: &Tokens) -> impl IntoElement {
    div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, "Waiting for you")).flex_none().size(px(14.0)).rounded_full().bg(t.info_surface).flex().items_center().justify_center().child(div().size(px(8.0)).rounded_full().bg(t.info))
}

fn badge_chip(count: usize, working: bool, t: &Tokens) -> gpui::Div {
    let (fill, ink) = if working { (t.info_surface, t.info) } else { (t.chip, t.muted_strong) };
    div()
        .flex_none()
        .h(px(18.0))
        .min_w(px(18.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(fill)
        .text_color(ink)
        .text_size(px(11.0))
        .font_weight(FontWeight(600.0))
        .child(count.to_string())
}

fn menu_button(id: &'static str, glyph: IconName, glyph_size: f32, open: bool, probe: &BoundsCell, t: &Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .relative()
        .flex_none()
        .size(px(34.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(9.0))
        .cursor_pointer()
        .when(open, |button| button.bg(t.surface_hover))
        .text_color(if open { t.ink_dim } else { t.muted })
        .hover(|style| style.bg(t.surface_hover).text_color(t.ink_dim))
        .child(ink_icon(glyph, glyph_size))
        .child(bounds_probe(probe))
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let (owner, dock) = front_dock(state);
    let active = dock.tab.clone();
    let expanded = dock.expanded;
    let sheet = t.page.a > 0.0;
    let tabs = tabs(state, &owner);
    let tab_count = tabs.len();
    // This window's shells live on while a paired computer's thread is on screen.
    let own = root.state(cx);
    let alive: Vec<String> = state.docks.values().chain(own.docks.values()).flat_map(|dock| dock.terminals.iter().map(|terminal| terminal.id.clone())).collect();
    let shown_terminal = (dock.open && !state.settings_shown()).then_some(active.as_str()).filter(|tab| dock.terminals.iter().any(|terminal| terminal.id == *tab));
    root.dock.terminals.retain(&alive, shown_terminal);
    root.dock.agents.sync(state);

    root.dock.hold_review(state, cx);

    if root.dock.shown_tab != active {
        root.dock.shown_tab = active.clone();
        if let Some(index) = tabs.iter().position(|tab| tab.id == active) {
            root.dock.strip.scroll_to_item(index);
        }
    }

    let roving = tabs.iter().find(|tab| tab.id == active).or(tabs.first()).map(|tab| tab.id.clone());
    root.dock.tab_focus.retain(|id, _| tabs.iter().any(|tab| &tab.id == id));
    for tab in &tabs {
        let handle = root.dock.tab_focus.remove(&tab.id).unwrap_or_else(|| cx.focus_handle());
        root.dock.tab_focus.insert(tab.id.clone(), handle.tab_stop(roving.as_ref() == Some(&tab.id)));
    }
    if std::mem::take(&mut root.dock.refocus_tabs) {
        if let Some(handle) = roving.as_ref().and_then(|id| root.dock.tab_focus.get(id)) {
            window.focus(handle, cx);
        }
    }
    let strip_handle = root.dock.strip.clone();
    let wheel_handle = root.dock.strip.clone();
    let mut strip = div()
        .id("dock-tab-strip")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::TabList, "Right panel tabs"))
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .gap(px(4.0))
        .overflow_x_scroll()
        .track_scroll(&strip_handle)
        .on_key_down(cx.listener(|root, event: &KeyDownEvent, window, cx| strip_key(root, event, window, cx)))
        .on_scroll(move |event: &ScrollEvent, window, _| {
            let delta = event.delta.pixel_delta(window.line_height());
            if delta.x != px(0.0) || delta.y == px(0.0) {
                return;
            }
            let mut offset = wheel_handle.offset();
            let max = wheel_handle.max_offset().x;
            offset.x = (offset.x + delta.y).min(px(0.0)).max(-max);
            wheel_handle.set_offset(offset);
        });
    for tab in &tabs {
        let selected = tab.id == active;
        let id = tab.id.clone();
        let close = close_command(state, &tab.id);
        let middle_close = close.clone();
        let pinned = tab.pinned;
        let group: SharedString = format!("dock-tab-{}", tab.id).into();
        let ink = if selected { t.ink } else { t.quiet_strong };
        let focus = root.dock.tab_focus[&tab.id].clone();
        let ring = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
        let mut button = div()
            .id(SharedString::from(format!("dock-tab-{}", tab.id)))
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Tab, tab.title.clone()).selected(selected))
            .group(group.clone())
            .relative()
            .flex_shrink(1.)
            .min_w(px(96.0))
            .w(px(184.0))
            .h(px(40.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .pr(px(4.0))
            .rounded_t(px(8.0))
            .cursor_pointer()
            .text_color(ink)
            .track_focus(&focus)
            .hover(|style| {
                let style = style.bg(with_alpha(t.surface_hover, 0.6));
                if selected { style } else { style.text_color(t.ink_faint) }
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .pl(px(9.0))
                    .pr(px(2.0))
                    .when(ring, |button| button.child(div().absolute().inset_0().rounded_t(px(8.0)).border_2().border_color(t.focus_ring)))
                    // The glyph squeezes with the title in a narrow tab, as the SVG does in a flex row.
                    .child(ink_icon(tab.glyph, 16.0).shrinks())
                    .child(
                        div()
                            .flex_shrink(1.)
                            .min_w_0()
                            .ellipsis()
                            .text_size(px(m.text_control))
                            .when(!selected, |title| title.group_hover(group.clone(), |style| style.text_color(t.ink_faint)))
                            .when(tab.unread, |title| title.text_color(t.ink).font_weight(FontWeight(620.0)))
                            .child(tab.title.clone()),
                    )
                    .when_some(tab.badge.clone(), |row, (count, label, working)| row.child(div().flex_none().ml_auto().child(badge_chip(count, working, t).accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, label)))))
                    .when(tab.unread, |row| row.child(div().flex_none().ml_auto().mr(px(1.0)).child(attention_dot(t)))),
            )
            .when(tab.title.chars().count() > 16, |button| button.tip(tab.title.clone()))
            .on_mouse_down(MouseButton::Middle, move |_, _, cx| {
                if !pinned {
                    send(middle_close.clone(), cx);
                }
            })
            .on_click(move |_, _, cx| dispatch(AppCommand::SelectDockTab { tab: id.clone() }, cx));
        if !tab.pinned {
            button = button.child(
                IconButton::new(SharedString::from(format!("dock-close-{}", tab.id)), IconName::X, format!("Close {}", tab.title))
                    .size(IconButtonSize::Xs)
                    .icon_size(14.0)
                    .skip_tab_stop()
                    .tip(if selected && !expanded { super::chrome::key_tip("Close tab", "Mod+W") } else { "Close tab".into() })
                    // The close sits inside the tab, so its press must not also select the tab it closed.
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        send(close.clone(), cx);
                    }),
            );
        }
        if selected {
            button = button.child(div().absolute().left(px(8.0)).right(px(8.0)).bottom_0().h(px(2.0)).rounded_t(px(2.0)).bg(t.ink));
        }
        strip = strip.child(button);
    }
    let overflowing = root.dock.strip.max_offset().x > px(1.0);
    let scroll_x = -f32::from(root.dock.strip.offset().x);
    let max_x = f32::from(root.dock.strip.max_offset().x);
    // `dock-tabs-fade`: each edge's fade grows to 28px over the first and last 4% of the scroll.
    let progress = if overflowing { (scroll_x / max_x).clamp(0.0, 1.0) } else { 0.0 };
    let fade_start = if overflowing { 28.0 * (progress / 0.04).min(1.0) } else { 0.0 };
    let fade_end = if overflowing { 28.0 * ((1.0 - progress) / 0.04).min(1.0) } else { 0.0 };
    let strip = div()
        .relative()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .child(strip)
        .when(overflowing, |wrap| wrap.edge_fade(gpui::EdgeFade::horizontal().start(px(0.0), px(fade_start)).end(px(0.0), px(fade_end))));

    let tabs_probe = root.dock.tabs_probe.clone();
    let tabs_open = root.menus.is_open(TABS_MENU);
    let all_tabs = overflowing.then(|| {
        let probe = tabs_probe.clone();
        let mut button = menu_button("dock-all-tabs", IconName::ChevronDown, 16.0, tabs_open, &tabs_probe, t);
        if !tabs_open {
            button = button.tip("All tabs");
        }
        let button = button.accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, format!("All tabs ({tab_count})")).expanded(tabs_open));
        button.on_click(cx.listener(move |root, _, window, cx| {
            if root.menus.is_open(TABS_MENU) {
                root.menus.close(cx);
                return;
            }
            let state = root.state(cx);
            let (owner, dock) = front_dock(&state);
            let anchor = probe.get().unwrap_or_default();
            let info = cx.theme().tokens.info;
            let open = tabs_of(&state, &owner);
            let start = open.iter().position(|tab| tab.id == dock.tab).unwrap_or(0);
            let entries: Vec<MenuEntry> = open
                .into_iter()
                .map(|tab| {
                    let id = tab.id.clone();
                    let mut item = MenuItem::new(tab.title.clone()).icon(tab.glyph).icon_size(16.0).checked(tab.id == dock.tab).on_select(move |_, cx| dispatch(AppCommand::SelectDockTab { tab: id.clone() }, cx));
                    if tab.unread {
                        item = item.detail_dot(info);
                    }
                    item.into()
                })
                .collect();
            let menu = Menu::build(entries, Placement::Below { anchor, align: Align::End, gap: px(MENU_GAP) }, window, cx, |menu| menu.named("Open tabs").checkable(true).min_width(px(184.0)).max_width(px(280.0)).highlighted(Some(start)).exempt(&probe));
            root.menus.show(TABS_MENU, menu, cx);
        }))
    });

    let add_probe = root.dock.add_probe.clone();
    let add_open = root.menus.is_open(ADD_TAB_MENU);
    let mut add = menu_button("dock-add", IconName::Plus, 18.0, add_open, &add_probe, t);
    if !add_open {
        add = add.tip("New tab");
    }
    let add = add.accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, "Add right panel tab").expanded(add_open));
    let add = add.on_click(cx.listener(move |root, _, window, cx| {
        if root.menus.is_open(ADD_TAB_MENU) {
            root.menus.close(cx);
            return;
        }
        let state = root.state(cx);
        let probe = add_probe.clone();
        let anchor = probe.get().unwrap_or_default();
        let launchers = launchers(&state);
        let first = launchers.iter().position(|launcher| launcher.missing.is_none());
        let entries: Vec<MenuEntry> = launchers
            .into_iter()
            .map(|launcher| {
                let command = launcher.command.clone();
                let mut item = MenuItem::new(launcher.title).icon(launcher.glyph).icon_size(16.0).disabled(launcher.missing.is_some()).on_select(move |_, cx| launch(&command, cx));
                if let Some(missing) = launcher.missing {
                    item = item.description(missing);
                }
                item.into()
            })
            .collect();
        let menu = Menu::build(entries, Placement::Below { anchor, align: Align::End, gap: px(MENU_GAP) }, window, cx, |menu| menu.width(px(236.0)).highlighted(first).caret_holds().exempt(&probe));
        root.menus.show(ADD_TAB_MENU, menu, cx);
    }));
    let head_height = if sheet { m.chrome_head - m.frame_gap } else { m.chrome_head };
    let head = div()
        .relative()
        .flex_none()
        .h(px(head_height))
        .flex()
        .items_center()
        .gap(px(5.0))
        .px(px(8.0))
        .border_b_1()
        .border_color(t.line)
        .when(expanded && !state.sidebar_open, |head| head.pl(px(m.window_controls_inset.max(8.0))))
        .child(strip)
        .children(all_tabs)
        .child(add)
        .child(
            IconButton::new("dock-expand", if expanded { IconName::Minimize2 } else { IconName::Maximize2 }, if expanded { "Restore right panel" } else { "Expand right panel" })
                .size(IconButtonSize::Lg)
                .icon_size(16.0)
                .pressed(expanded)
                .tip(super::chrome::key_tip(if expanded { "Restore panel" } else { "Expand panel" }, "Mod+Shift+\\"))
                .on_click(move |_, _, cx| dispatch(AppCommand::SetDockExpanded { expanded: !expanded }, cx)),
        )
        .child(IconButton::new("dock-hide", IconName::PanelRightClose, "Hide right panel").size(IconButtonSize::Lg).icon_size(18.0).tip(super::chrome::key_tip("Hide panel", "Mod+\\")).on_click(|_, _, cx| dispatch(AppCommand::SetDockOpen { open: false }, cx)));

    let body_focus = root.dock.body(&active, cx);
    // After `content`, so a tab opened this frame has its shell or composer to take the keys.
    let content = content(root, state, &owner, &active, window, cx);
    if let Some(focus) = state.dock_focus.as_ref().filter(|focus| focus.owner == owner && focus.tab == active) {
        if focus.count != root.dock.focus_count {
            root.dock.focus_count = focus.count;
            focus_tab(root, state, &active, &body_focus, window, cx);
        }
    }
    report_keys(root, state, &active, window, cx);
    // The bar hangs under the panel's own header, so it covers none of it.
    let find_head = if active == DIFF_PANEL {
        root.dock.diff.toolbar_probe.get().map(|bounds| f32::from(bounds.size.height)).unwrap_or(0.0)
    } else if dock.terminals.iter().any(|terminal| terminal.id == active) {
        34.0
    } else if (dock.panels.iter().any(|panel| panel == &active) && active != SIMULATOR_PANEL) || dock.thread_tabs.contains(&active) || dock_side_chats(state, &owner).iter().any(|chat| chat.id == active) {
        m.chrome_head
    } else {
        0.0
    };
    let browser_tab = dock.browser_tabs.iter().any(|page| page.id == active);
    let find = if browser_tab { None } else { root.parts.as_ref().map(|parts| div().absolute().top(px(find_head)).left_0().right_0().child(parts.find(true))) };
    let focus_for_click = body_focus.clone();
    // Each panel is a section named as the web app names it.
    let region = if browser_tab {
        Some("Browser".to_string())
    } else if dock.terminals.iter().any(|terminal| terminal.id == active) {
        Some("Terminal".to_string())
    } else {
        match active.as_str() {
            "agents" => Some("Agents".to_string()),
            DIFF_PANEL => Some("Changes".to_string()),
            AUTOMATION_PANEL => Some("Automation".to_string()),
            SIMULATOR_PANEL => Some("Simulator".to_string()),
            WORKFLOW_PANEL => act_core::state::workflow_by_id(state, dock.workflow_id.as_deref()).map(|workflow| format!("Workflow {}", workflow.name)),
            OVERVIEW_PANEL => Some("Threads under this coordinator".to_string()),
            _ => None,
        }
    };
    let body = div()
        .id("dock-body")
        .when_some(region, |body, name| body.accessible(gpui::AccessInfo::new(gpui::AccessRole::Region, name)))
        .relative()
        .flex_1()
        .min_h_0()
        .track_focus(&body_focus)
        .capture_any_mouse_down(move |_, window, cx| window.focus(&focus_for_click, cx))
        .child(content)
        .children(find);
    let dock = div()
        .id("right-dock")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Complementary, "Right panel"))
        .tab_order(1)
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .when(sheet, |dock| dock.bg(t.sunken).rounded(px(m.page_radius)))
        .child(head)
        .child(body)
        // Glass over the dock frosts the dock and stands in for it, so its text leaves no sharp ghost.
        .when(!sheet && t.glass_blur, |dock| {
            dock.child(gpui::canvas(|_, _, _| {}, move |bounds, _, window, _| window.replace_under_backdrops(bounds)).absolute().top_0().left_0().size_full())
        });

    // The resizer comes before the panel in the Tab order, as it does in the page.
    let mut outer = div().group("right-dock-outer").tab_order(0).relative().size_full().when(sheet, |outer| outer.pt(px(m.frame_gap))).child(dock);
    if !expanded {
        outer = outer.child(resizer(root, sheet, window, cx));
    }
    // The dock's edge lies over the resizer's line, as `.right-dock::before` does.
    outer.when(!sheet, |outer| outer.child(div().absolute().top_0().bottom_0().left_0().w(px(1.0)).bg(t.line_glass)))
}

/// Left and Right walk the tabs and Home and End jump to either end; Enter or Space selects the one
/// with the keyboard and Delete closes it, unless it is pinned.
fn strip_key(root: &mut Root, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Root>) {
    let keys = &event.keystroke;
    if keys.modifiers.platform || keys.modifiers.control || keys.modifiers.alt {
        return;
    }
    let state = root.state(cx);
    let (owner, _) = front_dock(&state);
    let tabs = tabs(&state, &owner);
    let Some(at) = tabs.iter().position(|tab| root.dock.tab_focus.get(&tab.id).is_some_and(|focus| focus.is_focused(window))) else { return };
    let count = tabs.len();
    let next = match keys.key.as_str() {
        "left" => Some((at + count - 1) % count),
        "right" => Some((at + 1) % count),
        "home" => Some(0),
        "end" => Some(count - 1),
        _ => None,
    };
    if let Some(next) = next {
        if let Some(focus) = root.dock.tab_focus.get(&tabs[next].id) {
            window.focus(focus, cx);
        }
        root.dock.strip.scroll_to_item(next);
        cx.stop_propagation();
        cx.notify();
        return;
    }
    let tab = &tabs[at];
    match keys.key.as_str() {
        "enter" | "space" => {
            dispatch(AppCommand::SelectDockTab { tab: tab.id.clone() }, cx);
            cx.stop_propagation();
        }
        "delete" if !keys.modifiers.shift && !tab.pinned => {
            root.dock.refocus_tabs = true;
            send(close_command(&state, &tab.id), cx);
            cx.stop_propagation();
        }
        _ => {}
    }
}

fn tabs_of(state: &WorkspaceState, owner: &str) -> Vec<Tab> {
    tabs(state, owner)
}

/// The 16px grab strip reaching into the gap beside the dock: a line on hover, a grip at rest.
fn resizer(root: &mut Root, sheet: bool, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let seam = if sheet { 11.5 } else { 12.5 };
    let (right, workspace) = root.dock.workspace;
    let focus = root.dock.resizer_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
    let ring = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
    let strip_hovered = root.dock.resizer_hovered;
    div()
        .id("right-dock-resizer")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Splitter, "Resize right panel"))
        .track_focus(&focus)
        .group("dock-resizer")
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(-12.0))
        .w(px(16.0))
        .occlude()
        .cursor_col_resize()
        .on_hover(cx.listener(|root, hovered: &bool, _, cx| {
            if root.dock.resizer_hovered != *hovered {
                root.dock.resizer_hovered = *hovered;
                cx.notify();
            }
        }))
        .child(div().absolute().top_0().bottom_0().left(px(seam - 1.0)).w(px(2.0)).bg(t.line_input).when(!ring, |line| line.invisible().group_hover("dock-resizer", |style| style.visible())))
        .child(div().id("dock-grip").transition(gpui::Transition::ease(150)).absolute().top(relative(0.5)).mt(px(-16.0)).left(px(seam - 2.0)).w(px(4.0)).h(px(32.0)).rounded(px(2.0)).bg(t.line_input).when(!ring && !strip_hovered, |grip| grip.opacity(0.4).group_hover("right-dock-outer", |style| style.opacity(1.0))))
        // Arrows move the edge 10px, 50px with Shift; Home and End take the width to its least and most; Enter resets it.
        .on_key_down(cx.listener(|root, event: &KeyDownEvent, _, cx| {
            let keys = &event.keystroke;
            let step = if keys.modifiers.shift { 50.0 } else { 10.0 };
            match keys.key.as_str() {
                "left" => root.dock.nudge_width(step),
                "right" => root.dock.nudge_width(-step),
                "home" => root.dock.nudge_width(f32::NEG_INFINITY),
                "end" => root.dock.nudge_width(f32::INFINITY),
                "enter" => root.dock.width = None,
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
        }))
        .tip("Drag to resize · Double-click to reset")
        .on_drag(DockEdge, |_, _, _, cx| cx.new(|_| Empty))
        .on_drag_move(cx.listener(move |root, event: &DragMoveEvent<DockEdge>, _, cx| {
            let x = f32::from(event.event.position.x);
            let wanted = (right - x).max(MIN_DOCK_WIDTH).min(workspace - MIN_DOCK_WIDTH);
            root.dock.width = Some(wanted);
            cx.notify();
        }))
        .on_click(cx.listener(|root, event: &gpui::ClickEvent, _, cx| {
            if event.click_count() == 2 {
                root.dock.width = None;
                cx.notify();
            }
        }))
        .into_any_element()
}

/// Hands the keyboard to the tab asked for: a shell, a page or a chat takes it itself, any other
/// panel's body takes it.
fn focus_tab(root: &mut Root, state: &WorkspaceState, active: &str, body: &FocusHandle, window: &mut Window, cx: &mut Context<Root>) {
    let (_, dock) = front_dock(state);
    if let Some(view) = root.dock.terminals.view(active) {
        view.update(cx, |view, cx| view.focus(window, cx));
        return;
    }
    if dock.browser_tabs.iter().any(|page| page.id == active) {
        root.dock.browser.request_focus(window, cx);
        return;
    }
    if root.dock.chats.focus(active, window, cx) {
        return;
    }
    window.focus(body, cx);
}

/// Tells the reducer which tab holds the keyboard as the window's focus moves.
fn report_keys(root: &mut Root, state: &WorkspaceState, active: &str, window: &mut Window, cx: &mut Context<Root>) {
    let dock_open = front_dock(state).1.open && !state.zen;
    let holding = dock_open && root.dock.bodies.get(active).is_some_and(|focus| focus.contains_focused(window, cx));
    let tab = holding.then(|| active.to_string());
    if root.dock.reported_keys.as_ref() == Some(&tab) && state.keyboard_tab == tab {
        return;
    }
    root.dock.reported_keys = Some(tab.clone());
    if state.keyboard_tab != tab {
        cx.defer(move |cx| dispatch(AppCommand::DockKeys { tab }, cx));
    }
}

fn content(root: &mut Root, state: &WorkspaceState, owner: &str, active: &str, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let (_, dock) = front_dock(state);
    let browser = dock.browser_tabs.iter().find(|page| page.id == active).cloned();
    if state.settings_shown() {
        root.dock.browser.hide(cx);
    } else {
        let covered = root.menus.any_open() || root.library.is_some() || state.jump.is_some() || state.project_add.is_some() || state.fork_name.is_some() || state.archive_prompt.is_some() || state.worktree_settings.confirming.is_some();
        root.dock.browser.sync(state, browser.as_ref(), covered, window, cx);
    }
    let simulating = active == SIMULATOR_PANEL && dock.open && !state.zen && !state.settings_shown();
    if simulating {
        root.dock.simulator.sync(state, true, window, cx);
    } else {
        root.dock.simulator.hide(state, cx);
    }
    if let Some(terminal) = dock.terminals.iter().find(|terminal| terminal.id == active) {
        return terminal_panel::render(root, state, terminal, window, cx);
    }
    if let Some(page) = browser {
        return root.dock.browser.render(&page, cx);
    }
    if active == DIFF_PANEL {
        return super::diff_panel::render(root, state, owner, window, cx);
    }
    if active == "agents" {
        return super::agents_panel::render(root, state, window, cx);
    }
    if active == AUTOMATION_PANEL {
        return super::automation_panel::render(root, state, owner, window, cx);
    }
    if active == SIMULATOR_PANEL {
        return root.dock.simulator.render(cx);
    }
    if active == WORKFLOW_PANEL {
        return super::workflow_panel::render(root, state, dock.workflow_id.as_deref(), window, cx);
    }
    if active == OVERVIEW_PANEL {
        return super::coordination_overview::render(root, state, window, cx);
    }
    if dock_side_chats(state, owner).iter().any(|chat| chat.id == active) || dock.thread_tabs.iter().any(|id| id == active) {
        return super::side_chat::render(root, state, active, window, cx);
    }
    picker(root, state, active, window, cx)
}

/// "Choose a panel": every launcher, each a button.
fn picker(root: &mut Root, state: &WorkspaceState, active: &str, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    if active != DOCK_PICKER {
        return div().size_full().into_any_element();
    }
    let launchers = launchers(state);
    while root.dock.picker_focus.len() < launchers.len() {
        root.dock.picker_focus.push(cx.focus_handle().tab_stop(true));
    }
    root.dock.picker_focus.truncate(launchers.len());
    let handles = root.dock.picker_focus.clone();
    let walk = handles.clone();
    // Up and Down walk the launchers, wrapping round.
    let mut list = div().w(px(360.0)).max_w_full().flex().flex_col().gap(px(10.0)).on_key_down(move |event: &KeyDownEvent, window, cx| {
        let down = match event.keystroke.key.as_str() {
            "down" => true,
            "up" => false,
            _ => return,
        };
        let count = walk.len();
        if count == 0 {
            return;
        }
        let at = walk.iter().position(|handle| handle.is_focused(window));
        let next = match at {
            None => if down { 0 } else { count - 1 },
            Some(at) => (at + if down { 1 } else { count - 1 }) % count,
        };
        window.focus(&walk[next], cx);
        cx.stop_propagation();
    });
    let ring_visible = act_ui::focus::keyboard_focus_visible(cx);
    // The column is as wide as its widest line unwrapped, as a grid track sized to its content; the
    // list keeps to its left edge and the heading centres across it. The subtitle's 330px box keeps
    // to the left, its lines centred within it.
    let measure = super::text_fit::line_width;
    let widest = launchers.iter().map(|launcher| measure(&launcher.title, m.text_control, window).max(measure(launcher.missing.as_deref().unwrap_or(&launcher.description), m.text_meta, window))).fold(0.0, f32::max);
    let subtitle = "Inspect delegated work or start a focused conversation.";
    let column = (widest + 78.0).max(measure("Choose a panel", m.text_heading, window)).max(measure(subtitle, m.text_control, window).min(330.0));
    for (index, launcher) in launchers.into_iter().enumerate() {
        let disabled = launcher.missing.is_some();
        let command = launcher.command.clone();
        let focus = handles[index].clone();
        let ring = ring_visible && focus.is_focused(window);
        list = list.child(
            div()
                .id(SharedString::from(format!("launch-{}", launcher.id)))
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Open {} panel", launcher.title)).disabled(disabled))
                .track_focus(&focus)
                .relative()
                .when(ring, |button| button.child(div().absolute().inset_0().rounded(px(12.0)).border_2().border_color(t.focus_ring)))
                .flex()
                .items_center()
                .gap(px(12.0))
                .p(px(13.0))
                .rounded(px(12.0))
                .border_1()
                .map(|button| if disabled { button.border_dashed().border_color(t.line_strong) } else { button.border_color(t.line).bg(t.control_soft).cursor_pointer().hover(|style| style.border_color(t.info_line).bg(t.surface_hover)) })
                .rim(t.glass_rim, Corners::Radius(12.0))
                .child(div().w(px(38.0)).flex_none().flex().justify_center().child(icon(launcher.glyph, 18.0, if disabled { t.quiet } else { t.link })))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(div().text_color(if disabled { t.muted } else { t.ink_soft }).text_size(px(m.text_control)).font_weight(FontWeight(580.0)).child(launcher.title))
                        .child(div().text_color(t.quiet_strong).text_size(px(m.text_meta)).line_height(relative(1.35)).child(launcher.missing.unwrap_or(launcher.description))),
                )
                .on_click(move |_, _, cx| {
                    if !disabled {
                        launch(&command, cx);
                    }
                }),
        );
    }
    div()
        .id("dock-picker")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, "Choose a right panel"))
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .p(px(32.0))
        .child(
            div()
                .w(px(column))
                .max_w_full()
                .flex()
                .flex_col()
                .gap(px(24.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(div().text_color(t.ink_soft).text_size(px(m.text_heading)).font_weight(FontWeight(580.0)).child("Choose a panel"))
                        .child(div().w_full().flex().child(div().max_w(px(330.0)).mt(px(7.0)).text_center().text_color(t.quiet).text_size(px(m.text_control)).line_height(relative(1.45)).child(subtitle))),
                )
                .child(list),
        )
        .into_any_element()
}
