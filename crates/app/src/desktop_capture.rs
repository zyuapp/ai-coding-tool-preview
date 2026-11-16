//! The capture keystroke the whole desktop hears (keyboard-host.ts `claimDesktopShortcut`): it grabs
//! the frontmost app's window into the composer without bringing this app forward first.

#[cfg(any(target_os = "macos", test))]
use act_core::domain::shortcuts::parse_shortcut;
use act_core::domain::shortcuts::{resolve_shortcuts, Keystroke, ShortcutSurface};
#[cfg(target_os = "macos")]
use act_core::events::{DesktopShortcutUnavailable, WorkspaceEvent};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use gpui::{App, Global};

/// The keystroke held now, if any.
#[derive(Default)]
struct Claimed {
    binding: Option<String>,
    #[cfg(target_os = "macos")]
    hotkey: Option<mac::HotKey>,
}

impl Global for Claimed {}

/// Claims the bound keystroke, or lets it go, when the binding changed since the last call.
pub fn sync(state: &WorkspaceState, cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let wanted = resolve_shortcuts(&state.shortcuts)
        .into_iter()
        .find(|binding| binding.surface == ShortcutSurface::Desktop && binding.action == "window.capture")
        .map(|binding| binding.binding);
    if cx.try_global::<Claimed>().is_some_and(|claimed| claimed.binding == wanted) {
        return;
    }
    if cx.try_global::<Claimed>().is_none() {
        cx.set_global(Claimed::default());
        listen(cx);
    }
    #[cfg(target_os = "macos")]
    {
        cx.global_mut::<Claimed>().hotkey = None;
        let claimed = wanted.as_deref().and_then(parse_shortcut).and_then(|stroke| mac::HotKey::register(&stroke));
        if let (Some(binding), None) = (&wanted, &claimed) {
            let refusal = DesktopShortcutUnavailable { binding: binding.clone(), reason: "taken".into(), message: String::new() };
            crate::workspace::workspace(cx).read(cx).host.sink.event(WorkspaceEvent::ShortcutUnavailable { refusal });
        }
        cx.global_mut::<Claimed>().hotkey = claimed;
    }
    cx.global_mut::<Claimed>().binding = wanted;
}

/// Hands each press of the keystroke to the main thread.
fn listen(cx: &mut App) {
    let (sender, mut presses) = futures::channel::mpsc::unbounded::<()>();
    #[cfg(target_os = "macos")]
    mac::set_sender(sender);
    #[cfg(not(target_os = "macos"))]
    drop(sender);
    cx.spawn(async move |cx| {
        use futures::StreamExt;
        while presses.next().await.is_some() {
            let _ = cx.update(capture);
        }
    })
    .detach();
}

/// Captures the frontmost window and attaches it to the composer, or says why it could not.
fn capture(cx: &mut App) {
    use act_devices::window_screenshot::{capture_frontmost_window, WindowShot};
    let workspace = crate::workspace::workspace(cx).read(cx);
    let options = workspace.state.capture_options();
    let host = workspace.host.clone();
    let driver = crate::effects::system::computer_use_driver();
    cx.spawn(async move |cx| {
        let shot = act_host::runtime().spawn(capture_frontmost_window(options.sound, driver)).await;
        let Ok(shot) = shot else { return };
        let notify = |title: &str, body: &str| act_host::notify::notify(title, body, None);
        match shot {
            WindowShot::Captured { app, title, png, frame, context } => {
                let context = crate::effects::convert(&context).ok();
                let store = host.clone();
                let saved = act_host::runtime().spawn(async move { store.services.attachments.write(&png, context.as_ref()).await }).await;
                match saved {
                    Ok(Ok(path)) => {
                        let label = if title.is_empty() { app.clone() } else { format!("{app} — {title}") };
                        host.sink.command(AppCommand::ImageAdd { task_id: None, path: path.to_string_lossy().into_owned(), label, source: None });
                        if options.focus {
                            let _ = cx.update(|_| act_devices::capture_flash::flash_window(frame));
                            host.sink.send(crate::effects::Inbound::Window(crate::effects::requests::WindowWork::Reveal));
                        } else {
                            notify("Screenshot attached", &format!("{app} — waiting in {}", act_core::identity::APP_NAME));
                        }
                    }
                    Ok(Err(error)) => notify("Could not keep the screenshot", &error.to_string()),
                    Err(error) => notify("Could not keep the screenshot", &error.to_string()),
                }
            }
            WindowShot::Denied => {
                notify(concat!(act_core::app_name!(), " needs Screen Recording"), "Grant it in System Settings → Privacy & Security, then try again.");
                let _ = cx.update(|cx| cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"));
            }
            WindowShot::NoWindow { app } => notify("Nothing to capture", &format!("{app} has no window on screen.")),
            WindowShot::Failed { message } | WindowShot::Unsupported { message } => notify("Could not capture the window", &message),
        }
    })
    .detach();
}

/// The Carbon key code and modifiers of a keystroke, as `RegisterEventHotKey` takes them.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn carbon_key(stroke: &Keystroke) -> Option<(u32, u32)> {
    const CMD: u32 = 0x100;
    const SHIFT: u32 = 0x200;
    const OPTION: u32 = 0x800;
    const CONTROL: u32 = 0x1000;
    let code = match stroke.key.as_str() {
        "A" => 0, "S" => 1, "D" => 2, "F" => 3, "H" => 4, "G" => 5, "Z" => 6, "X" => 7, "C" => 8, "V" => 9,
        "B" => 11, "Q" => 12, "W" => 13, "E" => 14, "R" => 15, "Y" => 16, "T" => 17, "1" => 18, "2" => 19,
        "3" => 20, "4" => 21, "6" => 22, "5" => 23, "=" => 24, "9" => 25, "7" => 26, "-" => 27, "8" => 28,
        "0" => 29, "]" => 30, "O" => 31, "U" => 32, "[" => 33, "I" => 34, "P" => 35, "Enter" => 36, "L" => 37,
        "J" => 38, "'" => 39, "K" => 40, ";" => 41, "\\" => 42, "," => 43, "/" => 44, "N" => 45, "M" => 46,
        "." => 47, "Space" => 49, "`" => 50, "Backspace" => 51, "F5" => 96, "F6" => 97, "F7" => 98, "F3" => 99,
        "F8" => 100, "F9" => 101, "F11" => 103, "F10" => 109, "F12" => 111, "Home" => 115, "PageUp" => 116,
        "Delete" => 117, "F4" => 118, "End" => 119, "F2" => 120, "PageDown" => 121, "F1" => 122,
        "ArrowLeft" => 123, "ArrowRight" => 124, "ArrowDown" => 125, "ArrowUp" => 126,
        _ => return None,
    };
    let mut modifiers = 0;
    if stroke.r#mod {
        modifiers |= CMD;
    }
    if stroke.shift {
        modifiers |= SHIFT;
    }
    if stroke.alt {
        modifiers |= OPTION;
    }
    if stroke.ctrl {
        modifiers |= CONTROL;
    }
    Some((code, modifiers))
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::sync::{Mutex, Once};

    use act_core::domain::shortcuts::Keystroke;
    use futures::channel::mpsc::UnboundedSender;

    #[repr(C)]
    struct EventHotKeyId {
        signature: u32,
        id: u32,
    }

    #[repr(C)]
    struct EventTypeSpec {
        class: u32,
        kind: u32,
    }

    type Handler = extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> *mut c_void;
        fn RegisterEventHotKey(code: u32, modifiers: u32, id: EventHotKeyId, target: *mut c_void, options: u32, out: *mut *mut c_void) -> i32;
        fn UnregisterEventHotKey(hotkey: *mut c_void) -> i32;
        fn InstallEventHandler(target: *mut c_void, handler: Handler, count: u32, kinds: *const EventTypeSpec, data: *mut c_void, out: *mut *mut c_void) -> i32;
    }

    const KEYBOARD_CLASS: u32 = u32::from_be_bytes(*b"keyb");
    const HOT_KEY_PRESSED: u32 = 5;
    const SIGNATURE: u32 = u32::from_be_bytes(*b"aict");

    static SENDER: Mutex<Option<UnboundedSender<()>>> = Mutex::new(None);
    static HANDLER: Once = Once::new();

    pub fn set_sender(sender: UnboundedSender<()>) {
        *SENDER.lock().unwrap() = Some(sender);
    }

    extern "C" fn pressed(_: *mut c_void, _: *mut c_void, _: *mut c_void) -> i32 {
        if let Some(sender) = SENDER.lock().unwrap().as_ref() {
            let _ = sender.unbounded_send(());
        }
        0
    }

    /// A keystroke claimed from the whole desktop, given back when dropped.
    pub struct HotKey(*mut c_void);

    impl HotKey {
        /// Claims `stroke`, or None when it cannot be named or another app holds it.
        pub fn register(stroke: &Keystroke) -> Option<HotKey> {
            let (code, modifiers) = super::carbon_key(stroke)?;
            HANDLER.call_once(|| unsafe {
                let kinds = EventTypeSpec { class: KEYBOARD_CLASS, kind: HOT_KEY_PRESSED };
                InstallEventHandler(GetApplicationEventTarget(), pressed, 1, &kinds, std::ptr::null_mut(), std::ptr::null_mut());
            });
            let mut hotkey = std::ptr::null_mut();
            let status = unsafe { RegisterEventHotKey(code, modifiers, EventHotKeyId { signature: SIGNATURE, id: 1 }, GetApplicationEventTarget(), 0, &mut hotkey) };
            (status == 0 && !hotkey.is_null()).then_some(HotKey(hotkey))
        }
    }

    impl Drop for HotKey {
        fn drop(&mut self) {
            unsafe { UnregisterEventHotKey(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_keystroke_maps_to_carbon_codes() {
        let stroke = parse_shortcut("Alt+Shift+S").unwrap();
        assert_eq!(carbon_key(&stroke), Some((1, 0x200 | 0x800)));
        assert_eq!(carbon_key(&parse_shortcut("Mod+Ctrl+ArrowUp").unwrap()), Some((126, 0x100 | 0x1000)));
    }
}
