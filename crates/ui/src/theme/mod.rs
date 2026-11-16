//! Themes: the catalogue (domain/theme.ts), each theme's primitives (themes.css), the semantic tokens
//! derived from them (tokens.css), and the active theme as a GPUI global.

mod metrics;
pub mod palette;
mod palettes;
pub mod tokens;

use gpui::{App, Global, WindowAppearance};

pub use metrics::Metrics;
pub use palette::Palette;
pub use tokens::{Elevation, Environment, MaterialGround, Motion, Pane, Rim, ShadowLayer, Tone, ToneColors, Tokens, WindowMaterial};

use crate::typography::Typography;

/// Whether a theme paints on a dark or a light ground, which native chrome follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeVariant {
    Dark,
    Light,
}

impl ThemeVariant {
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeVariant::Dark => "dark",
            ThemeVariant::Light => "light",
        }
    }
}

/// The ground the user asked for. `Auto` hands the choice to the system's own appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeMode {
    Dark,
    Light,
    Auto,
}

impl ThemeMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "dark" => Some(ThemeMode::Dark),
            "light" => Some(ThemeMode::Light),
            "auto" => Some(ThemeMode::Auto),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
            ThemeMode::Auto => "auto",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeInfo {
    pub id: &'static str,
    pub label: &'static str,
    /// The family the picker offers, which holds one theme per variant.
    pub family: &'static str,
    pub variant: ThemeVariant,
    /// The theme's bg-0, which the window paints before anything else has drawn.
    pub canvas: u32,
}

const fn theme(id: &'static str, label: &'static str, family: &'static str, variant: ThemeVariant, canvas: u32) -> ThemeInfo {
    ThemeInfo { id, label, family, variant, canvas }
}

use ThemeVariant::{Dark as D, Light as L};

pub const THEMES: &[ThemeInfo] = &[
    theme("aicodingtool-dark", "AI Coding Tool Dark", "AI Coding Tool", D, 0x0e1117),
    theme("aicodingtool-light", "AI Coding Tool Light", "AI Coding Tool", L, 0xfaf9f6),
    theme("ayu-dark", "Ayu Dark", "Ayu", D, 0x0b0e14),
    theme("ayu-light", "Ayu Light", "Ayu", L, 0xf8f9fa),
    theme("catppuccin-mocha", "Catppuccin Mocha", "Catppuccin", D, 0x11111b),
    theme("catppuccin-latte", "Catppuccin Latte", "Catppuccin", L, 0xeff1f5),
    theme("dracula", "Dracula", "Dracula", D, 0x21222c),
    theme("alucard", "Alucard", "Dracula", L, 0xfffbeb),
    theme("everforest-dark", "Everforest Dark", "Everforest", D, 0x2d353b),
    theme("everforest-light", "Everforest Light", "Everforest", L, 0xfdf6e3),
    theme("github-dark", "GitHub Dark", "GitHub", D, 0x0d1117),
    theme("github-light", "GitHub Light", "GitHub", L, 0xf6f8fa),
    theme("gruvbox-dark", "Gruvbox Dark", "Gruvbox", D, 0x1d2021),
    theme("gruvbox-light", "Gruvbox Light", "Gruvbox", L, 0xfbf1c7),
    theme("kanagawa-wave", "Kanagawa Wave", "Kanagawa", D, 0x1f1f28),
    theme("kanagawa-lotus", "Kanagawa Lotus", "Kanagawa", L, 0xf2ecbc),
    theme("lights-out", "Lights Out", "Lights Out", D, 0x000000),
    theme("lights-on", "Lights On", "Lights Out", L, 0xffffff),
    theme("material-darker", "Material Darker", "Material", D, 0x212121),
    theme("material-lighter", "Material Lighter", "Material", L, 0xfafafa),
    theme("monokai", "Monokai", "Monokai", D, 0x1e1f1c),
    theme("monokai-light", "Monokai Light", "Monokai", L, 0xfbf9f2),
    theme("nier-automata-dark", "NieR: Automata Dark", "NieR: Automata", D, 0x2f2d26),
    theme("nier-automata", "NieR: Automata", "NieR: Automata", L, 0xccc8b1),
    theme("night-owl", "Night Owl", "Night Owl", D, 0x011627),
    theme("light-owl", "Light Owl", "Night Owl", L, 0xfbfbfb),
    theme("nord", "Nord", "Nord", D, 0x2e3440),
    theme("nord-snow", "Nord Snow Storm", "Nord", L, 0xeceff4),
    theme("one-dark", "One Dark", "One", D, 0x21252b),
    theme("one-light", "One Light", "One", L, 0xfafafa),
    theme("rose-pine", "Rosé Pine", "Rosé Pine", D, 0x191724),
    theme("rose-pine-dawn", "Rosé Pine Dawn", "Rosé Pine", L, 0xfaf4ed),
    theme("solarized-dark", "Solarized Dark", "Solarized", D, 0x002b36),
    theme("solarized-light", "Solarized Light", "Solarized", L, 0xfdf6e3),
    theme("tokyo-night", "Tokyo Night", "Tokyo Night", D, 0x16161e),
    theme("tokyo-night-day", "Tokyo Night Day", "Tokyo Night", L, 0xe1e2e7),
    theme("vitesse-dark", "Vitesse Dark", "Vitesse", D, 0x121212),
    theme("vitesse-light", "Vitesse Light", "Vitesse", L, 0xffffff),
];

pub const DEFAULT_THEME: &str = "aicodingtool-dark";
pub const DEFAULT_THEME_MODE: ThemeMode = ThemeMode::Dark;

/// A theme the app no longer ships reports nothing, so the caller falls back to the default.
pub fn theme_by_id(id: &str) -> Option<&'static ThemeInfo> {
    THEMES.iter().find(|entry| entry.id == id)
}

pub fn theme_or_default(id: Option<&str>) -> &'static ThemeInfo {
    id.and_then(theme_by_id).unwrap_or_else(|| theme_by_id(DEFAULT_THEME).expect("default theme"))
}

pub fn theme_mode_or_default(value: Option<&str>) -> ThemeMode {
    value.and_then(ThemeMode::parse).unwrap_or(DEFAULT_THEME_MODE)
}

/// The families the picker draws, in the order they were declared.
pub fn theme_families() -> Vec<&'static str> {
    let mut families: Vec<&'static str> = Vec::new();
    for entry in THEMES {
        if !families.contains(&entry.family) {
            families.push(entry.family);
        }
    }
    families
}

/// Which ground a mode lands on; only `Auto` needs the system's own appearance to answer.
pub fn variant_for(mode: ThemeMode, system_dark: bool) -> ThemeVariant {
    match mode {
        ThemeMode::Auto if system_dark => ThemeVariant::Dark,
        ThemeMode::Auto => ThemeVariant::Light,
        ThemeMode::Dark => ThemeVariant::Dark,
        ThemeMode::Light => ThemeVariant::Light,
    }
}

/// The one theme in a family that paints on this ground. A family that has never heard of the
/// variant, or of the name, falls back to the default's family so the caller always gets a theme.
pub fn theme_for(family: &str, variant: ThemeVariant) -> &'static ThemeInfo {
    THEMES
        .iter()
        .find(|entry| entry.family == family && entry.variant == variant)
        .or_else(|| {
            let default_family = theme_or_default(Some(DEFAULT_THEME)).family;
            THEMES.iter().find(|entry| entry.family == default_family && entry.variant == variant)
        })
        .expect("default family covers both variants")
}

/// What applyStoredTheme resolves: the stored theme, or under `Auto` its family's theme for the
/// system's ground.
pub fn resolve_theme(stored_theme: Option<&str>, mode: ThemeMode, system_dark: bool) -> &'static ThemeInfo {
    let chosen = theme_or_default(stored_theme);
    match mode {
        ThemeMode::Auto => theme_for(chosen.family, variant_for(mode, system_dark)),
        _ => chosen,
    }
}

pub fn palette_for(id: &str) -> Palette {
    palettes::palette(id)
        .or_else(|| palettes::palette(DEFAULT_THEME))
        .expect("default palette")
}

pub fn system_is_dark(appearance: WindowAppearance) -> bool {
    matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// The window's whole look: which theme, its tokens, the type, and the fixed metrics.
#[derive(Clone, Debug)]
pub struct Theme {
    pub info: &'static ThemeInfo,
    pub mode: ThemeMode,
    pub palette: Palette,
    pub tokens: Tokens,
    pub environment: Environment,
    pub typography: Typography,
    pub metrics: Metrics,
}

impl Global for Theme {}

impl Theme {
    pub fn new(id: &str, mode: ThemeMode, environment: Environment, typography: Typography) -> Self {
        let info = theme_or_default(Some(id));
        let palette = palette_for(info.id);
        let tokens = Tokens::derive(&palette, &environment);
        let metrics = Metrics::new(&typography, cfg!(target_os = "macos"));
        Self { info, mode, palette, tokens, environment, typography, metrics }
    }

    pub fn variant(&self) -> ThemeVariant {
        self.info.variant
    }

    /// Installs the theme as the app's global and redraws every window.
    pub fn set_global(self, cx: &mut App) {
        cx.set_global(self);
        cx.refresh_windows();
    }

    /// Switches to another theme id, keeping everything else.
    pub fn change(cx: &mut App, id: &str, mode: ThemeMode) {
        let current = cx.global::<Theme>();
        let next = Theme::new(id, mode, current.environment, current.typography.clone());
        next.set_global(cx);
    }

    /// Under `Auto`, follows the system's appearance to the family's other theme.
    pub fn follow_system(cx: &mut App, appearance: WindowAppearance) {
        let current = cx.global::<Theme>();
        if current.mode != ThemeMode::Auto {
            return;
        }
        let next = theme_for(current.info.family, variant_for(ThemeMode::Auto, system_is_dark(appearance)));
        if next.id == current.info.id {
            return;
        }
        let id = next.id;
        Theme::change(cx, id, ThemeMode::Auto);
    }

    pub fn set_environment(cx: &mut App, environment: Environment) {
        let current = cx.global::<Theme>();
        if current.environment == environment {
            return;
        }
        let next = Theme::new(current.info.id, current.mode, environment, current.typography.clone());
        next.set_global(cx);
    }

    pub fn set_typography(cx: &mut App, typography: Typography) {
        let current = cx.global::<Theme>();
        let next = Theme::new(current.info.id, current.mode, current.environment, typography);
        next.set_global(cx);
    }
}

/// `cx.theme()` from any context that derefs to `App`.
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_theme_has_a_palette_of_its_variant() {
        for entry in THEMES {
            let palette = palettes::palette(entry.id).unwrap_or_else(|| panic!("no palette for {}", entry.id));
            assert_eq!(palette.variant, entry.variant, "{}", entry.id);
            assert_eq!(palette.bg_0, entry.canvas, "{}", entry.id);
        }
    }

    #[test]
    fn auto_takes_the_family_theme_for_the_system_ground() {
        assert_eq!(resolve_theme(Some("nord"), ThemeMode::Auto, false).id, "nord-snow");
        assert_eq!(resolve_theme(Some("nord-snow"), ThemeMode::Auto, true).id, "nord");
        assert_eq!(resolve_theme(Some("nord-snow"), ThemeMode::Dark, false).id, "nord-snow");
    }

    #[test]
    fn unknown_family_falls_back_to_the_default_family() {
        assert_eq!(theme_for("Nope", ThemeVariant::Light).id, "aicodingtool-light");
    }
}
