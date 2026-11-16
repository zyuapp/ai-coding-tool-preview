//! Messages waiting on the run, each with what it carries and what can be done to it (QueuedRow.tsx).

use act_core::domain::message_origin::{is_users_own, message_label};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::{Button, IconButton};
use act_ui::style::{glass, GlassKind};
use act_ui::theme::ActiveTheme;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString, Window};

use super::common::{icon, ink_icon};
use super::root::Root;
use crate::workspace::dispatch;

pub fn render(root: &mut Root, state: &WorkspaceState, window: &Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let id = root.composer.task_id(state)?;
    let task = root.composer.task.clone();
    let messages = act_core::state::queued_view(state, &id).filter(|messages| !messages.is_empty())?;
    let can_steer = state.active_runs.contains_key(&id);
    let width = root.composer.pane_probe.get().map(|bounds| f32::from(bounds.size.width)).unwrap_or(860.0);
    let mut list = div().accessible(gpui::AccessInfo::new(gpui::AccessRole::List, "Queued messages")).w_full().max_w(px(860.0)).mb(px(8.0)).flex().flex_col().gap(px(4.0));
    for view in messages {
        let message = &view.message;
        let held = &message.held;
        let message_id = held.id.clone();
        let mut body = div().flex_1().min_w_0().flex().flex_col().gap(px(5.0));
        if let Some(label) = message_label(held.origin.as_ref(), held.detail.as_deref()) {
            body = body.child(div().whitespace_nowrap().overflow_hidden().text_color(t.muted).text_size(px(m.text_meta)).child(super::text_fit::fit_line(&label, width - 190.0, m.text_meta, window)));
        }
        if !held.text.is_empty() {
            let open = root.composer.queued_open.contains(&message_id);
            let cut = held.text.contains('\n') || super::text_fit::line_width(&held.text, m.text_control, window) > width - 190.0;
            let toggles = cut || open;
            let mut text = div().id(SharedString::from(format!("queued-text-{message_id}"))).flex().items_start().gap(px(6.0)).min_w_0().text_color(t.ink_dim);
            let words = if open {
                div().id(SharedString::from(format!("queued-words-{message_id}"))).flex_1().min_w_0().max_h(px(360.0)).overflow_y_scroll().whitespace_normal().child(held.text.clone()).into_any_element()
            } else {
                div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().child(super::text_fit::fit_line(&held.text, width - 214.0, m.text_control, window)).into_any_element()
            };
            text = text.child(words);
            if toggles {
                let toggle_id = message_id.clone();
                text = text
                    .mx(px(-4.0))
                    .my(px(-2.0))
                    .px(px(4.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .group("queued-text")
                    .hover(|style| style.bg(t.wash).text_color(t.ink))
                    .child(super::common::turning("chevron", if open { 180.0 } else { 0.0 }, 120).p(px(2.0)).rounded(px(5.0)).bg(t.wash).text_color(t.ink_dim).group_hover("queued-text", |style| style.text_color(t.ink)).child(ink_icon(IconName::ChevronDown, 14.0)))
                    .on_click(root.composer_listener(cx, move |root, _, _, cx| {
                        if !root.composer.queued_open.remove(&toggle_id) {
                            root.composer.queued_open.insert(toggle_id.clone());
                        }
                        cx.notify();
                    }));
            }
            body = body.child(text);
        }
        if let Some(annotations) = held.annotations.as_ref().filter(|items| !items.is_empty()) {
            if let Some(row) = super::composer_attached::annotation_row(&format!("queued-{message_id}"), annotations, false, root, window, cx) {
                body = body.child(row);
            }
        }
        let trailing = if message.steering || view.starting {
            div().flex_none().pr(px(6.0)).text_color(t.muted).text_size(px(m.text_meta)).child(if message.steering { "Steering…" } else { "Sending…" }).into_any_element()
        } else {
            let mut actions = div().flex_none().flex().items_center().gap(px(2.0));
            if is_users_own(held.origin.as_ref(), held.detail.as_deref(), held.answers.as_deref()) {
                let edit_id = message_id.clone();
                let task = task.clone();
                actions = actions.child(IconButton::new(SharedString::from(format!("queued-edit-{message_id}")), IconName::Pencil, "Edit queued message").tip("Edit").on_click(move |_, _, cx| dispatch(AppCommand::TaskEditQueued { task_id: task.clone(), message_id: edit_id.clone() }, cx)));
            }
            let steer_id = message_id.clone();
            let steer_task = task.clone();
            actions = actions.child(
                div().mr(px(2.0)).child(
                    Button::new(SharedString::from(format!("queued-steer-{message_id}")), "Steer")
                        .disabled(!can_steer)
                        .reason("Steer once the run resumes")
                        .tip("Send now, into the running turn")
                        .on_click(move |_, _, cx| {
                            if can_steer {
                                dispatch(AppCommand::TaskSteerQueued { task_id: steer_task.clone(), message_id: steer_id.clone() }, cx);
                            }
                        }),
                ),
            );
            let drop_id = message_id.clone();
            let task = task.clone();
            actions = actions.child(IconButton::new(SharedString::from(format!("queued-drop-{message_id}")), IconName::X, "Remove queued message").tip("Remove").on_click(move |_, _, cx| dispatch(AppCommand::TaskDropQueued { task_id: task.clone(), message_id: drop_id.clone(), if_waiting: false }, cx)));
            actions.into_any_element()
        };
        list = list.child(
            glass(div(), t, GlassKind::Float, Some(12.0), None)
                .flex()
                .items_center()
                .gap(px(8.0))
                .pl(px(12.0))
                .pr(px(6.0))
                .py(px(6.0))
                .text_size(px(m.text_control))
                .child(div().flex_none().child(icon(IconName::CornerDownRight, 14.0, t.muted)))
                .child(body)
                .child(trailing),
        );
    }
    Some(list.into_any_element())
}
