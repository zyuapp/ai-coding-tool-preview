//! Page zoom as Electron's View menu gives it: levels in half steps, a factor of 1.2 per level,
//! clamped to Chromium's 25%–500%, remembered per profile.

use std::path::{Path, PathBuf};

use gpui::App;

const FILE: &str = "window-zoom.v1.json";
const STEP: f64 = 0.5;
const MIN_FACTOR: f64 = 0.25;
const MAX_FACTOR: f64 = 5.0;

struct ZoomGlobal {
    level: f64,
    file: PathBuf,
}

impl gpui::Global for ZoomGlobal {}

pub fn factor(level: f64) -> f64 {
    1.2f64.powf(level)
}

fn clamp_level(level: f64) -> f64 {
    let min = MIN_FACTOR.ln() / 1.2f64.ln();
    let max = MAX_FACTOR.ln() / 1.2f64.ln();
    level.clamp(min, max)
}

/// The level this profile last used; a fresh profile takes the Electron app's.
pub fn load_level(user_data: &Path) -> f64 {
    let own = std::fs::read_to_string(user_data.join(FILE)).ok().and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok()).and_then(|value| value.get("level")?.as_f64());
    let level = own.or_else(|| chromium_level(&user_data.join("Preferences"))).or_else(|| electron_profile().and_then(|dir| chromium_level(&dir.join("Preferences")))).unwrap_or(0.0);
    if level.is_finite() {
        clamp_level(level)
    } else {
        0.0
    }
}

/// The Electron dev app's profile folder, whose Chromium preferences hold the zoom it renders at.
fn electron_profile() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let base = if cfg!(target_os = "macos") { PathBuf::from(home).join("Library/Application Support") } else { std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(home).join(".config")) };
    Some(base.join("AI Coding Tool Dev"))
}

/// The zoom level Chromium keeps for the app's page in a profile's `Preferences`.
fn chromium_level(preferences: &Path) -> Option<f64> {
    let text = std::fs::read_to_string(preferences).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let partitions = value.get("partition")?.get("per_host_zoom_levels")?.as_object()?;
    partitions.values().filter_map(|hosts| hosts.as_object()).flat_map(|hosts| hosts.iter()).find(|(page, _)| page.ends_with("renderer/index.html")).and_then(|(_, level)| level.as_f64())
}

/// Applies the profile's zoom before the first window opens.
pub fn init(user_data: &Path, cx: &mut App) {
    let level = load_level(user_data);
    gpui::set_page_zoom(factor(level) as f32);
    cx.set_global(ZoomGlobal { level, file: user_data.join(FILE) });
}

pub fn zoom_in(cx: &mut App) {
    change(|level| level + STEP, cx);
}

pub fn zoom_out(cx: &mut App) {
    change(|level| level - STEP, cx);
}

pub fn reset(cx: &mut App) {
    change(|_| 0.0, cx);
}

fn change(next: impl FnOnce(f64) -> f64, cx: &mut App) {
    let Some(global) = cx.try_global::<ZoomGlobal>() else { return };
    let level = clamp_level(next(global.level));
    if (level - global.level).abs() < f64::EPSILON {
        return;
    }
    let file = global.file.clone();
    cx.set_global(ZoomGlobal { level, file: file.clone() });
    let zoom = factor(level) as f32;
    gpui::set_page_zoom(zoom);
    // A keystroke runs inside its window's update, so the windows relayout once it returns.
    cx.defer(move |cx| {
        for window in cx.windows() {
            let _ = window.update(cx, |_, window, cx| window.set_page_zoom(zoom, cx));
        }
    });
    if let Err(error) = std::fs::write(&file, serde_json::json!({ "level": level }).to_string()) {
        eprintln!("Could not remember the zoom level: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_electron_page_level() {
        let dir = std::env::temp_dir().join(format!("act-zoom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let preferences = dir.join("Preferences");
        std::fs::write(&preferences, r#"{"partition":{"per_host_zoom_levels":{"1":{"file:///x/dist/renderer/index.html":0.8964319661521385}}}}"#).unwrap();
        let level = chromium_level(&preferences).unwrap();
        assert!((factor(level) - 1.1775).abs() < 0.001);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clamps_to_chromium_limits() {
        assert!((factor(clamp_level(100.0)) - MAX_FACTOR).abs() < 1e-9);
        assert!((factor(clamp_level(-100.0)) - MIN_FACTOR).abs() < 1e-9);
    }
}
