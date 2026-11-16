//! The glass marble a working run spins in (`.run-bead`, `.task-spinner`).

use act_ui::components::spinner::Spinning;
use act_ui::theme::Tokens;
use act_ui::IconName;
use act_ui::style::Raised;
use gpui::{div, prelude::*, px, SharedString};

use super::common::with_alpha;

pub fn bead(id: impl Into<SharedString>, size: f32, t: &Tokens, held: bool) -> impl IntoElement {
    // `radial-gradient(circle at x y, …)`: the radius reaches the farthest corner.
    let circle = |x: f32, y: f32, radius: f32, from: (gpui::Rgba, f32), to: (gpui::Rgba, f32)| {
        div().absolute().inset_0().rounded_full().bg(gpui::radial_gradient(gpui::point(x, y), gpui::size(radius, radius), gpui::linear_color_stop(from.0, from.1), gpui::linear_color_stop(to.0, to.1)))
    };
    let clear = gpui::Rgba { r: 1.0, g: 1.0, b: 1.0, a: 0.0 };
    let base = div().relative().flex_none().size(px(size)).rounded_full();
    let base = if held {
        // A held run's light stops low in the marble, in the warning hue.
        base.bg(with_alpha(t.warning, 0.2)).child(circle(0.62, 0.74, 0.9654, (t.warning, 0.0), (with_alpha(t.warning, 0.35), 0.72)))
    } else {
        base.bg(with_alpha(t.accent_text, 0.16))
            .shadow(vec![gpui::BoxShadow { color: with_alpha(t.accent_text, 0.28).into(), offset: gpui::point(px(0.0), px(0.0)), blur_radius: px(7.0), spread_radius: px(0.0) }])
            .child(div().absolute().inset_0().flex().items_center().justify_center().child(Spinning::new(gpui::ElementId::Name(id.into()), IconName::ConicSweep).pace(std::time::Duration::from_millis(1500), 30).size(px(size)).color(t.accent_text)))
    };
    // A highlight that stays put, over a shade toward the rim.
    base.child(circle(0.34, 0.26, 0.9915, (with_alpha(gpui::white().into(), 0.75), 0.0), (clear, 0.34)))
        .child(circle(0.5, 0.5, 0.7071, (gpui::Rgba { a: 0.0, ..gpui::black().into() }, 0.36), (with_alpha(gpui::black().into(), 0.22), 1.0)))
        .rim(t.glass_rim, act_ui::style::Corners::Full)
        .into_any_element()
}
