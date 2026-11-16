//! What the app reaches outside itself for: the iOS simulator panel, dictation, the window's glass,
//! and the context and flash around a captured window.

#[cfg(target_os = "macos")]
mod appkit;
pub mod capture_flash;
pub mod dictation;
pub mod glass;
pub mod native;
pub mod screenshot_context;
pub mod simulator;
pub mod window_screenshot;
