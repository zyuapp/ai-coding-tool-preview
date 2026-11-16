//! Small labels set beside or inside a line of text (Chip.tsx): a count, a tag, or a link.

use std::rc::Rc;

use gpui::{div, prelude::*, px, App, ClickEvent, CursorStyle, ElementId, FontWeight, SharedString, Window};

use crate::style::{Corners, PaneFill, Raised};
use crate::theme::{ActiveTheme, Tone};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipVariant {
    /// A count beside a label.
    Count,
    /// A short fact about the thing it sits beside.
    Tag,
    /// A link into the app, set in the line as a chip of glass.
    Link,
}

#[derive(IntoElement)]
pub struct Chip {
    id: Option<ElementId>,
    label: SharedString,
    variant: ChipVariant,
    tone: Option<Tone>,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl Chip {
    pub fn new(variant: ChipVariant, label: impl Into<SharedString>) -> Self {
        Self { id: None, label: label.into(), variant, tone: None, on_click: None }
    }

    pub fn count(label: impl Into<SharedString>) -> Self {
        Self::new(ChipVariant::Count, label)
    }

    pub fn tag(label: impl Into<SharedString>) -> Self {
        Self::new(ChipVariant::Tag, label)
    }

    pub fn link(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self { id: Some(id.into()), ..Self::new(ChipVariant::Link, label) }
    }

    pub fn tone(mut self, tone: Option<Tone>) -> Self {
        self.tone = tone;
        self
    }

    pub fn on_click(mut self, id: impl Into<ElementId>, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.id = Some(id.into());
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Chip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let tone = self.tone.map(|tone| t.tone(tone));
        let base = div().relative().flex_none().whitespace_nowrap();
        let chip = match self.variant {
            ChipVariant::Count => base
                .min_w(px(18.0))
                .h(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .px(px(6.0))
                .bg(tone.map(|tone| tone.chip).unwrap_or(t.surface_active))
                .text_color(tone.map(|tone| tone.text).unwrap_or(t.muted))
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .line_height(gpui::relative(1.0))
                .map(crate::style::tabular_nums)
                .rim(t.glass_rim, Corners::Full),
            ChipVariant::Tag => base
                .border_1()
                .border_color(tone.map(|tone| tone.line).unwrap_or(t.line_mid))
                .rounded(px(5.0))
                .px(px(5.0))
                .py(px(1.0))
                .bg(t.wash)
                .text_color(tone.map(|tone| tone.soft).unwrap_or(t.muted_soft))
                .text_size(px(theme.metrics.text_badge))
                .font_weight(FontWeight(560.0))
                .rim(t.glass_rim, Corners::Radius(5.0)),
            ChipVariant::Link => base
                .rounded(px(7.0))
                .px(px(7.0))
                .py(px(2.0))
                .pane(t.glass_control)
                .text_color(t.accent_text)
                .rim(t.glass_rim, Corners::Radius(7.0)),
        };
        let chip = chip.child(self.label);
        match self.id {
            Some(id) => {
                let mut chip = chip.id(id);
                if self.variant == ChipVariant::Link {
                    let (fill, color) = (t.glass_control_hover, t.accent_soft);
                    chip = chip.cursor(CursorStyle::PointingHand).transition(gpui::Transition::ease(120)).hover(move |style| style.bg(fill.background()).text_color(color));
                }
                if let Some(handler) = self.on_click {
                    chip = chip.on_click(move |event, window, cx| handler(event, window, cx));
                }
                chip.into_any_element()
            }
            None => chip.into_any_element(),
        }
    }
}
