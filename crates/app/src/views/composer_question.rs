//! What stands over the composer while a run asks for it: the goal it works toward and a question
//! waiting on an answer (GoalBar.tsx, QuestionPrompt.tsx).

use act_core::domain::question::PendingQuestion;
use act_core::domain::run::GoalStatus;
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::field::FieldGroup;
use act_ui::components::notice::{Notice, NoticeTone};
use act_ui::input::{Autosize, InputEvent, TextInput};
use act_ui::style::{glass, Corners, GlassKind, Raised};
use act_ui::theme::{ActiveTheme, Tone};
use act_ui::IconName;
use gpui::{div, prelude::*, px, relative, AnyElement, Context, Entity, FontWeight, SharedString, Subscription, Window};
use serde_json::json;

use super::common::ink_icon;
use super::letter_spacing::LetterSpacing;
use super::root::Root;
use super::sidebar::send;
use crate::workspace::dispatch;

/// The answer field of the question on screen, and whether its words were typed or picked.
#[derive(Default)]
pub struct QuestionState {
    asked: Option<String>,
    field: Option<(Entity<TextInput>, Subscription)>,
    typed: bool,
    /// What was typed, kept while a picked option stands in for it as the answer.
    draft: String,
}

fn address(question: &PendingQuestion) -> (String, String, String) {
    (question.run_id.clone(), question.request_id.clone(), question.question_id.clone())
}

impl Root {
    fn answer_question(&mut self, cx: &mut Context<Root>) {
        let state = self.shown(cx);
        let Some(task_id) = self.composer.task_id(&state) else { return };
        let Some(question) = current_question(&state, &task_id).cloned() else { return };
        if question.submitting || question.answer.as_deref().unwrap_or("").trim().is_empty() {
            return;
        }
        let (run_id, request_id, question_id) = address(&question);
        dispatch(AppCommand::QuestionAnswer { task_id, run_id, request_id, question_id, text: None }, cx);
    }

    fn set_question_answer(&mut self, text: String, cx: &mut Context<Root>) {
        let state = self.shown(cx);
        let Some(task_id) = self.composer.task_id(&state) else { return };
        let Some(question) = current_question(&state, &task_id).cloned() else { return };
        let (run_id, request_id, question_id) = address(&question);
        dispatch(AppCommand::QuestionSetAnswer { task_id, run_id, request_id, question_id, text }, cx);
    }
}

fn current_question<'a>(state: &'a WorkspaceState, task_id: &str) -> Option<&'a PendingQuestion> {
    let run = state.active_runs.get(task_id)?;
    run.questions.as_ref()?.first()
}

pub fn question(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    let task_id = root.composer.task_id(state)?;
    let question = current_question(state, &task_id)?.clone();
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let accent = t.tone(Tone::Accent);
    let answer = question.answer.clone().unwrap_or_default();
    let disabled = root.composer.disabled(state) || (state.restored && act_core::workspace::activity::waiting_on(state, Some(&task_id), &root.composer.key(state)).is_some());
    let unavailable = disabled || question.submitting;
    let options = question.question.options.clone();
    if root.composer.question.asked.as_deref() != Some(question.question_id.as_str()) {
        let shown = if options.iter().any(|option| option.label == answer) { String::new() } else { answer.clone() };
        let field = cx.new(|cx| TextInput::new(cx).multiline(Autosize { min_rows: 2, max_height: Some(px(100.0)) }).submit_on_enter(true).with_text(&shown));
        let scope = root.composer.task.clone();
        let subscription = cx.subscribe_in(&field, window, move |root, field, event: &InputEvent, _, cx| {
            root.in_composer(scope.as_deref(), |root| match event {
                InputEvent::Changed => {
                    let text = field.read(cx).text().to_string();
                    root.composer.question.typed = true;
                    root.composer.question.draft = text.clone();
                    root.set_question_answer(text, cx);
                }
                InputEvent::Submit => root.answer_question(cx),
                _ => {}
            });
        });
        root.composer.question = QuestionState { asked: Some(question.question_id.clone()), field: Some((field, subscription)), typed: false, draft: shown };
    }
    let typed = root.composer.question.typed;
    let chosen = !typed && options.iter().any(|option| option.label == answer);
    let overridden = chosen && !root.composer.question.draft.trim().is_empty();
    let field = root.composer.question.field.as_ref().map(|(field, _)| field.clone())?;
    field.update(cx, |field, cx| {
        let placeholder = if options.is_empty() { "Type your answer…" } else { "Or type your answer…" };
        if field.placeholder_text().as_ref() != placeholder {
            field.set_placeholder(placeholder, cx);
        }
        if field.is_disabled() != unavailable {
            field.set_disabled(unavailable, cx);
        }
    });

    let mut content = div()
        .id("question-content")
        .min_h_0()
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_baseline()
                .gap(px(8.0))
                .child(div().text_color(t.accent_text).font_weight(FontWeight(650.0)).child(if question.question.header.is_empty() { "Question".to_string() } else { question.question.header.clone() }))
                .child(div().text_color(t.muted).text_size(px(m.text_meta)).child(if question.blocking { "Waiting for your answer" } else { "Working while you answer" })),
        )
        .child(div().my(px(6.0)).text_color(t.ink).text_size(px(m.text_control)).font_weight(FontWeight(600.0)).whitespace_normal().child(question.question.question.clone()));
    if !options.is_empty() {
        let dark = t.variant == act_ui::ThemeVariant::Dark;
        let mut list = div().flex().flex_col().gap(px(6.0)).my(px(10.0));
        for (index, option) in options.iter().enumerate() {
            let checked = chosen && answer == option.label;
            let label = option.label.clone();
            let mut row = div()
                .id(SharedString::from(format!("question-option-{index}")))
                .relative()
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
                .child(
                    // Chromium's native radio in the theme's scheme, tinted by `accent-color` once picked.
                    div()
                        .flex_none()
                        .mt(px(4.0))
                        .size(px(12.0))
                        .rounded_full()
                        .border_1()
                        .border_color(if checked { t.accent } else if dark { gpui::rgb(0x858585).into() } else { gpui::rgb(0x767676).into() })
                        .bg(if dark { gpui::rgb(0x3b3b3b) } else { gpui::rgb(0xffffff) })
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(checked, |radio| radio.child(div().size(px(7.0)).rounded_full().bg(t.accent))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight(700.0)).child(option.label.clone()))
                        .when(!option.description.is_empty(), |copy| copy.child(div().text_color(t.muted).text_size(px(m.text_meta)).child(option.description.clone()))),
                );
            if unavailable {
                row = row.opacity(0.5);
            } else {
                row = row.cursor_pointer().hover(|style| style.bg(t.accent_surface).border_color(t.accent_line_strong)).on_click(root.composer_listener(cx, move |root, _, _, cx| {
                    root.composer.question.typed = false;
                    let draft = root.composer.question.draft.clone();
                    if let Some((field, _)) = &root.composer.question.field {
                        field.update(cx, |field, cx| {
                            if field.text() != draft {
                                field.reset(&draft, cx);
                            }
                        });
                    }
                    root.composer.question.typed = false;
                    root.set_question_answer(label.clone(), cx);
                }));
            }
            list = list.child(row);
        }
        content = content.child(list);
    }
    let can_send = !unavailable && !answer.trim().is_empty();
    let answer_box = div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .flex_none()
        .pt(px(10.0))
        .child(
            FieldGroup::new().label("Your answer").child(
                div()
                    .min_h(px(60.0))
                    .max_h(px(120.0))
                    .px(px(9.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(t.line_input)
                    .bg(t.surface_input)
                    .text_size(px(m.text_control))
                    .line_height(relative(1.45))
                    .text_color(if overridden { t.muted } else { t.ink })
                    .child(field),
            ),
        )
        .child(div().flex().justify_end().child(Button::new("question-send", if question.submitting { "Sending…" } else { "Send answer" }).variant(ButtonVariant::Inverse).disabled(!can_send).on_click(root.composer_listener(cx, |root, _, _, cx| root.answer_question(cx)))));
    let viewport = f32::from(window.viewport_size().height);
    Some(
        glass(div(), t, GlassKind::Float, None, Some(accent))
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Region, "Pending question"))
            .w_full()
            .max_w(px(860.0))
            .max_h(px((viewport * 0.46).min(400.0)))
            .mb(px(8.0))
            .p(px(14.0))
            .flex()
            .flex_col()
            .text_color(t.ink)
            .text_size(px(m.text_menu))
            .line_height(relative(1.5))
            .child(content)
            .child(answer_box)
            .into_any_element(),
    )
}

/// The goal the thread works toward, with Clear once confirmed.
pub fn goal_bar(root: &mut Root, state: &WorkspaceState, window: &Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    let task_id = state.current_id.clone()?;
    let goal = state.goals.get(&task_id)?.clone();
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let blocked = goal.status == GoalStatus::Blocked;
    let hue = if blocked { t.danger } else { t.accent };
    let detail = if blocked {
        goal.last_reason.clone().unwrap_or_else(|| "Needs attention".into())
    } else if goal.iterations.is_some_and(|passes| passes > 1) {
        format!("Pass {}", goal.iterations.unwrap_or(0))
    } else {
        "Working toward goal".into()
    };
    let open = root.composer.goal_open;
    let long = goal.objective.contains('\n') || goal.objective.chars().count() > 90;
    let toggles = long || open;
    let objective = if open {
        div().flex_1().min_w_0().whitespace_normal().child(goal.objective.clone()).into_any_element()
    } else {
        div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().child(super::text_fit::fit_line(&goal.objective, root.composer.pane_probe.get().map(|bounds| f32::from(bounds.size.width)).unwrap_or(860.0) - 330.0, m.text_control, window)).into_any_element()
    };
    let mut objective_row = div().id("goal-objective").flex().flex_1().min_w_0().items_start().gap(px(6.0)).text_color(t.ink).text_size(px(m.text_control)).child(objective);
    if toggles {
        objective_row = objective_row
            .mx(px(-4.0))
            .my(px(-2.0))
            .px(px(4.0))
            .py(px(2.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .group("goal-objective")
            .hover(|style| style.bg(t.wash))
            .child(div().mt(px(2.0)).text_color(t.muted).group_hover("goal-objective", |style| style.text_color(t.ink)).child(super::common::turning("chevron", if open { 180.0 } else { 0.0 }, 120).child(ink_icon(IconName::ChevronDown, 14.0))))
            .on_click(cx.listener(|root, _, _, cx| {
                root.composer.goal_open = !root.composer.goal_open;
                cx.notify();
            }));
    }
    let target = div().flex_none().size(px(16.0)).rounded_full().border_2().border_color(hue).flex().items_center().justify_center().child(div().size(px(4.0)).rounded_full().bg(hue));
    let width = root.composer.pane_probe.get().map(|bounds| f32::from(bounds.size.width)).unwrap_or(860.0);
    let text = div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .min_w_0()
        .child(target)
        .child(div().flex().flex_1().min_w_0().items_start().gap(px(8.0)).child(div().flex_none().pt(px((m.text_control - m.text_meta) * 0.75)).text_color(hue).text_size(px(m.text_meta)).font_weight(FontWeight(650.0)).letter_spacing(0.06).child("GOAL")).child(objective_row))
        .when(width > 470.0 || blocked, |text| text.child(div().flex_none().max_w(px(width * 0.4)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(t.muted).text_size(px(m.text_meta)).child(detail)));
    let clear_task = task_id.clone();
    Some(
        div()
            .w_full()
            .max_w(px(860.0))
            .mb(px(8.0))
            .child(
                Notice::new("goal-bar", text)
                    .floating(true)
                    .tone(NoticeTone::Accent)
                    .action(ConfirmAction::new("goal-clear", "Clear", "Clear goal", move |_, cx| send(json!({ "type": "task.send", "taskId": clear_task, "text": "/goal clear", "steer": true }), cx)).name("Clear goal").variant(ButtonVariant::Soft)),
            )
            .into_any_element(),
    )
}
