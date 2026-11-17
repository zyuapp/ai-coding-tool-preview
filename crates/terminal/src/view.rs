//! The panel's terminal: an entity that paints a [`Screen`] and turns the user's keys, mouse, and
//! clipboard into input for the shell.
//!
//! The view never writes to a shell itself. Input leaves as [`TerminalViewEvent::Input`] and the
//! grid it fits as [`TerminalViewEvent::Resize`], so both can travel as app commands.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use futures::future::LocalBoxFuture;
use futures::StreamExt;
use gpui::{
    actions, div, prelude::*, px, App, Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollDelta, ScrollEvent, Subscription, Task, UTF16Selection, Window,
};

use crate::contract::{TerminalOutputRead, TerminalReadKind};
use crate::element::TerminalElement;
use crate::keys::{self, KeyResult};
use crate::mouse::{self, MouseReport, ReportAction, ReportButton};
use crate::screen::Screen;
use crate::search::{FindResults, Search};
use crate::theme::{TerminalFont, TerminalTheme};

actions!(terminal, [Copy, Paste, SelectAll]);

/// The key context the view sets, so app bindings can leave the shell's keys alone.
pub const KEY_CONTEXT: &str = "Terminal";

/// `.terminal-viewport`'s padding: top, right, bottom, left.
pub const VIEWPORT_PADDING: [f32; 4] = [8., 4., 8., 10.];

/// The width xterm keeps for its scrollbar, which the grid never covers.
pub const SCROLLBAR_WIDTH: Pixels = px(14.);

/// How long the scrollbar stays after a scroll, and how long it then takes to fade.
const SCROLLBAR_LINGER: Duration = Duration::from_millis(500);
const SCROLLBAR_FADE: Duration = Duration::from_millis(800);
/// How long output must settle before a search's highlights are rebuilt.
const SEARCH_REFRESH: Duration = Duration::from_millis(200);
const DRAG_SCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// Binds copy, paste, and select-all inside the terminal.
pub fn init(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-c", Copy, context),
            KeyBinding::new("cmd-v", Paste, context),
            KeyBinding::new("cmd-a", SelectAll, context),
        ]);
    } else {
        cx.bind_keys([
            KeyBinding::new("ctrl-shift-c", Copy, context),
            KeyBinding::new("ctrl-shift-v", Paste, context),
            KeyBinding::new("shift-insert", Paste, context),
        ]);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalViewEvent {
    /// Bytes for the shell: keys, pastes, mouse and focus reports, IME text.
    Input(String),
    /// The grid the view fits, first reported once it is laid out.
    Resize { cols: u16, rows: u16 },
    /// What a search found.
    FindResults(FindResults),
    /// A followed remote shell's connection: `None` once it is live.
    RemoteStatus(Option<String>),
}

/// Reads a paired computer's shell: everything after a sequence, or a snapshot when `None`.
pub type RemoteRead = Rc<dyn Fn(Option<u64>) -> LocalBoxFuture<'static, Result<Option<TerminalOutputRead>, String>>>;

/// Where the grid was painted, so pointer positions can be turned into cells.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GridLayout {
    pub bounds: Bounds<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub cols: usize,
    pub rows: usize,
    pub display_offset: usize,
    pub history: usize,
    pub scrollbar: Option<ScrollbarLayout>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScrollbarLayout {
    pub track: Bounds<Pixels>,
    pub thumb: Bounds<Pixels>,
}

impl GridLayout {
    /// The cell under a point, clamped to the grid, with the half of the cell it falls in.
    pub fn cell_at(&self, position: Point<Pixels>) -> (usize, usize, Side) {
        let x = (position.x - self.bounds.origin.x).max(px(0.));
        let y = (position.y - self.bounds.origin.y).max(px(0.));
        let col = ((x / self.cell_width) as usize).min(self.cols.saturating_sub(1));
        let row = ((y / self.line_height) as usize).min(self.rows.saturating_sub(1));
        let within = x - self.cell_width * col as f32;
        let side = if within > self.cell_width / 2. { Side::Right } else { Side::Left };
        (col, row, side)
    }

    pub fn grid_point(&self, col: usize, row: usize) -> GridPoint {
        GridPoint::new(Line(row as i32 - self.display_offset as i32), Column(col))
    }
}

struct SelectionDrag {
    /// How far past the grid's top (negative) or bottom the pointer is, in pixels.
    overshoot: f32,
    last_position: Point<Pixels>,
    scrolling: Option<Task<()>>,
}

struct ScrollbarDrag {
    grab: Pixels,
}

pub struct TerminalView {
    screen: Arc<Screen>,
    focus_handle: FocusHandle,
    theme: TerminalTheme,
    font: TerminalFont,
    is_mac: bool,
    option_is_meta: bool,
    input_enabled: bool,
    pub(crate) layout: Option<GridLayout>,
    reported_size: Option<(u16, u16)>,
    pub(crate) search: Search,
    search_refresh: Option<Task<()>>,
    pub(crate) marked_text: Option<String>,
    selection_drag: Option<SelectionDrag>,
    reporting_button: Option<ReportButton>,
    last_report: Option<(usize, usize, ReportButton)>,
    wheel_lines: f32,
    viewport_pixels: f32,
    scrollbar_drag: Option<ScrollbarDrag>,
    pub(crate) hovered: bool,
    pub(crate) scrollbar_hovered: bool,
    scrollbar_hide_at: Option<Instant>,
    last_scroll_top: Option<usize>,
    remote: Option<Task<()>>,
    remote_live: bool,
    _watch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalView {
    pub fn new(screen: Arc<Screen>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus_handle, window, |this, _, cx| this.focus_changed(true, cx)),
            cx.on_blur(&focus_handle, window, |this, _, cx| this.focus_changed(false, cx)),
            cx.observe_window_activation(window, |_, _, cx| cx.notify()),
        ];
        let theme = TerminalTheme::default();
        screen.set_theme(&theme);
        Self {
            _watch: Self::watch(&screen, cx),
            screen,
            focus_handle,
            theme,
            font: TerminalFont::default(),
            is_mac: cfg!(target_os = "macos"),
            option_is_meta: false,
            input_enabled: true,
            layout: None,
            reported_size: None,
            search: Search::default(),
            search_refresh: None,
            marked_text: None,
            selection_drag: None,
            reporting_button: None,
            last_report: None,
            wheel_lines: 0.,
            viewport_pixels: 0.,
            scrollbar_drag: None,
            hovered: false,
            scrollbar_hovered: false,
            scrollbar_hide_at: None,
            last_scroll_top: None,
            remote: None,
            remote_live: false,
            _subscriptions: subscriptions,
        }
    }

    fn watch(screen: &Arc<Screen>, cx: &mut Context<Self>) -> Task<()> {
        let mut changes = screen.subscribe();
        cx.spawn(async move |this, cx| {
            while changes.next().await.is_some() {
                // A burst of output wakes the view once.
                while let Ok(()) = changes.try_recv() {}
                let alive = this.update(cx, |this, cx| {
                    this.screen_changed(cx);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
    }

    pub fn screen(&self) -> &Arc<Screen> {
        &self.screen
    }

    /// Shows another screen, such as a restarted shell's. Selection, search, and the grid it was
    /// told about start over.
    pub fn set_screen(&mut self, screen: Arc<Screen>, cx: &mut Context<Self>) {
        screen.set_theme(&self.theme);
        self._watch = Self::watch(&screen, cx);
        self.screen = screen;
        self.reported_size = None;
        self.search.clear();
        self.search_refresh = None;
        self.last_scroll_top = None;
        cx.notify();
    }

    /// Repaints in a theme's colours.
    pub fn set_theme(&mut self, theme: TerminalTheme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.screen.set_theme(&theme);
            self.theme = theme;
            cx.notify();
        }
    }

    pub fn theme(&self) -> &TerminalTheme {
        &self.theme
    }

    /// Redraws in another face or size; the new grid is reported once laid out.
    pub fn set_font(&mut self, font: TerminalFont, cx: &mut Context<Self>) {
        if self.font != font {
            self.font = font;
            cx.notify();
        }
    }

    pub fn font(&self) -> &TerminalFont {
        &self.font
    }

    /// Makes Option act as Meta on macOS, sending ESC-prefixed keys rather than composed characters.
    pub fn set_option_is_meta(&mut self, option_is_meta: bool) {
        self.option_is_meta = option_is_meta;
    }

    /// The grid last reported, once the view has been laid out.
    pub fn size(&self) -> Option<(u16, u16)> {
        self.reported_size
    }

    /// Whether input reaches the shell. A followed remote shell takes input only while live.
    pub fn set_input_enabled(&mut self, enabled: bool) {
        self.input_enabled = enabled;
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    /// Steps to the next match, or the one before it, highlighting every match.
    pub fn find(&mut self, query: &str, forward: bool, cx: &mut Context<Self>) {
        let results = {
            let mut term = self.screen.lock();
            self.search.find(&mut term, query, forward, false)
        };
        cx.emit(TerminalViewEvent::FindResults(results));
        cx.notify();
    }

    /// Removes every highlight. The match last selected stays selected.
    pub fn clear_find(&mut self, cx: &mut Context<Self>) {
        self.search.clear();
        self.search_refresh = None;
        cx.notify();
    }

    /// The selected text, as copy would put it on the clipboard.
    pub fn selection_text(&self) -> Option<String> {
        self.screen.lock().selection_to_string().filter(|text| !text.is_empty())
    }

    /// Follows a paired computer's shell into this view's screen until stopped or closed. Input
    /// only leaves while the view is live, so neither retries nor reconnections send keys.
    pub fn follow_remote(&mut self, read: RemoteRead, cx: &mut Context<Self>) {
        self.remote_live = false;
        cx.emit(TerminalViewEvent::RemoteStatus(Some("Connecting…".into())));
        let screen = self.screen.clone();
        self.remote = Some(cx.spawn(async move |this, cx| {
            let mut after = None;
            loop {
                let result = read(after).await;
                let status = match result {
                    Ok(None) => {
                        let _ = this.update(cx, |this, cx| {
                            this.remote_live = false;
                            cx.emit(TerminalViewEvent::RemoteStatus(Some("This terminal has closed.".into())));
                        });
                        return;
                    }
                    Ok(Some(read)) => {
                        if read.kind == TerminalReadKind::Snapshot {
                            let _ = this.update(cx, |this, _| this.remote_live = false);
                        }
                        screen.apply_read(&read);
                        after = Some(read.sequence);
                        None
                    }
                    Err(error) => {
                        after = None;
                        Some(error)
                    }
                };
                let failed = status.is_some();
                let alive = this.update(cx, |this, cx| {
                    this.remote_live = !failed;
                    cx.emit(TerminalViewEvent::RemoteStatus(status));
                });
                if alive.is_err() {
                    return;
                }
                if failed {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                }
            }
        }));
    }

    pub fn stop_remote(&mut self) {
        self.remote = None;
        self.remote_live = false;
    }

    fn input_allowed(&self) -> bool {
        self.input_enabled && (self.remote.is_none() || self.remote_live)
    }

    /// Input the user typed, pasted, or scrolled: it brings the prompt back into view and drops
    /// the selection, as xterm does.
    fn send_user_input(&mut self, text: String, cx: &mut Context<Self>) {
        {
            let mut term = self.screen.lock();
            if term.grid().display_offset() != 0 {
                term.scroll_display(Scroll::Bottom);
            }
            term.selection = None;
        }
        self.send(text, cx);
        cx.notify();
    }

    fn send(&mut self, text: String, cx: &mut Context<Self>) {
        if !text.is_empty() && self.input_allowed() {
            cx.emit(TerminalViewEvent::Input(text));
        }
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        if self.screen.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
            self.send(if focused { "\x1b[I" } else { "\x1b[O" }.into(), cx);
        }
        cx.notify();
    }

    fn screen_changed(&mut self, cx: &mut Context<Self>) {
        if !self.search.is_active() {
            return;
        }
        self.search_refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_REFRESH).await;
            let _ = this.update(cx, |this, cx| {
                let results = {
                    let mut term = this.screen.lock();
                    this.search.refresh(&mut term)
                };
                if let Some(results) = results {
                    cx.emit(TerminalViewEvent::FindResults(results));
                    cx.notify();
                }
            });
        }));
    }

    /// Called by the element once it knows the grid that fits; reports it when it changed.
    pub(crate) fn laid_out(&mut self, layout: GridLayout, fit: (u16, u16), cx: &mut Context<Self>) {
        self.layout = Some(layout);
        let top = layout.history - layout.display_offset;
        if self.last_scroll_top.is_some_and(|last| last != top) {
            self.reveal_scrollbar();
        }
        self.last_scroll_top = Some(top);
        if self.reported_size != Some(fit) {
            self.reported_size = Some(fit);
            cx.emit(TerminalViewEvent::Resize { cols: fit.0, rows: fit.1 });
        }
    }

    fn reveal_scrollbar(&mut self) {
        self.scrollbar_hide_at = if self.hovered || self.scrollbar_drag.is_some() { None } else { Some(Instant::now() + SCROLLBAR_LINGER) };
    }

    /// How visible the scrollbar is, and whether it is still fading.
    pub(crate) fn scrollbar_opacity(&self) -> (f32, bool) {
        if self.hovered || self.scrollbar_drag.is_some() {
            return (1., false);
        }
        match self.scrollbar_hide_at {
            None => (0., false),
            Some(at) => {
                let now = Instant::now();
                if now < at {
                    return (1., true);
                }
                let faded = (now - at).as_secs_f32() / SCROLLBAR_FADE.as_secs_f32();
                ((1. - faded).max(0.), faded < 1.)
            }
        }
    }

    pub(crate) fn scrollbar_active(&self) -> bool {
        self.scrollbar_drag.is_some()
    }

    fn scroll_lines(&mut self, lines: i32, cx: &mut Context<Self>) {
        if lines != 0 {
            self.screen.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let bracketed = self.screen.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let text = if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text };
        self.send_user_input(text, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let mut term = self.screen.lock();
        let start = GridPoint::new(term.topmost_line(), Column(0));
        let end = GridPoint::new(term.bottommost_line(), term.last_column());
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(end, Side::Right);
        term.selection = Some(selection);
        drop(term);
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let application_cursor = self.screen.lock().mode().contains(TermMode::APP_CURSOR);
        match keys::evaluate(&event.keystroke, application_cursor, self.is_mac, self.option_is_meta) {
            KeyResult::Send(text) => {
                self.send_user_input(text, cx);
                cx.stop_propagation();
            }
            KeyResult::ScrollPage(direction) => {
                let rows = self.screen.size().1 as i32;
                self.scroll_lines(-direction * (rows - 1), cx);
                cx.stop_propagation();
            }
            KeyResult::Ignore => {}
        }
    }

    /// Whether the user is forcing a selection over a program that tracks the mouse.
    fn forces_selection(&self, modifiers: &gpui::Modifiers) -> bool {
        // macOptionClickForcesSelection is off, so a Mac has no forcing modifier.
        !self.is_mac && modifiers.shift
    }

    fn report_mouse(&mut self, report: MouseReport, mode: TermMode, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        if report.action == ReportAction::Move {
            let key = (report.col, report.row, report.button);
            if self.last_report == Some(key) {
                return;
            }
            self.last_report = Some(key);
        }
        if let Some(text) = mouse::encode(report, mode, layout.cols, layout.rows) {
            self.send(text, cx);
        }
    }

    pub(crate) fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        let Some(layout) = self.layout else { return };
        if let Some(scrollbar) = layout.scrollbar.filter(|bar| bar.track.contains(&event.position)) {
            if event.button == MouseButton::Left {
                let grab = if scrollbar.thumb.contains(&event.position) {
                    event.position.y - scrollbar.thumb.origin.y
                } else {
                    scrollbar.thumb.size.height / 2.
                };
                self.scrollbar_drag = Some(ScrollbarDrag { grab });
                self.drag_scrollbar(event.position, cx);
            }
            return;
        }
        let (col, row, side) = layout.cell_at(event.position);
        let mode = *self.screen.lock().mode();
        if mouse::tracking(mode) && !self.forces_selection(&event.modifiers) {
            let button = match event.button {
                MouseButton::Left => ReportButton::Left,
                MouseButton::Middle => ReportButton::Middle,
                MouseButton::Right => ReportButton::Right,
                _ => return,
            };
            self.reporting_button = Some(button);
            let report = MouseReport {
                col,
                row,
                button,
                action: ReportAction::Down,
                ctrl: event.modifiers.control,
                alt: event.modifiers.alt,
                shift: event.modifiers.shift,
            };
            self.report_mouse(report, mode, cx);
            return;
        }
        let point = layout.grid_point(col, row);
        let mut term = self.screen.lock();
        match event.button {
            // A right click selects the word under it, unless it lands in the selection.
            MouseButton::Right if self.is_mac => {
                let inside = term.selection.as_ref().and_then(|s| s.to_range(&term)).is_some_and(|range| range.contains(point));
                if !inside {
                    term.selection = Some(Selection::new(SelectionType::Semantic, point, side));
                }
            }
            MouseButton::Left => {
                if event.modifiers.shift && term.selection.is_some() {
                    if let Some(selection) = term.selection.as_mut() {
                        selection.update(point, side);
                    }
                } else {
                    let kind = match event.click_count {
                        2 => SelectionType::Semantic,
                        count if count >= 3 => SelectionType::Lines,
                        _ if event.modifiers.alt => SelectionType::Block,
                        _ => SelectionType::Simple,
                    };
                    term.selection = Some(Selection::new(kind, point, side));
                }
                self.selection_drag = Some(SelectionDrag { overshoot: 0., last_position: event.position, scrolling: None });
            }
            _ => {}
        }
        drop(term);
        cx.notify();
    }

    pub(crate) fn mouse_move(&mut self, event: &MouseMoveEvent, inside: bool, over_scrollbar: bool, cx: &mut Context<Self>) {
        if inside != self.hovered || over_scrollbar != self.scrollbar_hovered {
            if self.hovered && !inside && self.scrollbar_drag.is_none() {
                self.scrollbar_hide_at = Some(Instant::now());
            }
            self.hovered = inside;
            self.scrollbar_hovered = over_scrollbar;
            cx.notify();
        }
        if self.scrollbar_drag.is_some() {
            self.drag_scrollbar(event.position, cx);
            return;
        }
        let Some(layout) = self.layout else { return };
        let mode = *self.screen.lock().mode();
        if let Some(button) = self.reporting_button {
            if event.pressed_button.is_some() {
                let (col, row, _) = layout.cell_at(event.position);
                let report = MouseReport {
                    col,
                    row,
                    button,
                    action: ReportAction::Move,
                    ctrl: event.modifiers.control,
                    alt: event.modifiers.alt,
                    shift: event.modifiers.shift,
                };
                self.report_mouse(report, mode, cx);
            }
            return;
        }
        if self.selection_drag.is_some() {
            if event.pressed_button != Some(MouseButton::Left) {
                self.end_selection(cx);
                return;
            }
            self.extend_selection(event.position, cx);
            return;
        }
        if inside && event.pressed_button.is_none() && mouse::reports_hover(mode) {
            let (col, row, _) = layout.cell_at(event.position);
            let report = MouseReport {
                col,
                row,
                button: ReportButton::None,
                action: ReportAction::Move,
                ctrl: event.modifiers.control,
                alt: event.modifiers.alt,
                shift: event.modifiers.shift,
            };
            self.report_mouse(report, mode, cx);
        }
    }

    pub(crate) fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.scrollbar_drag.take().is_some() {
            if !self.hovered {
                self.scrollbar_hide_at = Some(Instant::now());
            }
            cx.notify();
            return;
        }
        if let Some(button) = self.reporting_button.take() {
            if let Some(layout) = self.layout {
                let (col, row, _) = layout.cell_at(event.position);
                let mode = *self.screen.lock().mode();
                let report = MouseReport {
                    col,
                    row,
                    button,
                    action: ReportAction::Up,
                    ctrl: event.modifiers.control,
                    alt: event.modifiers.alt,
                    shift: event.modifiers.shift,
                };
                self.report_mouse(report, mode, cx);
            }
            self.last_report = None;
            return;
        }
        if self.selection_drag.is_some() {
            self.end_selection(cx);
        }
    }

    fn end_selection(&mut self, cx: &mut Context<Self>) {
        self.selection_drag = None;
        let mut term = self.screen.lock();
        if term.selection.as_ref().is_some_and(|selection| selection.is_empty()) {
            term.selection = None;
        }
        drop(term);
        cx.notify();
    }

    /// Moves the selection's end to the pointer, scrolling while it is held past the grid.
    fn extend_selection(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        let top = layout.bounds.origin.y;
        let bottom = top + layout.line_height * layout.rows as f32;
        let overshoot = if position.y < top {
            f32::from(position.y - top)
        } else if position.y > bottom {
            f32::from(position.y - bottom)
        } else {
            0.
        };
        self.update_selection_end(position);
        let Some(drag) = self.selection_drag.as_mut() else { return };
        drag.last_position = position;
        drag.overshoot = overshoot;
        if overshoot == 0. {
            drag.scrolling = None;
        } else if drag.scrolling.is_none() {
            drag.scrolling = Some(cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(DRAG_SCROLL_INTERVAL).await;
                let alive = this.update(cx, |this, cx| {
                    let Some(drag) = this.selection_drag.as_ref() else { return false };
                    // xterm's pace: up to 15 lines a tick, reached 50px past the edge.
                    let speed = (drag.overshoot / 50. * 15.).clamp(-15., 15.);
                    let lines = if speed < 0. { speed.floor() } else { speed.ceil() } as i32;
                    let position = drag.last_position;
                    this.screen.lock().scroll_display(Scroll::Delta(-lines));
                    this.update_selection_end(position);
                    cx.notify();
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }));
        }
        cx.notify();
    }

    fn update_selection_end(&mut self, position: Point<Pixels>) {
        let Some(layout) = self.layout else { return };
        let (col, row, side) = layout.cell_at(position);
        let mut term = self.screen.lock();
        let offset = term.grid().display_offset();
        let mut point = GridPoint::new(Line(row as i32 - offset as i32), Column(col));
        let mut side = side;
        // Past the grid's edges the selection reaches the end of the edge row.
        let top = layout.bounds.origin.y;
        if position.y < top {
            point.column = Column(0);
            side = Side::Left;
        } else if position.y > top + layout.line_height * layout.rows as f32 {
            point.column = term.last_column();
            side = Side::Right;
        }
        let point = term.expand_wide(point, Direction::Left);
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    fn drag_scrollbar(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(layout), Some(drag)) = (self.layout, self.scrollbar_drag.as_ref()) else { return };
        let Some(bar) = layout.scrollbar else { return };
        let travel = bar.track.size.height - bar.thumb.size.height;
        if travel <= px(0.) || layout.history == 0 {
            return;
        }
        let thumb_top = (position.y - drag.grab - bar.track.origin.y).clamp(px(0.), travel);
        let top = ((thumb_top / travel) * layout.history as f32).round() as usize;
        let offset = layout.history - top.min(layout.history);
        let mut term = self.screen.lock();
        let current = term.grid().display_offset() as i32;
        term.scroll_display(Scroll::Delta(offset as i32 - current));
        drop(term);
        cx.notify();
    }

    /// xterm's `consumeWheelEvent`: whole lines for a program, kept fractional between events.
    fn consume_wheel(&mut self, event: &ScrollEvent, line_height: Pixels) -> f32 {
        let fast = event.modifiers.alt || event.modifiers.control || event.modifiers.shift;
        let scale = if fast { 5. } else { 1. };
        match event.delta {
            ScrollDelta::Pixels(delta) => {
                let delta_y = -f32::from(delta.y);
                if delta_y == 0. || event.modifiers.shift {
                    return 0.;
                }
                let mut amount = delta_y * scale / f32::from(line_height);
                if delta_y.abs() < 50. {
                    amount *= 0.3;
                }
                self.wheel_lines += amount;
                let lines = self.wheel_lines.trunc();
                self.wheel_lines -= lines;
                lines
            }
            ScrollDelta::Lines(delta) => {
                if delta.y == 0. || event.modifiers.shift {
                    return 0.;
                }
                -delta.y * scale
            }
        }
    }

    pub(crate) fn scroll_wheel(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        let mode = *self.screen.lock().mode();
        if mouse::tracking(mode) {
            let lines = self.consume_wheel(event, layout.line_height);
            if lines == 0. {
                return;
            }
            let (col, row, _) = layout.cell_at(event.position);
            let report = MouseReport {
                col,
                row,
                button: ReportButton::Wheel,
                action: if lines < 0. { ReportAction::Up } else { ReportAction::Down },
                ctrl: event.modifiers.control,
                alt: event.modifiers.alt,
                shift: event.modifiers.shift,
            };
            self.report_mouse(report, mode, cx);
            return;
        }
        if mode.contains(TermMode::ALT_SCREEN) {
            // The alternate screen has no scrollback, so the wheel moves the program's cursor.
            let lines = self.consume_wheel(event, layout.line_height);
            if lines == 0. {
                return;
            }
            let prefix = if mode.contains(TermMode::APP_CURSOR) { "\x1bO" } else { "\x1b[" };
            let arrow = if lines < 0. { 'A' } else { 'B' };
            self.send_user_input(format!("{prefix}{arrow}"), cx);
            return;
        }
        if event.modifiers.shift {
            return;
        }
        let delta = event.delta.pixel_delta(layout.line_height);
        let scale = if event.modifiers.alt { 5. } else { 1. };
        self.viewport_pixels += f32::from(delta.y) * scale;
        let lines = (self.viewport_pixels / f32::from(layout.line_height)).trunc();
        self.viewport_pixels -= lines * f32::from(layout.line_height);
        self.scroll_lines(lines as i32, cx);
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked_text.as_deref().map_or(0, |text| text.encode_utf16().count());
        Some(UTF16Selection { range: end..end, reversed: false })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<std::ops::Range<usize>> {
        self.marked_text.as_deref().map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        self.send_user_input(text.to_owned(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout?;
        let term = self.screen.lock();
        let cursor = term.grid().cursor.point;
        let row = cursor.line.0 + term.grid().display_offset() as i32;
        let origin = layout.bounds.origin
            + Point::new(layout.cell_width * cursor.column.0 as f32, layout.line_height * row.max(0) as f32);
        Some(Bounds::new(origin, gpui::size(layout.cell_width, layout.line_height)))
    }

    fn character_index_for_point(&mut self, _point: Point<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let [top, right, bottom, left] = VIEWPORT_PADDING;
        div()
            .id("terminal-viewport")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::select_all))
            .on_key_down(cx.listener(Self::key_down))
            // The viewport is wider than the rows it holds, so a click in the margin still means the shell.
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus(window, cx)))
            .size_full()
            .min_h_0()
            .flex()
            .bg(self.theme.background)
            .pt(px(top))
            .pr(px(right))
            .pb(px(bottom))
            .pl(px(left))
            .child(TerminalElement::new(cx.entity(), self.focus_handle.clone(), self.theme.clone(), self.font.clone()))
    }
}
