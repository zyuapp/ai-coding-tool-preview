//! The `/review` picker, kept in the composer: what to review, then the base branch, the commit or
//! the instructions it needs (ReviewPicker.tsx).

use act_core::domain::question::ReviewTarget;
use act_core::state::{ReviewStep, WorkspaceState};
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant, IconButton};
use act_ui::components::chip::Chip;
use act_ui::components::field::{SearchField, SearchVariant, TextField};
use act_ui::input::{Autosize, InputEvent, TextInput};
use act_ui::style::{glass, Corners, GlassKind, PaneFill, Raised};
use act_ui::theme::ActiveTheme;
use act_ui::IconName;
use gpui::{deferred, div, prelude::*, px, AnyElement, Context, Entity, FontWeight, KeyDownEvent, MouseButton, SharedString, Subscription};

use super::common::{icon, Ellipsis};
use super::root::Root;
use crate::workspace::dispatch;

/// The branches of the checkout under review, as read.
#[derive(Clone)]
pub enum BranchRead {
    Reading,
    Read { branches: Vec<String>, remotes: Vec<String>, current: Option<String> },
    Failed(String),
}

/// The picker's own fields, kept until it closes so stepping back keeps what was typed.
#[derive(Default)]
pub struct ReviewState {
    open: bool,
    search: Option<(Entity<TextInput>, Subscription)>,
    sha: Option<(Entity<TextInput>, Subscription)>,
    instructions: Option<(Entity<TextInput>, Subscription)>,
    active: usize,
    focused_target: usize,
    branches: Option<(String, BranchRead)>,
    focused_step: Option<ReviewStep>,
    focus: Option<gpui::FocusHandle>,
}

/// Which branches a typed query keeps, loosely, with the exact name first.
pub fn match_branches(branches: &[String], query: &str) -> Vec<String> {
    let typed = query.trim();
    let needle = typed.to_lowercase();
    if needle.is_empty() {
        return branches.to_vec();
    }
    let mut matched: Vec<String> = branches.iter().filter(|name| name.to_lowercase().contains(&needle)).cloned().collect();
    if let Some(exact) = matched.iter().position(|name| name == typed).filter(|at| *at > 0) {
        let name = matched.remove(exact);
        matched.insert(0, name);
    }
    matched
}

/// Reads a checkout's branches off the window's thread.
pub fn read_branches(workspace_id: String, cx: &mut Context<Root>, done: impl FnOnce(&mut Root, BranchRead, &mut Context<Root>) + 'static) {
    let read = crate::effects::window_desktop::branches(workspace_id, cx);
    cx.spawn(async move |root, cx| {
        let read = match read.await {
            Ok(list) => BranchRead::Read { branches: list.branches, remotes: list.remotes, current: list.current },
            Err(message) => BranchRead::Failed(message),
        };
        let _ = root.update(cx, |root, cx| {
            done(root, read, cx);
            cx.notify();
        });
    })
    .detach();
}

fn targets(engine: &str) -> [(Option<ReviewStep>, Option<ReviewTarget>, &'static str, String, IconName); 4] {
    [
        (Some(ReviewStep::Base), None, "Review against a base branch", "Compare this branch with another branch.".into(), IconName::GitBranch),
        (None, Some(ReviewTarget::UncommittedChanges), "Review uncommitted changes", "Review staged, unstaged, and untracked changes.".into(), IconName::ListChecks),
        (Some(ReviewStep::Commit), None, "Review a commit", "Review the changes introduced by one commit.".into(), IconName::GitCommitHorizontal),
        (Some(ReviewStep::Custom), None, "Custom review instructions", format!("Tell {engine} what to inspect."), IconName::MessageSquareText),
    ]
}

fn is_hex(text: &str) -> bool {
    text.chars().all(|ch| ch.is_ascii_hexdigit())
}

impl Root {
    fn review(&mut self, target: ReviewTarget, cx: &mut Context<Root>) {
        let task_id = self.state(cx).current_id.clone();
        dispatch(AppCommand::ReviewStart { task_id, target }, cx);
    }

    fn review_step(&mut self, step: ReviewStep, cx: &mut Context<Root>) {
        dispatch(AppCommand::ReviewSetStep { step }, cx);
    }

    fn review_back_or_close(&mut self, cx: &mut Context<Root>) {
        let step = self.state(cx).review_picker.as_ref().map(|picker| picker.step);
        match step {
            Some(ReviewStep::Targets) | None => dispatch(AppCommand::ReviewClose, cx),
            Some(_) => self.review_step(ReviewStep::Targets, cx),
        }
    }

    fn review_branch_names(&self, query: &str) -> Vec<String> {
        let Some((_, BranchRead::Read { branches, remotes, current })) = &self.composer.review.branches else { return Vec::new() };
        let mut names: Vec<String> = Vec::new();
        for name in branches.iter().chain(remotes.iter()) {
            if Some(name) != current.as_ref() && !names.contains(name) {
                names.push(name.clone());
            }
        }
        match_branches(&names, query)
    }

    fn submit_sha(&mut self, cx: &mut Context<Root>) {
        let Some((field, _)) = &self.composer.review.sha else { return };
        let commit = field.read(cx).text().trim().to_string();
        if (4..=64).contains(&commit.len()) && is_hex(&commit) {
            self.review(ReviewTarget::Commit { sha: commit, title: None }, cx);
        }
    }

    fn submit_instructions(&mut self, cx: &mut Context<Root>) {
        let Some((field, _)) = &self.composer.review.instructions else { return };
        let instructions = field.read(cx).text().trim().to_string();
        if !instructions.is_empty() {
            self.review(ReviewTarget::Custom { instructions }, cx);
        }
    }
}

fn field(window: &mut gpui::Window, cx: &mut Context<Root>, multiline: bool, placeholder: &str, on_submit: fn(&mut Root, &mut Context<Root>)) -> (Entity<TextInput>, Subscription) {
    let input = cx.new(|cx| {
        let input = TextInput::new(cx).placeholder(placeholder.to_string());
        if multiline {
            input.multiline(Autosize { min_rows: 4, max_height: Some(px(200.0)) }).submit_on_enter(true)
        } else {
            input
        }
    });
    let subscription = cx.subscribe_in(&input, window, move |root, _, event: &InputEvent, _, cx| match event {
        InputEvent::Changed => {
            root.composer.review.active = 0;
            cx.notify();
        }
        InputEvent::Submit => on_submit(root, cx),
        _ => {}
    });
    (input, subscription)
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut gpui::Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    let picker = state.review_picker.clone().filter(|picker| Some(&picker.task_id) == state.current_id.as_ref() && !state.active_runs.contains_key(&picker.task_id));
    let Some(picker) = picker else {
        if root.composer.review.open {
            root.composer.review = ReviewState::default();
            root.composer.input.update(cx, |input, _| input.focus(window));
        }
        return None;
    };
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let mono = theme.typography.mono_family.clone();
    let engine = state.current_thread().map(|thread| act_core::domain::engine::engine_label(thread.engine)).unwrap_or("Claude");
    let workspace_id = state.current_thread().and_then(|thread| act_core::workspace::location::thread_workspace_id(state, Some(thread)));
    let review = &mut root.composer.review;
    review.open = true;
    if picker.step == ReviewStep::Base {
        let wanted = workspace_id.clone().unwrap_or_default();
        if review.branches.as_ref().map(|(id, _)| id) != Some(&wanted) {
            review.branches = Some((wanted.clone(), if workspace_id.is_some() { BranchRead::Reading } else { BranchRead::Failed("Open a project to compare branches.".into()) }));
            if workspace_id.is_some() {
                read_branches(wanted.clone(), cx, move |root, read, _| {
                    if root.composer.review.branches.as_ref().map(|(id, _)| id) == Some(&wanted) {
                        root.composer.review.branches = Some((wanted, read));
                    }
                });
            }
        }
    }
    let focus_now = root.composer.review.focused_step != Some(picker.step);
    root.composer.review.focused_step = Some(picker.step);
    let current = match &root.composer.review.branches {
        Some((_, BranchRead::Read { current, .. })) => current.clone(),
        _ => None,
    };
    let title: AnyElement = match picker.step {
        ReviewStep::Targets => "Choose a review".into_any_element(),
        ReviewStep::Base => match &current {
            Some(current) => div().flex().items_center().gap(px(4.0)).child("Compare").child(div().text_color(t.ink_soft).font_family(mono.clone()).font_weight(FontWeight(550.0)).child(current.clone())).child("with…").into_any_element(),
            None => "Choose a base branch".into_any_element(),
        },
        ReviewStep::Commit => "Review a commit".into_any_element(),
        ReviewStep::Custom => "Custom review".into_any_element(),
    };
    let targets_step = picker.step == ReviewStep::Targets;
    let mut head = div().flex_none().flex().items_center().gap(px(7.0)).min_h(px(39.0)).py(px(5.0)).pr(px(6.0)).border_b_1().border_color(t.line).text_color(t.quiet).text_size(px(m.text_meta)).font_weight(FontWeight(500.0));
    head = if targets_step {
        head.pl(px(13.0)).child(icon(IconName::ListChecks, 14.0, t.quiet))
    } else {
        head.pl(px(6.0)).child(IconButton::new("review-back", IconName::ChevronLeft, "Back to review options").tip(super::chrome::key_tip("Back", "Escape")).on_click(cx.listener(|root, _, _, cx| root.review_step(ReviewStep::Targets, cx))))
    };
    head = head.child(div().flex_1().min_w_0().ellipsis().child(title)).child(IconButton::new("review-close", IconName::X, "Close review picker").tip(if targets_step { super::chrome::key_tip("Close", "Escape") } else { "Close".into() }).on_click(|_, _, cx| dispatch(AppCommand::ReviewClose, cx)));

    let body: AnyElement = match picker.step {
        ReviewStep::Targets => {
            let focused = root.composer.review.focused_target;
            let room = root.composer.pane_probe.get().map(|bounds| f32::from(bounds.size.width)).unwrap_or(860.0) - 12.0 - 20.0 - 34.0 - 20.0 - super::text_fit::line_width(engine, m.text_badge, window) - 22.0;
            let mut list = div().accessible(gpui::AccessInfo::new(gpui::AccessRole::List, "Review options")).flex().flex_col().p(px(6.0));
            for (index, (step, target, label, description, glyph)) in targets(engine).into_iter().enumerate() {
                let selected = index == focused;
                let mut row = div()
                    .id(SharedString::from(format!("review-target-{index}")))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .min_h(px(56.0))
                    .px(px(10.0))
                    .py(px(7.0))
                    .rounded(px(10.0))
                    .cursor_pointer()
                    .child(div().relative().flex_none().size(px(30.0)).mr(px(4.0)).flex().items_center().justify_center().rounded(px(9.0)).border_1().border_color(t.accent_line).bg(t.accent_chip).rim(t.glass_rim, Corners::Radius(9.0)).child(icon(glyph, 16.0, t.accent_soft)))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(3.0)).child(div().whitespace_nowrap().overflow_hidden().text_size(px(m.text_control)).font_weight(FontWeight(550.0)).text_color(if selected { t.ink } else { t.ink_soft }).child(super::text_fit::fit_line(label, room, m.text_control, window))).child(div().whitespace_nowrap().overflow_hidden().text_size(px(m.text_meta)).text_color(if selected { t.ink_dim } else { t.quiet_strong }).child(super::text_fit::fit_line(&description, room, m.text_meta, window))))
                    .child(Chip::tag(engine.to_string()))
                    .on_hover(cx.listener(move |root, hovered: &bool, _, cx| {
                        if *hovered {
                            root.composer.review.focused_target = index;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |root, _, _, cx| match (&target, step) {
                        (Some(target), _) => root.review(target.clone(), cx),
                        (None, Some(step)) => root.review_step(step, cx),
                        _ => {}
                    }));
                if selected {
                    row = row.pane(t.glass_pick).rim(t.glass_rim, Corners::Radius(10.0));
                }
                list = list.child(row);
            }
            list.into_any_element()
        }
        ReviewStep::Base => {
            if root.composer.review.search.is_none() {
                root.composer.review.search = Some(field(window, cx, false, "Search branches", |root, cx| {
                    let query = root.composer.review.search.as_ref().map(|(field, _)| field.read(cx).text().to_string()).unwrap_or_default();
                    let names = root.review_branch_names(&query);
                    if let Some(branch) = names.get(root.composer.review.active.min(names.len().saturating_sub(1))).cloned() {
                        root.review(ReviewTarget::BaseBranch { branch }, cx);
                    }
                }));
                if let Some((search, _)) = &root.composer.review.search {
                    search.update(cx, |search, _| search.set_label("Search base branches"));
                }
            }
            let (search, _) = root.composer.review.search.as_ref().unwrap();
            let search = search.clone();
            if focus_now {
                search.update(cx, |search, _| search.focus(window));
            }
            let query = search.read(cx).text().to_string();
            let names = root.review_branch_names(&query);
            let chosen = if names.is_empty() { None } else { Some(root.composer.review.active.min(names.len() - 1)) };
            let mut list = div().id("review-branches").accessible(gpui::AccessInfo::new(gpui::AccessRole::List, "Base branches")).flex().flex_col().pt(px(4.0)).px(px(8.0)).pb(px(8.0)).overflow_y_scroll().max_h(px(260.0));
            for (index, branch) in names.iter().enumerate() {
                let active = Some(index) == chosen;
                let pick = branch.clone();
                let mut row = div()
                    .id(SharedString::from(format!("review-branch-{index}")))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .px(px(10.0))
                    .py(px(9.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .text_color(if active { t.ink } else { t.ink_soft })
                    .child(icon(IconName::GitBranch, 14.0, t.muted))
                    .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().font_family(mono.clone()).text_size(px(m.text_control)).child(branch.clone()))
                    .on_hover(cx.listener(move |root, hovered: &bool, _, cx| {
                        if *hovered {
                            root.composer.review.active = index;
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |root, _, _, cx| root.review(ReviewTarget::BaseBranch { branch: pick.clone() }, cx)));
                if active {
                    row = row.pane(t.glass_pick).rim(t.glass_rim, Corners::Radius(8.0));
                }
                list = list.child(row);
            }
            let note = |text: String, danger: bool| div().py(px(18.0)).px(px(12.0)).flex().justify_center().text_size(px(m.text_control)).text_color(if danger { t.danger_soft } else { t.quiet }).child(text);
            match &root.composer.review.branches {
                Some((_, BranchRead::Reading)) | None => list = list.child(note("Reading branches…".into(), false)),
                Some((_, BranchRead::Failed(message))) => list = list.child(note(message.clone(), true)),
                Some((_, BranchRead::Read { .. })) if names.is_empty() => list = list.child(note(if query.trim().is_empty() { "No other branch to compare with".into() } else { "No branch matches".into() }, false)),
                _ => {}
            }
            let count = names.len();
            div()
                .flex()
                .flex_col()
                .min_h(px(180.0))
                .overflow_hidden()
                .child(
                    div()
                        .flex_none()
                        .mt(px(8.0))
                        .mx(px(8.0))
                        .mb(px(4.0))
                        .capture_action(cx.listener(move |root, _: &act_ui::input::Up, _, cx| {
                            if count > 0 {
                                root.composer.review.active = (root.composer.review.active.min(count - 1) + count - 1) % count;
                                cx.stop_propagation();
                                cx.notify();
                            }
                        }))
                        .capture_action(cx.listener(move |root, _: &act_ui::input::Down, _, cx| {
                            if count > 0 {
                                root.composer.review.active = (root.composer.review.active.min(count - 1) + 1) % count;
                                cx.stop_propagation();
                                cx.notify();
                            }
                        }))
                        .on_key_down(cx.listener(move |root, event: &KeyDownEvent, _, cx| match event.keystroke.key.as_str() {
                            "up" if count > 0 => {
                                root.composer.review.active = (root.composer.review.active.min(count - 1) + count - 1) % count;
                                cx.stop_propagation();
                                cx.notify();
                            }
                            "down" if count > 0 => {
                                root.composer.review.active = (root.composer.review.active.min(count - 1) + 1) % count;
                                cx.stop_propagation();
                                cx.notify();
                            }
                            _ => {}
                        }))
                        .child(SearchField::new("review-search", &search).variant(SearchVariant::Menu)),
                )
                .child(list)
                .into_any_element()
        }
        ReviewStep::Commit => {
            if root.composer.review.sha.is_none() {
                root.composer.review.sha = Some(field(window, cx, false, "e.g. a1b2c3d", |root, cx| root.submit_sha(cx)));
            }
            let (sha, _) = root.composer.review.sha.as_ref().unwrap();
            let sha = sha.clone();
            if focus_now {
                sha.update(cx, |sha, cx| {
                    sha.move_to_end(cx);
                    sha.focus(window);
                });
            }
            let commit = sha.read(cx).text().trim().to_string();
            let valid = (4..=64).contains(&commit.len()) && is_hex(&commit);
            let invalid = !valid && (!is_hex(&commit) || commit.len() > 64);
            let short = !valid && !invalid && !commit.is_empty();
            let mut text = TextField::new("review-sha", &sha).label("Commit SHA").mono(true);
            if short {
                text = text.hint("At least 4 characters");
            }
            if invalid {
                text = text.error(Some("Enter a hex commit SHA".into()));
            }
            div()
                .flex()
                .flex_col()
                .gap(px(14.0))
                .p(px(14.0))
                .child(text)
                .child(div().flex().justify_end().child(Button::new("review-commit-start", "Start review").variant(ButtonVariant::Primary).disabled(!valid).on_click(cx.listener(|root, _, _, cx| root.submit_sha(cx)))))
                .into_any_element()
        }
        ReviewStep::Custom => {
            let placeholder = format!("What should {engine} focus on?");
            if root.composer.review.instructions.is_none() {
                root.composer.review.instructions = Some(field(window, cx, true, &placeholder, |root, cx| root.submit_instructions(cx)));
            }
            let (instructions, _) = root.composer.review.instructions.as_ref().unwrap();
            let instructions = instructions.clone();
            if focus_now {
                instructions.update(cx, |field, cx| {
                    field.move_to_end(cx);
                    field.focus(window);
                });
            }
            let ready = !instructions.read(cx).text().trim().is_empty();
            div()
                .flex()
                .flex_col()
                .gap(px(14.0))
                .p(px(14.0))
                .child(TextField::new("review-instructions", &instructions).label("Review instructions"))
                .child(div().flex().justify_end().child(Button::new("review-custom-start", "Start review").variant(ButtonVariant::Primary).disabled(!ready).on_click(cx.listener(|root, _, _, cx| root.submit_instructions(cx)))))
                .into_any_element()
        }
    };
    let focus = root.composer.review.focus.get_or_insert_with(|| cx.focus_handle()).clone();
    if focus_now && targets_step {
        focus.focus(window);
    }
    let card = glass(div(), t, GlassKind::Popover, Some(15.0), None)
        .id("review-picker")
        .map(act_ui::style::glass_in)
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Dialog, "Start code review"))
        .track_focus(&focus)
        .flex()
        .flex_col()
        .max_h(px(390.0))
        .overflow_hidden()
        .on_key_down(cx.listener(|root, event: &KeyDownEvent, _, cx| {
            let state = root.state(cx);
            let targets_step = state.review_picker.as_ref().is_some_and(|picker| picker.step == ReviewStep::Targets);
            match event.keystroke.key.as_str() {
                "escape" => {
                    root.review_back_or_close(cx);
                    cx.stop_propagation();
                }
                "up" | "down" if targets_step => {
                    let step = if event.keystroke.key == "up" { 3 } else { 1 };
                    root.composer.review.focused_target = (root.composer.review.focused_target + step) % 4;
                    cx.stop_propagation();
                    cx.notify();
                }
                "enter" | "space" if targets_step => {
                    let engine = state.current_thread().map(|thread| act_core::domain::engine::engine_label(thread.engine)).unwrap_or("Claude");
                    let (step, target, ..) = targets(engine)[root.composer.review.focused_target % 4].clone();
                    match (target, step) {
                        (Some(target), _) => root.review(target, cx),
                        (None, Some(step)) => root.review_step(step, cx),
                        _ => {}
                    }
                    cx.stop_propagation();
                }
                _ => {}
            }
        }))
        .on_mouse_down_out(|_, _, cx| dispatch(AppCommand::ReviewClose, cx))
        .child(head)
        .child(body);
    Some(deferred(div().absolute().left_0().right_0().bottom(gpui::relative(1.0)).pb(px(9.0)).child(card)).with_priority(2).into_any_element())
}

#[cfg(test)]
mod tests {
    use super::match_branches;

    #[test]
    fn the_exact_branch_leads_the_matches() {
        let names: Vec<String> = ["feature/main-fix", "main", "dev"].iter().map(|name| name.to_string()).collect();
        assert_eq!(match_branches(&names, "main"), vec!["main".to_string(), "feature/main-fix".to_string()]);
        assert_eq!(match_branches(&names, " "), names);
    }
}
