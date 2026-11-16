//! Registers the bundled faces with GPUI's text system, and lists the installed families a picker
//! may offer (system-fonts.ts).

use std::borrow::Cow;

use gpui::App;

use crate::embedded::FILES;
use crate::typography::{is_nameable_font, SYSTEM_MONO_FAMILIES};

/// Adds every bundled TTF to the text system. Call once, before the first window draws.
pub fn register_fonts(cx: &mut App) -> anyhow::Result<()> {
    let faces: Vec<Cow<'static, [u8]>> = FILES
        .iter()
        .filter(|(name, _)| name.starts_with("fonts/") && (name.ends_with(".ttf") || name.ends_with(".otf")))
        .map(|(_, bytes)| Cow::Borrowed(*bytes))
        .collect();
    cx.text_system().add_fonts(faces)?;
    Ok(())
}


/// The system's installed families (not the bundled ones, as `queryLocalFonts` reports), one per
/// family, sorted, keeping only names a custom id can carry.
pub fn installed_fonts(cx: &App) -> Vec<String> {
    let mut families: Vec<String> = cx
        .text_system()
        .installed_font_names()
        .into_iter()
        .filter(|name| !name.starts_with('.') && is_nameable_font(name))
        .collect();
    families.sort();
    families.dedup();
    families.sort_by(|a, b| locale_order(a, b));
    families
}

/// `localeCompare` for the characters a nameable family holds: spaces, then `_ - .`, then digits,
/// then letters regardless of case; on a tie, lowercase first.
fn locale_order(a: &str, b: &str) -> std::cmp::Ordering {
    let primary = |ch: char| match ch {
        ' ' => 1,
        '_' => 2,
        '-' => 3,
        '.' => 4,
        '0'..='9' => 10 + ch as u32,
        _ => 100 + ch.to_ascii_lowercase() as u32,
    };
    a.chars()
        .map(primary)
        .cmp(b.chars().map(primary))
        .then_with(|| a.chars().map(|ch| ch.is_ascii_uppercase()).cmp(b.chars().map(|ch| ch.is_ascii_uppercase())))
}

#[cfg(test)]
mod tests {
    use super::locale_order;

    #[test]
    fn families_sort_as_locale_compare_does() {
        let mut names = vec!["Noto_Sans", "Noto Sans", "noto", "Noto", "Noto-Sans", "Noto.Sans", "Noto2", "NotoSans"];
        names.sort_by(|a, b| locale_order(a, b));
        assert_eq!(names, ["noto", "Noto", "Noto Sans", "Noto_Sans", "Noto-Sans", "Noto.Sans", "Noto2", "NotoSans"]);
    }
}

/// The face `system` mono stands for: the first of the platform's monospace families installed.
pub fn system_mono_family(cx: &App) -> &'static str {
    let names = cx.text_system().all_font_names();
    if cfg!(target_os = "macos") {
        // Chromium skips `ui-monospace`, so `--system-mono-font` is SFMono-Regular where SF Mono is
        // installed and Menlo otherwise.
        return if names.iter().any(|name| name == "SF Mono") { "SF Mono" } else { "Menlo" };
    }
    if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        // GPUI resolves `monospace` through fontconfig, as Chromium does past an absent Menlo.
        return if names.iter().any(|name| name == "Menlo") { "Menlo" } else { "monospace" };
    }
    SYSTEM_MONO_FAMILIES
        .iter()
        .copied()
        .find(|family| names.iter().any(|name| name == family))
        .unwrap_or("Menlo")
}
