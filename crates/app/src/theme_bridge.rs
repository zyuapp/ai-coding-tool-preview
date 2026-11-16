//! Keeps the design system's theme in step with the appearance the workspace holds.

use act_core::state::WorkspaceState;
use act_ui::theme::{resolve_theme, Environment, Theme, ThemeMode};
use act_ui::typography::{StoredSize, Typography};
use gpui::App;

/// Whether the system reads dark, as the window last reported it.
pub fn system_dark(cx: &App) -> bool {
    !matches!(cx.window_appearance(), gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight)
}

/// The material under the window: the system's Liquid Glass where macOS has it, as in Electron.
pub fn environment() -> Environment {
    use act_devices::glass::{self, WindowMaterial};
    match WindowMaterial::current() {
        WindowMaterial::Glass => Environment { material: act_ui::theme::WindowMaterial::Glass, glass_tint: glass::system_glass_tint() as f32, ..Environment::default() },
        WindowMaterial::Vibrancy => Environment { material: act_ui::theme::WindowMaterial::Vibrancy, ..Environment::default() },
        WindowMaterial::None => Environment::default(),
    }
}

pub fn apply(state: &WorkspaceState, cx: &mut App) {
    let mode = match state.theme_mode {
        act_core::domain::appearance::ThemeMode::Dark => ThemeMode::Dark,
        act_core::domain::appearance::ThemeMode::Light => ThemeMode::Light,
        act_core::domain::appearance::ThemeMode::Auto => ThemeMode::Auto,
    };
    act_devices::glass::set_app_appearance(match mode {
        ThemeMode::Auto => None,
        ThemeMode::Light => Some(false),
        ThemeMode::Dark => Some(true),
    });
    let theme = resolve_theme(Some(&state.theme), mode, system_dark(cx));
    let system_mono = act_ui::fonts::system_mono_family(cx);
    let typography = Typography::new(Some(&state.ui_font), Some(&state.mono_font), StoredSize::Number(state.reading_size), StoredSize::Number(state.terminal_size), system_mono);
    if let Some(current) = cx.try_global::<Theme>() {
        if current.info.id == theme.id && current.mode == mode && current.typography == typography && current.environment == environment() {
            return;
        }
    }
    let theme = Theme::new(theme.id, mode, environment(), typography);
    gpui::set_backdrop_ground(theme.tokens.ground.map(Into::into));
    theme.set_global(cx);
}

/// Remembers the ground the window paints before anything has drawn, as the Electron app's main
/// process does, so the next launch opens on the theme the user chose.
pub fn remember(state: &WorkspaceState, file: &std::path::Path, cx: &App) {
    let follow = state.theme_mode == act_core::domain::appearance::ThemeMode::Auto;
    let mode = if follow { ThemeMode::Auto } else if state.theme_mode == act_core::domain::appearance::ThemeMode::Light { ThemeMode::Light } else { ThemeMode::Dark };
    let theme = resolve_theme(Some(&state.theme), mode, system_dark(cx));
    let next = act_store::files::WindowTheme { variant: theme.variant.as_str().to_string(), canvas: format!("#{:06x}", theme.canvas), follow: Some(follow) };
    if act_store::files::load_window_theme(file) != next {
        let _ = act_store::files::save_window_theme(file, &next);
    }
}
