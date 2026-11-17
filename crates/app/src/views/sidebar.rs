//! The sidebar: the window strip with history arrows, New thread and search, the project filter,
//! the activity lists, and Settings (ProjectSidebar.tsx, SidebarChrome.tsx, SidebarActivity.tsx,
//! SidebarFooter.tsx).

use std::collections::{HashMap, HashSet};
use std::cell::Cell;
use std::rc::Rc;

use act_core::domain::project::project_name;
use act_core::domain::sidebar::SidebarSection;
use act_core::domain::thread::Thread;
use act_core::state::WorkspaceState;
use act_core::view::sidebar::sidebar_lists;
use act_core::view::workspace::remote_collections;
use act_core::workspace::activity::{thread_activity, thread_lists};
use act_core::workspace::coordination::can_dismiss;
use act_core::AppCommand;
use act_ui::components::button::IconButton;
use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
use act_ui::overlay::{bounds_cell, bounds_probe, Align, BoundsCell, Placement};
use act_ui::theme::ActiveTheme;
use act_ui::style::{Backdrop, PaneFill};
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, relative, Context, FontWeight, Pixels, ScrollHandle, SharedString, Window};

use super::chrome::{drag_layer, key_tip, no_drag};
use super::common::{count_chip, icon, ink_icon, rimmed, Ellipsis};
use super::letter_spacing::LetterSpacing;
use act_core::domain::platform::{AppUpdate, ComputerStatus};
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::style::Raised;
use super::root::Root;
use super::sidebar_row::{row, RowContext};
use crate::workspace::dispatch;

pub const SIDEBAR_WIDTH: f32 = 280.0;
const MIN_SIDEBAR_WIDTH: f32 = 270.0;
/// How far one arrow press moves the edge, and one with Shift held.
const KEY_STEP: f32 = 10.0;
const SHIFT_KEY_STEP: f32 = 50.0;

/// What a drag on the sidebar's edge carries.
#[derive(Clone)]
pub struct SidebarResize;

struct ResizePreview;

impl Render for ResizePreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Whether the window is too narrow to keep the sidebar beside the conversation (responsive.css),
/// sooner while the right panel takes room too.
pub fn floats(state: &WorkspaceState, window: &Window) -> bool {
    let viewport = f32::from(window.viewport_size().width);
    let (_, dock) = act_core::state::front_dock(state);
    viewport <= 900.0 || (viewport <= 1080.0 && dock.open && !dock.expanded && !state.zen)
}

/// The sidebar's width: the one dragged to, else the stylesheet's, never under its minimum or over
/// half the window.
pub fn width(sidebar: &SidebarState, window: &Window) -> f32 {
    let viewport = f32::from(window.viewport_size().width);
    let preferred = sidebar.width.unwrap_or(if viewport <= 1100.0 { 240.0 } else { SIDEBAR_WIDTH });
    preferred.min(viewport / 2.0).max(MIN_SIDEBAR_WIDTH)
}

/// The floating sidebar's width: as wide as it rests, inside the window.
fn floating_width(window: &Window) -> f32 {
    let viewport = f32::from(window.viewport_size().width);
    SIDEBAR_WIDTH.min(viewport - 40.0).max(MIN_SIDEBAR_WIDTH)
}

/// The room the sidebar takes beside the workspace: none while hidden or floating.
pub fn docked_width(root: &Root, state: &WorkspaceState, window: &Window) -> f32 {
    if state.sidebar_open && !state.zen && !floats(state, window) {
        width(&root.sidebar, window)
    } else {
        0.0
    }
}

impl Root {
    /// A sidebar that starts to float folds away rather than veiling the page, and comes back once
    /// there is room beside the conversation again. One opened while it floats is the person's to close.
    pub fn follow_floating_sidebar(&mut self, state: &WorkspaceState, window: &Window, cx: &mut Context<Root>) {
        let now = floats(state, window);
        let previous = self.sidebar.floating.replace(now);
        let Some(previous) = previous else { return };
        if previous == now {
            return;
        }
        let open = state.sidebar_open && !state.zen;
        if now && open {
            self.sidebar.folded = true;
            cx.defer(|cx| dispatch(AppCommand::SetSidebarOpen { open: false }, cx));
        } else if !now && self.sidebar.folded {
            self.sidebar.folded = false;
            if !open {
                cx.defer(|cx| dispatch(AppCommand::SetSidebarOpen { open: true }, cx));
            }
        }
    }

    /// Follows a drag on the sidebar's edge.
    pub fn resize_sidebar(&mut self, event: &gpui::DragMoveEvent<SidebarResize>, window: &mut Window, cx: &mut Context<Root>) {
        let viewport = f32::from(window.viewport_size().width);
        let next = f32::from(event.event.position.x).round().clamp(MIN_SIDEBAR_WIDTH, (viewport / 2.0).max(MIN_SIDEBAR_WIDTH));
        if self.sidebar.width != Some(next) {
            self.sidebar.width = Some(next);
            cx.notify();
        }
    }
}

/// What the sidebar draws besides its thread lists, derived from the state.
pub struct SidebarFacts {
    pub projects: Vec<act_core::domain::project::Project>,
    pub activity_project: Option<String>,
    pub busy: HashSet<String>,
    pub monitoring: HashSet<String>,
    pub blocked: HashSet<String>,
    pub waiting: HashMap<String, String>,
    pub worktree_ids: HashSet<String>,
    pub side_chat_attention: HashSet<String>,
    /// The paired computer each of its threads lives on.
    pub thread_hosts: HashMap<String, act_core::view::sidebar::ThreadHost>,
    pub remote_worktrees: Vec<act_core::domain::worktree::Worktree>,
}

/// What the sidebar draws, with its threads borrowed from the state.
pub struct SidebarModel<'a> {
    pub sections: [(SidebarSection, &'static str, Vec<&'a Thread>); 4],
    pub by_coordinator: HashMap<String, Vec<&'a Thread>>,
    facts: &'a SidebarFacts,
}

impl std::ops::Deref for SidebarModel<'_> {
    type Target = SidebarFacts;

    fn deref(&self) -> &SidebarFacts {
        self.facts
    }
}

/// Where a thread is held: this computer's list, or a paired computer's.
#[derive(Clone, Copy)]
enum ThreadAt {
    Own(usize),
    Paired(usize, usize),
}

/// Where each thread of the state sits, by address, so the model can be kept as places.
fn places_of(state: &WorkspaceState) -> HashMap<*const Thread, ThreadAt> {
    let mut places: HashMap<*const Thread, ThreadAt> = state.threads.iter().enumerate().map(|(index, thread)| (&**thread as *const Thread, ThreadAt::Own(index))).collect();
    for (computer, paired) in state.computers.paired.iter().enumerate() {
        for (index, thread) in paired.state.iter().flat_map(|held| held.threads.iter()).enumerate() {
            places.entry(&**thread as *const Thread).or_insert(ThreadAt::Paired(computer, index));
        }
    }
    places
}

fn place(places: &HashMap<*const Thread, ThreadAt>, thread: &Thread) -> ThreadAt {
    places.get(&(thread as *const Thread)).copied().unwrap_or(ThreadAt::Own(usize::MAX))
}

fn resolve(state: &WorkspaceState, at: ThreadAt) -> Option<&Thread> {
    match at {
        ThreadAt::Own(index) => state.threads.get(index).map(|thread| &**thread),
        ThreadAt::Paired(computer, index) => state.computers.paired.get(computer)?.state.as_deref()?.threads.get(index).map(|thread| &**thread),
    }
}

/// The sidebar's model and row context for one state, kept until the state changes.
pub struct SidebarCache {
    state: std::rc::Weak<WorkspaceState>,
    /// The workspace's outline revision it was made at, when made from the workspace's own state.
    outline: Option<u64>,
    sections: [(SidebarSection, &'static str, Vec<ThreadAt>); 4],
    by_coordinator: Vec<(String, Vec<ThreadAt>)>,
    facts: SidebarFacts,
    pub rows: RowContext,
}

impl SidebarCache {
    fn new(held: &Rc<WorkspaceState>, outline: Option<u64>) -> SidebarCache {
        let state: &WorkspaceState = held;
        let model = model(state);
        let rows = RowContext::new(state, &model.facts, &model.by_coordinator);
        let at = places_of(state);
        let places = |threads: &[&Thread]| threads.iter().map(|thread| place(&at, thread)).collect::<Vec<_>>();
        let [a, b, c, d] = &model.sections;
        SidebarCache {
            state: Rc::downgrade(held),
            outline,
            sections: [(a.0, a.1, places(&a.2)), (b.0, b.1, places(&b.2)), (c.0, c.1, places(&c.2)), (d.0, d.1, places(&d.2))],
            by_coordinator: model.by_coordinator.iter().map(|(id, threads)| (id.clone(), places(threads))).collect(),
            facts: model.facts,
            rows,
        }
    }

    fn holds(&self, state: &WorkspaceState) -> bool {
        std::ptr::eq(self.state.as_ptr(), state)
    }

    /// The model over `state`, which must be the state the cache was made from.
    pub fn model<'a>(&'a self, state: &'a WorkspaceState) -> SidebarModel<'a> {
        let threads = |places: &[ThreadAt]| places.iter().filter_map(|at| resolve(state, *at)).collect::<Vec<_>>();
        let [a, b, c, d] = &self.sections;
        SidebarModel {
            sections: [(a.0, a.1, threads(&a.2)), (b.0, b.1, threads(&b.2)), (c.0, c.1, threads(&c.2)), (d.0, d.1, threads(&d.2))],
            by_coordinator: self.by_coordinator.iter().map(|(id, places)| (id.clone(), threads(places))).collect(),
            facts: &self.facts,
        }
    }
}

/// The cache for the state the sidebar draws, made again when that state changed.
fn cache(root: &mut Root, state: &WorkspaceState, cx: &gpui::App) -> Rc<SidebarCache> {
    let held = root.state(cx);
    let own = std::ptr::eq(Rc::as_ptr(&held), state);
    let outline = own.then(|| root.workspace.read(cx).outline_revision);
    // Text streaming into a message leaves the lists as they were.
    if let Some(cache) = root.sidebar.model.as_ref().filter(|cache| cache.holds(state) || (outline.is_some() && cache.outline == outline)) {
        cache.rows.set_now(act_core::util::now());
        return cache.clone();
    }
    let held = if own { held } else { Rc::new(state.clone()) };
    let cache = Rc::new(SidebarCache::new(&held, outline));
    // A state made here for one draw is not kept.
    if Rc::strong_count(&held) > 1 {
        root.sidebar.model = Some(cache.clone());
    }
    cache
}

/// This computer's threads and those of the paired computers the filter shows, as the lists draw them.
fn model(state: &WorkspaceState) -> OwnedModel<'_> {
    let activity = thread_activity(state);
    let lists = thread_lists(state);
    let remote = remote_collections(state);
    let own = state.computers.filter == "all" || state.computers.filter == "this";
    let mut projects = if own { state.projects.clone() } else { Vec::new() };
    projects.extend(remote.projects.iter().map(|project| (*project).clone()));
    let mut threads: Vec<&Thread> = if own { lists.visible_threads.clone() } else { Vec::new() };
    threads.extend(remote.threads.iter().copied());
    let union = |own: &HashSet<String>, others: &HashSet<String>| own.union(others).cloned().collect::<HashSet<String>>();
    let ranked = union(&activity.ranked, &remote.ranked);
    let blocked = union(&activity.blocked, &remote.blocked);
    let sidebar = sidebar_lists(state, &projects, &threads, &ranked, &blocked, Some(&remote.project_hosts));
    let sections = sidebar.activity_threads;
    OwnedModel {
        sections: [
            (SidebarSection::Priority, "Priority", sections.priority),
            (SidebarSection::Running, "Running", sections.running),
            (SidebarSection::Threads, "Threads", sections.threads),
            (SidebarSection::Snoozed, "Snoozed", sections.snoozed),
        ],
        by_coordinator: sidebar.threads_by_coordinator.into_iter().collect(),
        facts: SidebarFacts {
            projects: sidebar.projects,
            activity_project: sidebar.activity_project,
            busy: union(&activity.working, &remote.busy),
            monitoring: union(&activity.monitoring, &remote.monitoring),
            blocked,
            waiting: sidebar.waiting_lines,
            worktree_ids: union(&lists.worktree_thread_ids, &remote.worktree_thread_ids),
            side_chat_attention: union(&lists.side_chat_attention, &remote.side_chat_attention),
            thread_hosts: remote.thread_hosts,
            remote_worktrees: remote.worktrees.into_iter().cloned().collect(),
        },
    }
}

/// The model as `model` derives it, owning its facts.
struct OwnedModel<'a> {
    sections: [(SidebarSection, &'static str, Vec<&'a Thread>); 4],
    by_coordinator: HashMap<String, Vec<&'a Thread>>,
    facts: SidebarFacts,
}

impl std::ops::Deref for OwnedModel<'_> {
    type Target = SidebarFacts;

    fn deref(&self) -> &SidebarFacts {
        &self.facts
    }
}

fn section_open(state: &WorkspaceState, section: SidebarSection) -> bool {
    match section {
        SidebarSection::Priority => state.sections.priority,
        SidebarSection::Running => state.sections.running,
        SidebarSection::Threads => state.sections.threads,
        SidebarSection::Snoozed => state.sections.snoozed,
    }
}

/// Sidebar-local state the workspace does not hold: the scroll, and the project filter's anchor.
pub struct SidebarState {
    pub scroll: ScrollHandle,
    pub filter_probe: BoundsCell,
    pub computer_probe: BoundsCell,
    /// The width the edge was dragged to; None follows the stylesheet.
    pub width: Option<f32>,
    /// Whether the sidebar floated on the last frame, and whether floating folded it away.
    pub floating: Option<bool>,
    pub folded: bool,
    /// The thread the list last scrolled to, so it follows the one the keyboard steps to.
    pub followed: Option<String>,
    pub resizer_focus: Option<gpui::FocusHandle>,
    /// The open thread's row, measured each frame.
    pub active_probe: BoundsCell,
    /// Each row's place in the keyboard's tab order.
    pub row_focus: HashMap<String, gpui::FocusHandle>,
    /// The row a rename ended by a key hands focus back to once it is drawn again.
    pub refocus_row: Option<String>,
    /// The row the keyboard is on, measured each frame, under which its menu opens from the keyboard.
    pub focused_probe: BoundsCell,
    /// The row under the pointer, whose rail widens to its actions.
    pub hovered_row: Option<String>,
    /// The computers switch and the project filter, which the arrow keys open.
    pub computer_focus: Option<gpui::FocusHandle>,
    pub filter_focus: Option<gpui::FocusHandle>,
    /// One row's height and each list's top in the scrolled content (window position less the
    /// scroll offset), as last drawn: they pick the rows a draw lays out.
    pub row_height: Rc<Cell<Option<Pixels>>>,
    pub list_tops: HashMap<SidebarSection, Rc<Cell<Option<Pixels>>>>,
    /// The rows the last draw laid out.
    pub drawn: Vec<String>,
    /// The model of the state last drawn.
    pub model: Option<Rc<SidebarCache>>,
    /// Whether the sidebar was open on the last frame, and when it last started sliding.
    shown: Option<bool>,
    slide_start: Option<std::time::Instant>,
}

impl Default for SidebarState {
    fn default() -> Self {
        SidebarState { scroll: ScrollHandle::new(), filter_probe: bounds_cell(), computer_probe: bounds_cell(), width: None, floating: None, folded: false, followed: None, resizer_focus: None, active_probe: bounds_cell(), row_focus: HashMap::new(), refocus_row: None, focused_probe: bounds_cell(), hovered_row: None, computer_focus: None, filter_focus: None, row_height: Rc::default(), list_tops: HashMap::new(), drawn: Vec::new(), model: None, shown: None, slide_start: None }
    }
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let cached = cache(root, state, cx);
    let sidebar = cached.model(state);
    let open = state.sidebar_open && !state.zen;
    let settings_open = state.settings_shown();

    let can_go_back = act_core::reducer::reachable_visit(state, -1).is_some();
    let can_go_forward = act_core::reducer::reachable_visit(state, 1).is_some();
    let strip = div()
        .id("traffic-space")
        .relative()
        .h(px(m.window_strip))
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .gap(px(4.0))
        .pl(px((m.window_controls_inset - 10.0).max(0.0)))
        .child(drag_layer("traffic-drag"))
        .when(!state.computers.paired.is_empty(), |strip| strip.child(no_drag(computer_switch(root, state, window, cx))))
        .child(
            div()
                .occlude()
                .flex()
                .items_center()
                .gap(px(1.0))
                .child(
                    IconButton::new("go-back", IconName::ChevronLeft, "Go back")
                        .icon_size(16.0)
                        .tip(key_tip("Back", "Mod+["))
                        .disabled(!can_go_back)
                        .on_click(|_, _, cx| dispatch(AppCommand::GoBack, cx)),
                )
                .child(
                    IconButton::new("go-forward", IconName::ChevronRight, "Go forward")
                        .icon_size(16.0)
                        .tip(key_tip("Forward", "Mod+]"))
                        .disabled(!can_go_forward)
                        .on_click(|_, _, cx| dispatch(AppCommand::GoForward, cx)),
                ),
        );

    let bar_height = m.chrome_head - m.window_strip;
    let new_thread = rimmed(div(), t, 8.0)
        .id("new-thread")
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(7.0))
        .px(px(10.0))
        .bg(t.frame_control)
        .transition(act_ui::style::touch())
        .hover(|style| style.bg(t.frame_control_hover))
        .active(|style| style.transform(gpui::ElementTransform::scale(0.97)))
        .text_color(t.ink_dim)
        .text_size(px(m.text_control))
        .font_weight(FontWeight(500.0))
        .cursor_pointer()
        .child(icon(IconName::SquarePen, 14.0, t.ink_dim))
        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child("New thread"))
        .tip(key_tip("New thread", "Mod+N"))
        .on_click(|_, _, cx| dispatch(AppCommand::TaskNew { project_id: None, worktree_id: None }, cx));
    let search = rimmed(div(), t, 8.0)
        .id("sidebar-search")
        .flex_none()
        .min_w(px(bar_height))
        .flex()
        .items_center()
        .justify_center()
        .px(px(9.0))
        .bg(t.frame_control)
        .transition(gpui::Transition::ease(150))
        .hover(|style| style.bg(t.frame_control_hover))
        .cursor_pointer()
        .child(icon(IconName::Search, 14.0, t.muted_strong))
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, "Search threads and settings"))
        .tip(key_tip("Search", "Mod+K"))
        .on_click(|_, _, cx| dispatch(AppCommand::JumpOpen, cx));
    let bar = div().flex().flex_none().gap(px(6.0)).h(px(bar_height)).mx(px(3.0)).child(new_thread).child(search);

    let mut lists = div().flex().flex_col();
    if sidebar.projects.is_empty() {
        lists = lists.child(empty_projects(state, cx));
    }
    if sidebar.projects.len() > 1 || sidebar.activity_project.is_some() {
        lists = lists.child(project_filter(root, state, &sidebar, window, cx));
    }
    let row_context = &cached.rows;
    let pinned = pinned_rows(root, state, window, cx);
    let mut drawn_ids = Vec::new();
    let mut first = true;
    for (section, label, threads) in &sidebar.sections {
        if matches!(section, SidebarSection::Running | SidebarSection::Snoozed) && threads.is_empty() {
            continue;
        }
        let section = *section;
        let open = section_open(state, section);
        let priority = section == SidebarSection::Priority;
        let dotted = if priority {
            threads.iter().filter(|thread| can_dismiss(thread, sidebar.by_coordinator.get(&thread.id).map(Vec::as_slice).unwrap_or(&[]), &sidebar.blocked)).count()
        } else {
            0
        };
        let heading_id = SharedString::from(format!("section-{label}"));
        let toggle_focus = window.use_keyed_state(SharedString::from(format!("{heading_id}-focus")), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        let action_focus = window.use_keyed_state(SharedString::from(format!("{heading_id}-action-focus")), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        // The fold shows while anything in the heading holds focus, as `:focus-within` shows it.
        let focus_within = toggle_focus.is_focused(window) || (dotted > 0 && action_focus.is_focused(window));
        let toggle = div()
            .id(heading_id.clone())
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, "").expanded(open))
            .track_focus(&toggle_focus)
            .focus_ring(true)
            .flex_1()
            .min_w_0()
            // Without an action, the toggle runs to the rows' edge, inset as far as on its left.
            .when(dotted == 0, |toggle| toggle.mr(px(4.0 - m.row_inset)))
            .h(px(30.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .px(px(6.0))
            .ml(px(-6.0))
            .rounded(px(7.0))
            .text_color(t.quiet)
            .text_size(px(11.0))
            .font_weight(FontWeight(620.0))
            .letter_spacing(0.06)
            .hover(|style| style.bg(t.wash).text_color(t.ink_dim))
            .cursor_pointer()
            .child(label.to_uppercase())
            .when(!open && !threads.is_empty(), |toggle| toggle.child(count_chip(threads.len().to_string(), t)))
            .child(chevron(open, focus_within, heading_id.clone()))
            .on_click(move |_, _, cx| dispatch(AppCommand::SetSectionOpen { section, open: !open }, cx));
        let mut heading = div().group(heading_id.clone()).flex().items_center().min_h(px(30.0)).pl(px(10.0)).pr(px(m.row_inset)).child(toggle);
        if dotted > 0 {
            let tip: SharedString = if dotted == 1 { "Dismiss 1 thread".into() } else { format!("Dismiss all {dotted} threads").into() };
            let project = sidebar.activity_project.clone();
            heading = heading.child(
                div()
                    .id("dismiss-all")
                    .track_focus(&action_focus)
                    .focus_ring(true)
                    .flex_none()
                    .w(px(28.0))
                    .h(px(34.0))
                    .mx(px(-3.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .text_color(t.quiet)
                    .group_hover(heading_id.clone(), |style| style.text_color(t.muted_strong))
                    .hover(|style| style.bg(t.wash))
                    .cursor_pointer()
                    .child(act_ui::Icon::new(IconName::CheckCheck).size(px(16.0)))
                    .tip(tip)
                    .on_click(move |_, _, cx| dispatch(AppCommand::TaskDismissAll { local_only: None, project_id: project.clone() }, cx)),
            );
        }
        let mut group = div().flex().flex_col().when(!first, |group| group.mt(px(12.0))).child(heading);
        first = false;
        if open && threads.is_empty() && priority {
            group = group.child(div().mx(px(11.0)).my(px(4.0)).text_color(t.quiet).text_size(px(m.text_control)).line_height(relative(1.5)).child("Nothing waiting"));
        }
        if open {
            let mut list = div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Navigation, *label)).relative().flex().flex_col().min_h(px(8.0));
            let pitch = root.sidebar.row_height.get().unwrap_or(px(ESTIMATED_ROW_HEIGHT));
            let mut next = 0;
            for index in drawn_rows(&root.sidebar, section, threads, &pinned) {
                if index > next {
                    list = list.child(div().flex_none().h(pitch * (index - next) as f32));
                }
                let thread = threads[index];
                let dismiss = priority && can_dismiss(thread, sidebar.by_coordinator.get(&thread.id).map(Vec::as_slice).unwrap_or(&[]), &sidebar.blocked);
                let entry = row(root, row_context, thread, dismiss, priority, window, cx).into_any_element();
                // One row a draw is measured: rows are all as tall, which places the ones left out.
                if drawn_ids.is_empty() && root.renaming.as_deref() != Some(thread.id.as_str()) {
                    list = list.child(div().relative().child(entry).child(row_height_probe(&root.sidebar.row_height)));
                } else {
                    list = list.child(entry);
                }
                drawn_ids.push(thread.id.clone());
                next = index + 1;
            }
            if threads.len() > next {
                list = list.child(div().flex_none().h(pitch * (threads.len() - next) as f32));
            }
            let top = root.sidebar.list_tops.entry(section).or_default().clone();
            group = group.child(list.child(list_top_probe(&top, &root.sidebar.scroll)));
        }
        lists = lists.child(group);
    }

    let listed: std::collections::HashSet<&str> = sidebar.sections.iter().flat_map(|(_, _, threads)| threads.iter().map(|thread| thread.id.as_str())).collect();
    root.sidebar.row_focus.retain(|id, _| listed.contains(id.as_str()));
    root.sidebar.drawn = drawn_ids;
    let scroll = div()
        .relative()
        .flex_1()
        .min_h_0()
        .child(div().id("sidebar-scroll").size_full().overflow_y_scroll().track_scroll(&root.sidebar.scroll).pt(px(6.0)).pb(px(12.0)).pr(act_ui::components::scrollbar::gutter(&root.sidebar.scroll)).child(lists))
        .edge_fade(gpui::EdgeFade::vertical().start(px(0.0), px(12.0)).end(px(0.0), px(12.0)))
        .child(act_ui::components::scrollbar::Scrollbar::vertical("sidebar-scrollbar", &root.sidebar.scroll));

    let footer = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .id("sidebar-settings")
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(10.0))
                .h(px(32.0))
                .rounded(px(9.0))
                .px(px(11.0))
                .text_size(px(m.text_control))
                .cursor_pointer()
                .map(|button| {
                    if settings_open {
                        rimmed(button, t, 9.0).bg(t.frame_row_selected).text_color(t.ink)
                    } else {
                        button.text_color(t.muted).hover(|style| style.bg(t.wash).text_color(t.ink_faint))
                    }
                })
                .child(act_ui::Icon::new(IconName::Settings).size(px(18.0)))
                .child("Settings")
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, "Settings").toggled(settings_open))
                .tip(key_tip("Settings", "Mod+,"))
                .on_click(|_, _, cx| dispatch(AppCommand::SetSettingsOpen { open: true, section: None, setting_id: None }, cx)),
        )
        .children(update_button(state, cx));

    let current = state.selected_computer().and_then(|computer| computer.state.as_deref()).unwrap_or(state).current_id.clone();
    if root.sidebar.followed != current {
        root.sidebar.followed = current;
        let probe = root.sidebar.active_probe.clone();
        let scroll = root.sidebar.scroll.clone();
        probe.set(None);
        follow_row(probe, scroll, None, FOLLOW_PASSES, window);
    }
    let floating = floats(state, window);
    let mut aside = div()
        .id("sidebar")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Complementary, ""))
        .tab_order(-2)
        .inert(!open || settings_open)
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .pl(px(10.0))
        .pr(px(if floating { 10.0 } else { 3.0 }))
        .pb(px(10.0))
        .child(strip)
        .child(bar)
        .child(scroll)
        .child(footer);
    if floating {
        aside = aside
            .occlude()
            .pane(t.glass_menu)
            .glass_backdrop(&t)
            .shadow(t.shadow_sidebar.box_shadows())
            .rim(t.glass_rim, act_ui::style::Corners::Radius(0.0));
    }
    aside = aside.when(t.page.a == 0.0, |aside| aside.child(div().absolute().top_0().bottom_0().right(px(-4.5)).w(px(1.0)).bg(t.line_glass)));
    if open {
        aside = aside.child(resizer(root, window, cx));
    }
    aside
}

/// How many frames following the open thread may take: rows laid out where spacers stood can move
/// it after the first scroll.
const FOLLOW_PASSES: usize = 3;

/// Which edge of the scroll padding a followed row was brought to.
#[derive(Clone, Copy, PartialEq)]
enum FollowEdge {
    Top,
    Bottom,
}

/// Scrolls the list on the next frame so the measured row sits inside the scroll padding, then
/// holds it at the edge it was brought to once the rows around it are laid out.
fn follow_row(probe: BoundsCell, scroll: ScrollHandle, edge: Option<FollowEdge>, passes: usize, window: &mut Window) {
    window.on_next_frame(move |window, _| {
        let (Some(row), view) = (probe.get(), scroll.bounds()) else { return };
        let padding = px(16.0);
        let edge = edge.or(if row.top() < view.top() + padding {
            Some(FollowEdge::Top)
        } else if row.bottom() > view.bottom() - padding {
            Some(FollowEdge::Bottom)
        } else {
            None
        });
        let delta = match edge {
            Some(FollowEdge::Top) => view.top() + padding - row.top(),
            Some(FollowEdge::Bottom) => view.bottom() - padding - row.bottom(),
            None => return,
        };
        if delta.abs() < px(0.5) {
            return;
        }
        let mut offset = scroll.offset();
        // Whole pixels, as Chromium snaps `scrollTop`.
        offset.y = (offset.y + delta).round();
        scroll.set_offset(offset);
        window.refresh();
        if passes > 1 {
            follow_row(probe, scroll, edge, passes - 1, window);
        }
    });
}

/// How far past the visible part of a list rows are laid out; the rest are left out as
/// `content-visibility: auto` on `.task-entry` leaves them, their room kept by spacers.
const ROW_OVERSCAN: f32 = 240.0;
/// A row's height until one is measured (`contain-intrinsic-block-size: auto 45px`).
const ESTIMATED_ROW_HEIGHT: f32 = 45.0;
/// Rows a list lays out before it has been placed once.
const FIRST_DRAW_ROWS: usize = 40;

/// Rows that are laid out wherever they are: the open thread's, the one being renamed, and any
/// holding keyboard focus, so following, renaming and Tab keep their element.
fn pinned_rows(root: &Root, state: &WorkspaceState, window: &Window, cx: &gpui::App) -> HashSet<String> {
    let mut pinned = HashSet::new();
    pinned.extend(state.selected_computer().and_then(|computer| computer.state.as_deref()).unwrap_or(state).current_id.clone());
    pinned.extend(root.renaming.clone());
    if window.focused(cx).is_some() {
        pinned.extend(root.sidebar.drawn.iter().filter(|id| root.sidebar.row_focus.get(*id).is_some_and(|focus| focus.contains_focused(window, cx))).cloned());
    }
    pinned
}

/// The indices of a list's rows this draw lays out, in order.
fn drawn_rows(sidebar: &SidebarState, section: SidebarSection, threads: &[&Thread], pinned: &HashSet<String>) -> Vec<usize> {
    let count = threads.len();
    let top = sidebar.list_tops.get(&section).and_then(|top| top.get());
    let range = match (sidebar.row_height.get(), top) {
        (Some(pitch), Some(top)) if pitch > px(0.0) => {
            let view = sidebar.scroll.bounds();
            let list_top = top + sidebar.scroll.offset().y;
            let first = ((view.top() - px(ROW_OVERSCAN) - list_top) / pitch).floor().max(0.0) as usize;
            let last = ((view.bottom() + px(ROW_OVERSCAN) - list_top) / pitch).ceil().max(0.0) as usize;
            first.min(count)..last.min(count)
        }
        _ => 0..count.min(FIRST_DRAW_ROWS),
    };
    let mut drawn: Vec<usize> = range.clone().collect();
    if !pinned.is_empty() {
        drawn.extend((0..count).filter(|index| !range.contains(index) && pinned.contains(&threads[*index].id)));
        drawn.sort_unstable();
    }
    drawn
}

/// Records a list's top in the scrolled content; a change draws the sidebar again.
fn list_top_probe(cell: &Rc<Cell<Option<Pixels>>>, scroll: &ScrollHandle) -> impl IntoElement {
    let (cell, scroll) = (cell.clone(), scroll.clone());
    gpui::canvas(
        move |bounds, window, _| {
            let top = bounds.top() - scroll.offset().y;
            if cell.get().is_none_or(|old| (old - top).abs() > px(0.5)) {
                cell.set(Some(top));
                window.request_animation_frame();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// Records a row's height before rounding to device pixels, so the rows left out stack in layout
/// units as Chromium's do; a change draws the sidebar again.
fn row_height_probe(cell: &Rc<Cell<Option<Pixels>>>) -> RowHeightProbe {
    RowHeightProbe(cell.clone())
}

struct RowHeightProbe(Rc<Cell<Option<Pixels>>>);

impl IntoElement for RowHeightProbe {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for RowHeightProbe {
    type RequestLayoutState = gpui::LayoutId;
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, gpui::LayoutId) {
        let mut style = gpui::Style { position: gpui::Position::Absolute, ..gpui::Style::default() };
        style.inset.top = px(0.0).into();
        style.inset.left = px(0.0).into();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        let id = window.request_layout(style, [], cx);
        (id, id)
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        layout_id: &mut gpui::LayoutId,
        window: &mut Window,
        _: &mut gpui::App,
    ) {
        let height = window.unrounded_layout_size(*layout_id).height;
        if height > px(0.0) && self.0.get().is_none_or(|old| (old - height).abs() > px(0.01)) {
            self.0.set(Some(height));
            window.request_animation_frame();
        }
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        _: &mut gpui::LayoutId,
        _: &mut (),
        _: &mut Window,
        _: &mut gpui::App,
    ) {
    }
}

/// The box the sidebar takes in the window as it slides in and out, which its drawn part fills.
pub fn slot(root: &mut Root, state: &WorkspaceState, window: &mut Window) -> gpui::Div {
    let open = state.sidebar_open && !state.zen;
    let floating = floats(state, window);
    let sidebar_width = if floating { floating_width(window) } else { width(&root.sidebar, window) };
    let shown = slide(&mut root.sidebar, open, window);
    let slot = div().flex_none().w(px(sidebar_width)).h_full();
    if floating {
        slot.absolute().top_0().bottom_0().left(px(-(sidebar_width + 60.0) * (1.0 - shown))).when(shown == 0.0, |slot| slot.invisible())
    } else {
        slot.ml(px(-sidebar_width * (1.0 - shown)))
    }
}

const SLIDE_MS: f32 = 300.0;

/// How far the sidebar has slid in, 0 hidden to 1 shown, easing as `--panel-slide-ease` does.
fn slide(sidebar: &mut SidebarState, open: bool, window: &mut Window) -> f32 {
    if sidebar.shown.is_some_and(|shown| shown != open) {
        sidebar.slide_start = Some(std::time::Instant::now());
    }
    sidebar.shown = Some(open);
    let Some(start) = sidebar.slide_start else { return if open { 1.0 } else { 0.0 } };
    let elapsed = start.elapsed().as_secs_f32() * 1000.0 / SLIDE_MS;
    if elapsed >= 1.0 {
        sidebar.slide_start = None;
        return if open { 1.0 } else { 0.0 };
    }
    window.request_animation_frame();
    let eased = cubic_bezier(0.32, 0.72, 0.0, 1.0, elapsed);
    if open { eased } else { 1.0 - eased }
}

/// CSS `cubic-bezier(x1, y1, x2, y2)` at time `t`.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let curve = |a: f32, b: f32, s: f32| 3.0 * a * s * (1.0 - s).powi(2) + 3.0 * b * s * s * (1.0 - s) + s.powi(3);
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (low + high) / 2.0;
        if curve(x1, x2, mid) < t {
            low = mid;
        } else {
            high = mid;
        }
    }
    curve(y1, y2, (low + high) / 2.0)
}

/// How much of the blur shows over the right panel, which Chromium blurs from its translucent
/// glass, so its own crisp copy shows through.
const DOCK_VEIL_BLUR: f32 = 0.5;

/// The veil behind a floating sidebar, which closes it. It blurs the workspace, but the right
/// panel beside it only partly.
pub fn backdrop(root: &Root, state: &WorkspaceState, shown: &WorkspaceState, window: &Window, cx: &mut Context<Root>) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    let veil = t.veil;
    let gap = cx.theme().metrics.frame_gap;
    let viewport = f32::from(window.viewport_size().width);
    let dock = act_core::state::front_dock(shown).1;
    let dock_left = (dock.open && !dock.expanded && !state.zen).then(|| viewport - super::dock::width(viewport, viewport - 2.0 * gap, root.dock.width));
    div()
        .id("sidebar-backdrop")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, "Close sidebar"))
        .skip_tab_stop()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .child(div().absolute().top_0().bottom_0().left_0().map(|blurred| match dock_left {
            Some(left) => blurred.w(px(left)),
            None => blurred.right_0(),
        }).bg(t.veil).veil_backdrop(&t))
        .children(dock_left.map(|left| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .left(px(left))
                .child(div().absolute().size_full().opacity(DOCK_VEIL_BLUR).veil_backdrop(&t))
                .child(div().absolute().size_full().bg(t.veil))
        }))
        // Glass over the veil frosts the transcript as the veil dims it.
        .child(gpui::canvas(|_, _, _| {}, move |bounds, _, window, _| window.tint_hidden_backdrops(bounds, veil)).absolute().size_full())
        .on_click(|_, _, cx| dispatch(AppCommand::SetSidebarOpen { open: false }, cx))
}

/// The edge the sidebar is dragged wider by. The arrow keys nudge it, Home and End take it to its
/// narrowest and widest, and a double-click or Enter puts it back.
fn resizer(root: &mut Root, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    let focus = root.sidebar.resizer_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
    let focused = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
    div()
        .id("sidebar-resizer")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Splitter, "Resize sidebar"))
        .group("sidebar-resizer")
        .absolute()
        .top_0()
        .bottom_0()
        .right(px(-9.0))
        .w(px(10.0))
        .cursor_col_resize()
        .track_focus(&focus)
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(if focused { 3.5 } else { 4.0 }))
                .right(px(if focused { 3.5 } else { 4.0 }))
                .when(focused, |line| line.bg(t.accent))
                .when(!focused, |line| line.group_hover("sidebar-resizer", |style| style.bg(t.line_input))),
        )
        .tip("Drag to resize · Double-click to reset")
        .on_drag(SidebarResize, |_, _, _, cx| cx.new(|_| ResizePreview))
        .on_click(cx.listener(|root, event: &gpui::ClickEvent, _, cx| {
            if event.click_count() == 2 {
                root.sidebar.width = None;
                cx.notify();
            }
        }))
        .on_key_down(cx.listener(|root, event: &gpui::KeyDownEvent, window, cx| {
            let viewport = f32::from(window.viewport_size().width);
            let current = width(&root.sidebar, window);
            let step = if event.keystroke.modifiers.shift { SHIFT_KEY_STEP } else { KEY_STEP };
            let next = match event.keystroke.key.as_str() {
                "enter" => None,
                "home" => Some(MIN_SIDEBAR_WIDTH),
                "end" => Some(viewport / 2.0),
                "left" => Some(current - step),
                "right" => Some(current + step),
                _ => return,
            };
            root.sidebar.width = next.map(|next| next.round().clamp(MIN_SIDEBAR_WIDTH, (viewport / 2.0).max(MIN_SIDEBAR_WIDTH)));
            cx.stop_propagation();
            cx.notify();
        }))
}

/// Says why the lists are empty and offers the way out (ProjectSidebar's empty state).
fn empty_projects(state: &WorkspaceState, cx: &mut Context<Root>) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let filter = state.computers.filter.as_str();
    let selected = state.computers.paired.iter().find(|computer| computer.link.id == filter);
    let away = selected.filter(|computer| computer.link.status != ComputerStatus::Connected);
    let block = div().flex().flex_col().items_start().gap(px(10.0)).mx(px(11.0)).my(px(12.0)).text_color(t.quiet).text_size(px(m.text_control)).line_height(relative(1.5));
    match away {
        Some(computer) => {
            let message = if computer.link.status == ComputerStatus::Connecting { format!("Reconnecting to {}…", computer.link.name) } else { format!("{} is offline", computer.link.name) };
            block.child(div().child(message)).child(Button::new("sidebar-show-all", "Show all computers").variant(ButtonVariant::Soft).on_click(|_, _, cx| dispatch(AppCommand::ComputersFilter { filter: "all".into() }, cx)))
        }
        None => {
            let name = if filter == "all" {
                "No projects yet".to_string()
            } else {
                let name = selected.map(|computer| computer.link.name.clone()).unwrap_or_else(|| if state.computers.name.is_empty() { "This computer".into() } else { state.computers.name.clone() });
                format!("{name} has no projects yet")
            };
            block.child(div().child(name)).child(Button::new("sidebar-add-project", "Add project").icon(IconName::Plus).icon_size(14.0).variant(ButtonVariant::Soft).on_click(|_, _, cx| dispatch(AppCommand::ProjectOpen { start: false }, cx)))
        }
    }
}

/// A newer build: offered, then downloading with how far it has come, then waiting on a restart.
fn update_button(state: &WorkspaceState, cx: &mut Context<Root>) -> Option<gpui::AnyElement> {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let pill = |id: &'static str| {
        div()
            .id(id)
            .when(!t.reduced_motion, |pill| pill.enter(act_ui::style::panel_slide(220), |style| style.opacity(0.0).transform(gpui::ElementTransform::scale(0.92))))
            .flex_none()
            .h(px(28.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .pl(px(9.0))
            .pr(px(11.0))
            .rounded_full()
            .border_1()
            .border_color(t.accent_line)
            .bg(t.accent_chip)
            .text_color(t.accent_soft)
            .text_size(px(m.text_meta))
            .font_weight(FontWeight(600.0))
    };
    match &state.app_update {
        AppUpdate::Idle => None,
        AppUpdate::Available { version } => Some(
            pill("sidebar-update")
                .cursor_pointer()
                .transition(gpui::Transition::ease(140))
                .hover(|style| style.border_color(t.accent_line_strong).text_color(t.accent_bright))
                .child(act_ui::Icon::new(IconName::ArrowDown).size(px(14.0)))
                .child("Update")
                .tip(format!("Update to version {version}"))
                .on_click(|_, _, cx| dispatch(AppCommand::AppDownloadUpdate, cx))
                .into_any_element(),
        ),
        AppUpdate::Downloading { version, percent } => Some(
            pill("sidebar-update")
                .tabular_nums()
                .child(super::ring::Ring { progress: *percent as f32 / 100.0, size: 14.0, thickness: 2.0, track: t.accent_line, fill: t.accent_soft })
                .child(format!("{percent}%"))
                .tip(format!("Downloading version {version}: {percent}%"))
                .into_any_element(),
        ),
        AppUpdate::Ready { version } => Some(
            Button::new("sidebar-update", "Restart")
                .icon(IconName::RotateCw)
                .icon_size(14.0)
                .variant(ButtonVariant::Primary)
                .pill(true)
                .tip(format!("Restart to install version {version}"))
                .on_click(|_, _, cx| dispatch(AppCommand::AppInstallUpdate, cx))
                .into_any_element(),
        ),
    }
}

/// The size the fold mark's glyph is drawn at, so its strokes match a 6px box's right and bottom
/// edges turned 45 degrees.
const CHEVRON_GLYPH: f32 = 10.25;

/// The fold mark: a chevron drawn as two strokes in the heading's colour, open pointing down. It
/// takes a 6px box in the heading and turns about the box's centre, its strokes past the box.
fn chevron(open: bool, focus_within: bool, group: SharedString) -> impl IntoElement {
    let overhang = (CHEVRON_GLYPH - 6.0) / 2.0;
    // The glyph's corner sits 1.8px nearer its centre than the turned box's does; open, it is also
    // lifted 2px.
    let transform = if open { gpui::ElementTransform::translate(px(0.0), px(-0.2)) } else { gpui::ElementTransform::translate(px(1.8), px(0.0)).rotated(-90.0) };
    div()
        .id("chevron")
        .relative()
        .flex_none()
        .size(px(6.0))
        .transition(gpui::Transition::ease(120).only(&[gpui::TransitionProperty::Opacity, gpui::TransitionProperty::Transform]))
        .transform(transform)
        .when(open && !focus_within, |mark| mark.opacity(0.0).group_hover(group, |style| style.opacity(1.0)))
        .child(div().absolute().left(px(-overhang)).top(px(-overhang)).size(px(CHEVRON_GLYPH)).flex().child(ink_icon(IconName::HeadingChevron, CHEVRON_GLYPH)))
}

/// Narrows the activity lists to one project (ProjectFilter).
fn project_filter(root: &mut Root, state: &WorkspaceState, sidebar: &SidebarModel, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    let selected = sidebar.activity_project.as_ref().and_then(|id| sidebar.projects.iter().find(|project| &project.id == id));
    let label = selected.map(project_name).unwrap_or_else(|| "All projects".into());
    let narrowed = selected.is_some();
    let probe = root.sidebar.filter_probe.clone();
    let projects = sidebar.projects.clone();
    let chosen = sidebar.activity_project.clone();
    let open = root.menus.is_open("activity:project");
    let focus = root.sidebar.filter_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
    let ringed = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
    let menu_probe = probe.clone();
    let open_menu = Rc::new(move |root: &mut Root, window: &mut Window, cx: &mut Context<Root>| {
        if root.menus.is_open("activity:project") {
            root.menus.close(cx);
            return;
        }
        let probe = menu_probe.clone();
        let anchor = probe.get().unwrap_or_default();
        let entries = filter_entries(&projects, chosen.as_deref(), "");
        let filter_projects = projects.clone();
        let filter_chosen = chosen.clone();
        let menu = Menu::build(entries, Placement::Anchor { anchor, align: Align::Start }, window, cx, move |menu| {
            menu.from_frame().checkable(true).min_width(px(220.0)).exempt(&probe).searchable("Search projects", move |query, _| filter_entries(&filter_projects, filter_chosen.as_deref(), query))
        });
        root.menus.show("activity:project", menu, cx);
    });
    let on_key = open_menu.clone();
    let trigger = div()
        .id("activity-filter")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, format!("Project: {label}")).expanded(open))
        .track_focus(&focus)
        .relative()
        .flex()
        .items_center()
        .gap(px(5.0))
        .max_w_full()
        .h(px(24.0))
        .pl(px(8.0))
        .pr(px(7.0))
        .rounded(px(7.0))
        .text_size(px(12.0))
        .cursor_pointer()
        // A narrowed trigger keeps its fill while hovered or open.
        .map(|trigger| if narrowed { trigger.bg(t.accent_chip).text_color(t.ink_dim) } else { trigger.bg(t.frame_control).text_color(t.muted) })
        .when(open && !narrowed, |trigger| trigger.bg(t.frame_control_hover).text_color(t.muted_strong))
        .when(!narrowed, |trigger| trigger.hover(|style| style.bg(t.frame_control_hover).text_color(t.muted_strong)))
        .child(icon(IconName::Folder, 12.0, t.quiet))
        .child(div().ellipsis().child(label))
        .child(icon(IconName::ChevronDown, 12.0, t.quiet))
        .child(bounds_probe(&probe))
        .when(ringed, |trigger| trigger.child(div().absolute().inset_0().rounded(px(7.0)).border_2().border_color(t.focus_ring)))
        .on_key_down(cx.listener(move |root, event: &gpui::KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "down" | "up") && !root.menus.is_open("activity:project") {
                cx.stop_propagation();
                on_key(root, window, cx);
            }
        }))
        .on_click(cx.listener(move |root, _, window, cx| open_menu(root, window, cx)));
    let mut filter = div().flex().items_center().gap(px(2.0)).mt(px(8.0)).mb(px(2.0)).ml(px(3.0)).mr(px(8.0)).child(trigger);
    if narrowed {
        filter = filter.child(
            div()
                .id("activity-filter-clear")
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, "Show all projects"))
                .size(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.0))
                .text_color(t.muted)
                .hover(|style| style.bg(t.wash).text_color(t.ink_dim))
                .cursor_pointer()
                .child(ink_icon(IconName::X, 14.0))
                .tip("All projects")
                .on_click(|_, _, cx| dispatch(AppCommand::SetActivityProject { project_id: None }, cx)),
        );
    }
    let _ = state;
    filter
}

fn filter_entries(projects: &[act_core::domain::project::Project], chosen: Option<&str>, query: &str) -> Vec<MenuEntry> {
    let query = query.trim().to_lowercase();
    let mut entries: Vec<MenuEntry> = Vec::new();
    if query.is_empty() {
        entries.push(MenuItem::new("All projects").checked(chosen.is_none()).on_select(|_, cx| dispatch(AppCommand::SetActivityProject { project_id: None }, cx)).into());
        entries.push(MenuEntry::Separator);
    }
    let mut matches: Vec<(usize, &act_core::domain::project::Project)> = projects
        .iter()
        .filter_map(|project| {
            let name = project_name(project).to_lowercase();
            if query.is_empty() {
                return Some((0, project));
            }
            if name.starts_with(&query) {
                Some((0, project))
            } else if name.split(|ch: char| !ch.is_alphanumeric()).any(|word| word.starts_with(&query)) {
                Some((1, project))
            } else if name.contains(&query) {
                Some((2, project))
            } else {
                None
            }
        })
        .collect();
    matches.sort_by_key(|(rank, _)| *rank);
    for (_, project) in matches {
        let id = project.id.clone();
        entries.push(MenuItem::new(project_name(project)).checked(chosen == Some(project.id.as_str())).on_select(move |_, cx| dispatch(AppCommand::SetActivityProject { project_id: Some(id.clone()) }, cx)).into());
    }
    entries
}

/// Which computers' threads the lists draw (ComputerSwitch).
fn computer_switch(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    let filter = state.computers.filter.clone();
    let narrowed = filter != "all";
    let away = state.computers.paired.iter().filter(|computer| computer.link.status != act_core::domain::platform::ComputerStatus::Connected).count();
    let mut choices: Vec<(String, String, Option<&'static str>)> = vec![("all".into(), "All".into(), None), ("this".into(), if state.computers.name.is_empty() { "This computer".into() } else { state.computers.name.clone() }, None)];
    for computer in &state.computers.paired {
        let status = if computer.link.status == act_core::domain::platform::ComputerStatus::Connected { None } else if computer.link.status == act_core::domain::platform::ComputerStatus::Connecting { Some("Reconnecting") } else { Some("Offline") };
        choices.push((computer.link.id.clone(), computer.link.name.clone(), status));
    }
    let probe = root.sidebar.computer_probe.clone();
    let open = root.menus.is_open("sidebar:computers");
    let focus = root.sidebar.computer_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
    let ringed = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
    let selected = choices.iter().find(|choice| choice.0 == filter).cloned().unwrap_or_else(|| choices[0].clone());
    let selected_away = selected.2.is_some();
    let selected_label = selected.1.clone();
    let open_menu = {
        let probe = probe.clone();
        let choices = choices.clone();
        let filter = filter.clone();
        Rc::new(move |root: &mut Root, window: &mut Window, cx: &mut Context<Root>| {
            if root.menus.is_open("sidebar:computers") {
                root.menus.close(cx);
                return;
            }
            let anchor = probe.get().unwrap_or_default();
            let entries: Vec<MenuEntry> = choices
                .iter()
                .map(|(id, label, status)| {
                    let id = id.clone();
                    let mut item = MenuItem::new(label.clone()).checked(id == filter);
                    if let Some(status) = status {
                        item = item.detail(*status).away(true);
                    }
                    item.on_select(move |_, cx| dispatch(AppCommand::ComputersFilter { filter: id.clone() }, cx)).into()
                })
                .collect();
            let probe = probe.clone();
            let menu = Menu::build(entries, Placement::Anchor { anchor, align: Align::Start }, window, cx, |menu| menu.from_frame().checkable(true).exempt(&probe));
            root.menus.show("sidebar:computers", menu, cx);
        })
    };
    let tip = if narrowed { format!("Only {selected_label}{}", selected.2.map(|status| format!(" ({})", status.to_lowercase())).unwrap_or_default()) } else if away > 0 { format!("Computers · {}", choices.iter().filter_map(|choice| choice.2.map(|status| format!("{} {}", choice.1, status.to_lowercase()))).collect::<Vec<_>>().join(" · ")) } else { "Computers".into() };
    // A narrow sidebar shortens the computer's name before the header runs into the traffic lights.
    div()
        .min_w_0()
        .flex()
        .items_center()
        .child(
            div()
                .id("computer-switch")
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, if away > 0 { format!("Computers, {away} offline") } else { "Computers".to_string() }).expanded(open))
                .relative()
                .h(px(28.0))
                .min_w(px(28.0))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(5.0))
                .when(narrowed, |button| button.flex_shrink().max_w(px(120.0)).pl(px(6.0)).pr(px(8.0)))
                .rounded(px(8.0))
                .when(narrowed, |button| rimmed(button, &t, 8.0).bg(t.accent_chip))
                .when(open && !narrowed, |button| rimmed(button, &t, 8.0).pane(t.glass_control_hover))
                .when(!narrowed && !open, |button| button.hover(|style| style.bg(t.surface_hover).text_color(t.ink_dim)))
                .cursor_pointer()
                .text_color(if narrowed { t.accent_soft } else if open { t.ink } else { t.quiet_strong })
                .child(div().flex_none().child(act_ui::Icon::new(if selected_away { IconName::MonitorOff } else { IconName::Monitor }).size(px(16.0))))
                .when_some(narrowed.then(|| selected_label.clone()), |button, name| button.child(div().min_w_0().ellipsis().text_size(px(12.0)).font_weight(FontWeight::MEDIUM).child(name)))
                .when(!narrowed && away > 0, |button| button.child(div().absolute().top(px(3.5)).right(px(3.5)).size(px(9.0)).rounded_full().bg(t.warning).border(px(1.5)).border_color(t.sidebar)))
                .child(bounds_probe(&probe))
                .when(ringed, |button| button.child(div().absolute().inset_0().rounded(px(8.0)).border_2().border_color(t.focus_ring)))
                .tip(tip)
                .track_focus(&focus)
                .on_key_down(cx.listener({
                    let open_menu = open_menu.clone();
                    move |root, event: &gpui::KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "down" | "up") && !root.menus.is_open("sidebar:computers") {
                            cx.stop_propagation();
                            open_menu(root, window, cx);
                        }
                    }
                }))
                .on_click(cx.listener(move |root, _, window, cx| open_menu(root, window, cx))),
        )
        .child(div().w(px(1.0)).h(px(14.0)).ml(px(1.0)).mr(px(5.0)).bg(t.line))
}

/// `json!` commands for the shapes with many optional fields.
pub fn command(value: serde_json::Value) -> Option<AppCommand> {
    match serde_json::from_value(value.clone()) {
        Ok(command) => Some(command),
        Err(error) => {
            eprintln!("unreadable command {value}: {error}");
            None
        }
    }
}

pub fn send(value: serde_json::Value, cx: &mut gpui::App) {
    if let Some(command) = command(value) {
        dispatch(command, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use act_core::domain::engine::AgentEngine;
    use act_core::domain::platform::ComputerLink;
    use act_core::domain::run::ExecutionPolicy;
    use act_core::state::PairedComputer;

    fn thread(id: &str, ranked: i64) -> Thread {
        let mut thread = Thread::new(id, id, AgentEngine::Claude, ExecutionPolicy::Confirm, ranked);
        thread.ranked_at = ranked;
        thread
    }

    fn ids<'a>(model: &[(SidebarSection, &'static str, Vec<&'a Thread>); 4]) -> Vec<Vec<String>> {
        model.iter().map(|(_, _, threads)| threads.iter().map(|thread| thread.id.clone()).collect()).collect()
    }

    #[test]
    fn cached_model_lists_the_threads_the_state_lists() {
        let mut paired_state = WorkspaceState::default();
        paired_state.threads = vec![thread("far-1", 7).into(), thread("far-2", 3).into()];
        let mut state = WorkspaceState::default();
        state.threads = (0..5).map(|index| thread(&format!("near-{index}"), index).into()).collect();
        state.computers.filter = "all".into();
        state.computers.paired.push(PairedComputer {
            link: ComputerLink { id: "far".into(), name: "Far".into(), host: "far".into(), status: ComputerStatus::Connected, error: None, paired_at: 0, capabilities: None },
            state: Some(Box::new(paired_state)),
        });
        let held = Rc::new(state);
        let cache = SidebarCache::new(&held, None);
        let fresh = model(&held);
        assert!(ids(&fresh.sections).iter().flatten().any(|id| id == "far-1"));
        assert_eq!(ids(&cache.model(&held).sections), ids(&fresh.sections));
        assert!(cache.holds(&held));
        assert!(!cache.holds(&held.as_ref().clone()));
    }
}
