//! Everything waiting on the user from a coordinator and its threads, one page at a time, over the
//! composer: approvals, threads asked for on other computers, decisions, then the threads it
//! suggested here (NeedsYouCard.tsx, coordination.css `.decision-*`, `.suggestion-*`).

use std::collections::HashMap;

use act_core::domain::engine::{model_label, AgentEngine};
use act_core::state::WorkspaceState;
use act_core::view::coordination::{CoordinationApprovalView, CoordinationDecisionView, CoordinationView, ProposalView};
use act_core::commands::ProposalStartPlace;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize, IconButtonVariant};
use act_ui::components::chip::Chip;
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::field::{Checkbox, TextField};
use act_ui::components::select::{Select, SelectOption};
use act_ui::components::spinner::Spinner;
use act_ui::input::{InputEvent, TextInput};
use act_ui::style::{glass, Corners, GlassKind, PaneFill, Raised};
use act_ui::theme::{ActiveTheme, Tone};
use act_ui::tooltip::{shortcut_tip, Tippable};
use act_ui::IconName;
use gpui::{Focusable, div, prelude::*, px, relative, AnyElement, App, Context, Div, Entity, FontWeight, SharedString, Subscription, Window};

use super::common::{icon, ink_icon};
use super::root::Root;
use crate::workspace::dispatch;

/// What the user has chosen or typed on a page, kept under its key while they look at the others.
#[derive(Clone)]
enum Draft {
    Decision { choice: String },
    Elsewhere { picked: Option<String>, worktree: bool },
}

#[derive(Default)]
pub struct NeedsYouState {
    /// The page on screen and where it sat, which is where the card goes on once it is settled.
    page: Option<(String, usize)>,
    drafts: HashMap<String, Draft>,
    /// The own-answer field of each decision page.
    fields: HashMap<String, (Entity<TextInput>, Subscription)>,
    /// The coordinator the card was drawn for; another one starts it afresh.
    thread: Option<String>,
    /// Whether each confirm button is asking, as it last drew, so what it covers hides meanwhile.
    armed: HashMap<String, std::rc::Rc<std::cell::Cell<bool>>>,
}

impl NeedsYouState {
    fn armed(&mut self, key: &str) -> std::rc::Rc<std::cell::Cell<bool>> {
        self.armed.entry(key.to_string()).or_default().clone()
    }
}

enum Item<'v, 'a> {
    Approval(&'v CoordinationApprovalView<'a>),
    Elsewhere(&'v ProposalView<'a>),
    Decision(&'v CoordinationDecisionView<'a>),
    Suggestions(Vec<&'v ProposalView<'a>>),
}

fn items<'v, 'a>(view: &'v CoordinationView<'a>, suggestions: bool) -> Vec<(String, Item<'v, 'a>)> {
    let mut items: Vec<(String, Item)> = view.approvals.iter().map(|approval| (format!("approval:{}:{}", approval.approval.task_id, approval.approval.approval_id), Item::Approval(approval))).collect();
    if suggestions {
        items.extend(view.proposals.iter().filter(|proposal| proposal.elsewhere.is_some()).map(|proposal| (format!("elsewhere:{}", proposal.proposal.id), Item::Elsewhere(proposal))));
    }
    items.extend(view.decisions.iter().map(|decision| (format!("decision:{}:{}", decision.thread.id, decision.decision.id), Item::Decision(decision))));
    let here: Vec<&ProposalView> = view.proposals.iter().filter(|proposal| proposal.elsewhere.is_none()).collect();
    if suggestions && !here.is_empty() {
        items.push(("suggestions".into(), Item::Suggestions(here)));
    }
    items
}

impl Root {
    fn needs_you_step(&mut self, delta: isize, count: usize, keys: Vec<String>, cx: &mut Context<Root>) {
        let Some((_, index)) = self.composer.needs_you.page.clone() else { return };
        let next = (index as isize + delta).rem_euclid(count as isize) as usize;
        self.composer.needs_you.page = keys.get(next).map(|key| (key.clone(), next));
        cx.notify();
    }

    fn needs_you_draft(&mut self, key: &str, draft: Draft, cx: &mut Context<Root>) {
        self.composer.needs_you.drafts.insert(key.to_string(), draft);
        cx.notify();
    }
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    let thread = state.current_thread()?;
    let view = super::coordination::view(state, Some(thread));
    if view.approvals.is_empty() && view.decisions.is_empty() && view.proposals.is_empty() {
        return None;
    }
    let card = &mut root.composer.needs_you;
    if card.thread.as_deref() != Some(thread.id.as_str()) {
        *card = NeedsYouState { thread: Some(thread.id.clone()), ..NeedsYouState::default() };
    }
    let items = items(&view, !view.proposals.is_empty());
    if items.is_empty() {
        return None;
    }
    let found = card.page.as_ref().and_then(|(key, _)| items.iter().position(|(item, _)| item == key));
    let shown = found.unwrap_or_else(|| card.page.as_ref().map(|(_, index)| *index).unwrap_or(0).min(items.len() - 1));
    card.page = Some((items[shown].0.clone(), shown));
    let keys: Vec<String> = items.iter().map(|(key, _)| key.clone()).collect();
    card.drafts.retain(|key, _| keys.contains(key));
    card.fields.retain(|key, _| keys.contains(key));
    let pager = (items.len() > 1).then_some((shown, items.len()));
    let (key, item) = &items[shown];
    let limit = (f32::from(window.viewport_size().height) * 0.46).min(400.0);
    let page = match item {
        Item::Approval(approval) => approval_page(approval, limit, pager, &keys, state, cx),
        Item::Elsewhere(request) => elsewhere_page(root, key, &thread.id, request, limit, pager, &keys, cx),
        Item::Decision(decision) => decision_page(root, key, decision, limit, pager, &keys, window, cx),
        Item::Suggestions(proposals) => suggestions_page(root, &thread.id, view.thread_model.as_ref(), proposals, limit, pager, &keys, state, cx),
    };
    let keys_for_arrows = keys.clone();
    let count = items.len();
    Some(
        div()
            .id("needs-you")
            .w_full()
            .max_w(px(860.0))
            .on_key_down(cx.listener(move |root, event: &gpui::KeyDownEvent, window, cx| {
                let keys = &event.keystroke;
                if count < 2 || keys.modifiers.modified() || window.focused(cx).is_some_and(|focus| root.composer.needs_you.fields.values().any(|(field, _)| field.read(cx).focus_handle(cx) == focus)) {
                    return;
                }
                let delta = match keys.key.as_str() {
                    "left" => -1,
                    "right" => 1,
                    _ => return,
                };
                root.needs_you_step(delta, count, keys_for_arrows.clone(), cx);
                cx.stop_propagation();
            }))
            .child(page)
            .into_any_element(),
    )
}

/// The card's pane: accent glass with a head, a body that scrolls, and a foot.
fn card(id: &str, label: impl Into<SharedString>, head: Div, body: Option<AnyElement>, foot: Div, limit: f32, cx: &App) -> AnyElement {
    let t = &cx.theme().tokens;
    let m = &cx.theme().metrics;
    let mut pane = glass(div(), t, GlassKind::Float, None, Some(t.tone(Tone::Accent)))
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Region, label))
        .w_full()
        .mb(px(8.0))
        .max_h(px(limit))
        .overflow_hidden()
        .flex()
        .flex_col()
        .text_size(px(m.text_menu))
        .line_height(relative(1.5))
        .text_color(t.ink)
        .child(head);
    if let Some(body) = body {
        pane = pane.child(div().id(SharedString::from(format!("{id}-body"))).min_h_0().flex_shrink().overflow_y_scroll().child(body));
    }
    pane.child(foot.flex_none().flex().items_center().gap(px(8.0)).px(px(14.0)).py(px(10.0)).border_t_1().border_color(t.line_soft)).into_any_element()
}

fn head(title: impl IntoElement, cx: &App) -> Div {
    let t = &cx.theme().tokens;
    let m = &cx.theme().metrics;
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.0))
        .pt(px(10.0))
        .px(px(14.0))
        .pb(px(6.0))
        .text_size(px(m.text_meta))
        .child(div().text_color(t.accent_text).text_size(px(m.text_menu)).font_weight(FontWeight(650.0)).child(title))
}

/// "Needs you", and the thread that asks, which opens it.
fn needs_head(id: &str, thread_id: &str, title: &str, cx: &App) -> Div {
    let t = cx.theme().tokens.clone();
    let thread_id = thread_id.to_string();
    head("Needs you", cx).child(
        div()
            .min_w_0()
            .flex()
            .items_center()
            .rounded(px(7.0))
            .px(px(7.0))
            .py(px(2.0))
            .pane(t.glass_control)
            .rim(t.glass_rim, Corners::Radius(7.0))
            .id(SharedString::from(format!("{id}-from")))
            .text_color(t.accent_text)
            .cursor_pointer()
            .hover(move |style: gpui::StyleRefinement| style.bg(t.glass_control_hover.background()).text_color(t.accent_soft))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title.to_string()))
            .child(div().ml(px(3.0)).opacity(0.8).child(ink_icon(IconName::ArrowUpRight, 12.0)))
            .tip("Open thread")
            .on_click(move |_, _, cx| dispatch(AppCommand::TaskSelect { task_id: thread_id.clone() }, cx)),
    )
}

fn pager(head: Div, pager: Option<(usize, usize)>, keys: &[String], cx: &mut Context<Root>) -> Div {
    let Some((index, count)) = pager else { return head };
    let back = keys.to_vec();
    let on = keys.to_vec();
    head.child(
        div()
            .ml_auto()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(IconButton::new("needs-you-previous", IconName::ChevronLeft, "Previous").variant(IconButtonVariant::Soft).size(IconButtonSize::Xs).icon_size(14.0).tip(super::chrome::key_tip("Previous", "ArrowLeft")).on_click(cx.listener(move |root, _, _, cx| root.needs_you_step(-1, count, back.clone(), cx))))
            .child(Chip::count(format!("{} of {count}", index + 1)).tone(Some(Tone::Accent)))
            .child(IconButton::new("needs-you-next", IconName::ChevronRight, "Next").variant(IconButtonVariant::Soft).size(IconButtonSize::Xs).icon_size(14.0).tip(super::chrome::key_tip("Next", "ArrowRight")).on_click(cx.listener(move |root, _, _, cx| root.needs_you_step(1, count, on.clone(), cx)))),
    )
}

/// Turns to the next page, leaving this one waiting.
fn later(foot: Div, pager: Option<(usize, usize)>, keys: &[String], cx: &mut Context<Root>) -> Div {
    let Some((_, count)) = pager else { return foot };
    let keys = keys.to_vec();
    foot.child(Button::new("needs-you-later", "Later").variant(ButtonVariant::Secondary).on_click(cx.listener(move |root, _, _, cx| root.needs_you_step(1, count, keys.clone(), cx))))
}

fn question(text: &str, cx: &App) -> Div {
    let t = &cx.theme().tokens;
    div().mx(px(14.0)).mb(px(2.0)).text_color(t.ink).text_size(px(cx.theme().metrics.text_control)).font_weight(FontWeight(600.0)).whitespace_normal().child(text.to_string())
}

fn context(text: &str, cx: &App) -> Div {
    div().mx(px(14.0)).mb(px(4.0)).text_color(cx.theme().tokens.muted).whitespace_normal().child(text.to_string())
}

fn binding_tip(state: &WorkspaceState, label: &str, action: &str) -> SharedString {
    let binding = act_core::domain::shortcuts::shortcut_settings(&state.shortcuts).into_iter().find(|setting| setting.action.id == action).and_then(|setting| setting.binding);
    match binding {
        Some(binding) => shortcut_tip(label, &act_core::domain::shortcuts::display_shortcut(&binding, cfg!(target_os = "macos"))),
        None => label.to_string().into(),
    }
}

fn approval_page(view: &CoordinationApprovalView, limit: f32, position: Option<(usize, usize)>, keys: &[String], state: &WorkspaceState, cx: &mut Context<Root>) -> AnyElement {
    let approval = view.approval;
    let id = format!("needs-approval-{}", approval.approval_id);
    let mut body = div().flex().flex_col().child(question(&approval.title, cx));
    if !approval.description.is_empty() {
        body = body.child(context(&approval.description, cx));
    }
    // `margin: 6px 14px 10px`, collapsing with the 4px (context) or 2px (question) above it; the frame carries 11px.
    let above = if approval.description.is_empty() { 2.0 } else { 4.0 };
    body = body.child(div().mx(px(14.0)).mt(px(6.0 - above - 11.0)).mb(px(10.0)).child(super::conversation::approval_action(&id, &approval.input, &approval.tool_name, cx)));
    let decide = |allow: bool| {
        let (task_id, run_id, approval_id) = (approval.task_id.clone(), approval.run_id.clone(), approval.approval_id.clone());
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| dispatch(AppCommand::RunDecide { allow, task_id: task_id.clone(), run_id: run_id.clone(), approval_id: approval_id.clone() }, cx)
    };
    let head = pager(needs_head(&id, &view.thread.id, &view.thread.title, cx), position, keys, cx);
    let foot = later(div(), position, keys, cx)
        .child(div().flex_1())
        .child(Button::new("needs-you-deny", "Deny").variant(ButtonVariant::Secondary).tip(binding_tip(state, "Deny", "run.deny")).on_click(decide(false)))
        .child(Button::new("needs-you-allow", "Allow once").variant(ButtonVariant::Primary).tip(binding_tip(state, "Allow once", "run.allow")).on_click(decide(true)));
    card(&id, "Approval waiting on you", head, Some(body.into_any_element()), foot, limit, cx)
}

fn decision_page(root: &mut Root, key: &str, view: &CoordinationDecisionView, limit: f32, position: Option<(usize, usize)>, keys: &[String], window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let decision = view.decision;
    let id = format!("needs-decision-{}", decision.id);
    let choice = match root.composer.needs_you.drafts.get(key) {
        Some(Draft::Decision { choice }) => choice.clone(),
        _ => decision.options.iter().find(|option| option.recommended).map(|option| option.label.clone()).unwrap_or_default(),
    };
    if !root.composer.needs_you.fields.contains_key(key) {
        let field = cx.new(|cx| TextInput::new(cx));
        let (draft_key, thread_id, decision_id, recommended) = (key.to_string(), view.thread.id.clone(), decision.id.clone(), choice.clone());
        let subscription = cx.subscribe_in(&field, window, move |root, field, event: &InputEvent, _, cx| match event {
            InputEvent::Changed => cx.notify(),
            InputEvent::Submit => {
                let own = field.read(cx).text().trim().to_string();
                let choice = match root.composer.needs_you.drafts.get(&draft_key) {
                    Some(Draft::Decision { choice }) => choice.clone(),
                    _ => recommended.clone(),
                };
                let answer = if own.is_empty() { choice } else { own };
                if !answer.is_empty() {
                    dispatch(AppCommand::DecisionAnswer { task_id: thread_id.clone(), decision_id: decision_id.clone(), answer }, cx);
                }
            }
            _ => {}
        });
        root.composer.needs_you.fields.insert(key.to_string(), (field, subscription));
    }
    let field = root.composer.needs_you.fields[key].0.clone();
    let own = field.read(cx).text().trim().to_string();
    let answer = if own.is_empty() { choice.clone() } else { own.clone() };
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let mut body = div().flex().flex_col().child(question(&decision.question, cx));
    if let Some(text) = decision.context.as_deref().filter(|text| !text.is_empty()) {
        body = body.child(context(text, cx));
    }
    if !decision.options.is_empty() {
        let mut list = div().flex().flex_col().gap(px(6.0)).mt(px(8.0)).mx(px(14.0)).mb(px(12.0));
        for (index, option) in decision.options.iter().enumerate() {
            let checked = own.is_empty() && choice == option.label;
            let label = option.label.clone();
            let key = key.to_string();
            let field = field.clone();
            let mut name = div().flex().items_center().gap(px(8.0)).text_color(t.ink).child(option.label.clone());
            if option.recommended {
                name = name.child(Chip::tag("Recommended").tone(Some(Tone::Accent)));
            }
            let row = div()
                .id(SharedString::from(format!("{id}-option-{index}")))
                .flex()
                .items_start()
                .gap(px(10.0))
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(9.0))
                .border_1()
                .border_color(if checked { t.accent_line_strong } else { t.line })
                .rim(t.glass_rim, Corners::Radius(9.0))
                .when(checked, |row| row.bg(t.accent_surface))
                .when(!own.is_empty(), |row| row.opacity(0.55))
                .cursor_pointer()
                .hover(|style| style.bg(t.accent_surface).border_color(t.accent_line_strong).opacity(1.0))
                .child(radio(checked, cx))
                .child(div().flex().flex_col().child(name).when_some(option.description.clone().filter(|text| !text.is_empty()), |copy, text| copy.child(div().text_color(t.muted).text_size(px(m.text_meta)).child(text))))
                .on_click(cx.listener(move |root, _, _, cx| {
                    field.update(cx, |field, cx| field.reset("", cx));
                    root.needs_you_draft(&key, Draft::Decision { choice: label.clone() }, cx);
                }));
            list = list.child(row);
        }
        body = body.child(list);
    }
    let head = pager(needs_head(&id, &view.thread.id, &view.thread.title, cx), position, keys, cx);
    let thread_id = view.thread.id.clone();
    let decision_id = decision.id.clone();
    let send = answer.clone();
    let foot = div()
        .items_end()
        .child(div().flex_1().child(TextField::new(SharedString::from(format!("{id}-own")), &field).label(if decision.options.is_empty() { "Your answer" } else { "Or your own answer" })));
    let foot = later(foot, position, keys, cx).child(Button::new("needs-you-send", "Send answer").variant(ButtonVariant::Inverse).disabled(answer.is_empty()).on_click(move |_, _, cx| {
        dispatch(AppCommand::DecisionAnswer { task_id: thread_id.clone(), decision_id: decision_id.clone(), answer: send.clone() }, cx);
    }));
    card(&id, "Decision waiting on you", head, Some(body.into_any_element()), foot, limit, cx)
}

fn radio(checked: bool, cx: &App) -> Div {
    let t = &cx.theme().tokens;
    div()
        .flex_none()
        .mt(px(4.0))
        .size(px(14.0))
        .rounded_full()
        .border_1()
        .border_color(if checked { t.accent } else { t.line_strong })
        .flex()
        .items_center()
        .justify_center()
        .when(checked, |dot| dot.bg(t.accent).child(div().size(px(5.0)).rounded_full().bg(t.accent_ink)))
}

fn elsewhere_page(root: &mut Root, key: &str, task_id: &str, request: &ProposalView, limit: f32, position: Option<(usize, usize)>, keys: &[String], cx: &mut Context<Root>) -> AnyElement {
    let proposal = request.proposal;
    let Some(elsewhere) = request.elsewhere.as_ref() else { return div().into_any_element() };
    let id = format!("needs-elsewhere-{}", proposal.id);
    let (picked, worktree) = match root.composer.needs_you.drafts.get(key) {
        Some(Draft::Elsewhere { picked, worktree }) => (picked.clone(), *worktree),
        _ => (None, proposal.place.worktree == Some(true)),
    };
    let project = picked.filter(|picked| elsewhere.projects.iter().any(|item| &item.id == picked)).or_else(|| elsewhere.project.clone()).unwrap_or_default();
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let host = &elsewhere.computer;
    let elsewhere_label = format!("Run on {}?", host.name);
    let title = div()
        .flex()
        .items_center()
        .gap(px(5.0))
        .child("Run on")
        .child(div().flex().items_center().gap(px(4.0)).child(div().flex().when(host.offline, |mark| mark.accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, if host.reconnecting { "Reconnecting" } else { "Offline" }))).child(icon(if host.offline { IconName::MonitorOff } else { IconName::Monitor }, 12.0, t.accent_text))).child(host.name.clone()))
        .child("?");
    let head = pager(head(title, cx), position, keys, cx);
    let mut place = div().flex().flex_wrap().items_center().gap_x(px(16.0)).gap_y(px(8.0)).mt(px(8.0)).mx(px(14.0)).mb(px(10.0)).text_color(t.ink_dim).text_size(px(m.text_menu));
    if !elsewhere.projects.is_empty() {
        let options = elsewhere.projects.iter().map(|item| SelectOption::new(item.id.clone(), act_core::domain::project::project_name(item))).collect();
        let draft_key = key.to_string();
        place = place.child(div().flex().items_center().gap(px(7.0)).min_w_0().child("Folder").child(Select::new(SharedString::from(format!("{id}-folder")), options, project.clone()).on_change(cx.listener(move |root, value: &SharedString, _, cx| {
            root.needs_you_draft(&draft_key, Draft::Elsewhere { picked: Some(value.to_string()), worktree }, cx);
        }))));
    }
    let draft_key = key.to_string();
    let picked_now = (!project.is_empty()).then(|| project.clone());
    place = place.child(Checkbox::new(SharedString::from(format!("{id}-worktree")), worktree).accent(true).label("New worktree").on_change(cx.listener(move |root, checked: &bool, _, cx| {
        root.needs_you_draft(&draft_key, Draft::Elsewhere { picked: picked_now.clone(), worktree: *checked }, cx);
    })));
    let mut body = div().flex().flex_col().child(question(&proposal.title, cx)).child(place);
    if let Some(brief) = proposal.brief.as_ref() {
        body = body.child(div().mx(px(14.0)).mb(px(10.0)).border_1().border_color(t.line_soft).rounded(px(9.0)).child(brief_details(&id, root, brief, cx)));
    }
    if let Some(refusal) = elsewhere.refusal.as_ref() {
        body = body.child(div().mx(px(14.0)).mb(px(10.0)).text_color(t.danger_soft).text_size(px(m.text_meta)).child(refusal.clone()));
    } else if let Some(error) = proposal.error.as_ref().filter(|_| !request.starting) {
        body = body.child(div().mx(px(14.0)).mb(px(10.0)).text_color(t.danger_soft).text_size(px(m.text_meta)).child(error.clone()));
    }
    let (task_dismiss, proposal_dismiss) = (task_id.to_string(), proposal.id.clone());
    let (task_start, proposal_start) = (task_id.to_string(), proposal.id.clone());
    let place = ProposalStartPlace { project, worktree };
    let armed = root.composer.needs_you.armed(&format!("{id}-deny"));
    let hidden = armed.get();
    let foot = later(div(), position, keys, cx)
        .child(div().flex_1())
        .child(ConfirmAction::new(SharedString::from(format!("{id}-deny")), "Deny", "Deny", move |_, cx| dispatch(AppCommand::ProposalDismiss { task_id: task_dismiss.clone(), proposal_id: Some(proposal_dismiss.clone()), proposal_ids: None }, cx)).disabled(request.starting).armed_into(&armed))
        .when(!hidden, |foot| foot.child(Button::new("needs-you-allow-elsewhere", if request.starting { "Starting…" } else { "Allow" }).variant(ButtonVariant::Inverse).disabled(request.starting || elsewhere.refusal.is_some()).on_click(move |_, _, cx| {
            dispatch(AppCommand::ProposalStart { task_id: task_start.clone(), proposal_id: Some(proposal_start.clone()), proposal_ids: None, place: Some(place.clone()) }, cx)
        })));
    card(&id, elsewhere_label, head, Some(body.into_any_element()), foot, limit, cx)
}

/// What a coordinator handed a thread, folded under "Brief".
fn brief_details(id: &str, root: &mut Root, brief: &act_core::domain::coordination::ThreadBrief, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let open = root.composer.needs_you.drafts.contains_key(&format!("brief:{id}"));
    let toggle = format!("brief:{id}");
    let mut details = div().flex().flex_col().child(
        div()
            .id(SharedString::from(format!("{id}-brief")))
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(12.0))
            .py(px(7.0))
            .text_color(t.ink_dim)
            .font_weight(gpui::FontWeight(550.0))
            .text_size(px(m.text_meta))
            .cursor_pointer()
            .hover(|style| style.bg(t.wash).text_color(t.ink_soft))
            .child(icon(IconName::ChevronRight, 12.0, t.quiet_strong).rotate(if open { 90.0 } else { 0.0 }))
            .child("Brief")
            .on_click(cx.listener(move |root, _, _, cx| {
                if root.composer.needs_you.drafts.remove(&toggle).is_none() {
                    root.composer.needs_you.drafts.insert(toggle.clone(), Draft::Decision { choice: String::new() });
                }
                cx.notify();
            })),
    );
    if open {
        let row = |label: &'static str, value: Div| div().flex().gap(px(12.0)).child(div().w(px(96.0)).flex_none().pt(px(1.0)).text_color(t.quiet).text_size(px(m.text_meta)).child(label)).child(value.flex_1().min_w_0().text_color(t.ink_dim).whitespace_normal());
        let delivers = brief.delivers.label();
        details = details.child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .pt(px(2.0))
                .px(px(12.0))
                .pb(px(11.0))
                .child(row("Your words", div().border_l_2().border_color(t.accent_line).pl(px(9.0)).italic().child(brief.intent.clone())))
                .child(row("Done when", div().child(brief.done_when.clone())))
                .child(row("Delivers", div().child(delivers))),
        );
    }
    details.into_any_element()
}

fn suggestions_page(root: &mut Root, task_id: &str, thread_model: Option<&act_core::domain::coordination::ThreadModelChoice>, proposals: &[&ProposalView], limit: f32, position: Option<(usize, usize)>, keys: &[String], state: &WorkspaceState, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let catalog = state.catalog();
    let shown: Vec<String> = proposals.iter().filter(|view| !view.starting).map(|view| view.proposal.id.clone()).collect();
    let waiting = !shown.is_empty();
    let mut list = div().flex().flex_col().px(px(8.0)).pb(px(8.0));
    for view in proposals {
        let proposal = view.proposal;
        let engine = proposal.engine.as_deref().and_then(AgentEngine::parse).or(thread_model.and_then(|choice| choice.engine));
        let model = proposal.model.as_ref().filter(|model| thread_model.is_none_or(|choice| model.as_str() != choice.model.as_str() || choice.engine != engine)).map(|model| model_label(&catalog, engine, model.as_str()));
        let worktree = proposal.place.worktree == Some(true) || proposal.place.worktree_id.is_some();
        let summary = proposal.brief.as_ref().map(|brief| brief.intent.clone()).unwrap_or_else(|| proposal.prompt.clone());
        let group: SharedString = format!("suggestion-{}", proposal.id).into();
        let mut text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .when(view.starting, |text| text.opacity(0.7))
            .child(div().text_color(t.ink).font_weight(FontWeight(600.0)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(proposal.title.clone()))
            .child(div().text_color(t.muted).text_size(px(m.text_meta)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(summary.replace('\n', " ")));
        if let Some(error) = proposal.error.as_ref().filter(|_| !view.starting) {
            text = text.child(div().text_color(t.danger_soft).text_size(px(m.text_meta)).whitespace_normal().child(error.clone()));
        }
        let mut row = div()
            .id(group.clone())
            .group(group.clone())
            .flex()
            .items_center()
            .gap(px(6.0))
            .min_w_0()
            .rounded(px(9.0))
            .pl(px(8.0))
            .pr(px(6.0))
            .py(px(5.0))
            .hover(|style| style.bg(t.surface_hover))
            .child(text);
        if let Some(model) = model {
            row = row.child(Chip::tag(model));
        }
        if worktree {
            row = row.child(Chip::tag("Worktree"));
        }
        if view.starting {
            row = row.child(div().mx(px(7.0)).accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, format!("Starting {}", proposal.title))).child(Spinner::new(SharedString::from(format!("{group}-starting"))).size(px(14.0))));
        } else {
            let armed = root.composer.needs_you.armed(&format!("{group}-dismiss"));
            let (dismiss_task, dismiss_id) = (task_id.to_string(), proposal.id.clone());
            let (start_task, start_id) = (task_id.to_string(), proposal.id.clone());
            row = row.child(
                ConfirmAction::new(SharedString::from(format!("{group}-dismiss")), "", "Dismiss", move |_, cx| dispatch(AppCommand::ProposalDismiss { task_id: dismiss_task.clone(), proposal_id: Some(dismiss_id.clone()), proposal_ids: None }, cx))
                    .icon(IconName::X)
                    .variant(ButtonVariant::Ghost)
                    .tip("Dismiss")
                    .name(format!("Dismiss {}", proposal.title))
                    .armed_into(&armed),
            );
            if !armed.get() {
                row = row.child(
                    IconButton::new(SharedString::from(format!("{group}-start")), IconName::Play, format!("Start {}", proposal.title))
                        .variant(IconButtonVariant::Inverse)
                        .round(true)
                        .icon_size(12.0)
                        .glyph(super::glyphs::play(12.0, t.ink_inverse))
                        .tip("Start")
                        .on_click(move |_, _, cx| dispatch(AppCommand::ProposalStart { task_id: start_task.clone(), proposal_id: Some(start_id.clone()), proposal_ids: None, place: None }, cx)),
                );
            }
        }
        list = list.child(row);
    }
    let head = pager(head("Suggested threads", cx), position, keys, cx);
    let (dismiss_task, dismiss_ids) = (task_id.to_string(), shown.clone());
    let (start_task, start_ids) = (task_id.to_string(), shown);
    let all_armed = root.composer.needs_you.armed("dismiss-all");
    let mut foot = later(div(), position, keys, cx)
        .child(div().flex_1())
        .child(ConfirmAction::new("needs-you-dismiss-all", "Dismiss all", "Dismiss all", move |_, cx| dispatch(AppCommand::ProposalDismiss { task_id: dismiss_task.clone(), proposal_id: None, proposal_ids: Some(dismiss_ids.clone()) }, cx)).disabled(!waiting).armed_into(&all_armed));
    if !all_armed.get() {
        foot = foot.child(Button::new("needs-you-start-all", "Start all").variant(ButtonVariant::Inverse).size(ButtonSize::Md).disabled(!waiting).on_click(move |_, _, cx| {
            dispatch(AppCommand::ProposalStart { task_id: start_task.clone(), proposal_id: None, proposal_ids: Some(start_ids.clone()), place: None }, cx)
        }));
    }
    card("needs-suggestions", "Suggested threads", head, Some(list.into_any_element()), foot, limit, cx)
}
