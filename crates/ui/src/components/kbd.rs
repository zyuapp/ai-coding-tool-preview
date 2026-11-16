//! A keystroke (Kbd.tsx): a cap per key, or the keys run together as one chip inside a line.

use gpui::{div, prelude::*, px, App, FontWeight, SharedString, Window};

use crate::theme::ActiveTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KbdSize {
    #[default]
    Cap,
    Inline,
}

#[derive(IntoElement)]
pub struct Kbd {
    keys: Vec<SharedString>,
    size: KbdSize,
    flush: bool,
}

impl Kbd {
    /// The keys in the order they are held, as they should read.
    pub fn new<I, S>(keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SharedString>,
    {
        Self { keys: keys.into_iter().map(Into::into).collect(), size: KbdSize::Cap, flush: false }
    }

    pub fn inline<I, S>(keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SharedString>,
    {
        Self { size: KbdSize::Inline, ..Self::new(keys) }
    }

    pub fn size(mut self, size: KbdSize) -> Self {
        self.size = size;
        self
    }

    /// An inline chip with no gap before it.
    pub fn flush(mut self) -> Self {
        self.flush = true;
        self
    }
}

impl RenderOnce for Kbd {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = &cx.theme().tokens;
        match self.size {
            KbdSize::Inline => div()
                .flex_none()
                .ml(px(if self.flush { 0.0 } else { 2.0 }))
                .rounded(px(4.0))
                .px(px(5.0))
                .bg(tokens.chip)
                .text_color(tokens.ink_dim)
                .line_height(px(17.0))
                .map(crate::style::tabular_nums)
                .whitespace_nowrap()
                .child(self.keys.iter().map(|key| key.as_ref()).collect::<String>()),
            KbdSize::Cap => div().flex().items_center().gap(px(5.0)).children(self.keys.into_iter().map(|key| {
                div()
                    .min_w(px(30.0))
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .border_b_2()
                    .border_color(tokens.line_strong)
                    .rounded(px(8.0))
                    .px(px(8.0))
                    .bg(tokens.wash)
                    .text_color(tokens.ink)
                    .text_size(px(14.0))
                    .font_weight(FontWeight(570.0))
                    .line_height(gpui::relative(1.0))
                    .child(key)
            })),
        }
    }
}
