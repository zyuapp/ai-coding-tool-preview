//! The iOS simulator panel: the host that drives devices, the screen stream, and the panel's views.

#[cfg(target_os = "macos")]
pub mod decoder;
pub mod devices;
pub mod effects;
pub mod frames;
pub mod gestures;
pub mod host;
pub mod keys;
#[cfg(target_os = "macos")]
mod magnify;
pub mod panel;
pub mod reads;
pub mod stream;

pub use frames::SimulatorFrame;
pub use host::{SimulatorHost, SimulatorHostOptions};
pub use panel::{CommandHandler, SimulatorPanel};
