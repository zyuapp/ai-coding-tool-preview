//! Settings, as a sheet over the window: the page list with its search, and every page
//! (WorkspaceSettings.tsx, SettingsPanel.tsx, SettingsSidebar.tsx, SettingsLayout.tsx, SettingRow.tsx
//! and the page components, one module each).

mod appearance;
mod archive;
mod browser;
mod computer_use;
mod devices;
mod engines;
mod general;
mod projects;
mod shortcuts;
mod usage;
mod worktrees;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use act_core::domain::settings_catalog::{rank_settings_jumps, settings_jump_options, settings_page_label, SettingsJumpOption};
use act_core::domain::sidebar::{SettingsSection, SETTINGS_SECTIONS};
use act_core::events::WorkspaceEvent;
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::Button;
use act_ui::components::field::SearchField;
use act_ui::input::{InputEvent, TextInput};
use act_ui::overlay::{bounds_cell, bounds_probe, BoundsCell};
use act_ui::style::PaneFill;
use act_ui::theme::{ActiveTheme, Tokens};
use act_ui::IconName;
use gpui::{
    div, point, prelude::*, px, relative, Animation, AnimationExt, AnyElement, App, Context, Entity, FontWeight, KeyDownEvent, Rgba, ScrollHandle, SharedString, Subscription, Task, Window,
};

use super::common::{icon, ink_icon, rimmed};
use super::letter_spacing::LetterSpacing;
use super::root::Root;
use crate::workspace::dispatch;

/// A field a page owns while it is drawn.
pub(super) struct Held {
    pub input: Entity<TextInput>,
    _subscription: Subscription,
}

impl Held {
    pub fn new(
        text: &str,
        placeholder: &str,
        on_event: impl Fn(&mut Root, &Entity<TextInput>, &InputEvent, &mut Context<Root>) + 'static,
        cx: &mut Context<Root>,
    ) -> Held {
        let placeholder = placeholder.to_string();
        let input = cx.new(|cx| TextInput::new(cx).with_text(text).placeholder(placeholder));
        let subscription = cx.subscribe(&input, move |root, input, event: &InputEvent, cx| {
            on_event(root, &input, event, cx);
            cx.notify();
        });
        Held { input, _subscription: subscription }
    }

    pub fn text(&self, cx: &App) -> String {
        self.input.read(cx).text().to_string()
    }
}

/// Where each marked control was last drawn, and which one a search last landed on.
#[derive(Clone, Default)]
pub(super) struct Marks {
    cells: Rc<RefCell<HashMap<String, BoundsCell>>>,
    /// Whether each confirming button is asking its question now, so its neighbours step aside.
    armed: Rc<RefCell<HashMap<String, Rc<std::cell::Cell<bool>>>>>,
    /// The control flashing now, keyed by the visit that sent the user to it.
    flash: Option<(String, i64)>,
    /// The control outlined until the user next presses or types.
    landed: Option<String>,
    /// The first control of each marked setting, which takes the keyboard when a search lands there.
    stops: Rc<RefCell<HashMap<String, gpui::FocusHandle>>>,
    /// The page text's normal line, which a row's title sits on: its height, and how far the
    /// title's baseline sits above a 14px line's own.
    title_line: (f32, f32),
}

impl Marks {
    /// The keyboard stop of a marked setting's first control.
    pub fn stop(&self, id: &str, cx: &mut App) -> gpui::FocusHandle {
        self.stops.borrow_mut().entry(id.to_string()).or_insert_with(|| cx.focus_handle().tab_stop(true)).clone()
    }

    fn cell(&self, id: &str) -> BoundsCell {
        self.cells.borrow_mut().entry(id.to_string()).or_insert_with(bounds_cell).clone()
    }

    pub fn armed(&self, id: &str) -> Rc<std::cell::Cell<bool>> {
        self.armed.borrow_mut().entry(id.to_string()).or_default().clone()
    }

    /// Lays the flash and the outline over a control a search landed on, and records where it is.
    pub fn wrap(&self, id: &str, element: gpui::Div, t: &Tokens) -> AnyElement {
        let element = element.relative().child(bounds_probe(&self.cell(id)));
        let element = if self.landed.as_deref() == Some(id) {
            element.child(div().absolute().top(px(-4.0)).left(px(-4.0)).right(px(-4.0)).bottom(px(-4.0)).rounded(px(12.0)).border_2().border_color(t.accent))
        } else {
            element
        };
        match &self.flash {
            Some((flashing, visit)) if flashing == id => {
                let chip = t.accent_chip;
                element
                    .rounded(px(8.0))
                    .with_animation(SharedString::from(format!("found-{id}-{visit}")), Animation::new(Duration::from_millis(2200)).with_easing(gpui::ease_out_quint()), move |element, delta| {
                        let alpha = if delta < 0.6 { 1.0 } else { 1.0 - (delta - 0.6) / 0.4 };
                        element.bg(Rgba { a: chip.a * alpha, ..chip })
                    })
                    .into_any_element()
            }
            _ => element.into_any_element(),
        }
    }
}

pub struct SettingsState {
    search: Option<(Entity<TextInput>, Subscription)>,
    picked: usize,
    /// The picked result moved by the keyboard, so the list brings it into view.
    picked_moved: bool,
    nav_scroll: ScrollHandle,
    main_scroll: ScrollHandle,
    main_bounds: BoundsCell,
    marks: Marks,
    seen_visit: Option<(String, i64)>,
    /// The page whose reads have been asked for since it was opened.
    mounted: Option<SettingsSection>,
    /// The fast read of what the platform allows computer use, while settings is open.
    computer_use_poll: Option<Task<()>>,
    /// One keyboard stop per page in the list, walked by the arrow keys.
    nav_focus: Vec<gpui::FocusHandle>,
    pub(super) appearance: appearance::AppearanceState,
    pub(super) general: general::GeneralState,
    pub(super) archive: archive::ArchiveState,
    pub(super) browser: browser::BrowserState,
    pub(super) devices: devices::DevicesState,
    pub(super) shortcuts: shortcuts::ShortcutsState,
}

impl Default for SettingsState {
    fn default() -> Self {
        SettingsState {
            search: None,
            picked: 0,
            picked_moved: false,
            nav_scroll: ScrollHandle::new(),
            main_scroll: ScrollHandle::new(),
            main_bounds: bounds_cell(),
            marks: Marks::default(),
            seen_visit: None,
            mounted: None,
            computer_use_poll: None,
            nav_focus: Vec::new(),
            appearance: Default::default(),
            general: Default::default(),
            archive: Default::default(),
            browser: Default::default(),
            devices: Default::default(),
            shortcuts: Default::default(),
        }
    }
}

impl SettingsState {
    /// Settings closed: the search starts empty and takes the keyboard next time, and every page
    /// reads afresh when it opens again.
    pub fn closed(&mut self) {
        if self.search.is_none() && self.mounted.is_none() {
            return;
        }
        *self = SettingsState { seen_visit: self.seen_visit.take(), ..SettingsState::default() };
    }

    fn scroll_to_mark(&mut self, id: &str) {
        let Some(row) = self.marks.cell(id).get() else { return };
        let Some(view) = self.main_bounds.get() else { return };
        let offset = self.main_scroll.offset();
        let max = self.main_scroll.max_offset();
        let delta = row.center().y - view.center().y;
        let y = (offset.y - delta).max(-max.height).min(px(0.0));
        self.main_scroll.set_offset(point(offset.x, y));
    }

    /// Takes the outline off once the user presses or types anywhere.
    fn let_go(&mut self) -> bool {
        self.marks.landed.take().is_some()
    }
}

pub(super) fn section_id(section: SettingsSection) -> String {
    serde_json::to_value(section).ok().and_then(|value| value.as_str().map(String::from)).unwrap_or_default()
}

pub(super) fn page_icon(section: SettingsSection) -> IconName {
    match section {
        SettingsSection::General => IconName::SlidersHorizontal,
        SettingsSection::Appearance => IconName::Palette,
        SettingsSection::Usage => IconName::Gauge,
        SettingsSection::Engines => IconName::Cpu,
        SettingsSection::Projects => IconName::Folder,
        SettingsSection::Worktrees => IconName::FolderGit2,
        SettingsSection::Shortcuts => IconName::Keyboard,
        SettingsSection::ComputerUse => IconName::MonitorCog,
        SettingsSection::Browser => IconName::Globe,
        SettingsSection::Phone => IconName::Smartphone,
        SettingsSection::Archive => IconName::Archive,
    }
}

/// Opens a page, marking the control a search named. Three pages ask for a fresh read as they are opened.
fn land(section: SettingsSection, setting_id: Option<String>, cx: &mut App) {
    dispatch(AppCommand::SetSettingsOpen { open: true, section: Some(section), setting_id }, cx);
    match section {
        SettingsSection::Engines => dispatch(AppCommand::EngineRead { refresh: Some(true) }, cx),
        SettingsSection::Worktrees => dispatch(AppCommand::WorktreeRefresh, cx),
        SettingsSection::Phone => dispatch(AppCommand::RemoteRefresh, cx),
        _ => {}
    }
}

/// The reads a page asks for each time it is opened.
fn mount(section: SettingsSection, cx: &mut App) {
    match section {
        SettingsSection::General => dispatch(AppCommand::CliRead, cx),
        SettingsSection::Usage => dispatch(AppCommand::UsageRead { everywhere: Some(true) }, cx),
        SettingsSection::Browser => dispatch(AppCommand::BrowserImportRead, cx),
        SettingsSection::Phone => dispatch(AppCommand::ComputersDiscover, cx),
        _ => {}
    }
}

pub(super) fn event(event: WorkspaceEvent, cx: &mut App) {
    let workspace = crate::workspace::workspace(cx);
    workspace.update(cx, |workspace, cx| workspace.dispatch(act_core::events::WorkspaceInput::Event(event), cx));
}

/// While a shortcut row waits for a keystroke, every keystroke goes to it: Escape gives up, and one
/// held with a modifier becomes the binding. Returns whether the keystroke was taken.
pub fn capture_key(state: &WorkspaceState, keystroke: &gpui::Keystroke, cx: &mut App) -> bool {
    if state.capturing_shortcut.is_none() {
        return false;
    }
    if keystroke.key == "escape" {
        event(WorkspaceEvent::ShortcutCaptured { binding: None }, cx);
        return true;
    }
    let Some(stroke) = crate::keys::keystroke(keystroke) else { return false };
    if !(stroke.r#mod || stroke.ctrl || stroke.alt) {
        return false;
    }
    event(WorkspaceEvent::ShortcutCaptured { binding: Some(act_core::domain::shortcuts::format_shortcut(&stroke)) }, cx);
    true
}

// Layout pieces every page shares (SettingsLayout.tsx, SettingRow.tsx, settings.css).

/// The normal line of the UI face at `size` as Chromium lays it (ascent, descent and gap rounded to
/// device pixels), and how much deeper its descent is than a 14px line's.
fn title_line(size: f32, window: &Window, cx: &App) -> (f32, f32) {
    let text = window.text_system();
    let font = text.resolve_font(&gpui::font(cx.theme().typography.ui_family.clone()));
    let scale = window.scale_factor();
    let device = |metric: gpui::Pixels| f32::from(metric).abs().round() / scale;
    let at = |size: f32| px(size) * scale;
    let line = device(text.ascent(font, at(size))) + device(text.descent(font, at(size))) + device(text.line_gap(font, at(size)));
    (line, device(text.descent(font, at(size))) - device(text.descent(font, at(14.0))))
}

/// A short window spends its height on the pages themselves rather than on the space above them.
fn short_window(window: &Window) -> bool {
    f32::from(window.viewport_size().height) <= 660.0
}

/// A settings page: its title, what it is for, one action over every section, and its sections.
pub(super) fn page(title: &str, description: Option<AnyElement>, action: Option<AnyElement>, window: &Window, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    let t = &theme.tokens;
    let mut text = div().flex().flex_col().child(div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Heading, title.to_string())).text_color(t.ink).text_size(px(28.0)).font_weight(FontWeight(610.0)).letter_spacing(-0.035).child(title.to_string()));
    if let Some(description) = description {
        text = text.child(div().mt(px(9.0)).text_color(t.quiet_strong).text_size(px(theme.metrics.text_content)).line_height(relative(1.5)).child(description));
    }
    let heading = div().mb(px(if short_window(window) { 28.0 } else { 48.0 })).child(text);
    let heading = match action {
        Some(action) => heading.flex().items_end().justify_between().gap(px(24.0)).child(div().flex_none().flex().items_center().gap(px(10.0)).child(action)),
        None => heading,
    };
    div().w_full().flex().flex_col().child(heading)
}

pub(super) fn description(content: impl Into<SharedString>) -> AnyElement {
    div().child(content.into()).into_any_element()
}

/// A piece of a line that flows like inline text: words that wrap one at a time, or an element kept whole.
pub(super) enum Inline {
    Words(String),
    Element(AnyElement),
}

/// Inline text: words wrap one at a time around the elements set between them.
pub(super) fn flow(parts: Vec<Inline>) -> AnyElement {
    let mut items: Vec<AnyElement> = Vec::new();
    for part in parts {
        match part {
            Inline::Words(text) => {
                let lead = text.starts_with(' ');
                let words: Vec<&str> = text.split_whitespace().collect();
                let trail = text.ends_with(' ');
                for (index, word) in words.iter().enumerate() {
                    let space_before = index == 0 && lead;
                    let space_after = index + 1 < words.len() || trail;
                    let word = format!("{}{word}{}", if space_before { " " } else { "" }, if space_after { " " } else { "" });
                    items.push(div().whitespace_nowrap().child(word).into_any_element());
                }
            }
            Inline::Element(element) => items.push(element),
        }
    }
    div().flex().flex_wrap().items_baseline().children(items).into_any_element()
}

/// Text with inline code, as a description reads it: plain runs and `code` runs alternating.
pub(super) fn with_code(parts: &[(&str, bool)], cx: &App) -> AnyElement {
    let t = &cx.theme().tokens;
    let mono = cx.theme().typography.mono_family.clone();
    flow(
        parts
            .iter()
            .enumerate()
            .map(|(index, (text, code))| {
                if *code {
                    return Inline::Element(div().px(px(5.0)).py(px(1.0)).rounded(px(5.0)).bg(t.wash).text_color(t.ink_dim).font_family(mono.clone()).text_size(px(12.6)).line_height(relative(1.4)).child(text.to_string()).into_any_element());
                }
                let after_code = index > 0 && parts[index - 1].1 && !text.starts_with([',', '.', ';', ':']);
                let before_code = parts.get(index + 1).is_some_and(|next| next.1);
                Inline::Words(format!("{}{text}{}", if after_code { " " } else { "" }, if before_code { " " } else { "" }))
            })
            .collect(),
    )
}

/// A titled group of rows on a page.
pub(super) struct Section {
    title: Option<AnyElement>,
    description: Option<AnyElement>,
    detail: Option<AnyElement>,
    badge: Option<AnyElement>,
    action: Option<AnyElement>,
    mark: Option<String>,
    first: bool,
    rows: Vec<AnyElement>,
}

impl Section {
    pub fn new(title: impl Into<SharedString>) -> Section {
        let title: SharedString = title.into();
        Section::titled(div().child(title).into_any_element())
    }

    pub fn titled(title: AnyElement) -> Section {
        Section { title: Some(title), description: None, detail: None, badge: None, action: None, mark: None, first: false, rows: Vec::new() }
    }

    /// A section a search can land on, named by its catalog entry.
    pub fn setting(id: &str) -> Section {
        let label = act_core::domain::settings_catalog::settings_controls().into_iter().find(|control| control.id == id).map(|control| control.label).unwrap_or_default();
        Section { mark: Some(id.to_string()), ..Section::new(label) }
    }

    pub fn description(mut self, description: impl IntoElement) -> Self {
        self.description = Some(description.into_any_element());
        self
    }

    pub fn detail(mut self, detail: impl IntoElement) -> Self {
        self.detail = Some(detail.into_any_element());
        self
    }

    pub fn badge(mut self, badge: impl IntoElement) -> Self {
        self.badge = Some(badge.into_any_element());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }

    pub fn first(mut self) -> Self {
        self.first = true;
        self
    }

    pub fn row(mut self, row: impl IntoElement) -> Self {
        self.rows.push(row.into_any_element());
        self
    }

    pub fn rows(mut self, rows: impl IntoIterator<Item = AnyElement>) -> Self {
        self.rows.extend(rows);
        self
    }

    pub fn build(self, marks: &Marks, t: &Tokens) -> AnyElement {
        // A bare title holds its width against the actions; one with words under it wraps them instead.
        let bare = self.description.is_none() && self.detail.is_none();
        let mut text = if bare { div().flex_none() } else { div().flex_1().min_w_0() }.flex().flex_col();
        if let Some(title) = self.title {
            text = text.child(div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Heading, "")).text_color(t.ink_soft).text_size(px(17.0)).font_weight(FontWeight(590.0)).child(title));
        }
        if let Some(description) = self.description {
            text = text.child(div().mt(px(6.0)).text_color(t.quiet_strong).text_size(px(14.0)).line_height(relative(1.45)).child(description));
        }
        if let Some(detail) = self.detail {
            text = text.child(detail);
        }
        let mut heading = div().flex().items_start().justify_between().gap(px(20.0)).pb(px(14.0)).child(text);
        if let Some(badge) = self.badge {
            heading = heading.child(div().flex_none().child(badge));
        }
        if let Some(action) = self.action {
            heading = heading.child(div().flex_none().flex().items_center().gap(px(10.0)).child(action));
        }
        let heading = match &self.mark {
            Some(id) => marks.wrap(id, heading, t),
            None => heading.into_any_element(),
        };
        div().flex().flex_col().when(!self.first, |group| group.mt(px(40.0))).child(heading).children(self.rows).into_any_element()
    }
}

/// The circle at the start of a row: ticked once its step is done, a dashed ring while it is still to do.
/// A quiet one draws only its icon until it is done.
pub(super) fn status(done: bool, quiet: bool, glyph: Option<IconName>, t: &Tokens) -> AnyElement {
    let circle = div().size(px(20.0)).flex_none().rounded_full().flex().items_center().justify_center();
    let glyph = glyph.or(done.then_some(IconName::Check));
    let color = if done { t.success_ink } else { t.quiet_strong };
    let circle = if done {
        circle.border_1().border_color(t.success_line).bg(t.success_soft)
    } else if quiet {
        circle.border_1().border_color(gpui::transparent_black())
    } else {
        circle.border_1().border_dashed().border_color(t.quiet)
    };
    // A step's circle says aloud whether it is done; a quiet one says nothing.
    circle
        .when(!quiet, |circle| circle.accessible(gpui::AccessInfo::new(gpui::AccessRole::Label, if done { "Done" } else { "Not done" })))
        .when_some(glyph, |circle, glyph| circle.child(icon(glyph, 14.0, color)))
        .into_any_element()
}

/// A glyph in a row's lead column.
pub(super) fn glyph(name: IconName, color: Rgba) -> AnyElement {
    div().size(px(20.0)).flex().items_center().justify_center().child(icon(name, 16.0, color)).into_any_element()
}

/// The quiet status word at a row's or heading's end (`.setting-pill`, the heading badge).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PillTone {
    Accent,
    Ready,
    Blocked,
    Neutral,
}

pub(super) fn pill(label: impl Into<SharedString>, tone: PillTone, t: &Tokens) -> AnyElement {
    let (fill, color) = match tone {
        PillTone::Accent => (t.accent_surface, t.accent_soft),
        PillTone::Ready => (act_ui::color::mix(t.success, 0.08, t.canvas), t.success_soft),
        PillTone::Blocked => (t.danger_surface, t.danger),
        PillTone::Neutral => (t.wash, t.muted),
    };
    rimmed(div(), t, 999.0).flex_none().px(px(9.0)).py(px(5.0)).bg(fill).text_color(color).text_size(px(12.0)).font_weight(FontWeight(600.0)).whitespace_nowrap().child(label.into()).into_any_element()
}

/// The quiet `em` a row's action column says instead of a control.
pub(super) fn em(content: impl IntoElement, color: Rgba) -> AnyElement {
    act_ui::style::tabular_nums(div().flex().items_center().gap(px(6.0)).text_color(color).text_size(px(12.0))).child(content).into_any_element()
}

pub(super) fn empty(message: impl Into<SharedString>, t: &Tokens) -> AnyElement {
    div().border_t_1().border_color(t.line).py(px(19.0)).text_color(t.quiet_strong).text_size(px(14.0)).child(message.into()).into_any_element()
}

pub(super) fn error(message: impl IntoElement, t: &Tokens) -> gpui::Div {
    div().flex().flex_wrap().items_center().border_t_1().border_color(t.danger_line).py(px(13.0)).text_color(t.danger_soft).text_size(px(14.0)).child(message)
}

pub(super) fn notice(message: impl Into<SharedString>, t: &Tokens) -> AnyElement {
    div().border_t_1().border_color(t.line).py(px(13.0)).text_color(t.success_soft).text_size(px(14.0)).child(message.into()).into_any_element()
}

pub(super) fn strong(title: impl Into<SharedString>, t: &Tokens) -> AnyElement {
    div().text_color(t.ink_dim).text_size(px(14.0)).font_weight(FontWeight(570.0)).child(title.into()).into_any_element()
}

pub(super) fn line(text: impl Into<SharedString>, t: &Tokens) -> AnyElement {
    div().mt(px(5.0)).text_color(t.quiet_strong).text_size(px(12.0)).line_height(relative(1.45)).child(text.into()).into_any_element()
}

/// One line of a page: a lead mark, a name over what it says, and the controls at its end.
pub(super) struct Row {
    lead: Option<AnyElement>,
    title: AnyElement,
    description: Vec<AnyElement>,
    actions: Vec<AnyElement>,
    footer: Option<AnyElement>,
    mark: Option<String>,
    compact: bool,
    action_width: f32,
    action_gap: f32,
    capturing: bool,
    /// The row above is capturing a keystroke, whose tint takes this row's top border.
    under_capture: bool,
    on_title_click: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    /// The title is a block of its own rather than text on the row's line.
    block_title: bool,
}

impl Row {
    pub fn new(title: impl Into<SharedString>, t: &Tokens) -> Row {
        Row::titled(strong(title, t))
    }

    pub fn titled(title: AnyElement) -> Row {
        Row {
            lead: None,
            title,
            description: Vec::new(),
            actions: Vec::new(),
            footer: None,
            mark: None,
            compact: false,
            action_width: 150.0,
            action_gap: 12.0,
            capturing: false,
            under_capture: false,
            on_title_click: None,
            block_title: false,
        }
    }

    pub fn block_title(mut self) -> Self {
        self.block_title = true;
        self
    }

    /// A row named by its catalog entry, which a search can land on.
    pub fn setting(id: &str, t: &Tokens) -> Row {
        let label = act_core::domain::settings_catalog::settings_controls().into_iter().find(|control| control.id == id).map(|control| control.label).unwrap_or_default();
        Row { mark: Some(id.to_string()), ..Row::new(label, t) }
    }

    pub fn mark(mut self, id: impl Into<String>) -> Self {
        self.mark = Some(id.into());
        self
    }

    pub fn lead(mut self, lead: AnyElement) -> Self {
        self.lead = Some(lead);
        self
    }

    pub fn status(self, done: bool, t: &Tokens) -> Self {
        self.lead(status(done, false, None, t))
    }

    pub fn line(mut self, text: impl Into<SharedString>, t: &Tokens) -> Self {
        self.description.push(line(text, t));
        self
    }

    pub fn describe(mut self, element: impl IntoElement) -> Self {
        self.description.push(element.into_any_element());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub fn actions(mut self, actions: impl IntoIterator<Item = AnyElement>) -> Self {
        self.actions.extend(actions);
        self
    }

    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    pub fn action_column(mut self, width: f32, gap: f32) -> Self {
        self.action_width = width;
        self.action_gap = gap;
        self
    }

    pub fn capturing(mut self, capturing: bool) -> Self {
        self.capturing = capturing;
        self
    }

    pub fn under_capture(mut self, under: bool) -> Self {
        self.under_capture = under;
        self
    }

    /// The row's text works the switch it holds.
    pub fn on_title_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_title_click = Some(Rc::new(handler));
        self
    }

    pub fn build(self, marks: &Marks, t: &Tokens) -> AnyElement {
        let has_lead = self.lead.is_some();
        // The title sits on the baseline of a line as tall as the page's own text, as an inline
        // `strong` in a block does.
        let (strut, lift) = marks.title_line;
        let title = if self.block_title { div().child(self.title) } else { div().min_h(px(strut)).flex().flex_col().justify_end().pb(px(lift)).child(self.title) };
        let text = div().flex_1().min_w_0().flex().flex_col().child(title).children(self.description);
        let text = match self.on_title_click {
            Some(handler) => {
                let id = SharedString::from(format!("row-text-{}", self.mark.clone().unwrap_or_default()));
                // A label for the row's control, as in the page: it is not a stop of its own.
                div().id(id).skip_tab_stop().accessible(gpui::AccessInfo::new(gpui::AccessRole::Group, "")).flex_1().min_w_0().cursor_pointer().on_click(move |_, window, cx| handler(window, cx)).child(text).into_any_element()
            }
            None => text.into_any_element(),
        };
        let mut line = div().flex().items_center().gap(px(13.0));
        if let Some(lead) = self.lead {
            line = line.child(div().w(px(22.0)).flex_none().flex().justify_center().child(lead));
        }
        line = line.child(text);
        // The action column holds its width even when empty; a compact row's is only as wide as what it holds.
        let action_width = if self.compact { 0.0 } else { self.action_width };
        line = line.child(div().flex_none().min_w(px(action_width)).flex().items_center().justify_end().gap(px(self.action_gap)).children(self.actions));
        let border: gpui::Hsla = if self.under_capture { gpui::transparent_black() } else { t.line.into() };
        let mut row = div().flex().flex_col().border_t_1().border_color(border).py(px(if self.compact { 13.0 } else { 19.0 })).child(line);
        if self.capturing {
            row = rimmed(row, t, 8.0).mx(px(-12.0)).px(px(12.0)).border_color(gpui::transparent_black()).pane(act_ui::theme::Pane::sheened(t.accent_surface));
        }
        if let Some(footer) = self.footer {
            row = row.child(div().when(has_lead, |footer| footer.pl(px(35.0))).child(footer));
        }
        match &self.mark {
            Some(id) => marks.wrap(id, row, t),
            None => row.into_any_element(),
        }
    }
}

/// A button whose icon turns while its work runs, keeping its label (`SpinningIcon` in a `Button`).
pub(super) fn spinning_button(id: impl Into<SharedString>, label: impl Into<SharedString>, glyph: IconName, spinning: bool) -> Button {
    let id: SharedString = id.into();
    let label: SharedString = label.into();
    let mark = if spinning {
        act_ui::components::spinner::Spinning::new(SharedString::from(format!("{id}-spin")), glyph).size(px(14.0)).into_any_element()
    } else {
        act_ui::Icon::new(glyph).size(px(14.0)).into_any_element()
    };
    Button::bare(id).child(mark).child(label)
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let current = state.settings_page().unwrap_or(SettingsSection::General);
    if root.settings.search.is_none() {
        // The sheet covers the window, so a menu the window had open goes with it.
        if root.menus.any_open() {
            root.menus.close(cx);
        }
        let input = cx.new(|cx| TextInput::new(cx).placeholder("Search").label("Search settings"));
        let subscription = cx.subscribe(&input, |root, input, event: &InputEvent, cx| match event {
            InputEvent::Changed => {
                root.settings.picked = 0;
                cx.notify();
            }
            InputEvent::Submit => {
                let query = input.read(cx).text().trim().to_lowercase();
                let results = if query.is_empty() { Vec::new() } else { rank_settings_jumps(&query, settings_jump_options().len()) };
                if let Some(option) = results.get(root.settings.picked.min(results.len().saturating_sub(1))).cloned() {
                    input.update(cx, |input, cx| input.reset("", cx));
                    land(option.section, option.setting_id, cx);
                }
            }
            _ => {}
        });
        input.update(cx, |input, _| input.focus(window));
        root.settings.search = Some((input, subscription));
        dispatch(AppCommand::ComputerUseRead, cx);
    }
    if root.settings.computer_use_poll.is_none() && state.computer_use_permissions.permissions.as_ref().is_none_or(|permissions| permissions.linux_runtime.is_none()) {
        root.settings.computer_use_poll = Some(cx.spawn(async |root, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let Ok(polling) = root.update(cx, |root, cx| {
                let state = root.state(cx);
                let polling = state.settings_shown() && state.computer_use_permissions.permissions.as_ref().is_none_or(|permissions| permissions.linux_runtime.is_none());
                if polling {
                    dispatch(AppCommand::ComputerUseRead, cx);
                } else {
                    root.settings.computer_use_poll = None;
                }
                polling
            }) else {
                break;
            };
            if !polling {
                break;
            }
        }));
    }
    if root.settings.mounted != Some(current) {
        root.settings.mounted = Some(current);
        root.settings.main_scroll.set_offset(point(px(0.0), px(0.0)));
        mount(current, cx);
    }
    if let Some(mark) = &state.settings_focus {
        let visit = (mark.id.clone(), mark.visit);
        if root.settings.seen_visit.as_ref() != Some(&visit) {
            root.settings.seen_visit = Some(visit.clone());
            root.settings.marks.flash = Some(visit);
            root.settings.marks.landed = Some(mark.id.clone());
            let id = mark.id.clone();
            cx.on_next_frame(window, move |root, window, cx| {
                root.settings.scroll_to_mark(&id);
                let stop = root.settings.marks.stops.borrow().get(&id).cloned();
                if let Some(stop) = stop {
                    window.focus(&stop);
                }
                cx.notify();
            });
        }
    }

    let search_input = root.settings.search.as_ref().map(|(input, _)| input.clone()).expect("the search is made above");
    let typed = search_input.read(cx).text().to_string();
    let query = typed.trim().to_lowercase();
    let results: Vec<SettingsJumpOption> = if query.is_empty() { Vec::new() } else { rank_settings_jumps(&query, settings_jump_options().len()) };
    if !results.is_empty() {
        root.settings.picked = root.settings.picked.min(results.len() - 1);
    }

    let row_button = |id: SharedString| div().id(id).w_full().flex().items_center().gap(px(10.0)).px(px(10.0)).py(px(9.0)).rounded(px(9.0)).text_size(px(14.0)).cursor_pointer();
    let nav_access = if query.is_empty() {
        gpui::AccessInfo::new(gpui::AccessRole::Navigation, "Settings sections")
    } else if results.is_empty() {
        gpui::AccessInfo::new(gpui::AccessRole::Group, "")
    } else {
        gpui::AccessInfo::new(gpui::AccessRole::List, "Matching settings")
    };
    let mut nav = div().id("settings-nav").accessible(nav_access).flex_1().min_h_0().overflow_y_scroll().track_scroll(&root.settings.nav_scroll).flex().flex_col();
    if query.is_empty() {
        while root.settings.nav_focus.len() < SETTINGS_SECTIONS.len() {
            root.settings.nav_focus.push(cx.focus_handle());
        }
        let keyboard = act_ui::focus::keyboard_focus_visible(cx);
        for (index, page) in SETTINGS_SECTIONS.into_iter().enumerate() {
            let active = page == current;
            // The page shown is the list's one stop; arrows walk the rest.
            let focus = root.settings.nav_focus[index].clone().tab_stop(active);
            let ringed = keyboard && focus.is_focused(window);
            nav = nav.child(
                row_button(SharedString::from(format!("settings-page-{}", section_id(page))))
                    .accessible_name(settings_page_label(page))
                    .track_focus(&focus)
                    .relative()
                    .when(ringed, |button| button.child(div().absolute().inset_0().rounded(px(9.0)).border_2().border_color(t.focus_ring)))
                    .on_key_down(move |event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            cx.stop_propagation();
                            land(page, None, cx);
                        }
                    })
                    .map(|button| if active { rimmed(button, t, 9.0).bg(t.frame_row_selected).text_color(t.ink).font_weight(FontWeight(560.0)) } else { button.text_color(t.muted).hover(|style| style.bg(t.frame_row_hover).text_color(t.ink_dim)) })
                    .child(icon(page_icon(page), 18.0, if active { t.accent } else { t.muted }))
                    .child(settings_page_label(page))
                    .on_click(move |_, _, cx| land(page, None, cx)),
            );
        }
    } else if results.is_empty() {
        let input = search_input.clone();
        nav = nav.child(div().px(px(10.0)).pt(px(9.0)).pb(px(6.0)).text_color(t.muted).text_size(px(14.0)).child("No settings match")).child(
            row_button("settings-show-all".into()).keep_focus_on_click().text_color(t.muted).hover(|style| style.bg(t.frame_row_hover).text_color(t.ink_dim)).child("Show all pages").on_click(move |_, window, cx| {
                input.update(cx, |input, cx| {
                    input.reset("", cx);
                    input.focus(window);
                });
            }),
        );
    } else {
        let picked_index = root.settings.picked;
        for (index, option) in results.iter().cloned().enumerate() {
            let picked = index == picked_index;
            let input = search_input.clone();
            let title = option.title.clone();
            let at = title.to_lowercase().find(&query).filter(|at| title.is_char_boundary(*at) && title.is_char_boundary(at + query.len()));
            let label = match at {
                Some(at) => div()
                    .min_w_0()
                    .flex()
                    .overflow_hidden()
                    .child(div().flex_none().child(title[..at].to_string()))
                    .child(div().flex_none().text_color(t.ink).font_weight(FontWeight(650.0)).child(title[at..at + query.len()].to_string()))
                    .child(div().min_w_0().clip().child(title[at + query.len()..].to_string())),
                None => div().min_w_0().clip().child(title.clone()),
            };
            nav = nav.child(
                row_button(SharedString::from(format!("settings-search-{}", option.id)))
                    .skip_tab_stop()
                    .keep_focus_on_click()
                    .accessible(gpui::AccessInfo::new(gpui::AccessRole::ListItem, option.title.clone()).selected(picked))
                    .map(|button| if picked { rimmed(button, t, 9.0).bg(t.frame_row_selected).text_color(t.ink).font_weight(FontWeight(560.0)) } else { button.text_color(t.muted).hover(|style| style.bg(t.frame_row_hover).text_color(t.ink_dim)) })
                    .child(icon(page_icon(option.section), 18.0, if picked { t.accent } else { t.muted }))
                    .child(div().flex_1().min_w_0().flex().child(label))
                    .when_some(option.page.clone(), |button, page| button.child(div().ml_auto().flex_none().max_w(relative(0.45)).clip().text_color(t.quiet_strong).text_size(px(12.0)).font_weight(FontWeight(400.0)).child(page)))
                    .on_mouse_move(cx.listener(move |root, _, _, cx| {
                        if root.settings.picked != index {
                            root.settings.picked = index;
                            cx.notify();
                        }
                    }))
                    .on_click(move |_, _, cx| {
                        input.update(cx, |input, cx| input.reset("", cx));
                        land(option.section, option.setting_id.clone(), cx);
                    }),
            );
        }
        if root.settings.picked_moved {
            root.settings.picked_moved = false;
            root.settings.nav_scroll.scroll_to_item(picked_index);
        }
    }

    let short = short_window(window);
    let clear_input = search_input.clone();
    let sidebar = div()
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Complementary, ""))
        .w(px(260.0))
        .relative()
        .when(t.page.a == 0.0, |sidebar| sidebar.child(div().absolute().top_0().bottom_0().right_0().w(px(1.0)).bg(t.line_glass)))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .pl(px(12.0))
        .pr(px(4.0))
        .pb(px(18.0))
        .child(div().h(px(m.settings_titlebar_inset.max(12.0))).flex_none())
        .child(
            div()
                .id("settings-back")
                .flex_none()
                .flex()
                .items_center()
                .gap(px(9.0))
                .px(px(10.0))
                .py(px(9.0))
                .rounded(px(9.0))
                .text_color(t.muted)
                .text_size(px(14.0))
                .cursor_pointer()
                .hover(|style| style.bg(t.wash).text_color(t.ink_soft))
                .child(ink_icon(IconName::ArrowLeft, 18.0))
                .child("Back")
                .on_click(|_, _, cx| dispatch(AppCommand::SetSettingsOpen { open: false, section: None, setting_id: None }, cx)),
        )
        .child(
            div()
                .flex_none()
                .mx(px(10.0))
                .mt(px(if short { 14.0 } else { 26.0 }))
                .mb(px(if short { 12.0 } else { 22.0 }))
                .text_color(t.ink)
                .text_size(px(if short { 17.0 } else { 21.0 }))
                .font_weight(FontWeight(610.0))
                .letter_spacing(-0.03)
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Heading, "Settings"))
                .child("Settings"),
        )
        .child(div().flex_none().mb(px(10.0)).child(SearchField::new("settings-search", &search_input).on_clear(move |_, window, cx| clear_input.update(cx, |input, cx| {
            input.reset("", cx);
            input.focus(window);
        }))))
        .child(nav);

    root.settings.marks.title_line = title_line(m.text_content, window, cx);
    let marks = root.settings.marks.clone();
    let body = match current {
        SettingsSection::General => general::render(root, state, &marks, window, cx),
        SettingsSection::Appearance => appearance::render(root, state, &marks, window, cx),
        SettingsSection::Usage => usage::render(state, &marks, window, cx),
        SettingsSection::Engines => engines::render(state, &marks, window, cx),
        SettingsSection::Projects => projects::render(state, &marks, window, cx),
        SettingsSection::Worktrees => worktrees::render(state, &marks, window, cx),
        SettingsSection::Shortcuts => shortcuts::render(root, state, &marks, window, cx),
        SettingsSection::ComputerUse => computer_use::render(state, &marks, window, cx),
        SettingsSection::Browser => browser::render(root, state, &marks, window, cx),
        SettingsSection::Phone => devices::render(root, state, &marks, window, cx),
        SettingsSection::Archive => archive::render(root, state, &marks, window, cx),
    };
    let height = f32::from(window.viewport_size().height);
    let width = f32::from(window.viewport_size().width);
    let max_width = if current == SettingsSection::Worktrees { 1020.0 } else { 780.0 };
    let main = div()
        .id("settings-main")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Main, ""))
        .relative()
        .size_full()
        .overflow_y_scroll()
        .track_scroll(&root.settings.main_scroll)
        .rounded(px(m.page_radius))
        .when(t.page.a > 0.0, |main| main.bg(t.page))
        .pr(act_ui::components::scrollbar::gutter(&root.settings.main_scroll))
        .child(bounds_probe(&root.settings.main_bounds))
        .child(
            div()
                .w_full()
                .py(px(if short { 36.0 } else { (height * 0.1).clamp(64.0, 104.0) }))
                .px(px((width * 0.07).clamp(32.0, 96.0)))
                .child(div().w_full().max_w(px(max_width)).mx_auto().child(body)),
        );
    div()
        .id("settings-view")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Region, "Settings"))
        .occlude()
        .absolute()
        .inset_0()
        .flex()
        .when(cx.theme().environment.material == act_ui::theme::WindowMaterial::None, |view| view.bg(t.sidebar))
        .capture_any_mouse_down(cx.listener(|root, _, _, cx| {
            if root.settings.let_go() {
                cx.notify();
            }
        }))
        .capture_key_down(cx.listener(|root, event: &KeyDownEvent, _, cx| {
            let state = root.state(cx);
            if capture_key(&state, &event.keystroke, cx) {
                cx.stop_propagation();
                return;
            }
            if root.settings.let_go() {
                cx.notify();
            }
        }))
        .on_key_down(cx.listener(|root, event: &KeyDownEvent, window, cx| {
            let Some((input, _)) = &root.settings.search else { return };
            let input = input.clone();
            let on_page = root.settings.nav_focus.iter().position(|focus| focus.is_focused(window));
            let key = event.keystroke.key.as_str();
            if let Some(at) = on_page {
                let count = root.settings.nav_focus.len();
                match key {
                    "down" => window.focus(&root.settings.nav_focus[(at + 1) % count]),
                    "up" => window.focus(&root.settings.nav_focus[(at + count - 1) % count]),
                    _ => return,
                }
                // Left to bubble on, so the keystroke still reaches what shows focus rings.
                cx.notify();
                return;
            }
            if !input.read(cx).is_focused(window) {
                return;
            }
            let query = input.read(cx).text().trim().to_lowercase();
            let results = if query.is_empty() { Vec::new() } else { rank_settings_jumps(&query, settings_jump_options().len()) };
            let count = results.len();
            match event.keystroke.key.as_str() {
                "down" if count > 0 => {
                    root.settings.picked = (root.settings.picked + 1) % count;
                    root.settings.picked_moved = true;
                }
                "up" if count > 0 => {
                    root.settings.picked = (root.settings.picked + count - 1) % count;
                    root.settings.picked_moved = true;
                }
                "escape" if !input.read(cx).text().is_empty() => input.update(cx, |input, cx| input.reset("", cx)),
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
        }))
        .child(sidebar)
        .child(
            div().flex_1().min_w_0().h_full().p(px(m.frame_gap)).child(
                div().relative().size_full().child(main).child(act_ui::components::scrollbar::Scrollbar::vertical("settings-scrollbar", &root.settings.main_scroll)),
            ),
        )
        .children(devices::overlay(root, window, cx).map(|dialog| div().absolute().inset_0().child(dialog)))
}

/// One line cut short with an ellipsis, without wrapping: a wrapped clamp breaks on a title that
/// already holds a multi-byte character where the cut falls.
pub(super) trait Clip: Styled + Sized {
    fn clip(self) -> Self {
        self.whitespace_nowrap().overflow_hidden().text_ellipsis()
    }
}

impl<T: Styled + Sized> Clip for T {}
