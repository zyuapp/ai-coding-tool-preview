//! Grabs the window the person is in without taking the keyboard from it, with what accessibility says
//! about it (window-screenshot.ts, hyprland-window-capture.ts).

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use act_core::domain::conversation::{ScreenshotContext, ScreenshotPlatform};
use act_host::process::{Exec, RunError};
use base64::Engine;
use regex::Regex;
use serde::Deserialize;

use crate::capture_flash::WindowFrame;
use crate::screenshot_context::{capture_screenshot_context, hyprland_accessibility_window_id, ScreenshotTarget};

/// macOS's own screenshot shutter, which is the sound the gesture already means to everyone.
const SHUTTER: &str = "/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/system/Grab.aif";
const RUN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq)]
pub enum WindowShot {
    Captured { app: String, title: String, png: String, frame: WindowFrame, context: ScreenshotContext },
    Denied,
    NoWindow { app: String },
    Unsupported { message: String },
    Failed { message: String },
}

/// The shutter, played beside the capture. `screencapture` makes no sound of its own, so this stays one choice.
pub fn play_shutter() {
    if !cfg!(target_os = "macos") || !Path::new(SHUTTER).exists() {
        return;
    }
    let mut player = std::process::Command::new("/usr/bin/afplay");
    player.arg(SHUTTER).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        player.process_group(0);
    }
    let _ = player.spawn();
}

/// Whether this app may record the screen, without asking.
pub fn screen_capture_allowed() -> bool {
    #[cfg(target_os = "macos")]
    {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGPreflightScreenCaptureAccess() -> bool;
        }
        // SAFETY: a plain query of this process's screen-recording permission.
        unsafe { CGPreflightScreenCaptureAccess() }
    }
    #[cfg(not(target_os = "macos"))]
    true
}

/// The frontmost app's frontmost ordinary window, as CoreGraphics reports it. Reading the window list
/// needs no permission and never activates this app.
const PICK_WINDOW: &str = r#"
ObjC.import("AppKit");
ObjC.bindFunction("CGWindowListCopyWindowInfo", ["id", ["unsigned int", "unsigned int"]]);
var ON_SCREEN = 1, NO_DESKTOP = 16;
var front = $.NSWorkspace.sharedWorkspace.frontmostApplication;
var pid = front.processIdentifier;
var app = ObjC.unwrap(front.localizedName);
var windows = ObjC.deepUnwrap($.CGWindowListCopyWindowInfo(ON_SCREEN | NO_DESKTOP, 0));
var found = windows.find(function (window) {
  return window.kCGWindowOwnerPID === pid
    && window.kCGWindowLayer === 0
    && window.kCGWindowBounds.Width > 60
    && window.kCGWindowBounds.Height > 60;
});
JSON.stringify(found
  ? { app: app, pid: pid, windowId: found.kCGWindowNumber, title: found.kCGWindowName || "", bounds: found.kCGWindowBounds }
  : { app: app, pid: pid, windowId: null });
"#;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CoreGraphicsBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pick {
    app: String,
    pid: i64,
    window_id: Option<i64>,
    title: Option<String>,
    bounds: Option<CoreGraphicsBounds>,
}

fn message(error: &RunError) -> String {
    error.to_string()
}

async fn run(command: &str, args: &[&str]) -> Result<String, RunError> {
    Exec::new(command).args(args).timeout(RUN_TIMEOUT).run().await.map(|output| output.stdout_text())
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<TempDir> {
        let dir = std::env::temp_dir().join(format!("aic-shot-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir)?;
        Ok(TempDir(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn capture_frontmost_mac_window(sound: bool, driver: Option<PathBuf>) -> WindowShot {
    if !screen_capture_allowed() {
        return WindowShot::Denied;
    }
    let outcome = async {
        let picked = run("/usr/bin/osascript", &["-l", "JavaScript", "-e", PICK_WINDOW]).await.map_err(|error| message(&error))?;
        let window: Pick = serde_json::from_str(picked.trim()).map_err(|error| error.to_string())?;
        if window.pid == i64::from(std::process::id()) {
            return Ok(WindowShot::NoWindow { app: act_core::identity::APP_NAME.into() });
        }
        let Some(window_id) = window.window_id else { return Ok(WindowShot::NoWindow { app: window.app }) };
        let title = window.title.clone().unwrap_or_default();
        let target = ScreenshotTarget { platform: ScreenshotPlatform::Macos, pid: window.pid, window_id, app: window.app.clone(), title: title.clone() };
        let context = act_host::runtime().spawn(capture_screenshot_context(target, driver));
        let directory = TempDir::new().map_err(|error| error.to_string())?;
        let file = directory.0.join("window.png");
        let file_text = file.to_string_lossy().into_owned();
        run("/usr/sbin/screencapture", &[&format!("-l{window_id}"), "-x", "-o", &file_text]).await.map_err(|error| message(&error))?;
        if sound {
            play_shutter();
        }
        let png = tokio::fs::read(&file).await.map_err(|error| error.to_string())?;
        if png.is_empty() {
            return Ok(WindowShot::Denied);
        }
        let bounds = window.bounds.unwrap_or(CoreGraphicsBounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 });
        let frame = WindowFrame { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height };
        let context = context.await.map_err(|error| error.to_string())?;
        Ok(WindowShot::Captured { app: window.app, title, png: base64::engine::general_purpose::STANDARD.encode(png), frame, context })
    }
    .await;
    outcome.unwrap_or_else(|message| WindowShot::Failed { message })
}

static X11_ACTIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:^|\s)(0x[\da-f]+)(?:\s|$)").unwrap());
static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""(?:\\.|[^"\\])*""#).unwrap());
static X11_PID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"=\s*(\d+)").unwrap());
static X11_WIDTH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*Width:\s*(\d+)\s*$").unwrap());
static X11_HEIGHT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*Height:\s*(\d+)\s*$").unwrap());

pub fn x11_active_window_id(output: &str) -> Option<i64> {
    let value = X11_ACTIVE.captures(output)?.get(1)?.as_str();
    let id = i64::from_str_radix(&value[2..], 16).ok()?;
    (id > 0 && id < (1i64 << 53)).then_some(id)
}

fn quoted_values(value: &str) -> Vec<String> {
    QUOTED.find_iter(value).filter_map(|found| serde_json::from_str::<String>(found.as_str()).ok()).collect()
}

/// A window's pid, title and app from `xprop`.
#[derive(Clone, Debug, PartialEq)]
pub struct X11Window {
    pub id: i64,
    pub pid: Option<i64>,
    pub app: String,
    pub title: String,
}

pub fn x11_window_properties(id: i64, output: &str) -> X11Window {
    let lines: Vec<&str> = output.split('\n').collect();
    let name = lines.iter().find(|line| line.starts_with("_NET_WM_NAME")).or_else(|| lines.iter().find(|line| line.starts_with("WM_NAME")));
    let class = lines.iter().find(|line| line.starts_with("WM_CLASS"));
    let pid = lines.iter().find(|line| line.starts_with("_NET_WM_PID")).and_then(|line| X11_PID.captures(line)).and_then(|found| found[1].parse::<i64>().ok()).filter(|pid| *pid > 0);
    let title = name.and_then(|line| quoted_values(line).into_iter().next()).unwrap_or_default();
    let classes = class.map(|line| quoted_values(line)).unwrap_or_default();
    let app = classes.last().filter(|text| !text.is_empty()).or_else(|| classes.first().filter(|text| !text.is_empty())).cloned().or_else(|| (!title.is_empty()).then(|| title.clone())).unwrap_or_else(|| "Unknown app".into());
    X11Window { id, pid, app, title }
}

/// `xwininfo` reports device pixels.
pub fn x11_window_size(output: &str) -> Option<(u32, u32)> {
    let width = X11_WIDTH.captures(output)?[1].parse::<u32>().ok()?;
    let height = X11_HEIGHT.captures(output)?[1].parse::<u32>().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

fn x11_failure(error: &RunError) -> String {
    if matches!(error, RunError::NotFound { .. }) {
        return "X11 window capture needs xprop, xwininfo and ImageMagick's import. Install those tools (x11-utils and imagemagick on Debian or Ubuntu), then try again.".into();
    }
    format!("Could not capture the active X11 window: {error}")
}

async fn capture_frontmost_x11_window(driver: Option<PathBuf>) -> WindowShot {
    let outcome: Result<WindowShot, RunError> = async {
        let Some(id) = x11_active_window_id(&run("xprop", &["-root", "_NET_ACTIVE_WINDOW"]).await?) else { return Ok(WindowShot::NoWindow { app: "the desktop".into() }) };
        let target = format!("0x{id:x}");
        let property_args = ["-id", target.as_str(), "_NET_WM_PID", "_NET_WM_NAME", "WM_NAME", "WM_CLASS"];
        let geometry_args = ["-id", target.as_str()];
        let (properties, geometry) = futures::join!(run("xprop", &property_args), run("xwininfo", &geometry_args));
        let window = x11_window_properties(id, &properties?);
        if x11_window_size(&geometry?).is_none() {
            return Ok(WindowShot::Failed { message: "Could not capture the active X11 window: xwininfo did not report a usable active-window size.".into() });
        }
        if window.pid == Some(i64::from(std::process::id())) {
            return Ok(WindowShot::NoWindow { app: act_core::identity::APP_NAME.into() });
        }
        let context = act_host::runtime().spawn(capture_screenshot_context(
            ScreenshotTarget { platform: ScreenshotPlatform::LinuxX11, pid: window.pid.unwrap_or(0), window_id: id, app: window.app.clone(), title: window.title.clone() },
            driver,
        ));
        let png = Exec::new("import").args(["-silent", "-window", target.as_str(), "png:-"]).timeout(RUN_TIMEOUT).max_buffer(256 * 1024 * 1024).run().await?.stdout;
        if png.is_empty() {
            return Ok(WindowShot::Failed { message: "The desktop capture returned an empty image for the active window.".into() });
        }
        let context = match context.await {
            Ok(context) => context,
            Err(error) => return Ok(WindowShot::Failed { message: error.to_string() }),
        };
        // The capture flash stays macOS-only, so Linux needs no compositor geometry.
        Ok(WindowShot::Captured {
            app: window.app,
            title: window.title,
            png: base64::engine::general_purpose::STANDARD.encode(png),
            frame: WindowFrame { x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
            context,
        })
    }
    .await;
    outcome.unwrap_or_else(|error| WindowShot::Failed { message: x11_failure(&error) })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HyprlandWindow {
    stable_id: String,
    address: Option<String>,
    pid: i64,
    class: String,
    title: String,
    at: (f64, f64),
    size: (f64, f64),
}

/// Captures the selected toplevel, never a screen crop that could include an overlapping window.
async fn capture_frontmost_hyprland_window(driver: Option<PathBuf>) -> WindowShot {
    let outcome: Result<WindowShot, RunError> = async {
        let active: serde_json::Value = match serde_json::from_str(&run("hyprctl", &["-j", "activewindow"]).await?) {
            Ok(serde_json::Value::Object(active)) => serde_json::Value::Object(active),
            _ => return Ok(WindowShot::Failed { message: "Could not capture the active Hyprland window: Hyprland returned invalid window information.".into() }),
        };
        let object = active.as_object().unwrap();
        if object.is_empty() || object.get("mapped") == Some(&serde_json::Value::Bool(false)) || object.get("hidden") == Some(&serde_json::Value::Bool(true)) {
            return Ok(WindowShot::NoWindow { app: "the desktop".into() });
        }
        if object.get("pid").and_then(serde_json::Value::as_i64) == Some(i64::from(std::process::id())) {
            return Ok(WindowShot::NoWindow { app: act_core::identity::APP_NAME.into() });
        }
        let parsed: Option<HyprlandWindow> = serde_json::from_value(active.clone()).ok().filter(|window: &HyprlandWindow| {
            let stable = !window.stable_id.is_empty() && window.stable_id.len() <= 128 && window.stable_id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
            stable && window.pid > 0 && window.size.0 > 0.0 && window.size.1 > 0.0 && window.at.0.is_finite() && window.at.1.is_finite()
        });
        let Some(window) = parsed else {
            return Ok(WindowShot::Failed { message: "Hyprland did not provide a usable window ID or size. Update Hyprland and grim, then try again.".into() });
        };
        let app = if window.class.is_empty() { "Unknown app".to_string() } else { window.class.clone() };
        let context = act_host::runtime().spawn(capture_screenshot_context(
            ScreenshotTarget {
                platform: ScreenshotPlatform::LinuxHyprland,
                pid: window.pid,
                window_id: hyprland_accessibility_window_id(window.address.as_deref()).unwrap_or(0),
                app: app.clone(),
                title: window.title.clone(),
            },
            driver,
        ));
        let directory = TempDir::new().map_err(RunError::Io)?;
        let file = directory.0.join("window.png");
        run("grim", &["-T", &window.stable_id, "-t", "png", &file.to_string_lossy()]).await?;
        let png = tokio::fs::read(&file).await.map_err(RunError::Io)?;
        if png.len() < 24 || png[..8] != [137, 80, 78, 71, 13, 10, 26, 10] {
            return Ok(WindowShot::Failed { message: "Hyprland returned an empty or invalid window image. Try capturing the window again.".into() });
        }
        let context = match context.await {
            Ok(context) => context,
            Err(error) => return Ok(WindowShot::Failed { message: error.to_string() }),
        };
        Ok(WindowShot::Captured {
            app,
            title: window.title,
            png: base64::engine::general_purpose::STANDARD.encode(png),
            frame: WindowFrame { x: window.at.0, y: window.at.1, width: window.size.0, height: window.size.1 },
            context,
        })
    }
    .await;
    outcome.unwrap_or_else(|error| match error {
        RunError::NotFound { .. } => WindowShot::Failed { message: "Hyprland window capture needs hyprctl and grim. Install or update those tools, then try again.".into() },
        other => WindowShot::Failed { message: format!("Could not capture the active Hyprland window: {other}") },
    })
}

/// Which Linux capture this session supports.
fn linux_backend() -> Result<&'static str, String> {
    let has = |name: &str| std::env::var(name).ok().is_some_and(|value| !value.trim().is_empty());
    if has("WAYLAND_DISPLAY") && has("HYPRLAND_INSTANCE_SIGNATURE") {
        return Ok("hyprland");
    }
    let wayland_session = std::env::var("XDG_SESSION_TYPE").map(|value| value.trim().eq_ignore_ascii_case("wayland")).unwrap_or(false) || has("WAYLAND_DISPLAY");
    if has("DISPLAY") && !wayland_session {
        return Ok("x11");
    }
    if has("DISPLAY") || has("WAYLAND_DISPLAY") {
        return Err("Window capture is not available on this Wayland desktop yet. Supported Linux sessions are Hyprland and X11.".into());
    }
    Err("Window capture needs a graphical Linux session, but neither DISPLAY nor WAYLAND_DISPLAY is available.".into())
}

/// Grabs the frontmost window, with what accessibility says about it near the moment of capture.
pub async fn capture_frontmost_window(sound: bool, driver: Option<PathBuf>) -> WindowShot {
    match std::env::consts::OS {
        "macos" => capture_frontmost_mac_window(sound, driver).await,
        "linux" => match linux_backend() {
            Ok("hyprland") => capture_frontmost_hyprland_window(driver).await,
            Ok(_) => capture_frontmost_x11_window(driver).await,
            Err(message) => WindowShot::Unsupported { message },
        },
        other => WindowShot::Unsupported { message: format!("Window capture is not supported on {other}.") },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xprop_answers_read_as_a_window() {
        assert_eq!(x11_active_window_id("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x3a00007"), Some(0x3a00007));
        assert_eq!(x11_active_window_id("_NET_ACTIVE_WINDOW: not found."), None);
        let window = x11_window_properties(5, "_NET_WM_PID(CARDINAL) = 4242\n_NET_WM_NAME(UTF8_STRING) = \"Notes \\\"draft\\\"\"\nWM_CLASS(STRING) = \"gedit\", \"Gedit\"");
        assert_eq!(window, X11Window { id: 5, pid: Some(4242), app: "Gedit".into(), title: "Notes \"draft\"".into() });
        assert_eq!(x11_window_size("  Width: 800\n  Height: 600\n"), Some((800, 600)));
    }
}
