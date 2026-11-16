//! Web views the app draws itself, for windows GTK cannot place them in (Wayland): each renders
//! in a GTK offscreen window, the panel paints the last frame it rendered, and the pointer and keys
//! the panel receives are handed to it as GTK events.

use std::cell::{Cell, RefCell};
use std::ffi::CString;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{Keystroke, Modifiers, RenderImage};
use gtk::gdk;
use gtk::gdk::ffi as gdk_ffi;
use gtk::glib;
use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;

use crate::types::{PageInput, PointerKind};

/// How often frames are taken while the page changes.
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// Frames taken after the last change the page reported.
const SETTLE_FRAMES: u32 = 8;
/// Points of smooth scrolling that make one of GTK's scroll steps.
const PIXELS_PER_LINE: f64 = 40.0;

pub struct Offscreen {
    window: gtk::OffscreenWindow,
    widget: gtk::Widget,
    frame: Rc<RefCell<Option<Arc<RenderImage>>>>,
    shown: Rc<Cell<bool>>,
    focused: Cell<bool>,
    size: Cell<(i32, i32)>,
    started: Instant,
    pressed: Cell<u32>,
    cursor: Rc<Cell<gpui::CursorStyle>>,
}

impl Offscreen {
    /// Puts `widget` in an offscreen window. `painted` runs whenever a new frame is ready while shown.
    pub fn new(widget: &impl IsA<gtk::Widget>, painted: impl Fn() + 'static) -> Offscreen {
        let window = gtk::OffscreenWindow::new();
        window.add(widget);
        window.show_all();
        let frame: Rc<RefCell<Option<Arc<RenderImage>>>> = Rc::default();
        let shown = Rc::new(Cell::new(false));
        let gathering = Rc::new(Cell::new(false));
        let painted = Rc::new(painted);
        // GTK tells of a change before the page has always drawn it, so frames keep being taken for a
        // little while after the last change; one that matches the frame before is dropped.
        let remaining = Rc::new(Cell::new(0u32));
        let schedule: Rc<dyn Fn(&gtk::OffscreenWindow)> = {
            let (frame, shown, painted) = (frame.clone(), shown.clone(), painted.clone());
            Rc::new(move |window: &gtk::OffscreenWindow| {
                if !shown.get() {
                    return;
                }
                remaining.set(SETTLE_FRAMES);
                if gathering.replace(true) {
                    return;
                }
                let (window, frame, shown, gathering, remaining, painted) = (window.clone(), frame.clone(), shown.clone(), gathering.clone(), remaining.clone(), painted.clone());
                glib::timeout_add_local_full(FRAME_INTERVAL, glib::Priority::DEFAULT_IDLE, move || {
                    if shown.get() {
                        let last = frame.borrow().clone();
                        if let Some(image) = capture(&window, last.as_deref()) {
                            *frame.borrow_mut() = Some(image);
                            painted();
                        }
                    }
                    remaining.set(remaining.get().saturating_sub(1));
                    if remaining.get() > 0 && shown.get() {
                        return glib::ControlFlow::Continue;
                    }
                    gathering.set(false);
                    glib::ControlFlow::Break
                });
            })
        };
        {
            let schedule = schedule.clone();
            window.connect_damage_event(move |window, _| {
                schedule(window);
                false
            });
        }
        window.connect_draw(move |window, _| {
            schedule(window);
            glib::Propagation::Proceed
        });
        let cursor = Rc::new(Cell::new(gpui::CursorStyle::Arrow));
        if let Some(target) = widget.as_ref().window() {
            let (cursor, painted) = (cursor.clone(), painted.clone());
            target.connect_notify_local(Some("cursor"), move |target, _| {
                let style = cursor_style(target);
                if cursor.replace(style) != style {
                    painted();
                }
            });
        }
        Offscreen { window, widget: widget.clone().upcast(), frame, shown, focused: Cell::new(false), size: Cell::new((0, 0)), started: Instant::now(), pressed: Cell::new(0), cursor }
    }

    /// Shows it at `width` by `height` device pixels.
    pub fn place(&self, width: i32, height: i32) {
        self.wake(width, height);
        if !self.shown.replace(true) {
            self.widget.queue_draw();
        }
    }

    /// Has the page draw at `width` by `height` device pixels, for a picture, without showing it.
    pub fn wake(&self, width: i32, height: i32) {
        self.resize(width, height);
        if !self.window.is_visible() {
            self.window.show();
        }
    }

    /// Lays it out at `width` by `height` device pixels, hidden, so it lays out as it would appear.
    pub fn park(&self, width: i32, height: i32) {
        self.hide();
        self.resize(width, height);
    }

    pub fn hide(&self) {
        self.shown.set(false);
        self.frame.borrow_mut().take();
        if self.window.is_visible() {
            self.window.hide();
        }
    }

    /// The widget input goes to: the first one down the tree with a window of its own (the page,
    /// when the widget holds it in a container).
    fn page(&self) -> gtk::Widget {
        let mut widget = self.widget.clone();
        while !widget.has_window() {
            let Some(child) = widget.downcast_ref::<gtk::Container>().and_then(|container| container.children().into_iter().next()) else { break };
            widget = child;
        }
        widget
    }

    /// Device pixels per GTK unit.
    pub fn gtk_scale(&self) -> i32 {
        self.window.scale_factor().max(1)
    }

    fn resize(&self, width: i32, height: i32) {
        let scale = self.gtk_scale();
        let size = ((width / scale).max(1), (height / scale).max(1));
        if self.size.replace(size) != size {
            self.widget.set_size_request(size.0, size.1);
            self.window.set_default_size(size.0, size.1);
            self.window.queue_resize();
        }
    }

    /// The page as it last drew, while shown.
    pub fn frame(&self) -> Option<Arc<RenderImage>> {
        self.frame.borrow().clone()
    }

    /// The pointer the page asked for over itself.
    pub fn cursor(&self) -> gpui::CursorStyle {
        self.cursor.get()
    }

    pub fn holds_keys(&self) -> bool {
        self.focused.get()
    }

    /// Gives the page the keyboard, or takes it away.
    pub fn focus(&self, focused: bool) {
        if self.focused.replace(focused) == focused {
            return;
        }
        if focused {
            self.page().grab_focus();
        }
        let Some(target) = self.window.window() else { return };
        unsafe {
            let event = gdk_ffi::gdk_event_new(gdk_ffi::GDK_FOCUS_CHANGE);
            let focus = &mut *(event as *mut gdk_ffi::GdkEventFocus);
            focus.window = target.to_glib_full();
            focus.send_event = 1;
            focus.in_ = i16::from(focused);
            if let Some(keyboard) = target.display().default_seat().and_then(|seat| seat.keyboard()) {
                gdk_ffi::gdk_event_set_device(event, keyboard.to_glib_none().0);
            }
            dispatch(event);
        }
    }

    fn time(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    /// Hands the page input the panel received. Positions are in device pixels from the page's top left.
    pub fn input(&self, input: &PageInput) {
        let Some(target) = self.page().window() else { return };
        let scale = f64::from(self.gtk_scale());
        let device = gdk::Display::default().and_then(|display| display.default_seat()).and_then(|seat| seat.pointer());
        match input {
            PageInput::Pointer { kind, x, y, button, modifiers } => unsafe {
                let (x, y) = (x / scale, y / scale);
                let event = match kind {
                    PointerKind::Move => {
                        let event = gdk_ffi::gdk_event_new(gdk_ffi::GDK_MOTION_NOTIFY);
                        let motion = &mut *(event as *mut gdk_ffi::GdkEventMotion);
                        motion.window = target.to_glib_full();
                        motion.send_event = 1;
                        motion.time = self.time();
                        (motion.x, motion.y, motion.x_root, motion.y_root) = (x, y, x, y);
                        motion.state = pointer_state(*modifiers, self.pressed.get());
                        event
                    }
                    PointerKind::Down | PointerKind::Up => {
                        let down = *kind == PointerKind::Down;
                        let event = gdk_ffi::gdk_event_new(if down { gdk_ffi::GDK_BUTTON_PRESS } else { gdk_ffi::GDK_BUTTON_RELEASE });
                        let press = &mut *(event as *mut gdk_ffi::GdkEventButton);
                        press.window = target.to_glib_full();
                        press.send_event = 1;
                        press.time = self.time();
                        (press.x, press.y, press.x_root, press.y_root) = (x, y, x, y);
                        press.state = pointer_state(*modifiers, self.pressed.get());
                        press.button = *button;
                        let bit = 1 << button.min(&5);
                        self.pressed.set(if down { self.pressed.get() | bit } else { self.pressed.get() & !bit });
                        event
                    }
                };
                if let Some(device) = &device {
                    gdk_ffi::gdk_event_set_device(event, device.to_glib_none().0);
                }
                dispatch(event);
            },
            PageInput::Scroll { x, y, delta, modifiers } => unsafe {
                let (dx, dy) = match delta {
                    gpui::ScrollDelta::Lines(lines) => (-f64::from(lines.x), -f64::from(lines.y)),
                    gpui::ScrollDelta::Pixels(pixels) => (-f64::from(pixels.x) / PIXELS_PER_LINE, -f64::from(pixels.y) / PIXELS_PER_LINE),
                };
                let event = gdk_ffi::gdk_event_new(gdk_ffi::GDK_SCROLL);
                let scroll = &mut *(event as *mut gdk_ffi::GdkEventScroll);
                scroll.window = target.to_glib_full();
                scroll.send_event = 1;
                scroll.time = self.time();
                (scroll.x, scroll.y, scroll.x_root, scroll.y_root) = (x / scale, y / scale, x / scale, y / scale);
                scroll.state = modifier_state(*modifiers);
                scroll.direction = gdk_ffi::GDK_SCROLL_SMOOTH;
                (scroll.delta_x, scroll.delta_y) = (dx, dy);
                if let Some(device) = &device {
                    gdk_ffi::gdk_event_set_device(event, device.to_glib_none().0);
                }
                dispatch(event);
            },
            PageInput::Leave => unsafe {
                let event = gdk_ffi::gdk_event_new(gdk_ffi::GDK_LEAVE_NOTIFY);
                let crossing = &mut *(event as *mut gdk_ffi::GdkEventCrossing);
                crossing.window = target.to_glib_full();
                crossing.send_event = 1;
                crossing.time = self.time();
                (crossing.x, crossing.y) = (-1.0, -1.0);
                if let Some(device) = &device {
                    gdk_ffi::gdk_event_set_device(event, device.to_glib_none().0);
                }
                dispatch(event);
            },
            PageInput::Key { down, keystroke } => {
                if self.focused.get() {
                    self.key(&target, *down, keystroke);
                }
            }
            PageInput::Focus(focused) => self.focus(*focused),
        }
    }

    fn key(&self, target: &gdk::Window, down: bool, keystroke: &Keystroke) {
        let Some((keyval, text)) = keyval(keystroke) else { return };
        let display = target.display();
        let keyboard = display.default_seat().and_then(|seat| seat.keyboard());
        let code = gdk::Keymap::for_display(&display).and_then(|keymap| keymap.entries_for_keyval(keyval).first().map(|entry| entry.keycode())).unwrap_or(0);
        unsafe {
            let event = gdk_ffi::gdk_event_new(if down { gdk_ffi::GDK_KEY_PRESS } else { gdk_ffi::GDK_KEY_RELEASE });
            let key = &mut *(event as *mut gdk_ffi::GdkEventKey);
            key.window = target.to_glib_full();
            key.send_event = 1;
            key.time = self.time();
            key.state = modifier_state(keystroke.modifiers);
            key.keyval = keyval;
            key.hardware_keycode = code as u16;
            if let Some(text) = text.filter(|_| down).and_then(|text| CString::new(text).ok()) {
                key.length = text.as_bytes().len() as i32;
                key.string = glib::ffi::g_strdup(text.as_ptr());
            }
            if let Some(keyboard) = &keyboard {
                gdk_ffi::gdk_event_set_device(event, keyboard.to_glib_none().0);
            }
            dispatch(event);
        }
    }
}

impl Drop for Offscreen {
    fn drop(&mut self) {
        unsafe { self.window.destroy() };
    }
}

unsafe fn dispatch(event: *mut gdk_ffi::GdkEvent) {
    gtk::ffi::gtk_main_do_event(event);
    gdk_ffi::gdk_event_free(event);
}

fn modifier_state(modifiers: Modifiers) -> gdk_ffi::GdkModifierType {
    let mut state = 0;
    if modifiers.shift {
        state |= gdk_ffi::GDK_SHIFT_MASK;
    }
    if modifiers.control {
        state |= gdk_ffi::GDK_CONTROL_MASK;
    }
    if modifiers.alt {
        state |= gdk_ffi::GDK_MOD1_MASK;
    }
    if modifiers.platform {
        state |= gdk_ffi::GDK_SUPER_MASK;
    }
    state
}

/// The pointer GPUI draws for the cursor WebKit set on `window`. GDK hands out one cursor per name,
/// so the cursor is known by which named one it is.
fn cursor_style(window: &gdk::Window) -> gpui::CursorStyle {
    use gpui::CursorStyle;
    let Some(cursor) = window.cursor() else { return CursorStyle::Arrow };
    let display = window.display();
    const NAMES: [(&str, CursorStyle); 14] = [
        ("pointer", CursorStyle::PointingHand),
        ("text", CursorStyle::IBeam),
        ("vertical-text", CursorStyle::IBeamCursorForVerticalLayout),
        ("crosshair", CursorStyle::Crosshair),
        ("grab", CursorStyle::OpenHand),
        ("grabbing", CursorStyle::ClosedHand),
        ("not-allowed", CursorStyle::OperationNotAllowed),
        ("no-drop", CursorStyle::OperationNotAllowed),
        ("col-resize", CursorStyle::ResizeColumn),
        ("row-resize", CursorStyle::ResizeRow),
        ("ew-resize", CursorStyle::ResizeLeftRight),
        ("ns-resize", CursorStyle::ResizeUpDown),
        ("copy", CursorStyle::DragCopy),
        ("alias", CursorStyle::DragLink),
    ];
    NAMES
        .iter()
        .find(|(name, _)| gdk::Cursor::from_name(&display, name).is_some_and(|named| named == cursor))
        .map(|(_, style)| *style)
        .unwrap_or(CursorStyle::Arrow)
}

/// Modifiers plus the buttons held, as bits 1 << button.
fn pointer_state(modifiers: Modifiers, pressed: u32) -> gdk_ffi::GdkModifierType {
    let mut state = modifier_state(modifiers);
    for (button, mask) in [(1, gdk_ffi::GDK_BUTTON1_MASK), (2, gdk_ffi::GDK_BUTTON2_MASK), (3, gdk_ffi::GDK_BUTTON3_MASK)] {
        if pressed & (1 << button) != 0 {
            state |= mask;
        }
    }
    state
}

/// The GDK key a keystroke is, and the text it types.
fn keyval(keystroke: &Keystroke) -> Option<(u32, Option<String>)> {
    let named = match keystroke.key.as_str() {
        "enter" => Some(0xff0d),
        "backspace" => Some(0xff08),
        "tab" => Some(if keystroke.modifiers.shift { 0xfe20 } else { 0xff09 }),
        "escape" => Some(0xff1b),
        "delete" => Some(0xffff),
        "insert" => Some(0xff63),
        "left" => Some(0xff51),
        "up" => Some(0xff52),
        "right" => Some(0xff53),
        "down" => Some(0xff54),
        "home" => Some(0xff50),
        "end" => Some(0xff57),
        "pageup" => Some(0xff55),
        "pagedown" => Some(0xff56),
        "space" => Some(0x20),
        key => key.strip_prefix('f').and_then(|number| number.parse::<u32>().ok()).filter(|number| (1..=24).contains(number)).map(|number| 0xffbe + number - 1),
    };
    if let Some(keyval) = named {
        let text = (keyval == 0x20).then(|| " ".to_string());
        return Some((keyval, text));
    }
    let typed = keystroke.key_char.clone().filter(|text| !text.is_empty() && !text.chars().any(char::is_control));
    let character = typed.as_deref().unwrap_or(&keystroke.key).chars().next()?;
    if keystroke.key.chars().count() != 1 && typed.is_none() {
        return None;
    }
    let keyval = unsafe { gdk_ffi::gdk_unicode_to_keyval(character as u32) };
    Some((keyval, typed))
}

/// The window's last drawing as the BGRA frame GPUI paints, or None when it is `last` again.
fn capture(window: &gtk::OffscreenWindow, last: Option<&RenderImage>) -> Option<Arc<RenderImage>> {
    let surface = window.surface()?;
    let scale = window.scale_factor().max(1);
    let (width, height) = (window.allocated_width() * scale, window.allocated_height() * scale);
    if width <= 1 || height <= 1 {
        return None;
    }
    let mut image = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    image.set_device_scale(f64::from(scale), f64::from(scale));
    {
        let context = cairo::Context::new(&image).ok()?;
        context.set_source_surface(&surface, 0.0, 0.0).ok()?;
        context.set_operator(cairo::Operator::Source);
        context.paint().ok()?;
    }
    image.flush();
    let stride = image.stride() as usize;
    let data = image.data().ok()?;
    let row = width as usize * 4;
    let mut pixels = Vec::with_capacity(row * height as usize);
    for line in data.chunks(stride).take(height as usize) {
        pixels.extend_from_slice(&line[..row]);
    }
    if last.and_then(|last| last.as_bytes(0)) == Some(pixels.as_slice()) {
        return None;
    }
    let buffer = image::RgbaImage::from_raw(width as u32, height as u32, pixels)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}
