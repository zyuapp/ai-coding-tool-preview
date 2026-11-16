//! The head of a panel (PanelHeader.tsx): one row as tall as the window's chrome, its title, and
//! what closes it; and the toolbar strip under it.

use std::rc::Rc;

use gpui::{div, prelude::*, px, AnyElement, App, ClickEvent, ElementId, FontWeight, SharedString, Window};

use super::button::{IconButton, IconButtonSize};
use crate::icon::IconName;
use crate::theme::ActiveTheme;

#[derive(IntoElement)]
pub struct PanelHeader {
    id: ElementId,
    icon: Option<AnyElement>,
    title: SharedString,
    subtitle: Option<SharedString>,
    actions: Vec<AnyElement>,
    children: Vec<AnyElement>,
    on_close: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    close_label: SharedString,
    close_tip: Option<SharedString>,
    height: Option<f32>,
    subtitle_color: Option<gpui::Rgba>,
    border: bool,
}

impl PanelHeader {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            title: title.into(),
            subtitle: None,
            actions: Vec::new(),
            children: Vec::new(),
            on_close: None,
            close_label: "Close".into(),
            close_tip: None,
            height: None,
            subtitle_color: None,
            border: true,
        }
    }

    /// A mark before the title, drawn by the caller at its own size.
    pub fn icon(mut self, icon: impl IntoElement) -> Self {
        self.icon = Some(icon.into_any_element());
        self
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Controls at the trailing edge, before the close button.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub fn on_close(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }

    pub fn close_label(mut self, label: impl Into<SharedString>) -> Self {
        self.close_label = label.into();
        self
    }

    pub fn close_tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.close_tip = Some(tip.into());
        self
    }

    /// The subtitle's colour, where it says how something stands.
    pub fn subtitle_color(mut self, color: gpui::Rgba) -> Self {
        self.subtitle_color = Some(color);
        self
    }

    /// Leaves the bottom edge to what is drawn under the header.
    pub fn borderless(mut self) -> Self {
        self.border = false;
        self
    }

    /// `--panel-header-height`, for a panel inset from the chrome.
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }
}

impl ParentElement for PanelHeader {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for PanelHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let mut text = div().min_w_0().child(
            div()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(t.ink_soft)
                .text_size(px(m.text_heading))
                .font_weight(FontWeight(580.0))
                .map(|title| crate::style::letter_spacing(title, -0.018))
                .child(self.title),
        );
        if let Some(subtitle) = self.subtitle {
            text = text.child(
                div()
                    .mt(px(4.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(self.subtitle_color.unwrap_or(t.quiet))
                    .text_size(px(m.text_meta))
                    .child(subtitle),
            );
        }
        let mut title = div().flex_1().min_w_0().flex().items_center().gap(px(11.0));
        if let Some(icon) = self.icon {
            title = title.child(div().flex_none().flex().items_center().justify_center().child(icon));
        }
        title = title.child(text);
        let mut header = div()
            .id(self.id.clone())
            .min_w_0()
            .min_h(px(self.height.unwrap_or(m.chrome_head)))
            .flex()
            .items_center()
            .gap(px(14.0))
            .when(self.border, |header| header.border_b_1().border_color(t.line))
            .px(px(20.0))
            .child(title)
            .children(self.children);
        if !self.actions.is_empty() {
            header = header.child(div().flex_none().flex().items_center().gap(px(6.0)).children(self.actions));
        }
        if let Some(handler) = self.on_close {
            let mut close = IconButton::new(
                ElementId::NamedChild(Box::new(self.id), "close".into()),
                IconName::X,
                self.close_label,
            )
            .size(IconButtonSize::Lg)
            .icon_size(18.0)
            .on_click(move |event, window, cx| handler(event, window, cx));
            if let Some(tip) = self.close_tip {
                close = close.tip(tip);
            }
            header = header.child(div().flex_none().mr(px(-3.0)).child(close));
        }
        header
    }
}

/// A strip of small icon buttons and fields under a panel's head.
pub fn toolbar(cx: &App) -> gpui::Div {
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(4.0))
        .border_b_1()
        .border_color(cx.theme().tokens.line_soft)
        .px(px(10.0))
        .py(px(8.0))
}
