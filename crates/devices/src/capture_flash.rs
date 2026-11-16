//! Whitens the window that was just captured, the way a camera says it took the picture
//! (capture-flash.ts). Drawn after the capture, so the flash is never in the shot. The flash is a
//! panel that never takes focus or the pointer, above every window, fading out over 200ms.

/// A window's place on screen, in the points CoreGraphics counts from the top left of the main display.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindowFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub const FLASH_MS: u64 = 200;
/// Long enough for the fade to finish before the overlay goes, short enough to never be in the way.
pub const TEARDOWN_MS: u64 = FLASH_MS + 120;

/// Whether this desktop marks a capture with a flash.
pub fn flashes_captured_windows() -> bool {
    cfg!(target_os = "macos")
}

/// Flashes `frame`. Must be called on the main thread, as GPUI's own callbacks are.
pub fn flash_window(frame: WindowFrame) {
    if !flashes_captured_windows() || frame.width < 1.0 || frame.height < 1.0 {
        return;
    }
    #[cfg(target_os = "macos")]
    mac::flash(frame);
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{msg_send, sel, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSAnimationContext, NSBackingStoreType, NSColor, NSPanel, NSScreen, NSScreenSaverWindowLevel, NSWindowAnimationBehavior, NSWindowCollectionBehavior, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    use super::{WindowFrame, FLASH_MS, TEARDOWN_MS};

    pub fn flash(frame: WindowFrame) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        // CoreGraphics counts down from the main display's top; AppKit counts up from its bottom.
        let screens = NSScreen::screens(mtm);
        let Some(main) = screens.firstObject() else { return };
        let height = main.frame().size.height;
        let rect = NSRect::new(NSPoint::new(frame.x.round(), (height - frame.y - frame.height).round()), NSSize::new(frame.width.round(), frame.height.round()));
        // SAFETY: a borderless, non-activating panel made on the main thread, kept until the delayed
        // `orderOut:` below runs, which retains it.
        unsafe {
            let panel: Retained<NSPanel> = NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm),
                rect,
                NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                NSBackingStoreType::Buffered,
                false,
            );
            panel.setReleasedWhenClosed(false);
            panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
            panel.setOpaque(false);
            panel.setHasShadow(false);
            panel.setIgnoresMouseEvents(true);
            panel.setBackgroundColor(Some(&NSColor::whiteColor()));
            panel.setAlphaValue(0.9);
            // Above the window coming forward, so the flash is not drawn under what it announces.
            panel.setLevel(NSScreenSaverWindowLevel);
            panel.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary | NSWindowCollectionBehavior::Stationary);
            panel.orderFrontRegardless();
            let fading = panel.clone();
            let changes = RcBlock::new(move |context: NonNull<NSAnimationContext>| {
                let context = context.as_ref();
                context.setDuration(FLASH_MS as f64 / 1000.0);
                if let Some(class) = AnyClass::get(c"CAMediaTimingFunction") {
                    let ease: *mut AnyObject = msg_send![class, functionWithName: &*NSString::from_str("easeOut")];
                    let _: () = msg_send![context, setTimingFunction: ease];
                }
                let animator: Retained<NSPanel> = msg_send![&fading, animator];
                animator.setAlphaValue(0.0);
            });
            NSAnimationContext::runAnimationGroup(&changes);
            let _: () = msg_send![&panel, performSelector: sel!(orderOut:), withObject: std::ptr::null::<AnyObject>(), afterDelay: TEARDOWN_MS as f64 / 1000.0];
        }
    }
}
