//! One dynamic workflow: what each phase has left, when each agent ran, and what any one of them
//! was given or came back with (WorkflowPanel.tsx, workflow-panel.css), and the processes list a
//! session keeps running (BackgroundProcessList.tsx).

use act_core::domain::run::{process_label, BackgroundProcess, BackgroundProcessKind};
use act_core::domain::workflow::{agent_state_in, format_elapsed, workflow_agent_counts, workflow_agent_duration, workflow_agent_note, workflow_bar, workflow_groups, workflow_label, workflow_now, workflow_span, workflow_status_label, workflow_ticks, Workflow, WorkflowAgent, WorkflowAgentState, WorkflowIsolation, WorkflowStatus};
use act_core::state::{workflow_by_id, WorkspaceState};
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::components::chip::Chip;
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::empty_state::EmptyState;
use act_ui::components::list::{ListHeading, ListRow};
use act_ui::components::meter::{Meter, MeterSize};
use act_ui::components::panel_header::PanelHeader;
use act_ui::theme::{ActiveTheme, Tokens, Tone};
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, relative, AnyElement, App, Context, FontWeight, Rgba, SharedString, Window};

use super::common::{icon, ink_icon, rimmed, Ellipsis};
use super::root::Root;
use crate::workspace::dispatch;

#[derive(Default)]
pub struct WorkflowState {
    selected: Option<(String, i64)>,
    ticker: Option<gpui::Task<()>>,
}

fn agent_glyph(state: WorkflowAgentState) -> IconName {
    match state {
        WorkflowAgentState::Done => IconName::CircleCheckBig,
        WorkflowAgentState::Error => IconName::CircleX,
        WorkflowAgentState::Queued => IconName::Clock,
        WorkflowAgentState::Stopped => IconName::Square,
        WorkflowAgentState::Running => IconName::CircleDot,
    }
}

fn orb_colors(state: Option<WorkflowAgentState>, t: &Tokens) -> (Rgba, Rgba) {
    match state {
        Some(WorkflowAgentState::Done) => (t.success_surface, t.success),
        Some(WorkflowAgentState::Error) => (t.danger_chip, t.danger),
        Some(WorkflowAgentState::Queued) | Some(WorkflowAgentState::Stopped) => (t.control, t.quiet_strong),
        _ => (t.info_surface, t.info),
    }
}

fn orb(glyph: IconName, state: Option<WorkflowAgentState>, size: f32, glyph_size: f32, t: &Tokens) -> gpui::Div {
    let (fill, ink) = orb_colors(state, t);
    rimmed(div().flex_none().size(px(size)).flex().items_center().justify_center().rounded_full().bg(fill), t, size / 2.0).child(icon(glyph, glyph_size, ink))
}

/// An agent's orb; a running one's glyph pulses (`.agent-working-icon`).
fn agent_orb(id: SharedString, state: WorkflowAgentState, size: f32, glyph_size: f32, t: &Tokens, window: &mut Window, cx: &mut App) -> gpui::Div {
    if state != WorkflowAgentState::Running {
        return orb(agent_glyph(state), Some(state), size, glyph_size, t);
    }
    let (fill, ink) = orb_colors(Some(state), t);
    let glyph = super::agents_panel::status_icon(id, agent_glyph(state), glyph_size, ink, true, window, cx);
    rimmed(div().flex_none().size(px(size)).flex().items_center().justify_center().rounded_full().bg(fill), t, size / 2.0).child(glyph)
}

fn compact(value: i64) -> String {
    let value = value as f64;
    let (scaled, suffix) = if value.abs() >= 1e9 {
        (value / 1e9, "B")
    } else if value.abs() >= 1e6 {
        (value / 1e6, "M")
    } else if value.abs() >= 1e3 {
        (value / 1e3, "K")
    } else {
        return format!("{}", value as i64);
    };
    let rounded = (scaled * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{}{suffix}", rounded as i64)
    } else {
        format!("{rounded:.1}{suffix}")
    }
}

fn badges(agent: &WorkflowAgent, t: &Tokens) -> Vec<AnyElement> {
    let mut chips = Vec::new();
    if let Some(model) = &agent.model {
        chips.push(Chip::tag(model.clone()).into_any_element());
    }
    if let Some(isolation) = agent.isolation {
        chips.push(Chip::tag(match isolation { WorkflowIsolation::Worktree => "worktree", WorkflowIsolation::Remote => "remote" }).tone(Some(Tone::Info)).into_any_element());
    }
    if agent.cached == Some(true) {
        chips.push(Chip::tag("cached").tone(Some(Tone::Success)).into_any_element());
    }
    if let Some(attempt) = agent.attempt.filter(|attempt| *attempt > 1) {
        let chip = div().id(SharedString::from(format!("retry-{}", agent.index))).child(Chip::tag(format!("retry {attempt}")).tone(Some(Tone::Accent)));
        chips.push(match &agent.last_attempt_reason {
            Some(reason) => chip.tip(reason.clone()).into_any_element(),
            None => chip.into_any_element(),
        });
    }
    let _ = t;
    chips
}

fn stop_control(id: &'static str, name: &str, stopping: bool, process: String) -> AnyElement {
    if stopping {
        Button::new(SharedString::from(format!("{id}-stopping")), "Stopping…").variant(ButtonVariant::Danger).disabled(true).into_any_element()
    } else {
        let _ = name;
        ConfirmAction::new(SharedString::from(id), "Stop", if id == "workflow-stop" { "Stop workflow" } else { "Stop subagent" }, move |_, cx| dispatch(AppCommand::RunStopProcess { task_id: None, process_id: process.clone() }, cx)).variant(ButtonVariant::Danger).into_any_element()
    }
}

pub fn render(root: &mut Root, state: &WorkspaceState, workflow_id: Option<&str>, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let Some(workflow) = workflow_by_id(state, workflow_id).cloned() else {
        return div().size_full().p(px(18.0)).child(EmptyState::inline().title("This workflow is no longer running.")).into_any_element();
    };
    let live = workflow.status == WorkflowStatus::Running;
    if live {
        if root.dock.workflow.ticker.is_none() {
            root.dock.workflow.ticker = Some(cx.spawn(async |root, cx| loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                if root.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }));
        }
    } else {
        root.dock.workflow.ticker = None;
    }
    let now = workflow_now(&workflow, act_core::util::now());
    if root.dock.workflow.selected.as_ref().is_some_and(|(id, _)| *id != workflow.id) {
        root.dock.workflow.selected = None;
    }
    if let Some(agent) = root.dock.workflow.selected.as_ref().and_then(|(_, index)| workflow.agents.iter().find(|agent| agent.index == *index)).cloned() {
        return inspector(&workflow, &agent, now, window, cx);
    }
    let groups = workflow_groups(&workflow);
    let (done, failed) = workflow_agent_counts(&workflow);
    let count = workflow.agents.len();
    let mark = orb(IconName::Boxes, if live { None } else if workflow.status == WorkflowStatus::Completed { Some(WorkflowAgentState::Done) } else { Some(WorkflowAgentState::Error) }, 28.0, 16.0, t);
    let mut header = PanelHeader::new("workflow-header", workflow.name.clone()).icon(mark).subtitle(format!("{} · {} {} · {}", workflow_status_label(&workflow), count, if count == 1 { "agent" } else { "agents" }, format_elapsed(now - workflow.started_at)));
    if live {
        header = header.action(stop_control("workflow-stop", &workflow.name, workflow.stopping, workflow.id.clone()));
    }
    let stat = |value: String, label: &'static str, danger: bool| {
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(act_ui::style::tabular_nums(div().text_color(if danger { t.danger } else { t.ink_dim }).text_size(px(theme.metrics.text_content)).font_weight(FontWeight(560.0))).child(value))
            .child(div().text_color(t.quiet_strong).text_size(px(theme.metrics.text_badge)).child(label.to_uppercase()))
    };
    let overview = div()
        .px(px(20.0))
        .pb(px(13.0))
        .border_b_1()
        .border_color(t.line_soft)
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(18.0))
                .mb(px(9.0))
                .child(stat(format!("{done}/{count}"), "Agents", false))
                .child(stat(compact(workflow.total_tokens), "Tokens", false))
                .child(stat(workflow.total_tool_calls.to_string(), "Tool calls", false))
                .when(failed > 0, |stats| stats.child(stat(failed.to_string(), "Failed", true))),
        )
        .child(Meter::new(if count > 0 { done as f32 / count as f32 } else { 0.0 }).size(MeterSize::Hair).tone(Tone::Info));

    let mut body = div().flex().flex_col();
    if workflow.agents.is_empty() {
        body = body.child(EmptyState::inline().title("Waiting for the workflow's first agents…"));
    } else {
        body = body.child(timeline(&workflow, &groups, now, window, cx));
        for group in &groups {
            let finished = group.agents.iter().filter(|agent| matches!(agent.state, WorkflowAgentState::Done | WorkflowAgentState::Error)).count();
            let mut section = div().mt(px(14.0)).flex().flex_col().gap(px(2.0)).child(ListHeading::new(SharedString::from(format!("workflow-group-{}", group.key)), group.title.clone()).count(format!("{finished}/{}", group.agents.len())));
            for agent in &group.agents {
                let state = agent_state_in(&workflow, agent);
                let duration = workflow_agent_duration(agent, now);
                let index = agent.index;
                let id = workflow.id.clone();
                let title = div().flex().items_center().gap(px(6.0)).min_w_0().child(div().min_w_0().ellipsis().child(agent.label.clone())).children(badges(agent, t));
                let _ = title;
                section = section.child(
                    ListRow::new(SharedString::from(format!("workflow-agent-{}", agent.index)), agent.label.clone())
                        .name(format!("Open {} details", agent.label))
                        .leading(agent_orb(SharedString::from(format!("workflow-agent-orb-{}", agent.index)), state, 24.0, 16.0, t, window, cx))
                        .detail(workflow_agent_note(agent, state))
                        .trailing(
                            div()
                                .flex()
                                .flex_col()
                                .items_end()
                                .gap(px(2.0))
                                .text_color(t.quiet)
                                .text_size(px(theme.metrics.text_meta))
                                .map(act_ui::style::tabular_nums)
                                .children(agent.tokens.filter(|tokens| *tokens > 0).map(compact))
                                .children(duration.map(|duration| div().text_color(t.quiet_soft).text_size(px(theme.metrics.text_badge)).child(format_elapsed(duration)))),
                        )
                        .on_click(cx.listener(move |root, _, _, cx| {
                            root.dock.workflow.selected = Some((id.clone(), index));
                            cx.notify();
                        })),
                );
            }
            body = body.child(section);
        }
    }
    if workflow.status != WorkflowStatus::Running {
        if let Some(summary) = &workflow.summary {
            body = body.child(div().mt(px(16.0)).mx(px(4.0)).flex().items_center().gap(px(7.0)).text_color(t.quiet_strong).text_size(px(theme.metrics.text_meta)).when(workflow.status == WorkflowStatus::Failed, |row| row.child(icon(IconName::TriangleAlert, 14.0, t.quiet_strong))).child(summary.clone()));
        }
    }
    div()
        .id("workflow-panel")
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(header)
        .child(overview)
        .child(div().id("workflow-body").flex_1().min_h_0().overflow_y_scroll().px(px(14.0)).pt(px(12.0)).pb(px(20.0)).child(body))
        .into_any_element()
}

/// Every agent on one clock: queue time hollow, run time filled, all lanes from the same origin.
fn timeline(workflow: &Workflow, groups: &[act_core::domain::workflow::WorkflowGroup], now: i64, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let span = workflow_span(workflow, now);
    let ticks = workflow_ticks(span);
    let lane_name = 132.0;
    let mut axis = div().relative().h(px(15.0)).ml(px(lane_name + 8.0)).mr(px(4.0)).border_b_1().border_color(t.line_soft);
    for tick in ticks {
        axis = axis.child(div().absolute().top_0().left(relative(tick.left as f32 / 100.0)).ml(px(-10.0)).text_color(t.quiet_strong).text_size(px(theme.metrics.text_badge)).map(act_ui::style::tabular_nums).child(tick.label));
    }
    let mut column = div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Region, "Workflow timeline")).mb(px(8.0)).child(axis);
    for group in groups {
        let mut lanes = div().mt(px(11.0)).child(div().mb(px(4.0)).text_color(t.quiet_strong).text_size(px(theme.metrics.text_badge)).font_weight(FontWeight(600.0)).child(group.title.to_uppercase()));
        for agent in &group.agents {
            let state = agent_state_in(workflow, agent);
            let bar = workflow_bar(agent, span, now);
            let index = agent.index;
            let id = workflow.id.clone();
            let label = match state {
                WorkflowAgentState::Queued => "queued",
                WorkflowAgentState::Running => "running",
                WorkflowAgentState::Done => "done",
                WorkflowAgentState::Error => "failed",
                WorkflowAgentState::Stopped => "stopped",
            };
            let waited = agent.queued_at.map(|queued| agent.started_at.unwrap_or(now) - queued);
            let ran = workflow_agent_duration(agent, now);
            let mut tip = vec![format!("{} · {label}", agent.label)];
            if let Some(waited) = waited {
                tip.push(format!("waited {}", format_elapsed(waited)));
            }
            if let Some(ran) = ran {
                tip.push(format!("ran {}", format_elapsed(ran)));
            }
            let mut track = div().relative().flex_1().h_full().child(div().absolute().top(px(13.0)).left_0().right_0().h(px(1.0)).bg(t.line_soft));
            if let Some(queue) = bar.queue {
                track = track.child(div().absolute().top(px(12.0)).h(px(4.0)).left(relative(queue.left as f32 / 100.0)).w(relative(queue.width as f32 / 100.0)).border_1().border_dashed().border_color(t.line_strong).rounded(px(3.0)));
            }
            if let Some(run) = bar.run {
                let (fill, edge) = match state {
                    WorkflowAgentState::Done => (super::common::with_alpha(t.success, 0.6), t.success),
                    WorkflowAgentState::Error => (t.danger_line, t.danger),
                    WorkflowAgentState::Stopped => (t.control, t.line_strong),
                    _ => (t.info, t.info),
                };
                let mut placed = div().absolute().top(px(8.0)).h(px(12.0)).left(relative(run.left as f32 / 100.0)).w(relative(run.width as f32 / 100.0));
                if state == WorkflowAgentState::Running {
                    let style = placed.style().clone();
                    let segment = move || div().size_full().bg(fill).border_1().border_color(edge).rounded_l(px(3.0)).into_any_element();
                    track = track.child(act_ui::components::spinner::pulse(SharedString::from(format!("lane-run-{index}")), style, std::time::Duration::from_millis(1600), 16, 0.45, window, cx, segment));
                } else {
                    track = track.child(placed.bg(fill).border_1().border_color(edge).rounded(px(3.0)));
                }
            }
            lanes = lanes.child(
                div()
                    .id(SharedString::from(format!("lane-{}", agent.index)))
                    .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Open {} details, {label}", agent.label)))
                    .group("workflow-lane")
                    .w_full()
                    .h(px(28.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .pr(px(4.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.surface_hover))
                    .child(div().w(px(lane_name)).flex_none().min_w_0().ellipsis().text_color(t.muted_strong).group_hover("workflow-lane", |style| style.text_color(t.ink_dim)).text_size(px(theme.metrics.text_meta)).child(agent.label.clone()))
                    .child(track)
                    .tip(tip.join(" · "))
                    .on_click(cx.listener(move |root, _, _, cx| {
                        root.dock.workflow.selected = Some((id.clone(), index));
                        cx.notify();
                    })),
            );
        }
        column = column.child(lanes);
    }
    column.into_any_element()
}

fn inspector(workflow: &Workflow, agent: &WorkflowAgent, now: i64, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let state = agent_state_in(workflow, agent);
    let duration = workflow_agent_duration(agent, now);
    let waited = match (agent.queued_at, agent.started_at) {
        (Some(queued), Some(started)) => Some(started - queued),
        _ => None,
    };
    let mut facts: Vec<(&str, String)> = Vec::new();
    if let Some(waited) = waited {
        facts.push(("Waited", format_elapsed(waited)));
    }
    if let Some(duration) = duration {
        facts.push(("Ran", format_elapsed(duration)));
    }
    if let Some(tokens) = agent.tokens {
        facts.push(("Tokens", tokens.to_string()));
    }
    if let Some(calls) = agent.tool_calls {
        facts.push(("Tool calls", calls.to_string()));
    }
    if let Some(tool) = &agent.last_tool_name {
        facts.push(("Last tool", tool.clone()));
    }
    if let Some(id) = &agent.agent_id {
        facts.push(("Agent id", id.clone()));
    }
    let heading_label = |text: &str| div().mt(px(18.0)).mb(px(6.0)).text_color(t.quiet_strong).text_size(px(m.text_badge)).font_weight(FontWeight(600.0)).child(text.to_uppercase());
    let preview = |text: String, failed: bool| div().rounded(px(8.0)).border_1().border_color(t.line_soft).px(px(10.0)).py(px(9.0)).bg(t.code_surface).text_color(if failed { t.danger_soft } else { t.code_ink }).font_family(theme.typography.mono_family.clone()).text_size(px(m.text_meta)).line_height(relative(1.55)).child(text);
    let mut body = div()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(11.0))
                .child(agent_orb("inspector-orb".into(), state, 28.0, 16.0, t, window, cx))
                .child(div().flex().flex_col().child(div().text_color(t.ink_soft).text_size(px(m.text_content)).font_weight(FontWeight(560.0)).child(agent.label.clone())).child(div().mt(px(4.0)).text_color(t.quiet).text_size(px(m.text_meta)).child(format!("{}{}", agent.phase_title.as_ref().map(|phase| format!("{phase} · ")).unwrap_or_default(), workflow_agent_note(agent, state))))),
        )
        .child(div().flex().flex_wrap().gap(px(5.0)).mt(px(12.0)).children(badges(agent, t)).children(agent.agent_type.clone().map(|kind| Chip::tag(kind))))
        .child(div().mt(px(15.0)).flex().flex_col().gap(px(6.0)).text_size(px(m.text_meta)).children(facts.into_iter().map(|(label, value)| div().flex().gap(px(14.0)).child(div().w(px(70.0)).text_color(t.quiet).child(label)).child(act_ui::style::tabular_nums(div().text_color(t.ink_faint)).child(value)))));
    if let Some(summary) = &agent.last_tool_summary {
        body = body.child(heading_label("Last tool")).child(preview(summary.clone(), false));
    }
    if let Some(prompt) = &agent.prompt_preview {
        body = body.child(heading_label("Prompt")).child(preview(prompt.clone(), false));
    }
    body = match &agent.error {
        Some(error) => body.child(heading_label("Error")).child(preview(error.clone(), true)),
        None => body.child(heading_label("Result")).child(preview(agent.result_preview.clone().unwrap_or_else(|| if state == WorkflowAgentState::Done { "Returned nothing".into() } else { "Pending".into() }), false)),
    };
    body = body.child(div().mt(px(16.0)).text_color(t.quiet_soft).text_size(px(m.text_badge)).child("Previews are the first 400 characters the workflow reported."));
    div()
        .id("workflow-inspect")
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .on_key_down(cx.listener(|root, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                root.dock.workflow.selected = None;
                cx.notify();
            }
        }))
        .child(
            div().flex().pt(px(14.0)).px(px(14.0)).child(
                Button::new("workflow-back", workflow.name.clone()).variant(ButtonVariant::Ghost).icon(IconName::ArrowLeft).icon_size(16.0).tip(super::chrome::key_tip("Back", "Escape")).on_click(cx.listener(|root, _, _, cx| {
                    root.dock.workflow.selected = None;
                    cx.notify();
                })),
            ),
        )
        .child(div().id("workflow-inspect-body").flex_1().min_h_0().overflow_y_scroll().px(px(16.0)).pt(px(8.0)).pb(px(22.0)).child(body))
        .into_any_element()
}

/// The processes a thread's session keeps running, and the workflows it drove meanwhile.
pub fn processes_section(processes: &[BackgroundProcess], workflows: &[Workflow], cx: &mut App) -> Option<AnyElement> {
    if processes.is_empty() && workflows.is_empty() {
        return None;
    }
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let running = processes.len() + workflows.iter().filter(|workflow| workflow.status == WorkflowStatus::Running).count();
    let mut list = div().flex().flex_col().gap(px(2.0)).mt(px(8.0));
    for workflow in workflows {
        let id = workflow.id.clone();
        let state = if workflow.status == WorkflowStatus::Running { None } else if workflow.status == WorkflowStatus::Completed { Some(WorkflowAgentState::Done) } else { Some(WorkflowAgentState::Error) };
        list = list.child(
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .pr(px(6.0))
                .child(
                    div().flex_1().min_w_0().group("process-open").child(
                        ListRow::new(SharedString::from(format!("process-workflow-{}", workflow.id)), workflow.name.clone())
                            .inset(6.0, 22.0)
                            .leading(orb(IconName::Boxes, state, 22.0, 12.0, t))
                            .detail(workflow_label(workflow))
                            .trailing(div().text_color(t.quiet_strong).group_hover("process-open", |style| style.text_color(t.ink_dim)).child(ink_icon(IconName::ChevronRight, 14.0)))
                            .on_click(move |_, _, cx| dispatch(AppCommand::OpenWorkflow { workflow_id: id.clone() }, cx)),
                    ),
                )
                .when(workflow.status == WorkflowStatus::Running, |row| row.child(stop_process(&workflow.id, &workflow.name, workflow.stopping))),
        );
    }
    for process in processes {
        let glyph = if process.kind == BackgroundProcessKind::Shell { IconName::Terminal } else { IconName::Radio };
        let leading = rimmed(div().flex_none().size(px(22.0)).flex().items_center().justify_center().rounded(px(8.0)).bg(t.info_surface), t, 8.0).child(icon(glyph, 12.0, t.info));
        list = list.child(ListRow::new(SharedString::from(format!("process-{}", process.id)), process.description.clone()).inset(6.0, 22.0).leading(leading).detail(process_label(process)).trailing(stop_process(&process.id, &process.description, process.stopping)));
    }
    Some(
        div()
            .border_t_1()
            .border_color(t.line_soft)
            .px(px(8.0))
            .pt(px(7.0))
            .pb(px(8.0))
            .child(ListHeading::new("processes-heading", "Processes").action(div().when(running > 0, |note| note.text_color(t.info_soft).font_weight(gpui::FontWeight(550.0)).text_size(px(theme.metrics.text_meta)).child(format!("{running} running")))))
            .child(list)
            .into_any_element(),
    )
}

/// Stop asks once more; while the stop is in flight a spinner holds its place.
fn stop_process(id: &str, name: &str, stopping: bool) -> AnyElement {
    if stopping {
        div().id(SharedString::from(format!("stopping-{id}"))).child(Button::new(SharedString::from(format!("stopping-button-{id}")), "").name(format!("Stopping {name}")).icon(IconName::LoaderCircle).variant(ButtonVariant::Danger).disabled(true)).tip("Stopping…").into_any_element()
    } else {
        let process = id.to_string();
        ConfirmAction::new(SharedString::from(format!("stop-{id}")), "", "Stop", move |_, cx| dispatch(AppCommand::RunStopProcess { task_id: None, process_id: process.clone() }, cx)).icon(IconName::Square).variant(ButtonVariant::Danger).tip("Stop").name(format!("Stop {name}")).into_any_element()
    }
}
