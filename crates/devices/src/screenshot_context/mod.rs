//! What a captured window said through accessibility near the moment it was captured
//! (screenshot-context.ts). The native walk runs in a disposable driver process with a deadline, so a
//! hung app or a native failure cannot hold the app up, and at most one walk runs at a time.

pub mod snapshot;
pub mod tree;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_core::domain::conversation::{AccessibilitySnapshot, ScreenshotContext, ScreenshotPlatform};
use serde_json::{json, Value};

pub use snapshot::{accessibility_from_snapshot, hyprland_accessibility_window_id, ContextDriver, ScreenshotTarget, ToolAnswer};

pub const SCREENSHOT_CONTEXT_TIMEOUT: Duration = Duration::from_millis(1_500);

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The computer-use driver: `ACT_CUA_DRIVER`, else the one shipped beside the app, else the one on the PATH.
pub fn default_driver() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("ACT_CUA_DRIVER") {
        return Some(PathBuf::from(named));
    }
    let shipped = crate::native::helper("cua-driver", None);
    if shipped.is_file() {
        return Some(shipped);
    }
    act_platform::search_path::folders().into_iter().map(|dir| dir.join("cua-driver")).find(|candidate| candidate.is_file())
}

/// Whether this app may read other apps through accessibility, without asking.
pub fn accessibility_trusted() -> bool {
    #[cfg(target_os = "macos")]
    {
        #[link(name = "ApplicationServices", kind = "framework")]
        extern "C" {
            fn AXIsProcessTrusted() -> u8;
        }
        // SAFETY: a plain query of this process's accessibility trust.
        unsafe { AXIsProcessTrusted() != 0 }
    }
    #[cfg(not(target_os = "macos"))]
    true
}

fn js_slice(text: &str, length: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().take(length).collect();
    String::from_utf16_lossy(&units)
}

/// The driver's own MCP server over stdio, run embedded in a process of its own.
struct McpDriver {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl McpDriver {
    fn start(stdin: ChildStdin, stdout: ChildStdout) -> Result<McpDriver, String> {
        let mut driver = McpDriver { stdin, stdout: BufReader::new(stdout), next: 1 };
        driver.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "aicodingtool-screenshot-context", "version": "1" } }))?;
        driver.notify("notifications/initialized")?;
        Ok(driver)
    }

    fn notify(&mut self, method: &str) -> Result<(), String> {
        writeln!(self.stdin, "{}", json!({ "jsonrpc": "2.0", "method": method })).map_err(|error| error.to_string())?;
        self.stdin.flush().map_err(|error| error.to_string())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        writeln!(self.stdin, "{}", json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).map_err(|error| error.to_string())?;
        self.stdin.flush().map_err(|error| error.to_string())?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line).map_err(|error| error.to_string())? == 0 {
                return Err("The driver stopped.".into());
            }
            let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(error.to_string());
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

impl ContextDriver for McpDriver {
    fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<ToolAnswer, String> {
        let result = self.request("tools/call", json!({ "name": name, "arguments": arguments }))?;
        Ok(ToolAnswer {
            is_error: result.get("isError") == Some(&Value::Bool(true)),
            structured_json: result.get("structuredContent").map(Value::to_string),
        })
    }
}

/// Reads the captured window's accessibility text within the deadline. Never fails: what could not be
/// read says why instead.
pub async fn capture_screenshot_context(target: ScreenshotTarget, driver: Option<PathBuf>) -> ScreenshotContext {
    let base = |accessibility: AccessibilitySnapshot| ScreenshotContext {
        version: 1,
        platform: target.platform,
        app: js_slice(&target.app, snapshot::MAX_SCREENSHOT_CONTEXT_NAME),
        title: js_slice(&target.title, snapshot::MAX_SCREENSHOT_CONTEXT_NAME),
        captured_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as i64).unwrap_or(0),
        accessibility,
    };
    let unavailable = |reason: &str| AccessibilitySnapshot::Unavailable { reason: reason.into() };
    let safe = |value: i64| value > 0 && value < (1i64 << 53);
    if !safe(target.pid) || !safe(target.window_id) {
        return base(unavailable("window"));
    }
    if target.platform == ScreenshotPlatform::Macos && !accessibility_trusted() {
        return base(unavailable("permission"));
    }
    let Some(driver) = driver.or_else(default_driver) else { return base(unavailable("failed")) };
    if ACTIVE.swap(true, Ordering::AcqRel) {
        return base(unavailable("busy"));
    }
    let accessibility = read(&target, driver).await;
    ACTIVE.store(false, Ordering::Release);
    base(accessibility)
}

/// A driver process started and introduced ahead of a capture: the driver takes seconds to start,
/// which the walk's own deadline leaves no room for. Each capture uses one up and readies the next.
struct Warm {
    key: (PathBuf, bool),
    child: Child,
    driver: McpDriver,
}

static WARM: Mutex<Option<Warm>> = Mutex::new(None);
static WARMING: AtomicBool = AtomicBool::new(false);

fn spawn_driver(driver: &PathBuf, hyprland: bool) -> Option<(Child, ChildStdin, ChildStdout)> {
    let mut command = act_platform::process::std_command(driver);
    command.arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // The person's own compositor is read, even with computer-use tools off or XWayland present.
    command.env_remove("CUA_WAYLAND_NEST");
    if hyprland {
        command.env("CUA_DRIVER_RS_ENABLE_WAYLAND", "1");
    }
    let mut child = command.spawn().ok()?;
    match (child.stdin.take(), child.stdout.take()) {
        (Some(stdin), Some(stdout)) => Some((child, stdin, stdout)),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Readies a driver for the next capture in the background, so it starts within its deadline.
pub fn prepare_screenshot_context(platform: ScreenshotPlatform, driver: Option<PathBuf>) {
    let Some(driver) = driver.or_else(default_driver) else { return };
    let key = (driver, platform == ScreenshotPlatform::LinuxHyprland);
    if WARM.lock().unwrap().as_ref().is_some_and(|warm| warm.key == key) || WARMING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        if let Some((mut child, stdin, stdout)) = spawn_driver(&key.0, key.1) {
            match McpDriver::start(stdin, stdout) {
                Ok(driver) => {
                    if let Some(mut stale) = WARM.lock().unwrap().replace(Warm { key, child, driver }) {
                        let _ = stale.child.kill();
                        let _ = stale.child.wait();
                    }
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        WARMING.store(false, Ordering::Release);
    });
}

async fn read(target: &ScreenshotTarget, driver: PathBuf) -> AccessibilitySnapshot {
    let hyprland = target.platform == ScreenshotPlatform::LinuxHyprland;
    let key = (driver.clone(), hyprland);
    let warm = WARM.lock().unwrap().take_if(|warm| warm.key == key);
    let child: Arc<Mutex<Option<Child>>> = Arc::default();
    let partial: Arc<Mutex<Option<AccessibilitySnapshot>>> = Arc::default();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let reading = target.clone();
    let keeping = partial.clone();
    let owning = child.clone();
    std::thread::spawn(move || {
        let driver = match warm {
            Some(warm) => {
                *owning.lock().unwrap() = Some(warm.child);
                Some(warm.driver)
            }
            None => spawn_driver(&key.0, key.1).and_then(|(spawned, stdin, stdout)| {
                *owning.lock().unwrap() = Some(spawned);
                McpDriver::start(stdin, stdout).ok()
            }),
        };
        let Some(mut driver) = driver else {
            let _ = sender.send(AccessibilitySnapshot::Unavailable { reason: "failed".into() });
            return;
        };
        let snapshot = snapshot::snapshot_window_accessibility(&mut driver, &reading, &mut |first| {
            if let AccessibilitySnapshot::Captured { text, .. } = first {
                *keeping.lock().unwrap() = Some(AccessibilitySnapshot::Captured { text: text.clone(), truncated: true });
            }
        });
        let _ = sender.send(snapshot);
    });
    let outcome = tokio::time::timeout(SCREENSHOT_CONTEXT_TIMEOUT, receiver).await;
    if let Some(mut child) = child.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    prepare_screenshot_context(target.platform, Some(driver));
    let partial = partial.lock().unwrap().take();
    match outcome {
        Ok(Ok(snapshot)) => match snapshot {
            captured @ AccessibilitySnapshot::Captured { .. } => captured,
            other => partial.unwrap_or(other),
        },
        Ok(Err(_)) => partial.unwrap_or(AccessibilitySnapshot::Unavailable { reason: "failed".into() }),
        Err(_) => partial.unwrap_or(AccessibilitySnapshot::Unavailable { reason: "timeout".into() }),
    }
}
