//! The tabs themselves. Only the page on screen is ever a child of the app's window; the rest wait
//! in a window of their own that is never shown, which gives them a viewport to lay out in without
//! putting anything on screen or taking the keyboard from the app.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use act_core::domain::find::FindResults;
use act_core::domain::platform::{BrowserAction, BrowserPermissions};
use act_core::events::NavigationRequest;
use act_host::tools::types::{BrowserElement, BrowserReadResult, BrowserShot, BrowserSnapshot};
use block2::RcBlock;
use futures::channel::mpsc::UnboundedSender;
use gpui::BackgroundExecutor;
use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSBackingStoreType, NSEvent, NSEventMask, NSEventType, NSView, NSWindow, NSWindowStyleMask};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSUUID};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration, WKWebsiteDataStore};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::raw_window_handle::{AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use wry::{NewWindowFeatures, NewWindowResponse, PermissionResponse, Rect, WebView, WebViewBuilder, WebViewBuilderExtMacos, WebViewExtMacOS};

use super::delegate::{failure_message, PageDelegate, PropertyObserver, Signal, SignalHandler};
use super::webkit;
use crate::diagnostics::{console_rank, newest, Diagnostics, MAX_DIAGNOSTIC_ENTRIES};
use crate::import::{cookie_on_site, CookieDetails};
use crate::permissions::{is_web_url, origin_of, Permissions, SCRIPT_APPROVAL_ERROR};
use crate::scripts::{action_script, count_matches_script, guarded_script, page_condition_script, snapshot_script, DIAGNOSTICS_SCRIPT, MOUSE_NAVIGATION_SCRIPT};
use crate::shots::{png_size, Shots};
use crate::types::{now_ms, page_event, BrowserEvent, Inspection, PageBounds};

/// The panel's one website data store. Every tab opens it, so they share one session.
const DATA_STORE: [u8; 16] = *b"aicodingtool-brw";
const DEFAULT_TEXT_LIMIT: f64 = 4_000.0;
/// A page nobody is looking at still lays itself out, so it is given a window's worth of room.
const PARKED_VIEWPORT: (f64, f64) = (1_200.0, 800.0);
/// A long page is cut off here rather than refused.
const MAX_SHOT_POINTS: f64 = 8_000.0;
/// How long a load asked for may take to start before the tab counts as settled without it.
const LOAD_START_GRACE: Duration = Duration::from_millis(1_500);
/// How long an action's own navigation may take to begin.
const ACTION_LOAD_GRACE: Duration = Duration::from_millis(300);
const POPUP_SIZE: (f64, f64) = (560.0, 720.0);
const PAGE_CHANGED: &str = "The page changed while it was being read. Try again.";

/// What decides which page a navigation may reach: the run driving the tab, if one is.
struct Owner {
    id: String,
    task_id: RefCell<Option<String>>,
}

/// What a tab's WebKit callbacks need, kept apart from the tab so they never hold it.
struct TabShared {
    owner: Rc<Owner>,
    epoch: Cell<u64>,
    /// When a load was asked for that WebKit has yet to start.
    requested: Cell<Option<Instant>>,
    closed: Cell<bool>,
    main_request: RefCell<Option<(String, i64, String)>>,
}

struct Popup {
    window: Retained<NSWindow>,
    webview: Retained<WKWebView>,
    _delegate: Retained<PageDelegate>,
    owner: Rc<Owner>,
}

#[derive(Clone, Default)]
struct FindState {
    query: String,
    index: i64,
    matches: i64,
}

struct Tab {
    id: String,
    shared: Rc<TabShared>,
    /// Owns the view; dropping it takes the view out of whichever window holds it.
    _webview: WebView,
    wk: Retained<WKWebView>,
    _delegate: Retained<PageDelegate>,
    observer: Retained<PropertyObserver>,
    diagnostics: Rc<RefCell<Diagnostics>>,
    popups: Rc<RefCell<Vec<Popup>>>,
    shown: Cell<bool>,
    find: RefCell<FindState>,
}

impl Tab {
    fn loading(&self) -> bool {
        let requested = self.shared.requested.get().is_some_and(|at| at.elapsed() < LOAD_START_GRACE);
        (unsafe { self.wk.isLoading() }) || requested
    }

    fn url(&self) -> String {
        webkit::current_url(&self.wk)
    }

    fn title(&self) -> String {
        webkit::current_title(&self.wk)
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        self.observer.detach();
    }
}

struct Inner {
    mtm: MainThreadMarker,
    host_view: Retained<NSView>,
    parking: RefCell<Option<Retained<NSWindow>>>,
    permissions: Rc<RefCell<Permissions>>,
    tabs: RefCell<Vec<Rc<Tab>>>,
    active: RefCell<Option<String>>,
    bounds: Cell<Option<PageBounds>>,
    parked: Cell<(f64, f64)>,
    shots: RefCell<Shots>,
    events: UnboundedSender<BrowserEvent>,
    executor: BackgroundExecutor,
    user_agent: String,
    monitor: RefCell<Option<Retained<objc2::runtime::AnyObject>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(monitor) = self.monitor.borrow_mut().take() {
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        for tab in self.tabs.borrow_mut().drain(..) {
            close_popups(&tab);
            tab.shared.closed.set(true);
        }
        if let Some(parking) = self.parking.borrow_mut().take() {
            parking.close();
        }
    }
}

pub struct PagesOptions {
    /// The view the app draws into, which the page on screen is placed over.
    pub host_view: NonNull<c_void>,
    pub executor: BackgroundExecutor,
    pub events: UnboundedSender<BrowserEvent>,
}

/// Every tab the panel holds, and the one place that drives WebKit for them.
#[derive(Clone)]
pub struct Pages {
    inner: Rc<Inner>,
}

struct ViewHandle(NonNull<c_void>);

impl HasWindowHandle for ViewHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(self.0))) })
    }
}

fn send(events: &UnboundedSender<BrowserEvent>, event: BrowserEvent) {
    let _ = events.unbounded_send(event);
}

fn state_event(id: &str, webview: &WKWebView) -> act_core::events::BrowserPageEvent {
    let mut event = page_event(id);
    event.url = Some(webkit::current_url(webview));
    event.title = Some(webkit::current_title(webview));
    event.can_go_back = Some(unsafe { webview.canGoBack() });
    event.can_go_forward = Some(unsafe { webview.canGoForward() });
    event
}

/// Whether a navigation may happen. One a run may not make yet is put to the user instead.
fn allow_navigation(permissions: &RefCell<Permissions>, owner: &Owner, url: &str, events: &UnboundedSender<BrowserEvent>) -> bool {
    if !is_web_url(url) {
        return false;
    }
    let task_id = owner.task_id.borrow().clone();
    if permissions.borrow().permitted(task_id.as_deref(), url) {
        return true;
    }
    if let Some(task_id) = task_id {
        let mut event = page_event(&owner.id);
        event.loading = Some(false);
        event.navigation_request = Some(NavigationRequest { task_id, url: url.to_string() });
        send(events, BrowserEvent::Page(event));
    }
    false
}

fn close_popups(tab: &Tab) {
    for popup in tab.popups.borrow_mut().drain(..) {
        popup.window.close();
    }
}

fn data_store(mtm: MainThreadMarker) -> Retained<WKWebsiteDataStore> {
    unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&NSUUID::from_bytes(DATA_STORE), mtm) }
}

/// Every tab's configuration: the panel's one session, and windows a page opens answered by the
/// panel's own checks rather than blocked outright.
fn configuration(mtm: MainThreadMarker) -> Retained<WKWebViewConfiguration> {
    let configuration = unsafe { WKWebViewConfiguration::new(mtm) };
    unsafe {
        configuration.setWebsiteDataStore(&data_store(mtm));
        configuration.preferences().setJavaScriptCanOpenWindowsAutomatically(true);
    }
    configuration
}

fn same(left: &NSWindow, right: &NSWindow) -> bool {
    std::ptr::eq(left, right)
}

impl Pages {
    pub fn new(options: PagesOptions) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("The browser runs on the main thread.")?;
        let host_view = unsafe { Retained::retain(options.host_view.as_ptr().cast::<NSView>()) }.ok_or("The window has no view to draw pages over.")?;
        let inner = Rc::new(Inner {
            mtm,
            host_view,
            parking: RefCell::new(None),
            permissions: Rc::new(RefCell::new(Permissions::default())),
            tabs: RefCell::new(Vec::new()),
            active: RefCell::new(None),
            bounds: Cell::new(None),
            parked: Cell::new(PARKED_VIEWPORT),
            shots: RefCell::new(Shots::default()),
            events: options.events,
            executor: options.executor,
            user_agent: crate::identity::user_agent(),
            monitor: RefCell::new(None),
        });
        let pages = Pages { inner };
        pages.watch_input();
        Ok(pages)
    }

    /// A press or key in a page is the user taking it over, so a run driving it stops being its owner.
    fn watch_input(&self) {
        let weak: Weak<Inner> = Rc::downgrade(&self.inner);
        let mask = NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown | NSEventMask::KeyDown;
        let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            if let Some(inner) = weak.upgrade() {
                Pages { inner }.saw_input(unsafe { event.as_ref() });
            }
            event.as_ptr()
        });
        let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
        *self.inner.monitor.borrow_mut() = monitor;
    }

    fn saw_input(&self, event: &NSEvent) {
        let Some(window) = event.window(self.inner.mtm) else { return };
        let Ok(tabs) = self.inner.tabs.try_borrow() else { return };
        let kind = event.r#type();
        if kind == NSEventType::KeyDown {
            let Some(responder) = window.firstResponder() else { return };
            let Ok(view) = responder.downcast::<NSView>() else { return };
            for tab in tabs.iter() {
                if view.isDescendantOf(&tab.wk) {
                    *tab.shared.owner.task_id.borrow_mut() = None;
                }
                for popup in tab.popups.borrow().iter() {
                    if view.isDescendantOf(&popup.webview) {
                        *popup.owner.task_id.borrow_mut() = None;
                    }
                }
            }
            return;
        }
        let mut in_page = false;
        for tab in tabs.iter() {
            for popup in tab.popups.borrow().iter() {
                if same(&popup.window, &window) {
                    *popup.owner.task_id.borrow_mut() = None;
                }
            }
            if !tab.shown.get() || tab.wk.window().is_none_or(|home| !same(&home, &window)) {
                continue;
            }
            let point = tab.wk.convertPoint_fromView(event.locationInWindow(), None);
            let frame = tab.wk.bounds();
            let inside = point.x >= frame.origin.x && point.y >= frame.origin.y && point.x < frame.origin.x + frame.size.width && point.y < frame.origin.y + frame.size.height;
            if inside && !tab.wk.isHidden() {
                in_page = true;
                *tab.shared.owner.task_id.borrow_mut() = None;
                if kind == NSEventType::LeftMouseDown {
                    send(&self.inner.events, BrowserEvent::PageFocused { tab_id: tab.id.clone() });
                }
            }
        }
        // The app's view never asks for the keyboard back on a press, so it is handed back here.
        if !in_page && same(&window, &self.host_window().unwrap_or_else(|| window.clone())) {
            drop(tabs);
            self.focus_host();
        }
    }

    fn host_window(&self) -> Option<Retained<NSWindow>> {
        self.inner.host_view.window()
    }

    /// Gives the keyboard back to the app's own view when a page holds it.
    pub fn focus_host(&self) {
        let Some(window) = self.host_window() else { return };
        let Some(responder) = window.firstResponder() else { return };
        let Ok(view) = responder.downcast::<NSView>() else { return };
        let in_page = self.inner.tabs.borrow().iter().any(|tab| view.isDescendantOf(&tab.wk));
        if in_page {
            window.makeFirstResponder(Some(&self.inner.host_view));
        }
    }

    fn parking_window(&self) -> Retained<NSWindow> {
        if let Some(parking) = self.inner.parking.borrow().as_ref() {
            return parking.clone();
        }
        let (width, height) = PARKED_VIEWPORT;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                self.inner.mtm.alloc(),
                NSRect::new(NSPoint::new(-20_000.0, -20_000.0), NSSize::new(width, height)),
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        *self.inner.parking.borrow_mut() = Some(window.clone());
        window
    }

    fn tab(&self, tab_id: &str) -> Option<Rc<Tab>> {
        self.inner.tabs.borrow().iter().find(|tab| tab.id == tab_id).cloned()
    }

    fn live(&self, tab: &Tab) -> bool {
        !tab.shared.closed.get() && self.tab(&tab.id).is_some_and(|current| Rc::ptr_eq(&current.shared, &tab.shared))
    }

    pub fn configure_permissions(&self, permissions: &BrowserPermissions) {
        self.inner.permissions.borrow_mut().configure(permissions);
    }

    fn require_page(&self, tab: &Tab, task_id: Option<&str>) -> Result<(), String> {
        self.inner.permissions.borrow().require(task_id, &tab.url())
    }

    fn same_page(&self, tab: &Tab, epoch: u64, task_id: Option<&str>) -> Result<(), String> {
        self.require_page(tab, task_id)?;
        if tab.shared.epoch.get() != epoch || !self.live(tab) {
            return Err(PAGE_CHANGED.into());
        }
        Ok(())
    }

    fn guarded(&self, script: &str, task_id: Option<&str>) -> String {
        guarded_script(script, self.inner.permissions.borrow().script_origins(task_id).as_deref())
    }

    /// Idempotent: a tab that already has a view keeps it, so showing a tab never reloads its page.
    pub fn open_tab(&self, tab_id: &str, url: Option<&str>, task_id: Option<&str>) -> Result<(), String> {
        if self.tab(tab_id).is_some() {
            return Ok(());
        }
        let tab = self.create_tab(tab_id, task_id)?;
        self.inner.tabs.borrow_mut().push(tab.clone());
        if let Some(url) = url {
            self.load(&tab, url);
        }
        self.layout(&tab);
        Ok(())
    }

    fn create_tab(&self, tab_id: &str, task_id: Option<&str>) -> Result<Rc<Tab>, String> {
        let mtm = self.inner.mtm;
        let parking = self.parking_window();
        let content = parking.contentView().ok_or("The browser has nowhere to keep its pages.")?;
        let handle = ViewHandle(NonNull::from(&*content).cast());
        let owner = Rc::new(Owner { id: tab_id.to_string(), task_id: RefCell::new(task_id.map(str::to_string)) });
        let shared = Rc::new(TabShared { owner: owner.clone(), epoch: Cell::new(0), requested: Cell::new(None), closed: Cell::new(false), main_request: RefCell::new(None) });
        let diagnostics = Rc::new(RefCell::new(Diagnostics::new(now_ms())));
        let popups: Rc<RefCell<Vec<Popup>>> = Rc::new(RefCell::new(Vec::new()));
        let (width, height) = self.inner.parked.get();

        let ipc_diagnostics = diagnostics.clone();
        let ipc_shared = shared.clone();
        let popup_owner = owner.clone();
        let popup_permissions = self.inner.permissions.clone();
        let popup_events = self.inner.events.clone();
        let popup_list = popups.clone();
        let builder = WebViewBuilder::new()
            .with_user_agent(&self.inner.user_agent)
            .with_bounds(Rect { position: LogicalPosition::new(0.0, 0.0).into(), size: LogicalSize::new(width, height).into() })
            .with_visible(false)
            .with_accept_first_mouse(true)
            .with_clipboard(true)
            .with_devtools(true)
            .with_initialization_script_for_main_only(DIAGNOSTICS_SCRIPT, true)
            .with_initialization_script_for_main_only(MOUSE_NAVIGATION_SCRIPT, false)
            .with_ipc_handler(move |request| {
                if !ipc_shared.closed.get() {
                    record_message(&ipc_diagnostics, request.body());
                }
            })
            .with_new_window_req_handler(move |url, features| open_popup(mtm, &popup_owner, &popup_permissions, &popup_events, &popup_list, &url, features))
            .with_permission_handler(|_| PermissionResponse::Deny)
            .with_webview_configuration(configuration(mtm));
        let webview = builder.build_as_child(&handle).map_err(|error| error.to_string())?;
        let wk: Retained<WKWebView> = Retained::into_super(webview.webview());

        let delegate = PageDelegate::new(mtm, tab_handler(shared.clone(), diagnostics.clone(), self.inner.permissions.clone(), self.inner.events.clone()));
        delegate.attach(&wk, false);
        let kvo_shared = shared.clone();
        let kvo_events = self.inner.events.clone();
        let observer = PropertyObserver::new(
            wk.clone(),
            Box::new(move |webview, key| {
                if kvo_shared.closed.get() {
                    return;
                }
                let id = &kvo_shared.owner.id;
                let event = match key {
                    "loading" if unsafe { webview.isLoading() } => {
                        let mut event = page_event(id);
                        event.loading = Some(true);
                        event
                    }
                    "loading" => {
                        kvo_shared.requested.set(None);
                        let mut event = state_event(id, webview);
                        event.loading = Some(false);
                        event
                    }
                    "title" => {
                        let mut event = page_event(id);
                        event.title = Some(webkit::current_title(webview));
                        event
                    }
                    _ => state_event(id, webview),
                };
                send(&kvo_events, BrowserEvent::Page(event));
            }),
        );
        Ok(Rc::new(Tab {
            id: tab_id.to_string(),
            shared,
            _webview: webview,
            wk,
            _delegate: delegate,
            observer,
            diagnostics,
            popups,
            shown: Cell::new(false),
            find: RefCell::new(FindState::default()),
        }))
    }

    fn load(&self, tab: &Tab, url: &str) {
        if !allow_navigation(&self.inner.permissions, &tab.shared.owner, url, &self.inner.events) {
            return;
        }
        match webkit::load(&tab.wk, url) {
            Ok(()) => tab.shared.requested.set(Some(Instant::now())),
            Err(message) => {
                let mut event = page_event(&tab.id);
                event.loading = Some(false);
                event.error = Some(message);
                send(&self.inner.events, BrowserEvent::Page(event));
            }
        }
    }

    pub fn navigate(&self, tab_id: &str, url: &str, task_id: Option<&str>) -> Result<(), String> {
        let Some(tab) = self.tab(tab_id) else { return self.open_tab(tab_id, Some(url), task_id) };
        *tab.shared.owner.task_id.borrow_mut() = task_id.map(str::to_string);
        self.load(&tab, url);
        Ok(())
    }

    pub fn go_history(&self, tab_id: &str, delta: i8, task_id: Option<&str>) -> Result<(), String> {
        let Some(tab) = self.tab(tab_id) else { return Ok(()) };
        self.require_page(&tab, task_id)?;
        *tab.shared.owner.task_id.borrow_mut() = task_id.map(str::to_string);
        unsafe {
            if delta < 0 && tab.wk.canGoBack() {
                tab.shared.requested.set(Some(Instant::now()));
                tab.wk.goBack();
            }
            if delta > 0 && tab.wk.canGoForward() {
                tab.shared.requested.set(Some(Instant::now()));
                tab.wk.goForward();
            }
        }
        Ok(())
    }

    pub fn reload(&self, tab_id: &str, task_id: Option<&str>) -> Result<(), String> {
        let Some(tab) = self.tab(tab_id) else { return Ok(()) };
        self.require_page(&tab, task_id)?;
        *tab.shared.owner.task_id.borrow_mut() = task_id.map(str::to_string);
        tab.shared.requested.set(Some(Instant::now()));
        unsafe { tab.wk.reload() };
        Ok(())
    }

    /// Stops a page that is still loading.
    pub fn stop(&self, tab_id: &str) {
        if let Some(tab) = self.tab(tab_id) {
            tab.shared.requested.set(None);
            unsafe { tab.wk.stopLoading() };
        }
    }

    pub fn close_tab(&self, tab_id: &str) {
        let tab = {
            let mut tabs = self.inner.tabs.borrow_mut();
            let Some(index) = tabs.iter().position(|tab| tab.id == tab_id) else { return };
            tabs.remove(index)
        };
        tab.shared.closed.set(true);
        close_popups(&tab);
        let held_keys = self.holds_keyboard(&tab);
        tab.wk.removeFromSuperview();
        if held_keys {
            self.give_keys_back();
        }
    }

    fn holds_keyboard(&self, tab: &Tab) -> bool {
        let Some(window) = tab.wk.window() else { return false };
        let Some(responder) = window.firstResponder() else { return false };
        responder.downcast::<NSView>().is_ok_and(|view| view.isDescendantOf(&tab.wk))
    }

    fn give_keys_back(&self) {
        if let Some(window) = self.host_window() {
            window.makeFirstResponder(Some(&self.inner.host_view));
        }
    }

    /// Which tab the panel shows. The view is only drawn once the panel has also said where it is.
    pub fn show_tab(&self, tab_id: Option<&str>) {
        *self.inner.active.borrow_mut() = tab_id.map(str::to_string);
        self.layout_all();
    }

    /// Where the panel is, in window points. None means the panel is not on screen at all.
    pub fn set_bounds(&self, bounds: Option<PageBounds>) {
        if self.inner.bounds.get() == bounds {
            return;
        }
        self.inner.bounds.set(bounds);
        // A page off screen lays out in the panel it would appear in, or a window's worth when there is none.
        self.inner.parked.set(bounds.map(|bounds| (bounds.width.max(1.0), bounds.height.max(1.0))).unwrap_or(PARKED_VIEWPORT));
        self.layout_all();
    }

    pub fn bounds(&self) -> Option<PageBounds> {
        self.inner.bounds.get()
    }

    fn layout_all(&self) {
        let tabs: Vec<Rc<Tab>> = self.inner.tabs.borrow().clone();
        for tab in tabs {
            self.layout(&tab);
        }
    }

    /// Puts a tab where it belongs: over the panel when it is the one on screen, else parked.
    fn layout(&self, tab: &Tab) {
        let bounds = self.inner.bounds.get();
        let shown = bounds.is_some() && self.inner.active.borrow().as_deref() == Some(tab.id.as_str());
        let home: Retained<NSView> = if shown {
            self.inner.host_view.clone()
        } else {
            match self.parking_window().contentView() {
                Some(view) => view,
                None => return,
            }
        };
        let moving = unsafe { tab.wk.superview() }.is_none_or(|parent| !std::ptr::eq(&*parent, &*home));
        if moving {
            let held_keys = self.holds_keyboard(tab);
            tab.wk.removeFromSuperview();
            home.addSubview(&tab.wk);
            if held_keys {
                self.give_keys_back();
            }
        }
        let frame = match bounds.filter(|_| shown) {
            Some(bounds) => {
                let height = home.frame().size.height;
                let y = if home.isFlipped() { bounds.y } else { height - bounds.y - bounds.height };
                NSRect::new(NSPoint::new(bounds.x, y), NSSize::new(bounds.width.max(1.0), bounds.height.max(1.0)))
            }
            None => {
                let (width, height) = self.inner.parked.get();
                let parent = home.frame().size.height;
                NSRect::new(NSPoint::new(0.0, if home.isFlipped() { 0.0 } else { parent - height }), NSSize::new(width, height))
            }
        };
        tab.wk.setFrame(frame);
        tab.wk.setHidden(!shown);
        tab.shown.set(shown);
    }

    /// Hands the keyboard to the page a tab shows.
    pub fn focus_tab(&self, tab_id: &str) {
        if let Some(tab) = self.tab(tab_id) {
            if let Some(window) = tab.wk.window() {
                window.makeFirstResponder(Some(&tab.wk));
            }
        }
    }

    pub fn discard_shots(&self) {
        self.inner.shots.borrow_mut().discard();
    }

    pub fn tab_ids(&self) -> Vec<String> {
        self.inner.tabs.borrow().iter().map(|tab| tab.id.clone()).collect()
    }

    /// WebKit views sit over the window and take their own input, so the app paints no page.
    pub fn painted_by_app(&self) -> bool {
        false
    }

    pub fn frame(&self, _tab_id: &str) -> Option<std::sync::Arc<gpui::RenderImage>> {
        None
    }

    pub fn input(&self, _tab_id: &str, _input: &crate::PageInput) {}

    pub fn key(&self, _tab_id: &str, _down: bool, _keystroke: &gpui::Keystroke) -> bool {
        false
    }

    pub fn choose_option(&self, _tab_id: &str, _choice: Option<usize>) {}

    pub fn cursor(&self, _tab_id: &str) -> gpui::CursorStyle {
        gpui::CursorStyle::Arrow
    }

    /// AppKit hands the app its shortcuts before a page sees them.
    pub fn set_shortcuts(&self, _overrides: &act_core::domain::shortcuts::ShortcutOverrides) {}

    /// Waits for the tab to stop loading, up to the timeout. None when the tab is gone by then.
    async fn settled(&self, tab_id: &str, timeout_ms: u64) -> Option<Rc<Tab>> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let tab = self.tab(tab_id)?;
            if !tab.loading() || Instant::now() >= deadline {
                return Some(tab);
            }
            drop(tab);
            self.inner.executor.timer(Duration::from_millis(50)).await;
        }
    }

    /// Waits for the page to settle, then reads it. None when that tab is gone by the time it settles.
    pub async fn read_page(&self, tab_id: &str, text_limit: f64, timeout_ms: u64, task_id: Option<&str>) -> Result<Option<BrowserSnapshot>, String> {
        let Some(tab) = self.settled(tab_id, timeout_ms).await else { return Ok(None) };
        self.require_page(&tab, task_id)?;
        let epoch = tab.shared.epoch.get();
        let requested = if text_limit > 0.0 { text_limit } else { DEFAULT_TEXT_LIMIT };
        let limit = requested.clamp(200.0, 120_000.0) as usize;
        let page = webkit::evaluate(&tab.wk, &self.guarded(&snapshot_script(limit), task_id)).await?;
        self.same_page(&tab, epoch, task_id)?;
        let url = page["url"].as_str().unwrap_or_default().to_string();
        if !self.inner.permissions.borrow().permitted(task_id, &url) {
            return Err(SCRIPT_APPROVAL_ERROR.into());
        }
        let text = page["text"].as_str().unwrap_or_default();
        let elements: Vec<BrowserElement> = serde_json::from_value(page["elements"].clone()).unwrap_or_default();
        Ok(Some(BrowserSnapshot {
            tab_id: tab.id.clone(),
            url,
            title: page["title"].as_str().unwrap_or_default().to_string(),
            loading: tab.loading(),
            text: if page["truncated"].as_bool() == Some(true) { format!("{text}\n… (page text truncated)") } else { text.to_string() },
            elements,
        }))
    }

    /// Reads bounded diagnostic history or waits for a condition in one tab.
    pub async fn inspect_page(&self, tab_id: &str, inspection: Inspection, task_id: Option<&str>) -> Result<Option<BrowserReadResult>, String> {
        let Some(tab) = self.tab(tab_id) else { return Ok(None) };
        self.require_page(&tab, task_id)?;
        let permissions = self.inner.permissions.borrow().clone();
        match inspection {
            Inspection::Console { since, limit, minimum_level } => {
                let minimum = console_rank(minimum_level.as_deref().unwrap_or("debug"));
                let diagnostics = tab.diagnostics.borrow();
                let entries = diagnostics.console.iter().filter(|recorded| permissions.permitted(task_id, &recorded.page_origin)).map(|recorded| (recorded.entry.sequence, recorded.entry.clone()));
                let limit = limit.unwrap_or(50).clamp(1, MAX_DIAGNOSTIC_ENTRIES as u64) as usize;
                let (entries, omitted) = newest(entries, since.unwrap_or(0), limit, |entry| console_rank(&entry.level) >= minimum);
                Ok(Some(BrowserReadResult::Console { tab_id: tab.id.clone(), url: tab.url(), title: tab.title(), entries, latest_sequence: diagnostics.console_sequence, omitted }))
            }
            Inspection::Network { since, limit, failures_only } => {
                let diagnostics = tab.diagnostics.borrow();
                let entries = diagnostics.network.iter().filter(|recorded| permissions.permitted(task_id, &recorded.page_origin)).map(|recorded| (recorded.entry.sequence, recorded.entry.clone()));
                let limit = limit.unwrap_or(50).clamp(1, MAX_DIAGNOSTIC_ENTRIES as u64) as usize;
                let (entries, omitted) = newest(entries, since.unwrap_or(0), limit, |entry| !failures_only || entry.error.is_some() || entry.status.unwrap_or(0) >= 400);
                Ok(Some(BrowserReadResult::Network { tab_id: tab.id.clone(), url: tab.url(), title: tab.title(), entries, latest_sequence: diagnostics.network_sequence, omitted }))
            }
            Inspection::Wait { condition, value, timeout_ms } => self.wait_for_page(tab, condition, value, timeout_ms, task_id).await,
        }
    }

    async fn wait_matches(&self, tab: &Tab, condition: &str, value: &str, task_id: Option<&str>) -> Result<bool, String> {
        self.require_page(tab, task_id)?;
        match condition {
            "url" => Ok(tab.url().to_lowercase().contains(&value.to_lowercase())),
            "network-idle" => Ok(tab.diagnostics.borrow().idle(now_ms())),
            _ => {
                let epoch = tab.shared.epoch.get();
                let result = webkit::evaluate(&tab.wk, &self.guarded(&page_condition_script(condition, value), task_id)).await?;
                self.same_page(tab, epoch, task_id)?;
                Ok(result.as_bool() == Some(true))
            }
        }
    }

    async fn wait_for_page(&self, tab: Rc<Tab>, condition: String, value: Option<String>, timeout_ms: u64, task_id: Option<&str>) -> Result<Option<BrowserReadResult>, String> {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(timeout_ms);
        let matched = loop {
            if !self.live(&tab) {
                return Ok(None);
            }
            let matched = self.wait_matches(&tab, &condition, value.as_deref().unwrap_or_default(), task_id).await?;
            let now = Instant::now();
            if matched || now >= deadline {
                break matched;
            }
            let pause = (deadline - now).min(Duration::from_millis(100)).max(Duration::from_millis(1));
            self.inner.executor.timer(pause).await;
        };
        self.require_page(&tab, task_id)?;
        Ok(Some(BrowserReadResult::Wait {
            tab_id: tab.id.clone(),
            url: tab.url(),
            title: tab.title(),
            condition,
            value,
            matched,
            elapsed_ms: started.elapsed().as_millis() as u64,
        }))
    }

    /// Waits the same way, then writes a picture of the page. None when that tab is gone by then.
    pub async fn capture_page(&self, tab_id: &str, full_page: bool, timeout_ms: u64, task_id: Option<&str>) -> Result<Option<BrowserShot>, String> {
        let Some(tab) = self.settled(tab_id, timeout_ms).await else { return Ok(None) };
        self.require_page(&tab, task_id)?;
        let epoch = tab.shared.epoch.get();
        // A page the window is not drawing is made visible where it is parked for the capture. The
        // parking window is never shown, so this puts nothing on screen.
        let woken = !tab.shown.get();
        if woken {
            tab.wk.setHidden(false);
        }
        let drawn = self.draw(&tab, full_page).await;
        if woken && self.live(&tab) {
            self.layout(&tab);
        }
        let bytes = drawn?;
        self.same_page(&tab, epoch, task_id)?;
        let (width, height) = png_size(&bytes).unwrap_or((0, 0));
        let path = self.inner.shots.borrow_mut().write(&bytes).map_err(|error| error.to_string())?;
        Ok(Some(BrowserShot { tab_id: tab.id.clone(), url: tab.url(), title: tab.title(), path: path.to_string_lossy().into_owned(), width, height }))
    }

    async fn draw(&self, tab: &Tab, full_page: bool) -> Result<Vec<u8>, String> {
        let scale = tab.wk.window().map(|window| window.backingScaleFactor()).unwrap_or(2.0);
        if !full_page {
            return webkit::snapshot(&tab.wk, scale).await;
        }
        let size = webkit::evaluate(&tab.wk, "({ width: Math.max(document.documentElement.scrollWidth, innerWidth), height: Math.max(document.documentElement.scrollHeight, document.body ? document.body.scrollHeight : 0, innerHeight) })").await?;
        let clamp = |value: Option<f64>| value.unwrap_or(1.0).round().clamp(1.0, MAX_SHOT_POINTS);
        let size = CGSize::new(clamp(size["width"].as_f64()), clamp(size["height"].as_f64()));
        webkit::full_page(&tab.wk, size, scale).await
    }

    pub async fn act(&self, tab_id: &str, action: &BrowserAction, task_id: Option<&str>) -> Result<String, String> {
        let Some(tab) = self.tab(tab_id) else { return Ok("That tab is no longer open.".into()) };
        self.require_page(&tab, task_id)?;
        *tab.shared.owner.task_id.borrow_mut() = task_id.map(str::to_string);
        let result = webkit::evaluate(&tab.wk, &self.guarded(&action_script(action), task_id)).await?;
        self.require_page(&tab, task_id)?;
        // WebKit starts a navigation the action caused a moment later, so the read that follows would see the old page.
        if matches!(action, BrowserAction::Click { .. } | BrowserAction::Type { submit: Some(true), .. }) {
            let until = Instant::now() + ACTION_LOAD_GRACE;
            while !unsafe { tab.wk.isLoading() } && Instant::now() < until {
                self.inner.executor.timer(Duration::from_millis(25)).await;
            }
        }
        Ok(result.as_str().unwrap_or_default().to_string())
    }

    /// Searches a page, stepping to the next match when the query is one the page is already showing.
    pub async fn find_in_page(&self, tab_id: &str, query: &str, forward: bool, find_next: bool) {
        let Some(tab) = self.tab(tab_id) else { return };
        let previous = tab.find.borrow().clone();
        let fresh = !find_next || previous.query != query;
        let matches = if fresh {
            let counted = webkit::evaluate(&tab.wk, &count_matches_script(query)).await.ok().and_then(|count| count.as_i64()).unwrap_or(0);
            let _ = webkit::evaluate(&tab.wk, "getSelection().removeAllRanges()").await;
            counted
        } else {
            previous.matches
        };
        let found = !query.is_empty() && webkit::find(&tab.wk, query, !forward).await;
        let matches = if found { matches.max(1) } else { 0 };
        let index = match (found, fresh) {
            (false, _) => 0,
            (true, true) => if forward { 0 } else { matches - 1 },
            (true, false) if forward => (previous.index + 1).rem_euclid(matches),
            (true, false) => (previous.index - 1).rem_euclid(matches),
        };
        *tab.find.borrow_mut() = FindState { query: query.to_string(), index, matches };
        if self.live(&tab) {
            send(&self.inner.events, BrowserEvent::Find { tab_id: tab.id.clone(), results: FindResults { matches, index: Some(index), counting: None } });
        }
    }

    pub async fn stop_find_in_page(&self, tab_id: &str) {
        let Some(tab) = self.tab(tab_id) else { return };
        *tab.find.borrow_mut() = FindState::default();
        let _ = webkit::evaluate(&tab.wk, "getSelection().removeAllRanges()").await;
    }

    /// Signs the whole app out of everything the panel has ever logged into.
    pub async fn clear_data(&self) {
        self.inner.permissions.borrow_mut().clear_origins();
        webkit::clear_store(&data_store(self.inner.mtm)).await;
        let tabs: Vec<Rc<Tab>> = self.inner.tabs.borrow().clone();
        for tab in tabs {
            unsafe { tab.wk.reload() };
        }
    }

    /// Replaces those sites' cookies in the panel's session with these, then reloads the pages on
    /// those sites. Nothing is removed when there is nothing to set. Answers how many were set.
    pub async fn replace_cookies(&self, cookies: &[CookieDetails], sites: &[String]) -> usize {
        let store = data_store(self.inner.mtm);
        if !cookies.is_empty() {
            for cookie in webkit::all_cookies(&store).await {
                let domain = cookie.domain().to_string();
                if !domain.is_empty() && sites.iter().any(|site| cookie_on_site(&domain, site)) {
                    webkit::delete_cookie(&store, &cookie).await;
                }
            }
        }
        let mut added = 0;
        for cookie in cookies {
            if webkit::set_cookie(&store, cookie).await {
                added += 1;
            }
        }
        let tabs: Vec<Rc<Tab>> = self.inner.tabs.borrow().clone();
        for tab in tabs {
            let host = url::Url::parse(&tab.url()).ok().and_then(|url| url.host_str().map(str::to_string));
            if host.is_some_and(|host| sites.iter().any(|site| cookie_on_site(&host, site))) {
                unsafe { tab.wk.reload() };
            }
        }
        added
    }
}

/// How a tab hears from WebKit: what it may load, when its document changes, and how loads end.
fn tab_handler(
    shared: Rc<TabShared>,
    diagnostics: Rc<RefCell<Diagnostics>>,
    permissions: Rc<RefCell<Permissions>>,
    events: UnboundedSender<BrowserEvent>,
) -> SignalHandler {
    Box::new(move |_webview, signal| {
        if shared.closed.get() {
            return false;
        }
        let id = &shared.owner.id;
        match signal {
            Signal::Decide { url, main_frame, method } => {
                if !main_frame {
                    return true;
                }
                if !allow_navigation(&permissions, &shared.owner, &url, &events) {
                    shared.requested.set(None);
                    return false;
                }
                let now = now_ms();
                diagnostics.borrow_mut().start_request("document".into(), now);
                *shared.main_request.borrow_mut() = Some((url, now, method));
                true
            }
            Signal::Started => {
                shared.requested.set(None);
                true
            }
            Signal::Response { url, status, main_frame } => {
                if main_frame {
                    finish_document(&shared, &diagnostics, Some(&url), status, None);
                }
                true
            }
            Signal::Committed => {
                shared.epoch.set(shared.epoch.get() + 1);
                let mut diagnostics = diagnostics.borrow_mut();
                diagnostics.drop_pending();
                true
            }
            Signal::Finished => {
                shared.requested.set(None);
                true
            }
            Signal::Failed { error, provisional } => {
                shared.requested.set(None);
                if provisional {
                    finish_document(&shared, &diagnostics, None, None, Some(error.localizedDescription().to_string()));
                }
                if let Some(message) = failure_message(error) {
                    let mut event = page_event(id);
                    event.loading = Some(false);
                    event.error = Some(message);
                    send(&events, BrowserEvent::Page(event));
                }
                true
            }
            Signal::Terminated => {
                shared.requested.set(None);
                let mut event = page_event(id);
                event.loading = Some(false);
                event.error = Some("The page stopped responding.".into());
                send(&events, BrowserEvent::Page(event));
                true
            }
            Signal::Closed => true,
        }
    })
}

/// The top document's request, recorded once WebKit has its answer or gives up on it.
fn finish_document(shared: &TabShared, diagnostics: &RefCell<Diagnostics>, url: Option<&str>, status: Option<u16>, error: Option<String>) {
    let Some((requested, started, method)) = shared.main_request.borrow_mut().take() else { return };
    let url = url.unwrap_or(&requested).to_string();
    let now = now_ms();
    diagnostics.borrow_mut().finish_request(Some("document"), origin_of(&url), now, started, &method, &url, "mainFrame", (now - started).max(0) as u64, status, error.as_deref(), None);
}

/// A message the page's hooks posted: a console line, or a request starting or finishing.
fn record_message(diagnostics: &RefCell<Diagnostics>, body: &str) {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(body) else { return };
    let origin = message["o"].as_str().unwrap_or_default().to_string();
    let now = now_ms();
    let mut diagnostics = diagnostics.borrow_mut();
    match message["k"].as_str() {
        Some("console") => {
            let level = message["level"].as_str().filter(|level| matches!(*level, "debug" | "info" | "warning" | "error")).unwrap_or("info");
            diagnostics.log(origin, now, level, message["message"].as_str().unwrap_or_default(), message["source"].as_str(), message["line"].as_u64());
        }
        Some("net-start") => {
            if let Some(id) = message["id"].as_u64() {
                diagnostics.start_request(format!("page:{id}"), now);
            }
        }
        Some("net") => {
            let id = message["id"].as_u64().map(|id| format!("page:{id}"));
            diagnostics.finish_request(
                id.as_deref(),
                origin,
                now,
                message["started"].as_i64().unwrap_or(now),
                message["method"].as_str().unwrap_or("GET"),
                message["url"].as_str().unwrap_or_default(),
                message["type"].as_str().unwrap_or("other"),
                message["duration"].as_u64().unwrap_or(0),
                message["status"].as_u64().and_then(|status| u16::try_from(status).ok()).filter(|status| *status > 0),
                message["error"].as_str(),
                message["cached"].as_bool(),
            );
        }
        _ => {}
    }
}

/// A page opening a window: a sign-in popup, usually. It gets a window of its own with the same
/// session and the same checks on where it may go, on behalf of whoever drives its opener.
fn open_popup(
    mtm: MainThreadMarker,
    owner: &Rc<Owner>,
    permissions: &Rc<RefCell<Permissions>>,
    events: &UnboundedSender<BrowserEvent>,
    popups: &Rc<RefCell<Vec<Popup>>>,
    url: &str,
    features: NewWindowFeatures,
) -> NewWindowResponse {
    if !allow_navigation(permissions, owner, url, events) {
        return NewWindowResponse::Deny;
    }
    popups.borrow_mut().retain(|popup| popup.window.isVisible());
    let child = Rc::new(Owner { id: owner.id.clone(), task_id: RefCell::new(owner.task_id.borrow().clone()) });
    let (width, height) = POPUP_SIZE;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    let webview = unsafe { WKWebView::initWithFrame_configuration(mtm.alloc(), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height)), &features.opener.target_configuration) };
    let handler_owner = child.clone();
    let handler_permissions = permissions.clone();
    let handler_events = events.clone();
    let delegate = PageDelegate::new(
        mtm,
        Box::new(move |webview, signal| match signal {
            Signal::Decide { url, main_frame, .. } => !main_frame || allow_navigation(&handler_permissions, &handler_owner, &url, &handler_events),
            Signal::Closed => {
                if let Some(window) = webview.window() {
                    window.close();
                }
                true
            }
            _ => true,
        }),
    );
    delegate.attach(&webview, true);
    window.setContentView(Some(&webview));
    window.center();
    window.makeKeyAndOrderFront(None);
    popups.borrow_mut().push(Popup { window, webview: webview.clone(), _delegate: delegate, owner: child });
    NewWindowResponse::Create { webview }
}
