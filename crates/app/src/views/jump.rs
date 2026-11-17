//! The palette that jumps to a thread or a setting (ThreadJump.tsx).

use super::letter_spacing::LetterSpacing;
use std::collections::HashSet;

use act_core::state::WorkspaceState;
use act_core::view::jump::{jump_view, JumpRow};
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::components::field::{SearchField, SearchVariant};
use act_ui::components::kbd::{Kbd, KbdSize};
use act_ui::input::{InputEvent, TextInput};
use act_ui::style::{glass, GlassKind, PaneFill};
use act_ui::style::Backdrop;
use act_ui::theme::ActiveTheme;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FontWeight, MouseButton, Subscription, Window};

use super::common::{engine_icon, icon, Ellipsis};
use super::root::Root;
use super::spinner::bead;
use crate::workspace::dispatch;

#[derive(Default)]
pub struct JumpState {
    search: Option<(Entity<TextInput>, Subscription)>,
    /// What had the keys when the palette opened, which gets them back if the palette leaves them nowhere.
    returns_to: Option<gpui::FocusHandle>,
    scroll: gpui::ScrollHandle,
    /// The row the list last scrolled to.
    followed: Option<usize>,
}

impl JumpState {
    pub fn closed(&mut self, window: &mut Window, cx: &mut gpui::App) {
        self.followed = None;
        let Some((input, _)) = self.search.take() else { return };
        if let Some(previous) = self.returns_to.take() {
            if window.focused(cx).is_none() || input.read(cx).is_focused(window) {
                window.focus(&previous, cx);
            }
        }
    }
}

fn choose(row: &JumpRow) -> AppCommand {
    match row {
        JumpRow::Thread { option, .. } => AppCommand::JumpChoose { task_id: option.id.clone() },
        JumpRow::Setting(option) => AppCommand::JumpChooseSetting { section: option.section, setting_id: option.setting_id.clone() },
    }
}

fn picked(cx: &gpui::App) -> Option<AppCommand> {
    let state = crate::workspace::workspace(cx).read(cx).state.clone();
    let busy: HashSet<String> = HashSet::new();
    let view = jump_view(&state, &busy)?;
    view.options.get(view.index as usize).map(choose)
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let busy: HashSet<String> = state.active_runs.keys().cloned().collect();
    let Some(view) = jump_view(state, &busy) else { return div().into_any_element() };
    if root.jump.search.is_none() {
        let input = cx.new(|cx| TextInput::new(cx).with_text(&view.query).placeholder("Search threads, pull requests, and settings").label("Search threads and settings"));
        let subscription = cx.subscribe(&input, |_, input, event: &InputEvent, cx| match event {
            InputEvent::Changed => {
                let query = input.read(cx).text().to_string();
                if crate::workspace::workspace(cx).read(cx).state.jump.as_ref().is_some_and(|jump| jump.query != query) {
                    dispatch(AppCommand::JumpQuery { query }, cx);
                }
            }
            InputEvent::Submit => {
                if let Some(command) = picked(cx) {
                    dispatch(command, cx);
                }
            }
            _ => {}
        });
        root.jump.returns_to = window.focused(cx);
        input.update(cx, |input, cx| input.focus(window, cx));
        root.jump.search = Some((input, subscription));
    }
    let Some((input, _)) = root.jump.search.as_ref() else { return div().into_any_element() };
    let input = input.clone();
    if input.read(cx).text() != view.query {
        let query = view.query.clone();
        input.update(cx, |input, cx| input.reset(&query, cx));
    }

    let first_setting = view.options.iter().position(|row| matches!(row, JumpRow::Setting(_)));
    let heading = |label: &'static str| div().mt(px(10.0)).mb(px(4.0)).px(px(9.0)).text_color(t.quiet).text_size(px(12.0)).font_weight(FontWeight(500.0)).child(label);
    let mut picked_child = None;
    let mut child_count = 0usize;
    let gutter = act_ui::components::scrollbar::gutter(&root.jump.scroll);
    let mut list = div().id("jump-list").accessible(gpui::AccessInfo::new(gpui::AccessRole::List, "Threads and settings")).max_h(px(336.0)).overflow_y_scroll().track_scroll(&root.jump.scroll).p(px(6.0)).pr(px(6.0) + gutter).flex().flex_col().gap(px(2.0));
    for (index, row) in view.options.iter().enumerate() {
        if index == 0 && first_setting != Some(0) {
            list = list.child(heading("Threads"));
            child_count += 1;
        }
        if Some(index) == first_setting {
            list = list.child(heading("Settings"));
            child_count += 1;
        }
        let selected = index as i64 == view.index;
        if selected {
            picked_child = Some(child_count);
        }
        child_count += 1;
        let command = choose(row);
        // Named by its words, as the page's option button is.
        let access = gpui::AccessInfo::new(gpui::AccessRole::ListItem, "").selected(selected);
        let mut line = div()
            .id(("jump-row", index))
            .skip_tab_stop()
            .accessible(access)
            .flex()
            .items_center()
            .gap(px(8.0))
            .min_w_0()
            .px(px(9.0))
            .py(px(7.0))
            .rounded(px(9.0))
            .text_size(px(14.0))
            .text_color(if selected { t.ink } else { t.ink_soft })
            .cursor_pointer()
            .map(|line| if selected { super::common::rimmed(line, t, 9.0).pane(t.glass_pick) } else { line.hover(|style| style.bg(t.wash)) })
            .on_click(move |_, _, cx| dispatch(command.clone(), cx));
        match row {
            JumpRow::Thread { option, running, named_pull_request } => {
                line = line.child(icon(engine_icon(option.engine), 14.0, t.quiet)).child(div().flex_1().min_w_0().ellipsis().child(option.title.clone()));
                if *running {
                    line = line.child(div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, "Working")).child(bead(format!("jump-run-{}", option.id), 12.0, t, false)));
                }
                if let Some(number) = named_pull_request {
                    line = line.child(div().flex_none().text_color(t.muted).text_size(px(12.0)).tabular_nums().child(format!("#{number}")));
                }
                if let Some(project) = &option.project {
                    line = line.child(div().flex_none().max_w(px(180.0)).ellipsis().text_color(t.muted).text_size(px(12.0)).child(project.clone()));
                }
            }
            JumpRow::Setting(option) => {
                line = line.child(div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, "Setting")).child(icon(IconName::Settings, 14.0, t.quiet))).child(div().flex_1().min_w_0().ellipsis().child(option.title.clone()));
                if let Some(page) = &option.page {
                    line = line.child(div().flex_none().text_color(t.muted).text_size(px(12.0)).child(page.clone()));
                }
            }
        }
        list = list.child(line);
    }
    if root.jump.followed != picked_child {
        root.jump.followed = picked_child;
        if let Some(child) = picked_child {
            root.jump.scroll.scroll_to_item(child);
        }
    }
    let body = if view.options.is_empty() {
        let mut empty = div().flex().items_center().justify_between().gap(px(12.0)).px(px(14.0)).py(px(12.0)).text_color(t.muted).text_size(px(12.0)).child("No matches");
        if !view.query.is_empty() {
            let field = input.clone();
            empty = empty.child(Button::new("jump-clear", "Clear search").variant(ButtonVariant::Soft).on_click(move |_, window, cx| {
                dispatch(AppCommand::JumpQuery { query: String::new() }, cx);
                field.update(cx, |field, cx| field.focus(window, cx));
            }));
        }
        empty.into_any_element()
    } else {
        div().relative().child(list).child(act_ui::components::scrollbar::Scrollbar::vertical("jump-scrollbar", &root.jump.scroll)).into_any_element()
    };
    let hint = |keys: &'static str, label: &'static str| div().flex().items_center().gap(px(4.0)).child(Kbd::new([keys]).size(KbdSize::Inline).flush()).child(label);
    let foot = div().flex().items_center().gap(px(14.0)).border_t_1().border_color(t.line_soft).px(px(14.0)).py(px(7.0)).text_color(t.quiet).text_size(px(11.0)).child(hint("↑↓", "Move")).child(hint("↵", "Open")).child(hint("esc", "Close"));
    let panel = glass(div(), t, GlassKind::Overlay, None, None)
        .w(px(520.0))
        .max_w_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .capture_key_down(|event, _, cx| {
            let command = match event.keystroke.key.as_str() {
                "down" => AppCommand::JumpStep { delta: 1 },
                "up" => AppCommand::JumpStep { delta: -1 },
                "escape" => AppCommand::JumpClose,
                _ => return,
            };
            cx.stop_propagation();
            dispatch(command, cx);
        })
        .child(SearchField::new("jump-search", &input).variant(SearchVariant::Palette))
        .child(body)
        .child(foot);
    div()
        .id("jump")
        .focus_trap()
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Dialog, "Jump to a thread or a setting").modal())
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(96.0))
        .px(px(28.0))
        .pb(px(28.0))
        .bg(t.veil)
        .veil_backdrop(t)
        .when(!t.reduced_motion, |scrim| scrim.enter(gpui::Transition::ease_out(140), |style| style.opacity(0.0)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| dispatch(AppCommand::JumpClose, cx))
        .child(div().id("jump-panel").when(!t.reduced_motion, |panel| {
            panel.enter(act_ui::style::panel_slide(160), |style| style.opacity(0.0).transform(gpui::ElementTransform::translate(px(0.0), px(-6.0)).scaled(0.985)))
        }).child(act_ui::style::under_veil(panel, t.veil, 16.0, t)))
        .into_any_element()
}
