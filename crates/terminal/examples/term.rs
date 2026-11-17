//! A window with a working login shell: `cargo run -p act_terminal --example term`.
//!
//! `ACT_TERM_INPUT` is typed into the shell once it starts.

use std::borrow::Cow;
use std::path::Path;

use act_terminal::{TerminalFont, TerminalHost, TerminalStatus, TerminalView, TerminalViewEvent};
use futures::StreamExt;
use gpui::{
    actions, div, prelude::*, px, size, App, Bounds, Context, Entity, Focusable, KeyBinding, Window,
    WindowBounds, WindowOptions,
};

actions!(term_example, [Quit]);

const TERMINAL_ID: &str = "example";

struct Shell {
    terminal: Entity<TerminalView>,
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().child(self.terminal.clone())
    }
}

fn load_fonts(cx: &App) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/fira-code");
    let fonts: Vec<Cow<'static, [u8]>> = ["FiraCode-Regular.ttf", "FiraCode-Bold.ttf"]
        .iter()
        .filter_map(|name| std::fs::read(dir.join(name)).ok())
        .map(Cow::Owned)
        .collect();
    if !fonts.is_empty() {
        let _ = cx.text_system().add_fonts(fonts);
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        load_fonts(cx);
        act_terminal::init(cx);
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let (updates, mut received) = futures::channel::mpsc::unbounded();
        let host = TerminalHost::new(move |update| {
            let _ = updates.unbounded_send(update);
        });
        let cwd = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        host.start(TERMINAL_ID, &cwd);
        let screen = host.screen(TERMINAL_ID).expect("the shell's screen");

        let bounds = Bounds::centered(None, size(px(900.), px(560.)), cx);
        let window = cx
            .open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                |window, cx| {
                    let terminal = cx.new(|cx| {
                        let mut view = TerminalView::new(screen, window, cx);
                        view.set_font(TerminalFont { family: "Fira Code".into(), size: px(12.) }, cx);
                        view
                    });
                    let input = host.clone();
                    cx.subscribe(&terminal, move |_, event, _| match event {
                        TerminalViewEvent::Input(data) => {
                            let _ = input.write(TERMINAL_ID, data);
                        }
                        TerminalViewEvent::Resize { cols, rows } => {
                            let _ = input.resize(TERMINAL_ID, *cols as u32, *rows as u32);
                        }
                        _ => {}
                    })
                    .detach();
                    window.focus(&terminal.focus_handle(cx), cx);
                    cx.new(|_| Shell { terminal })
                },
            )
            .expect("a window");
        window.update(cx, |_, window, _| window.set_window_title("Terminal")).ok();
        cx.activate(true);

        if let Ok(text) = std::env::var("ACT_TERM_INPUT") {
            let _ = host.write(TERMINAL_ID, &text);
        }

        let closing = host.clone();
        cx.on_app_quit(move |_| {
            closing.stop_all();
            async {}
        })
        .detach();

        cx.spawn(async move |cx| {
            while let Some(update) = received.next().await {
                if update.status == Some(TerminalStatus::Exited) {
                    let _ = cx.update(|cx| cx.quit());
                    break;
                }
                if let Some(title) = update.title {
                    let _ = window.update(cx, |_, window, _| window.set_window_title(&title));
                }
            }
        })
        .detach();
    });
}
