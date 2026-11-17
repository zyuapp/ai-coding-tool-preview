//! The session summary card at the conversation's top right: where the thread works, its changes,
//! branch and pull request, its schedule, and the agents and processes it has going
//! (SessionPanel.tsx, WorkspaceSession.tsx).

use act_core::domain::pull_request::PullRequestAnswer;
use act_core::domain::run::SubagentStatus;
use act_core::events::ChangedFilesResult;
use act_core::state::{front_dock, WorkspaceState};
use act_core::workspace::location::{project_for, thread_workspace_id};
use act_core::AppCommand;
use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
use act_ui::overlay::{bounds_cell, bounds_probe, Align, BoundsCell, Placement};
use act_ui::style::{Corners, PaneFill, Raised};
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, SharedString, Window};
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;

use super::letter_spacing::LetterSpacing;
use super::common::{icon, ink_icon, Ellipsis};
use super::root::Root;
use super::sidebar::send;
use crate::workspace::dispatch;

pub struct SessionState {
    pub branch_probe: BoundsCell,
    pub location_probe: BoundsCell,
    /// What the open location menu was last built from, so it is rebuilt only when that changes.
    location_view: Option<act_core::view::dock::WorktreeMenuView>,
}

impl Default for SessionState {
    fn default() -> Self {
        SessionState { branch_probe: bounds_cell(), location_probe: bounds_cell(), location_view: None }
    }
}

/// How much of the conversation's right side the card keeps clear.
/// The widest window, in layout pixels, that takes responsive.css's narrow layout.
pub const NARROW_WINDOW: f32 = 1100.0;

/// Where the transcript's column starts and how much more its right side keeps clear, in a window
/// `width` wide: the narrow layout pads the conversation 28px and keeps 300px beside the card.
pub fn conversation_insets(state: &WorkspaceState, width: f32, page_inset: f32) -> (f32, f32) {
    let reserved = reserved(state);
    if width <= NARROW_WINDOW {
        (28.0, if reserved > 0.0 { 272.0 } else { 0.0 })
    } else {
        (page_inset, reserved)
    }
}

/// The composer strip's side padding and the room it keeps beside the card.
pub fn composer_insets(state: &WorkspaceState, width: f32, page_inset: f32) -> (f32, f32) {
    let reserved = reserved(state);
    if width <= NARROW_WINDOW {
        (24.0, if reserved > 0.0 { 272.0 } else { 0.0 })
    } else {
        (page_inset, reserved)
    }
}

pub fn reserved(state: &WorkspaceState) -> f32 {
    let dock_open = front_dock(state).1.open;
    if state.session_panel_open && !dock_open && !state.zen {
        282.0
    } else {
        0.0
    }
}

fn row(id: impl Into<SharedString>, glyph: IconName, label: impl Into<SharedString>, cx: &Context<Root>) -> gpui::Stateful<gpui::Div> {
    row_with(id, glyph, label, true, cx)
}

/// `grows`: the label takes the row's spare room; otherwise what follows it does.
fn row_with(id: impl Into<SharedString>, glyph: IconName, label: impl Into<SharedString>, grows: bool, cx: &Context<Root>) -> gpui::Stateful<gpui::Div> {
    let t = cx.theme().tokens.clone();
    let label: SharedString = label.into();
    div()
        .id(id.into())
        .accessible_name("")
        .w_full()
        .min_h(px(36.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(6.0))
        .rounded(px(8.0))
        .text_color(t.ink_dim)
        .text_size(px(14.0))
        .cursor_pointer()
        .group(SESSION_ROW)
        .hover(|style| style.bg(t.surface_hover))
        .child(div().w(px(22.0)).flex_none().flex().justify_center().child(icon(glyph, 16.0, t.muted)))
        .child(div().when(grows, |label| label.flex_1().min_w_0()).when(!grows, |label| label.flex_none()).whitespace_nowrap().overflow_hidden().child(label))
}

/// A row whose glyph says something in its own colour, such as a pull request's state.
fn row_glyph(id: impl Into<SharedString>, glyph: IconName, color: gpui::Rgba, label: impl Into<SharedString>, cx: &Context<Root>) -> gpui::Stateful<gpui::Div> {
    let t = cx.theme().tokens.clone();
    div()
        .id(id.into())
        .w_full()
        .min_h(px(36.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(6.0))
        .rounded(px(8.0))
        .text_color(t.ink_dim)
        .text_size(px(14.0))
        .cursor_pointer()
        .group(SESSION_ROW)
        .hover(|style| style.bg(t.surface_hover))
        .child(div().w(px(22.0)).flex_none().flex().justify_center().child(icon(glyph, 16.0, color)))
        .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().child(label.into()))
}

/// The row's arrow, quiet until its row is under the pointer.
fn go(cx: &Context<Root>) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    div()
        .id("go")
        .w(px(14.0))
        .flex_none()
        .flex()
        .transition(gpui::Transition::ease(120))
        .text_color(t.quiet)
        .group_hover(SESSION_ROW, |style| style.text_color(t.ink_dim).transform(gpui::ElementTransform::translate(px(2.0), px(0.0))))
        .child(ink_icon(IconName::ChevronRight, 12.0))
}

const SESSION_ROW: &str = "session-row";

fn environment_message(environment: Option<&ChangedFilesResult>, workspace_id: Option<&str>) -> Option<String> {
    if workspace_id.is_none() {
        return Some("Reopen the project to inspect Git".into());
    }
    let Some(environment) = environment else { return Some("Reading Git…".into()) };
    match environment {
        ChangedFilesResult::Error { message } => Some(message.clone()),
        ChangedFilesResult::Unknown { .. } => Some("Workspace is no longer registered".into()),
        ChangedFilesResult::Unavailable { reason } => Some(format!("Workspace is {}", serde_json::to_value(reason).ok().and_then(|value| value.as_str().map(String::from)).unwrap_or_default())),
        ChangedFilesResult::Available { .. } => None,
    }
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let mono = theme.typography.mono_family.clone();
    let thread = state.current_thread();
    let project = match thread {
        Some(thread) => project_for(state, Some(thread)),
        None => state.draft_project_id.as_ref().and_then(|id| state.project(id)),
    };
    let workspace_id = match thread {
        Some(thread) => thread_workspace_id(state, Some(thread)),
        None => project.and_then(|project| project.workspace_id.clone()),
    };
    let environment = workspace_id.as_ref().and_then(|id| state.environments.get(id));
    let mut rows = div().flex().flex_col().p(px(8.0));

    if let (Some(thread), Some(_)) = (thread, project) {
        if let Some(element) = location_row(root, state, thread, cx) {
            rows = rows.child(element);
        }
    }

    if project.is_some() {
        let available = match environment {
            Some(ChangedFilesResult::Available { additions, deletions, branch, baseline, .. }) => Some((*additions, *deletions, branch.clone(), baseline.clone())),
            _ => None,
        };
        let task = thread.map(|thread| thread.id.clone());
        let mut changes = row("session-changes", IconName::FileDiff, "Changes", cx).on_click(move |_, _, cx| dispatch(AppCommand::DiffToggle { task_id: task.clone() }, cx));
        if let Some((additions, deletions, _, baseline)) = &available {
            changes = changes.child(
                div()
                    .id("change-counts")
                    .flex()
                    .gap(px(8.0))
                    .tabular_nums()
                    .text_size(px(12.0))
                    .font_weight(FontWeight(600.0))
                    .child(div().text_color(t.success).child(format!("+{additions}")))
                    .child(div().text_color(t.danger).child(format!("−{deletions}")))
                    .tip(baseline.as_ref().map(|baseline| format!("Since {baseline}")).unwrap_or_else(|| "Uncommitted work".into())),
            );
        }
        rows = rows.child(changes.child(go(cx)));
        let branch = available.as_ref().and_then(|(_, _, branch, _)| branch.clone());
        let probe = root.session.branch_probe.clone();
        let branch_open = root.menus.is_open("session:branch");
        let branch_workspace = workspace_id.clone();
        let current_task = thread.map(|thread| thread.id.clone());
        rows = rows.child(
            row_with("session-branch", IconName::GitBranch, "Branch", false, cx)
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, "").expanded(branch_open))
                .relative()
                .child(div().flex_none().ml(px(-4.0)).text_color(if branch_open { t.quiet_strong } else { t.quiet }).group_hover(SESSION_ROW, |style| style.text_color(t.quiet_strong)).child(ink_icon(IconName::ChevronDown, 12.0)))
                .child(div().flex_1().min_w_0().flex().justify_end().child(div().max_w(px(140.0)).whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(t.muted).font_family(mono.clone()).text_size(px(12.0)).child(branch.clone().unwrap_or_else(|| "—".into()))))
                .child(bounds_probe(&probe))
                .on_click(cx.listener(move |root, _, window, cx| {
                    let Some(workspace_id) = branch_workspace.clone() else { return };
                    let anchor = probe.get().unwrap_or_default();
                    root.open_branch_menu(&workspace_id, anchor, branch.clone(), current_task.clone(), window, cx);
                })),
        );
        let pull_request = state.pull_request.as_ref().filter(|read| Some(&read.workspace_id) == workspace_id.as_ref()).map(|read| read.answer.clone());
        match pull_request {
            Some(PullRequestAnswer::Found { pull_request }) => {
                let url = pull_request.url.clone();
                let color = super::coordination::pull_request_color(pull_request.state, cx);
                rows = rows.child(
                    row_glyph("session-pull-request", super::coordination::pull_request_icon(pull_request.state), color, "Pull request", cx)
                        .child(div().text_color(t.muted).font_family(mono.clone()).text_size(px(12.0)).child(format!("#{}", pull_request.number)))
                        .tip(format!("#{} {}", pull_request.number, pull_request.title))
                        .on_mouse_down(gpui::MouseButton::Right, super::side_chat::web_link_menu(url.clone()))
                        .on_click(super::side_chat::web_link(url)),
                );
            }
            Some(PullRequestAnswer::GhMissing) => {
                rows = rows.child(
                    row_glyph("session-gh", IconName::GitPullRequest, t.quiet, "Install gh for pull requests", cx)
                        .tip("Pull requests are read with the GitHub CLI, which is not installed")
                        .on_mouse_down(gpui::MouseButton::Right, super::side_chat::web_link_menu("https://cli.github.com".into()))
                        .on_click(super::side_chat::web_link("https://cli.github.com".into())),
                );
            }
            _ => {}
        }
        if let Some(message) = environment_message(environment, workspace_id.as_deref()) {
            rows = rows.child(div().mt(px(2.0)).mr(px(6.0)).ml(px(34.0)).text_color(t.quiet).text_size(px(12.0)).line_height(gpui::relative(1.4)).child(message));
        }
    }

    let automations = state.automations.iter().filter(|automation| Some(&automation.automation.task_id) == state.current_id.as_ref()).count();
    rows = rows.child(
        row("session-automations", IconName::AlarmClock, "Automations", cx)
            .child(div().text_color(t.muted).text_size(px(12.0)).tabular_nums().child(automations.to_string()))
            .child(go(cx))
            .on_click(|_, _, cx| send(json!({ "type": "view.open-dock-panel", "panel": "automation" }), cx)),
    );

    let mut card = div().flex().flex_col().child(rows);
    if let Some(id) = &state.current_id {
        if let Some(subagents) = state.subagents.get(id).filter(|subagents| !subagents.is_empty()) {
            card = card.child(subagent_section(state, subagents, window, cx));
        }
        let processes = state.background_processes.get(id).map(Vec::as_slice).unwrap_or(&[]);
        let workflows = state.workflows.get(id).map(Vec::as_slice).unwrap_or(&[]);
        if let Some(section) = super::workflow_panel::processes_section(processes, workflows, cx) {
            card = card.child(section);
        }
    }
    if let Some(lead) = thread.filter(|thread| act_core::domain::coordination::is_coordinator(Some(thread))) {
        let sections: Vec<AnyElement> = [super::coordination::member_worktrees(state, lead, cx), super::coordination::member_pull_requests(state, lead, cx)].into_iter().flatten().collect();
        if !sections.is_empty() {
            card = card.children(sections);
        }
    }

    div()
        .id("session-float")
        .when(!t.reduced_motion, |panel| panel.enter(gpui::Transition::ease_out(150), |style| style.opacity(0.0).transform(gpui::ElementTransform::translate(px(10.0), px(0.0)))))
        .absolute()
        .top(px(16.0))
        .right(px(if f32::from(window.viewport_size().width) <= NARROW_WINDOW { 12.0 } else { 22.0 }))
        .w(px(260.0))
        .flex()
        .flex_col()
        .rounded(px(12.0))
        .elevation(&t.shadow_composer)
        .glass_pane(t.glass, &t, t.shadow_composer.glint.then_some(t.glint), Corners::Radius(12.0))
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Complementary, "Session panel"))
        .child(div().id("session-panel").max_h(px(600.0)).overflow_y_scroll().rounded(px(12.0)).child(card))
        .child(super::common::seen_by_backdrops())
}

/// The thread's subagents, working ones first, up to six; the rest are a click away in the dock.
fn subagent_section(state: &WorkspaceState, subagents: &[act_core::domain::run::Subagent], window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    use act_core::domain::run::{order_subagents, subagent_line};
    use act_ui::components::button::Button;
    use act_ui::components::list::{ListHeading, ListRow};
    use super::agents_panel::{orb, status_glyph, status_ink};
    const SHOWN: usize = 6;
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let working = subagents.iter().filter(|subagent| subagent.status == SubagentStatus::Working).count();
    let open = state.subagent_groups.sidebar;
    let mut heading = ListHeading::new("subagents-heading", "Subagents").fold(open, move |_, _, cx| send(json!({ "type": "view.set-subagent-group", "group": "sidebar", "open": !open }), cx));
    if working > 0 {
        heading = heading.action(div().text_color(t.info_soft).text_size(px(theme.metrics.text_meta)).font_weight(FontWeight(550.0)).child(format!("{working} working")));
    }
    let mut section = div().border_t_1().border_color(t.line_soft).px(px(8.0)).pt(px(7.0)).pb(px(8.0)).child(heading);
    if open {
        let mut list = div().flex().flex_col().gap(px(2.0)).mt(px(8.0));
        for subagent in order_subagents(subagents).into_iter().take(SHOWN) {
            let id = subagent.id.clone();
            list = list.child(
                ListRow::new(SharedString::from(format!("subagent-{}", subagent.id)), subagent.description.clone())
                    .inset(6.0, 22.0)
                    .leading(orb(subagent.status, 22.0, 12.0, t))
                    .detail(subagent_line(&subagent))
                    .trailing(div().flex_none().child(super::agents_panel::status_icon(SharedString::from(format!("subagent-status-{}", subagent.id)), status_glyph(subagent.status), 16.0, status_ink(subagent.status, t), subagent.status == act_core::domain::run::SubagentStatus::Working, window, cx)))
                    .on_click(cx.listener(move |root, _, _, cx| {
                        root.inspect_subagent(&id, cx);
                        dispatch(AppCommand::OpenDockPanel { panel: "agents".into(), task_id: None }, cx);
                    })),
            );
        }
        if subagents.len() > SHOWN {
            list = list.child(div().mt(px(4.0)).child(Button::new("subagents-view-all", format!("View all {}", subagents.len())).full_width().on_click(|_, _, cx| dispatch(AppCommand::OpenDockPanel { panel: "agents".into(), task_id: None }, cx))));
        }
        section = section.child(list);
    }
    section.into_any_element()
}

impl Root {
    /// The branch picker (BranchMenu.tsx): a search over the checkout's branches, and the name to make.
    pub fn open_branch_menu(&mut self, workspace_id: &str, anchor: gpui::Bounds<gpui::Pixels>, current: Option<String>, task_id: Option<String>, window: &mut Window, cx: &mut Context<Root>) {
        if self.menus.is_open("session:branch") {
            self.menus = Default::default();
            cx.notify();
            return;
        }
        let branches = crate::effects::window_desktop::branches(workspace_id.to_string(), cx);
        let read: Rc<RefCell<Option<Result<Vec<String>, String>>>> = Rc::new(RefCell::new(None));
        let pick = Rc::new(BranchPick { current, task_id });
        let (shared, chosen) = (read.clone(), pick.clone());
        let menu = Menu::build(branch_entries(None, "", &pick), Placement::Anchor { anchor, align: Align::Start }, window, cx, move |menu| {
            menu.named("Branches").top_level().checkable(true).min_width(px(260.0)).searchable("Search branches", move |query, _| branch_entries(shared.borrow().as_ref(), query, &chosen))
        });
        self.menus.show("session:branch", menu.clone(), cx);
        let weak_menu = menu.downgrade();
        cx.spawn(async move |_, cx| {
            let branches = branches.await;
            *read.borrow_mut() = Some(branches.map(|branches| branches.branches));
            let _ = cx.update(|cx| {
                if let Some(menu) = weak_menu.upgrade() {
                    let query = menu.read(cx).query(cx);
                    let entries = branch_entries(read.borrow().as_ref(), &query, &pick);
                    menu.update(cx, |menu, cx| menu.set_entries(entries, cx));
                }
            });
        })
        .detach();
    }
}

/// Where a picked branch goes: the thread whose checkout moves, and the branch it is on now.
struct BranchPick {
    current: Option<String>,
    task_id: Option<String>,
}

/// The name typed when no branch has it, then the branches the query keeps.
fn branch_entries(branches: Option<&Result<Vec<String>, String>>, query: &str, pick: &BranchPick) -> Vec<MenuEntry> {
    let checkout = |branch: String, create: bool, task: Option<String>| move |_: &mut Window, cx: &mut gpui::App| dispatch(AppCommand::TaskCheckoutBranch { task_id: task.clone(), branch: branch.clone(), create: create.then_some(true) }, cx);
    let names = match branches {
        None => return vec![MenuEntry::Note { text: "Reading branches…".into(), danger: false }],
        Some(Err(message)) => return vec![MenuEntry::Note { text: message.clone().into(), danger: true }],
        Some(Ok(names)) => names,
    };
    let mut entries: Vec<MenuEntry> = Vec::new();
    let typed = query.trim();
    if !typed.is_empty() && !names.iter().any(|name| name == typed) {
        entries.push(MenuItem::new(format!("Create branch \u{201c}{typed}\u{201d}")).mark(IconName::Plus).on_select(checkout(typed.to_string(), true, pick.task_id.clone())).into());
    }
    let matched = super::review_picker::match_branches(names, query);
    if matched.is_empty() && entries.is_empty() {
        entries.push(MenuEntry::Note { text: "No branch matches".into(), danger: false });
    }
    for name in matched {
        let checked = pick.current.as_deref() == Some(name.as_str());
        entries.push(MenuItem::new(name.clone()).checked(checked).on_select(checkout(name, false, pick.task_id.clone())).into());
    }
    entries
}

/// The checkout a thread works in, and the menu that moves it (SessionLocationMenu.tsx).
fn location_row(root: &mut Root, state: &WorkspaceState, thread: &act_core::domain::thread::Thread, cx: &mut Context<Root>) -> Option<AnyElement> {
    use act_core::view::dock::{worktree_menu_view, WORKTREE_MENU};
    use act_core::workspace::location::ThreadLocation;
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let threads: Vec<&act_core::domain::thread::Thread> = state.threads.iter().map(|thread| &**thread).collect();
    let view = worktree_menu_view(state, Some(thread), &threads, Some(WORKTREE_MENU), &state.worktree_menu_search)?;
    let working = matches!(view.location, ThreadLocation::Creating | ThreadLocation::Releasing);
    let label = match &view.location {
        ThreadLocation::Worktree { worktree, .. } => act_core::domain::worktree::worktree_name(worktree),
        ThreadLocation::Creating => "Creating worktree…".into(),
        ThreadLocation::Releasing => "Removing worktree…".into(),
        ThreadLocation::Local => if view.worktree_id.is_some() { "Worktree".into() } else { "Local".into() },
    };
    let glyph_color = view.worktree_id.as_ref().map(|id| t.worktree[act_core::domain::worktree::worktree_hue(id).rem_euclid(6) as usize]).unwrap_or(t.muted);
    let glyph = if view.worktree_id.is_some() || working { IconName::FolderSymlink } else { IconName::House };
    let name: AnyElement = if working { super::sweep::sweep("session-location-sweep", label.clone(), cx).into_any_element() } else { div().min_w_0().ellipsis().text_color(t.ink_dim).child(label.clone()).into_any_element() };
    let probe = root.session.location_probe.clone();
    let open = root.menus.is_open(WORKTREE_MENU);
    let tip = match &view.location {
        ThreadLocation::Worktree { worktree, .. } => Some(worktree.root.clone()),
        _ => None,
    };
    let mut label_box = div().flex_1().min_w_0().flex().items_baseline().gap(px(6.0)).child(name);
    if view.worktree_id.is_some() && view.count > 1 {
        label_box = label_box.child(div().flex_none().text_color(t.quiet_strong).text_size(px(m.text_meta)).child(format!("{} threads", view.count)));
    }
    let access_label = format!("{}, thread options", if matches!(view.location, ThreadLocation::Worktree { .. }) { format!("Worktree {label}") } else { label.to_string() });
    let mut trigger = div()
        .id("session-location")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, access_label).expanded(open))
        .relative()
        .w_full()
        .min_h(px(36.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(6.0))
        .rounded(px(8.0))
        .text_size(px(14.0))
        .cursor_pointer()
        .group("session-location")
        .when(open, |row| row.bg(t.surface_hover))
        .hover(|style| style.bg(t.surface_hover))
        .child(div().w(px(22.0)).flex_none().flex().justify_center().child(icon(glyph, 16.0, glyph_color)))
        .child(label_box)
        .child(
            div()
                .id("session-location-chevron")
                .w(px(14.0))
                .flex_none()
                .flex()
                .justify_center()
                .transition(gpui::Transition::ease(120))
                .text_color(if open { t.muted_strong } else { t.quiet })
                .group_hover("session-location", |style| style.text_color(t.muted_strong))
                .child(ink_icon(IconName::ChevronDown, 12.0)),
        )
        .child(bounds_probe(&probe));
    if let Some(tip) = tip {
        trigger = trigger.tip(tip);
    }
    let right_probe = probe.clone();
    let trigger = trigger.on_mouse_down(gpui::MouseButton::Right, cx.listener(move |root, _, window, cx| {
        // A right-click opens the same menu, and never closes it.
        if !root.menus.is_open(WORKTREE_MENU) {
            open_location_menu(root, &right_probe, window, cx);
        }
    }));
    let click_probe = probe.clone();
    let trigger = trigger.on_click(cx.listener(move |root, _, window, cx| {
        if root.menus.is_open(WORKTREE_MENU) {
            root.menus.close(cx);
        } else {
            open_location_menu(root, &click_probe, window, cx);
        }
    }));
    if let Some(menu) = root.menus.entity(WORKTREE_MENU) {
        if root.session.location_view.as_ref() != Some(&view) {
            let entries = location_entries(&view);
            root.session.location_view = Some(view.clone());
            menu.update(cx, |menu, cx| menu.set_entries(entries, cx));
        }
    }
    let mut block = div().pb(px(4.0)).mb(px(4.0)).border_b_1().border_color(t.line_soft).child(trigger);
    if let Some(error) = view.error.filter(|_| !open) {
        block = block.child(div().mt(px(2.0)).ml(px(34.0)).mr(px(6.0)).text_color(t.danger_soft).text_size(px(m.text_meta)).child(error));
    }
    Some(block.into_any_element())
}

fn open_location_menu(root: &mut Root, probe: &BoundsCell, window: &mut Window, cx: &mut Context<Root>) {
    let state = root.state(cx);
    let Some(view) = current_location_view(&state) else { return };
    let anchor = probe.get().unwrap_or_default();
    let entries = location_entries(&view);
    root.session.location_view = Some(view);
    let probe = probe.clone();
    let menu = Menu::build(entries, Placement::Anchor { anchor, align: Align::End }, window, cx, |menu| menu.top_level().min_width(px(176.0)).exempt(&probe));
    root.menus.show(act_core::view::dock::WORKTREE_MENU, menu, cx);
}

fn current_location_view(state: &WorkspaceState) -> Option<act_core::view::dock::WorktreeMenuView> {
    let thread = state.current_thread()?;
    let threads: Vec<&act_core::domain::thread::Thread> = state.threads.iter().map(|thread| &**thread).collect();
    act_core::view::dock::worktree_menu_view(state, Some(thread), &threads, Some(act_core::view::dock::WORKTREE_MENU), &state.worktree_menu_search)
}

/// The rows of a worktree submenu for `query`, keeping the workspace's search in step (WorktreeMenuChoices.tsx).
fn searched_choices(list: act_core::state::WorktreeMenuList, query: &str, cx: &mut gpui::App) -> Vec<MenuEntry> {
    let state = crate::workspace::workspace(cx).read(cx).state.clone();
    if state.worktree_menu_search.get(list) != query {
        dispatch(AppCommand::WorktreeMenuSearch { list, query: query.to_string() }, cx);
    }
    let state = crate::workspace::workspace(cx).read(cx).state.clone();
    current_location_view(&state).map(|view| choice_entries(&view, list)).unwrap_or_default()
}

fn choice_entries(view: &act_core::view::dock::WorktreeMenuView, list: act_core::state::WorktreeMenuList) -> Vec<MenuEntry> {
    use act_core::domain::worktree::WorktreeDestination;
    use act_core::state::WorktreeMenuList;
    let destinations = list == WorktreeMenuList::Destinations;
    let rows = if destinations { &view.destinations } else { &view.threads };
    let mut entries: Vec<MenuEntry> = Vec::new();
    if destinations {
        let thread_id = view.thread_id.clone();
        entries.push(MenuItem::new("New worktree").icon(IconName::Plus).edge(true).disabled(!view.can_move).on_select(move |_, cx| {
            dispatch(AppCommand::TaskMoveWorktree { task_id: Some(thread_id.clone()), destination: WorktreeDestination::New, after_run: false, sender: None, then: None }, cx)
        }).into());
        // A top edge action is ruled off from the list under it (MenuEdgeAction).
        entries.push(MenuEntry::Separator);
    }
    for choice in rows {
        let command = choice.command.clone();
        entries.push(MenuItem::new(choice.title.clone()).description(choice.detail.clone()).checked(choice.current).disabled(choice.disabled).tip(format!("{} · {}", choice.title, choice.detail)).on_select(move |_, cx| dispatch(command.clone(), cx)).into());
    }
    if rows.is_empty() {
        let note = if destinations && view.loading {
            "Reading worktrees…"
        } else if !view.search.get(list).trim().is_empty() {
            "No matches"
        } else if destinations {
            "No other worktrees"
        } else {
            "No other threads"
        };
        entries.push(MenuEntry::Note { text: note.into(), danger: false });
    }
    if destinations {
        if let Some(error) = &view.error {
            entries.push(MenuEntry::Note { text: error.clone().into(), danger: true });
            entries.push(MenuItem::new("Retry").on_select(|_, cx| dispatch(AppCommand::WorktreeMenuOpen { list: WorktreeMenuList::Destinations }, cx)).into());
        }
        entries.push(MenuEntry::Note { text: "Existing file changes stay in the old checkout.".into(), danger: false });
    }
    entries
}

fn location_entries(view: &act_core::view::dock::WorktreeMenuView) -> Vec<MenuEntry> {
    use act_core::state::WorktreeMenuList;
    use act_core::workspace::location::ThreadLocation;
    let working = matches!(view.location, ThreadLocation::Creating | ThreadLocation::Releasing);
    let creating = matches!(view.location, ThreadLocation::Creating);
    let busy_working = if creating { "Creating…" } else { "Removing…" };
    let wait_working = if creating { "Wait for the worktree to be created" } else { "Wait for the worktree to be removed" };
    let wait_move = "Wait for the run to finish";
    let mut entries: Vec<MenuEntry> = Vec::new();
    let (project_id, here) = (Some(view.project_id.clone()), view.worktree_id.clone());
    let mut new_here = MenuItem::new("New thread here").disabled(working).on_select(move |_, cx| dispatch(AppCommand::TaskNew { project_id: project_id.clone(), worktree_id: here.clone() }, cx));
    if working {
        new_here = new_here.detail(busy_working).tip(wait_working);
    }
    entries.push(new_here.into());
    if view.worktree_id.is_some() {
        let mut threads = MenuItem::new("Threads here")
            .detail(if working { busy_working.to_string() } else { view.count.to_string() })
            .disabled(working || view.count == 0)
            .on_open(|_, cx| dispatch(AppCommand::WorktreeMenuOpen { list: WorktreeMenuList::Threads }, cx))
            .submenu(choice_entries(view, WorktreeMenuList::Threads))
            .submenu_named("Threads here");
        if view.count > 8 {
            threads = threads.submenu_searchable("Search threads…", |query, cx| searched_choices(WorktreeMenuList::Threads, query, cx)).submenu_search_label("Search shared threads");
        }
        if working {
            threads = threads.tip(wait_working);
        }
        entries.push(threads.into());
    }
    entries.push(MenuEntry::Separator);
    let mut move_to = MenuItem::new("Move to worktree")
        .disabled(!view.can_move)
        .on_open(|_, cx| dispatch(AppCommand::WorktreeMenuOpen { list: WorktreeMenuList::Destinations }, cx))
        .submenu(choice_entries(view, WorktreeMenuList::Destinations))
        .submenu_named("Move destinations")
        .submenu_searchable("Search worktrees…", |query, cx| searched_choices(WorktreeMenuList::Destinations, query, cx))
        .submenu_search_label("Search worktrees or branches");
    if !view.can_move {
        move_to = move_to.detail("Run active").tip(wait_move);
    }
    entries.push(move_to.into());
    if view.worktree_id.is_some() {
        let thread_id = view.thread_id.clone();
        let mut local = MenuItem::new("Return to local").disabled(!view.can_move).on_select(move |_, cx| {
            dispatch(AppCommand::TaskMoveWorktree { task_id: Some(thread_id.clone()), destination: act_core::domain::worktree::WorktreeDestination::Local, after_run: false, sender: None, then: None }, cx)
        });
        if !view.can_move {
            local = local.detail("Run active").tip(wait_move);
        }
        entries.push(local.into());
    }
    if let Some(root) = view.delete_root.clone() {
        let detail = if view.busy_count == 1 { Some("Run active".to_string()) } else if view.busy_count > 1 { Some(format!("{} runs active", view.busy_count)) } else if !view.can_delete { Some("Removing…".to_string()) } else { None };
        let mut delete = MenuItem::new("Delete worktree…").danger(true).disabled(!view.can_delete).on_select(move |_, cx| dispatch(AppCommand::WorktreeConfirmDelete { root: Some(root.clone()) }, cx));
        if let Some(detail) = detail {
            delete = delete.detail(detail);
        }
        if !view.can_delete {
            delete = delete.tip(if view.busy_count > 0 { "Wait for the active runs to finish" } else { "The worktree is being removed" });
        }
        entries.push(MenuEntry::Separator);
        entries.push(delete.into());
    }
    entries
}
