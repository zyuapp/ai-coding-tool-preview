//! Menus and pickers (Menu.tsx, PopoverMenu.tsx, menu.css, menu-trigger.css).
//!
//! A `Menu` is a view drawn over the window at its `Placement`: under a trigger, beside a row, or
//! at the pointer. It owns its keyboard: the arrows walk the rows and wrap, Home and End reach the
//! ends, letters jump by name, Right opens a row's own list and Left closes it, Enter or Space
//! chooses, Escape empties a typed search and then closes. A disabled row stays reachable so its
//! reason can be read. Choosing a row always closes the whole menu, which emits `DismissEvent`.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    actions, deferred, div, prelude::*, px, AnyElement, App, Context, CursorStyle, DismissEvent, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding, KeyDownEvent, MouseButton, Pixels, ScrollHandle,
    SharedString, Subscription, Task, WeakEntity, Window,
};

use super::field::{SearchField, SearchVariant};
use crate::icon::{Icon, IconName};
use crate::input::{InputEvent, TextInput};
use crate::overlay::{bounds_cell, bounds_probe, placed, Align, BoundsCell, Placement};
use crate::style::{glass, Corners, GlassKind, PaneFill, Raised};
use crate::theme::ActiveTheme;
use crate::tooltip::Tippable;

actions!(menu, [MenuUp, MenuDown, MenuFirst, MenuLast, MenuConfirm, MenuCancel, MenuOpenSubmenu, MenuCloseSubmenu]);

pub const KEY_CONTEXT: &str = "Menu";

pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", MenuUp, context),
        KeyBinding::new("down", MenuDown, context),
        KeyBinding::new("home", MenuFirst, context),
        KeyBinding::new("end", MenuLast, context),
        KeyBinding::new("enter", MenuConfirm, context),
        KeyBinding::new("space", MenuConfirm, Some("Menu && !TextInput")),
        KeyBinding::new("escape", MenuCancel, context),
        KeyBinding::new("right", MenuOpenSubmenu, Some("Menu && !TextInput")),
        KeyBinding::new("left", MenuCloseSubmenu, Some("Menu && !TextInput")),
    ]);
}

/// How long typed letters keep adding to one search before the next key starts a new one.
const TYPEAHEAD: Duration = Duration::from_millis(700);
/// How long the pointer may rest on a sibling before an open submenu gives way to it.
const SUBMENU_INTENT: Duration = Duration::from_millis(220);
/// The tallest a scrolling menu grows (`--menu-max-height`).
const MAX_HEIGHT: f32 = 420.0;
const PAD: f32 = 5.0;

pub type SelectHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// One row: tick, icon, label over an optional description, then detail or shortcut and chevron.
#[derive(Clone, Default)]
pub struct MenuItem {
    pub label: SharedString,
    pub icon: Option<IconName>,
    /// A picture in place of the icon, such as an application's own icon.
    pub image: Option<std::sync::Arc<gpui::Image>>,
    pub description: Option<SharedString>,
    /// Quiet words against the right edge, such as which computer holds the item.
    pub detail: Option<SharedString>,
    /// The keystroke that does the same, in the same place as `detail`.
    pub shortcut: Option<SharedString>,
    /// A status dot before `detail`, such as a running device's green.
    pub detail_dot: Option<gpui::Rgba>,
    /// Draws the tick. `None` for a row that is never ticked.
    pub checked: Option<bool>,
    /// Drawn in the tick column in place of the tick.
    pub mark: Option<IconName>,
    pub danger: bool,
    pub disabled: bool,
    /// Something out of reach, such as an offline computer: the label quiet, behind a dot.
    pub away: bool,
    /// Why the row cannot be chosen, or what it does, as its tooltip.
    pub tip: Option<SharedString>,
    /// Opens beside the row. A row with a list of its own chooses nothing itself.
    pub items: Vec<MenuEntry>,
    pub on_select: Option<SelectHandler>,
    /// Runs when the row's own list opens.
    pub on_open: Option<SelectHandler>,
    /// The face the label is drawn in, as a font picker shows each family in its own.
    pub font_family: Option<SharedString>,
    /// A field over the row's own list, which the filter rebuilds as it is typed into.
    pub submenu_search: Option<(SharedString, SearchFilter)>,
    /// An action at the list's edge, such as "New worktree", which Enter in the search never takes.
    pub edge: bool,
    /// The icon's size, 14 unless the menu draws larger ones.
    pub icon_size: Option<f32>,
    /// The name assistive technology reads for the row's own list.
    pub submenu_name: Option<SharedString>,
    /// The name assistive technology reads for the row's own search field; its placeholder otherwise.
    pub submenu_search_label: Option<SharedString>,
    /// `detail` follows the label on its line instead of standing against the right edge.
    pub inline_detail: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), ..Self::default() }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = Some(size);
        self
    }

    pub fn image(mut self, image: std::sync::Arc<gpui::Image>) -> Self {
        self.image = Some(image);
        self
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn detail_dot(mut self, color: gpui::Rgba) -> Self {
        self.detail_dot = Some(color);
        self
    }

    pub fn away(mut self, away: bool) -> Self {
        self.away = away;
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    pub fn mark(mut self, mark: IconName) -> Self {
        self.mark = Some(mark);
        self
    }

    pub fn font_family(mut self, family: impl Into<SharedString>) -> Self {
        self.font_family = Some(family.into());
        self
    }

    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    pub fn submenu(mut self, items: Vec<MenuEntry>) -> Self {
        self.items = items;
        self
    }

    pub fn on_select(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    pub fn on_open(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }

    /// Sets `detail` right after the label, giving way to it up to 45% of the line.
    pub fn inline_detail(mut self, inline: bool) -> Self {
        self.inline_detail = inline;
        self
    }

    pub fn edge(mut self, edge: bool) -> Self {
        self.edge = edge;
        self
    }

    pub fn submenu_named(mut self, name: impl Into<SharedString>) -> Self {
        self.submenu_name = Some(name.into());
        self
    }

    /// Gives the row's own list a search field; `filter` builds the list for what is typed.
    pub fn submenu_searchable(mut self, placeholder: impl Into<SharedString>, filter: impl Fn(&str, &mut App) -> Vec<MenuEntry> + 'static) -> Self {
        self.submenu_search = Some((placeholder.into(), Rc::new(filter)));
        self
    }

    pub fn submenu_search_label(mut self, label: impl Into<SharedString>) -> Self {
        self.submenu_search_label = Some(label.into());
        self
    }

    fn nested(&self) -> bool {
        !self.items.is_empty()
    }
}

/// What a list draws: items, rules, labelled groups, headings, and quiet notes.
#[derive(Clone)]
pub enum MenuEntry {
    Item(MenuItem),
    Separator,
    Group { label: SharedString, items: Vec<MenuItem> },
    /// A label over the whole menu, set like a group's.
    Heading(SharedString),
    /// A line of quiet text: an empty list, a status, or an error.
    Note { text: SharedString, danger: bool },
}

impl From<MenuItem> for MenuEntry {
    fn from(item: MenuItem) -> Self {
        MenuEntry::Item(item)
    }
}

/// Items in the order they are drawn, groups opened up.
fn items_of(entries: &[MenuEntry]) -> Vec<&MenuItem> {
    let mut items = Vec::new();
    for entry in entries {
        match entry {
            MenuEntry::Item(item) => items.push(item),
            MenuEntry::Group { items: group, .. } => items.extend(group.iter()),
            _ => {}
        }
    }
    items
}

/// Typing a row's first letters picks it. A repeated single letter steps through the rows that
/// start with it. Returns the row to move to.
pub fn typeahead_match(labels: &[(&str, bool)], at: Option<usize>, typed: &str) -> Option<usize> {
    if typed.is_empty() || labels.is_empty() {
        return None;
    }
    let first = typed.chars().next()?;
    let needle: String = if typed.chars().all(|ch| ch == first) { first.to_string() } else { typed.to_string() };
    let usable: Vec<usize> = labels.iter().enumerate().filter(|(_, (_, disabled))| !disabled).map(|(index, _)| index).collect();
    let position = at.and_then(|at| usable.iter().position(|index| *index == at));
    let start = if needle.chars().count() == 1 { position.map(|p| p + 1).unwrap_or(0) } else { position.unwrap_or(0) };
    let order = usable[start.min(usable.len())..].iter().chain(usable[..start.min(usable.len())].iter());
    for index in order {
        if labels[*index].0.trim().to_lowercase().starts_with(&needle) {
            return Some(*index);
        }
    }
    None
}

pub type SearchFilter = Rc<dyn Fn(&str, &mut App) -> Vec<MenuEntry>>;

struct Search {
    input: Entity<TextInput>,
    filter: SearchFilter,
    _subscription: Subscription,
}

struct OpenSubmenu {
    index: usize,
    menu: Entity<Menu>,
    _subscription: Subscription,
}

pub struct Menu {
    focus: FocusHandle,
    entries: Vec<MenuEntry>,
    placement: Option<Placement>,
    checkable: Option<bool>,
    /// What assistive technology calls the menu.
    name: SharedString,
    /// A label over the whole menu, above its search field (`MenuHeading`).
    title: Option<SharedString>,
    min_width: Pixels,
    width: Option<Pixels>,
    max_width: Option<Pixels>,
    /// The search sits inside the pane's padding rather than flush with its edge, as a searchable
    /// submenu's panel does (`.worktree-menu-panel`).
    inset_search: bool,
    empty: Option<SharedString>,
    active: Option<usize>,
    /// Whether the menu held the keyboard when last drawn.
    focus_held: bool,
    /// The highlight follows the pointer, which shows only while the pointer is on the row.
    pointer_active: bool,
    pointer_on_row: bool,
    typed: (String, Option<Instant>),
    submenu: Option<OpenSubmenu>,
    intent: Option<Task<()>>,
    search: Option<Search>,
    rows: Vec<BoundsCell>,
    surface: BoundsCell,
    scroll: ScrollHandle,
    /// Child index in the scrolled column of each row, for scrolling one into view.
    row_children: Vec<usize>,
    parent: Option<WeakEntity<Menu>>,
    /// Whether the first row takes the keyboard on open, as a keyboard-opened submenu does.
    focus_first: bool,
    dismissed: bool,
    /// Enter was pressed in the search field; the first row is chosen on the next draw.
    pending_submit: bool,
    search_focused: bool,
    /// The pointer is on the open submenu, so the sibling it crossed does not take over.
    pointer_on_submenu: bool,
    /// A press here is not a press outside: the trigger that toggles the menu.
    exempt: Option<BoundsCell>,
    search_config: Option<(SharedString, SearchFilter)>,
    search_label: Option<SharedString>,
    /// Notes after the last row sit below the scrolled list rather than in it.
    pinned_notes: bool,
    /// What held the keyboard before the menu took it, given back when Escape or a choice closes it.
    return_focus: Option<FocusHandle>,
    /// A cap on the scrolled rows alone, below the menu's own.
    list_max_height: Option<Pixels>,
    /// The menu's own cap, 420 unless set (`--menu-max-height`).
    max_height: Option<Pixels>,
    /// An edge action ending the list sits below the scrolled rows, ruled off from them (`MenuEdgeAction`).
    pinned_edge: bool,
    /// Icons drawn before the labels of the groups they name.
    group_icons: Vec<(SharedString, IconName)>,
    /// The first row is marked as Enter's choice only once something is typed.
    default_needs_query: bool,
    /// Enter in the search field takes the first row, which wears the highlight meanwhile.
    search_submits: bool,
    searched_text_empty: bool,
    /// Opened from the window's frame (the sidebars), where too little stands behind it to blur.
    from_frame: bool,
    top_level: bool,
}

impl EventEmitter<DismissEvent> for Menu {}

impl Focusable for Menu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Menu {
    pub fn new(entries: Vec<MenuEntry>, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            entries,
            placement: None,
            checkable: None,
            name: SharedString::default(),
            title: None,
            min_width: px(168.0),
            inset_search: false,
            width: None,
            max_width: None,
            empty: None,
            active: None,
            focus_held: false,
            pointer_active: false,
            pointer_on_row: false,
            typed: (String::new(), None),
            submenu: None,
            intent: None,
            search: None,
            rows: Vec::new(),
            surface: bounds_cell(),
            scroll: ScrollHandle::new(),
            row_children: Vec::new(),
            parent: None,
            focus_first: false,
            dismissed: false,
            pending_submit: false,
            search_focused: false,
            pointer_on_submenu: false,
            exempt: None,
            search_config: None,
            search_label: None,
            pinned_notes: false,
            return_focus: None,
            list_max_height: None,
            max_height: None,
            pinned_edge: false,
            group_icons: Vec::new(),
            default_needs_query: false,
            search_submits: true,
            searched_text_empty: true,
            from_frame: false,
            top_level: false,
        }
    }

    /// Builds a menu view and gives it the keyboard.
    pub fn build(
        entries: Vec<MenuEntry>,
        placement: Placement,
        window: &mut Window,
        cx: &mut App,
        configure: impl FnOnce(Menu) -> Menu,
    ) -> Entity<Menu> {
        let return_focus = window.focused(cx);
        let menu = cx.new(|cx| configure(Menu::new(entries, cx).placement(placement)));
        menu.update(cx, |menu, cx| {
            menu.return_focus = return_focus;
            menu.take_focus(window, cx);
        });
        menu
    }

    /// A menu the frame opened takes the near-solid glass over a window material (ui/glass.css).
    pub fn from_frame(mut self) -> Self {
        self.from_frame = true;
        self
    }

    /// A menu drawn at the window's top level (a child of the page body), which takes the frame's
    /// glass while the sidebar shows an expanded control (ui/glass.css).
    pub fn top_level(mut self) -> Self {
        self.top_level = true;
        self
    }

    /// The row the keyboard starts on, as `autoFocus` puts it there when the menu opens.
    pub fn highlighted(mut self, index: Option<usize>) -> Self {
        self.active = index;
        self
    }

    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = Some(placement);
        self
    }

    /// Caps the height of the scrolled rows, leaving the search and pinned notes outside the cap.
    pub fn list_max_height(mut self, height: Pixels) -> Self {
        self.list_max_height = Some(height);
        self
    }

    /// Caps the whole menu's height (`--menu-max-height`).
    pub fn max_height(mut self, height: Pixels) -> Self {
        self.max_height = Some(height);
        self
    }

    /// Draws an edge action that ends the list, and the rule before it, below the scrolled rows.
    pub fn pinned_edge(mut self, pinned: bool) -> Self {
        self.pinned_edge = pinned;
        self
    }

    /// Draws `icon` before the label of the group named `label`.
    pub fn group_icon(mut self, label: impl Into<SharedString>, icon: IconName) -> Self {
        self.group_icons.push((label.into(), icon));
        self
    }

    /// Draws the notes that end the list below its scrolled rows, always in view.
    /// Marks the first row as Enter's choice only while the search holds a query, as the font and
    /// theme pickers do.
    pub fn default_needs_query(mut self, needs: bool) -> Self {
        self.default_needs_query = needs;
        self
    }

    /// Whether Enter in the search field takes the first row; a list where it does not marks none.
    pub fn search_submits(mut self, submits: bool) -> Self {
        self.search_submits = submits;
        self
    }

    pub fn pinned_notes(mut self, pinned: bool) -> Self {
        self.pinned_notes = pinned;
        self
    }

    /// Labels the menu above its search field, naming what is chosen.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Reserves the tick column. Defaults to whether any row can be ticked.
    pub fn named(mut self, name: impl Into<SharedString>) -> Self {
        self.name = name.into();
        self
    }

    pub fn checkable(mut self, checkable: bool) -> Self {
        self.checkable = Some(checkable);
        self
    }

    pub fn min_width(mut self, width: Pixels) -> Self {
        self.min_width = width;
        self
    }

    /// A fixed width, as a menu sized to its trigger takes.
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }

    /// As wide as the widest row, within this bound (`.menu-list`: 320px).
    pub fn max_width(mut self, width: Pixels) -> Self {
        self.max_width = Some(width);
        self
    }

    /// A press on this area does not dismiss the menu, so its trigger can close it by toggling.
    pub fn exempt(mut self, area: &BoundsCell) -> Self {
        self.exempt = Some(area.clone());
        self
    }

    /// What the list says while it has no items.
    pub fn empty(mut self, text: impl Into<SharedString>) -> Self {
        self.empty = Some(text.into());
        self
    }

    /// Changes what an empty list says, as when a list still loading has loaded.
    pub fn set_empty(&mut self, text: impl Into<SharedString>) {
        self.empty = Some(text.into());
    }

    /// A field above the rows that narrows them. It holds the caret while the list is open, and
    /// Enter picks the first row that can be chosen.
    pub fn searchable(mut self, placeholder: impl Into<SharedString>, filter: impl Fn(&str, &mut App) -> Vec<MenuEntry> + 'static) -> Self {
        self.search_config = Some((placeholder.into(), Rc::new(filter)));
        self
    }

    /// The name assistive technology reads for the search field; its placeholder otherwise.
    pub fn search_label(mut self, label: impl Into<SharedString>) -> Self {
        self.search_label = Some(label.into());
        self
    }

    /// Makes the field a `searchable` menu asked for, once the view exists.
    fn init_search(&mut self, cx: &mut Context<Self>) {
        let Some((placeholder, filter)) = self.search_config.take() else { return };
        let label = self.search_label.clone();
        let input = cx.new(|cx| {
            let input = TextInput::new(cx).placeholder(placeholder);
            match label {
                Some(label) => input.label(label),
                None => input,
            }
        });
        let subscription = cx.subscribe(&input, |menu: &mut Menu, input, event: &InputEvent, cx| match event {
            InputEvent::Changed => {
                let query = input.read(cx).text().to_string();
                if let Some(search) = &menu.search {
                    let filter = search.filter.clone();
                    menu.entries = filter(&query, cx);
                    menu.active = None;
                    menu.submenu = None;
                    cx.notify();
                }
            }
            InputEvent::Submit => {
                if menu.search_submits && !(menu.default_needs_query && input.read(cx).text().trim().is_empty()) {
                    menu.pending_submit = true;
                    cx.notify();
                }
            }
            _ => {}
        });
        self.search = Some(Search { input, filter, _subscription: subscription });
    }

    /// Replaces the rows. An open list of one of them follows: a searched one is filtered again.
    pub fn set_entries(&mut self, entries: Vec<MenuEntry>, cx: &mut Context<Self>) {
        if let Some(open) = &self.submenu {
            if let Some(item) = items_of(&entries).get(open.index).filter(|item| item.nested()) {
                let items = item.items.clone();
                open.menu.update(cx, |menu, cx| match &menu.search {
                    Some(search) => {
                        let (query, filter) = (search.input.read(cx).text().to_string(), search.filter.clone());
                        let rows = filter(&query, cx);
                        menu.set_entries(rows, cx);
                    }
                    None => menu.set_entries(items, cx),
                });
            }
        }
        self.entries = entries;
        self.active = self.active.filter(|active| *active < items_of(&self.entries).len());
        cx.notify();
    }

    pub fn set_placement(&mut self, placement: Placement, cx: &mut Context<Self>) {
        if self.placement != Some(placement) {
            self.placement = Some(placement);
            cx.notify();
        }
    }

    /// The search field's text.
    pub fn query(&self, cx: &App) -> String {
        self.search.as_ref().map(|search| search.input.read(cx).text().to_string()).unwrap_or_default()
    }

    fn take_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.init_search(cx);
        if let Some(search) = &self.search {
            let query = search.input.read(cx).text().to_string();
            let filter = search.filter.clone();
            self.entries = filter(&query, cx);
            search.input.read(cx).focus(window);
        } else {
            window.focus(&self.focus);
            if self.focus_first {
                self.active = self.first_enabled();
            }
        }
        cx.notify();
    }

    fn items(&self) -> Vec<&MenuItem> {
        items_of(&self.entries)
    }

    fn first_enabled(&self) -> Option<usize> {
        self.items().iter().position(|item| !item.disabled)
    }

    /// The row Enter in the search field takes: the first match, never an edge action.
    fn first_searched(&self) -> Option<usize> {
        self.items().iter().position(|item| !item.disabled && !item.edge)
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.dismissed {
            return;
        }
        self.dismissed = true;
        self.submenu = None;
        if let Some(parent) = self.parent.as_ref().and_then(|parent| parent.upgrade()) {
            parent.update(cx, |parent, cx| parent.dismiss(cx));
        }
        cx.emit(DismissEvent);
    }

    fn choose(&mut self, index: usize, by_keyboard: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items().get(index).map(|item| (*item).clone()) else { return };
        if item.disabled {
            return;
        }
        if item.nested() {
            self.open_submenu(index, by_keyboard, window, cx);
            return;
        }
        self.give_back_focus(window);
        self.dismiss(cx);
        if let Some(handler) = item.on_select {
            handler(window, cx);
        }
    }

    fn open_submenu(&mut self, index: usize, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items().get(index).map(|item| (*item).clone()) else { return };
        if item.disabled || !item.nested() {
            return;
        }
        self.active = Some(index);
        if self.submenu.as_ref().is_some_and(|open| open.index == index) {
            if focus {
                let menu = self.submenu.as_ref().map(|open| open.menu.clone());
                if let Some(menu) = menu {
                    menu.update(cx, |menu, cx| {
                        menu.focus_first = true;
                        menu.take_focus(window, cx);
                    });
                }
            }
            return;
        }
        if let Some(handler) = &item.on_open {
            handler(window, cx);
        }
        let anchor = self.rows.get(index).and_then(|cell| cell.get()).unwrap_or_default();
        // `top: offsetTop - 4` in the parent pane (PopoverMenu.tsx).
        let placement = Placement::Beside { anchor: self.surface.get().unwrap_or(anchor), top: anchor.top() - px(4.0) };
        let parent = cx.entity().downgrade();
        let menu = cx.new(|cx| {
            let mut menu = Menu::new(item.items.clone(), cx).placement(placement);
            menu.parent = Some(parent);
            menu.focus_first = focus;
            menu.search_config = item.submenu_search.clone();
            menu.search_label = item.submenu_search_label.clone();
            if menu.search_config.is_some() {
                // A 280px panel inside the pane's padding and border.
                menu.inset_search = true;
                menu.width = Some(px(280.0 + 2.0 * PAD + 2.0));
            }
            if let Some(name) = &item.submenu_name {
                menu.name = name.clone();
            }
            menu.init_search(cx);
            menu
        });
        let subscription = cx.subscribe(&menu, |_: &mut Menu, _, _: &DismissEvent, _| {});
        if focus {
            menu.update(cx, |menu, cx| menu.take_focus(window, cx));
        }
        self.submenu = Some(OpenSubmenu { index, menu, _subscription: subscription });
        cx.notify();
    }

    fn close_submenu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submenu.take().is_some() {
            window.focus(&self.focus);
            cx.notify();
        }
    }

    fn move_by(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.items().len();
        if count == 0 {
            return;
        }
        let field_focused = self.search.as_ref().is_some_and(|search| search.input.read(cx).is_focused(window));
        if self.search.is_some() && delta < 0 && (self.active == Some(0) || field_focused) {
            self.active = None;
            if let Some(search) = &self.search {
                search.input.read(cx).focus(window);
            }
            cx.notify();
            return;
        }
        let next = match (self.active.filter(|_| !field_focused), delta > 0) {
            (None, true) => 0,
            (None, false) => count - 1,
            (Some(at), _) => (at as isize + delta).rem_euclid(count as isize) as usize,
        };
        self.highlight(next, window, cx);
    }

    fn highlight(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(index);
        self.pointer_active = false;
        window.focus(&self.focus);
        if let Some(child) = self.row_children.get(index) {
            self.scroll.scroll_to_item(*child);
        }
        if self.submenu.as_ref().is_some_and(|open| open.index != index) {
            self.submenu = None;
        }
        cx.notify();
    }

    fn up(&mut self, _: &MenuUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by(-1, window, cx);
    }

    fn down(&mut self, _: &MenuDown, window: &mut Window, cx: &mut Context<Self>) {
        self.move_by(1, window, cx);
    }

    fn first(&mut self, _: &MenuFirst, window: &mut Window, cx: &mut Context<Self>) {
        if !self.items().is_empty() {
            self.highlight(0, window, cx);
        }
    }

    fn last(&mut self, _: &MenuLast, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.items().len();
        if count > 0 {
            self.highlight(count - 1, window, cx);
        }
    }

    fn confirm(&mut self, _: &MenuConfirm, window: &mut Window, cx: &mut Context<Self>) {
        let field_focused = self.search.as_ref().is_some_and(|search| search.input.read(cx).is_focused(window));
        let target = if field_focused { self.first_searched().filter(|_| self.search_submits) } else { self.active };
        if let Some(index) = target {
            self.choose(index, true, window, cx);
        }
    }

    fn cancel(&mut self, _: &MenuCancel, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.search {
            if !search.input.read(cx).is_empty() {
                search.input.update(cx, |input, cx| input.set_text("", cx));
                return;
            }
        }
        if let Some(parent) = self.parent.as_ref().and_then(|parent| parent.upgrade()) {
            parent.update(cx, |parent, cx| parent.close_submenu(window, cx));
            return;
        }
        self.give_back_focus(window);
        self.dismiss(cx);
    }

    fn give_back_focus(&mut self, window: &mut Window) {
        if let Some(focus) = self.return_focus.take() {
            window.focus(&focus);
        }
    }

    fn open_active(&mut self, _: &MenuOpenSubmenu, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.active {
            self.open_submenu(index, true, window, cx);
        }
    }

    fn close_self(&mut self, _: &MenuCloseSubmenu, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(parent) = self.parent.as_ref().and_then(|parent| parent.upgrade()) {
            parent.update(cx, |parent, cx| parent.close_submenu(window, cx));
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform {
            return;
        }
        // Typeahead is the menu's own: not while its search field or an open submenu has the keyboard.
        if !self.focus.is_focused(window) {
            return;
        }
        let Some(typed) = keystroke.key_char.as_ref().filter(|text| text.chars().count() == 1) else { return };
        let now = Instant::now();
        let fresh = self.typed.1.is_none_or(|at| now.duration_since(at) > TYPEAHEAD);
        if typed == " " && fresh {
            return;
        }
        let text = if fresh { typed.to_lowercase() } else { format!("{}{}", self.typed.0, typed.to_lowercase()) };
        self.typed = (text.clone(), Some(now));
        let labels: Vec<(String, bool)> = self.items().iter().map(|item| (item.label.to_string(), item.disabled)).collect();
        let labels: Vec<(&str, bool)> = labels.iter().map(|(label, disabled)| (label.as_str(), *disabled)).collect();
        cx.stop_propagation();
        if let Some(index) = typeahead_match(&labels, self.active, &text) {
            self.highlight(index, window, cx);
        }
    }

    fn hover_row(&mut self, index: usize, hovered: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !hovered {
            if self.active == Some(index) {
                self.pointer_on_row = false;
                cx.notify();
            }
            return;
        }
        let other_open = self.submenu.as_ref().is_some_and(|open| open.index != index);
        let apply = move |menu: &mut Menu, window: &mut Window, cx: &mut Context<Menu>| {
            menu.active = Some(index);
            menu.pointer_active = true;
            menu.pointer_on_row = true;
            let nested = menu.items().get(index).is_some_and(|item| item.nested() && !item.disabled);
            if nested {
                menu.open_submenu(index, false, window, cx);
            } else if menu.submenu.is_some() {
                menu.submenu = None;
            }
            if menu.search.as_ref().is_none_or(|search| !search.input.read(cx).is_focused(window)) {
                window.focus(&menu.focus);
            }
            cx.notify();
        };
        if other_open {
            self.intent = Some(cx.spawn_in(window, async move |menu, cx| {
                cx.background_executor().timer(SUBMENU_INTENT).await;
                let _ = menu.update_in(cx, |menu, window, cx| {
                    if menu.pointer_on_submenu {
                        return;
                    }
                    apply(menu, window, cx);
                });
            }));
        } else {
            self.intent = None;
            apply(self, window, cx);
        }
    }
}

impl Menu {
    fn render_row(&self, index: usize, item: &MenuItem, checkable: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let open = self.submenu.as_ref().is_some_and(|open| open.index == index);
        let washed = !item.disabled
            && (open || (self.active == Some(index) && (!self.pointer_active || self.pointer_on_row)) || (self.search_default(index)));
        let ink = if item.disabled {
            t.quiet
        } else if item.danger {
            t.danger
        } else if washed || item.checked == Some(true) {
            t.ink
        } else {
            t.ink_dim
        };
        let quiet = |color| if item.disabled { t.quiet } else if washed { ink } else { color };
        let mut access = gpui::AccessInfo::new(gpui::AccessRole::MenuItem, item.label.clone())
            .disabled(item.disabled)
            .active(self.active == Some(index) && !self.pointer_active && self.focus_held);
        if let Some(checked) = item.checked {
            access = access.toggled(checked);
        }
        let mut row = div()
            .id(ElementId::NamedInteger("menu-row".into(), index as u64))
            .accessible(access)
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .w_full()
            .min_h(px(28.0))
            .rounded(px(6.0))
            .px(px(8.0))
            .py(px(4.0))
            .text_color(ink)
            .text_size(px(m.text_menu))
            .cursor(CursorStyle::Arrow)
            .child(bounds_probe(&self.rows[index]));
        if washed {
            let pick = if item.danger { t.glass_pick_danger } else { t.glass_pick };
            row = row.pane(pick).rim(t.glass_rim, Corners::Radius(6.0));
        }
        // The tick column: no room where nothing can be ticked, the same room on every row once anything can.
        let tick = match (item.mark, item.checked) {
            (Some(mark), _) => Some(Icon::new(mark).size(px(14.0)).into_any_element()),
            (None, Some(true)) => Some(Icon::new(IconName::Check).size(px(14.0)).into_any_element()),
            _ => None,
        };
        if checkable || tick.is_some() {
            row = row.child(div().flex_none().w(px(14.0)).flex().items_center().justify_center().children(tick));
        }
        if let Some(image) = &item.image {
            row = row.child(gpui::img(gpui::ImageSource::Image(image.clone())).flex_none().size(px(20.0)));
        } else if let Some(icon) = item.icon {
            row = row.child(div().flex_none().flex().items_center().text_color(quiet(t.quiet_strong)).child(Icon::new(icon).size(px(item.icon_size.unwrap_or(14.0)))));
        }
        let label = div().overflow_hidden().text_ellipsis().whitespace_nowrap().when_some(item.font_family.clone(), |label, family| label.font_family(family)).child(item.label.clone());
        let inline = item.detail.clone().filter(|detail| item.inline_detail && !detail.is_empty());
        let label = match inline.clone() {
            Some(detail) => div()
                .flex()
                .items_baseline()
                .gap(px(8.0))
                .child(label.min_w_0())
                .child(div().flex_none().max_w(gpui::relative(0.45)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(if item.disabled { t.quiet } else { t.quiet_strong }).text_size(px(m.text_meta)).child(detail)),
            None => label,
        };
        let label = if item.away {
            div().flex().items_center().gap(px(6.0)).text_color(quiet(t.quiet)).child(div().flex_none().size(px(6.0)).rounded_full().bg(quiet(t.quiet))).child(label).into_any_element()
        } else {
            label.into_any_element()
        };
        let mut copy = div().flex_1().min_w_0().flex().flex_col().gap(px(2.0)).child(label);
        if let Some(description) = &item.description {
            copy = copy.child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(quiet(t.quiet_strong))
                    .text_size(px(m.text_meta))
                    .line_height(gpui::relative(1.35))
                    .child(description.clone()),
            );
        }
        row = row.child(copy);
        if let Some(trailing) = item.shortcut.clone().or_else(|| item.detail.clone().filter(|_| inline.is_none())).filter(|text| !text.is_empty()) {
            let dot = item.detail_dot.filter(|_| item.shortcut.is_none());
            row = row.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_color(quiet(t.muted))
                    .when_some(dot, |trailing, color| trailing.child(div().flex_none().size(px(6.0)).rounded_full().bg(color)))
                    .child(trailing),
            );
        }
        if item.nested() {
            row = row.child(div().flex_none().text_color(quiet(t.muted)).child(Icon::new(IconName::ChevronRight).size(px(12.0))));
        }
        row = row
            .skip_tab_stop()
            .on_hover(cx.listener(move |menu, hovered: &bool, window, cx| menu.hover_row(index, *hovered, window, cx)))
            .on_click(cx.listener(move |menu, _, window, cx| menu.choose(index, false, window, cx)));
        match &item.tip {
            Some(tip) if item.disabled => row.tip_reason(tip.clone()).into_any_element(),
            Some(tip) => row.tip(tip.clone()).into_any_element(),
            None => row.into_any_element(),
        }
    }

    /// The row Enter takes while the caret is in the search field.
    fn search_default(&self, index: usize) -> bool {
        self.search.is_some() && self.search_submits && self.search_focused && self.first_searched() == Some(index) && !(self.default_needs_query && self.searched_text_empty)
    }
}

impl Render for Menu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focus_held = self.focus.is_focused(window);
        if self.pending_submit {
            self.pending_submit = false;
            if let Some(index) = self.first_searched() {
                self.choose(index, true, window, cx);
            }
        }
        self.search_focused = self.search.as_ref().is_some_and(|search| search.input.read(cx).is_focused(window));
        self.searched_text_empty = self.search.as_ref().is_none_or(|search| search.input.read(cx).text().trim().is_empty());
        let item_count = self.items().len();
        while self.rows.len() < item_count {
            self.rows.push(bounds_cell());
        }
        let checkable = self.checkable.unwrap_or_else(|| self.items().iter().any(|item| item.checked.is_some()));
        let theme = cx.theme().clone();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let viewport = window.viewport_size();
        let room = self.placement.map(|placement| placement.room(viewport)).unwrap_or(viewport.height - px(16.0));
        let max_height = self.max_height.unwrap_or(px(MAX_HEIGHT)).min(room).max(px(60.0));

        let mut column: Vec<AnyElement> = Vec::new();
        self.row_children.clear();
        let mut index = 0;
        let entries = self.entries.clone();
        let mut first_group = true;
        let trailing = if self.pinned_notes { entries.iter().rev().take_while(|entry| matches!(entry, MenuEntry::Note { .. })).count() } else { 0 };
        let edge_at = match entries.as_slice() {
            [.., MenuEntry::Separator, MenuEntry::Item(item)] if self.pinned_edge && item.edge => Some(entries.len() - 2),
            _ => None,
        };
        let mut pinned: Vec<AnyElement> = Vec::new();
        let mut edge: Vec<AnyElement> = Vec::new();
        for (at, entry) in entries.iter().enumerate() {
            if edge_at.is_some_and(|edge_at| at >= edge_at) {
                match entry {
                    MenuEntry::Item(item) => {
                        edge.push(self.render_row(index, item, checkable, cx));
                        index += 1;
                    }
                    _ => edge.push(separator(t).into_any_element()),
                }
                continue;
            }
            if at >= entries.len() - trailing {
                if let MenuEntry::Note { text, danger } = entry {
                    pinned.push(note(text.clone(), *danger, t, m).into_any_element());
                }
                continue;
            }
            match entry {
                MenuEntry::Item(item) => {
                    self.row_children.push(column.len());
                    column.push(self.render_row(index, item, checkable, cx));
                    index += 1;
                    first_group = false;
                }
                MenuEntry::Group { label, items } => {
                    let icon = self.group_icons.iter().find(|(named, _)| named == label).map(|(_, icon)| *icon);
                    column.push(group_label_with(icon, label.clone(), first_group, t, m).into_any_element());
                    for item in items {
                        self.row_children.push(column.len());
                        column.push(self.render_row(index, item, checkable, cx));
                        index += 1;
                    }
                    first_group = false;
                }
                MenuEntry::Separator => column.push(separator(t).into_any_element()),
                MenuEntry::Heading(label) => column.push(group_label(label.clone(), first_group, t, m).into_any_element()),
                MenuEntry::Note { text, danger } => column.push(note(text.clone(), *danger, t, m).into_any_element()),
            }
        }
        if item_count == 0 && !entries.iter().any(|entry| matches!(entry, MenuEntry::Note { .. })) {
            let empty = self.empty.clone().or_else(|| self.search.as_ref().map(|_| "No matches".into()));
            if let Some(empty) = empty {
                column.push(note(empty, false, t, m).into_any_element());
            }
        }

        let searchable = self.search.is_some();
        let frame_glass = crate::style::frame_glass(&cx.theme().environment, self.from_frame, self.top_level);
        let mut surface = glass(div(), t, GlassKind::Popover, None, None);
        // A submenu sits inside its blurred parent, so it is painted solid in the parent's colour.
        if self.parent.is_some() {
            surface.style().backdrop_filter = None;
        }
        let mut surface = surface
            .when(frame_glass, |surface| surface.bg(t.surface_glass))
            .when(self.parent.is_some(), |surface| surface.bg(gpui::Rgba { a: 1.0, ..t.surface_glass }))
            .flex()
            .flex_col()
            .min_w(self.min_width)
            .max_h(max_height)
            .text_size(px(m.text_menu))
            .child(bounds_probe(&self.surface));
        if let Some(width) = self.width {
            surface = surface.w(width);
        }
        if let Some(width) = self.max_width {
            surface = surface.max_w(width);
        }
        // `.menu-group-label` in the pane's padding; the search under it rises 5px into it.
        if let Some(title) = &self.title {
            surface = surface.child(div().flex_none().px(px(PAD + 8.0)).pt(px(PAD + 8.0)).pb(px(if self.search.is_some() { 0.0 } else { 4.0 })).text_size(px(m.text_meta)).text_color(t.quiet).font_weight(FontWeight(500.0)).child(title.clone()));
        }
        // The search field spans the pane's padding, so the rows carry it instead.
        if let Some(search) = &self.search {
            surface = surface.child(
                div()
                    .flex_none()
                    .when(self.inset_search, |search| search.mx(px(PAD)).mt(px(PAD)))
                    .rounded_t(px(9.0))
                    .px(px(6.0))
                    .pt(px(if self.title.is_some() { 5.0 } else { 6.0 }))
                    .pb(px(4.0))
                    .bg(t.surface_glass)
                    .child(SearchField::new("menu-search", &search.input).variant(SearchVariant::Menu)),
            );
        }
        // A list under a search or over pinned rows scrolls inside the pane's padding (`.menu-scroll`);
        // otherwise the whole pane scrolls, padding and all, with its bar against the pane's edge.
        let inner = searchable || !pinned.is_empty() || !edge.is_empty();
        let gutter = crate::components::scrollbar::gutter(&self.scroll);
        surface = surface.child(
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_shrink()
                .min_h_0()
                .when(inner, |wrap| wrap.mx(px(PAD)))
                .when_some(self.list_max_height, |wrap, height| wrap.max_h(height))
                .child(
                    div()
                        .id("menu-rows")
                        .flex_shrink()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .when(!inner, |rows| rows.pl(px(PAD)).pr(px(PAD) + gutter))
                        .when(inner, |rows| rows.pr(gutter))
                        .pb(px(if pinned.is_empty() && edge.is_empty() { PAD } else { 0.0 }))
                        .pt(px(if searchable { 0.0 } else { PAD }))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(column),
                )
                .child(crate::components::scrollbar::Scrollbar::vertical("menu-scrollbar", &self.scroll)),
        );
        if !pinned.is_empty() {
            surface = surface.child(div().flex_none().px(px(PAD)).pb(px(PAD)).children(pinned));
        }
        if !edge.is_empty() {
            surface = surface.child(div().flex_none().flex().flex_col().px(px(PAD)).pb(px(PAD)).children(edge));
        }
        let submenu = self.submenu.as_ref().map(|open| open.menu.clone());
        let surface = div()
            .id("menu")
            .map(crate::style::glass_in)
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Menu, self.name.clone()))
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .occlude()
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::first))
            .on_action(cx.listener(Self::last))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::open_active))
            .on_action(cx.listener(Self::close_self))
            .on_key_down(cx.listener(Self::key_down))
            .on_hover(cx.listener(|menu, hovered: &bool, _, cx| {
                if let Some(parent) = menu.parent.as_ref().and_then(|parent| parent.upgrade()) {
                    let hovered = *hovered;
                    parent.update(cx, |parent, _| parent.pointer_on_submenu = hovered);
                }
            }))
            .when(self.parent.is_none(), |this| {
                this.on_mouse_down_out(cx.listener(|menu, event: &gpui::MouseDownEvent, _, cx| {
                    let exempt = menu.exempt.as_ref().and_then(|area| area.get()).is_some_and(|area| area.contains(&event.position));
                    // A press reported outside the window is no press on the page around the menu.
                    let in_window = event.position.x >= px(0.0) && event.position.y >= px(0.0);
                    if !exempt && in_window && !menu.contains(event.position, cx) {
                        menu.dismiss(cx);
                    }
                }))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(surface)
            .children(submenu);
        let placement = self.placement.unwrap_or(Placement::At { position: gpui::point(px(0.0), px(0.0)) });
        // A submenu is drawn inside its parent's deferred layer, which cannot hold another.
        if self.parent.is_some() {
            placed(placement, surface).into_any_element()
        } else {
            deferred(placed(placement, surface)).with_priority(60).into_any_element()
        }
    }
}

impl Menu {
    /// Whether a window position falls on this menu or any list it has open.
    fn contains(&self, position: gpui::Point<Pixels>, cx: &App) -> bool {
        if self.surface.get().is_some_and(|bounds| bounds.contains(&position)) {
            return true;
        }
        self.submenu.as_ref().is_some_and(|open| open.menu.read(cx).contains(position, cx))
    }
}

fn group_label(label: SharedString, first: bool, t: &crate::theme::Tokens, m: &crate::theme::Metrics) -> gpui::Div {
    group_label_with(None, label, first, t, m)
}

fn group_label_with(icon: Option<IconName>, label: SharedString, first: bool, t: &crate::theme::Tokens, m: &crate::theme::Metrics) -> gpui::Div {
    div()
        .when_some(icon, |row, icon| row.flex().items_center().gap(px(5.0)).child(Icon::new(icon).size(px(12.0))))
        .flex_none()
        .px(px(8.0))
        .pt(px(if first { 3.0 } else { 8.0 }))
        .pb(px(4.0))
        .text_color(t.quiet)
        .text_size(px(m.text_meta))
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

fn separator(t: &crate::theme::Tokens) -> gpui::Div {
    div().flex_none().h(px(1.0)).mx(px(8.0)).my(px(5.0)).bg(t.line_glass)
}

fn note(text: SharedString, danger: bool, t: &crate::theme::Tokens, m: &crate::theme::Metrics) -> gpui::Div {
    div()
        .flex_none()
        .px(px(8.0))
        .py(px(6.0))
        .text_color(if danger { t.danger } else { t.quiet })
        .text_size(px(m.text_meta))
        .line_height(gpui::relative(1.45))
        .child(text)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TriggerVariant {
    /// A raised capsule of glass.
    #[default]
    Pill,
    /// No edge; shows its value at reading strength.
    Field,
    /// No edge; stays quiet.
    Ghost,
}

/// What opens a menu: an optional icon, the current value, and a chevron.
#[derive(IntoElement)]
pub struct MenuTrigger {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<IconName>,
    variant: TriggerVariant,
    danger: bool,
    chosen: bool,
    chevron: bool,
    open: bool,
    disabled: bool,
    tip: Option<SharedString>,
    name: Option<SharedString>,
    children: Vec<AnyElement>,
    on_click: Option<Rc<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>>,
    probe: Option<BoundsCell>,
}

impl MenuTrigger {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            icon: None,
            variant: TriggerVariant::Pill,
            danger: false,
            chosen: false,
            chevron: true,
            open: false,
            disabled: false,
            tip: None,
            name: None,
            children: Vec::new(),
            on_click: None,
            probe: None,
        }
    }

    /// What assistive technology calls it when its words alone do not say enough.
    pub fn name(mut self, name: impl Into<SharedString>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn variant(mut self, variant: TriggerVariant) -> Self {
        self.variant = variant;
        self
    }

    /// The value in the danger hue: one that acts without asking, or that could not be read.
    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    /// Tinted with the accent, as a toggle that is on.
    pub fn chosen(mut self, chosen: bool) -> Self {
        self.chosen = chosen;
        self
    }

    pub fn chevron(mut self, chevron: bool) -> Self {
        self.chevron = chevron;
        self
    }

    /// Its menu is open.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Reads as a fact rather than a faded control: keeps its words and drops the chevron.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    /// Records where the trigger was drawn, for placing its menu.
    pub fn probe(mut self, cell: &BoundsCell) -> Self {
        self.probe = Some(cell.clone());
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl ParentElement for MenuTrigger {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for MenuTrigger {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let t = &theme.tokens;
        let m = &theme.metrics;
        let pill = self.variant == TriggerVariant::Pill;
        let name = self.name.clone().or_else(|| self.label.clone()).or_else(|| self.tip.clone()).unwrap_or_default();
        let mut trigger = div()
            .id(self.id)
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, name).expanded(self.open).disabled(self.disabled))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .border_1()
            .border_color(gpui::transparent_black());
        let (mut color, mut hover_color, mut chevron_color, mut hover_chevron) = match self.variant {
            TriggerVariant::Pill => (t.ink_faint, t.ink, t.quiet, t.muted),
            TriggerVariant::Field => (t.ink_soft, t.ink_soft, t.muted, t.muted),
            TriggerVariant::Ghost => (t.muted, t.ink_soft, t.muted, t.ink_soft),
        };
        let icon_color = if self.variant == TriggerVariant::Field { Some(t.muted_soft) } else { None };
        let (mut fill, mut hover_fill) = match self.variant {
            TriggerVariant::Pill => (Some(t.glass_control), Some(t.glass_control_hover)),
            _ => (None, Some(crate::theme::Pane::solid(t.surface_hover))),
        };
        if self.open {
            fill = if pill { Some(crate::theme::Pane::solid(t.surface_active)) } else { Some(crate::theme::Pane::solid(t.surface_hover)) };
            hover_fill = fill;
            color = if pill { t.ink } else { t.ink_soft };
            chevron_color = hover_chevron;
        }
        if self.chosen {
            fill = Some(if self.open { crate::theme::Pane::sheened(t.accent_chip) } else { crate::theme::Pane::sheened(t.accent_surface) });
            hover_fill = Some(crate::theme::Pane::sheened(t.accent_chip));
            color = t.accent_soft;
            hover_color = t.accent_soft;
            chevron_color = color;
            hover_chevron = color;
            trigger = trigger.border_color(t.accent_line);
        }
        if self.danger {
            color = t.danger_soft;
            hover_color = t.danger_soft;
        }
        trigger = if pill {
            trigger
                .min_h(px(30.0))
                .border_0()
                .rounded(px(15.0))
                .px(px(8.0))
                .py(px(5.0))
                .text_size(px(m.text_control))
                .line_height(px(20.0))
        } else {
            trigger
                .min_h(px(28.0))
                .rounded(px(8.0))
                .px(px(10.0))
                .text_size(px(m.text_meta))
                .font_weight(FontWeight(560.0))
                .line_height(gpui::relative(1.2))
        };
        if let Some(fill) = fill {
            trigger = trigger.pane(fill);
        }
        if pill || self.chosen {
            trigger = trigger.rim(t.glass_rim, if pill { Corners::Radius(15.0) } else { Corners::Radius(8.0) });
        }
        trigger = trigger.text_color(color);
        if let Some(probe) = &self.probe {
            trigger = trigger.child(bounds_probe(probe));
        }
        // The chevron sits 2px closer than the gap between the other parts.
        let mut body = div().flex().items_center().gap(px(6.0));
        if let Some(icon) = self.icon {
            let glyph = Icon::new(icon).size(px(14.0));
            body = body.child(match (icon_color, self.chosen || self.danger) {
                (Some(color), false) => glyph.color(color),
                _ => glyph,
            });
        }
        if let Some(label) = self.label {
            body = body.child(div().whitespace_nowrap().child(label));
        }
        trigger = trigger.gap(px(if pill { 4.0 } else { 6.0 })).child(body.children(self.children));
        if self.chevron && !self.disabled {
            let turned = self.open && !pill;
            let ghost = self.variant == TriggerVariant::Ghost && !self.open && !self.chosen;
            trigger = trigger.child(
                div()
                    .id("chevron")
                    .flex_none()
                    .flex()
                    .items_center()
                    .transition(gpui::Transition::ease(140).only(&[gpui::TransitionProperty::Transform]).and(crate::style::touch().only(&[gpui::TransitionProperty::Color])))
                    .transform(gpui::ElementTransform::rotate(if turned { 180.0 } else { 0.0 }))
                    .when(!ghost, |chevron| {
                        chevron.text_color(chevron_color).group_hover("menu-trigger", move |style| style.text_color(hover_chevron))
                    })
                    .child(Icon::new(IconName::ChevronDown).size(px(if pill { 12.0 } else { 14.0 }))),
            );
        }
        if !self.disabled {
            trigger = trigger.group("menu-trigger").cursor(CursorStyle::PointingHand).transition(crate::style::touch()).hover(move |style| {
                let style = style.text_color(hover_color);
                match hover_fill {
                    Some(fill) => style.bg(fill.background()),
                    None => style,
                }
            });
            if let Some(handler) = self.on_click {
                trigger = trigger.on_click(move |event, window, cx| handler(event, window, cx));
            }
        }
        match self.tip {
            Some(tip) if !self.open => trigger.tip(tip),
            _ => trigger,
        }
    }
}

/// A trigger and the menu it opens, with the open state kept on the element. For a menu whose
/// open state the workspace owns, draw `MenuTrigger` and the `Menu` view directly.
#[derive(IntoElement)]
pub struct PopoverMenu {
    id: ElementId,
    trigger: Rc<dyn Fn(bool) -> MenuTrigger>,
    entries: Rc<dyn Fn(&mut Window, &mut App) -> Vec<MenuEntry>>,
    align: Align,
    configure: Option<Rc<dyn Fn(Menu) -> Menu>>,
}

impl PopoverMenu {
    pub fn new(
        id: impl Into<ElementId>,
        trigger: impl Fn(bool) -> MenuTrigger + 'static,
        entries: impl Fn(&mut Window, &mut App) -> Vec<MenuEntry> + 'static,
    ) -> Self {
        Self { id: id.into(), trigger: Rc::new(trigger), entries: Rc::new(entries), align: Align::Start, configure: None }
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    pub fn configure(mut self, configure: impl Fn(Menu) -> Menu + 'static) -> Self {
        self.configure = Some(Rc::new(configure));
        self
    }
}

struct PopoverState {
    menu: Option<Entity<Menu>>,
    probe: BoundsCell,
    _subscription: Option<Subscription>,
}

impl RenderOnce for PopoverMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| PopoverState { menu: None, probe: bounds_cell(), _subscription: None });
        let (menu, probe) = {
            let state = state.read(cx);
            (state.menu.clone(), state.probe.clone())
        };
        if let (Some(menu), Some(anchor)) = (&menu, probe.get()) {
            let placement = Placement::Anchor { anchor, align: self.align };
            menu.update(cx, |menu, cx| menu.set_placement(placement, cx));
        }
        let open = menu.is_some();
        let entries = self.entries.clone();
        let configure = self.configure.clone();
        let align = self.align;
        let trigger = (self.trigger)(open).probe(&probe).open(open).on_click(move |_, window, cx| {
            if state.read(cx).menu.is_some() {
                state.update(cx, |state, _| {
                    state.menu = None;
                    state._subscription = None;
                });
                return;
            }
            let anchor = state.read(cx).probe.get().unwrap_or_default();
            let list = entries(window, cx);
            let configure = configure.clone();
            let probe = state.read(cx).probe.clone();
            let menu = Menu::build(list, Placement::Anchor { anchor, align }, window, cx, move |menu| match &configure {
                Some(configure) => configure(menu.exempt(&probe)),
                None => menu.exempt(&probe),
            });
            let weak = state.downgrade();
            let subscription = cx.subscribe(&menu, move |_, _: &DismissEvent, cx| {
                if let Some(state) = weak.upgrade() {
                    state.update(cx, |state, cx| {
                        state.menu = None;
                        state._subscription = None;
                        cx.notify();
                    });
                }
            });
            state.update(cx, |state, cx| {
                state.menu = Some(menu);
                state._subscription = Some(subscription);
                cx.notify();
            });
        });
        div().relative().flex().child(trigger).children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::typeahead_match;

    #[test]
    fn a_repeated_letter_steps_through_rows_with_it() {
        let rows = [("Archive", false), ("Branch", false), ("Bookmark", false), ("Build", true)];
        assert_eq!(typeahead_match(&rows, None, "b"), Some(1));
        assert_eq!(typeahead_match(&rows, Some(1), "bb"), Some(2));
        assert_eq!(typeahead_match(&rows, Some(2), "bbb"), Some(1));
    }

    #[test]
    fn several_letters_match_a_prefix_from_the_current_row() {
        let rows = [("Archive", false), ("Branch", false), ("Bookmark", false)];
        assert_eq!(typeahead_match(&rows, Some(1), "bo"), Some(2));
        assert_eq!(typeahead_match(&rows, None, "zz"), None);
    }
}
