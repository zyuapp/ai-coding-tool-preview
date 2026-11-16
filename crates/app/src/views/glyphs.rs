//! Filled marks the outline icon set lacks: a pinned star and a lit bolt, as `fill="currentColor"`
//! draws them in the Electron app.

use gpui::{canvas, point, prelude::*, px, Bounds, PathBuilder, Pixels, Rgba, Window};

fn polygon(points: Vec<(f32, f32)>, size: f32, color: Rgba) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
            let scale = size / 24.0;
            let at = |(x, y): (f32, f32)| point(bounds.origin.x + px(x * scale), bounds.origin.y + px(y * scale));
            // Filled and stroked 2 units wide, as a Lucide icon with `fill="currentColor"` draws.
            let outline = gpui::StrokeOptions::default().with_line_width(2.0 * scale).with_miter_limit(1.0);
            for mut path in [PathBuilder::fill(), PathBuilder::fill().with_style(gpui::PathStyle::Stroke(outline))] {
                path.move_to(at(points[0]));
                for corner in &points[1..] {
                    path.line_to(at(*corner));
                }
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .size(px(size))
    .flex_none()
}

/// A five-pointed star, filled.
pub fn star(size: f32, color: Rgba) -> impl IntoElement {
    let points = (0..10)
        .map(|index| {
            let radius = if index % 2 == 0 { 10.4 } else { 5.9 };
            let angle = std::f32::consts::PI * (index as f32 / 5.0) - std::f32::consts::FRAC_PI_2;
            (12.0 + radius * angle.cos(), 12.76 + radius * angle.sin())
        })
        .collect();
    polygon(points, size, color)
}

/// The bolt of the fast-mode toggle, filled.
pub fn zap(size: f32, color: Rgba) -> impl IntoElement {
    polygon(vec![(4.0, 14.0), (14.2, 2.2), (12.0, 10.0), (20.0, 10.0), (9.8, 21.8), (12.0, 14.0)], size, color)
}

/// A play triangle, filled.
pub fn play(size: f32, color: Rgba) -> impl IntoElement {
    polygon(vec![(6.0, 3.0), (20.0, 12.0), (6.0, 21.0)], size, color)
}
