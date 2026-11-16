//! Web views inside GPUI's X11 window. Each sits in an X window of its own, a child of the app's,
//! which GTK draws into as if it were a toplevel. Keys go where the app says: a page only takes
//! them once it is clicked or focused, and hands any it is not meant to have to the app's window.

use std::cell::{Cell, RefCell};
use std::ffi::c_ulong;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib::translate::{FromGlibPtrFull, ToGlibPtr};
use gtk::prelude::*;
use x11_dl::xlib::{self, Xlib};

/// The app's window, as GTK and Xlib reach it.
pub struct Parent {
    xlib: Rc<Xlib>,
    display: *mut xlib::Display,
    gdk_display: gdkx11::X11Display,
    pub xid: c_ulong,
}

/// A rectangle in the app window's pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Parent {
    pub fn of(window: &gpui::Window) -> Result<Rc<Parent>, String> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
        let xid = match handle.as_raw() {
            RawWindowHandle::Xcb(handle) => handle.window.get() as c_ulong,
            RawWindowHandle::Xlib(handle) => handle.window,
            _ => return Err("Web pages can be placed only in an X11 window.".into()),
        };
        let gdk_display = gdk::Display::default().ok_or("GTK has no display.")?.downcast::<gdkx11::X11Display>().map_err(|_| "GTK is not on X11.")?;
        let display = unsafe { gdkx11::ffi::gdk_x11_display_get_xdisplay(gdk_display.to_glib_none().0) } as *mut xlib::Display;
        let xlib = Xlib::open().map_err(|error| error.to_string())?;
        Ok(Rc::new(Parent { xlib: Rc::new(xlib), display, gdk_display, xid }))
    }

    /// A new X window inside `parent` (the app's window when None), unmapped, at `rect`.
    fn child_window(&self, parent: Option<c_ulong>, rect: PixelRect) -> c_ulong {
        unsafe {
            let window = (self.xlib.XCreateSimpleWindow)(self.display, parent.unwrap_or(self.xid), rect.x, rect.y, rect.width.max(1) as u32, rect.height.max(1) as u32, 0, 0, 0);
            (self.xlib.XFlush)(self.display);
            window
        }
    }

    fn foreign(&self, xid: c_ulong) -> gdk::Window {
        unsafe { gdk::Window::from_glib_full(gdkx11::ffi::gdk_x11_window_foreign_new_for_display(self.gdk_display.to_glib_none().0, xid)) }
    }

    /// Gives the keyboard to `xid`.
    pub fn focus(&self, xid: c_ulong) {
        unsafe {
            (self.xlib.XSetInputFocus)(self.display, xid, xlib::RevertToParent, xlib::CurrentTime);
            (self.xlib.XFlush)(self.display);
        }
    }

    /// Gives the keyboard back to the app's window.
    pub fn focus_app(&self) {
        self.focus(self.xid);
    }

    /// The window that holds the keyboard.
    pub fn focused(&self) -> c_ulong {
        let (mut window, mut revert) = (0, 0);
        unsafe { (self.xlib.XGetInputFocus)(self.display, &mut window, &mut revert) };
        window
    }

    fn forward_key(&self, event: &gdk::EventKey) {
        let mut key = xlib::XKeyEvent {
            type_: if event.event_type() == gdk::EventType::KeyPress { xlib::KeyPress } else { xlib::KeyRelease },
            serial: 0,
            send_event: xlib::True,
            display: self.display,
            window: self.xid,
            root: 0,
            subwindow: 0,
            time: event.time() as xlib::Time,
            x: 0,
            y: 0,
            x_root: 0,
            y_root: 0,
            state: (event.state().bits() & 0x1fff) | (u32::from(event.group()) << 13),
            keycode: event.hardware_keycode() as u32,
            same_screen: xlib::True,
        };
        unsafe {
            key.root = (self.xlib.XDefaultRootWindow)(self.display);
            let mut sent = xlib::XEvent { key };
            let mask = if key.type_ == xlib::KeyPress { xlib::KeyPressMask } else { xlib::KeyReleaseMask };
            (self.xlib.XSendEvent)(self.display, self.xid, xlib::False, mask, &mut sent);
            (self.xlib.XFlush)(self.display);
        }
    }
}

/// The X window behind a GDK window.
pub fn xid(window: &gdk::Window) -> Option<c_ulong> {
    window.clone().downcast::<gdkx11::X11Window>().ok().map(|window| window.xid())
}

/// A widget shown in an X window of its own inside the app's window, optionally clipped by an
/// outer window that holes can be cut in.
pub struct Embedded {
    parent: Rc<Parent>,
    pub gtk: gtk::Window,
    /// The X window the widget draws in.
    pub area: c_ulong,
    /// The window that clips it, when it has one.
    clip: Option<(c_ulong, gdk::Window)>,
    shown: Cell<bool>,
    app_keys: AppKeys,
}

type AppKeys = Rc<RefCell<Option<Box<dyn Fn(&gdk::EventKey) -> bool>>>>;

impl Embedded {
    /// Puts `widget` in a new window inside the app's, hidden. With `clipped`, it sits inside a
    /// window that clips it and can have holes cut in it.
    pub fn new(parent: &Rc<Parent>, widget: &impl IsA<gtk::Widget>, clipped: bool) -> Embedded {
        let rect = PixelRect { x: 0, y: 0, width: 1, height: 1 };
        let clip = clipped.then(|| {
            let xid = parent.child_window(None, rect);
            (xid, parent.foreign(xid))
        });
        let area = parent.child_window(clip.as_ref().map(|(xid, _)| *xid), rect);
        let area_gdk = parent.foreign(area);
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        let realized = area_gdk.clone();
        // GTK made a toplevel of its own; the widget draws in `area` instead, registered as its own
        // so unrealizing it later finds it.
        window.connect_realize(move |window| {
            if let Some(made) = window.window() {
                window.unregister_window(&made);
                made.destroy();
            }
            window.set_window(realized.clone());
            window.register_window(&realized);
        });
        window.set_has_window(true);
        window.realize();
        window.add(widget);
        window.show_all();
        // Showing the widget maps its window; it stays out of sight until placed.
        unsafe {
            (parent.xlib.XUnmapWindow)(parent.display, area);
            (parent.xlib.XFlush)(parent.display);
        }
        let forward_to = parent.clone();
        let app_keys: AppKeys = Rc::default();
        let held: Rc<RefCell<Vec<u16>>> = Rc::default();
        let (keys, sent) = (app_keys.clone(), held.clone());
        // Keys reach the window under the pointer even when the app holds the keyboard; those go
        // back, as do the app's own shortcuts while the page holds it.
        let forward = move |_: &gtk::Window, event: &gdk::EventKey| {
            let code = event.hardware_keycode();
            let focused = forward_to.focused();
            let holds = focused == area || event.window().and_then(|window| xid(&window)) == Some(focused);
            let pressed = event.event_type() == gdk::EventType::KeyPress;
            let app_key = if pressed {
                let wanted = keys.borrow().as_ref().is_some_and(|wanted| wanted(event));
                if wanted {
                    sent.borrow_mut().push(code);
                }
                wanted
            } else {
                let mut sent = sent.borrow_mut();
                let had = sent.contains(&code);
                sent.retain(|held| *held != code);
                had
            };
            if holds && !app_key {
                return gtk::glib::Propagation::Proceed;
            }
            forward_to.forward_key(event);
            gtk::glib::Propagation::Stop
        };
        window.connect_key_press_event(forward.clone());
        window.connect_key_release_event(forward);
        Embedded { parent: parent.clone(), gtk: window, area, clip, shown: Cell::new(false), app_keys }
    }

    /// Which keys the app answers even while the widget holds the keyboard.
    pub fn set_app_keys(&self, wanted: impl Fn(&gdk::EventKey) -> bool + 'static) {
        *self.app_keys.borrow_mut() = Some(Box::new(wanted));
    }

    /// Shows the widget at `page`, in app window pixels. With a clip window, `clip` is the part
    /// of the window it may be seen in, and `holes` (relative to the clip) are cut out of it.
    pub fn place(&self, page: PixelRect, clip: Option<PixelRect>, holes: &[PixelRect]) {
        let (x, y) = match (&self.clip, clip) {
            (Some((xid, gdk_window)), Some(clip)) => {
                unsafe { (self.parent.xlib.XMoveResizeWindow)(self.parent.display, *xid, clip.x, clip.y, clip.width.max(1) as u32, clip.height.max(1) as u32) };
                let region = cairo::Region::create_rectangle(&cairo::RectangleInt::new(0, 0, clip.width.max(1), clip.height.max(1)));
                for hole in holes {
                    let _ = region.subtract_rectangle(&cairo::RectangleInt::new(hole.x, hole.y, hole.width, hole.height));
                }
                gdk_window.shape_combine_region(Some(&region), 0, 0);
                gdk_window.input_shape_combine_region(&region, 0, 0);
                unsafe { (self.parent.xlib.XMapWindow)(self.parent.display, *xid) };
                (page.x - clip.x, page.y - clip.y)
            }
            _ => (page.x, page.y),
        };
        unsafe {
            (self.parent.xlib.XMoveResizeWindow)(self.parent.display, self.area, x, y, page.width.max(1) as u32, page.height.max(1) as u32);
        }
        self.allocate(page.width, page.height);
        if !self.shown.replace(true) {
            unsafe { (self.parent.xlib.XMapWindow)(self.parent.display, self.area) };
        }
        unsafe { (self.parent.xlib.XFlush)(self.parent.display) };
    }

    /// Lays the widget out at `width` by `height` without showing it.
    pub fn park(&self, width: i32, height: i32) {
        self.hide();
        unsafe { (self.parent.xlib.XResizeWindow)(self.parent.display, self.area, width.max(1) as u32, height.max(1) as u32) };
        self.allocate(width, height);
    }

    /// Lays the widget out over `width` by `height` pixels, which GTK counts in its own scaled units.
    fn allocate(&self, width: i32, height: i32) {
        let scale = self.gtk.scale_factor().max(1);
        self.gtk.size_allocate(&gtk::Allocation::new(0, 0, (width / scale).max(1), (height / scale).max(1)));
    }

    pub fn hide(&self) {
        self.shown.set(false);
        unsafe { (self.parent.xlib.XUnmapWindow)(self.parent.display, self.area) };
        if let Some((xid, _)) = &self.clip {
            unsafe { (self.parent.xlib.XUnmapWindow)(self.parent.display, *xid) };
        }
        unsafe { (self.parent.xlib.XFlush)(self.parent.display) };
    }

    pub fn shown(&self) -> bool {
        self.shown.get()
    }

    /// Gives it the keyboard.
    pub fn focus(&self) {
        self.parent.focus(self.key_window());
    }

    /// The X window keys for the widget arrive on: its own, when it has one. GTK listens for keys
    /// there, while a window it wraps hears none when the pointer is elsewhere.
    fn key_window(&self) -> c_ulong {
        self.gtk.child().and_then(|child| child.window()).and_then(|window| xid(&window)).unwrap_or(self.area)
    }

    /// Whether it holds the keyboard.
    pub fn holds_keys(&self) -> bool {
        let focused = self.parent.focused();
        focused == self.area || focused == self.key_window()
    }
}

impl Drop for Embedded {
    fn drop(&mut self) {
        unsafe { self.gtk.destroy() };
        unsafe {
            (self.parent.xlib.XDestroyWindow)(self.parent.display, self.area);
            if let Some((xid, _)) = &self.clip {
                (self.parent.xlib.XDestroyWindow)(self.parent.display, *xid);
            }
            (self.parent.xlib.XFlush)(self.parent.display);
        }
    }
}
