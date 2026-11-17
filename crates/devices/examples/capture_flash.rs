//! The capture flash and the screenshot context, on their own.
//!
//! `cargo run -p act_devices --example capture_flash -- --flash X,Y,W,H` whitens that part of the screen
//! (CoreGraphics points from the main display's top left) the way a captured window is marked.
//! `... -- --context PID WINDOW_ID [TITLE]` prints what accessibility says about a window.
//! `... -- --frontmost` captures the frontmost window as the desktop hotkey does, without the shutter.

use std::time::Duration;

use act_core::domain::conversation::{AccessibilitySnapshot, ScreenshotPlatform};
use act_devices::capture_flash::{flash_window, WindowFrame, TEARDOWN_MS};
use act_devices::screenshot_context::{capture_screenshot_context, ScreenshotTarget};
use act_devices::window_screenshot::{capture_frontmost_window, WindowShot};
use gpui::{App};

fn print_accessibility(accessibility: &AccessibilitySnapshot) {
    match accessibility {
        AccessibilitySnapshot::Captured { text, truncated } => {
            println!("captured{}:", if *truncated { " (shortened)" } else { "" });
            for line in text.lines().take(40) {
                println!("  {line}");
            }
        }
        AccessibilitySnapshot::Unavailable { reason } => println!("unavailable: {reason}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = act_host::runtime();
    if let Some(at) = args.iter().position(|arg| arg == "--context") {
        let pid = args.get(at + 1).and_then(|value| value.parse().ok()).unwrap_or(0);
        let window_id = args.get(at + 2).and_then(|value| value.parse().ok()).unwrap_or(0);
        let title = args.get(at + 3).cloned().unwrap_or_default();
        // The app readies a driver ahead of the hotkey; here that is a moment before the read.
        act_devices::screenshot_context::prepare_screenshot_context(ScreenshotPlatform::Macos, None);
        std::thread::sleep(Duration::from_secs(4));
        let started = std::time::Instant::now();
        let context = runtime.block_on(capture_screenshot_context(ScreenshotTarget { platform: ScreenshotPlatform::Macos, pid, window_id, app: "app".into(), title }, None));
        println!("read in {:.2}s", started.elapsed().as_secs_f32());
        print_accessibility(&context.accessibility);
        return;
    }
    if args.iter().any(|arg| arg == "--frontmost") {
        match runtime.block_on(capture_frontmost_window(false, None)) {
            WindowShot::Captured { app, title, png, frame, context } => {
                println!("captured {app} — {title:?}, {} bytes of base64 PNG, frame {frame:?}", png.len());
                print_accessibility(&context.accessibility);
            }
            other => println!("{other:?}"),
        }
        return;
    }
    let frame = args
        .iter()
        .position(|arg| arg == "--flash")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| {
            let parts: Vec<f64> = value.split(',').filter_map(|part| part.parse().ok()).collect();
            (parts.len() == 4).then(|| WindowFrame { x: parts[0], y: parts[1], width: parts[2], height: parts[3] })
        })
        .unwrap_or(WindowFrame { x: 200.0, y: 200.0, width: 600.0, height: 400.0 });
    gpui_platform::application().run(move |cx: &mut App| {
        cx.spawn(async move |cx| {
            // A beat first, so a screen recording started with this example sees the flash begin.
            cx.background_executor().timer(Duration::from_millis(1_000)).await;
            let _ = cx.update(|_| flash_window(frame));
            cx.background_executor().timer(Duration::from_millis(TEARDOWN_MS + 600)).await;
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
