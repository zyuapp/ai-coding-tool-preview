//! The native half of a visual: a clipping view laid over the window's own, holding the WebKit page
//! a visual runs in. It clips the page to what of its row is in view and leaves holes where the
//! app's own controls sit over it, both for drawing and for clicks.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::encode::{Encoding, RefEncode};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventType, NSResponder, NSView};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize};
use wry::raw_window_handle::{AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};

#[derive(Default)]
pub struct ClipIvars {
    /// Rects in the view's own (flipped) space that neither draw the page nor take its clicks.
    holes: RefCell<Vec<NSRect>>,
    /// Whether the page scrolls itself; one that does not hands the wheel to the transcript under it.
    scrolls: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ClipIvars]
    pub struct ClipView;

    unsafe impl NSObjectProtocol for ClipView {}

    impl ClipView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let frame = self.frame();
            let local = NSPoint::new(point.x - frame.origin.x, point.y - frame.origin.y);
            // The superview is not flipped, so its y runs up from the bottom of this view.
            let local = NSPoint::new(local.x, frame.size.height - local.y);
            if self.ivars().holes.borrow().iter().any(|hole| contains(hole, local)) {
                return std::ptr::null_mut();
            }
            let hit: *mut NSView = unsafe { msg_send![super(self), hitTest: point] };
            if !hit.is_null() && !self.ivars().scrolls.get() {
                let wheel = NSApplication::sharedApplication(self.mtm()).currentEvent().is_some_and(|event| matches!(event.r#type(), NSEventType::ScrollWheel | NSEventType::Magnify));
                if wheel {
                    return self as *const ClipView as *mut NSView;
                }
            }
            hit
        }
    }
);

fn contains(rect: &NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x && point.x < rect.origin.x + rect.size.width && point.y >= rect.origin.y && point.y < rect.origin.y + rect.size.height
}

#[repr(C)]
struct CGColor {
    _private: [u8; 0],
}

unsafe impl RefEncode for CGColor {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColor", &[]));
}

impl ClipView {
    /// A hidden clip over `parent`, the window's content view.
    pub fn attach(parent: &NSView) -> Retained<ClipView> {
        let mtm = MainThreadMarker::new().expect("visuals are placed on the main thread");
        let view = Self::alloc(mtm).set_ivars(ClipIvars::default());
        let view: Retained<ClipView> = unsafe { msg_send![super(view), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0))] };
        view.setWantsLayer(true);
        let _: () = unsafe { msg_send![&*view, setClipsToBounds: true] };
        if let Some(layer) = layer(&view) {
            let _: () = unsafe { msg_send![layer, setMasksToBounds: true] };
        }
        view.setHidden(true);
        parent.addSubview(&view);
        view
    }

    /// Places the clip at `clip` and the page inside it at `page`, both in window points from the
    /// top left. `holes` are in the page's own points.
    pub fn place(&self, page_view: &NSView, clip: (f64, f64, f64, f64), page: (f64, f64, f64, f64), holes: &[(f64, f64, f64, f64)], shown: bool) {
        let Some(parent) = (unsafe { self.superview() }) else { return };
        let parent_height = parent.frame().size.height;
        let (x, y, width, height) = clip;
        let frame = NSRect::new(NSPoint::new(x, if parent.isFlipped() { y } else { parent_height - y - height }), NSSize::new(width, height));
        if self.frame() != frame {
            self.setFrame(frame);
        }
        let inner = NSRect::new(NSPoint::new(page.0 - x, page.1 - y), NSSize::new(page.2, page.3));
        if page_view.frame() != inner {
            page_view.setFrame(inner);
        }
        let holes: Vec<NSRect> = holes
            .iter()
            .map(|hole| NSRect::new(NSPoint::new(inner.origin.x + hole.0, inner.origin.y + hole.1), NSSize::new(hole.2, hole.3)))
            .filter(|hole| hole.size.width > 0.0 && hole.size.height > 0.0)
            .collect();
        let changed = *self.ivars().holes.borrow() != holes || self.frame().size != frame.size;
        if changed {
            self.mask(&holes, frame.size);
            *self.ivars().holes.borrow_mut() = holes;
        }
        let alpha = if shown { 1.0 } else { 0.0 };
        if self.alphaValue() != alpha {
            self.setAlphaValue(alpha);
        }
        if self.isHidden() {
            self.setHidden(false);
        }
    }

    pub fn set_scrolls(&self, scrolls: bool) {
        self.ivars().scrolls.set(scrolls);
    }

    pub fn hide(&self) {
        if !self.isHidden() {
            self.setHidden(true);
        }
    }

    pub fn detach(&self) {
        self.removeFromSuperview();
    }

    /// Draws only outside `holes`: the mask is the view less the holes, as rectangles that tile it.
    fn mask(&self, holes: &[NSRect], size: NSSize) {
        let Some(layer) = layer(self) else { return };
        if holes.is_empty() {
            let none: *mut AnyObject = std::ptr::null_mut();
            let _: () = unsafe { msg_send![layer, setMask: none] };
            return;
        }
        let Some(class) = AnyClass::get(c"CALayer") else { return };
        let mask: Retained<AnyObject> = unsafe { msg_send![class, layer] };
        let black: Retained<AnyObject> = unsafe { msg_send![AnyClass::get(c"NSColor").expect("AppKit"), blackColor] };
        let color: *const CGColor = unsafe { msg_send![&*black, CGColor] };
        let bands = uncovered(holes, size);
        let flipped: bool = unsafe { msg_send![layer, isGeometryFlipped] };
        for band in bands.into_iter().filter(|band| band.size.width > 0.0 && band.size.height > 0.0) {
            let piece: Retained<AnyObject> = unsafe { msg_send![class, layer] };
            // A layer's own space runs up from the bottom unless it is flipped.
            let frame = if flipped { band } else { NSRect::new(NSPoint::new(band.origin.x, size.height - band.origin.y - band.size.height), band.size) };
            let _: () = unsafe { msg_send![&*piece, setFrame: frame] };
            let _: () = unsafe { msg_send![&*piece, setBackgroundColor: color] };
            let _: () = unsafe { msg_send![&*mask, addSublayer: &*piece] };
        }
        let _: () = unsafe { msg_send![&*mask, setFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)] };
        let _: () = unsafe { msg_send![layer, setMask: &*mask] };
    }
}

/// Rectangles tiling `size` less every hole: a grid cut at each hole's edges, cells outside the
/// holes kept and joined along their rows.
fn uncovered(holes: &[NSRect], size: NSSize) -> Vec<NSRect> {
    let clamp = |value: f64, max: f64| value.clamp(0.0, max);
    let mut xs = vec![0.0, size.width];
    let mut ys = vec![0.0, size.height];
    for hole in holes {
        xs.extend([clamp(hole.origin.x, size.width), clamp(hole.origin.x + hole.size.width, size.width)]);
        ys.extend([clamp(hole.origin.y, size.height), clamp(hole.origin.y + hole.size.height, size.height)]);
    }
    for edges in [&mut xs, &mut ys] {
        edges.sort_by(f64::total_cmp);
        edges.dedup();
    }
    let mut bands = Vec::new();
    for row in ys.windows(2) {
        let mut start: Option<f64> = None;
        for column in xs.windows(2) {
            let center = NSPoint::new((column[0] + column[1]) / 2.0, (row[0] + row[1]) / 2.0);
            let open = !holes.iter().any(|hole| contains(hole, center));
            match (open, start) {
                (true, None) => start = Some(column[0]),
                (false, Some(from)) => {
                    bands.push(NSRect::new(NSPoint::new(from, row[0]), NSSize::new(column[0] - from, row[1] - row[0])));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(from) = start {
            bands.push(NSRect::new(NSPoint::new(from, row[0]), NSSize::new(size.width - from, row[1] - row[0])));
        }
    }
    bands
}

fn layer(view: &NSView) -> Option<&AnyObject> {
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    unsafe { layer.as_ref() }
}

/// Lets WebKit build its page as a child of a view of ours.
pub struct ParentHandle(pub NonNull<c_void>);

impl HasWindowHandle for ParentHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(self.0))) })
    }
}

/// The window's content view, which visuals are laid over.
pub fn content_view(window: &gpui::Window) -> Option<Retained<NSView>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }
}
