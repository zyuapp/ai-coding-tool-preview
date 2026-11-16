//! Asking a WKWebView for things: running a script, drawing the page, and its session's cookies.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use futures::channel::oneshot;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSCalibratedRGBColorSpace, NSGraphicsContext, NSImage};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSData, NSDate, NSDictionary, NSError, NSHTTPCookie, NSHTTPCookieDomain, NSHTTPCookieExpires, NSHTTPCookieName,
    NSHTTPCookieOriginURL, NSHTTPCookiePath, NSHTTPCookiePropertyKey, NSHTTPCookieSecure, NSHTTPCookieValue, NSMutableDictionary, NSRect, NSSize, NSString, NSURL,
    NSURLRequest,
};
use objc2_web_kit::{WKContentWorld, WKFindConfiguration, WKFindResult, WKPDFConfiguration, WKSnapshotConfiguration, WKWebView, WKWebsiteDataStore};

use crate::import::cookies::SameSite;
use crate::import::CookieDetails;

/// A one-shot answer from a completion block, which WebKit types as callable many times.
fn once<T: 'static>() -> (RefCell<Option<oneshot::Sender<T>>>, oneshot::Receiver<T>) {
    let (sender, receiver) = oneshot::channel();
    (RefCell::new(Some(sender)), receiver)
}

fn answer<T>(sender: &RefCell<Option<oneshot::Sender<T>>>, value: T) {
    if let Some(sender) = sender.borrow_mut().take() {
        let _ = sender.send(value);
    }
}

/// What a script threw, as the page wrote it, without the `Error:` WebKit puts before it.
fn script_error(error: &NSError) -> String {
    let info = error.userInfo();
    let message = info
        .objectForKey(ns_string!("WKJavaScriptExceptionMessage"))
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|message| message.to_string())
        .unwrap_or_else(|| error.localizedDescription().to_string());
    message.strip_prefix("Error: ").unwrap_or(&message).to_string()
}

/// Runs an expression in the host's own script world, where the page cannot reach its globals,
/// and answers what it settles to as JSON.
pub async fn evaluate(webview: &WKWebView, expression: &str) -> Result<serde_json::Value, String> {
    let mtm = MainThreadMarker::new().ok_or("The browser runs on the main thread.")?;
    let body = NSString::from_str(&format!("const value = await ({expression});\nreturn value === undefined ? null : JSON.stringify(value);"));
    let (sender, receiver) = once::<Result<Option<String>, String>>();
    let block = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
        if let Some(error) = unsafe { error.as_ref() } {
            answer(&sender, Err(script_error(error)));
            return;
        }
        let text = unsafe { result.as_ref() }.and_then(|result| result.downcast_ref::<NSString>()).map(|text| text.to_string());
        answer(&sender, Ok(text));
    });
    unsafe {
        let world = WKContentWorld::defaultClientWorld(mtm);
        webview.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(&body, None, None, &world, Some(&block));
    }
    let text = receiver.await.map_err(|_| "The page went away while it was being read.".to_string())??;
    match text {
        Some(text) => serde_json::from_str(&text).map_err(|error| error.to_string()),
        None => Ok(serde_json::Value::Null),
    }
}

pub fn load(webview: &WKWebView, url: &str) -> Result<(), String> {
    let address = NSURL::URLWithString(&NSString::from_str(url)).ok_or_else(|| format!("Cannot load {url}"))?;
    let request = NSURLRequest::requestWithURL(&address);
    unsafe { webview.loadRequest(&request) };
    Ok(())
}

pub fn current_url(webview: &WKWebView) -> String {
    unsafe { webview.URL() }.and_then(|url| url.absoluteString()).map(|url| url.to_string()).unwrap_or_default()
}

pub fn current_title(webview: &WKWebView) -> String {
    unsafe { webview.title() }.map(|title| title.to_string()).unwrap_or_default()
}

/// Draws an image into a bitmap of the given size in pixels and encodes it as PNG.
fn rasterize(image: &NSImage, points: NSSize, scale: f64) -> Result<Vec<u8>, String> {
    let width = (points.width * scale).round().max(1.0) as isize;
    let height = (points.height * scale).round().max(1.0) as isize;
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width,
            height,
            8,
            4,
            true,
            false,
            NSCalibratedRGBColorSpace,
            0,
            0,
        )
    }
    .ok_or("Could not make room for the picture.")?;
    rep.setSize(points);
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).ok_or("Could not draw the picture.")?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    image.drawInRect(NSRect::new(CGPoint::new(0.0, 0.0), points));
    context.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }.ok_or("Could not encode the picture.")?;
    Ok(data.to_vec())
}

/// The part of the page the view shows, at the screen's density.
pub async fn snapshot(webview: &WKWebView, scale: f64) -> Result<Vec<u8>, String> {
    let mtm = MainThreadMarker::new().ok_or("The browser runs on the main thread.")?;
    let configuration = unsafe { WKSnapshotConfiguration::new(mtm) };
    unsafe { configuration.setAfterScreenUpdates(false) };
    let (sender, receiver) = once::<Result<Retained<NSImage>, String>>();
    let block = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
        let result = match (unsafe { Retained::retain(image) }, unsafe { error.as_ref() }) {
            (Some(image), _) => Ok(image),
            (None, Some(error)) => Err(error.localizedDescription().to_string()),
            (None, None) => Err("The page drew nothing.".into()),
        };
        answer(&sender, result);
    });
    unsafe { webview.takeSnapshotWithConfiguration_completionHandler(Some(&configuration), &block) };
    let image = receiver.await.map_err(|_| "The page went away while it was being drawn.".to_string())??;
    rasterize(&image, image.size(), scale)
}

/// The whole page, drawn through a PDF of it so nothing on screen moves, cut off at `max` points.
pub async fn full_page(webview: &WKWebView, size: CGSize, scale: f64) -> Result<Vec<u8>, String> {
    let mtm = MainThreadMarker::new().ok_or("The browser runs on the main thread.")?;
    let configuration = unsafe { WKPDFConfiguration::new(mtm) };
    unsafe { configuration.setRect(CGRect::new(CGPoint::new(0.0, 0.0), size)) };
    let (sender, receiver) = once::<Result<Retained<NSData>, String>>();
    let block = RcBlock::new(move |data: *mut NSData, error: *mut NSError| {
        let result = match (unsafe { Retained::retain(data) }, unsafe { error.as_ref() }) {
            (Some(data), _) => Ok(data),
            (None, Some(error)) => Err(error.localizedDescription().to_string()),
            (None, None) => Err("The page drew nothing.".into()),
        };
        answer(&sender, result);
    });
    unsafe { webview.createPDFWithConfiguration_completionHandler(Some(&configuration), &block) };
    let data = receiver.await.map_err(|_| "The page went away while it was being drawn.".to_string())??;
    let image = NSImage::initWithData(NSImage::alloc(), &data).ok_or("Could not read the drawn page.")?;
    rasterize(&image, size, scale)
}

/// Selects the next match of `query` in the page, wrapping at either end.
pub async fn find(webview: &WKWebView, query: &str, backwards: bool) -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let configuration = unsafe { WKFindConfiguration::new(mtm) };
    unsafe {
        configuration.setBackwards(backwards);
        configuration.setWraps(true);
        configuration.setCaseSensitive(false);
    }
    let (sender, receiver) = once::<bool>();
    let block = RcBlock::new(move |result: NonNull<WKFindResult>| {
        answer(&sender, unsafe { result.as_ref().matchFound() });
    });
    unsafe { webview.findString_withConfiguration_completionHandler(&NSString::from_str(query), Some(&configuration), &block) };
    receiver.await.unwrap_or(false)
}

/// Every cookie the session holds, as (domain, name, path, the cookie itself).
pub async fn all_cookies(store: &WKWebsiteDataStore) -> Vec<Retained<NSHTTPCookie>> {
    let cookie_store = unsafe { store.httpCookieStore() };
    let (sender, receiver) = once::<Vec<Retained<NSHTTPCookie>>>();
    let block = RcBlock::new(move |cookies: NonNull<NSArray<NSHTTPCookie>>| {
        answer(&sender, unsafe { cookies.as_ref() }.to_vec());
    });
    unsafe { cookie_store.getAllCookies(&block) };
    receiver.await.unwrap_or_default()
}

pub async fn delete_cookie(store: &WKWebsiteDataStore, cookie: &NSHTTPCookie) {
    let cookie_store = unsafe { store.httpCookieStore() };
    let (sender, receiver) = once::<()>();
    let block = RcBlock::new(move || answer(&sender, ()));
    unsafe { cookie_store.deleteCookie_completionHandler(cookie, Some(&block)) };
    let _ = receiver.await;
}

fn http_cookie(details: &CookieDetails) -> Option<Retained<NSHTTPCookie>> {
    let properties: Retained<NSMutableDictionary<NSHTTPCookiePropertyKey, AnyObject>> = NSMutableDictionary::new();
    let set = |key: &NSHTTPCookiePropertyKey, value: &AnyObject| unsafe { properties.setObject_forKey(value, objc2::runtime::ProtocolObject::from_ref(key)) };
    unsafe {
        set(NSHTTPCookieName, &NSString::from_str(&details.name));
        set(NSHTTPCookieValue, &NSString::from_str(&details.value));
        set(NSHTTPCookiePath, &NSString::from_str(&details.path));
        match &details.domain {
            Some(domain) => set(NSHTTPCookieDomain, &NSString::from_str(domain)),
            None => set(NSHTTPCookieOriginURL, &*NSURL::URLWithString(&NSString::from_str(&details.url))?),
        }
        if details.secure {
            set(NSHTTPCookieSecure, ns_string!("TRUE"));
        }
        if details.http_only {
            set(ns_string!("HttpOnly"), ns_string!("TRUE"));
        }
        match details.same_site {
            SameSite::Lax => set(ns_string!("SameSite"), ns_string!("lax")),
            SameSite::Strict => set(ns_string!("SameSite"), ns_string!("strict")),
            SameSite::NoRestriction => set(ns_string!("SameSite"), ns_string!("none")),
            SameSite::Unspecified => {}
        }
        if let Some(expires) = details.expires {
            set(NSHTTPCookieExpires, &NSDate::dateWithTimeIntervalSince1970(expires as f64));
        }
        NSHTTPCookie::cookieWithProperties(&properties)
    }
}

/// Sets one cookie, answering whether the store took it.
pub async fn set_cookie(store: &WKWebsiteDataStore, details: &CookieDetails) -> bool {
    let Some(cookie) = http_cookie(details) else { return false };
    let cookie_store = unsafe { store.httpCookieStore() };
    let (sender, receiver) = once::<()>();
    let block = RcBlock::new(move || answer(&sender, ()));
    unsafe { cookie_store.setCookie_completionHandler(&cookie, Some(&block)) };
    receiver.await.is_ok()
}

/// Removes every cookie, cache and storage the session holds.
pub async fn clear_store(store: &WKWebsiteDataStore) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let types = unsafe { WKWebsiteDataStore::allWebsiteDataTypes(mtm) };
    let (sender, receiver) = once::<()>();
    let block = RcBlock::new(move || answer(&sender, ()));
    unsafe { store.removeDataOfTypes_modifiedSince_completionHandler(&types, &NSDate::distantPast(), &block) };
    let _ = receiver.await;
}
