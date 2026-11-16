//! Says what happened where the user already is: a desktop notification, silent, whose click brings
//! the thread forward. And whether the user has stepped away, which decides whether the phone is told.

/// How long the computer sits untouched before the user counts as away from it.
pub const AWAY_AFTER_SECONDS: f64 = 180.0;

/// The notice a thread raises.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadNotice {
    pub task_id: String,
    pub title: String,
    pub headline: String,
}

/// Shows a silent notification. `on_click` runs on a thread of its own when the user clicks it.
pub fn notify(title: &str, body: &str, on_click: Option<Box<dyn FnOnce() + Send>>) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        if show(&title, &body) {
            if let Some(on_click) = on_click {
                on_click();
            }
        }
    });
}

/// Names the application notifications come from; without one they are attributed to a stand-in.
#[cfg(target_os = "macos")]
pub fn set_notification_application(bundle_identifier: &str) {
    let _ = mac_notification_sys::set_application(bundle_identifier);
}

#[cfg(not(target_os = "macos"))]
pub fn set_notification_application(_bundle_identifier: &str) {}

/// Shows one notification and waits for it to be answered; true when it was clicked.
#[cfg(target_os = "macos")]
fn show(title: &str, body: &str) -> bool {
    let response = mac_notification_sys::Notification::new().title(title).message(body).wait_for_click(true).send();
    matches!(response, Ok(mac_notification_sys::NotificationResponse::Click))
}

#[cfg(target_os = "linux")]
fn show(title: &str, body: &str) -> bool {
    let app_id = crate::linux_protocol::LINUX_APP_ID;
    let Ok(handle) = notify_rust::Notification::new()
        .appname(act_core::identity::APP_NAME)
        .icon(app_id)
        .summary(title)
        .body(body)
        .action("default", "Open")
        .hint(notify_rust::Hint::DesktopEntry(app_id.into()))
        .hint(notify_rust::Hint::SuppressSound(true))
        .show()
    else {
        return false;
    };
    let mut clicked = false;
    handle.wait_for_action(|action| clicked = action == "default");
    clicked
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn show(_title: &str, _body: &str) -> bool {
    false
}

/// Whether the screen is locked right now.
pub fn screen_locked() -> bool {
    platform::screen_locked()
}

/// Whether the user has stepped away from this computer: it is locked, or nobody has touched it for a while.
pub fn desktop_away() -> bool {
    platform::screen_locked() || platform::idle_seconds().is_some_and(|idle| idle >= AWAY_AFTER_SECONDS)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_char, c_void};

    type CFTypeRef = *const c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
        fn CGSessionCopyCurrentDictionary() -> CFTypeRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(allocator: CFTypeRef, text: *const c_char, encoding: u32) -> CFTypeRef;
        fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFBooleanGetValue(boolean: CFTypeRef) -> u8;
        fn CFGetTypeID(value: CFTypeRef) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFRelease(value: CFTypeRef);
    }

    const HID_SYSTEM_STATE: i32 = 1;
    const ANY_INPUT_EVENT: u32 = u32::MAX;
    const UTF8: u32 = 0x0800_0100;

    pub fn idle_seconds() -> Option<f64> {
        let idle = unsafe { CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, ANY_INPUT_EVENT) };
        idle.is_finite().then_some(idle)
    }

    pub fn screen_locked() -> bool {
        unsafe {
            let session = CGSessionCopyCurrentDictionary();
            if session.is_null() {
                return false;
            }
            let key = CFStringCreateWithCString(std::ptr::null(), c"CGSSessionScreenIsLocked".as_ptr(), UTF8);
            let value = if key.is_null() { std::ptr::null() } else { CFDictionaryGetValue(session, key) };
            let locked = !value.is_null() && CFGetTypeID(value) == CFBooleanGetTypeID() && CFBooleanGetValue(value) != 0;
            if !key.is_null() {
                CFRelease(key);
            }
            CFRelease(session);
            locked
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    /// The login session's own hints, which every desktop's idle and lock tracking reports to.
    fn session_hint(name: &str) -> Option<String> {
        let session = std::env::var("XDG_SESSION_ID").ok()?;
        let output = std::process::Command::new("loginctl").args(["show-session", &session, "-p", name, "--value"]).output().ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Only whether the session counts itself idle is known here, which reads as away.
    pub fn idle_seconds() -> Option<f64> {
        (session_hint("IdleHint")? == "yes").then_some(super::AWAY_AFTER_SECONDS)
    }

    pub fn screen_locked() -> bool {
        session_hint("LockedHint").as_deref() == Some("yes")
    }
}
