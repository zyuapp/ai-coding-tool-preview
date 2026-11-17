//! A bar filled to a share (Meter.tsx).

use gpui::{div, prelude::*, px, relative, App, Window};

use crate::theme::{ActiveTheme, Pane, Tone};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MeterSize {
    /// Runs under a header.
    Hair,
    /// Sits in a line of text.
    #[default]
    Thin,
    /// Cut into a card.
    Groove,
}

#[derive(IntoElement)]
pub struct Meter {
    value: f32,
    tone: Option<Tone>,
    size: MeterSize,
}

impl Meter {
    /// The share filled, from 0 to 1; anything outside is clamped.
    pub fn new(value: f32) -> Self {
        Self { value, tone: None, size: MeterSize::Thin }
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    pub fn size(mut self, size: MeterSize) -> Self {
        self.size = size;
        self
    }

    /// The filled share as the percentage the stylesheet draws, to a tenth.
    pub fn percent(value: f32) -> f32 {
        let value = if value.is_nan() { 0.0 } else { value };
        (value.clamp(0.0, 1.0) * 1000.0).round() / 10.0
    }
}

impl RenderOnce for Meter {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = &cx.theme().tokens;
        let fill_color = self.tone.map(|tone| t.tone(tone).tone).unwrap_or(t.meter_fill);
        let (height, radius) = match self.size {
            MeterSize::Hair => (3.0, None),
            MeterSize::Thin => (5.0, Some(3.0)),
            MeterSize::Groove => (8.0, None),
        };
        let round = |element: gpui::Div| match radius {
            Some(radius) => element.rounded(px(radius)),
            None => element.rounded_full(),
        };
        let fill = round(div().h_full().w(relative(Meter::percent(self.value) / 100.0)));
        let fill = if self.size == MeterSize::Groove { fill.bg(Pane::sheened(fill_color).background()) } else { fill.bg(fill_color) };
        let mut track = round(div().relative().w_full().h(px(height)).overflow_hidden().bg(t.meter_track));
        if self.size == MeterSize::Groove {
            // The groove's inset shadow and hairline lie under the fill, as a box's inset shadows do.
            let shadow = t.meter_inset;
            let faded = gpui::Rgba { a: 0.0, ..shadow };
            let top = gpui::Rgba { a: shadow.a * 0.85, ..shadow };
            track = track
                .child(div().absolute().top_0().left_0().w_full().h(px(2.0)).bg(gpui::linear_gradient(180.0, gpui::linear_color_stop(top, 0.0), gpui::linear_color_stop(faded, 1.0))))
                .child(div().absolute().top_0().left_0().size_full().rounded_full().border_1().border_color(t.line_glass));
        }
        track.child(div().absolute().top_0().left_0().size_full().child(fill))
    }
}
