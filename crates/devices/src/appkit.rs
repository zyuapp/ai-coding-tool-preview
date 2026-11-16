//! The AppKit objects behind a GPUI window.

use objc2::rc::Retained;
use objc2_app_kit::NSView;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The window's content view, which GPUI draws into.
pub(crate) fn content_view(window: &gpui::Window) -> Option<Retained<NSView>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return None };
    // SAFETY: the handle's view is the live NSView GPUI made for this window.
    unsafe { Retained::retain(appkit.ns_view.as_ptr() as *mut NSView) }
}
