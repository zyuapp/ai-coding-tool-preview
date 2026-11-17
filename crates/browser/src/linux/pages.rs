//! The tabs themselves on Linux: one WebKitGTK view per tab, each in an X window inside the app's
//! own (or off screen for the app to paint, on Wayland), all sharing one persistent session. Only
//! the tab on screen is shown; the rest keep the panel's size so their pages lay out as they would appear.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use act_core::domain::find::FindResults;
use act_core::domain::platform::{BrowserAction, BrowserPermissions};
use act_core::events::NavigationRequest;
use act_host::tools::types::{BrowserElement, BrowserReadResult, BrowserShot, BrowserSnapshot};
use futures::channel::mpsc::UnboundedSender;
use gpui::BackgroundExecutor;
use gtk::glib;
use gtk::prelude::*;
use javascriptcore::ValueExt;
use webkit2gtk::{
    LoadEvent, NavigationPolicyDecision, OptionMenuExt, NavigationPolicyDecisionExt, PolicyDecisionExt, PolicyDecisionType, ResponsePolicyDecision, ResponsePolicyDecisionExt, SettingsExt,
    URIRequestExt, URIResponseExt, UserContentInjectedFrames, UserContentManagerExt, UserScript, UserScriptInjectionTime, WebViewExt,
    WebsiteDataManager,
};

use super::embed::{Embedded, Parent, PixelRect};
use super::offscreen::Offscreen;
use super::keys::AppShortcuts;
use super::webkit;
use crate::diagnostics::{console_rank, newest, Diagnostics, MAX_DIAGNOSTIC_ENTRIES};
use crate::import::{cookie_on_site, CookieDetails};
use crate::permissions::{is_web_url, origin_of, Permissions, SCRIPT_APPROVAL_ERROR};
use crate::scripts::{action_script, count_matches_script, guarded_script, page_condition_script, snapshot_script, DIAGNOSTICS_SCRIPT, MOUSE_NAVIGATION_SCRIPT};
use crate::shots::{png_size, Shots};
use crate::types::{now_ms, page_event, BrowserEvent, Inspection, OptionChoice, PageBounds, PageInput, PageRect};

const DEFAULT_TEXT_LIMIT: f64 = 4_000.0;
/// A page nobody is looking at still lays itself out, so it is given a window's worth of room.
const PARKED_VIEWPORT: (f64, f64) = (1_200.0, 800.0);
/// A long page is cut off here rather than refused.
const MAX_SHOT_POINTS: f64 = 8_000.0;
/// How long a load asked for may take to start before the tab counts as settled without it.
const LOAD_START_GRACE: Duration = Duration::from_millis(1_500);
const POPUP_SIZE: (i32, i32) = (560, 720);
const PAGE_CHANGED: &str = "The page changed while it was being read. Try again.";
/// Where a tab that is not on screen is drawn for a picture: inside the app's window, outside its bounds.
const OUT_OF_SIGHT: i32 = -30_000;
/// `window.ipc`, which the page's hooks post through, as wry defines it.
const IPC_SCRIPT: &str = "Object.defineProperty(window, 'ipc', { value: Object.freeze({ postMessage: function(x) { window.webkit.messageHandlers['ipc'].postMessage(x) } }) })";

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
    /// The drop-down list the page has open, while the app shows it.
    option_menu: RefCell<Option<webkit2gtk::OptionMenu>>,
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
    view: webkit2gtk::WebView,
    host: Host,
    diagnostics: Rc<RefCell<Diagnostics>>,
    popups: Rc<RefCell<Vec<gtk::Window>>>,
    shown: Cell<bool>,
    find: RefCell<FindState>,
    _accessible: super::gtk_loop::AccessibleHold,
}

/// Where a tab's view draws: in a window of its own inside the app's (X11), or off screen for the
/// app to paint (Wayland).
enum Host {
    Window(Embedded),
    Offscreen(Offscreen),
}

impl Host {
    fn place(&self, rect: PixelRect) {
        match self {
            Host::Window(window) => window.place(rect, None, &[]),
            Host::Offscreen(offscreen) => offscreen.place(rect.width, rect.height),
        }
    }

    /// Has the page draw at its full size where nobody sees it, for a picture.
    fn wake(&self, width: i32, height: i32) {
        match self {
            Host::Window(window) => window.place(PixelRect { x: OUT_OF_SIGHT, y: OUT_OF_SIGHT, width, height }, None, &[]),
            Host::Offscreen(offscreen) => offscreen.wake(width, height),
        }
    }

    fn park(&self, width: i32, height: i32) {
        match self {
            Host::Window(window) => window.park(width, height),
            Host::Offscreen(offscreen) => offscreen.park(width, height),
        }
    }

    fn hide(&self) {
        match self {
            Host::Window(window) => window.hide(),
            Host::Offscreen(offscreen) => offscreen.hide(),
        }
    }

    fn holds_keys(&self) -> bool {
        match self {
            Host::Window(window) => window.holds_keys(),
            Host::Offscreen(offscreen) => offscreen.holds_keys(),
        }
    }

    fn focus(&self) {
        match self {
            Host::Window(window) => window.focus(),
            Host::Offscreen(offscreen) => offscreen.focus(true),
        }
    }
}

impl Tab {
    fn loading(&self) -> bool {
        let requested = self.shared.requested.get().is_some_and(|at| at.elapsed() < LOAD_START_GRACE);
        self.view.is_loading() || requested
    }

    fn url(&self) -> String {
        self.view.uri().map(|uri| uri.to_string()).unwrap_or_default()
    }

    fn title(&self) -> String {
        self.view.title().map(|title| title.to_string()).unwrap_or_default()
    }
}

struct Inner {
    /// The app's X11 window the views are placed in; None where the app paints them itself.
    parent: Option<Rc<Parent>>,
    context: webkit2gtk::WebContext,
    data: WebsiteDataManager,
    permissions: Rc<RefCell<Permissions>>,
    tabs: RefCell<Vec<Rc<Tab>>>,
    active: RefCell<Option<String>>,
    bounds: Cell<Option<PageBounds>>,
    parked: Cell<(f64, f64)>,
    shots: RefCell<Shots>,
    events: UnboundedSender<BrowserEvent>,
    executor: BackgroundExecutor,
    /// Device pixels per window point.
    scale: Cell<f64>,
    shortcuts: AppShortcuts,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for tab in self.tabs.borrow_mut().drain(..) {
            close_popups(&tab);
            tab.shared.closed.set(true);
        }
    }
}

pub struct PagesOptions {
    pub parent: Option<Rc<Parent>>,
    /// Where the session's cookies and storage are kept.
    pub data_dir: PathBuf,
    pub scale: f64,
    pub executor: BackgroundExecutor,
    pub events: UnboundedSender<BrowserEvent>,
}

/// Every tab the panel holds, and the one place that drives WebKit for them.
#[derive(Clone)]
pub struct Pages {
    inner: Rc<Inner>,
}

fn send(events: &UnboundedSender<BrowserEvent>, event: BrowserEvent) {
    let _ = events.unbounded_send(event);
}

fn state_event(id: &str, view: &webkit2gtk::WebView) -> act_core::events::BrowserPageEvent {
    let mut event = page_event(id);
    event.url = Some(view.uri().map(|uri| uri.to_string()).unwrap_or_default());
    event.title = Some(view.title().map(|title| title.to_string()).unwrap_or_default());
    event.can_go_back = Some(view.can_go_back());
    event.can_go_forward = Some(view.can_go_forward());
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

/// Whether a run driving the tab, if any, may open `url`.
fn page_may_start(permissions: &RefCell<Permissions>, owner: &Owner, url: &str) -> bool {
    is_web_url(url) && permissions.borrow().permitted(owner.task_id.borrow().as_deref(), url)
}

/// Schemes frames are built from, which no page navigation is refused for where WebKit cannot tell the two apart.
fn frame_scheme(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    ["about:", "data:", "blob:"].iter().any(|scheme| lower.starts_with(scheme))
}

/// Which document a navigation loads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Page,
    Frame,
    /// WebKitGTK before 2.56 does not say.
    Unknown,
}

fn navigation_target(action: &mut webkit2gtk::NavigationAction) -> Target {
    match webkit::targets_main_frame(action) {
        Some(true) => Target::Page,
        Some(false) => Target::Frame,
        // A navigation aimed at a named frame is one of the page's frames.
        None if action.frame_name().is_some_and(|name| !name.is_empty()) => Target::Frame,
        None => Target::Unknown,
    }
}

fn close_popups(tab: &Tab) {
    for popup in tab.popups.borrow_mut().drain(..) {
        unsafe { popup.destroy() };
    }
}

/// The session every tab opens, kept in `data_dir` so sign-ins outlive the app.
fn session(data_dir: &std::path::Path) -> (webkit2gtk::WebContext, WebsiteDataManager) {
    let data = WebsiteDataManager::builder().base_data_directory(data_dir.join("data").to_string_lossy().to_string()).base_cache_directory(data_dir.join("cache").to_string_lossy().to_string()).build();
    if let Some(cookies) = webkit2gtk::WebsiteDataManagerExt::cookie_manager(&data) {
        webkit2gtk::CookieManagerExt::set_persistent_storage(&cookies, &data_dir.join("cookies.sqlite").to_string_lossy(), webkit2gtk::CookiePersistentStorage::Sqlite);
    }
    let context = webkit2gtk::WebContext::builder().website_data_manager(&data).build();
    (context, data)
}

impl Pages {
    pub fn new(options: PagesOptions) -> Result<Self, String> {
        let _ = std::fs::create_dir_all(&options.data_dir);
        let (context, data) = session(&options.data_dir);
        let inner = Rc::new(Inner {
            parent: options.parent,
            context,
            data,
            permissions: Rc::new(RefCell::new(Permissions::default())),
            tabs: RefCell::new(Vec::new()),
            active: RefCell::new(None),
            bounds: Cell::new(None),
            parked: Cell::new(PARKED_VIEWPORT),
            shots: RefCell::new(Shots::default()),
            events: options.events,
            executor: options.executor,
            scale: Cell::new(options.scale.max(0.5)),
            shortcuts: AppShortcuts::default(),
        });
        Ok(Pages { inner })
    }

    /// Device pixels per window point, as GPUI draws the window.
    pub fn set_scale(&self, scale: f64) {
        if (self.inner.scale.get() - scale).abs() > f64::EPSILON && scale > 0.0 {
            self.inner.scale.set(scale);
            for tab in self.inner.tabs.borrow().iter() {
                self.zoom(tab);
            }
            self.layout_all();
        }
    }

    /// A page the app paints is laid out in device pixels, so it is zoomed to read at the window's scale.
    fn zoom(&self, tab: &Tab) {
        if let Host::Offscreen(offscreen) = &tab.host {
            tab.view.set_zoom_level(self.inner.scale.get() / f64::from(offscreen.gtk_scale()));
        }
    }

    /// Whether the app paints the pages itself rather than placing them over its window.
    pub fn painted_by_app(&self) -> bool {
        self.inner.parent.is_none()
    }

    /// The last frame a page the app paints drew.
    pub fn frame(&self, tab_id: &str) -> Option<std::sync::Arc<gpui::RenderImage>> {
        match &self.tab(tab_id)?.host {
            Host::Offscreen(offscreen) => offscreen.frame(),
            Host::Window(_) => None,
        }
    }

    /// Hands a key the panel received to a page the app paints, unless it is one of the app's.
    /// True when the page took it.
    pub fn key(&self, tab_id: &str, down: bool, keystroke: &gpui::Keystroke) -> bool {
        let Some(tab) = self.tab(tab_id) else { return false };
        let Host::Offscreen(offscreen) = &tab.host else { return false };
        if !offscreen.holds_keys() || self.inner.shortcuts.wants_keystroke(keystroke) {
            return false;
        }
        offscreen.input(&PageInput::Key { down, keystroke: keystroke.clone() });
        true
    }

    /// Hands input the panel received to a page the app paints.
    pub fn input(&self, tab_id: &str, input: &PageInput) {
        if let Some(tab) = self.tab(tab_id) {
            if let Host::Offscreen(offscreen) = &tab.host {
                offscreen.input(input);
            }
        }
    }

    /// The user's shortcuts, which a page holding the keyboard hands to the app.
    pub fn set_shortcuts(&self, overrides: &act_core::domain::shortcuts::ShortcutOverrides) {
        self.inner.shortcuts.set(overrides);
    }

    /// Gives the keyboard back to the app's own window when a page holds it.
    pub fn focus_host(&self) {
        for tab in self.inner.tabs.borrow().iter() {
            self.release_keys(tab);
        }
    }

    fn release_keys(&self, tab: &Tab) {
        if !tab.host.holds_keys() {
            return;
        }
        match (&tab.host, &self.inner.parent) {
            (Host::Offscreen(offscreen), _) => offscreen.focus(false),
            (Host::Window(_), Some(parent)) => parent.focus_app(),
            (Host::Window(_), None) => {}
        }
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
        let tab = self.create_tab(tab_id, task_id);
        self.inner.tabs.borrow_mut().push(tab.clone());
        if let Some(url) = url {
            self.load(&tab, url);
        }
        self.layout(&tab);
        Ok(())
    }

    fn content_manager(diagnostics: &Rc<RefCell<Diagnostics>>, shared: &Rc<TabShared>) -> webkit2gtk::UserContentManager {
        let manager = webkit2gtk::UserContentManager::new();
        manager.add_script(&UserScript::new(IPC_SCRIPT, UserContentInjectedFrames::AllFrames, UserScriptInjectionTime::Start, &[], &[]));
        manager.add_script(&UserScript::new(DIAGNOSTICS_SCRIPT, UserContentInjectedFrames::TopFrame, UserScriptInjectionTime::Start, &[], &[]));
        manager.add_script(&UserScript::new(MOUSE_NAVIGATION_SCRIPT, UserContentInjectedFrames::AllFrames, UserScriptInjectionTime::Start, &[], &[]));
        let diagnostics = diagnostics.clone();
        let shared = shared.clone();
        manager.connect_script_message_received(Some("ipc"), move |_, message| {
            if shared.closed.get() {
                return;
            }
            if let Some(value) = message.js_value() {
                record_message(&diagnostics, value.to_str().as_str());
            }
        });
        manager.register_script_message_handler("ipc");
        manager
    }

    fn settings() -> webkit2gtk::Settings {
        let settings = webkit2gtk::Settings::new();
        settings.set_enable_developer_extras(true);
        settings.set_javascript_can_open_windows_automatically(true);
        settings.set_javascript_can_access_clipboard(true);
        settings
    }

    fn create_tab(&self, tab_id: &str, task_id: Option<&str>) -> Rc<Tab> {
        let owner = Rc::new(Owner { id: tab_id.to_string(), task_id: RefCell::new(task_id.map(str::to_string)) });
        let shared = Rc::new(TabShared { owner: owner.clone(), epoch: Cell::new(0), requested: Cell::new(None), closed: Cell::new(false), main_request: RefCell::new(None), option_menu: RefCell::new(None) });
        let diagnostics = Rc::new(RefCell::new(Diagnostics::new(now_ms())));
        let popups: Rc<RefCell<Vec<gtk::Window>>> = Rc::new(RefCell::new(Vec::new()));
        let manager = Self::content_manager(&diagnostics, &shared);
        let settings = Self::settings();
        let view = webkit2gtk::WebView::builder().web_context(&self.inner.context).user_content_manager(&manager).settings(&settings).build();
        // Panel pages have no context menu.
        view.connect_context_menu(|_, _, _, _| true);
        self.watch(&view, &shared, &diagnostics, &popups);
        let host = match &self.inner.parent {
            Some(parent) => {
                let host = Embedded::new(parent, &view, false);
                let shortcuts = self.inner.shortcuts.clone();
                host.set_app_keys(move |event| shortcuts.wants(event));
                Host::Window(host)
            }
            None => {
                // Keys reach it from the app, not from an input method of the desktop's.
                view.set_input_method_context(None::<&webkit2gtk::InputMethodContext>);
                Self::app_menus(&view, &shared, &self.inner.events);
                let (events, tab_id) = (self.inner.events.clone(), tab_id.to_string());
                Host::Offscreen(Offscreen::new(&view, move || send(&events, BrowserEvent::PageFrame { tab_id: tab_id.clone() })))
            }
        };
        let (width, height) = self.parked_pixels();
        host.park(width, height);
        let tab = Rc::new(Tab { id: tab_id.to_string(), shared, view, host, diagnostics, popups, shown: Cell::new(false), find: RefCell::new(FindState::default()), _accessible: super::gtk_loop::AccessibleHold::new() });
        self.zoom(&tab);
        tab
    }

    /// A page the app paints has no window for GTK to open menus from, so its drop-down lists are
    /// shown by the app.
    fn app_menus(view: &webkit2gtk::WebView, shared: &Rc<TabShared>, events: &UnboundedSender<BrowserEvent>) {
        let (shared, events) = (shared.clone(), events.clone());
        view.connect_show_option_menu(move |view, menu, _, rect| {
            let scale = f64::from(view.scale_factor().max(1));
            let choices = (0..menu.n_items())
                .filter_map(|index| menu.item(index))
                .map(|mut item| OptionChoice {
                    label: item.label().map(|label| label.to_string()).unwrap_or_default(),
                    enabled: item.is_enabled(),
                    selected: item.is_selected(),
                    heading: item.is_group_label(),
                })
                .collect();
            let anchor = PageRect { x: f64::from(rect.x()) * scale, y: f64::from(rect.y()) * scale, width: f64::from(rect.width()) * scale, height: f64::from(rect.height()) * scale };
            {
                let (shared, events) = (shared.clone(), events.clone());
                menu.connect_close(move |menu| {
                    let mut open = shared.option_menu.borrow_mut();
                    if open.as_ref() == Some(menu) {
                        open.take();
                        send(&events, BrowserEvent::OptionMenuClosed { tab_id: shared.owner.id.clone() });
                    }
                });
            }
            *shared.option_menu.borrow_mut() = Some(menu.clone());
            send(&events, BrowserEvent::OptionMenu { tab_id: shared.owner.id.clone(), choices, anchor });
            true
        });
    }

    /// Answers the drop-down list a tab has open: the row chosen, or None to close it.
    pub fn choose_option(&self, tab_id: &str, choice: Option<usize>) {
        let Some(tab) = self.tab(tab_id) else { return };
        let Some(menu) = tab.shared.option_menu.borrow_mut().take() else { return };
        if let Some(index) = choice {
            menu.activate_item(index as u32);
        } else {
            menu.close();
        }
    }

    /// The pointer a page the app paints shows.
    pub fn cursor(&self, tab_id: &str) -> gpui::CursorStyle {
        match self.tab(tab_id).as_deref().map(|tab| &tab.host) {
            Some(Host::Offscreen(offscreen)) => offscreen.cursor(),
            _ => gpui::CursorStyle::Arrow,
        }
    }

    fn parked_pixels(&self) -> (i32, i32) {
        let (width, height) = self.inner.parked.get();
        let scale = self.inner.scale.get();
        ((width * scale).round() as i32, (height * scale).round() as i32)
    }

    /// Connects what a tab hears from WebKit: what it may load, how loading goes, and its user taking it over.
    fn watch(&self, view: &webkit2gtk::WebView, shared: &Rc<TabShared>, diagnostics: &Rc<RefCell<Diagnostics>>, popups: &Rc<RefCell<Vec<gtk::Window>>>) {
        let events = self.inner.events.clone();
        let permissions = self.inner.permissions.clone();
        {
            let (shared, diagnostics, permissions, events) = (shared.clone(), diagnostics.clone(), permissions.clone(), events.clone());
            view.connect_decide_policy(move |_, decision, kind| {
                if shared.closed.get() {
                    decision.ignore();
                    return true;
                }
                match kind {
                    PolicyDecisionType::NavigationAction => {
                        let Some(navigation) = decision.downcast_ref::<NavigationPolicyDecision>() else { return false };
                        let Some(mut action) = navigation.navigation_action() else { return false };
                        let target = navigation_target(&mut action);
                        if target == Target::Frame {
                            decision.use_();
                            return true;
                        }
                        let Some(request) = action.request() else { return false };
                        let url = request.uri().map(|uri| uri.to_string()).unwrap_or_default();
                        let method = request.http_method().map(|method| method.to_string()).unwrap_or_else(|| "GET".into());
                        if target == Target::Unknown && !page_may_start(&permissions, &shared.owner, &url) {
                            if frame_scheme(&url) || is_web_url(&url) {
                                decision.use_();
                            } else {
                                decision.ignore();
                            }
                            return true;
                        }
                        if !allow_navigation(&permissions, &shared.owner, &url, &events) {
                            shared.requested.set(None);
                            decision.ignore();
                            return true;
                        }
                        let now = now_ms();
                        diagnostics.borrow_mut().start_request("document".into(), now);
                        *shared.main_request.borrow_mut() = Some((url, now, method));
                        decision.use_();
                        true
                    }
                    PolicyDecisionType::Response => {
                        let Some(response) = decision.downcast_ref::<ResponsePolicyDecision>() else { return false };
                        if !response.is_main_frame_main_resource() {
                            return false;
                        }
                        let answer = response.response();
                        let url = answer.as_ref().and_then(|answer| answer.uri()).map(|uri| uri.to_string()).unwrap_or_default();
                        // A navigation WebKit could not place in the page or a frame is checked here, once its
                        // response shows it is the page's.
                        if is_web_url(&url) && !page_may_start(&permissions, &shared.owner, &url) {
                            allow_navigation(&permissions, &shared.owner, &url, &events);
                            shared.requested.set(None);
                            shared.main_request.borrow_mut().take();
                            decision.ignore();
                            return true;
                        }
                        let status = answer.as_ref().map(|answer| answer.status_code() as u16).filter(|status| *status > 0);
                        finish_document(&shared, &diagnostics, Some(&url), status, None);
                        // The panel shows pages; a file it cannot show is not downloaded behind the user's back.
                        if response.is_mime_type_supported() {
                            decision.use_();
                        } else {
                            decision.ignore();
                        }
                        true
                    }
                    _ => false,
                }
            });
        }
        {
            let (shared, diagnostics, events) = (shared.clone(), diagnostics.clone(), events.clone());
            view.connect_load_changed(move |view, load| {
                if shared.closed.get() {
                    return;
                }
                let id = &shared.owner.id;
                match load {
                    LoadEvent::Started => {
                        shared.requested.set(None);
                        let mut event = page_event(id);
                        event.loading = Some(true);
                        send(&events, BrowserEvent::Page(event));
                    }
                    LoadEvent::Committed => {
                        shared.epoch.set(shared.epoch.get() + 1);
                        diagnostics.borrow_mut().drop_pending();
                        send(&events, BrowserEvent::Page(state_event(id, view)));
                    }
                    LoadEvent::Finished => {
                        shared.requested.set(None);
                        let mut event = state_event(id, view);
                        event.loading = Some(false);
                        send(&events, BrowserEvent::Page(event));
                    }
                    _ => {}
                }
            });
        }
        {
            let (shared, diagnostics, events) = (shared.clone(), diagnostics.clone(), events.clone());
            view.connect_load_failed(move |_, load, failing, error| {
                if shared.closed.get() {
                    return false;
                }
                shared.requested.set(None);
                if load == LoadEvent::Started {
                    finish_document(&shared, &diagnostics, None, None, Some(error.message().to_string()));
                }
                if let Some(message) = failure_message(error, failing) {
                    let mut event = page_event(&shared.owner.id);
                    event.loading = Some(false);
                    event.error = Some(message);
                    send(&events, BrowserEvent::Page(event));
                }
                false
            });
        }
        {
            let (shared, events) = (shared.clone(), events.clone());
            view.connect_web_process_terminated(move |_, _| {
                if shared.closed.get() {
                    return;
                }
                shared.requested.set(None);
                let mut event = page_event(&shared.owner.id);
                event.loading = Some(false);
                event.error = Some("The page stopped responding.".into());
                send(&events, BrowserEvent::Page(event));
            });
        }
        {
            let (shared, events) = (shared.clone(), events.clone());
            view.connect_title_notify(move |view| {
                if shared.closed.get() {
                    return;
                }
                let mut event = page_event(&shared.owner.id);
                event.title = Some(view.title().map(|title| title.to_string()).unwrap_or_default());
                send(&events, BrowserEvent::Page(event));
            });
        }
        {
            let (shared, events) = (shared.clone(), events.clone());
            view.connect_uri_notify(move |view| {
                if !shared.closed.get() {
                    send(&events, BrowserEvent::Page(state_event(&shared.owner.id, view)));
                }
            });
        }
        // A press or key in a page is the user taking it over, so a run driving it stops being its owner.
        {
            let (shared, events, parent) = (shared.clone(), events.clone(), self.inner.parent.clone());
            view.connect_button_press_event(move |view, _| {
                *shared.owner.task_id.borrow_mut() = None;
                if let Some((parent, window)) = parent.as_ref().zip(view.window().and_then(|window| super::embed::xid(&window))) {
                    parent.focus(window, cx);
                }
                view.grab_focus();
                send(&events, BrowserEvent::PageFocused { tab_id: shared.owner.id.clone() });
                glib::Propagation::Proceed
            });
        }
        {
            let shared = shared.clone();
            view.connect_key_press_event(move |_, _| {
                *shared.owner.task_id.borrow_mut() = None;
                glib::Propagation::Proceed
            });
        }
        {
            let (owner, permissions, events, popups) = (shared.owner.clone(), permissions, events, popups.clone());
            view.connect_create(move |view, action| {
                let url = action.request().and_then(|request| request.uri()).map(|uri| uri.to_string()).unwrap_or_default();
                open_popup(view, &owner, &permissions, &events, &popups, &url)
            });
        }
    }

    fn load(&self, tab: &Tab, url: &str) {
        if !allow_navigation(&self.inner.permissions, &tab.shared.owner, url, &self.inner.events) {
            return;
        }
        tab.view.load_uri(url);
        tab.shared.requested.set(Some(Instant::now()));
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
        if delta < 0 && tab.view.can_go_back() {
            tab.shared.requested.set(Some(Instant::now()));
            tab.view.go_back();
        }
        if delta > 0 && tab.view.can_go_forward() {
            tab.shared.requested.set(Some(Instant::now()));
            tab.view.go_forward();
        }
        Ok(())
    }

    pub fn reload(&self, tab_id: &str, task_id: Option<&str>) -> Result<(), String> {
        let Some(tab) = self.tab(tab_id) else { return Ok(()) };
        self.require_page(&tab, task_id)?;
        *tab.shared.owner.task_id.borrow_mut() = task_id.map(str::to_string);
        tab.shared.requested.set(Some(Instant::now()));
        tab.view.reload();
        Ok(())
    }

    /// Stops a page that is still loading.
    pub fn stop(&self, tab_id: &str) {
        if let Some(tab) = self.tab(tab_id) {
            tab.shared.requested.set(None);
            tab.view.stop_loading();
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
        self.release_keys(&tab);
        tab.host.hide();
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
        match bounds.filter(|_| shown) {
            Some(bounds) => {
                let scale = self.inner.scale.get();
                let rect = PixelRect {
                    x: (bounds.x * scale).round() as i32,
                    y: (bounds.y * scale).round() as i32,
                    width: (bounds.width.max(1.0) * scale).round() as i32,
                    height: (bounds.height.max(1.0) * scale).round() as i32,
                };
                tab.host.place(rect);
            }
            None => {
                self.release_keys(tab);
                let (width, height) = self.parked_pixels();
                tab.host.park(width, height);
            }
        }
        tab.shown.set(shown);
    }

    /// Hands the keyboard to the page a tab shows.
    pub fn focus_tab(&self, tab_id: &str) {
        if let Some(tab) = self.tab(tab_id) {
            if tab.shown.get() {
                tab.host.focus();
                tab.view.grab_focus();
            }
        }
    }

    pub fn discard_shots(&self) {
        self.inner.shots.borrow_mut().discard();
    }

    pub fn tab_ids(&self) -> Vec<String> {
        self.inner.tabs.borrow().iter().map(|tab| tab.id.clone()).collect()
    }

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
        let page = webkit::evaluate(&tab.view, &self.guarded(&snapshot_script(limit), task_id)).await?;
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
                let result = webkit::evaluate(&tab.view, &self.guarded(&page_condition_script(condition, value), task_id)).await?;
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
        // A page the window is not drawing is drawn where nobody sees it, inside the app's window
        // but past its edge, for the capture.
        let woken = !tab.shown.get();
        if woken {
            let (width, height) = self.parked_pixels();
            tab.host.wake(width, height);
            self.inner.executor.timer(Duration::from_millis(100)).await;
        }
        let max_height = (MAX_SHOT_POINTS * self.inner.scale.get()).round() as i32;
        let drawn = webkit::snapshot(&tab.view, full_page, max_height).await;
        if woken && self.live(&tab) {
            self.layout(&tab);
        }
        let bytes = drawn?;
        self.same_page(&tab, epoch, task_id)?;
        let (width, height) = png_size(&bytes).unwrap_or((0, 0));
        let path = self.inner.shots.borrow_mut().write(&bytes).map_err(|error| error.to_string())?;
        Ok(Some(BrowserShot { tab_id: tab.id.clone(), url: tab.url(), title: tab.title(), path: path.to_string_lossy().into_owned(), width, height }))
    }

    pub async fn act(&self, tab_id: &str, action: &BrowserAction, task_id: Option<&str>) -> Result<String, String> {
        let Some(tab) = self.tab(tab_id) else { return Ok("That tab is no longer open.".into()) };
        self.require_page(&tab, task_id)?;
        *tab.shared.owner.task_id.borrow_mut() = task_id.map(str::to_string);
        let result = webkit::evaluate(&tab.view, &self.guarded(&action_script(action), task_id)).await?;
        self.require_page(&tab, task_id)?;
        Ok(result.as_str().unwrap_or_default().to_string())
    }

    /// Searches a page, stepping to the next match when the query is one the page is already showing.
    pub async fn find_in_page(&self, tab_id: &str, query: &str, forward: bool, find_next: bool) {
        let Some(tab) = self.tab(tab_id) else { return };
        let previous = tab.find.borrow().clone();
        let fresh = !find_next || previous.query != query;
        let matches = if fresh {
            webkit::stop_find(&tab.view);
            webkit::evaluate(&tab.view, &count_matches_script(query)).await.ok().and_then(|count| count.as_i64()).unwrap_or(0)
        } else {
            previous.matches
        };
        let found = !query.is_empty() && webkit::find(&tab.view, query, !forward).await;
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
        webkit::stop_find(&tab.view);
    }

    /// Signs the whole app out of everything the panel has ever logged into.
    pub async fn clear_data(&self) {
        self.inner.permissions.borrow_mut().clear_origins();
        webkit::clear(&self.inner.data).await;
        let tabs: Vec<Rc<Tab>> = self.inner.tabs.borrow().clone();
        for tab in tabs {
            tab.view.reload();
        }
    }

    /// Replaces those sites' cookies in the panel's session with these, then reloads the pages on
    /// those sites. Nothing is removed when there is nothing to set. Answers how many were set.
    pub async fn replace_cookies(&self, cookies: &[CookieDetails], sites: &[String]) -> usize {
        if !cookies.is_empty() {
            webkit::delete_site_cookies(&self.inner.data, sites).await;
        }
        let mut added = 0;
        for cookie in cookies {
            if webkit::set_cookie(&self.inner.data, cookie).await {
                added += 1;
            }
        }
        let tabs: Vec<Rc<Tab>> = self.inner.tabs.borrow().clone();
        for tab in tabs {
            let host = url::Url::parse(&tab.url()).ok().and_then(|url| url.host_str().map(str::to_string));
            if host.is_some_and(|host| sites.iter().any(|site| cookie_on_site(&host, site))) {
                tab.view.reload();
            }
        }
        added
    }
}

/// The message a failed load reads as, or None for a load that was only replaced or refused.
fn failure_message(error: &glib::Error, failing: &str) -> Option<String> {
    // A navigation cut short by another, or refused by the panel itself, is not a failure to show.
    if error.matches(webkit2gtk::NetworkError::Cancelled) || error.matches(webkit2gtk::PolicyError::FrameLoadInterruptedByPolicyChange) || error.matches(webkit2gtk::PolicyError::CannotShowMimeType) {
        return None;
    }
    let description = error.message().to_string();
    Some(if failing.is_empty() { description } else { format!("{description} ({failing})") })
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
    opener: &webkit2gtk::WebView,
    owner: &Rc<Owner>,
    permissions: &Rc<RefCell<Permissions>>,
    events: &UnboundedSender<BrowserEvent>,
    popups: &Rc<RefCell<Vec<gtk::Window>>>,
    url: &str,
) -> Option<gtk::Widget> {
    if !allow_navigation(permissions, owner, url, events) {
        return None;
    }
    popups.borrow_mut().retain(|popup| popup.is_visible());
    let child = Rc::new(Owner { id: owner.id.clone(), task_id: RefCell::new(owner.task_id.borrow().clone()) });
    let view = webkit2gtk::WebView::builder().related_view(opener).build();
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_default_size(POPUP_SIZE.0, POPUP_SIZE.1);
    window.set_position(gtk::WindowPosition::Center);
    window.add(&view);
    {
        let (child, permissions, events) = (child.clone(), permissions.clone(), events.clone());
        view.connect_decide_policy(move |_, decision, kind| {
            if kind == PolicyDecisionType::Response {
                let Some(response) = decision.downcast_ref::<ResponsePolicyDecision>() else { return false };
                let url = response.response().and_then(|answer| answer.uri()).map(|uri| uri.to_string()).unwrap_or_default();
                if !response.is_main_frame_main_resource() || !is_web_url(&url) || page_may_start(&permissions, &child, &url) {
                    return false;
                }
                allow_navigation(&permissions, &child, &url, &events);
                decision.ignore();
                return true;
            }
            if kind != PolicyDecisionType::NavigationAction {
                return false;
            }
            let Some(navigation) = decision.downcast_ref::<NavigationPolicyDecision>() else { return false };
            let Some(mut action) = navigation.navigation_action() else { return false };
            let target = navigation_target(&mut action);
            let url = action.request().and_then(|request| request.uri()).map(|uri| uri.to_string()).unwrap_or_default();
            let frame_load = match target {
                Target::Frame => true,
                Target::Unknown => !page_may_start(&permissions, &child, &url) && (frame_scheme(&url) || is_web_url(&url)),
                Target::Page => false,
            };
            if frame_load || allow_navigation(&permissions, &child, &url, &events) {
                decision.use_();
            } else {
                decision.ignore();
            }
            true
        });
    }
    {
        let window = window.clone();
        view.connect_ready_to_show(move |_| window.show_all());
    }
    {
        let window = window.clone();
        view.connect_close(move |_| unsafe { window.destroy() });
    }
    {
        let child = child.clone();
        view.connect_button_press_event(move |_, _| {
            *child.task_id.borrow_mut() = None;
            glib::Propagation::Proceed
        });
    }
    popups.borrow_mut().push(window);
    Some(view.upcast())
}
