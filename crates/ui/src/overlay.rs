//! Placing floats against the window: menus beside their trigger (useMenuAnchor), a submenu beside
//! its row, a context menu at the pointer, and a tooltip under what it explains.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    point, px, AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Point, Position, Size, Style, Window,
};

/// How close to the window's edge a float may be drawn.
pub const EDGE: f32 = 8.0;
/// The gap between a trigger and the menu it opens.
pub const ANCHOR_GAP: f32 = 4.0;
/// The gap between a tooltip and what it explains.
pub const TIP_GAP: f32 = 8.0;
/// The gap between a menu row and the list it opens.
pub const SUBMENU_GAP: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    /// The float's left edge on the anchor's left edge.
    #[default]
    Start,
    /// The float's right edge on the anchor's right edge.
    End,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// Below the anchor if it fits there or below has the more room, else above; slid back inside.
    Anchor { anchor: Bounds<Pixels>, align: Align },
    /// Beside a menu row: to its right until the window runs out, then to its left; lifted when it
    /// runs past the bottom. `top` is where the float's top edge starts.
    Beside { anchor: Bounds<Pixels>, top: Pixels },
    /// At a point, as a context menu opens at the pointer; flipped where it would leave the window.
    At { position: Point<Pixels> },
    /// Centred under the anchor, or above it when there is no room below.
    Tooltip { anchor: Bounds<Pixels> },
    /// Always `gap` under the anchor, as a popover positioned in CSS under its trigger; never flipped.
    Below { anchor: Bounds<Pixels>, align: Align, gap: Pixels },
}

impl Placement {
    /// Where a float of `size` lands in a window of `viewport`.
    pub fn origin(&self, size: Size<Pixels>, viewport: Size<Pixels>) -> Point<Pixels> {
        let edge = px(EDGE);
        // A float as large as the window, such as a dialog's backdrop, covers it from its corner.
        let clamp_x = |x: Pixels| if size.width + edge * 2.0 > viewport.width { x.min(viewport.width - size.width).max(px(0.0)) } else { x.min(viewport.width - size.width - edge).max(edge) };
        let clamp_y = |y: Pixels| if size.height + edge * 2.0 > viewport.height { y.min(viewport.height - size.height).max(px(0.0)) } else { y.min(viewport.height - size.height - edge).max(edge) };
        match *self {
            Placement::Anchor { anchor, align } => {
                let gap = px(ANCHOR_GAP);
                let below = viewport.height - anchor.bottom() - gap - edge;
                let above = anchor.top() - gap - edge;
                let down = below >= size.height || below >= above;
                let start = match align {
                    Align::Start => anchor.left(),
                    Align::End => anchor.right() - size.width,
                };
                let y = if down { anchor.bottom() + gap } else { anchor.top() - gap - size.height };
                point(clamp_x(start), y)
            }
            Placement::Beside { anchor, top } => {
                let gap = px(SUBMENU_GAP);
                let mut x = anchor.right() + gap;
                if x + size.width + edge > viewport.width {
                    x = anchor.left() - size.width - gap;
                }
                point(clamp_x(x), clamp_y(top))
            }
            Placement::At { position } => {
                let mut x = position.x;
                let mut y = position.y;
                if x + size.width + edge > viewport.width {
                    x = (position.x - size.width).max(edge);
                }
                if y + size.height + edge > viewport.height {
                    y = (position.y - size.height).max(edge);
                }
                point(clamp_x(x), if size.height + edge * 2.0 > viewport.height { clamp_y(y) } else { y.max(edge) })
            }
            Placement::Below { anchor, align, gap } => {
                let start = match align {
                    Align::Start => anchor.left(),
                    Align::End => anchor.right() - size.width,
                };
                point(clamp_x(start), anchor.bottom() + gap)
            }
            Placement::Tooltip { anchor } => {
                let gap = px(TIP_GAP);
                let below = anchor.bottom() + gap;
                let y = if below + size.height > viewport.height - gap { anchor.top() - size.height - gap } else { below };
                let centred = anchor.left() + anchor.size.width / 2.0 - size.width / 2.0;
                point(centred.min(viewport.width - size.width - gap).max(gap).round(), y.round())
            }
        }
    }

    /// The height a float may grow to before it scrolls (`--menu-room`).
    pub fn room(&self, viewport: Size<Pixels>) -> Pixels {
        let edge = px(EDGE);
        match *self {
            Placement::Anchor { anchor, .. } => {
                let gap = px(ANCHOR_GAP);
                let below = viewport.height - anchor.bottom() - gap - edge;
                let above = anchor.top() - gap - edge;
                below.max(above).max(px(0.0))
            }
            Placement::Below { anchor, gap, .. } => (viewport.height - anchor.bottom() - gap - edge).max(px(0.0)),
            _ => viewport.height - edge * 2.0,
        }
    }
}

/// Lays its child out as its own root against the window, as `position: fixed` sizes from its
/// content, and places it there. Wrap it in `deferred` so it draws over everything that follows it.
pub struct Placed {
    placement: Placement,
    child: AnyElement,
}

pub fn placed(placement: Placement, child: impl IntoElement) -> Placed {
    Placed { placement, child: child.into_any_element() }
}

impl IntoElement for Placed {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Placed {
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
        let style = Style { position: Position::Absolute, ..Style::default() };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let viewport = window.viewport_size();
        // Shrink-to-fit: its max-content width, unless that is wider than the window.
        let height = AvailableSpace::Definite(viewport.height);
        let mut size = self.child.layout_as_root(gpui::size(AvailableSpace::MaxContent, height), window, cx);
        if size.width > viewport.width {
            size = self.child.layout_as_root(gpui::size(AvailableSpace::Definite(viewport.width), height), window, cx);
        }
        let origin = self.placement.origin(size, viewport);
        self.child.prepaint_at(point(origin.x.round(), origin.y.round()), window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// Where an element was last drawn, written by a `BoundsProbe` laid over it.
pub type BoundsCell = Rc<Cell<Option<Bounds<Pixels>>>>;

pub fn bounds_cell() -> BoundsCell {
    Rc::new(Cell::new(None))
}

/// An empty element covering its parent (which must be positioned) that records the parent's
/// window bounds each frame.
pub struct BoundsProbe {
    cell: BoundsCell,
}

pub fn bounds_probe(cell: &BoundsCell) -> BoundsProbe {
    BoundsProbe { cell: cell.clone() }
}

impl IntoElement for BoundsProbe {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for BoundsProbe {
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
        let mut style = Style { position: Position::Absolute, ..Style::default() };
        style.inset.top = px(0.0).into();
        style.inset.left = px(0.0).into();
        style.size.width = gpui::relative(1.0).into();
        style.size.height = gpui::relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
        self.cell.set(Some(bounds));
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn a_menu_opens_upward_when_below_is_short_and_above_is_roomier() {
        let viewport = size(px(800.0), px(600.0));
        let anchor = bounds(100.0, 560.0, 80.0, 28.0);
        let origin = Placement::Anchor { anchor, align: Align::Start }.origin(size(px(200.0), px(300.0)), viewport);
        assert_eq!(origin.y, px(560.0 - 4.0 - 300.0));
        let origin = Placement::Anchor { anchor: bounds(100.0, 40.0, 80.0, 28.0), align: Align::Start }
            .origin(size(px(200.0), px(300.0)), viewport);
        assert_eq!(origin.y, px(72.0));
    }

    #[test]
    fn an_end_aligned_menu_slides_back_inside_the_window() {
        let viewport = size(px(400.0), px(600.0));
        let origin = Placement::Anchor { anchor: bounds(10.0, 10.0, 30.0, 20.0), align: Align::End }
            .origin(size(px(200.0), px(100.0)), viewport);
        assert_eq!(origin.x, px(EDGE));
    }

    #[test]
    fn a_tooltip_goes_above_when_it_would_leave_the_bottom() {
        let viewport = size(px(400.0), px(300.0));
        let origin = Placement::Tooltip { anchor: bounds(100.0, 270.0, 40.0, 20.0) }.origin(size(px(60.0), px(24.0)), viewport);
        assert_eq!(origin.y, px(270.0 - 24.0 - 8.0));
        assert_eq!(origin.x, px(90.0));
    }

    #[test]
    fn a_submenu_flips_left_at_the_right_edge() {
        let viewport = size(px(500.0), px(600.0));
        let origin = Placement::Beside { anchor: bounds(300.0, 100.0, 180.0, 28.0), top: px(95.0) }
            .origin(size(px(160.0), px(100.0)), viewport);
        assert_eq!(origin.x, px(300.0 - 160.0 - 3.0));
    }
}
