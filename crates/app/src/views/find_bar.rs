//! The find bar over a transcript or a dock view (FindBar.tsx). It hangs from the top-right corner of
//! what it searches and takes no room of its own.

use super::letter_spacing::LetterSpacing;
use act_core::domain::find::FindTarget;
use act_core::state::WorkspaceState;
use act_core::view::find::find_view;
use act_core::AppCommand;
use act_ui::components::button::IconButton;
use act_ui::components::field::TextField;
use act_ui::input::{InputEvent, TextInput};
use act_ui::style::{glass, GlassKind};
use act_ui::theme::ActiveTheme;
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, Subscription, Window};

use super::common::icon;
use super::root::Root;
use crate::workspace::dispatch;

#[derive(Default)]
pub struct FindState {
    search: Option<(Entity<TextInput>, Subscription)>,
    focused: i64,
}

fn label(target: &FindTarget) -> String {
    match target {
        FindTarget::Thread { .. } => "thread".into(),
        FindTarget::Browser { .. } => "page".into(),
        FindTarget::Terminal { .. } => "terminal".into(),
        FindTarget::Review { .. } => "review".into(),
        FindTarget::Panel { panel, .. } => panel.clone(),
    }
}

/// The bar, when find is open on a view in this place: the transcript's page, or the dock.
pub fn render(root: &mut Root, state: &WorkspaceState, in_page: bool, window: &mut Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    let Some(view) = find_view(state) else {
        root.find.search = None;
        return None;
    };
    let in_dock_tab = matches!(&view.find.target, FindTarget::Thread { task_id: Some(id) } if act_core::state::dock_holds_tab(state, id));
    let thread = matches!(view.find.target, FindTarget::Thread { .. }) && !in_dock_tab;
    if thread != in_page {
        return None;
    }
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let named = label(&view.find.target);
    if root.find.search.is_none() {
        let input = cx.new(|cx| TextInput::new(cx).with_text(&view.find.query).label(format!("Find in {named}")).placeholder(format!("Find in {named}")));
        let subscription = cx.subscribe(&input, |_, input, event: &InputEvent, cx| match event {
            InputEvent::Changed => {
                let query = input.read(cx).text().to_string();
                if crate::workspace::workspace(cx).read(cx).state.find.as_ref().is_some_and(|find| find.query != query) {
                    dispatch(AppCommand::FindQuery { query }, cx);
                }
            }
            InputEvent::Submit => dispatch(AppCommand::FindStep { delta: 1 }, cx),
            _ => {}
        });
        root.find.search = Some((input, subscription));
        root.find.focused = view.find.focus - 1;
    }
    let input = root.find.search.as_ref().map(|(input, _)| input.clone())?;
    if input.read(cx).text() != view.find.query {
        let query = view.find.query.clone();
        input.update(cx, |input, cx| input.reset(&query, cx));
    }
    if root.find.focused != view.find.focus {
        root.find.focused = view.find.focus;
        input.update(cx, |input, cx| {
            input.focus(window);
            input.select_all_text(cx);
        });
    }

    let searching = !view.find.query.trim().is_empty();
    let count = if !searching {
        String::new()
    } else if view.matches == 0 {
        if view.counting { "Counting…".into() } else { "No matches".into() }
    } else {
        format!("{}/{}{}", view.find.index + 1, view.matches, if view.counting { "+" } else { "" })
    };
    let empty = searching && view.matches == 0 && !view.counting;
    let some = view.matches > 0;
    let bar = glass(div(), t, GlassKind::Popover, None, None)
        .id("find-bar-card")
        .map(act_ui::style::glass_in)
        .absolute()
        .top(px(if thread { 12.0 } else { 10.0 }))
        .right(px(if !thread {
            12.0
        } else {
            let width = f32::from(window.viewport_size().width);
            let reserved = super::session_panel::reserved(state);
            cx.theme().metrics.page_inset(width) + if width <= super::session_panel::NARROW_WINDOW && reserved > 0.0 { 272.0 } else { reserved }
        }))
        .w(px(if thread { 420.0 } else { 340.0 }))
        .max_w(gpui::relative(0.92))
        .flex()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(6.0))
        .capture_key_down(|event, _, cx| {
            let command = match (event.keystroke.key.as_str(), event.keystroke.modifiers.shift) {
                ("escape", _) => AppCommand::FindClose,
                ("enter", true) => AppCommand::FindStep { delta: -1 },
                _ => return,
            };
            cx.stop_propagation();
            dispatch(command, cx);
        })
        .child(icon(IconName::Search, 14.0, t.quiet))
        .child(div().flex_1().min_w_0().child(TextField::new("find-field", &input)))
        .child(div().flex_none().text_size(px(12.0)).tabular_nums().text_color(if empty { t.danger_soft } else { t.quiet }).child(count))
        .child(IconButton::new("find-previous", IconName::ChevronUp, "Previous match").tip("Previous match ⇧↩").disabled(!some).on_click(|_, _, cx| dispatch(AppCommand::FindStep { delta: -1 }, cx)))
        .child(IconButton::new("find-next", IconName::ChevronDown, "Next match").tip("Next match ↩").disabled(!some).on_click(|_, _, cx| dispatch(AppCommand::FindStep { delta: 1 }, cx)))
        .child(IconButton::new("find-close", IconName::X, "Close find").tip("Close ⎋").on_click(|_, _, cx| dispatch(AppCommand::FindClose, cx)));
    Some(div().absolute().top_0().left_0().right_0().h(px(0.0)).child(bar).into_any_element())
}
