//! Streams a long answer into a message at an engine's pace and reports frame times and how long a
//! simulated keystroke waits to be drawn. `BENCH_START` (chars already written, default 280000),
//! `BENCH_TOTAL` (default 320000), `BENCH_RATE` (deltas/s, default 50), `BENCH_DELTA` (chars per
//! delta, default 24), `BENCH_FINISH=1` ends the stream at the total and keeps drawing, `BENCH_SCROLL=<px>` holds the view
//! that far into the message instead of following its end.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use act_markdown::{bind_keys, LinkCapabilities, MarkdownStyle, MarkdownView};
use gpui::{
    canvas, div, list, prelude::*, px, rgb, size, App, Bounds, Context, Entity,
    ListAlignment, ListOffset, ListState, Window, WindowBounds, WindowOptions,
};

fn sample(len: usize) -> String {
    let mut text = String::with_capacity(len + 2_000);
    let mut index = 0;
    while text.len() < len {
        index += 1;
        match index % 7 {
            0 => text.push_str(&format!("## Section {index}\n\n")),
            1 => text.push_str(&format!(
                "- First point about `item_{index}` and **bold** text\n- Second point with a [link](https://example.com/{index})\n  - nested detail\n\n"
            )),
            2 => text.push_str(&format!(
                "```rust\nfn handler_{index}(input: &str) -> usize {{\n    input.len() * {index}\n}}\n```\n\n"
            )),
            3 => text.push_str("| Column | Value |\n|---|--:|\n| alpha | 1 |\n| beta | 22 |\n\n"),
            _ => text.push_str(&format!(
                "Paragraph {index} explains how the renderer keeps settled blocks while the live tail grows, with *emphasis*, a file [App.tsx](/checkout/src/App.tsx:{index}) and enough words to wrap over a few lines of the timeline column.\n\n"
            )),
        }
    }
    text
}

#[derive(Default)]
struct Stats {
    frames: Vec<f32>,
    keys: Vec<f32>,
    frame_start: Option<Instant>,
    key_at: Option<Instant>,
}

fn summary(name: &str, values: &mut [f32]) -> String {
    if values.is_empty() {
        return format!("{name}: none");
    }
    values.sort_by(f32::total_cmp);
    let at = |q: f32| values[((values.len() - 1) as f32 * q) as usize];
    format!(
        "{name}: n={} p50={:.1} p95={:.1} max={:.1} ms",
        values.len(),
        at(0.5),
        at(0.95),
        values[values.len() - 1]
    )
}

struct Bench {
    list: ListState,
    message: Entity<MarkdownView>,
    stats: Rc<RefCell<Stats>>,
    keystrokes: usize,
}

impl Render for Bench {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.stats.borrow_mut().frame_start = Some(Instant::now());
        let message = self.message.clone();
        let stats = self.stats.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x0e1117))
            .child(div().h(px(28.)).text_color(rgb(0xffffff)).child(format!("keys {}", self.keystrokes)))
            .child(
                list(self.list.clone(), move |index, _, _| {
                    let row = div().max_w(px(860.)).mx_auto().px(px(32.)).py(px(14.));
                    match index {
                        0 => row.child("Question").into_any_element(),
                        _ => row.child(message.clone()).into_any_element(),
                    }
                })
                .flex_1(),
            )
            .child(
                canvas(|_, _, _| {}, move |_, _, _, _| {
                    let mut stats = stats.borrow_mut();
                    let now = Instant::now();
                    if let Some(start) = stats.frame_start.take() {
                        stats.frames.push((now - start).as_secs_f32() * 1000.);
                    }
                    if let Some(key) = stats.key_at.take() {
                        stats.keys.push((now - key).as_secs_f32() * 1000.);
                    }
                })
                .h(px(1.)),
            )
    }
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

fn main() {
    let start = env("BENCH_START", 280_000);
    let total = env("BENCH_TOTAL", 320_000);
    let rate = env("BENCH_RATE", 50).max(1);
    let delta = env("BENCH_DELTA", 24).max(1);
    let finish = std::env::var("BENCH_FINISH").is_ok();
    let text = sample(total);
    gpui_platform::application().run(move |cx: &mut App| {
        bind_keys(cx);
        let mono = act_markdown::style::load_system_monospace(cx);
        let bounds = Bounds::centered(None, size(px(1000.), px(1000.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            |window, cx| {
                let message = cx.new(|cx| {
                    let mut view = MarkdownView::new(window, cx);
                    view.set_style(
                        MarkdownStyle { mono_font: mono.clone().unwrap_or_else(|| "Menlo".into()), ..MarkdownStyle::dark() },
                        cx,
                    );
                    view.set_link_capabilities(
                        LinkCapabilities { select_thread: true, open_file: true, open_url_in_app: true, open_image: true, open_commit: true },
                        cx,
                    );
                    let mut at = start.min(text.len());
                    while !text.is_char_boundary(at) {
                        at += 1;
                    }
                    view.set_text(&text[..at], true, cx);
                    view
                });
                let stats = Rc::new(RefCell::new(Stats::default()));
                let scroll = std::env::var("BENCH_SCROLL").ok().and_then(|value| value.parse::<f32>().ok());
                let list = ListState::new(2, if scroll.is_some() { ListAlignment::Top } else { ListAlignment::Bottom }, px(1000.));
                if let Some(offset) = scroll {
                    list.scroll_to(ListOffset { item_ix: 1, offset_in_item: px(offset) });
                }
                let bench = cx.new(|_| Bench { list, message: message.clone(), stats: stats.clone(), keystrokes: 0 });
                let streamed = message.clone();
                let text = text.clone();
                cx.spawn(async move |cx| {
                    let mut written = start.min(text.len());
                    let began = Instant::now();
                    let mut sent = 0u32;
                    while written < text.len() {
                        sent += 1;
                        let due = began + Duration::from_secs_f64(sent as f64 / rate as f64);
                        cx.background_executor().timer(due.saturating_duration_since(Instant::now())).await;
                        let mut next = (written + delta).min(text.len());
                        while !text.is_char_boundary(next) {
                            next += 1;
                        }
                        written = next;
                        let done = finish && written == text.len();
                        let _ = streamed.update(cx, |view, cx| view.set_text(&text[..written], !done, cx));
                    }
                    eprintln!("stream done after {:.1}s", began.elapsed().as_secs_f32());
                })
                .detach();
                let typing = bench.clone();
                cx.spawn(async move |cx| loop {
                    cx.background_executor().timer(Duration::from_millis(150)).await;
                    let _ = typing.update(cx, |bench, cx| {
                        bench.keystrokes += 1;
                        bench.stats.borrow_mut().key_at.get_or_insert_with(Instant::now);
                        cx.notify();
                    });
                })
                .detach();
                cx.spawn(async move |cx| loop {
                    cx.background_executor().timer(Duration::from_secs(5)).await;
                    let line = {
                        let mut stats = stats.borrow_mut();
                        let frames = summary("frame", &mut stats.frames);
                        let keys = summary("key", &mut stats.keys);
                        stats.frames.clear();
                        stats.keys.clear();
                        format!("{frames} | {keys}")
                    };
                    let length = cx.update(|cx| message.read(cx).text().len());
                    eprintln!("[{length} chars] {line}");
                })
                .detach();
                bench
            },
        )
        .expect("open window");
        cx.activate(true);
    });
}
