use gpui::{
    App, Application, BackdropFilter, Bounds, Context, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, rgba, size,
};

struct BackdropBlurExample;

impl Render for BackdropBlurExample {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let colors = [0xe5484d, 0xf5d90a, 0x46a758, 0x0090ff, 0x8e4ec6];
        div()
            .relative()
            .size_full()
            .bg(rgb(0x111111))
            .text_color(rgb(0xffffff))
            .text_xl()
            .child(div().flex().size_full().children(
                colors.iter().map(|&color| div().flex_1().h_full().bg(rgb(color)).child("Behind")),
            ))
            .child(
                div()
                    .absolute()
                    .top(px(60.))
                    .left(px(60.))
                    .w(px(380.))
                    .h(px(200.))
                    .rounded(px(16.))
                    .bg(rgba(0x20202080))
                    .backdrop_filter(BackdropFilter::blur(px(12.)).saturate(1.7))
                    .p_4()
                    .child("Frosted glass"),
            )
            .child(
                div()
                    .absolute()
                    .top(px(300.))
                    .left(px(260.))
                    .size(px(120.))
                    .rounded_full()
                    .backdrop_filter(BackdropFilter::blur(px(2.)).brightness(1.3)),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.), px(480.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| BackdropBlurExample),
        )
        .unwrap();
        cx.activate(true);
    });
}
