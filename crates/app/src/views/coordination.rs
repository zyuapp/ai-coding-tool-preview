//! A coordinator's threads as the conversation and the session card draw them, and a thread's place
//! under its coordinator (Coordination.tsx: ThreadCards, CoordinationBar, BriefDetails,
//! MemberWorktreeList; SessionPanel.tsx: MemberPullRequestList).

use std::collections::{HashMap, HashSet};


use act_core::domain::engine::AgentEngine;
use act_core::domain::pull_request::PullRequestState;
use act_core::state::WorkspaceState;
use act_core::view::coordination::{coordination_view, CoordinationView};
use act_core::view::sidebar::ThreadHost;
use act_core::workspace::coordinated_status::{pull_request_line, CoordinatedThreadStatus, OwnPullRequest};
use act_core::domain::coordination::NextStep;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant, IconButton, IconButtonVariant};
use act_ui::style::{glass, GlassKind};
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, App, Div, FontWeight, SharedString};

use super::common::{engine_icon, icon, ink_icon, with_alpha, Ellipsis};
use crate::workspace::dispatch;

/// One thread under a coordinator as its card draws it.
#[derive(Clone)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub engine: AgentEngine,
    pub status: CoordinatedThreadStatus,
    pub summary: Option<String>,
    pub pull_request: Option<OwnPullRequest>,
    pub steps: Vec<NextStep>,
    pub host: Option<ThreadHost>,
    pub last_heard: bool,
    pub worktree: Option<(String, String)>,
}

/// The busy and blocked threads the coordination views read their statuses from.
pub fn activity(state: &WorkspaceState) -> (HashSet<String>, HashSet<String>) {
    let activity = act_core::workspace::activity::thread_activity(state);
    (activity.working, activity.blocked)
}

pub fn view<'a>(state: &'a WorkspaceState, thread: Option<&'a act_core::domain::thread::Thread>) -> CoordinationView<'a> {
    let (busy, blocked) = activity(state);
    coordination_view(state, thread, &busy, &blocked)
}

/// The cards of a coordinator's threads, by thread id.
pub fn cards_of(state: &WorkspaceState, view: &CoordinationView) -> HashMap<String, Card> {
    view.members
        .iter()
        .map(|member| {
            let thread = member.thread;
            let worktree = thread.worktree_id.as_ref().map(|id| {
                let name = state.worktrees.iter().find(|worktree| &worktree.id == id).map(act_core::domain::worktree::worktree_name).unwrap_or_else(|| "a worktree".into());
                (id.clone(), name)
            });
            let card = Card {
                id: thread.id.clone(),
                title: thread.title.clone(),
                engine: thread.engine,
                status: member.status,
                summary: member.summary.clone(),
                pull_request: member.pull_request.clone(),
                steps: member.steps.clone(),
                host: member.host.clone(),
                last_heard: member.last_heard,
                worktree,
            };
            (thread.id.clone(), card)
        })
        .collect()
}

fn status_label(status: CoordinatedThreadStatus) -> &'static str {
    match status {
        CoordinatedThreadStatus::Approval => "Needs approval",
        CoordinatedThreadStatus::Asking => "Needs you",
        CoordinatedThreadStatus::Working => "Working",
        CoordinatedThreadStatus::Resolved => "Resolved",
        CoordinatedThreadStatus::Blocked => "Blocked",
        CoordinatedThreadStatus::Failed => "Failed",
        CoordinatedThreadStatus::NeedsWork => "Needs work",
        CoordinatedThreadStatus::Review => "Ready for review",
        CoordinatedThreadStatus::Done => "Done",
        CoordinatedThreadStatus::Idle => "Idle",
    }
}

pub fn status_line(status: CoordinatedThreadStatus, summary: Option<&str>, last_heard: bool) -> String {
    let label = if last_heard { format!("{} when last heard", status_label(status)) } else { status_label(status).to_string() };
    match summary {
        Some(summary) => format!("{label} · {summary}"),
        None => label,
    }
}

/// A thread's status as a dot, or a spinner while it works.
pub fn status_mark(id: impl Into<SharedString>, status: CoordinatedThreadStatus, last_heard: bool, cx: &App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    if status == CoordinatedThreadStatus::Working && !last_heard {
        return super::spinner::bead(id, 10.0, &t, false).into_any_element();
    }
    let dot = div().flex_none().size(px(7.0)).rounded_full();
    match status {
        CoordinatedThreadStatus::Done => dot.bg(t.success),
        CoordinatedThreadStatus::Blocked | CoordinatedThreadStatus::Failed => dot.bg(t.danger),
        CoordinatedThreadStatus::Asking | CoordinatedThreadStatus::Approval => dot.bg(t.accent),
        CoordinatedThreadStatus::Review => dot.border_2().border_color(t.success),
        CoordinatedThreadStatus::NeedsWork => dot.border_2().border_color(t.danger),
        CoordinatedThreadStatus::Resolved => dot.bg(t.quiet),
        _ => dot.bg(t.line_bright),
    }
    .into_any_element()
}

fn mark_fill(status: CoordinatedThreadStatus, cx: &App) -> gpui::Rgba {
    let t = &cx.theme().tokens;
    match status {
        CoordinatedThreadStatus::Working => t.info_surface,
        CoordinatedThreadStatus::Done | CoordinatedThreadStatus::Review => t.success_surface,
        CoordinatedThreadStatus::Blocked | CoordinatedThreadStatus::Failed | CoordinatedThreadStatus::NeedsWork => t.danger_chip,
        CoordinatedThreadStatus::Asking | CoordinatedThreadStatus::Approval => t.accent_chip,
        _ => t.control,
    }
}

pub fn pull_request_icon(state: PullRequestState) -> IconName {
    match state {
        PullRequestState::Draft => IconName::GitPullRequestDraft,
        PullRequestState::Open => IconName::GitPullRequest,
        PullRequestState::Merged => IconName::GitMerge,
        PullRequestState::Closed => IconName::GitPullRequestClosed,
    }
}

pub fn pull_request_color(state: PullRequestState, cx: &App) -> gpui::Rgba {
    let t = &cx.theme().tokens;
    match state {
        PullRequestState::Open => t.success,
        PullRequestState::Merged => t.pr_merged,
        PullRequestState::Closed => t.danger,
        PullRequestState::Draft => t.muted,
    }
}

/// A link chip: link-coloured words that light up under the pointer.
fn link_chip(id: impl Into<SharedString>, label: impl Into<SharedString>, cx: &App) -> gpui::Stateful<Div> {
    let t = cx.theme().tokens.clone();
    div()
        .id(id.into())
        .flex()
        .items_center()
        .gap(px(3.0))
        .h(px(24.0))
        .px(px(8.0))
        .rounded(px(7.0))
        .bg(t.wash)
        .text_color(t.link)
        .text_size(px(cx.theme().metrics.text_meta))
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|style| style.bg(t.surface_hover).text_color(t.link_hover))
        .child(label.into())
}

/// The threads a coordinator started together, side by side where they began in its conversation.
pub fn thread_cards(thread_ids: &[String], cards: &HashMap<String, Card>, cx: &App) -> AnyElement {
    let shown: Vec<&Card> = thread_ids.iter().filter_map(|id| cards.get(id)).collect();
    if shown.is_empty() {
        return div().into_any_element();
    }
    div().flex().flex_wrap().gap(px(8.0)).children(shown.into_iter().map(|card| thread_card(card, cx))).into_any_element()
}

fn thread_card(card: &Card, cx: &App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let status = card.status;
    let pr_summary = card.pull_request.is_some() && matches!(status, CoordinatedThreadStatus::NeedsWork | CoordinatedThreadStatus::Review);
    let line = status_line(status, if pr_summary { None } else { card.summary.as_deref() }, card.last_heard);
    let line_color = match status {
        CoordinatedThreadStatus::Asking | CoordinatedThreadStatus::Approval => t.accent,
        CoordinatedThreadStatus::Blocked | CoordinatedThreadStatus::Failed | CoordinatedThreadStatus::NeedsWork => t.danger_soft,
        _ => t.muted,
    };
    let working = status == CoordinatedThreadStatus::Working && !card.last_heard;
    let small: AnyElement = if working {
        div().pl(px(30.0)).min_w_0().ellipsis().text_size(px(m.text_meta)).child(super::sweep::sweep(SharedString::from(format!("card-sweep-{}", card.id)), line.clone(), cx)).into_any_element()
    } else {
        div().pl(px(30.0)).min_w_0().ellipsis().text_color(line_color).text_size(px(m.text_meta)).child(line.clone()).into_any_element()
    };
    let mut marks = div().flex().items_center().gap(px(6.0));
    if let Some(host) = &card.host {
        marks = marks.child(div().max_w(px(96.0)).ellipsis().text_color(if host.offline { t.quiet } else { t.muted }).text_size(px(m.text_meta)).child(host.name.clone()));
    }
    if let Some((id, name)) = &card.worktree {
        let hue = t.worktree[act_core::domain::worktree::worktree_hue(id).rem_euclid(6) as usize];
        marks = marks.child(div().id(SharedString::from(format!("card-worktree-{}", card.id))).child(icon(IconName::FolderSymlink, 12.0, hue)).tip(format!("Works in {name}")));
    }
    marks = marks.child(icon(engine_icon(card.engine), 12.0, t.quiet_strong));
    let head = div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(div().flex_none().size(px(22.0)).rounded_full().bg(mark_fill(status, cx)).flex().items_center().justify_center().child(status_mark(SharedString::from(format!("card-mark-{}", card.id)), status, card.last_heard, cx)))
        .child(div().flex_1().min_w_0().ellipsis().text_color(t.ink_soft).text_size(px(m.text_control)).font_weight(FontWeight(550.0)).child(card.title.clone()))
        .child(marks);
    let thread_id = card.id.clone();
    // Read by its status line and, when it has one, its pull request's.
    let described = match &card.pull_request {
        Some(pull_request) => format!("{line} {}", pull_request_line(pull_request)),
        None => line.clone(),
    };
    let mut open = div()
        .id(SharedString::from(format!("card-open-{}", card.id)))
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Open {}", card.title)).description(described))
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .px(px(10.0))
        .py(px(9.0))
        .text_color(t.muted)
        .cursor_pointer()
        .transition(gpui::Transition::ease(150))
        .hover(|style| style.bg(t.surface_hover))
        .child(head)
        .child(small)
        .on_click(move |_, _, cx| dispatch(AppCommand::TaskSelect { task_id: thread_id.clone() }, cx));
    if let Some(pull_request) = &card.pull_request {
        open = open.child(
            div()
                .ml(px(30.0))
                .mt(px(3.0))
                .max_w(px(200.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .rounded(px(6.0))
                .px(px(7.0))
                .py(px(1.0))
                .bg(t.wash)
                .text_color(t.ink_dim)
                .text_size(px(m.text_meta))
                .child(icon(pull_request_icon(pull_request.state), 12.0, pull_request_color(pull_request.state, cx)))
                .child(div().min_w_0().ellipsis().child(pull_request_line(pull_request))),
        );
    }
    let mut card_element = glass(div(), &t, GlassKind::Chip, Some(12.0), None).w(px(260.0)).max_w_full().flex().flex_col().overflow_hidden().child(open);
    if matches!(status, CoordinatedThreadStatus::Asking | CoordinatedThreadStatus::Approval) {
        card_element = card_element.border_1().border_color(with_alpha(t.accent, 0.4));
    }
    let mut card_element = card_element.id(SharedString::from(format!("card-{}", card.id)));
    if status == CoordinatedThreadStatus::Resolved {
        card_element = card_element.opacity(0.62).hover(|style| style.opacity(1.0));
    }
    if !card.steps.is_empty() || card.pull_request.is_some() {
        let mut steps = div().flex().flex_wrap().gap(px(6.0)).pl(px(40.0)).pr(px(10.0)).pb(px(10.0));
        for step in &card.steps {
            let task_id = card.id.clone();
            let step = *step;
            steps = steps.child(link_chip(format!("card-step-{}-{:?}", card.id, step), step.label(), cx).accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("{} for {}", step.label(), card.title))).on_click(move |_, _, cx| dispatch(AppCommand::CoordinationNextStep { task_id: task_id.clone(), step }, cx)));
        }
        if let Some(pull_request) = &card.pull_request {
            let moving = matches!(pull_request.state, PullRequestState::Open | PullRequestState::Draft);
            let url = pull_request.url.clone();
            steps = steps.child(
                link_chip(format!("card-pr-{}", card.id), if moving { "Review PR" } else { "View PR" }, cx)
                    .accessible(gpui::AccessInfo::new(gpui::AccessRole::Link, format!("{} for {}", if moving { "Review PR" } else { "View PR" }, card.title)).url(url.clone()))
                    .child(ink_icon(IconName::ArrowUpRight, 12.0))
                    .tip(format!("#{} {}", pull_request.number, pull_request.title))
                    .on_mouse_down(gpui::MouseButton::Right, super::side_chat::web_link_menu(url.clone()))
                    .on_click(super::side_chat::web_link(url)),
            );
        }
        card_element = card_element.child(steps);
    }
    card_element.into_any_element()
}

/// A thread's place under its coordinator: who it works for, what it was asked, and whether the
/// user owes it an answer. Drawn over the top of the transcript.
pub fn coordination_bar(view: &CoordinationView, brief_open: bool, on_brief: impl Fn(&mut App) + 'static, cx: &App) -> Option<AnyElement> {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    if view.coordinator || (view.lead.is_none() && view.remote_lead.is_none() && view.brief.is_none()) {
        return None;
    }
    let lead_id = view.lead.map(|lead| lead.id.clone()).or_else(|| view.remote_lead.as_ref().and_then(|remote| remote.thread_id.clone()));
    let title = view.lead.map(|lead| lead.title.clone()).or_else(|| view.remote_lead.as_ref().map(|remote| remote.title.clone()));
    let mut bar = glass(div(), &t, GlassKind::Float, Some(12.0), None).w_full().max_w(px(860.0)).mx_auto().flex().flex_col().overflow_hidden().text_size(px(m.text_menu));
    if view.asking.is_some() {
        bar = bar.border_1().border_color(with_alpha(t.accent, 0.4));
    }
    if let Some(title) = title {
        let mut lead = div().flex().items_center().gap(px(4.0)).text_color(t.muted).child("Works under");
        lead = match &lead_id {
            Some(id) => {
                let id = id.clone();
                lead.child(link_chip("coordination-lead", title, cx).bg(gpui::transparent_black()).on_click(move |_, _, cx| dispatch(AppCommand::TaskSelect { task_id: id.clone() }, cx)))
            }
            None => lead.child(div().font_weight(FontWeight(600.0)).text_color(t.ink_dim).child(title)),
        };
        if let Some(remote) = &view.remote_lead {
            lead = lead.child(format!("on {}", remote.computer));
        }
        let mut line = div().flex().items_center().gap(px(10.0)).min_h(px(36.0)).pl(px(12.0)).pr(px(8.0)).child(lead);
        if let Some(asking) = &view.asking {
            line = line.child(div().ml_auto().text_color(t.accent).child(asking.clone()));
            if let Some(id) = lead_id.clone() {
                line = line.child(Button::new("coordination-answer", "Answer").variant(ButtonVariant::Inverse).on_click(move |_, _, cx| dispatch(AppCommand::TaskSelect { task_id: id.clone() }, cx)));
            }
        }
        bar = bar.child(line);
    }
    if let Some(brief) = view.brief {
        let summary = div()
            .id("coordination-brief")
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(12.0))
            .py(px(7.0))
            .text_color(t.ink_dim)
            .font_weight(FontWeight(550.0))
            .cursor_pointer()
            .hover(|style| style.bg(t.wash).text_color(t.ink_soft))
            .when(view.lead.is_some() || view.remote_lead.is_some(), |summary| summary.border_t_1().border_color(t.line_soft))
            .child(icon(IconName::ChevronRight, 12.0, t.quiet_strong).rotate(if brief_open { 90.0 } else { 0.0 }))
            .child("Brief")
            .on_click(move |_, _, cx| on_brief(cx));
        bar = bar.child(summary);
        if brief_open {
            let row = |label: &'static str, value: Div| div().flex().gap(px(12.0)).child(div().w(px(96.0)).flex_none().pt(px(1.0)).text_color(t.quiet).text_size(px(m.text_meta)).child(label)).child(value.flex_1().min_w_0().text_color(t.ink_dim));
            bar = bar.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .px(px(12.0))
                    .pt(px(2.0))
                    .pb(px(11.0))
                    .child(row("Your words", div().border_l_2().border_color(t.accent_line).pl(px(9.0)).italic().child(brief.intent.clone())))
                    .child(row("Done when", div().child(brief.done_when.clone())))
                    .child(row("Delivers", div().child(brief.delivers.label()))),
            );
        }
    }
    Some(bar.into_any_element())
}

/// How far down the transcript starts under the bar: under a lead's line or under a brief alone.
pub fn bar_room(view: &CoordinationView) -> f32 {
    if view.coordinator {
        return 0.0;
    }
    if view.lead.is_some() || view.remote_lead.is_some() {
        96.0
    } else if view.brief.is_some() {
        64.0
    } else {
        0.0
    }
}

/// The checkouts a coordinator's threads work in, which only reveal and delete from here.
pub fn member_worktrees(state: &WorkspaceState, lead: &act_core::domain::thread::Thread, cx: &App) -> Option<AnyElement> {
    let (busy, _) = activity(state);
    let worktrees = act_core::view::settings::member_worktrees_view(state, Some(lead), &busy);
    if worktrees.is_empty() {
        return None;
    }
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let rows = worktrees.into_iter().map(|worktree| {
        let by = worktree.threads.iter().map(|thread| thread.title.clone()).collect::<Vec<_>>().join(", ");
        let detail = if worktree.deleting { "Deleting…".to_string() } else { worktree.branch.as_ref().map(|branch| format!("{branch} · {by}")).unwrap_or(by) };
        let hue = t.worktree[act_core::domain::worktree::worktree_hue(&worktree.id).rem_euclid(6) as usize];
        let root = worktree.root.clone();
        let reveal_root = worktree.root.clone();
        let delete_root = worktree.root.clone();
        let detail_element: AnyElement = if worktree.deleting { super::sweep::sweep(SharedString::from(format!("worktree-deleting-{}", worktree.id)), detail, cx).into_any_element() } else { div().child(detail).into_any_element() };
        div()
            .id(SharedString::from(format!("member-worktree-{}", worktree.id)))
            .flex()
            .items_center()
            .gap(px(8.0))
            .py(px(5.0))
            .px(px(6.0))
            .rounded(px(8.0))
            .hover(|style| style.bg(t.surface_hover))
            .child(div().flex_none().size(px(22.0)).rounded_full().bg(t.control).flex().items_center().justify_center().child(icon(IconName::FolderSymlink, 12.0, hue)))
            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.0)).child(div().ellipsis().text_color(t.ink_soft).font_weight(FontWeight(560.0)).text_size(px(m.text_control)).child(worktree.name.clone())).child(div().ellipsis().text_color(t.muted_soft).text_size(px(m.text_meta)).child(detail_element)))
            // `.list-row-trail`: the actions sit 6 apart, closer than the row's own gap.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(IconButton::new(SharedString::from(format!("reveal-{}", worktree.id)), IconName::FolderOpen, format!("Reveal {}", worktree.name)).icon_size(14.0).tip(if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Show in file manager" }).disabled(worktree.deleting).on_click(move |_, _, cx| dispatch(AppCommand::WorktreeReveal { root: reveal_root.clone() }, cx)))
                    .child(
                        IconButton::new(SharedString::from(format!("delete-{}", worktree.id)), IconName::Trash2, format!("Delete {}", worktree.name))
                            .variant(IconButtonVariant::Danger)
                            .icon_size(14.0)
                            .tip(if worktree.busy { "Wait for its threads to finish before deleting" } else { "Delete worktree…" })
                            .disabled(worktree.busy || worktree.deleting)
                            .on_click(move |_, _, cx| super::sidebar::send(serde_json::json!({ "type": "worktree.confirm-delete", "root": delete_root }), cx)),
                    ),
            )
            .tip(root)
    });
    Some(section("Worktrees", "Worktrees these threads work in", div().flex().flex_col().gap(px(2.0)).children(rows), cx))
}

/// The pull requests a coordinator's threads have opened, each with the threads working on it.
pub fn member_pull_requests(state: &WorkspaceState, lead: &act_core::domain::thread::Thread, cx: &App) -> Option<AnyElement> {
    let view = act_core::view::pull_request::member_pull_requests_view(state, Some(lead));
    if view.found.is_empty() {
        return None;
    }
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let rows = view.found.into_iter().map(|found| {
        let pull_request = found.pull_request;
        let state_label = match pull_request.state {
            PullRequestState::Draft => "Draft",
            PullRequestState::Open => "Open",
            PullRequestState::Merged => "Merged",
            PullRequestState::Closed => "Closed",
        };
        let url = pull_request.url.clone();
        let mut small = div().flex().flex_wrap().items_center().text_color(t.muted_soft).text_size(px(m.text_meta)).child(format!("#{} · {} · ", pull_request.number, state_label));
        for (index, thread) in found.threads.iter().enumerate() {
            if index > 0 {
                small = small.child(", ");
            }
            let id = thread.id.clone();
            small = small.child(div().id(SharedString::from(format!("pr-thread-{}-{}", pull_request.number, thread.id))).accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Open {}", thread.title))).cursor_pointer().text_color(t.ink_dim).underline().text_decoration_color(t.line_strong).hover(|style| style.text_color(t.ink_soft).underline().text_decoration_color(t.ink_soft)).child(thread.title.clone()).on_click(move |_, _, cx| {
                cx.stop_propagation();
                dispatch(AppCommand::TaskSelect { task_id: id.clone() }, cx);
            }));
        }
        div()
            .id(SharedString::from(format!("pr-row-{}", pull_request.number)))
            .flex()
            .items_start()
            .gap(px(8.0))
            .min_h(px(40.0))
            .px(px(6.0))
            .py(px(4.0))
            .rounded(px(9.0))
            .text_color(t.muted)
            .transition(gpui::Transition::ease(150))
            .hover(|style| style.bg(t.surface_hover).text_color(t.ink_soft))
            .cursor_pointer()
            .on_mouse_down(gpui::MouseButton::Right, super::side_chat::web_link_menu(url.clone()))
            .on_click(super::side_chat::web_link(url.clone()))
            .child(div().flex_none().size(px(22.0)).rounded_full().bg(if pull_request.state == PullRequestState::Open { t.success_surface } else { t.control }).flex().items_center().justify_center().child(icon(pull_request_icon(pull_request.state), 12.0, pull_request_color(pull_request.state, cx))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().id(SharedString::from(format!("pr-open-{}", pull_request.number))).accessible(gpui::AccessInfo::new(gpui::AccessRole::Link, pull_request.title.clone()).url(url.clone())).ellipsis().text_size(px(m.text_control)).font_weight(FontWeight(560.0)).child(pull_request.title.clone()).tip(format!("#{} {}", pull_request.number, pull_request.title)))
                    .child(small),
            )
    });
    Some(section("Pull requests", "Pull requests from these threads", div().flex().flex_col().gap(px(2.0)).children(rows), cx))
}

fn section(label: &'static str, region: &'static str, body: Div, cx: &App) -> AnyElement {
    let t = cx.theme().tokens.clone();
    div()
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Region, region))
        .flex()
        .flex_col()
        .px(px(8.0))
        .pt(px(7.0))
        .pb(px(8.0))
        .border_t_1()
        .border_color(t.line_soft)
        .child(div().min_h(px(30.0)).flex().items_center().px(px(8.0)).text_color(t.quiet_strong).text_size(px(cx.theme().metrics.text_menu)).font_weight(FontWeight(600.0)).child(label))
        .child(body.mt(px(8.0)))
        .into_any_element()
}
