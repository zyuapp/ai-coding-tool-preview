//! A window over the system's Liquid Glass, with one pane of clear glass over the sheet, where macOS
//! has glass; elsewhere the window falls back to the material `WindowMaterial` picks.
//!
//! `cargo run -p act_devices --example glass`

use act_devices::glass::{self, GlassRegion, WindowMaterial};
use act_ui::theme::ActiveTheme;
use act_ui::Assets;
use gpui::{div, point, prelude::*, px, size, App, Bounds, Context, Window, WindowBounds, WindowOptions};

struct Glassy {
    material: WindowMaterial,
    attached: bool,
}

impl Render for Glassy {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = Bounds { origin: point(px(40.0), px(90.0)), size: size(px(360.0), px(160.0)) };
        if self.attached {
            glass::regions(window, &[GlassRegion::new(pane, px(18.0))]);
        }
        let t = cx.theme().tokens.clone();
        div()
            .size_full()
            .relative()
            .font_family(cx.theme().typography.ui_family.clone())
            .text_color(t.ink)
            .p(px(40.0))
            .child(div().text_size(px(18.0)).child(format!("{:?} · glass attached: {} · tint {:.2}", self.material, self.attached, glass::system_glass_tint())))
            .child(
                div()
                    .absolute()
                    .left(pane.origin.x)
                    .top(pane.origin.y)
                    .w(pane.size.width)
                    .h(pane.size.height)
                    .p(px(20.0))
                    .text_color(t.ink_dim)
                    .child("A pane of clear glass over the window's sheet"),
            )
    }
}

fn main() {
    gpui_platform::application().with_assets(Assets).run(|cx: &mut App| {
        act_ui::init(cx);
        let material = WindowMaterial::current();
        let bounds = Bounds::centered(None, size(px(640.0), px(320.0)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), window_background: material.background(), ..Default::default() },
            |window, cx| {
                let attached = material == WindowMaterial::Glass && glass::attach(window);
                cx.new(|_| Glassy { material, attached })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
