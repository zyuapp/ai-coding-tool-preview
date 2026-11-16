//! Asking a WebKitGTK view for things: running a script, drawing the page, and its session's cookies.

use futures::channel::oneshot;
use gtk::glib;
use javascriptcore::ValueExt;
use gtk::prelude::*;
use webkit2gtk::{CookieManagerExt, FindControllerExt, FindOptions, SnapshotOptions, SnapshotRegion, WebViewExt, WebsiteDataManager, WebsiteDataManagerExt, WebsiteDataManagerExtManual, WebsiteDataTypes};

use crate::import::cookies::SameSite;
use crate::import::{cookie_on_site, CookieDetails};

/// The script world the host's own scripts run in, where the page cannot reach their globals.
pub const HOST_WORLD: &str = "aicodingtool-host";

/// What a script threw, as the page wrote it, without the `Error:` WebKit puts before it.
fn script_error(error: &glib::Error) -> String {
    let message = error.message().to_string();
    let message = message.rsplit_once(": Error: ").map(|(_, rest)| rest.to_string()).unwrap_or(message);
    message.strip_prefix("Error: ").unwrap_or(&message).to_string()
}

/// Runs an expression in the host's own world and answers what it settles to as JSON.
pub async fn evaluate(webview: &webkit2gtk::WebView, expression: &str) -> Result<serde_json::Value, String> {
    let body = format!("const value = await ({expression});\nreturn value === undefined ? null : JSON.stringify(value);");
    let (sender, receiver) = oneshot::channel();
    webview.call_async_javascript_function(&body, None, Some(HOST_WORLD), None, None::<&gtk::gio::Cancellable>, move |result| {
        let _ = sender.send(match result {
            Ok(value) if value.is_null() || value.is_undefined() => Ok(None),
            Ok(value) => Ok(Some(value.to_str().to_string())),
            Err(error) => Err(script_error(&error)),
        });
    });
    match receiver.await.map_err(|_| "The page went away while the script ran.".to_string())?? {
        Some(json) => serde_json::from_str(&json).map_err(|error| error.to_string()),
        None => Ok(serde_json::Value::Null),
    }
}

/// The page as an image: what the view shows, or the whole document, at most `max_height` pixels tall.
pub async fn snapshot(webview: &webkit2gtk::WebView, full_page: bool, max_height: i32) -> Result<Vec<u8>, String> {
    let (sender, receiver) = oneshot::channel();
    let region = if full_page { SnapshotRegion::FullDocument } else { SnapshotRegion::Visible };
    webview.snapshot(region, SnapshotOptions::NONE, None::<&gtk::gio::Cancellable>, move |result| {
        let _ = sender.send(result.map_err(|error| error.message().to_string()).and_then(|surface| encode(&surface, max_height)));
    });
    receiver.await.map_err(|_| "The page went away while it was being drawn.".to_string())?
}

fn encode(surface: &cairo::Surface, max_height: i32) -> Result<Vec<u8>, String> {
    let image = cairo::ImageSurface::try_from(surface.clone()).map_err(|_| "The page drew nothing.".to_string())?;
    let (width, height) = (image.width().max(1), image.height().max(1));
    let image = if height > max_height {
        let cut = cairo::ImageSurface::create(cairo::Format::ARgb32, width, max_height).map_err(|error| error.to_string())?;
        let context = cairo::Context::new(&cut).map_err(|error| error.to_string())?;
        context.set_source_surface(&image, 0.0, 0.0).map_err(|error| error.to_string())?;
        context.paint().map_err(|error| error.to_string())?;
        drop(context);
        cut
    } else {
        image
    };
    let mut bytes = Vec::new();
    image.write_to_png(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

/// Selects the next match of `query` in the page, wrapping at either end.
pub async fn find(webview: &webkit2gtk::WebView, query: &str, backwards: bool) -> bool {
    let Some(finder) = webview.find_controller() else { return false };
    let (sender, receiver) = oneshot::channel::<bool>();
    let sender = std::rc::Rc::new(std::cell::RefCell::new(Some(sender)));
    let found_sender = sender.clone();
    let found = finder.connect_found_text(move |_, _| {
        if let Some(sender) = found_sender.borrow_mut().take() {
            let _ = sender.send(true);
        }
    });
    let failed_sender = sender.clone();
    let failed = finder.connect_failed_to_find_text(move |_| {
        if let Some(sender) = failed_sender.borrow_mut().take() {
            let _ = sender.send(false);
        }
    });
    let same = finder.search_text().is_some_and(|text| text.as_str() == query);
    let mut options = FindOptions::CASE_INSENSITIVE | FindOptions::WRAP_AROUND;
    if backwards {
        options |= FindOptions::BACKWARDS;
    }
    if same {
        if backwards {
            finder.search_previous();
        } else {
            finder.search_next();
        }
    } else {
        finder.search(query, options.bits(), u32::MAX);
    }
    let found_match = receiver.await.unwrap_or(false);
    finder.disconnect(found);
    finder.disconnect(failed);
    found_match
}

pub fn stop_find(webview: &webkit2gtk::WebView) {
    if let Some(finder) = webview.find_controller() {
        finder.search_finish();
    }
}

/// Removes these sites' cookies from the session.
pub async fn delete_site_cookies(manager: &WebsiteDataManager, sites: &[String]) {
    let (sender, receiver) = oneshot::channel();
    manager.fetch(WebsiteDataTypes::COOKIES, None::<&gtk::gio::Cancellable>, move |result| {
        let _ = sender.send(result.unwrap_or_default());
    });
    let records = receiver.await.unwrap_or_default();
    let matching: Vec<&webkit2gtk::WebsiteData> = records.iter().filter(|record| record.name().is_some_and(|name| sites.iter().any(|site| cookie_on_site(name.as_str(), site)))).collect();
    if matching.is_empty() {
        return;
    }
    let (sender, receiver) = oneshot::channel();
    manager.remove(WebsiteDataTypes::COOKIES, &matching, None::<&gtk::gio::Cancellable>, move |_| {
        let _ = sender.send(());
    });
    let _ = receiver.await;
}

fn soup_cookie(details: &CookieDetails) -> soup::Cookie {
    let domain = match &details.domain {
        Some(domain) if domain.starts_with('.') => domain.clone(),
        Some(domain) => format!(".{domain}"),
        None => details.host.clone(),
    };
    let mut cookie = soup::Cookie::new(&details.name, &details.value, &domain, &details.path, -1);
    cookie.set_secure(details.secure);
    cookie.set_http_only(details.http_only);
    cookie.set_same_site_policy(match details.same_site {
        SameSite::Lax => soup::SameSitePolicy::Lax,
        SameSite::Strict => soup::SameSitePolicy::Strict,
        SameSite::NoRestriction | SameSite::Unspecified => soup::SameSitePolicy::None,
    });
    if let Some(expires) = details.expires.and_then(|expires| glib::DateTime::from_unix_utc(expires).ok()) {
        cookie.set_expires(&expires);
    }
    cookie
}

/// Sets one cookie, answering whether the session took it.
pub async fn set_cookie(manager: &WebsiteDataManager, details: &CookieDetails) -> bool {
    let Some(cookies) = manager.cookie_manager() else { return false };
    let mut cookie = soup_cookie(details);
    let (sender, receiver) = oneshot::channel();
    cookies.add_cookie(&mut cookie, None::<&gtk::gio::Cancellable>, move |result| {
        let _ = sender.send(result.is_ok());
    });
    receiver.await.unwrap_or(false)
}

/// Removes every cookie, cache and storage the session holds.
pub async fn clear(manager: &WebsiteDataManager) {
    let (sender, receiver) = oneshot::channel();
    manager.clear(WebsiteDataTypes::ALL, glib::TimeSpan::from_seconds(0), None::<&gtk::gio::Cancellable>, move |_| {
        let _ = sender.send(());
    });
    let _ = receiver.await;
}

/// Whether a navigation loads the top document rather than one of its frames. WebKitGTK says so
/// from 2.56 on; earlier versions answer None.
pub fn targets_main_frame(action: &mut webkit2gtk::NavigationAction) -> Option<bool> {
    use std::ffi::c_void;
    use std::sync::OnceLock;
    use gtk::glib::translate::ToGlibPtrMut;
    type TargetFrame = unsafe extern "C" fn(*mut webkit2gtk::ffi::WebKitNavigationAction) -> *mut c_void;
    type IsMainFrame = unsafe extern "C" fn(*mut c_void) -> glib::ffi::gboolean;
    static API: OnceLock<Option<(TargetFrame, IsMainFrame)>> = OnceLock::new();
    let (target_frame, is_main_frame) = (*API.get_or_init(|| unsafe {
        let target = libc::dlsym(libc::RTLD_DEFAULT, c"webkit_navigation_action_get_target_frame_info".as_ptr());
        let is_main = libc::dlsym(libc::RTLD_DEFAULT, c"webkit_frame_info_is_main_frame".as_ptr());
        (!target.is_null() && !is_main.is_null()).then(|| (std::mem::transmute::<*mut c_void, TargetFrame>(target), std::mem::transmute::<*mut c_void, IsMainFrame>(is_main)))
    }))?;
    let info = unsafe { target_frame(action.to_glib_none_mut().0) };
    // No target frame: the navigation opens a window.
    (!info.is_null()).then(|| unsafe { is_main_frame(info) } != 0)
}
