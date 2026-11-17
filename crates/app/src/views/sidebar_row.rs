//! One thread's row in the sidebar: title, meta line, and the rail of marks and actions
//! (SidebarThreadRow.tsx, thread-menu.ts).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use act_core::domain::automation::{AutomationRunStatus, AutomationView};
use act_core::domain::coordination::lead_id_of;
use act_core::domain::engine::engine_label;
use act_core::domain::project::{project_name, Project};
use act_core::domain::thread::{has_unread_attention, newest_unread_finding, Thread, ThreadOutcome, MEMBER_BLOCKED_LABEL, MEMBER_UNREAD_LABEL, SNOOZE_OPTIONS};
use act_core::domain::thread_role::{ThreadRole, THREAD_ROLES};
use act_core::domain::usage_limit::{line_positions, pause_summary};
use act_core::domain::worktree::{worktree_hue, worktree_name, Worktree};
use act_core::state::WorkspaceState;
use act_core::view::sidebar::ThreadHost;
use act_core::workspace::coordination::row_headline;
use act_core::AppCommand;
use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
use act_ui::overlay::Placement;
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, point, prelude::*, px, relative, App, Context, FontWeight, MouseButton, Rgba, SharedString, Window};
use serde_json::json;

use super::common::{engine_icon, icon, ink_icon, recency_label, rimmed, Ellipsis};
use super::root::Root;
use super::sidebar::{send, SidebarFacts};
use crate::workspace::dispatch;

/// What every row draws from besides its thread, settled once per frame.
pub struct RowContext {
    current: Option<String>,
    busy: HashSet<String>,
    monitoring: HashSet<String>,
    blocked: HashSet<String>,
    waiting: HashMap<String, String>,
    members: HashMap<String, Vec<String>>,
    worktree_ids: HashSet<String>,
    worktrees: HashMap<String, Worktree>,
    side_chat_attention: HashSet<String>,
    schedules: HashMap<String, AutomationView>,
    limit_positions: HashMap<String, usize>,
    projects: Vec<Project>,
    unread: HashSet<String>,
    now: Cell<i64>,
    /// A sidebar out of view, or under the settings sheet, takes no keyboard focus.
    inert: bool,
    /// The paired computer each of its threads lives on.
    hosts: HashMap<String, ThreadHost>,
}

impl RowContext {
    pub fn set_now(&self, now: i64) {
        self.now.set(now);
    }

    pub fn new(state: &WorkspaceState, sidebar: &SidebarFacts, by_coordinator: &HashMap<String, Vec<&Thread>>) -> RowContext {
        RowContext {
            // The thread on screen, which is a paired computer's while one is.
            current: state.selected_computer().and_then(|computer| computer.state.as_deref()).unwrap_or(state).current_id.clone(),
            busy: sidebar.busy.clone(),
            monitoring: sidebar.monitoring.clone(),
            blocked: sidebar.blocked.clone(),
            waiting: sidebar.waiting.clone(),
            members: by_coordinator.iter().map(|(id, threads)| (id.clone(), threads.iter().map(|thread| thread.id.clone()).collect())).collect(),
            worktree_ids: sidebar.worktree_ids.clone(),
            worktrees: state.worktrees.iter().chain(sidebar.remote_worktrees.iter()).map(|worktree| (worktree.id.clone(), worktree.clone())).collect(),
            side_chat_attention: sidebar.side_chat_attention.clone(),
            schedules: state.automations.iter().map(|automation| (automation.automation.task_id.clone(), automation.clone())).collect(),
            limit_positions: line_positions(&state.threads),
            projects: state.projects.iter().chain(sidebar.projects.iter()).cloned().collect(),
            unread: state.threads.iter().chain(state.computers.paired.iter().filter_map(|computer| computer.state.as_deref()).flat_map(|held| held.threads.iter())).filter(|thread| has_unread_attention(thread)).map(|thread| thread.id.clone()).collect(),
            now: Cell::new(act_core::util::now()),
            inert: !state.sidebar_open || state.zen || state.settings_shown(),
            hosts: sidebar.thread_hosts.clone(),
        }
    }
}

enum Status {
    Dot { tone: Option<Tone>, label: String },
    Paused(String),
    Working,
    Monitoring,
}

#[derive(Clone, Copy)]
enum Tone {
    Approval,
    Failed,
    Stopped,
    Finished,
}

fn status(context: &RowContext, thread: &Thread, pause: &Option<String>) -> Option<Status> {
    let members = context.members.get(&thread.id).cloned().unwrap_or_default();
    let blocked = context.blocked.contains(&thread.id);
    let member_blocked = members.iter().any(|member| context.blocked.contains(member));
    if blocked || member_blocked {
        return Some(Status::Dot { tone: Some(Tone::Approval), label: if blocked { "Needs approval".into() } else { MEMBER_BLOCKED_LABEL.into() } });
    }
    if let Some(waiting) = context.waiting.get(&thread.id) {
        return Some(Status::Dot { tone: Some(Tone::Approval), label: waiting.clone() });
    }
    if let Some(pause) = pause {
        return Some(Status::Paused(pause.clone()));
    }
    if context.busy.contains(&thread.id) || members.iter().any(|member| context.busy.contains(member)) {
        return Some(Status::Working);
    }
    if context.monitoring.contains(&thread.id) {
        return Some(Status::Monitoring);
    }
    if let Some(finding) = newest_unread_finding(thread) {
        return Some(Status::Dot { tone: None, label: finding.headline.clone() });
    }
    if has_unread_attention(thread) {
        let (tone, label) = match thread.outcome {
            Some(ThreadOutcome::Failed) => (Tone::Failed, "Failed"),
            Some(ThreadOutcome::Stopped) => (Tone::Stopped, "Stopped"),
            _ => (Tone::Finished, "Finished"),
        };
        return Some(Status::Dot { tone: Some(tone), label: label.into() });
    }
    if context.side_chat_attention.contains(&thread.id) {
        return Some(Status::Dot { tone: None, label: "A side chat is waiting".into() });
    }
    if members.iter().any(|member| context.unread.contains(member)) {
        return Some(Status::Dot { tone: None, label: MEMBER_UNREAD_LABEL.into() });
    }
    None
}

fn schedule_label(automation: &AutomationView) -> &'static str {
    if automation.automation.paused {
        "Schedule paused"
    } else if automation.next_run_at.is_none() {
        "Schedule missed its run"
    } else if automation.automation.last_status == Some(AutomationRunStatus::Failed) {
        "Runs on a schedule, and its last run failed"
    } else if automation.automation.last_status == Some(AutomationRunStatus::Skipped) {
        "Runs on a schedule, and its last tick could not run"
    } else {
        "Runs on a schedule"
    }
}

fn schedule_icon(automation: &AutomationView) -> IconName {
    let trouble = !automation.automation.paused && (automation.next_run_at.is_none() || matches!(automation.automation.last_status, Some(AutomationRunStatus::Failed) | Some(AutomationRunStatus::Skipped)));
    if trouble {
        IconName::ClockAlert
    } else if automation.automation.paused {
        IconName::AlarmClockOff
    } else {
        IconName::AlarmClock
    }
}

/// When a snoozed thread comes back: the time alone today, else the day too.
fn snooze_label(until: i64) -> String {
    let moment = chrono::TimeZone::timestamp_millis_opt(&chrono::Local, until).single().unwrap_or_else(chrono::Local::now);
    let today = moment.date_naive() == chrono::Local::now().date_naive();
    format!("Until {}", if today { moment.format("%-I:%M %p").to_string() } else { moment.format("%a %-I:%M %p").to_string() })
}

/// What a row says under its title, and whether the computer it lives on leads it.
fn meta_line(context: &RowContext, thread: &Thread, project: Option<String>, pause: &Option<String>) -> (bool, String) {
    let members = context.members.get(&thread.id).cloned().unwrap_or_default();
    if members.iter().any(|member| context.blocked.contains(member)) {
        return (false, MEMBER_BLOCKED_LABEL.into());
    }
    if let Some(headline) = row_headline(thread, context.waiting.get(&thread.id).cloned()) {
        return (false, headline);
    }
    if let Some(pause) = pause {
        return (true, pause.clone());
    }
    let when = match thread.snoozed_until {
        Some(until) => snooze_label(until),
        None => recency_label(thread.ranked_at, context.now.get()),
    };
    (true, [project, Some(when)].into_iter().flatten().collect::<Vec<_>>().join(" · "))
}

/// A computer named in a row's meta line: a small screen ahead of its name, dark while it is away (HostMark.tsx).
fn host_meta(host: &ThreadHost, rest: &str, t: &act_ui::theme::Tokens, window: &mut Window, cx: &mut App) -> gpui::Div {
    let glyph = if host.offline {
        let warning = t.warning;
        let mark = move || icon(IconName::MonitorOff, 12.0, warning).into_any_element();
        if host.reconnecting {
            let mut style = gpui::StyleRefinement::default();
            style.size.width = Some(px(12.0).into());
            style.size.height = Some(px(12.0).into());
            act_ui::components::spinner::pulse(SharedString::from(format!("reconnecting-{}", host.name)), style, std::time::Duration::from_millis(1400), 14, 0.55, window, cx, mark)
        } else {
            mark()
        }
    } else {
        act_ui::Icon::new(IconName::Monitor).size(px(12.0)).into_any_element()
    };
    let text = if rest.is_empty() { host.name.clone() } else { format!("{} · {rest}", host.name) };
    let state = host.offline.then_some(if host.reconnecting { "Reconnecting" } else { "Offline" });
    div().flex().items_center().gap(px(4.0)).child(div().flex_none().when_some(state, |mark, state| mark.accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, state))).child(glyph)).child(div().flex_1().min_w_0().ellipsis().child(text))
}

fn away_label(host: &ThreadHost) -> String {
    format!("{} is {}", host.name, if host.reconnecting { "reconnecting" } else { "offline" })
}

fn role_icon(role: ThreadRole) -> Option<(IconName, &'static str)> {
    match role {
        ThreadRole::Coordinator => Some((IconName::Waypoints, "Coordinator")),
        ThreadRole::Implementer => Some((IconName::Hammer, "Implementer")),
        ThreadRole::Reviewer => Some((IconName::SearchCheck, "Reviewer")),
        ThreadRole::Researcher => Some((IconName::FlaskConical, "Researcher")),
    }
}

pub fn row(root: &mut Root, context: &RowContext, thread: &Thread, dismiss: bool, priority: bool, _window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let active = context.current.as_deref() == Some(thread.id.as_str());
    let project = thread.project_id.as_ref().and_then(|id| context.projects.iter().find(|project| &project.id == id)).map(project_name);
    let pause = thread.limit_pause.as_ref().map(|pause| pause_summary(pause, context.limit_positions.get(&thread.id).copied(), context.now.get()));
    let status = status(context, thread, &pause);
    let (hosted, meta) = meta_line(context, thread, project, &pause);
    let host = context.hosts.get(&thread.id);
    let away = host.is_some_and(|host| host.offline);
    let id = thread.id.clone();
    let group: SharedString = format!("row-{}", thread.id).into();
    let editing = root.renaming.as_deref() == Some(thread.id.as_str());
    let attention = matches!(status, Some(Status::Dot { .. }));

    let mut marks: Vec<(gpui::AnyElement, SharedString, &str)> = Vec::new();
    let slot = |child: gpui::AnyElement, tip: SharedString, key: &'static str| (child, tip, key);
    if let Some(host) = host.filter(|_| away) {
        marks.push(slot(icon(IconName::MonitorOff, 14.0, t.warning).into_any_element(), away_label(host).into(), "away"));
    }
    if let Some((glyph, label)) = thread.role.and_then(role_icon) {
        let color = match thread.role {
            Some(ThreadRole::Coordinator) => t.role_coordinator,
            Some(ThreadRole::Implementer) => t.role_implementer,
            Some(ThreadRole::Reviewer) => t.role_reviewer,
            _ => t.role_researcher,
        };
        marks.push(slot(icon(glyph, 14.0, color).into_any_element(), label.into(), "role"));
    }
    if context.worktree_ids.contains(&thread.id) {
        let worktree = thread.worktree_id.as_ref().and_then(|id| context.worktrees.get(id));
        let color = worktree.map(|worktree| t.worktree[worktree_hue(&worktree.id).rem_euclid(6) as usize]).unwrap_or(t.quiet_strong);
        let label = format!("Works in {}", worktree.map(worktree_name).unwrap_or_else(|| "a worktree".into()));
        marks.push(slot(icon(IconName::FolderSymlink, 14.0, color).into_any_element(), label.into(), "worktree"));
    }
    if let Some(schedule) = context.schedules.get(&thread.id) {
        let trouble = schedule_icon(schedule) == IconName::ClockAlert;
        marks.push(slot(icon(schedule_icon(schedule), 14.0, if trouble { t.warning } else { t.quiet_strong }).into_any_element(), schedule_label(schedule).into(), "automation"));
    }
    if let Some(status) = &status {
        let (mark, label): (gpui::AnyElement, String) = match status {
            Status::Working => (super::spinner::bead(format!("spin-{}", thread.id), 14.0, t, false).into_any_element(), "Working".into()),
            Status::Paused(label) => (icon(IconName::Hourglass, 14.0, t.warning).into_any_element(), label.clone()),
            Status::Monitoring => (icon(IconName::Activity, 14.0, t.info).into_any_element(), "Monitoring".into()),
            Status::Dot { tone, label } => (attention_dot(*tone, active, t).into_any_element(), label.clone()),
        };
        marks.push(slot(mark, label.into(), "status"));
    }
    if thread.engine.is_known() {
        let engine_group = group.clone();
        let engine = div()
            .id("engine")
            .flex()
            .transition(gpui::Transition::ease(120))
            .text_color(if active { t.muted } else { t.quiet_soft })
            .group_hover(engine_group, |style| style.text_color(t.muted))
            .child(act_ui::Icon::new(engine_icon(thread.engine)).size(px(14.0)));
        marks.push(slot(engine.into_any_element(), format!("{} thread", engine_label(thread.engine)).into(), "engine"));
    }
    let marks_count = marks.len();

    let focus = root.sidebar.row_focus.entry(thread.id.clone()).or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
    if !editing && root.sidebar.refocus_row.as_deref() == Some(thread.id.as_str()) {
        root.sidebar.refocus_row = None;
        _window.focus(&focus);
    }
    let focus_ring = focus.is_focused(_window) && act_ui::focus::keyboard_focus_visible(cx);
    // The row's own buttons show while the keyboard is on them too, as `:focus-within` shows them.
    let more_focus = _window.use_keyed_state(SharedString::from(format!("{}-more-focus", thread.id)), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
    let dismiss_focus = _window.use_keyed_state(SharedString::from(format!("{}-dismiss-focus", thread.id)), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
    let actions_focused = more_focus.is_focused(_window) || dismiss_focus.is_focused(_window);
    let menu_open = root.menus.is_open(&format!("task:{}", thread.id)) || focus_ring || actions_focused;
    // Layout never sees hover styles, so the rail widens from the hovered row held in state.
    let revealed = menu_open || root.sidebar.hovered_row.as_deref() == Some(thread.id.as_str());
    let menu_id = id.clone();
    let more_box = _window.use_keyed_state(SharedString::from(format!("{}-more-box", thread.id)), cx, |_, _| act_ui::overlay::bounds_cell()).read(cx).clone();
    let menu_box = more_box.clone();
    // The menu opens under the button, from its left edge.
    let open_menu = cx.listener(move |root, event: &gpui::ClickEvent, window, cx| {
        cx.stop_propagation();
        let position = match menu_box.get() {
            Some(button) => point(button.left(), button.bottom() + px(4.0)),
            None => point(event.position().x, event.position().y + px(4.0)),
        };
        root.open_thread_menu(&menu_id, position, priority, window, cx);
    });
    let more = div()
        .id(SharedString::from(format!("{}-more", thread.id)))
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, format!("More options for {}", thread.title)))
        .track_focus(&more_focus)
        .focus_ring(true)
        .relative()
        .size(px(28.0))
        .m(px(-3.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .text_color(t.muted_strong)
        .hover(|style| style.bg(t.wash).text_color(t.ink))
        .cursor_pointer()
        .child(ink_icon(IconName::Ellipsis, 16.0))
        .child(act_ui::overlay::bounds_probe(&more_box))
        .tip("More")
        .on_click(open_menu);
    let dismiss_id = id.clone();
    let dismiss_label = if context.schedules.contains_key(&thread.id) { format!("Dismiss {}, which keeps running on its schedule", thread.title) } else { format!("Dismiss {}", thread.title) };
    let actions = div()
        .id("actions")
        .transition(gpui::Transition::ease(90))
        .absolute()
        .top_0()
        .right_0()
        .h(px(m.row_slot))
        .flex()
        .items_center()
        .when(!menu_open, |actions| actions.opacity(0.0).group_hover(group.clone(), |style| style.opacity(1.0)))
        .child(more)
        .when(dismiss, |actions| {
            actions.child(
                div()
                    .id(SharedString::from(format!("{}-dismiss", thread.id)))
                    .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, dismiss_label.clone()))
                    .track_focus(&dismiss_focus)
                    .focus_ring(true)
                    .size(px(28.0))
                    .m(px(-3.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .text_color(t.muted_strong)
                    .hover(|style| style.bg(t.wash).text_color(t.info))
                    .cursor_pointer()
                    .child(ink_icon(IconName::Check, 14.0))
                    .tip("Dismiss")
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        dispatch(AppCommand::TaskDismiss { task_id: Some(dismiss_id.clone()) }, cx);
                    }),
            )
        });
    let covered = if away { 0 } else if dismiss { 2 } else { 1 };
    let mark_group = group.clone();
    let thread_id = thread.id.clone();
    let marks_layer = div().flex().items_center().justify_end().children(marks.into_iter().enumerate().map(move |(index, (mark, tip, key))| {
        let hidden = index + covered >= marks_count;
        // The row's buttons lie over a covered mark wherever the pointer could reach it.
        let mark = div().id(SharedString::from(format!("{thread_id}-{key}"))).size(px(m.row_slot)).flex().items_center().justify_center().child(mark);
        let mark = if hidden { mark.accessible_name(tip) } else { mark.tip(tip) };
        div()
            .id(index)
            .when(hidden, |slot| slot.transition(gpui::Transition::ease(90).only(&[gpui::TransitionProperty::Opacity])))
            .when(hidden && menu_open, |slot| slot.opacity(0.0))
            .when(hidden, |slot| slot.group_hover(mark_group.clone(), |style| style.opacity(0.0)))
            .child(mark)
            .into_any_element()
    }));
    let rail = div()
        .relative()
        .flex_none()
        .h(px(m.row_slot))
        .flex()
        .items_center()
        .justify_end()
        .min_w(px(m.row_slot * if revealed { covered.max(1) } else { 1 } as f32))
        .child(marks_layer)
        .when(!away, |rail| rail.child(actions));

    let title_color = match (active, attention, away) {
        (_, true, _) => t.ink,
        (true, _, true) => t.muted_strong,
        (true, _, false) => t.ink,
        (false, _, true) => t.quiet,
        (false, _, false) => t.ink_faint,
    };
    let meta: gpui::AnyElement = match host.filter(|_| hosted) {
        Some(host) => host_meta(host, &meta, t, _window, cx).into_any_element(),
        None => meta.into_any_element(),
    };
    let text = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .id(SharedString::from(format!("{}-title", thread.id)))
                .w_full()
                .ellipsis()
                .text_size(px(m.text_control))
                .line_height(relative(1.3))
                .text_color(title_color)
                .when(attention, |title| title.font_weight(FontWeight(560.0)))
                .child(thread.title.clone())
                .tip_overflow(thread.title.clone()),
        )
        .child(
            div()
                .mt(px(3.0))
                .w_full()
                .ellipsis()
                .text_size(px(m.text_meta))
                .text_color(if active { t.muted_strong } else { t.frame_meta })
                .when(!away, |meta| meta.group_hover(group.clone(), |style| style.text_color(t.quiet_strong)))
                .child(meta),
        );

    let select_id = id.clone();
    let rename_id = id.clone();
    let context_id = id.clone();
    let body = div()
        .id(SharedString::from(format!("row-body-{}", thread.id)))
        // The row's entry takes the keyboard; this reads as where it is: a group of its title, meta
        // line, marks and buttons.
        .skip_tab_stop()
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, "").selected(active).active(focus.is_focused(_window)))
        .flex()
        .items_center()
        .gap(px(10.0))
        .pl(px(12.0))
        .pr(px(m.row_inset))
        .py(px(5.0))
        .rounded(px(8.0))
        .when(!away, |body| body.cursor_pointer())
        .map(|body| if active { rimmed(body, t, 8.0).bg(t.frame_row_selected) } else if away { body } else { body.hover(|style| style.bg(t.frame_row_hover)) })
        .on_click(cx.listener(move |root, event: &gpui::ClickEvent, _, cx| {
            if event.click_count() == 2 && !away {
                root.start_rename(&select_id, cx);
                return;
            }
            if event.click_count() == 1 {
                dispatch(AppCommand::TaskSelect { task_id: select_id.clone() }, cx);
            }
        }))
        .on_mouse_down(MouseButton::Right, cx.listener(move |root, event: &gpui::MouseDownEvent, window, cx| {
            if away {
                dispatch(AppCommand::TaskSelect { task_id: context_id.clone() }, cx);
            } else {
                root.open_thread_menu(&context_id, event.position, priority, window, cx);
            }
        }))
        .when(!editing, |body| body.child(text).child(rail))
        // The rail's empty track keeps its gap beside the field.
        .when(editing, |body| body.child(div().flex_1().min_w_0().mr(px(10.0)).flex().child(root.rename_field(&rename_id, cx))));
    let role_bar = thread.role.map(|role| {
        let color = match role {
            ThreadRole::Coordinator => t.role_coordinator,
            ThreadRole::Implementer => t.role_implementer,
            ThreadRole::Reviewer => t.role_reviewer,
            ThreadRole::Researcher => t.role_researcher,
        };
        div().absolute().top(px(10.0)).bottom(px(10.0)).left(px(3.0)).w(px(2.0)).rounded(px(2.0)).bg(act_ui::color::alpha(color, 0.7))
    });
    let key_id = id.clone();
    let hover_id = id.clone();
    div()
        .id(SharedString::from(format!("entry-{}", thread.id)))
        .group(group)
        .relative()
        // `content-visibility: auto` paint-contains the entry, so Chromium paints it from a whole pixel.
        .snap_origin()
        .rounded(px(10.0))
        .when(!context.inert, |entry| entry.track_focus(&focus))
        .on_hover(cx.listener(move |root, hovered: &bool, _, cx| {
            let next = if *hovered { Some(hover_id.clone()) } else { root.sidebar.hovered_row.clone().filter(|row| *row != hover_id) };
            if root.sidebar.hovered_row != next {
                root.sidebar.hovered_row = next;
                cx.notify();
            }
        }))
        .on_key_down(cx.listener(move |root, event: &gpui::KeyDownEvent, window, cx| {
            if !root.sidebar.row_focus.get(&key_id).is_some_and(|focus| focus.is_focused(window)) {
                return;
            }
            let keys = &event.keystroke;
            if keys.modifiers.platform || keys.modifiers.control || keys.modifiers.alt {
                return;
            }
            if keys.key == "f10" && keys.modifiers.shift && !away {
                // The context-menu key opens the row's menu under the row.
                let row = root.sidebar.focused_probe.get().unwrap_or_default();
                root.open_thread_menu(&key_id, point(row.left() + px(12.0), row.bottom() + px(4.0)), priority, window, cx);
                cx.stop_propagation();
                return;
            }
            match keys.key.as_str() {
                "enter" | "space" => dispatch(AppCommand::TaskSelect { task_id: key_id.clone() }, cx),
                "f2" if !away => root.start_rename(&key_id, cx),
                _ => return,
            }
            cx.stop_propagation();
        }))
        .child(body)
        .children(role_bar)
        .when(focus_ring, |entry| entry.child(div().absolute().inset_0().rounded(px(10.0)).border_2().border_color(t.focus_ring)))
        .when(active, |entry| entry.child(act_ui::overlay::bounds_probe(&root.sidebar.active_probe)))
        .when(focus.is_focused(_window), |entry| entry.child(act_ui::overlay::bounds_probe(&root.sidebar.focused_probe)))
}

fn attention_dot(tone: Option<Tone>, active: bool, t: &act_ui::theme::Tokens) -> impl IntoElement {
    let toward_ink = |color: Rgba| if active { act_ui::color::mix(color, 0.75, t.ink) } else { color };
    match tone {
        Some(Tone::Approval) => div().size(px(10.0)).rounded_full().border(px(2.0)).border_color(toward_ink(t.accent_text)),
        Some(Tone::Failed) => div().size(px(7.0)).rounded(px(1.5)).bg(toward_ink(t.danger)),
        Some(Tone::Stopped) => div().size(px(8.0)).rounded_full().bg(t.muted_strong),
        _ => div().size(px(8.0)).rounded_full().bg(toward_ink(t.info)),
    }
}

/// The thread actions from its sidebar row and its heading (thread-menu.ts).
pub fn thread_menu_entries(state: &WorkspaceState, thread: &Thread, priority: bool, on_rename: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static) -> Vec<MenuEntry> {
    let id = thread.id.clone();
    let leads: Vec<&Thread> = state.threads.iter().map(|lead| &**lead).filter(|lead| lead.role == Some(ThreadRole::Coordinator) && lead.archived_at.is_none() && lead.id != thread.id).collect();
    let lead = lead_id_of(thread).and_then(|lead_id| leads.iter().find(|lead| lead.id == lead_id)).copied();
    let under = lead.map(|lead| lead.title.clone()).or_else(|| thread.remote_lead.as_ref().map(|lead| lead.title.clone()));
    let mut entries: Vec<MenuEntry> = vec![MenuItem::new("Rename").on_select(on_rename).into()];
    if thread.snoozed_until.is_some() {
        let id = id.clone();
        entries.push(MenuItem::new("Unsnooze").on_select(move |_, cx| dispatch(AppCommand::TaskUnsnooze { task_id: id.clone() }, cx)).into());
    }
    if priority {
        let options = SNOOZE_OPTIONS
            .iter()
            .map(|(label, hours)| {
                let id = id.clone();
                let hours = *hours;
                MenuItem::new(*label).on_select(move |_, cx| dispatch(AppCommand::TaskSnooze { task_id: id.clone(), hours }, cx)).into()
            })
            .collect();
        entries.push(MenuItem::new("Snooze").submenu(options).into());
    }
    let mut roles: Vec<MenuEntry> = THREAD_ROLES
        .iter()
        .map(|role| {
            let id = id.clone();
            let role = *role;
            MenuItem::new(role.label()).checked(thread.role == Some(role)).on_select(move |_, cx| send(json!({ "type": "task.set-role", "taskId": id, "role": role }), cx)).into()
        })
        .collect();
    let none_id = id.clone();
    roles.push(MenuItem::new("None").checked(thread.role.is_none()).on_select(move |_, cx| send(json!({ "type": "task.set-role", "taskId": none_id, "role": null }), cx)).into());
    entries.push(MenuItem::new("Role").submenu(roles).into());
    if thread.role == Some(ThreadRole::Coordinator) {
        let stoppable = act_core::workspace::coordination::stoppable_coordinators(state).contains(&thread.id);
        let stop_id = id.clone();
        entries.push(MenuItem::new("Stop all").disabled(!stoppable).on_select(move |_, cx| dispatch(AppCommand::CoordinationStopAll { task_id: stop_id.clone() }, cx)).into());
        let mute_id = id.clone();
        let muted = thread.muted;
        let mut mute = MenuItem::new(if muted { "Unmute notifications" } else { "Mute notifications" }).on_select(move |_, cx| dispatch(AppCommand::CoordinationSetMuted { task_id: mute_id.clone(), muted: !muted }, cx));
        if !state.notifications {
            mute = mute.disabled(true).tip("Notifications are off in Settings");
        }
        entries.push(mute.into());
    } else {
        if !leads.is_empty() {
            let items = leads
                .iter()
                .map(|candidate| {
                    let id = id.clone();
                    let lead_id = candidate.id.clone();
                    MenuItem::new(candidate.title.clone()).checked(lead.is_some_and(|lead| lead.id == candidate.id)).on_select(move |_, cx| dispatch(AppCommand::TaskSetCoordinator { task_id: id.clone(), coordinator_id: Some(lead_id.clone()) }, cx)).into()
                })
                .collect();
            entries.push(MenuItem::new("Move under").submenu(items).into());
        }
        if let Some(under) = under {
            let id = id.clone();
            entries.push(MenuItem::new(format!("Move out of {under}")).on_select(move |_, cx| dispatch(AppCommand::TaskSetCoordinator { task_id: id.clone(), coordinator_id: None }, cx)).into());
        }
    }
    entries.push(MenuEntry::Separator);
    let link_id = id.clone();
    entries.push(MenuItem::new("Copy link").on_select(move |_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(act_core::workspace::handles::thread_link(&link_id)))).into());
    entries.push(MenuEntry::Separator);
    let fork_id = id.clone();
    entries.push(MenuItem::new("Fork").on_select(move |_, cx| send(json!({ "type": "view.fork-name", "sourceId": fork_id }), cx)).into());
    let fork_worktree_id = id.clone();
    entries.push(MenuItem::new("Fork into a new worktree").on_select(move |_, cx| send(json!({ "type": "view.fork-name", "sourceId": fork_worktree_id, "worktree": true }), cx)).into());
    entries.push(MenuEntry::Separator);
    let archive_id = id.clone();
    entries.push(MenuItem::new("Archive…").danger(true).on_select(move |_, cx| dispatch(AppCommand::ArchivePrompt { thread_id: Some(archive_id.clone()) }, cx)).into());
    entries
}

/// Every other project, and no project at all, for the row menu's Move to.
pub fn move_entries(state: &WorkspaceState, thread: &Thread) -> Option<MenuEntry> {
    let mut items: Vec<MenuEntry> = state
        .projects
        .iter()
        .filter(|project| !project.unlisted && Some(&project.id) != thread.project_id.as_ref())
        .map(|project| {
            let id = thread.id.clone();
            let project_id = project.id.clone();
            MenuItem::new(project_name(project)).on_select(move |_, cx| send(json!({ "type": "task.move", "taskId": id, "target": { "projectId": project_id, "index": 0 } }), cx)).into()
        })
        .collect();
    if thread.project_id.is_some() {
        let id = thread.id.clone();
        items.push(MenuItem::new("No project").on_select(move |_, cx| send(json!({ "type": "task.move", "taskId": id, "target": { "projectId": null, "index": 0 } }), cx)).into());
    }
    if items.is_empty() || thread.worktree_id.is_some() || lead_id_of(thread).is_some() {
        return None;
    }
    Some(MenuItem::new("Move to").submenu(items).into())
}

impl Root {
    pub fn open_thread_menu(&mut self, thread_id: &str, position: gpui::Point<gpui::Pixels>, priority: bool, window: &mut Window, cx: &mut Context<Root>) {
        let local = self.workspace.read(cx).state.clone();
        // A paired computer's thread is offered what its own computer holds, and stays where it is.
        let (state, remote) = match local.thread(thread_id) {
            Some(_) => (local.clone(), false),
            None => match local.computers.paired.iter().filter_map(|computer| computer.state.clone()).find(|held| held.thread(thread_id).is_some()) {
                Some(held) => (std::rc::Rc::new(*held), true),
                None => return,
            },
        };
        let Some(thread) = state.thread(thread_id) else { return };
        let rename_id = thread.id.clone();
        let weak = cx.entity().downgrade();
        let mut entries = thread_menu_entries(&state, thread, priority, move |_, cx| {
            if let Some(root) = weak.upgrade() {
                let id = rename_id.clone();
                root.update(cx, |root, cx| root.start_rename(&id, cx));
            }
        });
        if let Some(MenuEntry::Item(rename)) = entries.first_mut() {
            rename.shortcut = Some("F2".into());
        }
        if let Some(move_to) = move_entries(&state, thread).filter(|_| !remote) {
            let at = entries.iter().position(|entry| matches!(entry, MenuEntry::Separator)).unwrap_or(entries.len());
            entries.insert(at, move_to);
        }
        let menu = Menu::build(entries, Placement::At { position }, window, cx, |menu| menu.from_frame());
        self.menus.show(&format!("task:{thread_id}"), menu, cx);
    }
}
