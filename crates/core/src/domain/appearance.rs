//! Themes and type: what the window paints in and sets its text in.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeVariant {
    Dark,
    Light,
}

/// The ground the user asked for. Auto hands the choice to the system's own appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub id: &'static str,
    pub label: &'static str,
    pub family: &'static str,
    pub variant: ThemeVariant,
    /// The theme's background, which the window paints before anything is drawn.
    pub canvas: &'static str,
}

const fn theme(id: &'static str, label: &'static str, family: &'static str, variant: ThemeVariant, canvas: &'static str) -> Theme {
    Theme { id, label, family, variant, canvas }
}

use ThemeVariant::{Dark, Light};

pub const THEMES: [Theme; 38] = [
    theme("aicodingtool-dark", "AI Coding Tool Dark", "AI Coding Tool", Dark, "#0e1117"),
    theme("aicodingtool-light", "AI Coding Tool Light", "AI Coding Tool", Light, "#faf9f6"),
    theme("ayu-dark", "Ayu Dark", "Ayu", Dark, "#0b0e14"),
    theme("ayu-light", "Ayu Light", "Ayu", Light, "#f8f9fa"),
    theme("catppuccin-mocha", "Catppuccin Mocha", "Catppuccin", Dark, "#11111b"),
    theme("catppuccin-latte", "Catppuccin Latte", "Catppuccin", Light, "#eff1f5"),
    theme("dracula", "Dracula", "Dracula", Dark, "#21222c"),
    theme("alucard", "Alucard", "Dracula", Light, "#fffbeb"),
    theme("everforest-dark", "Everforest Dark", "Everforest", Dark, "#2d353b"),
    theme("everforest-light", "Everforest Light", "Everforest", Light, "#fdf6e3"),
    theme("github-dark", "GitHub Dark", "GitHub", Dark, "#0d1117"),
    theme("github-light", "GitHub Light", "GitHub", Light, "#f6f8fa"),
    theme("gruvbox-dark", "Gruvbox Dark", "Gruvbox", Dark, "#1d2021"),
    theme("gruvbox-light", "Gruvbox Light", "Gruvbox", Light, "#fbf1c7"),
    theme("kanagawa-wave", "Kanagawa Wave", "Kanagawa", Dark, "#1f1f28"),
    theme("kanagawa-lotus", "Kanagawa Lotus", "Kanagawa", Light, "#f2ecbc"),
    theme("lights-out", "Lights Out", "Lights Out", Dark, "#000000"),
    theme("lights-on", "Lights On", "Lights Out", Light, "#ffffff"),
    theme("material-darker", "Material Darker", "Material", Dark, "#212121"),
    theme("material-lighter", "Material Lighter", "Material", Light, "#fafafa"),
    theme("monokai", "Monokai", "Monokai", Dark, "#1e1f1c"),
    theme("monokai-light", "Monokai Light", "Monokai", Light, "#fbf9f2"),
    theme("nier-automata-dark", "NieR: Automata Dark", "NieR: Automata", Dark, "#2f2d26"),
    theme("nier-automata", "NieR: Automata", "NieR: Automata", Light, "#ccc8b1"),
    theme("night-owl", "Night Owl", "Night Owl", Dark, "#011627"),
    theme("light-owl", "Light Owl", "Night Owl", Light, "#fbfbfb"),
    theme("nord", "Nord", "Nord", Dark, "#2e3440"),
    theme("nord-snow", "Nord Snow Storm", "Nord", Light, "#eceff4"),
    theme("one-dark", "One Dark", "One", Dark, "#21252b"),
    theme("one-light", "One Light", "One", Light, "#fafafa"),
    theme("rose-pine", "Rosé Pine", "Rosé Pine", Dark, "#191724"),
    theme("rose-pine-dawn", "Rosé Pine Dawn", "Rosé Pine", Light, "#faf4ed"),
    theme("solarized-dark", "Solarized Dark", "Solarized", Dark, "#002b36"),
    theme("solarized-light", "Solarized Light", "Solarized", Light, "#fdf6e3"),
    theme("tokyo-night", "Tokyo Night", "Tokyo Night", Dark, "#16161e"),
    theme("tokyo-night-day", "Tokyo Night Day", "Tokyo Night", Light, "#e1e2e7"),
    theme("vitesse-dark", "Vitesse Dark", "Vitesse", Dark, "#121212"),
    theme("vitesse-light", "Vitesse Light", "Vitesse", Light, "#ffffff"),
];

pub const DEFAULT_THEME: &str = "aicodingtool-dark";
pub const DEFAULT_THEME_MODE: ThemeMode = ThemeMode::Dark;

pub fn theme_by_id(id: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|theme| theme.id == id)
}

pub fn theme_or_default(id: &str) -> &'static Theme {
    theme_by_id(id).unwrap_or_else(|| theme_by_id(DEFAULT_THEME).expect("default theme"))
}

/// The families the picker draws, in the order they were declared.
pub fn theme_families() -> Vec<&'static str> {
    let mut families: Vec<&'static str> = Vec::new();
    for theme in THEMES.iter() {
        if !families.contains(&theme.family) {
            families.push(theme.family);
        }
    }
    families
}

pub fn variant_for(mode: ThemeMode, system_dark: bool) -> ThemeVariant {
    match mode {
        ThemeMode::Auto => if system_dark { Dark } else { Light },
        ThemeMode::Dark => Dark,
        ThemeMode::Light => Light,
    }
}

/// The one theme in a family that paints on this ground, else the default family's.
pub fn theme_for(family: &str, variant: ThemeVariant) -> &'static Theme {
    THEMES
        .iter()
        .find(|theme| theme.family == family && theme.variant == variant)
        .or_else(|| THEMES.iter().find(|theme| theme.family == theme_or_default(DEFAULT_THEME).family && theme.variant == variant))
        .expect("default family has both variants")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Font {
    pub id: String,
    pub label: String,
}

pub const UI_FONTS: [(&str, &str); 4] = [("system", "System"), ("inter", "Inter"), ("plex-sans", "IBM Plex Sans"), ("geist", "Geist")];
pub const MONO_FONTS: [(&str, &str); 4] = [("system", "System"), ("jetbrains-mono", "JetBrains Mono"), ("fira-code", "Fira Code"), ("source-code-pro", "Source Code Pro")];
pub const DEFAULT_UI_FONT: &str = "system";
pub const DEFAULT_MONO_FONT: &str = "system";

pub fn custom_font_id(family: &str) -> String {
    format!("installed:{family}")
}

/// A family the app does not bundle, named rather than listed.
pub fn custom_font_family(id: &str) -> Option<&str> {
    let family = id.strip_prefix("installed:")?;
    let mut chars = family.chars();
    let first = chars.next()?;
    let valid = first.is_ascii_alphanumeric() && family.len() <= 64 && chars.all(|ch| ch.is_ascii_alphanumeric() || " ._-".contains(ch));
    valid.then_some(family)
}

pub fn is_nameable_font(family: &str) -> bool {
    custom_font_family(&custom_font_id(family)).is_some()
}

fn font_by_id(known: &[(&str, &str)], id: &str) -> Option<Font> {
    if let Some(family) = custom_font_family(id) {
        return Some(Font { id: id.into(), label: family.into() });
    }
    known.iter().find(|(known, _)| *known == id).map(|(id, label)| Font { id: (*id).into(), label: (*label).into() })
}

pub fn ui_font_by_id(id: &str) -> Option<Font> {
    font_by_id(&UI_FONTS, id)
}

pub fn mono_font_by_id(id: &str) -> Option<Font> {
    font_by_id(&MONO_FONTS, id)
}

pub fn ui_font_or_default(id: &str) -> Font {
    ui_font_by_id(id).unwrap_or_else(|| ui_font_by_id(DEFAULT_UI_FONT).expect("default ui font"))
}

pub fn mono_font_or_default(id: &str) -> Font {
    mono_font_by_id(id).unwrap_or_else(|| mono_font_by_id(DEFAULT_MONO_FONT).expect("default mono font"))
}

/// A size the user can land on, in px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeRange {
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// The px the four named rungs this once offered drew at.
    pub legacy: [(&'static str, f64); 4],
}

pub const READING_SIZE: SizeRange = SizeRange { min: 12.0, max: 24.0, default: 15.0, legacy: [("small", 13.0), ("regular", 15.0), ("large", 17.0), ("larger", 19.0)] };
pub const TERMINAL_SIZE: SizeRange = SizeRange { min: 10.0, max: 22.0, default: 12.0, legacy: [("small", 11.0), ("regular", 12.0), ("large", 13.0), ("larger", 15.0)] };

/// JavaScript's Math.round: halves round up.
pub fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

pub fn clamp_size(range: &SizeRange, value: f64) -> f64 {
    range.max.min(range.min.max(js_round(value)))
}

/// A number in range, a rung this once offered, or nothing at all.
pub fn size_by_value(range: &SizeRange, value: &serde_json::Value) -> Option<f64> {
    if let Some(number) = value.as_f64() {
        return (number >= range.min && number <= range.max).then(|| js_round(number));
    }
    let name = value.as_str()?;
    range.legacy.iter().find(|(rung, _)| *rung == name).map(|(_, px)| clamp_size(range, *px))
}

pub fn size_or_default(range: &SizeRange, value: &serde_json::Value) -> f64 {
    size_by_value(range, value).unwrap_or(range.default)
}

/// The next px up or down; the ends hold rather than wrap.
pub fn step_size(range: &SizeRange, value: f64, delta: i8) -> f64 {
    let current = size_or_default(range, &serde_json::json!(value));
    clamp_size(range, current + delta as f64)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureOptions {
    pub sound: bool,
    pub focus: bool,
}

pub const DEFAULT_CAPTURE_OPTIONS: CaptureOptions = CaptureOptions { sound: true, focus: true };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_family_falls_back_to_default_family() {
        assert_eq!(theme_for("Nope", ThemeVariant::Light).id, "aicodingtool-light");
        assert_eq!(theme_for("Nord", ThemeVariant::Light).id, "nord-snow");
    }

    #[test]
    fn sizes_read_numbers_and_legacy_rungs() {
        assert_eq!(size_by_value(&READING_SIZE, &serde_json::json!("large")), Some(17.0));
        assert_eq!(size_by_value(&READING_SIZE, &serde_json::json!(40)), None);
        assert_eq!(clamp_size(&TERMINAL_SIZE, 30.0), 22.0);
    }

    #[test]
    fn custom_fonts_refuse_quotes() {
        assert_eq!(custom_font_family("installed:Berkeley Mono"), Some("Berkeley Mono"));
        assert_eq!(custom_font_family("installed:a\"b"), None);
    }
}
