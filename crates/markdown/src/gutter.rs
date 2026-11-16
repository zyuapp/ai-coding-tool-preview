//! The room a styled horizontal scrollbar takes under content wider than its block: Chromium lays a
//! `::-webkit-scrollbar` below the content rather than over it.

use gpui::{
    px, size, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window,
};

/// An empty element `height` tall while the block it is laid in is narrower than `content`, and
/// flat otherwise.
pub struct OverflowGutter {
    content: Pixels,
    height: Pixels,
}

impl OverflowGutter {
    pub fn new(content: Pixels, height: Pixels) -> Self {
        Self { content, height }
    }
}

impl IntoElement for OverflowGutter {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for OverflowGutter {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, _: &mut App) -> (LayoutId, ()) {
        let (content, height) = (self.content, self.height);
        let layout_id = window.request_measured_layout(Style::default(), move |known, available, _, _| {
            let room = known.width.or(match available.width {
                gpui::AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            // Sub-pixel slack, as layout rounds the block's width.
            let overflows = room.is_some_and(|room| content > room + px(0.5));
            size(px(0.), if overflows { height } else { px(0.) })
        });
        (layout_id, ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), _: &mut Window, _: &mut App) {}
}
