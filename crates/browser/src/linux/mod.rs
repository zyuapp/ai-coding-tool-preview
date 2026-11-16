//! The panel's pages on Linux: WebKitGTK views placed inside GPUI's X11 window, or painted by the
//! app on Wayland, one persistent session shared by every tab.

pub mod embed;
pub mod gtk_loop;
mod keys;
pub mod offscreen;
mod pages;
mod webkit;

pub use pages::{Pages, PagesOptions};
