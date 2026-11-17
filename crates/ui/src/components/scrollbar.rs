//! The scrollbar (base.css): a capsule of glass floating clear of the edge, with no track. It
//! swells and clears under the pointer, as the system's own does. Lay it over a scrolling
//! container, as the container's last child, with the container's `ScrollHandle`.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    point, px, size, App, Background, Bounds, BorderStyle, Corners as QuadCorners, DispatchPhase, Edges, Element,
    ElementId, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton,
    ListState, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Position, ScrollHandle, Size, Style, Window,
};

use crate::color::{alpha, BLACK, WHITE};
use crate::theme::{ActiveTheme, ThemeVariant};

const THICKNESS: f32 = 12.0;
const MIN_THUMB: f32 = 36.0;
/// Clear of a rounded container's corners.
const END_MARGIN: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollAxis {
    Vertical,
    Horizontal,
}

/// The scroll of an element that scrolls itself rather than through a `ScrollHandle`: it publishes
/// what shows and how far it can go each frame, and reads back where its bar moved it.
#[derive(Clone, Default)]
pub struct ScrollModel(Rc<ScrollModelState>);

#[derive(Default)]
struct ScrollModelState {
    viewport: Cell<Bounds<Pixels>>,
    max: Cell<Size<Pixels>>,
    scrolled: Cell<gpui::Point<Pixels>>,
}

impl ScrollModel {
    pub fn publish(&self, viewport: Bounds<Pixels>, max: Size<Pixels>) {
        self.0.viewport.set(viewport);
        self.0.max.set(max);
    }

    /// How far the content has scrolled, as positive distances.
    pub fn scrolled(&self) -> gpui::Point<Pixels> {
        self.0.scrolled.get()
    }

    pub fn set_scrolled(&self, scrolled: gpui::Point<Pixels>) {
        self.0.scrolled.set(scrolled);
    }
}

/// What the bar scrolls: a container's handle, a virtual list, or an element scrolling itself.
#[derive(Clone)]
enum Source {
    Handle(ScrollHandle),
    List(ListState),
    Model(ScrollModel),
}

impl Source {
    fn viewport(&self) -> Bounds<Pixels> {
        match self {
            Source::Handle(handle) => handle.bounds(),
            Source::List(list) => list.viewport_bounds(),
            Source::Model(model) => model.0.viewport.get(),
        }
    }

    fn max_offset(&self) -> Size<Pixels> {
        match self {
            Source::Handle(handle) => handle.max_offset(),
            Source::List(list) => list.max_offset_for_scrollbar(),
            Source::Model(model) => model.0.max.get(),
        }
    }

    fn offset(&self) -> gpui::Point<Pixels> {
        match self {
            Source::Handle(handle) => handle.offset(),
            Source::List(list) => list.scroll_px_offset_for_scrollbar(),
            Source::Model(model) => {
                let scrolled = model.scrolled();
                point(-scrolled.x, -scrolled.y)
            }
        }
    }

    /// A list holds its measured height still while its thumb is dragged.
    fn drag(&self, started: bool) {
        if let Source::List(list) = self {
            if started {
                list.scrollbar_drag_started();
            } else {
                list.scrollbar_drag_ended();
            }
        }
    }

    fn set_offset(&self, offset: gpui::Point<Pixels>) {
        match self {
            Source::Handle(handle) => handle.set_offset(offset),
            Source::List(list) => list.set_offset_from_scrollbar(offset),
            Source::Model(model) => model.set_scrolled(point(-offset.x, -offset.y)),
        }
    }
}

pub struct Scrollbar {
    id: ElementId,
    handle: Source,
    axis: ScrollAxis,
}

/// The room a styled scrollbar takes at the right of a container that overflows: Chromium lays a
/// `::-webkit-scrollbar` beside the content rather than over it. Read from the last layout.
pub fn gutter(handle: &ScrollHandle) -> Pixels {
    if handle.max_offset().height > px(0.0) { px(THICKNESS) } else { px(0.0) }
}

/// `gutter` for an element scrolling itself.
pub fn model_gutter(model: &ScrollModel) -> Pixels {
    if model.0.max.get().height > px(0.0) { px(THICKNESS) } else { px(0.0) }
}

/// `gutter` for a virtual list.
pub fn list_gutter(list: &ListState) -> Pixels {
    if list.max_offset_for_scrollbar().height > px(0.0) { px(THICKNESS) } else { px(0.0) }
}

impl Scrollbar {
    pub fn vertical(id: impl Into<ElementId>, handle: &ScrollHandle) -> Self {
        Self { id: id.into(), handle: Source::Handle(handle.clone()), axis: ScrollAxis::Vertical }
    }

    pub fn vertical_model(id: impl Into<ElementId>, model: &ScrollModel) -> Self {
        Self { id: id.into(), handle: Source::Model(model.clone()), axis: ScrollAxis::Vertical }
    }

    pub fn horizontal(id: impl Into<ElementId>, handle: &ScrollHandle) -> Self {
        Self { id: id.into(), handle: Source::Handle(handle.clone()), axis: ScrollAxis::Horizontal }
    }

    /// A vertical bar for a virtual list.
    pub fn list(id: impl Into<ElementId>, list: &ListState) -> Self {
        Self { id: id.into(), handle: Source::List(list.clone()), axis: ScrollAxis::Vertical }
    }
}

#[derive(Default)]
struct DragState {
    /// Where a drag took hold of the thumb, along the axis, from the thumb's start.
    grab: Cell<Option<Pixels>>,
    /// Whether the container overflowed at the last frame, which is what `gutter` read when it was rendered.
    overflowed: Cell<bool>,
}

pub struct ScrollbarPrepaint {
    hitbox: Hitbox,
    thumb: Option<Bounds<Pixels>>,
    track: Bounds<Pixels>,
    state: Rc<DragState>,
}

/// The thumb's offset and length along a track, for a viewport over content scrolled by `scrolled`.
pub fn thumb_geometry(track: f32, viewport: f32, content: f32, scrolled: f32) -> Option<(f32, f32)> {
    if content <= viewport + 0.5 || track <= 0.0 {
        return None;
    }
    let length = (track * viewport / content).max(MIN_THUMB).min(track);
    let max_scroll = content - viewport;
    let position = (track - length) * (scrolled / max_scroll).clamp(0.0, 1.0);
    Some((position, length))
}

impl IntoElement for Scrollbar {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Scrollbar {
    type RequestLayoutState = ();
    type PrepaintState = ScrollbarPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
        match self.axis {
            ScrollAxis::Vertical => {
                style.inset.top = px(0.0).into();
                style.inset.right = px(0.0).into();
                style.size.width = px(THICKNESS).into();
                style.size.height = gpui::relative(1.0).into();
            }
            ScrollAxis::Horizontal => {
                style.inset.left = px(0.0).into();
                style.inset.bottom = px(0.0).into();
                style.size.height = px(THICKNESS).into();
                style.size.width = gpui::relative(1.0).into();
            }
        }
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let state = window.with_element_state(id.expect("scrollbar has an id"), |state: Option<Rc<DragState>>, _| {
            let state = state.unwrap_or_default();
            (state.clone(), state)
        });
        let viewport = self.handle.viewport();
        let max = self.handle.max_offset();
        let offset = self.handle.offset();
        let margin = px(END_MARGIN);
        let (track, thumb) = match self.axis {
            ScrollAxis::Vertical => {
                let track = Bounds::new(point(bounds.left(), bounds.top() + margin), size(bounds.size.width, bounds.size.height - margin * 2.0));
                let thumb = thumb_geometry(track.size.height.into(), viewport.size.height.into(), (viewport.size.height + max.height).into(), (-offset.y).into())
                    .map(|(position, length)| Bounds::new(point(track.left(), track.top() + px(position)), size(track.size.width, px(length))));
                (track, thumb)
            }
            ScrollAxis::Horizontal => {
                let track = Bounds::new(point(bounds.left() + margin, bounds.top()), size(bounds.size.width - margin * 2.0, bounds.size.height));
                let thumb = thumb_geometry(track.size.width.into(), viewport.size.width.into(), (viewport.size.width + max.width).into(), (-offset.x).into())
                    .map(|(position, length)| Bounds::new(point(track.left() + px(position), track.top()), size(px(length), track.size.height)));
                (track, thumb)
            }
        };
        // A container that starts or stops overflowing is rendered again, so its `gutter` takes the room.
        if self.axis == ScrollAxis::Vertical && state.overflowed.replace(max.height > px(0.0)) != (max.height > px(0.0)) {
            window.request_animation_frame();
        }
        let behavior = if thumb.is_some() { HitboxBehavior::BlockMouse } else { HitboxBehavior::Normal };
        let hitbox = window.insert_hitbox(if thumb.is_some() { bounds } else { Bounds::default() }, behavior);
        ScrollbarPrepaint { hitbox, thumb, track, state }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(thumb) = prepaint.thumb else { return };
        let theme = cx.theme();
        let t = &theme.tokens;
        let dragging = prepaint.state.grab.get().is_some();
        let hovered = prepaint.hitbox.is_hovered(window) || dragging;
        let inset = px(if hovered { 3.0 } else { 4.0 });
        let visible = Bounds::from_corners(thumb.origin + point(inset, inset), thumb.bottom_right() - point(inset, inset));
        let base = alpha(t.ink, if hovered { 0.26 } else { 0.14 });
        let sheen_from = crate::color::over(alpha(WHITE, 0.2), base);
        let sheen_to = crate::color::over(alpha(WHITE, 0.04), base);
        let angle = if self.axis == ScrollAxis::Vertical { 90.0 } else { 180.0 };
        let background: Background =
            gpui::linear_gradient(angle, gpui::linear_color_stop(sheen_from, 0.0), gpui::linear_color_stop(sheen_to, 0.7));
        let edge = if theme.variant() == ThemeVariant::Dark { alpha(WHITE, 0.12) } else { alpha(BLACK, 0.14) };
        let radius = visible.size.width.min(visible.size.height) / 2.0;
        window.paint_quad(PaintQuad {
            bounds: visible,
            corner_radii: QuadCorners::all(radius),
            background,
            border_widths: Edges::all(px(0.5)),
            border_color: edge.into(),
            border_style: BorderStyle::Solid,
        });

        let handle = self.handle.clone();
        let axis = self.axis;
        let track = prepaint.track;
        let state = prepaint.state.clone();
        let hitbox = prepaint.hitbox.clone();
        let along = move |position: gpui::Point<Pixels>| match axis {
            ScrollAxis::Vertical => position.y,
            ScrollAxis::Horizontal => position.x,
        };
        let thumb_start = along(thumb.origin);
        let thumb_length = match axis {
            ScrollAxis::Vertical => thumb.size.height,
            ScrollAxis::Horizontal => thumb.size.width,
        };
        let track_start = along(track.origin);
        let track_length = match axis {
            ScrollAxis::Vertical => track.size.height,
            ScrollAxis::Horizontal => track.size.width,
        };
        let scroll_to = {
            let handle = handle.clone();
            move |thumb_at: Pixels| {
                let max = handle.max_offset();
                let max = match axis {
                    ScrollAxis::Vertical => max.height,
                    ScrollAxis::Horizontal => max.width,
                };
                let room = (track_length - thumb_length).max(px(1.0));
                let share = ((thumb_at - track_start) / room).clamp(0.0, 1.0);
                let mut offset = handle.offset();
                match axis {
                    ScrollAxis::Vertical => offset.y = -(max * share),
                    ScrollAxis::Horizontal => offset.x = -(max * share),
                }
                handle.set_offset(offset);
            }
        };

        window.on_mouse_event({
            let state = state.clone();
            let hitbox = hitbox.clone();
            let scroll_to = scroll_to.clone();
            let drag_source = handle.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                    return;
                }
                let at = along(event.position);
                drag_source.drag(true);
                if at >= thumb_start && at <= thumb_start + thumb_length {
                    state.grab.set(Some(at - thumb_start));
                } else {
                    // A press on the track moves the thumb there, centred under the pointer.
                    state.grab.set(Some(thumb_length / 2.0));
                    scroll_to(at - thumb_length / 2.0);
                }
                cx.stop_propagation();
                window.refresh();
            }
        });
        window.on_mouse_event({
            let state = state.clone();
            move |event: &MouseMoveEvent, _, window, _| {
                if let Some(grab) = state.grab.get() {
                    scroll_to(along(event.position) - grab);
                    window.refresh();
                } else if hitbox.is_hovered(window) != hovered {
                    window.refresh();
                }
            }
        });
        window.on_mouse_event(move |_: &MouseUpEvent, _, window, _| {
            if state.grab.take().is_some() {
                handle.drag(false);
                window.refresh();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::thumb_geometry;

    #[test]
    fn the_thumb_is_proportional_but_never_shorter_than_its_minimum() {
        assert_eq!(thumb_geometry(400.0, 400.0, 400.0, 0.0), None);
        assert_eq!(thumb_geometry(400.0, 400.0, 800.0, 0.0), Some((0.0, 200.0)));
        assert_eq!(thumb_geometry(400.0, 400.0, 800.0, 400.0), Some((200.0, 200.0)));
        let (_, length) = thumb_geometry(400.0, 400.0, 100_000.0, 0.0).unwrap();
        assert_eq!(length, 36.0);
    }
}
