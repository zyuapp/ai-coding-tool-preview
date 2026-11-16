//! The panel's pages on macOS: one WKWebView per tab, all in one persistent website data store, so a
//! login made in any tab is there for every tab and every thread, and is still there after a restart.

pub mod delegate;
mod pages;
pub mod webkit;

pub use pages::{Pages, PagesOptions};
