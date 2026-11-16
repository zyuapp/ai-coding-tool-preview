//! A pane of glass of a given kind (Surface.tsx). Kinds own their edge, corner, fill and shadow;
//! whoever places one sets only its position, size and padding.

use gpui::{div, prelude::*, AnyElement, App, Window};

pub use crate::style::GlassKind as SurfaceKind;
use crate::style::glass;
use crate::theme::{ActiveTheme, Tone};

#[derive(IntoElement)]
pub struct Surface {
    kind: SurfaceKind,
    tone: Option<Tone>,
    radius: Option<f32>,
    style: gpui::StyleRefinement,
    children: Vec<AnyElement>,
}

impl Surface {
    pub fn new(kind: SurfaceKind) -> Self {
        Self { kind, tone: None, radius: None, style: gpui::StyleRefinement::default(), children: Vec::new() }
    }

    /// Mixes the hue into the pane and edges it in that hue.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// `--glass-radius`: rounds this surface differently from its kind.
    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = Some(radius);
        self
    }
}

impl Styled for Surface {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Surface {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Surface {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = &cx.theme().tokens;
        let tone = self.tone.map(|tone| t.tone(tone));
        let mut surface = glass(div(), t, self.kind, self.radius, tone);
        surface.style().refine(&self.style);
        surface.children(self.children)
    }
}
