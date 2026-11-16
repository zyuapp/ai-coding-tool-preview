//! Liquid Glass under a window (native/glass, window-backdrop.ts). The glass is the system's own, so it
//! is as clear or as tinted as the person has set it. GPUI draws into the window's content view, so the
//! sheet and its panes sit behind that view, and the window is opened transparent over them.

use gpui::{Pixels, WindowBackgroundAppearance};

/// A pane of clear glass over the window's sheet, in the window's points from its top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassRegion {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub radius: f32,
}

impl GlassRegion {
    pub fn new(bounds: gpui::Bounds<Pixels>, radius: Pixels) -> Self {
        GlassRegion { x: bounds.origin.x.into(), y: bounds.origin.y.into(), width: bounds.size.width.into(), height: bounds.size.height.into(), radius: radius.into() }
    }
}

/// What a window is drawn over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMaterial {
    /// The system's Liquid Glass.
    Glass,
    /// The sidebar blur, where this macOS has no glass.
    Vibrancy,
    /// The theme's own canvas, on desktops without a material.
    None,
}

impl WindowMaterial {
    /// The material this computer draws under a window.
    pub fn current() -> WindowMaterial {
        if !cfg!(target_os = "macos") {
            WindowMaterial::None
        } else if available() {
            WindowMaterial::Glass
        } else {
            WindowMaterial::Vibrancy
        }
    }

    /// How GPUI opens a window over this material: clear over glass, blurred for vibrancy, else opaque.
    pub fn background(self) -> WindowBackgroundAppearance {
        match self {
            WindowMaterial::Glass => WindowBackgroundAppearance::Transparent,
            WindowMaterial::Vibrancy => WindowBackgroundAppearance::Blurred,
            WindowMaterial::None => WindowBackgroundAppearance::Opaque,
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::AnyClass;
    use objc2::{msg_send, ClassType, MainThreadMarker};
    use objc2_app_kit::{NSAutoresizingMaskOptions, NSView, NSVisualEffectView, NSWindowOrderingMode};
    use objc2_foundation::{ns_string, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSUserDefaults};

    use super::GlassRegion;

    const SHEET: &str = "aic-window-glass";
    const PANE: &str = "aic-glass-region";
    const BACKING: &str = "aic-window-vibrancy";

    /// Looked up by name, so this builds against an SDK older than the class.
    fn glass_class() -> Option<&'static AnyClass> {
        AnyClass::get(c"NSGlassEffectView")
    }

    pub fn available() -> bool {
        glass_class().is_some()
    }

    fn identifier(view: &NSView) -> Option<String> {
        // SAFETY: every NSView answers `identifier` with an optional string.
        let identifier: Option<Retained<NSString>> = unsafe { msg_send![view, identifier] };
        identifier.map(|identifier| identifier.to_string())
    }

    fn make(class: &AnyClass, frame: NSRect, name: &str, style: Option<i32>, radius: f64) -> Retained<NSView> {
        // SAFETY: NSGlassEffectView is an NSView subclass made with initWithFrame:, and takes its style
        // and corner radius by key-value coding.
        unsafe {
            let allocated: Allocated<NSView> = msg_send![class, alloc];
            let view: Retained<NSView> = msg_send![allocated, initWithFrame: frame];
            if let Some(style) = style {
                let _: () = msg_send![&view, setValue: &*NSNumber::new_i32(style), forKey: ns_string!("style")];
            }
            let _: () = msg_send![&view, setValue: &*NSNumber::new_f64(radius), forKey: ns_string!("cornerRadius")];
            let _: () = msg_send![&view, setIdentifier: &*NSString::from_str(name)];
            view
        }
    }

    /// GPUI's content view and the frame view it sits in.
    fn views(window: &gpui::Window) -> Option<(Retained<NSView>, Retained<NSView>)> {
        MainThreadMarker::new()?;
        let content = crate::appkit::content_view(window)?;
        // SAFETY: a view in a window has the window's frame view, or its content view's own parent, above it.
        let frame = unsafe { content.superview() }?;
        Some((content, frame))
    }

    pub fn attach(window: &gpui::Window) -> bool {
        let (Some(class), Some((content, frame))) = (glass_class(), views(window)) else { return false };
        for view in frame.subviews().iter() {
            if matches!(identifier(&view).as_deref(), Some(SHEET | BACKING)) {
                view.removeFromSuperview();
            } else if view.isKindOfClass(NSVisualEffectView::class()) {
                view.setHidden(true);
            }
        }
        let fill = NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable;
        let sheet = make(class, frame.bounds(), SHEET, None, 0.0);
        sheet.setAutoresizingMask(fill);
        frame.addSubview_positioned_relativeTo(&sheet, NSWindowOrderingMode::Below, Some(&content));
        // Electron's window keeps its sidebar vibrancy over the glass and its panes, under the page:
        // frosted and light or dark with the appearance, not the desktop seen through.
        // SAFETY: NSVisualEffectView is made with initWithFrame: and takes its material, blending mode
        // and state as integers.
        let backing: Retained<NSView> = unsafe {
            let allocated: Allocated<NSView> = msg_send![NSVisualEffectView::class(), alloc];
            let view: Retained<NSView> = msg_send![allocated, initWithFrame: frame.bounds()];
            let _: () = msg_send![&view, setMaterial: 7_isize];
            let _: () = msg_send![&view, setBlendingMode: 0_isize];
            let _: () = msg_send![&view, setState: 0_isize];
            let _: () = msg_send![&view, setIdentifier: &*NSString::from_str(BACKING)];
            view
        };
        backing.setAutoresizingMask(fill);
        frame.addSubview_positioned_relativeTo(&backing, NSWindowOrderingMode::Below, Some(&content));
        true
    }

    pub fn regions(window: &gpui::Window, regions: &[GlassRegion]) -> bool {
        let (Some(class), Some((content, frame))) = (glass_class(), views(window)) else { return false };
        let mut panes: Vec<Retained<NSView>> = frame.subviews().iter().filter(|view| identifier(view).as_deref() == Some(PANE)).collect();
        let sheet = frame.subviews().iter().find(|view| identifier(view).as_deref() == Some(SHEET));
        while panes.len() > regions.len() {
            if let Some(pane) = panes.pop() {
                pane.removeFromSuperview();
            }
        }
        let height = content.bounds().size.height;
        let origin = content.frame().origin;
        let flipped = content.isFlipped();
        for (index, region) in regions.iter().enumerate() {
            let (x, y, width, region_height) = (f64::from(region.x), f64::from(region.y), f64::from(region.width), f64::from(region.height));
            let top = if flipped { y } else { height - y - region_height };
            let rect = NSRect::new(NSPoint::new(origin.x + x, origin.y + top), NSSize::new(width, region_height));
            let pane = match panes.get(index) {
                Some(pane) => pane.clone(),
                None => {
                    let pane = make(class, rect, PANE, Some(1), f64::from(region.radius));
                    match &sheet {
                        Some(sheet) => frame.addSubview_positioned_relativeTo(&pane, NSWindowOrderingMode::Above, Some(sheet)),
                        None => frame.addSubview_positioned_relativeTo(&pane, NSWindowOrderingMode::Below, Some(&content)),
                    }
                    panes.push(pane.clone());
                    pane
                }
            };
            pane.setFrame(rect);
            // SAFETY: the pane is a glass view, which takes its corner radius by key-value coding.
            unsafe {
                let _: () = msg_send![&pane, setValue: &*NSNumber::new_f64(f64::from(region.radius)), forKey: ns_string!("cornerRadius")];
            }
        }
        true
    }

    pub fn set_app_appearance(dark: Option<bool>) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        let Some(class) = AnyClass::get(c"NSAppearance") else { return };
        // SAFETY: NSAppearance answers appearanceNamed: with an optional appearance, and NSApplication
        // takes an optional appearance; nil follows the system.
        unsafe {
            let appearance: Option<Retained<objc2::runtime::AnyObject>> = match dark {
                Some(true) => msg_send![class, appearanceNamed: ns_string!("NSAppearanceNameDarkAqua")],
                Some(false) => msg_send![class, appearanceNamed: ns_string!("NSAppearanceNameAqua")],
                None => None,
            };
            let current: Option<Retained<objc2::runtime::AnyObject>> = msg_send![&app, appearance];
            let name = |appearance: &Option<Retained<objc2::runtime::AnyObject>>| -> Option<String> {
                let name: Option<Retained<NSString>> = appearance.as_ref().and_then(|appearance| msg_send![&**appearance, name]);
                name.map(|name| name.to_string())
            };
            if name(&current) == name(&appearance) {
                return;
            }
            let _: () = msg_send![&app, setAppearance: appearance.as_deref()];
        }
    }

    pub fn system_glass_tint() -> f64 {
        let defaults = NSUserDefaults::standardUserDefaults();
        let amount = defaults.floatForKey(ns_string!("NSGlassTintAmount"));
        if amount.is_finite() {
            f64::from(amount).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// Whether this macOS has Liquid Glass.
pub fn available() -> bool {
    #[cfg(target_os = "macos")]
    return mac::available();
    #[cfg(not(target_os = "macos"))]
    false
}

/// Lays one sheet of glass under the whole window. Returns whether glass went under it.
pub fn attach(window: &gpui::Window) -> bool {
    #[cfg(target_os = "macos")]
    return mac::attach(window);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        false
    }
}

/// One pane of clear glass per region, over the sheet and under the window's drawing. Panes are reused,
/// so a region that moves is not rebuilt.
pub fn regions(window: &gpui::Window, regions: &[GlassRegion]) -> bool {
    #[cfg(target_os = "macos")]
    return mac::regions(window, regions);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, regions);
        false
    }
}

/// Draws the app's windows, their glass and system controls light or dark, or as the system does for
/// `None` (Electron's `nativeTheme.themeSource`).
pub fn set_app_appearance(dark: Option<bool>) {
    #[cfg(target_os = "macos")]
    mac::set_app_appearance(dark);
    #[cfg(not(target_os = "macos"))]
    let _ = dark;
}

/// How tinted the person has set the system's glass, from 0 for the clearest to 1.
pub fn system_glass_tint() -> f64 {
    #[cfg(target_os = "macos")]
    return mac::system_glass_tint();
    #[cfg(not(target_os = "macos"))]
    0.0
}

/// A page names its panes in its own pixels, which a zoomed window draws larger than its points.
pub fn regions_in_points(regions: &[GlassRegion], zoom: f32) -> Vec<GlassRegion> {
    if zoom == 1.0 {
        return regions.to_vec();
    }
    regions.iter().map(|region| GlassRegion { x: region.x * zoom, y: region.y * zoom, width: region.width * zoom, height: region.height * zoom, radius: region.radius * zoom }).collect()
}
