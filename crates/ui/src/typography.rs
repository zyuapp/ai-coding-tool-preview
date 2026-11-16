//! The type the window draws in (domain/typography.ts): which families, and how big the two things
//! people actually resize. Chrome keeps the sizes it was designed at.

use gpui::SharedString;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Font {
    pub id: SharedString,
    pub label: SharedString,
}

/// A family the app bundles, with the name its faces register under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BundledFont {
    pub id: &'static str,
    pub label: &'static str,
    /// `None` defers to the system's own face.
    pub family: Option<&'static str>,
}

/// The families the window's chrome and prose can be set in.
pub const UI_FONTS: &[BundledFont] = &[
    BundledFont { id: "system", label: "System", family: None },
    BundledFont { id: "inter", label: "Inter", family: Some("Inter") },
    BundledFont { id: "plex-sans", label: "IBM Plex Sans", family: Some("IBM Plex Sans") },
    BundledFont { id: "geist", label: "Geist", family: Some("Geist") },
];

/// The families code, diffs and the terminal can be set in.
pub const MONO_FONTS: &[BundledFont] = &[
    BundledFont { id: "system", label: "System", family: None },
    BundledFont { id: "jetbrains-mono", label: "JetBrains Mono", family: Some("JetBrains Mono") },
    BundledFont { id: "fira-code", label: "Fira Code", family: Some("Fira Code") },
    BundledFont { id: "source-code-pro", label: "Source Code Pro", family: Some("Source Code Pro") },
];

pub const DEFAULT_UI_FONT: &str = "system";
pub const DEFAULT_MONO_FONT: &str = "system";

/// The system's UI face, as GPUI names it on every platform.
pub const SYSTEM_UI_FAMILY: &str = ".SystemUIFont";

/// The monospace face macOS always has, which `--system-mono-font` reaches without SF Mono installed.
pub const MACOS_SYSTEM_MONO: &str = "Menlo";

/// Elsewhere, the first installed of these stands for the system's monospace face.
pub const SYSTEM_MONO_FAMILIES: &[&str] = &["Menlo", "DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono", "monospace"];

const CUSTOM_PREFIX: &str = "installed:";

pub fn custom_font_id(family: &str) -> String {
    format!("{CUSTOM_PREFIX}{family}")
}

/// The family a custom id names, if the id is well formed: `installed:` then a name of 1–64
/// characters from `[A-Za-z0-9 ._-]`, starting with a letter or digit.
pub fn custom_font_family(id: &str) -> Option<&str> {
    let family = id.strip_prefix(CUSTOM_PREFIX)?;
    let mut chars = family.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphanumeric() || family.len() > 64 {
        return None;
    }
    chars
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '.' | '_' | '-'))
        .then_some(family)
}

/// Whether a name could be carried as a custom id at all, which the picker filters its list by.
pub fn is_nameable_font(family: &str) -> bool {
    custom_font_family(&custom_font_id(family)).is_some()
}

fn font_by_id(known: &[BundledFont], id: &str) -> Option<Font> {
    if let Some(family) = custom_font_family(id) {
        return Some(Font { id: id.to_string().into(), label: family.to_string().into() });
    }
    known
        .iter()
        .find(|font| font.id == id)
        .map(|font| Font { id: font.id.into(), label: font.label.into() })
}

/// A family the app no longer ships reports nothing, so the caller falls back to the system's own.
pub fn ui_font_by_id(id: &str) -> Option<Font> {
    font_by_id(UI_FONTS, id)
}

pub fn mono_font_by_id(id: &str) -> Option<Font> {
    font_by_id(MONO_FONTS, id)
}

pub fn ui_font_or_default(id: Option<&str>) -> Font {
    id.and_then(ui_font_by_id).unwrap_or_else(|| ui_font_by_id(DEFAULT_UI_FONT).expect("system font"))
}

pub fn mono_font_or_default(id: Option<&str>) -> Font {
    id.and_then(mono_font_by_id).unwrap_or_else(|| mono_font_by_id(DEFAULT_MONO_FONT).expect("system font"))
}

/// A size the user can land on, in px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeRange {
    pub min: u32,
    pub max: u32,
    pub default: u32,
    /// The px the four named rungs this once offered drew at, so a stored choice survives.
    pub legacy: [(&'static str, u32); 4],
}

pub const READING_SIZE: SizeRange = SizeRange {
    min: 12,
    max: 24,
    default: 15,
    legacy: [("small", 13), ("regular", 15), ("large", 17), ("larger", 19)],
};

pub const TERMINAL_SIZE: SizeRange = SizeRange {
    min: 10,
    max: 22,
    default: 12,
    legacy: [("small", 11), ("regular", 12), ("large", 13), ("larger", 15)],
};

/// A stored size: a number, a rung this once offered, or anything else.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StoredSize<'a> {
    Number(f64),
    Name(&'a str),
    Missing,
}

pub fn clamp_size(range: SizeRange, value: f64) -> u32 {
    (value.round().max(range.min as f64).min(range.max as f64)) as u32
}

/// A number in range, a rung this once offered, or nothing at all.
pub fn size_by_id(range: SizeRange, value: StoredSize) -> Option<u32> {
    match value {
        StoredSize::Number(number) if number.is_finite() => {
            (number >= range.min as f64 && number <= range.max as f64).then(|| number.round() as u32)
        }
        StoredSize::Name(name) => range
            .legacy
            .iter()
            .find(|(rung, _)| *rung == name)
            .map(|(_, px)| clamp_size(range, *px as f64)),
        _ => None,
    }
}

pub fn size_or_default(range: SizeRange, value: StoredSize) -> u32 {
    size_by_id(range, value).unwrap_or(range.default)
}

/// The next px up or down. The ends hold rather than wrap.
pub fn step_size(range: SizeRange, value: StoredSize, up: bool) -> u32 {
    let current = size_or_default(range, value) as f64;
    clamp_size(range, if up { current + 1.0 } else { current - 1.0 })
}

/// The four typographic choices, resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Typography {
    pub ui_font: Font,
    pub mono_font: Font,
    pub reading_size: u32,
    pub terminal_size: u32,
    /// The GPUI family the chrome and prose are set in.
    pub ui_family: SharedString,
    /// The GPUI family code and the terminal are set in.
    pub mono_family: SharedString,
}

impl Default for Typography {
    fn default() -> Self {
        Self::new(None, None, StoredSize::Missing, StoredSize::Missing, if cfg!(target_os = "macos") { MACOS_SYSTEM_MONO } else { SYSTEM_MONO_FAMILIES[0] })
    }
}

impl Typography {
    /// Resolves stored choices. `system_mono` is the installed face `system` mono stands for, as
    /// `crate::fonts::system_mono_family` finds it.
    pub fn new(ui_font: Option<&str>, mono_font: Option<&str>, reading: StoredSize, terminal: StoredSize, system_mono: &str) -> Self {
        let ui_font = ui_font_or_default(ui_font);
        let mono_font = mono_font_or_default(mono_font);
        let ui_family = family_for(UI_FONTS, &ui_font.id, SYSTEM_UI_FAMILY);
        let mono_family = family_for(MONO_FONTS, &mono_font.id, system_mono);
        Self {
            ui_font,
            mono_font,
            reading_size: size_or_default(READING_SIZE, reading),
            terminal_size: size_or_default(TERMINAL_SIZE, terminal),
            ui_family,
            mono_family,
        }
    }
}

fn family_for(known: &[BundledFont], id: &str, system: &str) -> SharedString {
    if let Some(family) = custom_font_family(id) {
        return family.to_string().into();
    }
    known
        .iter()
        .find(|font| font.id == id)
        .and_then(|font| font.family)
        .unwrap_or(system)
        .to_string()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_ids_refuse_names_that_could_escape_a_stack() {
        assert_eq!(custom_font_family("installed:Menlo"), Some("Menlo"));
        assert_eq!(custom_font_family("installed:Comic Sans MS"), Some("Comic Sans MS"));
        assert_eq!(custom_font_family("installed:\"x"), None);
        assert_eq!(custom_font_family("installed: lead"), None);
        assert_eq!(custom_font_family(&custom_font_id(&"a".repeat(65))), None);
    }

    #[test]
    fn sizes_accept_numbers_in_range_and_legacy_rungs() {
        assert_eq!(size_by_id(READING_SIZE, StoredSize::Number(16.4)), Some(16));
        assert_eq!(size_by_id(READING_SIZE, StoredSize::Number(30.0)), None);
        assert_eq!(size_by_id(TERMINAL_SIZE, StoredSize::Name("larger")), Some(15));
        assert_eq!(step_size(READING_SIZE, StoredSize::Number(24.0), true), 24);
    }

    #[test]
    fn custom_family_is_used_as_named() {
        let typography = Typography::new(Some("installed:Menlo"), Some("fira-code"), StoredSize::Missing, StoredSize::Missing, "Menlo");
        assert_eq!(typography.ui_family.as_ref(), "Menlo");
        assert_eq!(typography.mono_family.as_ref(), "Fira Code");
        assert_eq!(typography.reading_size, 15);
    }
}
