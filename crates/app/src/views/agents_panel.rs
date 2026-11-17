//! The subagents a thread started, filtered by status and searched, and one of them opened to read
//! its prompt and what it did (SubagentList.tsx, SubagentInspector.tsx, agents-panel.css).

use std::collections::HashSet;

use act_core::domain::engine::effort_label;
use act_core::domain::run::AgentEffort;
use act_core::domain::run::{order_subagents, subagent_line, ActivityKind, Subagent, SubagentGroup, SubagentGroups, SubagentStatus};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant, Segmented, SegmentedOption, SegmentedVariant};
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::empty_state::EmptyState;
use act_ui::components::field::SearchField;
use act_ui::components::list::{ListHeading, ListRow};
use act_ui::components::panel_header::PanelHeader;
use act_ui::input::{InputEvent, TextInput};
use act_ui::theme::{ActiveTheme, Tokens, Tone};
use act_ui::IconName;
use gpui::{div, prelude::*, px, relative, AnyElement, Context, Entity, FontWeight, Rgba, SharedString, Subscription, Window};

use super::common::{count_chip, icon, ink_icon, rimmed};
use super::root::Root;
use crate::workspace::dispatch;

const STATUS_FILTERS: [SubagentStatus; 5] = [SubagentStatus::Working, SubagentStatus::Idle, SubagentStatus::Failed, SubagentStatus::Stopped, SubagentStatus::Completed];
/// How much of a log opens with the subagent; the rest is read backwards, each window twice the last.
const TAIL: usize = 60;

#[derive(Default)]
pub struct AgentsState {
    status: Option<SubagentStatus>,
    search: Option<(Entity<TextInput>, Subscription)>,
    /// The subagent being read, dropped when its thread or the panel goes.
    pub inspected: Option<String>,
    thread: Option<String>,
    limit: usize,
    open_tools: HashSet<String>,
    prompt_open: bool,
}

impl AgentsState {
    /// Lets the inspected subagent go when it, its thread or the panel is no longer there.
    pub fn sync(&mut self, state: &WorkspaceState) {
        if self.thread != state.current_id {
            self.thread = state.current_id.clone();
            self.inspected = None;
            self.status = None;
        }
        let (_, dock) = act_core::state::front_dock(state);
        let held = state.current_id.as_ref().and_then(|id| state.subagents.get(id));
        if self.inspected.as_ref().is_some_and(|id| !dock.panels.iter().any(|panel| panel == "agents") || !held.is_some_and(|list| list.iter().any(|subagent| &subagent.id == id))) {
            self.inspected = None;
        }
    }
}

impl Root {
    /// Opens one subagent's details, asking for its log and settings when they are missing.
    pub fn inspect_subagent(&mut self, id: &str, cx: &mut Context<Root>) {
        self.dock.agents.thread = self.state(cx).current_id.clone();
        self.dock.agents.inspected = Some(id.to_string());
        self.dock.agents.limit = TAIL;
        self.dock.agents.open_tools.clear();
        self.dock.agents.prompt_open = false;
        dispatch(AppCommand::InspectSubagent { task_id: None, subagent_id: id.to_string() }, cx);
        cx.notify();
    }
}

fn status_label(status: SubagentStatus) -> &'static str {
    status.label()
}

pub(super) fn status_glyph(status: SubagentStatus) -> IconName {
    match status {
        SubagentStatus::Working => IconName::CircleDot,
        SubagentStatus::Idle => IconName::CirclePause,
        SubagentStatus::Completed => IconName::CircleCheckBig,
        SubagentStatus::Failed => IconName::CircleX,
        SubagentStatus::Stopped => IconName::CircleStop,
    }
}

pub(super) fn status_ink(status: SubagentStatus, t: &Tokens) -> Rgba {
    match status {
        SubagentStatus::Working => t.info,
        SubagentStatus::Idle => t.quiet_strong,
        SubagentStatus::Completed => t.success,
        SubagentStatus::Failed | SubagentStatus::Stopped => t.danger,
    }
}

fn group_of(status: SubagentStatus) -> SubagentGroup {
    match status {
        SubagentStatus::Working => SubagentGroup::Working,
        SubagentStatus::Idle => SubagentGroup::Idle,
        SubagentStatus::Completed => SubagentGroup::Completed,
        SubagentStatus::Failed => SubagentGroup::Failed,
        SubagentStatus::Stopped => SubagentGroup::Stopped,
    }
}

/// A status glyph in `ink`; a working one pulses (`.agent-working-icon`: 1.4s in 14 steps).
pub(super) fn status_icon(id: impl Into<SharedString>, glyph: IconName, size: f32, ink: Rgba, working: bool, window: &mut Window, cx: &mut gpui::App) -> AnyElement {
    if !working {
        return icon(glyph, size, ink).into_any_element();
    }
    let mut style = gpui::StyleRefinement::default();
    style.size.width = Some(px(size).into());
    style.size.height = Some(px(size).into());
    style.flex_shrink = Some(0.0);
    act_ui::components::spinner::pulse(id.into(), style, std::time::Duration::from_millis(1400), 14, 0.45, window, cx, move || icon(glyph, size, ink).into_any_element())
}

/// A list row's status glyph: in the row's ink, except a working one, which is info and pulses.
pub(super) fn row_status_icon(id: impl Into<SharedString>, status: SubagentStatus, window: &mut Window, cx: &mut gpui::App) -> AnyElement {
    if status == SubagentStatus::Working {
        let ink = cx.theme().tokens.info;
        return status_icon(id, status_glyph(status), 16.0, ink, true, window, cx);
    }
    ink_icon(status_glyph(status), 16.0).into_any_element()
}

/// A 14px `<summary>`'s marker as Chromium draws it: a filled triangle in the text colour, pointing
/// down once open, in a box as wide as the marker and the space after it.
fn disclosure_marker(open: bool) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let (side, height) = (7.74, 9.0);
            let left = bounds.origin.x;
            let middle = bounds.origin.y + bounds.size.height / 2.0;
            let at = |x: f32, y: f32| gpui::point(left + px(x), middle + px(y));
            let mut triangle = gpui::PathBuilder::fill();
            if open {
                triangle.move_to(at(0.0, -height / 2.0));
                triangle.line_to(at(height, -height / 2.0));
                triangle.line_to(at(height / 2.0, side - height / 2.0));
            } else {
                triangle.move_to(at(0.0, -height / 2.0));
                triangle.line_to(at(side, 0.0));
                triangle.line_to(at(0.0, height / 2.0));
            }
            triangle.close();
            if let Ok(path) = triangle.build() {
                let color = window.text_style().color;
                window.paint_path(path, color);
            }
        },
    )
    .flex_none()
    .mt(px(1.0))
    .w(px(14.83))
    .h(px(9.0))
}

/// The round mark a subagent is drawn with, tinted by where it stands.
pub fn orb(status: SubagentStatus, size: f32, glyph_size: f32, t: &Tokens) -> impl IntoElement {
    let (fill, ink) = match status {
        SubagentStatus::Working => (t.info_surface, t.info),
        SubagentStatus::Idle => (t.control, t.quiet_strong),
        SubagentStatus::Completed => (t.success_surface, t.success),
        SubagentStatus::Failed | SubagentStatus::Stopped => (t.danger_chip, t.danger),
    };
    rimmed(div().flex_none().size(px(size)).flex().items_center().justify_center().rounded_full().bg(fill), t, size / 2.0).child(icon(IconName::Bot, glyph_size, ink))
}

fn matches(subagent: &Subagent, status: Option<SubagentStatus>, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    status.is_none_or(|status| subagent.status == status)
        && (needle.is_empty() || format!("{} {} {}", subagent.description, subagent.last_tool_name.clone().unwrap_or_default(), subagent.summary.clone().unwrap_or_default()).to_lowercase().contains(&needle))
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let subagents = state.current_id.as_ref().and_then(|id| state.subagents.get(id)).cloned().unwrap_or_default();
    if let Some(id) = root.dock.agents.inspected.clone() {
        if let Some(subagent) = subagents.iter().find(|subagent| subagent.id == id) {
            return inspector(root, subagent, window, cx);
        }
    }
    list(root, state, &subagents, window, cx)
}

fn list(root: &mut Root, state: &WorkspaceState, subagents: &[Subagent], window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    if subagents.is_empty() {
        return div()
            .id("agents-panel")
            .size_full()
            .flex()
            .flex_col()
            .child(PanelHeader::new("agents-header", "Subagents"))
            .child(div().flex_1().flex().items_center().justify_center().child(EmptyState::new().icon(IconName::Bot).title("No subagents yet")))
            .into_any_element();
    }
    let search = root.dock.agents.search.get_or_insert_with(|| {
        let input = cx.new(|cx| TextInput::new(cx).placeholder("Search subagents"));
        let subscription = cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        });
        (input, subscription)
    });
    let input = search.0.clone();
    let query = input.read(cx).text().to_string();
    let status = root.dock.agents.status;
    let finding = state.find.as_ref().is_some_and(|find| matches!(&find.target, act_core::domain::find::FindTarget::Panel { panel, .. } if panel == "agents"));
    let groups = if finding { SubagentGroups::default() } else { state.subagent_groups };

    let mut counts = [0usize; 5];
    for subagent in subagents {
        if let Some(at) = STATUS_FILTERS.iter().position(|status| *status == subagent.status) {
            counts[at] += 1;
        }
    }
    let mut options = vec![SegmentedOption::new("all", "All").count(subagents.len().to_string())];
    for (at, candidate) in STATUS_FILTERS.iter().enumerate() {
        if counts[at] > 0 {
            let mut option = SegmentedOption::new(status_label(*candidate), status_label(*candidate)).count(counts[at].to_string());
            match candidate {
                SubagentStatus::Working => option = option.tone(Tone::Info),
                SubagentStatus::Failed | SubagentStatus::Stopped => option = option.tone(Tone::Danger),
                _ => {}
            }
            options.push(option);
        }
    }
    let strip = Segmented::new("agent-status-strip", options, status.map(status_label).unwrap_or("all")).variant(SegmentedVariant::Pills).on_change(cx.listener(|root, picked: &SharedString, _, cx| {
        let picked = STATUS_FILTERS.iter().find(|status| status_label(**status) == picked.as_ref()).copied();
        root.dock.agents.status = if picked.is_none() || picked == root.dock.agents.status { None } else { picked };
        cx.notify();
    }));
    let clear_input = input.clone();
    let field = div()
        .on_key_down(cx.listener(move |_, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" && !clear_input.read(cx).text().is_empty() {
                cx.stop_propagation();
                clear_input.update(cx, |input, cx| input.reset("", cx));
            }
        }))
        .child(SearchField::new("agents-search", &input).clear_tip(super::chrome::key_tip("Clear", "Escape")).on_clear({
            let input = input.clone();
            move |_, window, cx| {
                input.update(cx, |input, cx| {
                    input.reset("", cx);
                    input.focus(window, cx);
                })
            }
        }));
    let filters = div().flex().flex_col().gap(px(9.0)).border_b_1().border_color(t.line_soft).px(px(14.0)).py(px(12.0)).child(strip).child(field);

    let shown: Vec<Subagent> = order_subagents(subagents).into_iter().filter(|subagent| matches(subagent, status, &query)).collect();
    let body = if shown.is_empty() {
        div()
            .px(px(22.0))
            .py(px(18.0))
            .child(EmptyState::inline().title("No subagents match.").action(Button::new("agents-show-all", "Show all").variant(ButtonVariant::Soft).on_click(cx.listener(|root, _, _, cx| {
                root.dock.agents.status = None;
                if let Some((input, _)) = &root.dock.agents.search {
                    input.update(cx, |input, cx| input.reset("", cx));
                }
                cx.notify();
            }))))
            .into_any_element()
    } else {
        let grouped = status.is_none();
        let mut column = div().flex().flex_col().gap(px(2.0)).p(px(14.0));
        let mut bucket: Option<SubagentStatus> = None;
        for subagent in &shown {
            if grouped && bucket != Some(subagent.status) {
                bucket = Some(subagent.status);
                let group = group_of(subagent.status);
                let open = groups.get(group);
                let count = counts[STATUS_FILTERS.iter().position(|status| *status == subagent.status).unwrap_or(0)];
                column = column.child(div().pb(px(5.0)).child(ListHeading::new(SharedString::from(format!("agents-group-{}", status_label(subagent.status))), status_label(subagent.status)).count(count.to_string()).fold(open, move |_, _, cx| dispatch(AppCommand::SetSubagentGroup { group, open: !open }, cx))));
            }
            if grouped && !groups.get(group_of(subagent.status)) {
                continue;
            }
            let id = subagent.id.clone();
            column = column.child(
                div().pb(px(5.0)).child(
                    ListRow::new(SharedString::from(format!("agent-{}", subagent.id)), subagent.description.clone())
                        .inset(10.0, 24.0)
                        .pad_y(11.0)
                        .leading(orb(subagent.status, 28.0, 12.0, t))
                        .detail(subagent_line(subagent))
                        .trailing(div().flex_none().accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, status_label(subagent.status))).child(row_status_icon(SharedString::from(format!("agent-status-{}", subagent.id)), subagent.status, window, cx)))
                        .on_click(cx.listener(move |root, _, _, cx| root.inspect_subagent(&id, cx))),
                ),
            );
        }
        div().id("agents-list").flex_1().min_h_0().mt(px(8.0)).overflow_y_scroll().child(column).into_any_element()
    };
    let _ = m;
    div()
        .id("agents-panel")
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(PanelHeader::new("agents-header", "Subagents").action(count_chip(subagents.len().to_string(), t)))
        .child(filters)
        .child(body)
        .into_any_element()
}

fn inspector(root: &mut Root, subagent: &Subagent, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let mono = theme.typography.mono_family.clone();
    let limit = root.dock.agents.limit.max(TAIL);
    let start = subagent.activity.len().saturating_sub(limit);
    let shown = subagent.activity.len() - start;
    let status = subagent.status;

    let stop = (status == SubagentStatus::Working).then(|| {
        if subagent.stopping {
            Button::new("subagent-stopping", "Stopping…").variant(ButtonVariant::Danger).disabled(true).icon(IconName::LoaderCircle).icon_size(12.0).into_any_element()
        } else {
            let id = subagent.id.clone();
            ConfirmAction::new("subagent-stop", "Stop", "Stop subagent", move |_, cx| dispatch(AppCommand::RunStopProcess { task_id: None, process_id: id.clone() }, cx)).variant(ButtonVariant::Danger).into_any_element()
        }
    });
    let heading = div()
        .flex()
        .gap(px(10.0))
        .px(px(24.0))
        .pt(px(26.0))
        .pb(px(22.0))
        .child(orb(status, 36.0, 18.0, t))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Heading, "")).mt(px(1.0)).mb(px(8.0)).text_color(t.ink_soft).text_size(px(m.text_heading)).font_weight(FontWeight(580.0)).line_height(relative(1.35)).child(subagent.description.clone()))
                .child(div().flex().items_center().gap(px(6.0)).text_color(status_ink(status, t)).text_size(px(m.text_meta)).child(status_icon("inspector-status", status_glyph(status), 16.0, status_ink(status, t), status == SubagentStatus::Working, window, cx)).child(status_label(status))),
        )
        .children(stop.map(|stop| div().flex_none().mt(px(2.0)).child(stop)));

    let effort = subagent.effort.as_ref().map(|effort| serde_json::from_value::<AgentEffort>(serde_json::Value::String(effort.clone())).map(|effort| effort_label(effort).to_string()).unwrap_or_else(|_| effort.clone()));
    let fact = |label: &'static str, value: String| div().flex().gap(px(12.0)).child(div().w(px(52.0)).flex_none().text_color(t.quiet).child(label)).child(div().flex_1().min_w_0().text_color(t.ink_soft).child(value));
    let prompt_open = root.dock.agents.prompt_open;
    let details = div()
        .px(px(24.0))
        .pb(px(18.0))
        .border_b_1()
        .border_color(t.line_soft)
        .text_size(px(m.text_control))
        .child(div().flex().flex_col().gap(px(8.0)).mb(px(16.0)).child(fact("Model", subagent.model.clone().unwrap_or_else(|| "Not reported".into()))).child(fact("Effort", effort.unwrap_or_else(|| "Not reported".into()))))
        .child(div().flex().child(
            div()
                .id("subagent-prompt-toggle")
                .flex()
                .items_center()
                .min_h(px(28.0))
                .text_color(t.ink_dim)
                .font_weight(FontWeight(560.0))
                .cursor_pointer()
                .hover(|style| style.text_color(t.ink))
                .child(disclosure_marker(prompt_open))
                .child("Prompt")
                .on_click(cx.listener(|root, _, _, cx| {
                    root.dock.agents.prompt_open = !root.dock.agents.prompt_open;
                    cx.notify();
                })),
        ))
        .when(prompt_open, |details| {
            details.child(match &subagent.prompt {
                Some(prompt) => div().mt(px(12.0)).text_color(t.ink_soft).line_height(relative(1.6)).child(prompt.clone()),
                None => div().mt(px(12.0)).text_color(t.quiet).child("Not reported"),
            })
        });

    let summary = (subagent.summary.is_some() || subagent.last_tool_name.is_some() || subagent.total_tokens.is_some()).then(|| {
        div()
            .px(px(24.0))
            .py(px(18.0))
            .border_b_1()
            .border_color(t.line_soft)
            .when_some(subagent.summary.clone(), |block, summary| block.child(div().mb(px(10.0)).text_color(t.muted_strong).text_size(px(m.text_content)).line_height(relative(1.5)).child(summary)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(14.0))
                    .gap_y(px(7.0))
                    .text_color(t.quiet)
                    .text_size(px(m.text_meta))
                    .when_some(subagent.last_tool_name.clone(), |row, tool| row.child(format!("Last tool: {tool}")))
                    .when_some(subagent.total_tokens, |row, tokens| row.child(format!("{} tokens", group_thousands(tokens)))),
            )
    });

    let mut activity = div().px(px(24.0)).pt(px(22.0)).pb(px(32.0)).flex().flex_col();
    if start > 0 {
        let earlier = start.min(limit);
        activity = activity.child(
            div().mb(px(12.0)).child(Button::new("subagent-earlier", format!("Show {earlier} earlier")).icon(IconName::ChevronUp).icon_size(14.0).full_width().on_click(cx.listener(|root, _, _, cx| {
                root.dock.agents.limit = root.dock.agents.limit.max(TAIL) * 2;
                cx.notify();
            }))),
        );
    }
    if shown == 0 {
        activity = activity.child(EmptyState::inline().title("Waiting for activity…"));
    } else {
        for item in &subagent.activity[start..] {
            match item.kind {
                ActivityKind::Tool => {
                    let open = root.dock.agents.open_tools.contains(&item.id);
                    let id = item.id.clone();
                    let text = item.text.clone();
                    activity = activity.child(
                        div()
                            .mb(px(8.0))
                            .rounded(px(9.0))
                            .border_1()
                            .border_color(t.line)
                            .overflow_hidden()
                            .child(
                                div()
                                    .id(SharedString::from(format!("tool-{}", item.id)))
                                    .flex()
                                    .items_center()
                                    .gap(px(7.0))
                                    .min_h(px(32.0))
                                    .px(px(10.0))
                                    .py(px(7.0))
                                    .text_color(t.ink_dim)
                                    .text_size(px(m.text_meta))
                                    .cursor_pointer()
                                    .group("agent-tool")
                                    .hover(|style| style.bg(t.surface_hover))
                                    .when(open, |row| row.border_b_1().border_color(t.line))
                                    .child(div().flex_none().child(icon(IconName::Wrench, 14.0, t.muted)))
                                    .child(div().flex_1().min_w_0().child(item.title.clone().unwrap_or_else(|| "Tool".into())))
                                    .child(super::common::turning("chevron", if open { 90.0 } else { 0.0 }, 140).text_color(t.muted).group_hover("agent-tool", |style| style.text_color(t.ink)).child(ink_icon(IconName::ChevronRight, 14.0)))
                                    .on_click(cx.listener(move |root, _, _, cx| {
                                        if !root.dock.agents.open_tools.remove(&id) {
                                            root.dock.agents.open_tools.insert(id.clone());
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(open, |tool| tool.child(div().relative().bg(t.code_surface).px(px(12.0)).py(px(10.0)).font_family(mono.clone()).text_size(px(m.text_meta)).text_color(t.code_ink).child(text.clone()).child(div().absolute().top(px(4.0)).right(px(4.0)).child(super::timeline_row::copy_button(&format!("tool-copy-{}", item.id), text.clone(), "Copy the output", None, window, cx))))),
                    );
                }
                _ => {
                    activity = activity.child(div().mb(px(16.0)).text_color(t.ink_faint).text_size(px(m.text_content)).line_height(relative(1.6)).child(item.text.clone()));
                }
            }
        }
    }

    let header = PanelHeader::new("subagent-header", "Subagent").close_label("Close subagent details").close_tip(super::chrome::key_tip("Close", "Escape")).on_close(cx.listener(|root, _, _, cx| {
        root.dock.agents.inspected = None;
        cx.notify();
    }));
    let inspector = div()
        .id("subagent-inspector")
        .when(!cx.theme().tokens.reduced_motion, |inspector| inspector.enter(gpui::Transition::ease_out(160), |style| style.opacity(0.0).transform(gpui::ElementTransform::translate(px(18.0), px(0.0)))))
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Complementary, format!("{} details", subagent.description)))
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .on_key_down(cx.listener(|root, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                root.dock.agents.inspected = None;
                cx.notify();
            }
        }))
        .child(header)
        .child(div().id("inspector-scroll").flex_1().min_h_0().overflow_y_scroll().child(heading).child(details).children(summary).child(activity));
    // On the glass sheet the inspector stands a frame gap in from the dock's edge.
    div().size_full().when(t.page.a == 0.0, |frame| frame.pl(px(m.frame_gap))).child(inspector).into_any_element()
}

fn group_thousands(value: i64) -> String {
    let digits = value.abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 {
        format!("-{out}")
    } else {
        out
    }
}
