//! The browser tab in the dock (BrowserPanel.tsx): a bar to move through the page's history and
//! type an address, the ask when a run wants a site, and the area the page is drawn over.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use act_core::commands::AppCommand;
use act_core::domain::platform::{browser_tab_title, BrowserApproval, BrowserTab};
use act_ui::color::{alpha, mix};
use act_ui::components::button::{Button, ButtonSize, ButtonVariant, IconButton};
use act_ui::components::empty_state::EmptyState;
use act_ui::components::field::TextField;
use act_ui::components::notice::{Notice, NoticeTone};
use act_ui::components::panel_header::toolbar;
use act_ui::components::spinner::Spinning;
use act_ui::input::{InputEvent, TextInput};
use act_ui::style::{Corners, Raised};
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::shortcut_tip;
use act_ui::{Icon, IconName};
use gpui::{
    actions, canvas, div, img, linear_color_stop, linear_gradient, prelude::*, px, AnyView, App, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight,
    HighlightStyle, KeyBinding, KeyDownEvent, KeyUpEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, Point, RenderImage,
    ScrollEvent, SharedString, StyledText, Subscription, Window,
};

use crate::host::BrowserHost;
use crate::types::{BrowserEvent, OptionChoice, PageBounds, PageInput, PageRect, PointerKind};

actions!(browser_panel, [GoBack, GoForward, Reload, FocusAddress]);

pub const KEY_CONTEXT: &str = "BrowserPanel";

/// The panel's own keys, live while the address bar or the page holds the keyboard.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    let [back, forward, reload, address] = if cfg!(target_os = "macos") { ["cmd-[", "cmd-]", "cmd-r", "cmd-l"] } else { ["ctrl-[", "ctrl-]", "ctrl-r", "ctrl-l"] };
    cx.bind_keys([
        KeyBinding::new(back, GoBack, context),
        KeyBinding::new(forward, GoForward, context),
        KeyBinding::new(reload, Reload, context),
        KeyBinding::new(address, FocusAddress, context),
    ]);
}

type Dispatch = Rc<dyn Fn(AppCommand, &mut Window, &mut App)>;

pub struct BrowserPanel {
    host: Entity<BrowserHost>,
    tab: BrowserTab,
    approval: Option<BrowserApproval>,
    address: Entity<TextInput>,
    editing: bool,
    /// What the panel last put in the address bar itself, which is no edit of the user's.
    shown_address: String,
    focus_token: u64,
    page_focus: FocusHandle,
    find: Option<AnyView>,
    shown: bool,
    covered: bool,
    /// The page area, in the window's layout pixels, as last laid out.
    viewport: Rc<Cell<Bounds<Pixels>>>,
    /// The frame last painted of a page the app paints, given back to GPUI once replaced.
    painted: Option<Arc<RenderImage>>,
    pointer_inside: bool,
    /// A button went down on a page the app paints and has not come up yet.
    page_pressed: bool,
    /// A page's drop-down list, shown by the app for a page it paints.
    option_menu: Option<(Entity<act_ui::components::menu::Menu>, Subscription)>,
    dispatch: Dispatch,
    _subscriptions: Vec<Subscription>,
}

fn blank_tab() -> BrowserTab {
    BrowserTab { id: String::new(), url: String::new(), title: String::new(), loading: false, can_go_back: false, can_go_forward: false, error: None }
}

impl BrowserPanel {
    /// `dispatch` receives the commands the panel's controls make.
    pub fn new(host: Entity<BrowserHost>, dispatch: impl Fn(AppCommand, &mut Window, &mut App) + 'static, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| TextInput::new(cx).label("Address").placeholder("Enter a web address"));
        let address_events = cx.subscribe_in(&address, window, |this, _, event: &InputEvent, window, cx| this.address_event(event, window, cx));
        let page_events = cx.subscribe_in(&host, window, |this, _, event: &BrowserEvent, window, cx| match event {
            BrowserEvent::PageFocused { tab_id } if *tab_id == this.tab.id => {
                this.page_focus.focus(window, cx);
                cx.notify();
            }
            BrowserEvent::PageFrame { tab_id } if *tab_id == this.tab.id => cx.notify(),
            BrowserEvent::OptionMenu { tab_id, choices, anchor } if *tab_id == this.tab.id => this.show_options(choices, *anchor, window, cx),
            BrowserEvent::OptionMenuClosed { tab_id } if *tab_id == this.tab.id => {
                this.option_menu = None;
                cx.notify();
            }
            _ => {}
        });
        let page_focus = cx.focus_handle();
        let focused = cx.on_focus(&page_focus, window, |this, _, cx| this.host.read(cx).page_input(&this.tab.id, &PageInput::Focus(true)));
        let blurred = cx.on_blur(&page_focus, window, |this, _, cx| this.host.read(cx).page_input(&this.tab.id, &PageInput::Focus(false)));
        Self {
            host,
            tab: blank_tab(),
            approval: None,
            address,
            editing: false,
            shown_address: String::new(),
            focus_token: 0,
            page_focus,
            find: None,
            shown: true,
            covered: false,
            viewport: Rc::new(Cell::new(Bounds::default())),
            painted: None,
            pointer_inside: false,
            page_pressed: false,
            option_menu: None,
            dispatch: Rc::new(dispatch),
            _subscriptions: vec![address_events, page_events, focused, blurred],
        }
    }

    /// The tab this panel shows, and the navigation it is waiting on the user to answer, if any.
    pub fn set_tab(&mut self, tab: BrowserTab, approval: Option<BrowserApproval>, cx: &mut Context<Self>) {
        let moved = tab.id != self.tab.id || tab.url != self.tab.url;
        if tab != self.tab || approval != self.approval {
            self.tab = tab;
            self.approval = approval;
            if moved && !self.editing {
                self.show_address(cx);
            }
            cx.notify();
        }
    }

    pub fn tab(&self) -> &BrowserTab {
        &self.tab
    }

    /// The find bar, when it is this page being searched. It sits above the page rather than over it.
    pub fn set_find(&mut self, find: Option<AnyView>, cx: &mut Context<Self>) {
        self.find = find;
        cx.notify();
    }

    /// Whether the dock is showing this panel. A hidden panel takes its page off the window.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.shown != shown {
            self.shown = shown;
            if !shown {
                self.host.read(cx).set_bounds(None);
            }
            cx.notify();
        }
    }

    /// Whether something the app draws (a menu, a dialog) lies over the page area. A page that is a
    /// native view above everything the app draws steps aside while anything does.
    pub fn set_covered(&mut self, covered: bool, cx: &mut Context<Self>) {
        if self.covered != covered {
            self.covered = covered;
            if covered && !self.host.read(cx).paints_pages() {
                self.host.read(cx).set_bounds(None);
            }
            cx.notify();
        }
    }

    /// Bumped whenever something asks this tab to take the keyboard: a tab with a page hands it to
    /// the page, a tab without one to the address bar.
    pub fn request_focus(&mut self, token: u64, window: &mut Window, cx: &mut Context<Self>) {
        if token == 0 || token == self.focus_token {
            return;
        }
        self.focus_token = token;
        if self.tab.url.is_empty() {
            self.focus_address(window, cx);
        } else {
            self.page_focus.focus(window, cx);
            if let Some(pages) = self.host.read(cx).pages() {
                pages.focus_tab(&self.tab.id);
            }
        }
    }

    fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.host.read(cx).focus_app();
        self.address.update(cx, |input, cx| input.focus(window, cx));
        self.address.update(cx, |input, cx| input.select_all_text(cx));
    }

    fn address_event(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Focus => {
                self.host.read(cx).focus_app();
                self.editing = true;
                self.address.update(cx, |input, cx| input.select_all_text(cx));
            }
            InputEvent::Changed => {
                let address = self.address.read(cx);
                if address.is_focused(window) && address.text() != self.shown_address {
                    self.editing = true;
                }
            }
            InputEvent::Submit => {
                let typed = self.address.read(cx).text().to_string();
                self.editing = false;
                self.show_address(cx);
                if !typed.trim().is_empty() {
                    let command = AppCommand::BrowserOpen { task_id: None, url: typed, tab_id: Some(self.tab.id.clone()), new_tab: None };
                    self.send(command, window, cx);
                }
            }
            InputEvent::Blur => self.stop_editing(window, cx),
        }
        cx.notify();
    }

    fn show_address(&mut self, cx: &mut Context<Self>) {
        let url = self.tab.url.clone();
        self.shown_address = url.clone();
        self.address.update(cx, |input, cx| input.reset(&url, cx));
    }

    fn stop_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = false;
        self.show_address(cx);
        if self.address.read(cx).is_focused(window) {
            self.page_focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Commands leave once this panel's update is over, so whoever handles them may update it.
    fn send(&self, command: AppCommand, window: &mut Window, cx: &mut Context<Self>) {
        let dispatch = self.dispatch.clone();
        window.defer(cx, move |window, cx| dispatch(command, window, cx));
    }

    fn go(&mut self, delta: i8, window: &mut Window, cx: &mut Context<Self>) {
        let command = AppCommand::BrowserGo { task_id: None, tab_id: Some(self.tab.id.clone()), delta };
        self.send(command, window, cx);
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab.url.is_empty() {
            return;
        }
        let command = AppCommand::BrowserReload { task_id: None, tab_id: Some(self.tab.id.clone()) };
        self.send(command, window, cx);
    }

    fn decide(&mut self, allow: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(approval) = &self.approval else { return };
        let command = AppCommand::BrowserDecide { allow, approval_id: approval.approval_id.clone() };
        self.send(command, window, cx);
    }

    /// Whether the page is drawn: only once there is a page, and not while the panel's own
    /// message about it stands in its place.
    fn page_drawn(&self, painted_by_app: bool) -> bool {
        self.shown && (!self.covered || painted_by_app) && !self.tab.url.is_empty() && self.tab.error.is_none()
    }

    /// Where a window position falls on a page the app paints, in the page's device pixels.
    fn page_point(&self, position: Point<Pixels>, window: &Window) -> (f64, f64) {
        let origin = self.viewport.get().origin;
        let scale = f64::from(gpui::page_zoom()) * f64::from(window.scale_factor());
        (f64::from(position.x - origin.x) * scale, f64::from(position.y - origin.y) * scale)
    }

    fn page_pointer(&mut self, kind: PointerKind, position: Point<Pixels>, button: Option<MouseButton>, modifiers: Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        let (x, y) = self.page_point(position, window);
        let button = match button {
            Some(MouseButton::Left) | None => 1,
            Some(MouseButton::Middle) => 2,
            Some(MouseButton::Right) => 3,
            Some(MouseButton::Navigate(gpui::NavigationDirection::Back)) => 8,
            Some(MouseButton::Navigate(gpui::NavigationDirection::Forward)) => 9,
        };
        self.host.read(cx).page_input(&self.tab.id, &PageInput::Pointer { kind, x, y, button, modifiers });
    }

    /// Shows a page's drop-down list under the control that opened it.
    fn show_options(&mut self, choices: &[OptionChoice], anchor: PageRect, window: &mut Window, cx: &mut Context<Self>) {
        use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
        use act_ui::overlay::{Align, Placement};
        let scale = f64::from(gpui::page_zoom()) * f64::from(window.scale_factor());
        let origin = self.viewport.get().origin;
        let anchor = Bounds::new(
            gpui::point(origin.x + px((anchor.x / scale) as f32), origin.y + px((anchor.y / scale) as f32)),
            gpui::size(px((anchor.width / scale) as f32), px((anchor.height / scale) as f32)),
        );
        let chosen = Rc::new(Cell::new(false));
        let entries: Vec<MenuEntry> = choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                if choice.heading {
                    return MenuEntry::Heading(choice.label.clone().into());
                }
                let (host, tab_id, chosen) = (self.host.clone(), self.tab.id.clone(), chosen.clone());
                MenuItem::new(choice.label.clone())
                    .checked(choice.selected)
                    .disabled(!choice.enabled)
                    .on_select(move |_, cx| {
                        chosen.set(true);
                        host.read(cx).page_option(&tab_id, Some(index));
                    })
                    .into()
            })
            .collect();
        let start = choices.iter().position(|choice| choice.selected);
        let menu = Menu::build(entries, Placement::Anchor { anchor, align: Align::Start }, window, cx, |menu| menu.checkable(true).min_width(anchor.size.width).highlighted(start));
        let subscription = cx.subscribe(&menu, move |this, _, _: &gpui::DismissEvent, cx| {
            if !chosen.get() {
                this.host.read(cx).page_option(&this.tab.id, None);
            }
            this.option_menu = None;
            cx.notify();
        });
        self.option_menu = Some((menu, subscription));
        cx.notify();
    }

    fn page_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.page_pressed) || self.viewport.get().contains(&event.position) {
            self.page_pointer(PointerKind::Up, event.position, Some(event.button), event.modifiers, window, cx);
        }
    }

    fn page_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.page_focus.focus(window, cx);
        self.page_pressed = true;
        self.host.read(cx).page_input(&self.tab.id, &PageInput::Focus(true));
        self.page_pointer(PointerKind::Down, event.position, Some(event.button), event.modifiers, window, cx);
    }

    fn bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = &self.tab;
        let modifier = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" };
        let back = IconButton::new("browser-back", IconName::ArrowLeft, "Back")
            .icon_size(16.0)
            .tip(shortcut_tip("Back", &format!("{modifier}[")))
            .disabled(!tab.can_go_back)
            .on_click(cx.listener(|this, _, window, cx| this.go(-1, window, cx)));
        let forward = IconButton::new("browser-forward", IconName::ArrowRight, "Forward")
            .icon_size(16.0)
            .tip(shortcut_tip("Forward", &format!("{modifier}]")))
            .disabled(!tab.can_go_forward)
            .on_click(cx.listener(|this, _, window, cx| this.go(1, window, cx)));
        let mut reload = IconButton::new("browser-reload", IconName::RotateCw, "Reload")
            .icon_size(16.0)
            .tip(shortcut_tip("Reload", &format!("{modifier}R")))
            .disabled(tab.url.is_empty())
            .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx)));
        if tab.loading {
            reload = reload.glyph(Spinning::new("browser-reload-spin", IconName::RotateCw).size(px(16.0)));
        }
        let field = div()
            .id("browser-address")
            .flex_1()
            .min_w_0()
            .ml(px(4.0))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.editing {
                    cx.stop_propagation();
                    this.stop_editing(window, cx);
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                if this.editing {
                    this.stop_editing(window, cx);
                }
            }))
            .child(TextField::new("browser-address-field", &self.address));
        toolbar(cx).flex_none().child(back).child(forward).child(reload).child(field)
    }

    fn approval_notice(&self, approval: &BrowserApproval, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let site = browser_tab_title(&BrowserTab { url: approval.url.clone(), ..blank_tab() });
        let before = "The agent wants to open ";
        let text = format!("{before}{site}. It browses with every login you have here.");
        let bold = HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), color: Some(t.ink.into()), ..Default::default() };
        let message = StyledText::new(text).with_highlights([(before.len()..before.len() + site.len(), bold)]);
        let pool = linear_gradient(90.0, linear_color_stop(mix(t.accent, 0.12, t.glass_menu.fill), 0.0), linear_color_stop(t.glass_menu.fill, 0.6));
        div()
            .relative()
            .flex_none()
            .mx(px(10.0))
            .mt(px(8.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .rounded(px(11.0))
            .border_1()
            .border_color(alpha(t.accent, 0.32))
            .bg(pool)
            .pl(px(11.0))
            .pr(px(7.0))
            .py(px(7.0))
            .text_size(px(theme.metrics.text_meta))
            .text_color(t.ink_dim)
            .rim(t.glass_rim, Corners::Radius(11.0))
            .child(div().flex_none().text_color(t.accent_text).child(Icon::new(IconName::ShieldAlert).size(px(16.0))))
            .child(div().flex_1().min_w_0().child(message))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(Button::new("browser-block", "Block").on_click(cx.listener(|this, _, window, cx| this.decide(false, window, cx))))
                    .child(Button::new("browser-allow", "Always allow this site").variant(ButtonVariant::Danger).on_click(cx.listener(|this, _, window, cx| this.decide(true, window, cx)))),
            )
    }

    fn viewport(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self.host.clone();
        let painted_by_app = host.read(cx).paints_pages();
        let cursor = host.read(cx).page_cursor(&self.tab.id);
        let drawn = self.page_drawn(painted_by_app);
        let frame = drawn.then(|| host.read(cx).page_frame(&self.tab.id)).flatten();
        if self.painted.as_ref().map(|image| image.id) != frame.as_ref().map(|image| image.id) {
            if let Some(replaced) = std::mem::replace(&mut self.painted, frame.clone()) {
                let _ = window.drop_image(replaced);
            }
        }
        let theme = cx.theme();
        let t = &theme.tokens;
        let viewport_bounds = self.viewport.clone();
        let reporter = canvas(
            move |bounds: Bounds<Pixels>, window, cx| {
                viewport_bounds.set(bounds);
                #[cfg(target_os = "linux")]
                if let Some(pages) = host.read(cx).pages() {
                    pages.set_scale(f64::from(window.scale_factor()));
                }
                #[cfg(not(target_os = "linux"))]
                let _ = window;
                // The page is a native view, placed in window points rather than zoomed layout pixels.
                let zoom = gpui::page_zoom() as f64;
                let page = drawn.then(|| PageBounds { x: f64::from(bounds.origin.x) * zoom, y: f64::from(bounds.origin.y) * zoom, width: f64::from(bounds.size.width) * zoom, height: f64::from(bounds.size.height) * zoom });
                host.read(cx).set_bounds(page.filter(|page| page.width >= 1.0 && page.height >= 1.0));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        let mut viewport = div().id("browser-viewport").relative().flex_1().min_h_0().bg(t.dock_sunken).child(reporter);
        if painted_by_app && drawn {
            if let Some(frame) = frame {
                viewport = viewport.child(img(frame).absolute().inset_0().size_full().object_fit(ObjectFit::Fill));
            }
            viewport = viewport.cursor(cursor);
            viewport = viewport
                .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, window, cx| this.page_mouse_down(event, window, cx)))
                .on_mouse_down(MouseButton::Middle, cx.listener(|this, event: &MouseDownEvent, window, cx| this.page_mouse_down(event, window, cx)))
                .on_mouse_down(MouseButton::Right, cx.listener(|this, event: &MouseDownEvent, window, cx| this.page_mouse_down(event, window, cx)))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, event: &MouseUpEvent, window, cx| this.page_mouse_up(event, window, cx)))
                .on_mouse_up(MouseButton::Middle, cx.listener(|this, event: &MouseUpEvent, window, cx| this.page_mouse_up(event, window, cx)))
                .on_mouse_up(MouseButton::Right, cx.listener(|this, event: &MouseUpEvent, window, cx| this.page_mouse_up(event, window, cx)))
                .on_mouse_up_out(MouseButton::Left, cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    if this.page_pressed {
                        this.page_mouse_up(event, window, cx);
                    }
                }))
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    let inside = this.viewport.get().contains(&event.position);
                    if inside || (this.page_pressed && event.pressed_button.is_some()) {
                        this.pointer_inside = inside;
                        this.page_pointer(PointerKind::Move, event.position, event.pressed_button, event.modifiers, window, cx);
                    } else if std::mem::take(&mut this.pointer_inside) {
                        this.host.read(cx).page_input(&this.tab.id, &PageInput::Leave);
                    }
                }))
                .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                    let (x, y) = this.page_point(event.position, window);
                    this.host.read(cx).page_input(&this.tab.id, &PageInput::Scroll { x, y, delta: event.delta, modifiers: event.modifiers });
                    cx.stop_propagation();
                }));
        }
        if self.tab.url.is_empty() {
            viewport = viewport.child(
                div().absolute().inset_0().flex().flex_col().child(
                    EmptyState::new()
                        .icon(IconName::Globe)
                        .title("No page open")
                        .detail("Open a page here and every thread can read it, signed in as you, in one session for the whole app.")
                        .action(Button::new("browser-type-address", "Type an address").variant(ButtonVariant::Soft).size(ButtonSize::Lg).on_click(cx.listener(|this, _, window, cx| this.focus_address(window, cx)))),
                ),
            );
        }
        if let Some(error) = &self.tab.error {
            let mut notice = Notice::new("browser-error", error.clone()).tone(NoticeTone::Danger);
            if !self.tab.url.is_empty() {
                notice = notice.action(
                    Button::new("browser-retry", "Try again").size(ButtonSize::Sm).variant(ButtonVariant::Soft).on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
                );
            }
            viewport = viewport.child(div().absolute().left(px(12.0)).right(px(12.0)).bottom(px(12.0)).child(notice));
        }
        viewport
    }
}

impl Focusable for BrowserPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.page_focus.clone()
    }
}

impl Render for BrowserPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let approval = self.approval.clone();
        div()
            .id(SharedString::from(format!("browser-panel-{}", self.tab.id)))
            .key_context(KEY_CONTEXT)
            .track_focus(&self.page_focus)
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &GoBack, window, cx| {
                if this.tab.can_go_back {
                    this.go(-1, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &GoForward, window, cx| {
                if this.tab.can_go_forward {
                    this.go(1, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Reload, window, cx| this.reload(window, cx)))
            .on_action(cx.listener(|this, _: &FocusAddress, window, cx| this.focus_address(window, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.page_focus.is_focused(window) && this.host.read(cx).page_key(&this.tab.id, true, &event.keystroke) {
                    cx.stop_propagation();
                }
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                if this.page_focus.is_focused(window) && this.host.read(cx).page_key(&this.tab.id, false, &event.keystroke) {
                    cx.stop_propagation();
                }
            }))
            .child(self.bar(cx))
            .children(self.find.clone())
            .children(approval.map(|approval| self.approval_notice(&approval, cx)))
            .child(self.viewport(window, cx))
            .children(self.option_menu.as_ref().map(|(menu, _)| menu.clone()))
    }
}
