//! The native half of a visual on Linux: an X window inside the app's, clipping the WebKit page a
//! visual runs in to what of its row is in view, with holes where the app's own controls sit over it.
//! A page that does not scroll itself hands the wheel to the transcript under it. On Wayland the page
//! renders off screen instead, and its row paints its frames and hands it the pointer.

use std::cell::Cell;
use std::rc::Rc;

use act_browser::linux::embed::{Embedded, Parent, PixelRect};
use act_browser::linux::offscreen::Offscreen;
use act_browser::{PageInput, PointerKind};
use gpui::{
    point, px, App, AppContext as _, Bounds, DispatchPhase, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, RenderImage,
    ScrollDelta, ScrollEvent, ScrollSource, TouchPhase, Window,
};
use gtk::prelude::*;

/// A wheel turn over a page, in window points, for the transcript under it.
type Wheel = (gpui::AnyWindowHandle, f32, f32, f32, f32);

thread_local! {
    static PARENTS: std::cell::RefCell<Vec<(gpui::AnyWindowHandle, Rc<Parent>)>> = const { std::cell::RefCell::new(Vec::new()) };
    static WHEELS: std::cell::RefCell<Option<futures::channel::mpsc::UnboundedSender<Wheel>>> = const { std::cell::RefCell::new(None) };
    static REDRAWS: std::cell::RefCell<Option<futures::channel::mpsc::UnboundedSender<()>>> = const { std::cell::RefCell::new(None) };
}

enum Host {
    Window(Embedded),
    Painted(Rc<Painted>),
}

/// A page rendered off screen, which its row paints.
struct Painted {
    offscreen: Offscreen,
    /// The frame last painted, given back to GPUI once replaced.
    drawn: std::cell::RefCell<Option<std::sync::Arc<RenderImage>>>,
    zoom: Cell<f64>,
    pressed: Cell<bool>,
    focused: Cell<bool>,
}

/// Has every window drawn again, for a page that drew a new frame.
fn redraws(cx: &mut App) -> futures::channel::mpsc::UnboundedSender<()> {
    if let Some(sender) = REDRAWS.with(|redraws| redraws.borrow().clone()) {
        return sender;
    }
    let (sender, mut receiver) = futures::channel::mpsc::unbounded::<()>();
    cx.spawn(async move |cx| {
        use futures::StreamExt;
        while receiver.next().await.is_some() {
            while receiver.try_recv().is_ok() {}
            let _ = cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
    REDRAWS.with(|redraws| *redraws.borrow_mut() = Some(sender.clone()));
    sender
}

pub struct ClipView {
    host: Host,
    /// The app's X11 window, which a clicked page takes the keyboard from.
    parent: Option<Rc<Parent>>,
    container: gtk::Box,
    scrolls: Rc<Cell<bool>>,
    /// Device pixels per window point.
    scale: Rc<Cell<f64>>,
    origin: Rc<Cell<(i32, i32)>>,
    _accessible: act_browser::linux::gtk_loop::AccessibleHold,
}

fn parent(window: &Window) -> Option<Rc<Parent>> {
    let handle = window.window_handle();
    if let Some(parent) = PARENTS.with(|parents| parents.borrow().iter().find(|(known, _)| *known == handle).map(|(_, parent)| parent.clone())) {
        return Some(parent);
    }
    let parent = Parent::of(window).ok()?;
    PARENTS.with(|parents| parents.borrow_mut().push((handle, parent.clone())));
    Some(parent)
}

fn wheels(cx: &mut App) -> futures::channel::mpsc::UnboundedSender<Wheel> {
    if let Some(sender) = WHEELS.with(|wheels| wheels.borrow().clone()) {
        return sender;
    }
    let (sender, mut receiver) = futures::channel::mpsc::unbounded::<Wheel>();
    cx.spawn(async move |cx| {
        use futures::StreamExt;
        while let Some((handle, x, y, dx, dy)) = receiver.next().await {
            let _ = cx.update_window(handle, |_, window, cx| {
                let event = ScrollEvent { position: point(px(x), px(y)), delta: ScrollDelta::Lines(point(dx, dy)), modifiers: Modifiers::default(), touch_phase: TouchPhase::Moved, source: ScrollSource::Pointer };
                window.dispatch_event(PlatformInput::Scroll(event), cx);
            });
        }
    })
    .detach();
    WHEELS.with(|wheels| *wheels.borrow_mut() = Some(sender.clone()));
    sender
}

impl ClipView {
    /// A hidden clip inside `window`, ready for a page to be built into `container()`.
    pub fn attach(window: &mut Window, cx: &mut App) -> Option<ClipView> {
        act_browser::linux::gtk_loop::ensure(cx).ok()?;
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scrolls = Rc::new(Cell::new(false));
        let scale = Rc::new(Cell::new(f64::from(window.scale_factor())));
        let origin = Rc::new(Cell::new((0, 0)));
        if !act_browser::linux::gtk_loop::on_x11() {
            let redraw = redraws(cx);
            let offscreen = Offscreen::new(&container, move || {
                let _ = redraw.unbounded_send(());
            });
            let painted = Rc::new(Painted { offscreen, drawn: Default::default(), zoom: Cell::new(0.0), pressed: Cell::new(false), focused: Cell::new(false) });
            return Some(ClipView { host: Host::Painted(painted), parent: None, container, scrolls, scale, origin, _accessible: act_browser::linux::gtk_loop::AccessibleHold::new() });
        }
        let parent = parent(window)?;
        let host = Embedded::new(&parent, &container, true);
        let (sender, handle) = (wheels(cx), window.window_handle());
        let (wheel_scrolls, wheel_scale, wheel_origin) = (scrolls.clone(), scale.clone(), origin.clone());
        host.gtk.add_events(gtk::gdk::EventMask::SCROLL_MASK | gtk::gdk::EventMask::SMOOTH_SCROLL_MASK);
        host.gtk.connect_scroll_event(move |_, event| {
            if wheel_scrolls.get() {
                return gtk::glib::Propagation::Proceed;
            }
            let (dx, dy) = match event.direction() {
                gtk::gdk::ScrollDirection::Up => (0.0, 1.0),
                gtk::gdk::ScrollDirection::Down => (0.0, -1.0),
                gtk::gdk::ScrollDirection::Left => (1.0, 0.0),
                gtk::gdk::ScrollDirection::Right => (-1.0, 0.0),
                _ => {
                    let (dx, dy) = event.delta();
                    (-dx, -dy)
                }
            };
            let (x, y) = event.position();
            let (left, top) = wheel_origin.get();
            let scale = wheel_scale.get().max(0.5);
            let zoom = f64::from(gpui::page_zoom()).max(0.1);
            let at = (((left as f64 + x) / scale) / zoom, ((top as f64 + y) / scale) / zoom);
            let _ = sender.unbounded_send((handle, at.0 as f32, at.1 as f32, (dx * 3.0) as f32, (dy * 3.0) as f32));
            gtk::glib::Propagation::Stop
        });
        Some(ClipView { host: Host::Window(host), parent: Some(parent), container, scrolls, scale, origin, _accessible: act_browser::linux::gtk_loop::AccessibleHold::new() })
    }

    pub fn container(&self) -> &gtk::Box {
        &self.container
    }

    /// Places the clip at `clip` and the page inside it at `page`, both in window points from the
    /// top left. `holes` are in the page's own points. A page not `shown` lays out out of sight.
    pub fn place(&self, scale: f64, clip: (f64, f64, f64, f64), page: (f64, f64, f64, f64), holes: &[(f64, f64, f64, f64)], shown: bool) {
        self.scale.set(scale);
        let host = match &self.host {
            Host::Window(host) => host,
            Host::Painted(painted) => {
                let (width, height) = ((page.2 * scale).round() as i32, (page.3 * scale).round() as i32);
                let zoom = f64::from(gpui::page_zoom()) * scale / f64::from(painted.offscreen.gtk_scale());
                if (painted.zoom.replace(zoom) - zoom).abs() > f64::EPSILON {
                    if let Some(view) = self.container.children().first().and_then(|child| child.clone().downcast::<webkit2gtk::WebView>().ok()) {
                        webkit2gtk::WebViewExt::set_zoom_level(&view, zoom);
                    }
                }
                if shown {
                    painted.offscreen.place(width, height);
                } else {
                    painted.offscreen.wake(width, height);
                }
                return;
            }
        };
        let pixels = |rect: (f64, f64, f64, f64)| PixelRect { x: (rect.0 * scale).round() as i32, y: (rect.1 * scale).round() as i32, width: (rect.2 * scale).round() as i32, height: (rect.3 * scale).round() as i32 };
        let (mut clip, mut page) = (pixels(clip), pixels(page));
        if !shown {
            let (dx, dy) = (-30_000 - clip.x, -30_000 - clip.y);
            clip.x += dx;
            clip.y += dy;
            page.x += dx;
            page.y += dy;
        }
        let holes: Vec<PixelRect> = holes
            .iter()
            .map(|hole| PixelRect { x: page.x - clip.x + (hole.0 * scale).round() as i32, y: page.y - clip.y + (hole.1 * scale).round() as i32, width: (hole.2 * scale).round() as i32, height: (hole.3 * scale).round() as i32 })
            .filter(|hole| hole.width > 0 && hole.height > 0)
            .collect();
        self.origin.set((page.x, page.y));
        host.place(page, Some(clip), &holes);
    }

    /// Paints a page rendered off screen over `page` and hands it the pointer that lands on the part
    /// of it in view, `visible`. Bounds are in the window's layout pixels. Nothing to do for an X window.
    pub fn paint(&self, page: Bounds<Pixels>, visible: Bounds<Pixels>, focus: &gpui::FocusHandle, window: &mut Window) {
        let Host::Painted(painted) = &self.host else { return };
        let frame = painted.offscreen.frame();
        let replaced = painted.drawn.replace(frame.clone());
        if let Some(replaced) = replaced.filter(|replaced| frame.as_ref().is_none_or(|frame| frame.id != replaced.id)) {
            let _ = window.drop_image(replaced);
        }
        if let Some(frame) = frame {
            window.with_content_mask(Some(gpui::ContentMask { bounds: visible }), |window| {
                let _ = window.paint_image(page, gpui::Corners::default(), frame, 0, false);
            });
        }
        let scale = f64::from(gpui::page_zoom()) * f64::from(window.scale_factor());
        let at = move |position: gpui::Point<Pixels>| (f64::from(position.x - page.origin.x) * scale, f64::from(position.y - page.origin.y) * scale);
        let button_of = |button: MouseButton| match button {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 3,
            MouseButton::Navigate(_) => 1,
        };
        let pointer = move |kind: PointerKind, position, button, modifiers| {
            let (x, y) = at(position);
            PageInput::Pointer { kind, x, y, button, modifiers }
        };
        {
            let (painted, focus) = (painted.clone(), focus.clone());
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let inside = visible.contains(&event.position);
                if inside != painted.focused.replace(inside) {
                    painted.offscreen.input(&PageInput::Focus(inside));
                }
                if inside {
                    // After the rows under it have handled the press, which may take focus themselves.
                    let focus = focus.clone();
                    window.defer(cx, move |window, _| window.focus(&focus, cx));
                    painted.pressed.set(true);
                    painted.offscreen.input(&pointer(PointerKind::Down, event.position, button_of(event.button), event.modifiers));
                }
            });
        }
        {
            let painted = painted.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, _| {
                if phase == DispatchPhase::Bubble && (painted.pressed.replace(false) || visible.contains(&event.position)) {
                    painted.offscreen.input(&pointer(PointerKind::Up, event.position, button_of(event.button), event.modifiers));
                }
            });
        }
        {
            let painted = painted.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, _| {
                if phase == DispatchPhase::Bubble && (visible.contains(&event.position) || painted.pressed.get()) {
                    painted.offscreen.input(&pointer(PointerKind::Move, event.position, event.pressed_button.map(button_of).unwrap_or(1), event.modifiers));
                }
            });
        }
        let (painted, scrolls) = (painted.clone(), self.scrolls.clone());
        window.on_mouse_event(move |event: &ScrollEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && scrolls.get() && visible.contains(&event.position) {
                let (x, y) = at(event.position);
                painted.offscreen.input(&PageInput::Scroll { x, y, delta: event.delta, modifiers: event.modifiers });
                cx.stop_propagation();
            }
        });
    }

    pub fn set_scrolls(&self, scrolls: bool) {
        self.scrolls.set(scrolls);
    }

    /// Readies the page built in `container()` for input: in an X window a press gives it the
    /// keyboard; a page the app paints hears keys from the app, not from the desktop's input method.
    pub fn connect_page(&self, view: &webkit2gtk::WebView) {
        use webkit2gtk::WebViewExt;
        match &self.parent {
            Some(parent) => {
                let parent = parent.clone();
                view.connect_button_press_event(move |view, _| {
                    if let Some(window) = view.window().and_then(|window| act_browser::linux::embed::xid(&window)) {
                        parent.focus(window, cx);
                    }
                    view.grab_focus();
                    gtk::glib::Propagation::Proceed
                });
            }
            None => view.set_input_method_context(None::<&webkit2gtk::InputMethodContext>),
        }
    }

    /// Whether the app paints this page.
    pub fn painted(&self) -> bool {
        matches!(self.host, Host::Painted(_))
    }

    /// Hands a key to a page the app paints, when it holds the keyboard.
    pub fn key(&self, down: bool, keystroke: &gpui::Keystroke) {
        if let Host::Painted(painted) = &self.host {
            painted.offscreen.input(&PageInput::Key { down, keystroke: keystroke.clone() });
        }
    }

    pub fn hide(&self) {
        match &self.host {
            Host::Window(host) => host.hide(),
            Host::Painted(painted) => painted.offscreen.hide(),
        }
    }

    pub fn detach(&self) {
        self.hide();
    }
}
