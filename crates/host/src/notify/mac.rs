//! macOS notifications, idleness and the lock screen.

use std::ffi::{c_char, c_void};

/// Names the application notifications come from; without one they are attributed to a stand-in.
pub fn set_notification_application(bundle_identifier: &str) {
    let _ = mac_notification_sys::set_application(bundle_identifier);
}

/// Shows one notification and waits for it to be answered; true when it was clicked.
pub fn show(title: &str, body: &str) -> bool {
    let response = mac_notification_sys::Notification::new().title(title).message(body).wait_for_click(true).send();
    matches!(response, Ok(mac_notification_sys::NotificationResponse::Click))
}

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
