//! The design system: theme tokens, fonts, icons and the primitives every view is built from.

pub mod assets;
pub mod color;
pub mod components;
mod embedded;
pub mod focus;
pub mod fonts;
pub mod icon;
mod icon_names;
pub mod input;
pub mod overlay;
pub mod style;
pub mod theme;
pub mod tooltip;
pub mod typography;

pub use assets::Assets;
pub use icon::{icon, Icon, IconName};
pub use theme::{ActiveTheme, Theme, ThemeMode, ThemeVariant, Tokens};

use gpui::App;

/// Everything the design system needs before the first window: the bundled faces, the editing and
/// menu keys, focus-ring tracking, and a theme (the default unless one is already set).
pub fn init(cx: &mut App) {
    if let Err(error) = fonts::register_fonts(cx) {
        eprintln!("act_ui: could not register the bundled fonts: {error:#}");
    }
    input::bind_keys(cx);
    components::menu::bind_keys(cx);
    components::dialog::bind_keys(cx);
    components::popover::bind_keys(cx);
    focus::init(cx);
    if cx.try_global::<Theme>().is_none() {
        let typography = typography::Typography::new(
            None,
            None,
            typography::StoredSize::Missing,
            typography::StoredSize::Missing,
            fonts::system_mono_family(cx),
        );
        let theme = Theme::new(theme::DEFAULT_THEME, theme::DEFAULT_THEME_MODE, theme::Environment::default(), typography);
        cx.set_global(theme);
    }
}

/// The root of a window: the UI face, the reading size, ink on the canvas (or on the frame, where
/// the window is one glass sheet). Views build inside it.
pub fn root(cx: &App) -> gpui::Div {
    use gpui::{div, px, Styled};
    let theme = cx.global::<Theme>();
    div()
        .size_full()
        .font_family(theme.typography.ui_family.clone())
        .text_size(px(theme.metrics.text_content))
        .line_height(gpui::line_height_normal())
        .text_color(theme.tokens.ink)
        .bg(if theme.tokens.page.a == 0.0 { theme.tokens.frame } else { theme.tokens.canvas })
}
