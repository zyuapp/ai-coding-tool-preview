//! List rows and the heading over a list (List.tsx).

use std::rc::Rc;

use gpui::{div, prelude::*, px, AnyElement, App, ClickEvent, CursorStyle, ElementId, FocusHandle, FontWeight, SharedString, Window};

use super::chip::Chip;
use crate::icon::{Icon, IconName};
use crate::theme::{ActiveTheme, Tone};
use crate::tooltip::Tippable;

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// One row: a mark in a 24px lead column, a title over its detail, then trailing marks.
#[derive(IntoElement)]
pub struct ListRow {
    id: ElementId,
    leading: Option<AnyElement>,
    title: SharedString,
    detail: Option<SharedString>,
    trailing: Vec<AnyElement>,
    selected: bool,
    plain: bool,
    disabled: bool,
    on_click: Option<Handler>,
    focus: Option<FocusHandle>,
    detail_color: Option<gpui::Rgba>,
    inset: Option<(f32, f32)>,
    pad_y: Option<f32>,
    name: Option<SharedString>,
}

impl ListRow {
    /// What assistive technology calls the row when its title alone does not say enough.
    pub fn name(mut self, name: impl Into<SharedString>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            leading: None,
            title: title.into(),
            detail: None,
            trailing: Vec::new(),
            selected: false,
            plain: false,
            disabled: false,
            on_click: None,
            focus: None,
            detail_color: None,
            inset: None,
            pad_y: None,
            name: None,
        }
    }

    /// Side padding and leading column width, for a list that sits tighter than the default 8px and 24px.
    pub fn inset(mut self, padding: f32, lead: f32) -> Self {
        self.inset = Some((padding, lead));
        self
    }

    /// Top and bottom padding, for a list whose rows stand taller than the default 5px.
    pub fn pad_y(mut self, padding: f32) -> Self {
        self.pad_y = Some(padding);
        self
    }

    /// Makes the row a keyboard stop: Enter or Space presses it, and it rings while the keyboard is on it.
    pub fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    pub fn detail_color(mut self, color: gpui::Rgba) -> Self {
        self.detail_color = Some(color);
        self
    }

    pub fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn trailing(mut self, trailing: impl IntoElement) -> Self {
        self.trailing.push(trailing.into_any_element());
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Sets the title in the body weight, for a list of plain links.
    pub fn plain(mut self, plain: bool) -> Self {
        self.plain = plain;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ListRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ring = self.focus.as_ref().is_some_and(|focus| focus.is_focused(window)) && crate::focus::keyboard_focus_visible(cx);
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let clickable = self.on_click.is_some() && !self.disabled;
        let mut row = div()
            .id(self.id)
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::ListItem, self.name.clone().unwrap_or_else(|| self.title.clone())).selected(self.selected).disabled(self.disabled))
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded(px(9.0))
            .px(px(self.inset.map_or(8.0, |inset| inset.0)))
            .py(px(self.pad_y.unwrap_or(5.0)))
            .text_color(if self.selected { t.ink } else { t.ink_faint });
        if self.selected {
            row = row.bg(t.row_selected);
        }
        if clickable {
            let (fill, color) = (t.surface_hover, t.ink_soft);
            row = row.cursor(CursorStyle::PointingHand).transition(crate::style::touch());
            if !self.selected {
                row = row.hover(move |style| style.bg(fill).text_color(color));
            }
        }
        if let Some(leading) = self.leading {
            row = row.child(div().flex_none().w(px(self.inset.map_or(24.0, |inset| inset.1))).flex().items_center().justify_center().child(leading));
        }
        let title = div()
            .id("title")
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .text_size(px(m.text_control))
            .font_weight(if self.plain { FontWeight::NORMAL } else { FontWeight(560.0) })
            .child(self.title.clone());
        let mut text = div().flex_1().min_w_0().flex().flex_col().gap(px(2.0)).child(title);
        if let Some(detail) = self.detail {
            text = text.child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(self.detail_color.unwrap_or(t.muted_soft))
                    .text_size(px(m.text_meta))
                    .child(detail),
            );
        }
        row = row.child(text);
        if !self.trailing.is_empty() {
            row = row.child(div().flex_none().flex().items_center().gap(px(6.0)).children(self.trailing));
        }
        if let (true, Some(handler)) = (clickable, self.on_click) {
            row = row.on_click(move |event, window, cx| handler(event, window, cx));
        }
        if let Some(focus) = &self.focus {
            row = row.relative().track_focus(focus);
        }
        if ring {
            row = row.child(div().absolute().top_0().left_0().size_full().rounded(px(9.0)).border_2().border_color(t.focus_ring));
        }
        row
    }
}

/// The label over a list: its count, its fold, and at most one action.
#[derive(IntoElement)]
pub struct ListHeading {
    id: ElementId,
    label: SharedString,
    count: Option<SharedString>,
    count_tone: Option<Tone>,
    open: Option<bool>,
    on_toggle: Option<Handler>,
    tip: Option<SharedString>,
    action: Option<AnyElement>,
    sticky: bool,
    focus: Option<FocusHandle>,
}

impl ListHeading {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            count: None,
            count_tone: None,
            open: None,
            on_toggle: None,
            tip: None,
            action: None,
            sticky: false,
            focus: None,
        }
    }

    pub fn count(mut self, count: impl Into<SharedString>) -> Self {
        self.count = Some(count.into());
        self
    }

    pub fn count_tone(mut self, tone: Tone) -> Self {
        self.count_tone = Some(tone);
        self
    }

    /// Makes the label a button that folds the list, with a chevron saying which way it is.
    pub fn fold(mut self, open: bool, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.open = Some(open);
        self.on_toggle = Some(Rc::new(handler));
        self
    }

    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }

    /// Makes the fold a keyboard stop.
    pub fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    /// Stays on the list's ground while rows pass under it; the caller pins it.
    pub fn sticky(mut self, sticky: bool) -> Self {
        self.sticky = sticky;
        self
    }
}

impl RenderOnce for ListHeading {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ring = self.focus.as_ref().is_some_and(|focus| focus.is_focused(window)) && crate::focus::keyboard_focus_visible(cx);
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let mut label = div()
            .id("label")
            .flex_1()
            .min_w_0()
            .min_h(px(30.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(self.label));
        if let Some(count) = self.count {
            label = label.child(Chip::count(count).tone(self.count_tone));
        }
        if let (Some(open), Some(handler)) = (self.open, self.on_toggle) {
            let hover = t.ink_dim;
            label = label
                .child(
                    div()
                        .id("chevron")
                        .flex_none()
                        .flex()
                        .transition(gpui::Transition::ease(120).only(&[gpui::TransitionProperty::Transform]))
                        .transform(gpui::ElementTransform::rotate(if open { 0.0 } else { -90.0 }))
                        .child(Icon::new(IconName::HeadingChevron).size(px(12.0))),
                )
                .cursor(CursorStyle::PointingHand)
                .hover(move |style| style.text_color(hover))
                .on_click(move |event, window, cx| handler(event, window, cx));
            if let Some(tip) = self.tip {
                label = label.tip(tip);
            }
            if let Some(focus) = &self.focus {
                label = label.relative().track_focus(focus);
            }
            if ring {
                label = label.child(div().absolute().top_0().left(px(-4.0)).right(px(-4.0)).bottom_0().rounded(px(7.0)).border_2().border_color(t.focus_ring));
            }
        }
        let mut heading = div()
            .id(self.id)
            .min_h(px(30.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .text_color(t.quiet_strong)
            .text_size(px(m.text_menu))
            .font_weight(FontWeight::SEMIBOLD)
            .child(label);
        if self.sticky {
            heading = heading.bg(t.sunken);
        }
        if let Some(action) = self.action {
            heading = heading.child(div().flex_none().flex().items_center().gap(px(4.0)).child(action));
        }
        heading
    }
}
