//! A card that opens from a control (`.glass-popover`): anything other than a list of rows, such
//! as a usage card. A press outside or Escape closes it; the trigger toggles it.

use std::rc::Rc;

use gpui::{
    actions, deferred, div, prelude::*, AnyElement, App, ElementId, FocusHandle, KeyBinding, MouseButton, Window,
};

use crate::overlay::{bounds_cell, bounds_probe, placed, Align, BoundsCell, Placement};
use crate::style::{glass, GlassKind};
use crate::theme::ActiveTheme;

actions!(popover, [PopoverDismiss]);

pub const KEY_CONTEXT: &str = "Popover";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", PopoverDismiss, Some(KEY_CONTEXT))]);
}

type Builder = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

#[derive(IntoElement)]
pub struct Popover {
    id: ElementId,
    trigger: Rc<dyn Fn(bool) -> AnyElement>,
    content: Builder,
    align: Align,
    radius: Option<f32>,
}

impl Popover {
    /// `trigger` is drawn given whether the card is open; a click on it toggles the card.
    pub fn new(
        id: impl Into<ElementId>,
        trigger: impl Fn(bool) -> AnyElement + 'static,
        content: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self { id: id.into(), trigger: Rc::new(trigger), content: Rc::new(content), align: Align::Start, radius: None }
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// `--glass-radius` for this card.
    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = Some(radius);
        self
    }
}

struct PopoverState {
    open: bool,
    probe: BoundsCell,
    focus: FocusHandle,
    restore: Option<FocusHandle>,
}

impl RenderOnce for Popover {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| PopoverState {
            open: false,
            probe: bounds_cell(),
            focus: cx.focus_handle(),
            restore: None,
        });
        let (open, probe, focus) = {
            let state = state.read(cx);
            (state.open, state.probe.clone(), state.focus.clone())
        };
        let close = {
            let state = state.clone();
            move |window: &mut Window, cx: &mut App| {
                let restore = state.update(cx, |state, cx| {
                    state.open = false;
                    cx.notify();
                    state.restore.take()
                });
                if let Some(restore) = restore {
                    window.focus(&restore, cx);
                }
            }
        };
        let toggle = {
            let state = state.clone();
            let close = close.clone();
            move |window: &mut Window, cx: &mut App| {
                if state.read(cx).open {
                    close(window, cx);
                    return;
                }
                let restore = window.focused(cx);
                let focus = state.read(cx).focus.clone();
                state.update(cx, |state, cx| {
                    state.open = true;
                    state.restore = restore;
                    cx.notify();
                });
                window.focus(&focus, cx);
            }
        };
        let trigger = div()
            .id(self.id.clone())
            .relative()
            .child(bounds_probe(&probe))
            .child((self.trigger)(open))
            .on_click(move |_, window, cx| toggle(window, cx));
        let card = (open && probe.get().is_some()).then(|| {
            let anchor = probe.get().unwrap_or_default();
            let content = (self.content)(window, cx);
            let t = &cx.theme().tokens;
            let outside = close.clone();
            let escape = close.clone();
            let exempt = probe.clone();
            let surface = glass(div(), t, GlassKind::Popover, self.radius, None)
                .id("popover-card")
                .map(crate::style::glass_in)
                .key_context(KEY_CONTEXT)
                .track_focus(&focus)
                .occlude()
                .on_action(move |_: &PopoverDismiss, window, cx| escape(window, cx))
                .on_mouse_down_out(move |event, window, cx| {
                    if !exempt.get().is_some_and(|area| area.contains(&event.position)) {
                        outside(window, cx);
                    }
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(content);
            deferred(placed(Placement::Anchor { anchor, align: self.align }, surface)).with_priority(60)
        });
        div().relative().flex().child(trigger).children(card)
    }
}
