//! How a thread that does not exist yet starts, drawn over its composer: which project, which
//! branch, whether in a checkout of its own, and whether it coordinates (ThreadStartOptions.tsx,
//! BranchMenu.tsx).

use std::cell::RefCell;
use std::rc::Rc;

use act_core::domain::project::{folder_name, project_name, Project};
use act_core::domain::thread_role::ThreadRole;
use act_core::state::{WorkspaceState, THREAD_START_PROJECT_MENU};
use act_core::util::locale_cmp;
use act_core::AppCommand;
use act_ui::components::button::{toggle_button, Button, ButtonVariant, IconButton, IconButtonSize, ToggleTone};
use act_ui::components::chip::Chip;
use act_ui::components::menu::{Menu, MenuEntry, MenuItem, MenuTrigger, TriggerVariant};
use act_ui::overlay::{bounds_cell, Align, BoundsCell, Placement};
use act_ui::style::{glass, Corners, GlassKind, PaneFill, Raised};
use act_ui::style::Backdrop;
use act_ui::theme::ActiveTheme;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, Window};
use serde_json::json;

use super::common::{icon, ink_icon, rimmed, with_alpha};
use super::review_picker::{match_branches, read_branches, BranchRead};
use super::root::Root;
use super::sidebar::send;
use crate::workspace::dispatch;

const BRANCH_MENU: &str = "thread-start:branch";
const WORKTREE_MENU: &str = "thread-start:worktree";

/// The start tray's anchors and the branches of the project it would start in.
pub struct StartState {
    project: BoundsCell,
    branch: BoundsCell,
    worktree: BoundsCell,
    branches: Option<(String, BranchRead)>,
}

impl StartState {
    /// Whether the tray's project button has been drawn, so a menu can open under it.
    pub fn project_drawn(&self) -> bool {
        self.project.get().is_some_and(|bounds| bounds.size.width > gpui::px(0.0))
    }
}

impl Default for StartState {
    fn default() -> Self {
        StartState { project: bounds_cell(), branch: bounds_cell(), worktree: bounds_cell(), branches: None }
    }
}

fn listed(state: &WorkspaceState) -> Vec<&Project> {
    state.projects.iter().filter(|project| !project.unlisted).collect()
}

fn parent_folder(root: &str) -> String {
    let trimmed = root.trim_end_matches('/');
    let parent = match trimmed.rfind('/') {
        Some(0) | None => "/",
        Some(at) => &trimmed[..at],
    };
    folder_name(parent)
}

/// Every computer's projects a draft may start in, this one's first; a paired computer's while its
/// line is up, each with the computer it is on.
fn start_projects(state: &WorkspaceState) -> Vec<(&Project, Option<&str>)> {
    let mut projects: Vec<(&Project, Option<&str>)> = listed(state).into_iter().map(|project| (project, None)).collect();
    for computer in state.computers.paired.iter().filter(|computer| computer.link.status == act_core::domain::platform::ComputerStatus::Connected) {
        let Some(held) = computer.state.as_deref() else { continue };
        let name = computer.link.name.as_str();
        projects.extend(held.projects.iter().filter(|project| !project.unlisted || held.draft_project_id.as_deref() == Some(project.id.as_str())).map(|project| (project, Some(name))));
    }
    projects
}

fn project_entries(state: &WorkspaceState, query: &str) -> Vec<MenuEntry> {
    let needle = query.trim().to_lowercase();
    let projects = start_projects(state);
    let computers = projects.iter().any(|(_, host)| host.is_some());
    let host_name = |host: Option<&str>| host.unwrap_or(if computers { "This computer" } else { "" }).to_string();
    let shared: Vec<String> = projects.iter().filter(|(project, host)| projects.iter().filter(|(other, other_host)| other_host == host && project_name(other) == project_name(project)).count() > 1).map(|(project, _)| project.id.clone()).collect();
    // The draft on screen is the paired computer's while one is.
    let drafting = state.selected_computer().and_then(|computer| computer.state.as_deref()).unwrap_or(state);
    let chosen = drafting.draft_project_id.clone();
    let pick = |id: Option<String>| {
        move |_: &mut Window, cx: &mut gpui::App| {
            if let Some(id) = &id {
                dispatch(AppCommand::RememberProject { project_id: id.clone() }, cx);
            }
            let window = crate::workspace::workspace(cx).read(cx).state.clone();
            let current = window.selected_computer().and_then(|computer| computer.state.as_deref()).unwrap_or(&window).draft_project_id.clone();
            if current != id {
                dispatch(AppCommand::TaskNew { project_id: id.clone(), worktree_id: None }, cx);
            }
            send(json!({ "type": "view.focus-composer" }), cx);
        }
    };
    let option = |project: &Project, host: Option<&str>, names_host: bool| {
        let mut item = MenuItem::new(project_name(project)).checked(chosen.as_deref() == Some(project.id.as_str())).on_select(pick(Some(project.id.clone())));
        let mut detail = Vec::new();
        if shared.contains(&project.id) {
            detail.push(parent_folder(&project.root));
        }
        if names_host && computers {
            detail.push(host_name(host));
        }
        if !detail.is_empty() {
            item = item.detail(detail.join(" · ")).inline_detail(true);
        }
        item
    };
    let matched: Vec<(&Project, Option<&str>)> = projects.iter().copied().filter(|(project, host)| needle.is_empty() || format!("{} {} {}", project_name(project), project.root, host_name(*host)).to_lowercase().contains(&needle)).collect();
    let mut entries: Vec<MenuEntry> = Vec::new();
    let mut recent = Vec::new();
    if needle.is_empty() {
        entries.push(MenuItem::new("No project").checked(chosen.is_none()).on_select(pick(None)).into());
        entries.push(MenuEntry::Separator);
        recent = state.recent_projects.iter().filter_map(|id| projects.iter().find(|(project, _)| &project.id == id)).map(|(project, host)| option(project, *host, true)).collect();
    }
    let has_recent = !recent.is_empty();
    if has_recent {
        entries.push(MenuEntry::Group { label: "Recent".into(), items: recent });
    }
    if matched.is_empty() {
        entries.push(MenuEntry::Note { text: "No project matches".into(), danger: false });
    }
    // Each computer's projects by name, this computer's before the paired ones by name.
    let mut groups: Vec<(Option<&str>, Vec<&Project>)> = Vec::new();
    for (project, host) in matched {
        match groups.iter_mut().find(|(held, _)| *held == host) {
            Some((_, group)) => group.push(project),
            None => groups.push((host, vec![project])),
        }
    }
    groups.sort_by(|(a, _), (b, _)| a.is_some().cmp(&b.is_some()).then_with(|| locale_cmp(a.unwrap_or(""), b.unwrap_or(""))));
    for (host, mut group) in groups {
        group.sort_by(|a, b| locale_cmp(&project_name(a), &project_name(b)).then_with(|| locale_cmp(&a.root, &b.root)));
        let items: Vec<MenuItem> = group.into_iter().map(|project| option(project, host, false)).collect();
        if computers {
            entries.push(MenuEntry::Group { label: host_name(host).into(), items });
        } else if has_recent {
            entries.push(MenuEntry::Group { label: "All projects".into(), items });
        } else {
            entries.extend(items.into_iter().map(MenuEntry::from));
        }
    }
    entries.push(MenuEntry::Separator);
    entries.push(MenuItem::new("Add project…").mark(IconName::Plus).edge(true).on_select(|_, cx| dispatch(AppCommand::ProjectOpen { start: false }, cx)).into());
    entries
}

/// The branch list: the name typed when no branch has it, then the branches the query keeps.
fn branch_entries(read: &BranchRead, selected: Option<&str>, query: &str) -> Vec<MenuEntry> {
    let BranchRead::Read { branches, current, .. } = read else {
        return vec![match read {
            BranchRead::Failed(message) => MenuEntry::Note { text: message.clone().into(), danger: true },
            _ => MenuEntry::Note { text: "Reading branches…".into(), danger: false },
        }];
    };
    let mut entries = Vec::new();
    let typed = query.trim();
    if !typed.is_empty() && !branches.iter().any(|branch| branch == typed) {
        let name = typed.to_string();
        entries.push(MenuItem::new(format!("Create branch \u{201c}{name}\u{201d}")).mark(IconName::Plus).on_select(move |_, cx| dispatch(AppCommand::TaskSetBranch { branch: Some(name.clone()), create: Some(true) }, cx)).into());
    }
    let matched = match_branches(branches, query);
    if matched.is_empty() && entries.is_empty() {
        entries.push(MenuEntry::Note { text: "No branch matches".into(), danger: false });
    }
    for name in matched {
        let pick = if Some(&name) == current.as_ref() { None } else { Some(name.clone()) };
        entries.push(MenuItem::new(name.clone()).checked(selected == Some(name.as_str())).on_select(move |_, cx| dispatch(AppCommand::TaskSetBranch { branch: pick.clone(), create: None }, cx)).into());
    }
    entries
}

/// The tray sits at the foot of the window, so its lists open upward, away from the composer.
fn upward(trigger: gpui::Bounds<gpui::Pixels>, window: &Window) -> Placement {
    let bottom = window.viewport_size().height;
    let top = trigger.top() - px(2.0);
    Placement::Anchor { anchor: gpui::Bounds::from_corners(gpui::point(trigger.left(), top), gpui::point(trigger.right(), bottom)), align: Align::Start }
}

impl Root {
    pub(super) fn open_start_project_picker(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        let state = self.state(cx);
        if self.menus.is_open(THREAD_START_PROJECT_MENU) {
            self.menus = Default::default();
            cx.notify();
            return;
        }
        let anchor = self.composer.start.project.get().unwrap_or_default();
        let probe = self.composer.start.project.clone();
        let menu = Menu::build(project_entries(&state, ""), upward(anchor, window), window, cx, move |menu| {
            menu.checkable(true).search_submits(false).min_width(px(220.0)).max_width(px(340.0)).max_height(px(280.0)).pinned_edge(true).group_icon("Recent", IconName::Clock).exempt(&probe).searchable("Search projects", |query, cx| {
                let state = crate::workspace::workspace(cx).read(cx).state.clone();
                project_entries(&state, query)
            })
        });
        self.menus.show(THREAD_START_PROJECT_MENU, menu, cx);
    }

    fn open_start_branch_menu(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        if self.menus.is_open(BRANCH_MENU) {
            self.menus = Default::default();
            cx.notify();
            return;
        }
        let state = self.state(cx);
        let read = self.composer.start.branches.as_ref().map(|(_, read)| read.clone()).unwrap_or(BranchRead::Reading);
        let current = if let BranchRead::Read { current, .. } = &read { current.clone() } else { None };
        let selected = state.draft_branch.as_ref().map(|branch| branch.name.clone()).or(current);
        let anchor = self.composer.start.branch.get().unwrap_or_default();
        let probe = self.composer.start.branch.clone();
        let shared = Rc::new(RefCell::new((read.clone(), selected.clone())));
        let menu = Menu::build(branch_entries(&read, selected.as_deref(), ""), upward(anchor, window), window, cx, move |menu| {
            menu.checkable(true).min_width(px(220.0)).max_width(px(340.0)).exempt(&probe).searchable("Search branches", move |query, _| {
                let (read, selected) = shared.borrow().clone();
                branch_entries(&read, selected.as_deref(), query)
            })
        });
        self.menus.show(BRANCH_MENU, menu, cx);
    }

    fn open_start_worktree_menu(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        if self.menus.is_open(WORKTREE_MENU) {
            self.menus = Default::default();
            cx.notify();
            return;
        }
        let state = self.state(cx);
        let Some(project_id) = state.draft_project_id.clone() else { return };
        let worktree = state.draft_worktree;
        let choices = act_core::view::dock::worktree_choices(&state, &project_id, state.draft_worktree_id.as_deref());
        let mut entries: Vec<MenuEntry> = vec![
            MenuItem::new("No worktree").checked(!worktree).on_select(move |_, cx| {
                if worktree {
                    send(json!({ "type": "task.set-worktree", "worktree": false }), cx);
                }
            })
            .into(),
            MenuItem::new("New worktree").checked(worktree).on_select(move |_, cx| {
                if !worktree {
                    send(json!({ "type": "task.set-worktree", "worktree": true }), cx);
                }
            })
            .into(),
        ];
        let items = choices
            .into_iter()
            .map(|choice| {
                let project_id = project_id.clone();
                MenuItem::new(choice.name.clone()).checked(false).detail(choice.branch.clone().unwrap_or_else(|| "Detached".into())).on_select(move |_, cx| dispatch(AppCommand::TaskNew { project_id: Some(project_id.clone()), worktree_id: Some(choice.id.clone()) }, cx))
            })
            .collect();
        entries.push(MenuEntry::Group { label: "Existing worktrees".into(), items });
        let anchor = self.composer.start.worktree.get().unwrap_or_default();
        let probe = self.composer.start.worktree.clone();
        let menu = Menu::build(entries, upward(anchor, window), window, cx, move |menu| menu.checkable(true).min_width(px(220.0)).max_width(px(340.0)).exempt(&probe));
        self.menus.show(WORKTREE_MENU, menu, cx);
    }
}

fn coordinator_toggle(state: &WorkspaceState, cx: &Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let coordinator = state.draft_role == Some(ThreadRole::Coordinator);
    let hue = t.role_coordinator;
    div()
        .id("start-coordinator")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::ToggleButton, "Coordinator").toggled(coordinator))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0))
        .h(px(28.0))
        .px(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .text_size(px(m.text_meta))
        .line_height(gpui::relative(1.2))
        .font_weight(gpui::FontWeight(560.0))
        .cursor_pointer()
        .border_color(gpui::transparent_black())
        .transition(act_ui::style::touch())
        // A ghost toggle (ToggleButton, accent tone) whose accent is the coordinator role's hue.
        .map(|button| if coordinator { rimmed(button, &t, 8.0).bg(with_alpha(hue, 0.16)).text_color(hue) } else { button.text_color(t.muted).hover(|style| style.bg(t.surface_hover).text_color(t.ink_dim)) })
        .child(ink_icon(IconName::Waypoints, 14.0))
        .child("Coordinator")
        .on_click(move |_, _, cx| send(json!({ "type": "task.set-role", "role": if coordinator { serde_json::Value::Null } else { json!("coordinator") } }), cx))
        .into_any_element()
}

/// The tray over a new thread's composer.
pub fn render(root: &mut Root, state: &WorkspaceState, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let mut git = div().flex().items_center().gap(px(4.0)).max_w_full();
    if start_projects(&root.state(cx)).is_empty() {
        git = git.child(Button::new("start-add-project", "Add project").variant(ButtonVariant::Ghost).icon(IconName::Plus).on_click(|_, _, cx| dispatch(AppCommand::ProjectOpen { start: false }, cx))).child(coordinator_toggle(state, cx));
        return tray(div().child(git), cx);
    }
    let project = state.draft_project_id.as_ref().and_then(|id| state.project(id)).cloned();
    let workspace_id = project.as_ref().and_then(|project| project.workspace_id.clone());
    if let Some(id) = &workspace_id {
        if root.composer.start.branches.as_ref().map(|(known, _)| known) != Some(id) {
            root.composer.start.branches = Some((id.clone(), BranchRead::Reading));
            let wanted = id.clone();
            read_branches(id.clone(), cx, move |root, read, _| {
                if root.composer.start.branches.as_ref().map(|(id, _)| id) == Some(&wanted) {
                    root.composer.start.branches = Some((wanted, read));
                }
            });
        }
    }
    let label = project.as_ref().map(project_name).unwrap_or_else(|| "No project".into());
    let projects_open = root.menus.is_open(THREAD_START_PROJECT_MENU);
    let picker = MenuTrigger::new("start-project")
        .variant(TriggerVariant::Field)
        .icon(if project.is_some() { IconName::FolderGit2 } else { IconName::MessageCircle })
        .name(format!("Project: {label}"))
        .label(label)
        .open(projects_open)
        .probe(&root.composer.start.project)
        .on_click(cx.listener(|root, _, window, cx| root.open_start_project_picker(window, cx)));
    let mut options = div().flex().flex_wrap().items_center().gap(px(4.0)).child(picker);
    if let Some(project) = &project {
        if let Some(name) = state.draft_worktree_id.as_deref().and_then(|id| act_core::workspace::location::worktree_by_id(state, Some(id))).map(act_core::domain::worktree::worktree_name) {
            let leave = name.clone();
            git = git.child(
                glass(div(), &t, GlassKind::Chip, Some(8.0), None)
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .h(px(28.0))
                    .pl(px(9.0))
                    .pr(px(3.0))
                    .text_size(px(m.text_meta))
                    .font_weight(gpui::FontWeight(560.0))
                    .text_color(t.ink_soft)
                    .child(icon(IconName::FolderSymlink, 16.0, t.muted))
                    .child(div().whitespace_nowrap().child(super::text_fit::fit_line(&name, 200.0, m.text_meta, window)))
                    .child(IconButton::new("start-leave-worktree", IconName::X, format!("Leave {leave}")).size(IconButtonSize::Xs).tip("Start in the project instead").on_click(|_, _, cx| send(json!({ "type": "task.set-worktree", "worktree": false }), cx))),
            );
        } else {
            let read = root.composer.start.branches.as_ref().map(|(_, read)| read.clone());
            let current = match &read {
                Some(BranchRead::Read { current, .. }) => current.clone(),
                _ => None,
            };
            let unreadable = matches!(read, Some(BranchRead::Failed(_))) && state.draft_branch.is_none();
            let selected = state.draft_branch.as_ref().map(|branch| branch.name.clone()).or(current);
            let branch_label = selected.unwrap_or_else(|| if unreadable { "Branches unavailable".into() } else { "Current branch".into() });
            let branch_name = format!("Starting branch: {branch_label}{}", if state.draft_branch.as_ref().is_some_and(|branch| branch.create) { " (new)" } else { "" });
            let mut branch = MenuTrigger::new("start-branch").name(branch_name).variant(TriggerVariant::Field).icon(if unreadable { IconName::CircleAlert } else { IconName::GitBranch }).label(branch_label).danger(unreadable).open(root.menus.is_open(BRANCH_MENU)).probe(&root.composer.start.branch);
            if state.draft_branch.as_ref().is_some_and(|branch| branch.create) {
                branch = branch.child(Chip::tag("new"));
            }
            branch = match (&workspace_id, &read) {
                (None, _) => branch.disabled(true).tip("Starts from the project's current branch"),
                (_, Some(BranchRead::Failed(message))) if unreadable => branch.tip(message.clone()).on_click(cx.listener(|root, _, window, cx| root.open_start_branch_menu(window, cx))),
                _ => branch.on_click(cx.listener(|root, _, window, cx| root.open_start_branch_menu(window, cx))),
            };
            git = git.child(branch);
            let worktree = state.draft_worktree;
            let choices = act_core::view::dock::worktree_choices(state, &project.id, None);
            if choices.is_empty() {
                git = git.child(toggle_button("start-worktree", "Worktree", worktree).toggle_tone(ToggleTone::Accent).icon(IconName::FolderSymlink).on_click(move |_, _, cx| send(json!({ "type": "task.set-worktree", "worktree": !worktree }), cx)));
            } else {
                git = git.child(
                    MenuTrigger::new("start-worktree")
                        .variant(TriggerVariant::Field)
                        .icon(IconName::FolderSymlink)
                        .name(format!("Worktree: {}", if worktree { "New worktree" } else { "None" }))
                        .label(if worktree { "New worktree" } else { "Worktree" })
                        .chosen(worktree)
                        .open(root.menus.is_open(WORKTREE_MENU))
                        .probe(&root.composer.start.worktree)
                        .on_click(cx.listener(|root, _, window, cx| root.open_start_worktree_menu(window, cx))),
                );
            }
        }
    }
    git = git.child(coordinator_toggle(state, cx));
    options = options.child(git);
    tray(options, cx)
}

fn tray(content: gpui::Div, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    div()
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, "How this thread starts"))
        .w_full()
        .max_w(px(860.0))
        .relative()
        .mb(px(-17.0))
        .pt(px(6.0))
        .px(px(6.0))
        .pb(px(23.0))
        .rounded_t(px(17.0))
        .pane(t.glass)
        .glass_backdrop(&t)
        .shadow({
            // `.glass-pane::before`'s dark hairline outside the edge.
            let mut shadows = t.shadow_float.box_shadows();
            shadows.insert(0, act_ui::theme::ShadowLayer { x: 0.0, y: 0.0, blur: 0.0, spread: 0.5, color: t.glass_float_outline }.box_shadow());
            shadows
        })
        .edged_rim(t.glass_edge, t.glass_rim, Corners::Radius(17.0))
        .child(content)
        .into_any_element()
}
