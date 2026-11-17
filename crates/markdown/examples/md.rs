//! Renders a sample answer the way the timeline shows one. `MD_STREAM=1` streams it in instead.

use std::borrow::Cow;
use std::time::Duration;

use act_markdown::{bind_keys, LinkCapabilities, MarkdownEvent, MarkdownStyle, MarkdownView};
use gpui::{
    div, prelude::*, px, rgb, size, App, AssetSource, Bounds, Context, Entity,
    SharedString, Window, WindowBounds, WindowOptions,
};

const SAMPLE: &str = r#"## Streaming markdown

The renderer keeps **settled blocks** parsed once and re-reads only the *live tail*. It handles `inline code`, ~~struck text~~, [web links](https://example.com/docs), bare https://zed.dev links, a file like [App.tsx](/checkout/src/renderer/App.tsx:42), and a thread like [the sidebar work](aicodingtool://thread/task-9). Commit `60cceb8` opens when the checkout knows it.

### Lists

1. First ordered item
2. Second item with a nested list:
   - nested bullet
   - another one
     - third level
3. Third item

- [x] Port the parser
- [ ] Port selection

> A quote keeps its rule and muted ink.
> It can run over **several** lines.

```rust
fn main() {
    let greeting = "hello"; // a comment
    println!("{greeting}, world: {}", 42);
}
```

```
plain code block without a language, wide enough to scroll sideways when the window is narrow enough
```

| Column | Aligned right | Notes |
|---|--:|---|
| alpha | 1 | short |
| beta | 22 | a longer cell that wraps once the table runs out of room in the timeline |

---

#### Footnotes and the rest

A footnote reference[^note] and a final paragraph with a hard\
break inside it.

[^note]: The note itself.
"#;

const FILE_TEXT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="M10 9H8"/><path d="M16 13H8"/><path d="M16 17H8"/></svg>"#;
const THREAD: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/><path d="M13 8H7"/><path d="M17 12H7"/></svg>"#;

struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "icons/file-text.svg" => Some(Cow::Borrowed(FILE_TEXT.as_bytes())),
            "icons/message-square-text.svg" => Some(Cow::Borrowed(THREAD.as_bytes())),
            _ => None,
        })
    }

    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

struct Sample {
    message: Entity<MarkdownView>,
}

impl Render for Sample {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("timeline")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(0x0e1117))
            .child(
                div()
                    .max_w(px(860.))
                    .mx_auto()
                    .px(px(32.))
                    .py(px(28.))
                    .child(self.message.clone()),
            )
    }
}

fn main() {
    let streaming = std::env::var("MD_STREAM").is_ok();
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            bind_keys(cx);
            let mono = act_markdown::style::load_system_monospace(cx);
            let bounds = Bounds::centered(
                None,
                size(px(env_px("MD_WIDTH", 940.)), px(env_px("MD_HEIGHT", 1180.))),
                cx,
            );
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    let message = cx.new(|cx| {
                        let mut view = MarkdownView::new(window, cx);
                        let style = MarkdownStyle {
                            highlight_code_blocks: std::env::var("MD_HIGHLIGHT").is_ok(),
                            mono_font: mono.clone().unwrap_or_else(|| "Menlo".into()),
                            file_glyph: Some("icons/file-text.svg".into()),
                            thread_glyph: Some("icons/message-square-text.svg".into()),
                            ..MarkdownStyle::dark()
                        };
                        view.set_style(style, cx);
                        view.set_link_capabilities(
                            LinkCapabilities {
                                select_thread: true,
                                open_file: true,
                                open_url_in_app: true,
                                open_image: true,
                                open_commit: true,
                            },
                            cx,
                        );
                        view.set_known_commit(|hash| hash == "60cceb8");
                        view.set_emphasize_copy(true, cx);
                        if !streaming {
                            view.set_text(SAMPLE, false, cx);
                        }
                        view
                    });
                    cx.subscribe(&message, |_, event: &MarkdownEvent, _| {
                        eprintln!("{event:?}")
                    })
                    .detach();
                    if streaming {
                        let stream = message.clone();
                        cx.spawn(async move |cx| {
                            let mut written = 0;
                            while written < SAMPLE.len() {
                                cx.background_executor()
                                    .timer(Duration::from_millis(30))
                                    .await;
                                let mut next = (written + 7).min(SAMPLE.len());
                                while !SAMPLE.is_char_boundary(next) {
                                    next += 1;
                                }
                                written = next;
                                let done = written == SAMPLE.len();
                                let _ = stream.update(cx, |view, cx| {
                                    view.set_text(&SAMPLE[..written], !done, cx)
                                });
                            }
                        })
                        .detach();
                    }
                    cx.new(|_| Sample { message })
                },
            )
            .expect("open window");
            cx.activate(true);
        });
}

fn env_px(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
