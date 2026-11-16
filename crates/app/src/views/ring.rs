//! A progress ring drawn as two strokes (`.context-usage-ring`, the sidebar's update ring).

use gpui::{canvas, point, prelude::*, px, Bounds, PathBuilder, Pixels, Rgba, Window};

pub struct Ring {
    pub progress: f32,
    pub size: f32,
    pub thickness: f32,
    pub track: Rgba,
    pub fill: Rgba,
}

fn circle_point(center: gpui::Point<Pixels>, radius: f32, turn: f32) -> gpui::Point<Pixels> {
    let angle = turn * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
    point(center.x + px(radius * angle.cos()), center.y + px(radius * angle.sin()))
}

fn arc(center: gpui::Point<Pixels>, radius: f32, thickness: f32, from: f32, to: f32) -> Option<gpui::Path<Pixels>> {
    let mut builder = PathBuilder::stroke(px(thickness));
    let span = (to - from).clamp(0.0, 0.9999);
    if span <= 0.0 {
        return None;
    }
    builder.move_to(circle_point(center, radius, from));
    let steps = (span * 4.0).ceil().max(1.0) as usize;
    for step in 1..=steps {
        let at = from + span * step as f32 / steps as f32;
        builder.arc_to(point(px(radius), px(radius)), px(0.0), false, true, circle_point(center, radius, at));
    }
    builder.build().ok()
}

impl IntoElement for Ring {
    type Element = gpui::Canvas<()>;

    fn into_element(self) -> Self::Element {
        let Ring { progress, size, thickness, track, fill } = self;
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                let center = bounds.center();
                let radius = size / 2.0 - thickness / 2.0;
                if let Some(path) = arc(center, radius, thickness, 0.0, 0.9999) {
                    window.paint_path(path, track);
                }
                if let Some(path) = arc(center, radius, thickness, 0.0, progress.clamp(0.0, 1.0)) {
                    window.paint_path(path, fill);
                }
            },
        )
        .size(px(size))
        .flex_none()
        .into_element()
    }
}
