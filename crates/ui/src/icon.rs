//! Icons: a monochrome SVG from the bundled set, drawn in the text colour around it unless told
//! otherwise, as `currentColor` does.

use gpui::{
    px, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels,
    Radians, SharedString, Style, TransformationMatrix, Window,
};

pub use crate::icon_names::IconName;

/// A square glyph. Its colour is the inherited text colour unless `color` is set.
pub struct Icon {
    path: SharedString,
    size: Pixels,
    shrinks: bool,
    color: Option<Hsla>,
    rotation: Radians,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self { path: name.path().into(), size: px(16.0), shrinks: false, color: None, rotation: Radians(0.0) }
    }

    /// An SVG the app serves from its own asset source.
    pub fn from_path(path: impl Into<SharedString>) -> Self {
        Self { path: path.into(), size: px(16.0), shrinks: false, color: None, rotation: Radians(0.0) }
    }

    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = size.into();
        self
    }

    /// Lets a flex row squeeze the icon's width as it does an inline SVG: the glyph scales down
    /// to the room left, centred on its line.
    pub fn shrinks(mut self) -> Self {
        self.shrinks = true;
        self
    }

    pub fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// Draws the glyph's strokes `width` wide on its 24-unit grid instead of 2.
    pub fn stroke(mut self, width: f32) -> Self {
        let file = self.path.split_once('?').map_or(self.path.as_ref(), |(file, _)| file).to_string();
        self.path = format!("{file}?stroke={width}").into();
        self
    }

    /// Turns the glyph about its centre, clockwise.
    pub fn rotate(mut self, degrees: f32) -> Self {
        self.rotation = Radians(degrees.to_radians());
        self
    }
}

pub fn icon(name: IconName) -> Icon {
    Icon::new(name)
}

impl IntoElement for Icon {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Icon {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = self.size.into();
        style.size.height = self.size.into();
        style.flex_shrink = if self.shrinks { 1.0 } else { 0.0 };
        if self.shrinks {
            style.min_size.width = px(0.0).into();
        }
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        let bounds = if self.shrinks && bounds.size.width < bounds.size.height {
            let side = bounds.size.width;
            Bounds::new(gpui::point(bounds.origin.x, bounds.origin.y + (bounds.size.height - side) / 2.0), gpui::size(side, side))
        } else {
            bounds
        };
        let transformation = if self.rotation.0 == 0.0 {
            TransformationMatrix::unit()
        } else {
            let scale = window.scale_factor();
            let center = bounds.center();
            TransformationMatrix::unit()
                .translate(center.scale(scale))
                .rotate(self.rotation)
                .translate(gpui::point(-center.x, -center.y).scale(scale))
        };
        let _ = window.paint_svg(bounds, self.path.clone(), transformation, color, cx);
    }
}
