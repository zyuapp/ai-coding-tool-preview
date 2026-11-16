//! A choice of several drawn as a field with a chevron (Select in Field.tsx). Where the web app
//! defers to the native select, this opens the app's own menu of options, ticked at the current one.

use std::rc::Rc;

use gpui::{div, prelude::*, px, App, CursorStyle, DismissEvent, ElementId, Entity, SharedString, Subscription, Window};

use super::menu::{Menu, MenuEntry, MenuItem};
use crate::icon::{Icon, IconName};
use crate::overlay::{bounds_cell, bounds_probe, Align, BoundsCell, Placement};
use crate::style::{Corners, PaneFill, Raised};
use crate::theme::{ActiveTheme, Pane};

#[derive(Clone, Debug)]
pub struct SelectOption {
    pub value: SharedString,
    pub label: SharedString,
    pub disabled: bool,
}

impl SelectOption {
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self { value: value.into(), label: label.into(), disabled: false }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

pub type SelectHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Select {
    id: ElementId,
    options: Vec<SelectOption>,
    value: SharedString,
    disabled: bool,
    invalid: bool,
    on_change: Option<SelectHandler>,
}

impl Select {
    pub fn new(id: impl Into<ElementId>, options: Vec<SelectOption>, value: impl Into<SharedString>) -> Self {
        Self { id: id.into(), options, value: value.into(), disabled: false, invalid: false, on_change: None }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

struct SelectState {
    menu: Option<Entity<Menu>>,
    probe: BoundsCell,
    _subscription: Option<Subscription>,
}

impl RenderOnce for Select {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| SelectState { menu: None, probe: bounds_cell(), _subscription: None });
        let (menu, probe) = {
            let state = state.read(cx);
            (state.menu.clone(), state.probe.clone())
        };
        if let (Some(menu), Some(anchor)) = (&menu, probe.get()) {
            menu.update(cx, |menu, cx| menu.set_placement(Placement::Anchor { anchor, align: Align::Start }, cx));
        }
        let theme = cx.theme();
        let t = &theme.tokens;
        let label = self
            .options
            .iter()
            .find(|option| option.value == self.value)
            .map(|option| option.label.clone())
            .unwrap_or_default();
        let mut field = div()
            .id(self.id.clone())
            .relative()
            .min_w_0()
            .max_w(px(260.0))
            .flex()
            .items_center()
            .border_1()
            .border_color(if self.invalid { t.danger_line } else { t.line_strong })
            .rounded(px(7.0))
            .pl(px(12.0))
            .pr(px(34.0))
            .py(px(8.0))
            .pane(Pane::solid(t.surface))
            .rim(t.glass_rim, Corners::Radius(7.0))
            .text_color(t.ink)
            .text_size(px(theme.metrics.text_meta))
            .child(bounds_probe(&probe))
            // As a native select, the field is as wide as its widest option.
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
                    .children(self.options.iter().map(|option| div().h(px(0.0)).overflow_hidden().invisible().whitespace_nowrap().child(option.label.clone()))),
            )
            .child(
                div()
                    .absolute()
                    .right(px(11.0))
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .text_color(t.muted)
                    .child(Icon::new(IconName::ChevronDown).size(px(14.0))),
            );
        if self.disabled {
            field = field.opacity(t.disabled_opacity);
        } else {
            let options = self.options.clone();
            let value = self.value.clone();
            let on_change = self.on_change.clone();
            field = field.cursor(CursorStyle::PointingHand).on_click(move |_, window, cx| {
                if state.read(cx).menu.is_some() {
                    state.update(cx, |state, _| {
                        state.menu = None;
                        state._subscription = None;
                    });
                    return;
                }
                let anchor = state.read(cx).probe.get().unwrap_or_default();
                let entries: Vec<MenuEntry> = options
                    .iter()
                    .map(|option| {
                        let chosen = option.value.clone();
                        let on_change = on_change.clone();
                        MenuItem::new(option.label.clone())
                            .checked(option.value == value)
                            .disabled(option.disabled)
                            .on_select(move |window, cx| {
                                if let Some(handler) = &on_change {
                                    handler(&chosen, window, cx);
                                }
                            })
                            .into()
                    })
                    .collect();
                let width = anchor.size.width;
                let probe = state.read(cx).probe.clone();
                let menu = Menu::build(entries, Placement::Anchor { anchor, align: Align::Start }, window, cx, move |menu| {
                    menu.min_width(width).max_width(px(320.0)).exempt(&probe)
                });
                let weak = state.downgrade();
                let subscription = cx.subscribe(&menu, move |_, _: &DismissEvent, cx| {
                    if let Some(state) = weak.upgrade() {
                        state.update(cx, |state, cx| {
                            state.menu = None;
                            state._subscription = None;
                            cx.notify();
                        });
                    }
                });
                state.update(cx, |state, cx| {
                    state.menu = Some(menu);
                    state._subscription = Some(subscription);
                    cx.notify();
                });
            });
        }
        div().relative().flex().child(field).children(menu)
    }
}
