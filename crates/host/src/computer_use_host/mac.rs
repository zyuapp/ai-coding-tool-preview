//! macOS computer use: the app's own Accessibility and Screen Recording grants.

use super::{ComputerUsePermission, ComputerUsePermissions};

const ACCESSIBILITY_SETTINGS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
const SCREEN_RECORDING_SETTINGS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

/// A run cannot start until both grants are in.
pub const NEEDS_GRANTS: bool = true;

pub fn capability() -> Result<&'static str, String> {
    Ok("macos")
}

pub fn permissions(_binary: Option<&std::path::Path>) -> ComputerUsePermissions {
    ComputerUsePermissions { accessibility: accessibility(), screen_recording: screen_recording(), linux_runtime: None }
}

/// macOS shows its own prompt once; after that the user is sent to the setting, and a grant takes
/// effect only once the app restarts.
pub fn request(permission: ComputerUsePermission) {
    match permission {
        ComputerUsePermission::Accessibility => {
            if !prompt_accessibility() {
                crate::open::open_url(ACCESSIBILITY_SETTINGS);
            }
        }
        ComputerUsePermission::ScreenRecording => {
            request_screen_recording();
            if !screen_recording() {
                crate::open::open_url(SCREEN_RECORDING_SETTINGS);
            }
        }
    }
}

pub fn start_failure(_display: &str, error: String) -> String {
    error
}

use std::ffi::c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDictionaryCreate(allocator: *const c_void, keys: *const *const c_void, values: *const *const c_void, count: isize, key_callbacks: *const c_void, value_callbacks: *const c_void) -> *const c_void;
    fn CFRelease(value: *const c_void);
}

pub fn accessibility() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn screen_recording() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Asks macOS to list the app under Accessibility, which shows its own prompt the first time.
pub fn prompt_accessibility() -> bool {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), values.as_ptr(), 1, &kCFTypeDictionaryKeyCallBacks as *const c_void, &kCFTypeDictionaryValueCallBacks as *const c_void);
        let trusted = AXIsProcessTrustedWithOptions(options);
        if !options.is_null() {
            CFRelease(options);
        }
        trusted
    }
}

pub fn request_screen_recording() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}
