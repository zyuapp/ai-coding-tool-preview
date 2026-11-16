//! The window's own chrome: the bars it drags by (`-webkit-app-region: drag`), and the controls on
//! them that keep their clicks.

use std::cell::Cell;

use gpui::{div, prelude::*, AnyElement, Div, MouseButton, Pixels, Point, Stateful};

thread_local! {
    /// Where a press on a drag bar began, until it moves far enough to drag the window.
    static PRESSED: Cell<Option<Point<Pixels>>> = const { Cell::new(None) };
}

/// How far the pointer travels before a press on a bar becomes a window drag.
const DRAG_SLOP: f32 = 3.0;

/// A layer behind a bar's contents that drags the window, and zooms it on a double-click as the
/// title bar does. Controls over it are wrapped in [`no_drag`] so they keep their own presses. Only
/// a window drawn under its title bar drags by its content; a native frame drags by its own.
pub fn drag_layer(id: &'static str) -> Stateful<Div> {
    let layer = div().id(id).absolute().top_0().left_0().size_full();
    if !cfg!(target_os = "macos") {
        return layer;
    }
    layer
        .on_mouse_down(MouseButton::Left, |event, window, _| {
            if event.click_count == 2 {
                PRESSED.set(None);
                window.titlebar_double_click();
            } else {
                PRESSED.set(Some(event.position));
            }
        })
        .on_mouse_up(MouseButton::Left, |_, _, _| PRESSED.set(None))
        .on_mouse_move(|event, window, _| {
            let Some(start) = PRESSED.get() else { return };
            if !event.dragging() {
                PRESSED.set(None);
                return;
            }
            let moved = event.position - start;
            if f32::from(moved.x).abs() > DRAG_SLOP || f32::from(moved.y).abs() > DRAG_SLOP {
                PRESSED.set(None);
                window.start_window_move();
            }
        })
}

/// A control on a drag bar: it takes its presses, and the bar under it does not.
pub fn no_drag(element: impl IntoElement) -> AnyElement {
    div().occlude().flex().flex_none().items_center().child(element).into_any_element()
}

/// Like [`no_drag`], for a control that shrinks with the bar.
pub fn no_drag_shrink(element: impl IntoElement) -> AnyElement {
    div().occlude().flex().min_w_0().flex_shrink().items_center().child(element).into_any_element()
}

/// A tooltip naming a control and the keystroke for it, written the way this platform writes keys.
pub fn key_tip(label: &str, binding: &str) -> gpui::SharedString {
    act_ui::tooltip::shortcut_tip(label, &act_core::domain::shortcuts::display_shortcut(binding, cfg!(target_os = "macos")))
}
