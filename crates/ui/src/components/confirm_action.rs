//! A button that asks once more before it acts (ConfirmAction.tsx). The first press swaps it for
//! the destructive button and Cancel, with the keyboard on Cancel; any answer, or Escape, puts the
//! first button back. Moving the keyboard anywhere else takes the question back too.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{div, prelude::*, px, App, ElementId, FocusHandle, Pixels, SharedString, Window};

use super::button::{Button, ButtonSize, ButtonVariant};
use crate::icon::IconName;

#[derive(IntoElement)]
pub struct ConfirmAction {
    id: ElementId,
    label: SharedString,
    confirm: SharedString,
    icon: Option<IconName>,
    icon_size: Option<f32>,
    variant: ButtonVariant,
    size: ButtonSize,
    pill: bool,
    disabled: bool,
    tip: Option<SharedString>,
    name: Option<SharedString>,
    on_confirm: Rc<dyn Fn(&mut Window, &mut App)>,
    armed: Option<Rc<std::cell::Cell<bool>>>,
    gap: Pixels,
}

impl ConfirmAction {
    /// `confirm` labels the destructive button the first press reveals.
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        confirm: impl Into<SharedString>,
        on_confirm: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            confirm: confirm.into(),
            icon: None,
            icon_size: None,
            variant: ButtonVariant::Secondary,
            size: ButtonSize::Md,
            pill: false,
            disabled: false,
            tip: None,
            name: None,
            on_confirm: Rc::new(on_confirm),
            armed: None,
            gap: px(8.0),
        }
    }

    /// The space between the armed pair, which in the page sit in their parent's row and take its gap.
    pub fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
        self
    }

    /// What assistive technology calls the first button when its words alone do not say enough.
    pub fn name(mut self, name: impl Into<SharedString>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = Some(size);
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn pill(mut self, pill: bool) -> Self {
        self.pill = pill;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    /// Writes whether the question is being asked, so a row can hide the controls beside it meanwhile.
    pub fn armed_into(mut self, cell: &Rc<std::cell::Cell<bool>>) -> Self {
        self.armed = Some(cell.clone());
        self
    }
}

struct ConfirmState {
    confirming: bool,
    first: FocusHandle,
    cancel: FocusHandle,
    danger: FocusHandle,
    /// Focus should return to the first button once the question is answered.
    restore: bool,
    /// Cancel has held the keyboard since the question was asked.
    held: bool,
}

impl RenderOnce for ConfirmAction {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| ConfirmState {
            confirming: false,
            first: cx.focus_handle().tab_stop(true),
            cancel: cx.focus_handle().tab_stop(true),
            danger: cx.focus_handle().tab_stop(true),
            restore: false,
            held: false,
        });
        let (confirming, first, cancel, danger, restore) = {
            let state = state.read(cx);
            (state.confirming, state.first.clone(), state.cancel.clone(), state.danger.clone(), state.restore)
        };
        if confirming {
            if cancel.is_focused(window) || danger.is_focused(window) {
                if !state.read(cx).held {
                    state.update(cx, |state, _| state.held = true);
                }
            } else if state.read(cx).held {
                // The keyboard went elsewhere: the question is taken back, leaving the focus there.
                state.update(cx, |state, _| {
                    state.confirming = false;
                    state.held = false;
                    state.restore = false;
                });
            }
        } else if restore {
            window.focus(&first, cx);
            state.update(cx, |state, _| state.restore = false);
        }
        let answer = {
            let state = state.clone();
            move |cx: &mut App| {
                state.update(cx, |state, cx| {
                    state.confirming = false;
                    state.restore = true;
                    state.held = false;
                    cx.notify();
                })
            }
        };
        if let Some(cell) = &self.armed {
            let confirming = state.read(cx).confirming;
            if cell.replace(confirming) != confirming {
                window.on_next_frame(|window, _| window.refresh());
            }
        }
        if !state.read(cx).confirming {
            let start = state.clone();
            let mut button = Button::new(self.id.clone(), self.label.clone())
                .variant(self.variant)
                .size(self.size)
                .pill(self.pill)
                .disabled(self.disabled)
                .track_focus(&first)
                .on_click(move |_, window, cx| {
                    let cancel = start.read(cx).cancel.clone();
                    start.update(cx, |state, cx| {
                        state.confirming = true;
                        state.held = false;
                        state.restore = false;
                        cx.notify();
                    });
                    window.focus(&cancel, cx);
                });
            if let Some(icon) = self.icon {
                button = button.icon(icon);
            }
            if let Some(size) = self.icon_size {
                button = button.icon_size(size);
            }
            if let Some(tip) = self.tip {
                button = button.tip(tip);
            }
            if let Some(name) = self.name {
                button = button.name(name);
            }
            return div().flex().gap(self.gap).child(button).into_any_element();
        }
        let quiet = matches!(self.variant, ButtonVariant::Secondary | ButtonVariant::Soft | ButtonVariant::Ghost | ButtonVariant::Link);
        let cancel_variant = if quiet { self.variant } else { ButtonVariant::Secondary };
        let on_confirm = self.on_confirm.clone();
        let confirmed = answer.clone();
        let cancelled = answer.clone();
        let escaped = answer;
        div()
            .flex()
            .gap(self.gap)
            .on_key_down(move |event, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    escaped(cx);
                }
            })
            .child(
                Button::new(ElementId::NamedChild(Arc::new(self.id.clone()), "confirm".into()), self.confirm)
                    .variant(ButtonVariant::Danger)
                    .armed(true)
                    .size(self.size)
                    .pill(self.pill)
                    .track_focus(&danger)
                    .on_click(move |_, window, cx| {
                        confirmed(cx);
                        on_confirm(window, cx);
                    }),
            )
            .child(
                Button::new(ElementId::NamedChild(Arc::new(self.id), "cancel".into()), "Cancel")
                    .variant(cancel_variant)
                    .size(self.size)
                    .pill(self.pill)
                    .track_focus(&cancel)
                    .on_click(move |_, _, cx| cancelled(cx)),
            )
            .into_any_element()
    }
}
