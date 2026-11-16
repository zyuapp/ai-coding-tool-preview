//! A WebKit page nobody sees, for drawing what a browser draws best: diagrams and marked screenshots.

use gpui::{App, Window};

pub struct HiddenPage {
    pub view: wry::WebView,
    #[cfg(target_os = "linux")]
    host: gtk::OffscreenWindow,
}

#[cfg(target_os = "linux")]
impl Drop for HiddenPage {
    fn drop(&mut self) {
        use gtk::prelude::*;
        unsafe { self.host.destroy() };
    }
}

/// Builds `builder`'s page with a `width` by `height` viewport, out of sight.
#[cfg(target_os = "macos")]
pub fn open(builder: wry::WebViewBuilder, width: f64, height: f64, window: &mut Window, _cx: &mut App) -> Result<HiddenPage, String> {
    let bounds = wry::Rect { position: wry::dpi::LogicalPosition::new(-30000.0, -30000.0).into(), size: wry::dpi::LogicalSize::new(width, height).into() };
    let view = builder.with_visible(true).with_bounds(bounds).build_as_child(window).map_err(|error| error.to_string())?;
    Ok(HiddenPage { view })
}

/// Builds `builder`'s page with a `width` by `height` viewport, in a window GTK never shows.
#[cfg(target_os = "linux")]
pub fn open(builder: wry::WebViewBuilder, width: f64, height: f64, _window: &mut Window, cx: &mut App) -> Result<HiddenPage, String> {
    use gtk::prelude::*;
    use wry::WebViewBuilderExtUnix;
    crate::linux::gtk_loop::ensure(cx)?;
    let host = gtk::OffscreenWindow::new();
    host.set_default_size(width as i32, height as i32);
    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.add(&container);
    let view = builder.with_visible(true).build_gtk(&container).map_err(|error| error.to_string())?;
    host.show_all();
    Ok(HiddenPage { view, host })
}
