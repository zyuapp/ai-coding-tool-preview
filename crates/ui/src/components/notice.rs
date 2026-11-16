//! A message beside what it is about (Notice.tsx): a tinted strip in the flow, or a banner of
//! glass floating over the composer.

use std::rc::Rc;

use gpui::{div, prelude::*, px, AnyElement, App, ClickEvent, ElementId, SharedString, Window};

use super::button::{IconButton, IconButtonSize};
use crate::icon::{Icon, IconName};
use crate::style::{glass, Corners, GlassKind, PaneFill, Raised};
use crate::theme::{ActiveTheme, Pane, Tone};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NoticeTone {
    Info,
    Accent,
    Warning,
    Danger,
    #[default]
    Neutral,
}

impl NoticeTone {
    fn tone(self) -> Option<Tone> {
        match self {
            NoticeTone::Info => Some(Tone::Info),
            NoticeTone::Accent => Some(Tone::Accent),
            NoticeTone::Warning => Some(Tone::Warning),
            NoticeTone::Danger => Some(Tone::Danger),
            NoticeTone::Neutral => None,
        }
    }
}

#[derive(IntoElement)]
pub struct Notice {
    id: ElementId,
    tone: NoticeTone,
    icon: Option<IconName>,
    floating: bool,
    text: AnyElement,
    actions: Vec<AnyElement>,
    on_dismiss: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    dismiss_label: SharedString,
    dismiss_tip: Option<SharedString>,
}

impl Notice {
    pub fn new(id: impl Into<ElementId>, text: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            tone: NoticeTone::Neutral,
            icon: None,
            floating: false,
            text: text.into_any_element(),
            actions: Vec::new(),
            on_dismiss: None,
            dismiss_label: "Dismiss".into(),
            dismiss_tip: None,
        }
    }

    pub fn tone(mut self, tone: NoticeTone) -> Self {
        self.tone = tone;
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A banner of glass over the composer, rather than a strip in the flow.
    pub fn floating(mut self, floating: bool) -> Self {
        self.floating = floating;
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    /// Draws a close button; the notice stays until the caller stops drawing it.
    pub fn on_dismiss(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }

    pub fn dismiss_label(mut self, label: impl Into<SharedString>) -> Self {
        self.dismiss_label = label.into();
        self
    }

    pub fn dismiss_tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.dismiss_tip = Some(tip.into());
        self
    }
}

impl RenderOnce for Notice {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let tone = self.tone.tone().map(|tone| t.tone(tone));
        let color = if self.tone == NoticeTone::Danger { t.danger } else { t.ink_dim };
        let mut notice = if self.floating {
            glass(div(), t, GlassKind::Float, Some(12.0), tone)
                .max_w(px(860.0))
                .min_h(px(38.0))
                .mx_auto()
                .pl(px(12.0))
                .pr(px(5.0))
                .py(px(4.0))
                .gap(px(8.0))
                .text_size(px(m.text_control))
        } else {
            let fill = match tone {
                Some(tone) => Pane::sheened(tone.surface),
                None => t.glass_control,
            };
            div()
                .relative()
                .gap(px(10.0))
                .border_1()
                .border_color(tone.map(|tone| tone.line).unwrap_or(t.line_glass))
                .rounded(px(11.0))
                .pl(px(11.0))
                .pr(px(7.0))
                .py(px(7.0))
                .pane(fill)
                .rim(t.glass_rim, Corners::Radius(11.0))
                .text_size(px(m.text_meta))
        };
        notice = notice.flex().items_center().min_w_0().text_color(color);
        if let Some(icon) = self.icon {
            let glyph_color = match (self.tone, tone) {
                (NoticeTone::Danger, _) | (_, None) => color,
                (_, Some(tone)) => tone.text,
            };
            notice = notice.child(div().flex_none().flex().items_center().child(Icon::new(icon).size(px(14.0)).color(glyph_color)));
        }
        notice = notice.child(div().flex_1().min_w_0().child(self.text));
        if !self.actions.is_empty() {
            notice = notice.child(div().flex_none().flex().items_center().gap(px(6.0)).children(self.actions));
        }
        if let Some(handler) = self.on_dismiss {
            let mut close = IconButton::new(self.id.clone(), IconName::X, self.dismiss_label.clone())
                .size(IconButtonSize::Xs)
                .icon_size(12.0)
                .on_click(move |event, window, cx| handler(event, window, cx));
            if let Some(tip) = self.dismiss_tip {
                close = close.tip(tip);
            }
            notice = notice.child(close);
        }
        notice
    }
}
