//! Where the window sat when the user last left it (window-placement.ts), so a window closed at
//! full size opens at full size.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{point, px, size, App, AsyncApp, Bounds, Pixels, WindowBounds, WindowHandle};
use serde::{Deserialize, Serialize};

pub const MIN_WIDTH: f32 = 820.0;
pub const MIN_HEIGHT: f32 = 620.0;
const DEFAULT_WIDTH: f32 = 1240.0;
const DEFAULT_HEIGHT: f32 = 820.0;
/// How much of the window's top edge has to stay on a screen for the user to drag it back.
const VISIBLE_EDGE: f32 = 80.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub full_screen: bool,
}

impl Default for Placement {
    fn default() -> Self {
        Placement { x: None, y: None, width: DEFAULT_WIDTH, height: DEFAULT_HEIGHT, maximized: false, full_screen: false }
    }
}

pub fn file(user_data: &Path) -> PathBuf {
    user_data.join("window-placement.v1.json")
}

/// An unreadable or undersized placement is the default.
pub fn load(file: &Path) -> Placement {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<Placement>(&text).ok())
        .filter(|placement| placement.width.is_finite() && placement.height.is_finite() && placement.width >= MIN_WIDTH && placement.height >= MIN_HEIGHT)
        .unwrap_or_default()
}

/// Fits a placement to the screen it sits on; one off every screen loses its position.
pub fn fit(placement: Placement, screens: &[Bounds<Pixels>]) -> Placement {
    let Some(first) = screens.first() else { return placement };
    let under = screen_under(&placement, screens);
    let screen = under.unwrap_or(first);
    let width = placement.width.min(f32::from(screen.size.width)).max(MIN_WIDTH);
    let height = placement.height.min(f32::from(screen.size.height)).max(MIN_HEIGHT);
    let mut fitted = Placement { width, height, ..placement.clone() };
    match (placement.x, placement.y, under) {
        (Some(x), Some(y), Some(screen)) => {
            let (sx, sy) = (f32::from(screen.origin.x), f32::from(screen.origin.y));
            let (sw, sh) = (f32::from(screen.size.width), f32::from(screen.size.height));
            fitted.x = Some(x.max(sx - width + VISIBLE_EDGE).min(sx + sw - VISIBLE_EDGE));
            fitted.y = Some(y.max(sy).min(sy + sh - VISIBLE_EDGE));
        }
        _ => {
            fitted.x = None;
            fitted.y = None;
        }
    }
    fitted
}

fn screen_under<'a>(placement: &Placement, screens: &'a [Bounds<Pixels>]) -> Option<&'a Bounds<Pixels>> {
    let (x, y) = (placement.x?, placement.y?);
    screens.iter().find(|screen| {
        let (sx, sy) = (f32::from(screen.origin.x), f32::from(screen.origin.y));
        x + placement.width > sx && x < sx + f32::from(screen.size.width) && y + VISIBLE_EDGE > sy && y < sy + f32::from(screen.size.height)
    })
}

/// The window bounds to open with, centred when the placement has no position.
pub fn window_bounds(placement: &Placement, cx: &App) -> WindowBounds {
    let normal = match (placement.x, placement.y) {
        (Some(x), Some(y)) => Bounds::new(point(px(x), px(y)), size(px(placement.width), px(placement.height))),
        _ => Bounds::centered(None, size(px(placement.width), px(placement.height)), cx),
    };
    if placement.full_screen {
        WindowBounds::Fullscreen(normal)
    } else if placement.maximized {
        WindowBounds::Maximized(normal)
    } else {
        WindowBounds::Windowed(normal)
    }
}

/// The placement to remember. A maximized window says nothing about its normal bounds, so those
/// are the last ones it had while windowed.
fn placement_of(bounds: WindowBounds, last_normal: Option<Bounds<Pixels>>) -> Placement {
    let (normal, maximized, full_screen) = match bounds {
        WindowBounds::Windowed(bounds) => (bounds, false, false),
        WindowBounds::Maximized(bounds) => (last_normal.unwrap_or(bounds), true, false),
        WindowBounds::Fullscreen(bounds) => (bounds, false, true),
    };
    Placement {
        x: Some(f32::from(normal.origin.x).round()),
        y: Some(f32::from(normal.origin.y).round()),
        width: f32::from(normal.size.width).round(),
        height: f32::from(normal.size.height).round(),
        maximized,
        full_screen,
    }
}

/// Writes the placement once the user stops dragging or resizing.
pub fn watch<V: 'static>(window: WindowHandle<V>, file: PathBuf, opened: &Placement, cx: &mut App) {
    let mut last_normal = match (opened.x, opened.y) {
        (Some(x), Some(y)) => Some(Bounds::new(point(px(x), px(y)), size(px(opened.width), px(opened.height)))),
        _ => None,
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut written: Option<Placement> = None;
        loop {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            let Ok(bounds) = window.update(cx, |_, window, _| window.window_bounds()) else { break };
            if let WindowBounds::Windowed(normal) = bounds {
                last_normal = Some(normal);
            }
            let placement = placement_of(bounds, last_normal);
            if written.as_ref() == Some(&placement) {
                continue;
            }
            if written.is_some() {
                if let Ok(text) = serde_json::to_string(&placement) {
                    let _ = std::fs::write(&file, text);
                }
            }
            written = Some(placement);
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn a_window_off_every_screen_loses_its_position() {
        let placement = Placement { x: Some(5000.0), y: Some(10.0), width: 1000.0, height: 700.0, ..Placement::default() };
        let fitted = fit(placement, &[screen(0.0, 0.0, 1920.0, 1080.0)]);
        assert_eq!((fitted.x, fitted.y), (None, None));
    }

    #[test]
    fn a_window_wider_than_its_screen_is_cut_down() {
        let placement = Placement { x: Some(100.0), y: Some(50.0), width: 3000.0, height: 2000.0, ..Placement::default() };
        let fitted = fit(placement, &[screen(0.0, 0.0, 1440.0, 900.0)]);
        assert_eq!((fitted.width, fitted.height), (1440.0, 900.0));
    }
}
