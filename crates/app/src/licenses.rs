//! The Open Source Licenses reader (license-window.ts): one window, the shipped notices listed down
//! its side and the chosen one read beside them.

use act_ui::theme::ActiveTheme;
use gpui::{div, list, prelude::*, px, size, App, Bounds, Context, FontWeight, ListAlignment, ListState, SharedString, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions};

/// The overview comes first; the rest follow by name.
const NOTICES: &[(&str, &str)] = &[
    ("THIRD-PARTY-NOTICES.txt", include_str!("../../../assets/legal/THIRD-PARTY-NOTICES.txt")),
    ("AI-CODING-TOOL-MIT.txt", include_str!("../../../assets/legal/AI-CODING-TOOL-MIT.txt")),
    #[cfg(target_os = "linux")]
    ("APPIMAGE-RUNTIME.txt", include_str!("../../../assets/legal/linux/APPIMAGE-RUNTIME.txt")),
    ("CUA-MIT.txt", include_str!("../../../assets/legal/CUA-MIT.txt")),
    #[cfg(target_os = "linux")]
    ("CUA-RUST-DEPENDENCIES-LINUX.txt", include_str!("../../../assets/legal/linux/CUA-RUST-DEPENDENCIES-LINUX.txt")),
    ("CUA-THIRD-PARTY-NOTICES.md", include_str!("../../../assets/legal/CUA-THIRD-PARTY-NOTICES.md")),
    ("DICTATION-SWIFT-DEPENDENCIES.txt", include_str!("../../../assets/legal/DICTATION-SWIFT-DEPENDENCIES.txt")),
    ("FONTS.txt", include_str!("../../../assets/legal/FONTS.txt")),
    ("ICONS.txt", include_str!("../../../assets/legal/ICONS.txt")),
    ("INTER-OFL-1.1.txt", include_str!("../../../assets/legal/INTER-OFL-1.1.txt")),
    ("MERMAID-THIRD-PARTY-NOTICES.txt", include_str!("../../../assets/legal/MERMAID-THIRD-PARTY-NOTICES.txt")),
    ("MPL-2.0.txt", include_str!("../../../assets/legal/MPL-2.0.txt")),
    ("TEXTMATE-GRAMMARS.txt", include_str!("../../../assets/legal/TEXTMATE-GRAMMARS.txt")),
];

struct LicenseWindow(WindowHandle<LicenseView>);

impl gpui::Global for LicenseWindow {}

/// Opens the reader, or brings the one already open forward.
pub fn open(cx: &mut App) {
    if let Some(handle) = cx.try_global::<LicenseWindow>().map(|open| open.0) {
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(1000.0), px(760.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions { title: Some("Open Source Licenses".into()), appears_transparent: false, traffic_light_position: None }),
        window_min_size: Some(size(px(650.0), px(400.0))),
        ..Default::default()
    };
    match cx.open_window(options, |_, cx| cx.new(|_| LicenseView::new())) {
        Ok(handle) => cx.set_global(LicenseWindow(handle)),
        Err(error) => eprintln!("Could not open the licenses: {error:#}"),
    }
}

/// A notice is laid out a line at a time, and only the lines on screen, so a long one stays light.
struct LicenseView {
    chosen: usize,
    lines: std::rc::Rc<Vec<SharedString>>,
    list: ListState,
}

impl LicenseView {
    fn new() -> Self {
        let lines = notice_lines(0);
        let list = ListState::new(lines.len(), ListAlignment::Top, px(600.0));
        Self { chosen: 0, lines, list }
    }

    fn choose(&mut self, index: usize) {
        self.chosen = index;
        self.lines = notice_lines(index);
        self.list.reset(self.lines.len());
    }
}

fn notice_lines(index: usize) -> std::rc::Rc<Vec<SharedString>> {
    let text = NOTICES.get(index).map(|(_, text)| *text).unwrap_or_default();
    std::rc::Rc::new(text.lines().map(|line| SharedString::from(if line.is_empty() { " " } else { line })).collect())
}

impl Render for LicenseView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let t = &theme.tokens;
        let typography = &theme.typography;
        let links = NOTICES.iter().enumerate().map(|(index, (name, _))| {
            let label: SharedString = if index == 0 { "Overview".into() } else { (*name).into() };
            let chosen = index == self.chosen;
            div()
                .id(("license", index))
                .px(px(8.0))
                .py(px(10.0))
                .rounded(px(6.0))
                .text_size(px(12.0))
                .cursor_pointer()
                .when(chosen, |link| link.bg(t.wash))
                .hover(|style| style.bg(t.wash))
                .child(label)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.choose(index);
                    cx.notify();
                }))
        });
        let nav = div()
            .id("license-nav")
            .w(px(250.0))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .px(px(12.0))
            .py(px(20.0))
            .border_r_1()
            .border_color(t.line)
            .child(div().mx(px(8.0)).mb(px(20.0)).text_size(px(16.0)).font_weight(FontWeight::BOLD).child("Open Source Licenses"))
            .children(links);
        let lines = self.lines.clone();
        let main = div()
            .id("license-text")
            .flex_1()
            .min_w_0()
            .h_full()
            .font_family(typography.mono_family.clone())
            .text_size(px(13.0))
            .line_height(gpui::relative(1.65))
            .child(list(self.list.clone(), move |index, _, _| div().px(px(28.0)).child(lines.get(index).cloned().unwrap_or_default()).into_any_element()).size_full().py(px(28.0)));
        div().size_full().flex().bg(t.frame).text_color(t.ink).font_family(typography.ui_family.clone()).text_size(px(14.0)).child(nav).child(main)
    }
}
