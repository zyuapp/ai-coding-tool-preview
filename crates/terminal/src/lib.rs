//! The integrated terminal: shells on pseudo-terminals, their emulated screens, and the GPUI view
//! the dock's terminal panel draws them with.

pub mod contract;
mod element;
pub mod host;
pub mod keys;
pub mod mouse;
mod output_buffer;
pub mod screen;
pub mod search;
pub mod snapshot;
pub mod theme;
pub mod view;

pub use contract::*;
pub use host::TerminalHost;
pub use screen::{Screen, ScreenEvent};
pub use search::FindResults;
pub use theme::{TerminalFont, TerminalTheme};
pub use view::{init, RemoteRead, TerminalView, TerminalViewEvent};
