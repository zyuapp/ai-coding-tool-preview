//! The in-app browser: a real WebKit page per tab drawn over the dock's browser panel, one
//! persistent session shared by every tab, the approval a run needs before it reaches a site, the
//! reads and actions the browser tools drive it with, and copying sign-ins from other browsers.
//!
//! The app mounts it like this:
//! - `BrowserHost` (one per window) performs the reducer's `browser.*`, `focus-browser`,
//!   `find-in-page` and `browser-import.*` effects through `perform`, and emits `BrowserEvent`s;
//!   `workspace_event` turns those into the reducer's `browser.updated` and `find.results`.
//! - `BrowserPanel` is the dock tab. Feed it the tab and approval from state with `set_tab`.
//! - `BrowserHost::remote` and `PanelBridge` give the browser tools their `BrowserBridge`.

pub mod bridge;
mod diagnostics;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod hidden;
pub mod host;
pub mod identity;
pub mod import;
pub mod import_picker;
pub mod panel;
pub mod permissions;
pub mod remote;
pub mod scripts;
mod shots;
pub mod types;

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "linux")]
pub mod linux;

pub use bridge::{BrowserScope, BrowserWorkspace, PanelBridge};
pub use host::{workspace_event, BrowserHost, EffectOutcome};
pub use import_picker::ImportPicker;
pub use panel::BrowserPanel;
pub use remote::BrowserRemote;
pub use types::{BrowserEvent, Inspection, OptionChoice, PageBounds, PageInput, PageRect, PointerKind};

#[cfg(target_os = "macos")]
pub use mac::Pages;

#[cfg(target_os = "linux")]
pub use linux::Pages;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub use unsupported::Pages;

static SESSION_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Where the panel keeps its session on platforms whose web views store it in a folder. Set once at startup.
pub fn set_session_dir(dir: std::path::PathBuf) {
    let _ = SESSION_DIR.set(dir);
}

#[cfg(target_os = "linux")]
fn session_dir() -> std::path::PathBuf {
    SESSION_DIR.get().cloned().unwrap_or_else(|| act_host::user_data::app_data_dir().join(act_core::identity::PROFILE_NAME).join("Browser"))
}

/// The panel's keys. Call once, after `act_ui::init`.
pub fn init(cx: &mut gpui::App) {
    panel::bind_keys(cx);
}

#[cfg(target_os = "macos")]
fn platform_pages(window: &gpui::Window, _cx: &mut gpui::App, executor: gpui::BackgroundExecutor, events: futures::channel::mpsc::UnboundedSender<BrowserEvent>) -> Result<Pages, String> {
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return Err("The browser panel needs a macOS window.".into()) };
    Pages::new(mac::PagesOptions { host_view: appkit.ns_view, executor, events })
}

#[cfg(target_os = "linux")]
fn platform_pages(window: &gpui::Window, cx: &mut gpui::App, executor: gpui::BackgroundExecutor, events: futures::channel::mpsc::UnboundedSender<BrowserEvent>) -> Result<Pages, String> {
    linux::gtk_loop::ensure(cx)?;
    let parent = if linux::gtk_loop::on_x11() { Some(linux::embed::Parent::of(window)?) } else { None };
    Pages::new(linux::PagesOptions { parent, data_dir: session_dir(), scale: f64::from(window.scale_factor()), executor, events })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_pages(_window: &gpui::Window, _cx: &mut gpui::App, _executor: gpui::BackgroundExecutor, _events: futures::channel::mpsc::UnboundedSender<BrowserEvent>) -> Result<Pages, String> {
    Err("The browser panel is not available on this platform yet.".into())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod unsupported {
    //! No WebKit view to draw: every read finds no page.

    use act_core::domain::platform::{BrowserAction, BrowserPermissions};
    use act_host::tools::types::{BrowserReadResult, BrowserShot, BrowserSnapshot};

    use crate::import::CookieDetails;
    use crate::types::{Inspection, PageBounds};

    #[derive(Clone)]
    pub struct Pages;

    const UNAVAILABLE: &str = "The browser panel is not available on this platform yet.";

    impl Pages {
        pub fn configure_permissions(&self, _permissions: &BrowserPermissions) {}
        pub fn open_tab(&self, _tab_id: &str, _url: Option<&str>, _task_id: Option<&str>) -> Result<(), String> {
            Err(UNAVAILABLE.into())
        }
        pub fn navigate(&self, _tab_id: &str, _url: &str, _task_id: Option<&str>) -> Result<(), String> {
            Err(UNAVAILABLE.into())
        }
        pub fn go_history(&self, _tab_id: &str, _delta: i8, _task_id: Option<&str>) -> Result<(), String> {
            Err(UNAVAILABLE.into())
        }
        pub fn reload(&self, _tab_id: &str, _task_id: Option<&str>) -> Result<(), String> {
            Err(UNAVAILABLE.into())
        }
        pub fn stop(&self, _tab_id: &str) {}
        pub fn close_tab(&self, _tab_id: &str) {}
        pub fn show_tab(&self, _tab_id: Option<&str>) {}
        pub fn set_bounds(&self, _bounds: Option<PageBounds>) {}
        pub fn bounds(&self) -> Option<PageBounds> {
            None
        }
        pub fn focus_tab(&self, _tab_id: &str) {}
        pub fn focus_host(&self) {}
        pub fn discard_shots(&self) {}
        pub fn tab_ids(&self) -> Vec<String> {
            Vec::new()
        }
        pub fn painted_by_app(&self) -> bool {
            false
        }
        pub fn frame(&self, _tab_id: &str) -> Option<std::sync::Arc<gpui::RenderImage>> {
            None
        }
        pub fn input(&self, _tab_id: &str, _input: &crate::PageInput) {}
        pub fn key(&self, _tab_id: &str, _down: bool, _keystroke: &gpui::Keystroke) -> bool {
            false
        }
        pub fn choose_option(&self, _tab_id: &str, _choice: Option<usize>) {}
        pub fn cursor(&self, _tab_id: &str) -> gpui::CursorStyle {
            gpui::CursorStyle::Arrow
        }
        pub fn set_shortcuts(&self, _overrides: &act_core::domain::shortcuts::ShortcutOverrides) {}
        pub async fn read_page(&self, _tab_id: &str, _text_limit: f64, _timeout_ms: u64, _task_id: Option<&str>) -> Result<Option<BrowserSnapshot>, String> {
            Ok(None)
        }
        pub async fn inspect_page(&self, _tab_id: &str, _inspection: Inspection, _task_id: Option<&str>) -> Result<Option<BrowserReadResult>, String> {
            Ok(None)
        }
        pub async fn capture_page(&self, _tab_id: &str, _full_page: bool, _timeout_ms: u64, _task_id: Option<&str>) -> Result<Option<BrowserShot>, String> {
            Ok(None)
        }
        pub async fn act(&self, _tab_id: &str, _action: &BrowserAction, _task_id: Option<&str>) -> Result<String, String> {
            Err(UNAVAILABLE.into())
        }
        pub async fn find_in_page(&self, _tab_id: &str, _query: &str, _forward: bool, _find_next: bool) {}
        pub async fn stop_find_in_page(&self, _tab_id: &str) {}
        pub async fn clear_data(&self) {}
        pub async fn replace_cookies(&self, _cookies: &[CookieDetails], _sites: &[String]) -> usize {
            0
        }
    }
}
