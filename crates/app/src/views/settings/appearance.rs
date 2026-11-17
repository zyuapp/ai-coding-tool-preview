//! Appearance (AppearanceSettings.tsx): light or dark, the theme family as a grid of tiles each drawn
//! in its own colours, the two fonts, and the two text sizes.

use std::collections::HashMap;

use act_core::domain::appearance::{clamp_size, step_size, SizeRange, READING_SIZE, TERMINAL_SIZE};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::{Segmented, SegmentedOption};
use act_ui::components::field::{SearchField, SearchVariant};
use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
use act_ui::fonts::installed_fonts;
use act_ui::overlay::{bounds_cell, bounds_probe, placed, Align, BoundsCell, Placement};
use act_ui::style::{glass, GlassKind, PaneFill};
use act_ui::tooltip::Tippable;
use act_ui::theme::{palette_for, theme_families, theme_for, ActiveTheme, Pane, ThemeMode as UiMode, Tokens};
use act_ui::typography::{custom_font_family, custom_font_id, mono_font_or_default, ui_font_or_default, BundledFont, MONO_FONTS, UI_FONTS};
use act_ui::IconName;
use gpui::{deferred, div, prelude::*, px, relative, AnyElement, Context, FocusHandle, Focusable, FontWeight, KeyDownEvent, Rgba, SharedString, Window};

use super::{description, page, Held, Marks, Row, Section};
use crate::views::common::{icon, rimmed};
use crate::views::root::Root;
use crate::workspace::dispatch;

/// One size as `− 15 +`: the number is a field a size can be typed into, committed when it is left.
struct SizeField {
    held: Held,
    /// Escape gave the typing up, so leaving the field keeps the settled size.
    abandoned: bool,
    focused: bool,
}

struct ThemeMenu {
    search: Held,
    /// Holds the keyboard while the arrows walk the tiles.
    grid: FocusHandle,
    /// The tile the arrows reached, which Enter picks.
    active: Option<usize>,
    scroll: gpui::ScrollHandle,
}

#[derive(Default)]
pub struct AppearanceState {
    theme_probe: Option<BoundsCell>,
    ui_probe: Option<BoundsCell>,
    mono_probe: Option<BoundsCell>,
    theme_menu: Option<ThemeMenu>,
    reading: Option<SizeField>,
    terminal: Option<SizeField>,
    tile_tokens: HashMap<&'static str, Tokens>,
    trigger_focus: Option<FocusHandle>,
    mode_focus: Option<FocusHandle>,
    /// When the popover last closed, so the key that closed it does not press the trigger it lands on.
    closed_at: Option<std::time::Instant>,
}

fn probe(slot: &mut Option<BoundsCell>) -> BoundsCell {
    slot.get_or_insert_with(bounds_cell).clone()
}

/// The theme a tile stands for, drawn in its own tokens.
fn tile_tokens<'a>(cache: &'a mut HashMap<&'static str, Tokens>, id: &'static str) -> &'a Tokens {
    cache.entry(id).or_insert_with(|| Tokens::derive(&palette_for(id), &crate::theme_bridge::environment()))
}

/// Its accent and three ansi slots on its own canvas.
fn swatch(t: &Tokens) -> gpui::Div {
    let bar = |color: Rgba| div().w(px(5.0)).h(px(12.0)).rounded(px(2.0)).bg(color);
    div()
        .relative()
        .flex()
        .flex_none()
        .gap(px(2.0))
        .p(px(3.0))
        .rounded(px(5.0))
        .bg(t.canvas)
        .child(div().absolute().inset_0().rounded(px(5.0)).border_1().border_color(t.line_mid))
        .child(bar(t.accent))
        .child(bar(t.ansi[2]))
        .child(bar(t.ansi[4]))
        .child(bar(t.ansi[5]))
}

/// The window in miniature: a sidebar, a message, a shell line, and a diff row.
fn preview(t: &Tokens) -> gpui::Div {
    let bar = |height: f32, color: Rgba| div().h(px(height)).rounded(px(2.0)).bg(color);
    let sidebar = div()
        .w(relative(0.34))
        .flex()
        .flex_col()
        .gap(px(5.0))
        .px(px(7.0))
        .py(px(9.0))
        .border_r_1()
        .border_color(t.line_soft)
        .bg(t.sidebar)
        .child(bar(5.0, t.accent).w(relative(0.7)))
        .child(bar(5.0, t.quiet))
        .child(bar(5.0, t.quiet).w(relative(0.55)));
    let bubble = div().flex().flex_col().gap(px(4.0)).px(px(6.0)).py(px(5.0)).border_1().border_color(t.line).rounded(px(4.0)).bg(t.surface_raised).child(bar(4.0, t.ink_faint)).child(bar(4.0, t.muted_soft).w(relative(0.6)));
    let shell = div().flex().gap(px(4.0)).px(px(6.0)).py(px(5.0)).rounded(px(4.0)).bg(t.code_surface).children([t.ansi[2], t.ansi[4], t.ansi[3], t.ansi[5]].map(|color| bar(4.0, color).flex_1()));
    let diff_line = |surface: Rgba, edge: Rgba| div().h(px(6.0)).bg(surface).border_l_2().border_color(edge);
    let diff = div().flex().flex_col().gap(px(2.0)).child(diff_line(t.diff_add_surface, t.success)).child(diff_line(t.diff_del_surface, t.danger));
    div().flex().h(px(58.0)).bg(t.canvas).child(sidebar).child(div().flex_1().flex().flex_col().gap(px(6.0)).px(px(8.0)).py(px(9.0)).child(bubble).child(shell).child(diff))
}

const POPOVER_PAD: f32 = 10.0;
const TILE_MIN: f32 = 132.0;
const TILE_GAP: f32 = 9.0;

fn popover_width(window: &Window) -> f32 {
    (f32::from(window.viewport_size().width) - 330.0).min(460.0).max(200.0)
}

/// The grid's scrollbar takes `gutter` from its width once it overflows.
fn columns(window: &Window, gutter: f32) -> usize {
    let inner = popover_width(window) - 2.0 * POPOVER_PAD - 4.0 - gutter;
    (((inner + TILE_GAP) / (TILE_MIN + TILE_GAP)).floor() as usize).max(1)
}

fn matching_families(query: &str) -> Vec<&'static str> {
    let term = query.trim().to_lowercase();
    theme_families().into_iter().filter(|family| term.is_empty() || family.to_lowercase().contains(&term)).collect()
}

fn choose_theme(root: &mut Root, family: &str, window: &mut Window, cx: &mut Context<Root>) {
    close_theme_menu(root, window);
    let dark = crate::theme_bridge::system_dark(cx);
    dispatch(AppCommand::SetThemeFamily { family: family.to_string(), system_dark: dark }, cx);
}

fn close_theme_menu(root: &mut Root, window: &mut Window) {
    root.settings.appearance.theme_menu = None;
    root.settings.appearance.closed_at = Some(std::time::Instant::now());
    if let Some(focus) = &root.settings.appearance.trigger_focus {
        window.focus(focus);
    }
}

fn open_theme_menu(root: &mut Root, window: &mut Window, cx: &mut Context<Root>) {
    let held = Held::new(
        "",
        "Search themes",
        |root, input, event, cx| match event {
            act_ui::input::InputEvent::Changed => {
                if let Some(menu) = &mut root.settings.appearance.theme_menu {
                    menu.active = None;
                }
            }
            act_ui::input::InputEvent::Submit => {
                let query = input.read(cx).text().to_string();
                if let Some(family) = matching_families(&query).first().copied().filter(|_| !query.trim().is_empty()) {
                    let entity = cx.entity();
                    cx.defer(move |cx| {
                        let Some(window) = cx.active_window() else { return };
                        let _ = window.update(cx, |_, window, cx| entity.update(cx, |root, cx| choose_theme(root, family, window, cx)));
                    });
                }
            }
            _ => {}
        },
        cx,
    );
    root.pending_focus = Some(held.input.clone());
    let _ = window;
    root.settings.appearance.theme_menu = Some(ThemeMenu { search: held, grid: cx.focus_handle(), active: None, scroll: gpui::ScrollHandle::new() });
    cx.notify();
}

fn theme_select(root: &mut Root, current_id: &'static str, current_family: &'static str, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let probe = probe(&mut root.settings.appearance.theme_probe);
    let focus = root.settings.appearance.trigger_focus.get_or_insert_with(|| cx.focus_handle()).clone();
    let open = root.settings.appearance.theme_menu.is_some();
    let own = tile_tokens(&mut root.settings.appearance.tile_tokens, current_id).clone();
    let ringed = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
    let trigger = rimmed(div(), t, 9.0)
        .id("theme-select")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, format!("Theme: {current_family}")).expanded(open))
        .track_focus(&focus)
        .when(ringed, |trigger| trigger.child(div().absolute().inset_0().rounded(px(9.0)).border_2().border_color(t.focus_ring)))
        .min_w(px(190.0))
        .flex()
        .items_center()
        .gap(px(9.0))
        .px(px(11.0))
        .py(px(8.0))
        .border_1()
        .border_color(t.line_strong)
        .pane(Pane::sheened(t.control_soft))
        .text_size(px(14.0))
        .text_color(t.ink)
        .cursor_pointer()
        .hover(|style| style.border_color(t.line_bright).bg(t.control_hover))
        .child(swatch(&own))
        .child(div().flex_1().min_w_0().whitespace_nowrap().child(current_family))
        .child(div().flex_none().child(icon(IconName::ChevronDown, 14.0, t.quiet_strong)))
        .on_key_down(cx.listener(|root, event: &KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "down" | "up") && root.settings.appearance.theme_menu.is_none() {
                open_theme_menu(root, window, cx);
                cx.stop_propagation();
            }
        }))
        .on_click(cx.listener(move |root, event: &gpui::ClickEvent, window, cx| {
            let just_closed = root.settings.appearance.closed_at.is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(400));
            if event.is_keyboard() && just_closed {
                return;
            }
            if root.settings.appearance.theme_menu.is_some() {
                close_theme_menu(root, window);
                cx.notify();
            } else {
                open_theme_menu(root, window, cx);
            }
        }));
    // The popover hangs from the trigger's outer edge.
    let trigger = div().relative().child(trigger).child(bounds_probe(&probe));
    if !open {
        return trigger.into_any_element();
    }
    let menu = root.settings.appearance.theme_menu.as_ref().expect("checked above");
    let query = menu.search.text(cx);
    let active = menu.active;
    let input = menu.search.input.clone();
    let grid_focus = menu.grid.clone();
    let grid_scroll = menu.scroll.clone();
    let gutter = f32::from(act_ui::components::scrollbar::gutter(&grid_scroll));
    let families = matching_families(&query);
    let term = !query.trim().is_empty();
    let target = if term { families.first().copied() } else { None };
    let variant = theme.info.variant;
    let width = popover_width(window);
    let columns = columns(window, gutter);
    let grid_focused = grid_focus.is_focused(window);
    let mut tiles = Vec::new();
    for (index, family) in families.iter().copied().enumerate() {
        let option = theme_for(family, variant);
        let picked = option.id == current_id;
        let next = Some(family) == target;
        let keyed = active == Some(index);
        let tokens = tile_tokens(&mut root.settings.appearance.tile_tokens, option.id).clone();
        let border = if picked { t.accent } else if next || keyed { t.line_bright } else { t.line_strong };
        let mark: Option<AnyElement> = if next {
            Some(icon(IconName::CornerDownLeft, 14.0, tokens.muted).into_any_element())
        } else if picked {
            Some(icon(IconName::Check, 14.0, tokens.accent).into_any_element())
        } else {
            None
        };
        let mut tile = div()
            .id(SharedString::from(format!("theme-tile-{family}")))
            // The grid holds the keyboard and moves among the tiles with the arrows.
            .skip_tab_stop()
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::ListItem, family).selected(picked).active(keyed && grid_focused))
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(10.0))
            .border_1()
            .border_color(border)
            .bg(tokens.canvas)
            .cursor_pointer()
            .hover(|style| style.border_color(t.line_bright))
            .child(preview(&tokens))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .px(px(9.0))
                    .py(px(7.0))
                    .border_t_1()
                    .border_color(tokens.line_soft)
                    .bg(tokens.sidebar)
                    .text_color(tokens.ink)
                    .text_size(px(12.0))
                    .font_weight(FontWeight(560.0))
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(family))
                    .children(mark),
            )
            .on_click(cx.listener(move |root, _, window, cx| choose_theme(root, family, window, cx)));
        if picked || next || keyed {
            let ring = if picked { t.accent } else if keyed { t.focus_ring } else { t.line_bright };
            tile = tile.child(div().absolute().inset_0().rounded(px(10.0)).border_1().border_color(ring));
        }
        tiles.push(tile.into_any_element());
    }
    let tile_width = {
        let inner = width - 2.0 * POPOVER_PAD - 4.0 - gutter;
        ((inner - TILE_GAP * (columns as f32 - 1.0)) / columns as f32).floor() - 0.5
    };
    let grid = div()
        .id("theme-choices")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::List, "Theme"))
        .max_h(px(268.0))
        .overflow_y_scroll()
        .track_scroll(&grid_scroll)
        .p(px(2.0))
        .pb(px(4.0))
        .pr(px(2.0 + gutter))
        .flex()
        .flex_wrap()
        .gap(px(TILE_GAP))
        .children(tiles.into_iter().map(|tile| div().w(px(tile_width)).child(tile)));
    let grid = div().relative().child(grid).child(act_ui::components::scrollbar::Scrollbar::vertical("theme-choices-scrollbar", &grid_scroll));
    let surface = glass(div(), t, GlassKind::Popover, None, None)
        .id("theme-select-popover")
        .map(act_ui::style::glass_in)
        .track_focus(&grid_focus)
        .w(px(width))
        .p(px(POPOVER_PAD))
        .flex()
        .flex_col()
        .gap(px(9.0))
        .occlude()
        .capture_key_down(cx.listener(move |root, event: &KeyDownEvent, window, cx| {
            let Some(menu) = &mut root.settings.appearance.theme_menu else { return };
            let query = menu.search.text(cx);
            let families = matching_families(&query);
            let count = families.len();
            let term = !query.trim().is_empty();
            match event.keystroke.key.as_str() {
                "escape" => {
                    let input = menu.search.input.clone();
                    if input.read(cx).is_focused(window) && !query.is_empty() {
                        input.update(cx, |input, cx| input.reset("", cx));
                    } else {
                        close_theme_menu(root, window);
                    }
                }
                "enter" => {
                    let chosen = match menu.active {
                        Some(index) => families.get(index).copied(),
                        None if term => families.first().copied(),
                        None => None,
                    };
                    match chosen {
                        Some(family) => choose_theme(root, family, window, cx),
                        None => return,
                    }
                }
                key @ ("down" | "up" | "left" | "right") if count > 0 => match menu.active {
                    None if key == "down" => {
                        let chosen = if term { None } else { families.iter().position(|family| theme_for(family, variant).id == current_id) };
                        menu.active = Some(chosen.unwrap_or(0));
                        window.focus(&menu.grid);
                    }
                    None => return,
                    Some(at) => {
                        let step: i64 = match key {
                            "left" => -1,
                            "right" => 1,
                            "up" => -(columns as i64),
                            _ => columns as i64,
                        };
                        let next = at as i64 + step;
                        if next < 0 {
                            menu.active = None;
                            menu.search.input.read(cx).focus(window);
                        } else {
                            menu.active = Some((next as usize).min(count - 1));
                        }
                    }
                },
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
        }))
        .child(
            // `.menu-search` reaches out over the pane's padding.
            div()
                .flex_none()
                .mx(px(-POPOVER_PAD))
                .mt(px(-POPOVER_PAD))
                .rounded_t(px(9.0))
                .px(px(6.0))
                .pt(px(6.0))
                .pb(px(4.0))
                .bg(t.surface_glass)
                .child(SearchField::new("theme-search", &input).variant(SearchVariant::Menu)),
        )
        .child(grid)
        .when(families.is_empty(), |surface: gpui::Stateful<gpui::Div>| surface.child(div().px(px(8.0)).py(px(6.0)).text_color(t.quiet_strong).text_size(px(12.0)).child("No theme here is called that.")));
    let anchor = probe.get().unwrap_or_default();
    let scrim = div().id("theme-select-scrim").absolute().top(px(-4000.0)).left(px(-4000.0)).w(px(12000.0)).h(px(12000.0)).occlude().on_mouse_down(gpui::MouseButton::Left, cx.listener(|root, _, window, cx| {
        close_theme_menu(root, window);
        cx.notify();
    }));
    trigger
        .child(deferred(div().child(scrim)).with_priority(1))
        .child(deferred(placed(Placement::Below { anchor, align: Align::End, gap: px(7.0) }, surface)).with_priority(2))
        .into_any_element()
}

fn face(fonts: &[BundledFont], id: &str, system: SharedString) -> SharedString {
    if let Some(family) = custom_font_family(id) {
        return family.to_string().into();
    }
    fonts.iter().find(|font| font.id == id).and_then(|font| font.family).map(SharedString::from).unwrap_or(system)
}

fn font_entries(mono: bool, current: &str, query: &str, installed: &[String], system: SharedString) -> Vec<MenuEntry> {
    let bundled = if mono { MONO_FONTS } else { UI_FONTS };
    let term = query.trim().to_lowercase();
    let command = move |id: String| {
        move |_: &mut Window, cx: &mut gpui::App| {
            dispatch(if mono { AppCommand::SetMonoFont { font: id.clone() } } else { AppCommand::SetUiFont { font: id.clone() } }, cx);
        }
    };
    let families: Vec<&BundledFont> = bundled.iter().filter(|font| term.is_empty() || font.label.to_lowercase().contains(&term)).collect();
    let extras: Vec<&String> = installed.iter().filter(|family| (term.is_empty() || family.to_lowercase().contains(&term)) && !bundled.iter().any(|font| font.label == family.as_str())).collect();
    let shown: Vec<&String> = extras.iter().take(100).copied().collect();
    let first = if term.is_empty() { None } else { families.first().map(|font| font.id.to_string()).or_else(|| shown.first().map(|family| custom_font_id(family))) };
    let option = |id: String, name: String| {
        let mut item = MenuItem::new(name).checked(id == current).font_family(face(bundled, &id, system.clone())).on_select(command(id.clone()));
        if first.as_deref() == Some(id.as_str()) {
            item = item.shortcut("↵");
        }
        item
    };
    let mut entries: Vec<MenuEntry> = families.iter().map(|font| option(font.id.to_string(), font.label.to_string()).into()).collect();
    if !shown.is_empty() {
        entries.push(MenuEntry::Group { label: "On this computer".into(), items: shown.iter().map(|family| option(custom_font_id(family), family.to_string())).collect() });
    }
    if extras.len() > shown.len() {
        entries.push(MenuEntry::Note { text: format!("Showing the first 100 of {}. Narrow the search to see the rest.", extras.len()).into(), danger: false });
    }
    if families.is_empty() && shown.is_empty() {
        entries.push(MenuEntry::Note { text: "No font here is called that.".into(), danger: false });
    }
    entries
}

fn font_select(root: &mut Root, mono: bool, chosen: &str, focus: &gpui::FocusHandle, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let ringed = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let probe = probe(if mono { &mut root.settings.appearance.mono_probe } else { &mut root.settings.appearance.ui_probe });
    let current = if mono { mono_font_or_default(Some(chosen)) } else { ui_font_or_default(Some(chosen)) };
    let system: SharedString = if mono { theme.typography.mono_family.clone() } else { act_ui::typography::SYSTEM_UI_FAMILY.into() };
    let system = if mono && theme.typography.mono_font.id.as_ref() != "system" { act_ui::fonts::system_mono_family(cx).into() } else { system };
    let bundled = if mono { MONO_FONTS } else { UI_FONTS };
    let family = face(bundled, &current.id, system.clone());
    let id = if mono { "mono-font" } else { "ui-font" };
    let menu_id = if mono { "settings:mono-font" } else { "settings:ui-font" };
    let label = if mono { "Code and terminal font" } else { "Interface font" };
    let current_id = current.id.to_string();
    let open = root.menus.is_open(menu_id);
    rimmed(div(), t, 8.0)
        .id(id)
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, format!("{label}: {}", current.label)).expanded(open))
        .track_focus(focus)
        .relative()
        .when(ringed, |trigger| trigger.child(div().absolute().inset_0().rounded(px(8.0)).border_2().border_color(t.focus_ring)))
        .min_w(px(176.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
        .px(px(11.0))
        .py(px(7.0))
        .border_1()
        .border_color(t.line_strong)
        .pane(Pane::sheened(t.control))
        .text_size(px(14.0))
        .text_color(t.ink_dim)
        .font_family(family)
        .cursor_pointer()
        .hover(|style| style.border_color(t.line_bright).bg(t.control_hover))
        .child(current.label.clone())
        .child(icon(IconName::ChevronDown, 14.0, t.quiet_strong))
        .child(bounds_probe(&probe))
        .on_click(cx.listener(move |root, _, window, cx| {
            if root.menus.is_open(menu_id) {
                return;
            }
            let anchor = probe.get().unwrap_or_default();
            let installed = std::rc::Rc::new(installed_fonts(cx));
            let current = current_id.clone();
            let system = system.clone();
            // `.font-select-popover` hangs 6px under its trigger, right-aligned, and never flips.
            let menu = Menu::build(Vec::new(), Placement::Below { anchor, align: Align::End, gap: px(6.0) }, window, cx, |menu| {
                menu.checkable(true).pinned_notes(true).default_needs_query(true).list_max_height(px(264.0)).width(px(300.0)).exempt(&probe).searchable("Search fonts", move |query, _| font_entries(mono, &current, query, &installed, system.clone())).search_label(format!("Search fonts for {}", label.to_lowercase()))
            });
            root.menus.show(menu_id, menu, cx);
        }))
        .into_any_element()
}

fn size_field(root: &mut Root, terminal: bool, value: f64, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let range: SizeRange = if terminal { TERMINAL_SIZE } else { READING_SIZE };
    let choose = move |size: f64, cx: &mut gpui::App| dispatch(if terminal { AppCommand::SetTerminalSize { size } } else { AppCommand::SetReadingSize { size } }, cx);
    let slot = if terminal { &mut root.settings.appearance.terminal } else { &mut root.settings.appearance.reading };
    if slot.is_none() {
        let held = Held::new(
            &format!("{value}"),
            "",
            move |root, input, event, cx| {
                let state = root.state(cx);
                let value = if terminal { state.terminal_size } else { state.reading_size };
                let field = if terminal { &mut root.settings.appearance.terminal } else { &mut root.settings.appearance.reading };
                let Some(field) = field.as_mut() else { return };
                match event {
                    act_ui::input::InputEvent::Focus => {
                        field.focused = true;
                        input.update(cx, |input, cx| input.select_all_text(cx));
                    }
                    act_ui::input::InputEvent::Blur => {
                        field.focused = false;
                        let typed = input.read(cx).text().trim().to_string();
                        let keep = !field.abandoned && !typed.is_empty();
                        field.abandoned = false;
                        let parsed = typed.parse::<f64>().ok().filter(|number| number.is_finite());
                        let text = format!("{value}");
                        input.update(cx, |input, cx| input.reset(&text, cx));
                        if let (true, Some(number)) = (keep, parsed) {
                            choose(clamp_size(&range, number), cx);
                        }
                    }
                    act_ui::input::InputEvent::Submit => {
                        let typed = input.read(cx).text().trim().parse::<f64>().ok().filter(|number| number.is_finite());
                        if let Some(number) = typed {
                            let size = clamp_size(&range, number);
                            input.update(cx, |input, cx| input.reset(&format!("{size}"), cx));
                            choose(size, cx);
                        }
                    }
                    _ => {}
                }
            },
            cx,
        );
        *slot = Some(SizeField { held, abandoned: false, focused: false });
    }
    let field = slot.as_ref().expect("made above");
    let input = field.held.input.clone();
    if !field.focused && input.read(cx).text() != format!("{value}") {
        let text = format!("{value}");
        input.update(cx, |input, cx| input.reset(&text, cx));
    }
    let stepper = |id: &'static str, glyph: IconName, label: &'static str, disabled: bool, delta: i8| {
        div()
            .id(id)
            .size(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .transition(act_ui::style::touch().only(&[gpui::TransitionProperty::Background, gpui::TransitionProperty::Transform]))
            .when(disabled, |key| key.text_color(t.quiet).opacity(t.disabled_opacity))
            .when(!disabled, |key| {
                key.bg(t.control)
                    .text_color(t.ink_dim)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.control_hover).text_color(t.ink))
                    .active(|style| style.transform(gpui::ElementTransform::scale(0.92)))
                    .on_click(move |_, _, cx| choose(step_size(&range, value, delta), cx))
            })
            .child(act_ui::Icon::new(glyph).size(px(14.0)))
            .tip(label)
    };
    let (down_id, up_id, down_label, up_label) = if terminal { ("terminal-size-down", "terminal-size-up", "Smaller terminal text size", "Larger terminal text size") } else { ("reading-size-down", "reading-size-up", "Smaller conversation text size", "Larger conversation text size") };
    let key_input = input.clone();
    // Sized to what it holds, as `field-sizing: content` does.
    let digits = input.read(cx).text().chars().count().max(1) as f32;
    let ringed = input.read(cx).focus_handle(cx).is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
    rimmed(div(), &t, 999.0)
        .relative()
        .when(ringed, |stepper| stepper.child(div().absolute().inset_0().rounded_full().border_2().border_color(t.focus_ring)))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.0))
        .p(px(3.0))
        .pane(Pane::sheened(t.control_soft))
        .child(stepper(down_id, IconName::Minus, down_label, value <= range.min, -1))
        .child(
            div()
                .id(if terminal { "terminal-size-value" } else { "reading-size-value" })
                .min_w(px(58.0))
                .px(px(4.0))
                .flex()
                .items_baseline()
                .justify_center()
                .cursor_text()
                .skip_tab_stop()
                .on_click(move |_, window, cx| key_input.read(cx).focus(window))
                .capture_key_down(cx.listener(move |root, event: &KeyDownEvent, window, cx| {
                    let field = if terminal { &mut root.settings.appearance.terminal } else { &mut root.settings.appearance.reading };
                    let Some(field) = field.as_mut() else { return };
                    match event.keystroke.key.as_str() {
                        key @ ("up" | "down") => {
                            let typed = field.held.text(cx).trim().parse::<f64>().ok().filter(|number| number.is_finite());
                            let from = typed.map(|number| clamp_size(&range, number)).unwrap_or(value);
                            let next = step_size(&range, from, if key == "up" { 1 } else { -1 });
                            field.held.input.update(cx, |input, cx| {
                                input.reset(&format!("{next}"), cx);
                                input.select_all_text(cx);
                            });
                            choose(next, cx);
                        }
                        "escape" => {
                            field.abandoned = true;
                            window.focus(&root.focus);
                        }
                        "enter" => window.focus(&root.focus),
                        _ => return,
                    }
                    cx.stop_propagation();
                }))
                .child(div().w(px(digits * 7.6 + 1.0)).text_color(t.ink).text_size(px(13.0)).font_weight(FontWeight(600.0)).map(act_ui::style::tabular_nums).child(input))
                .child(div().pl(px(3.0)).text_color(t.quiet_strong).text_size(px(12.0)).child("px")),
        )
        .child(stepper(up_id, IconName::Plus, up_label, value >= range.max, 1))
        .into_any_element()
}

pub fn render(root: &mut Root, state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let mode = match theme.mode {
        UiMode::Light => "light",
        UiMode::Dark => "dark",
        UiMode::Auto => "auto",
    };
    let modes = vec![SegmentedOption::new("light", "Light").icon(IconName::Sun).icon_size(16.0), SegmentedOption::new("dark", "Dark").icon(IconName::Moon).icon_size(16.0), SegmentedOption::new("auto", "Auto").icon(IconName::SunMoon).icon_size(16.0)];
    let mode_focus = root.settings.appearance.mode_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
    let mode_control = Segmented::new("theme-mode", modes, mode).track_focus(&mode_focus).on_change(|value, _, cx| {
        let Ok(mode) = serde_json::from_value(serde_json::json!(value.to_string())) else { return };
        let dark = crate::theme_bridge::system_dark(cx);
        dispatch(AppCommand::SetThemeMode { mode, system_dark: dark }, cx);
    });
    let current_id = theme.info.id;
    let current_family = theme.info.family;
    if root.settings.appearance.trigger_focus.is_none() {
        root.settings.appearance.trigger_focus = Some(marks.stop("appearance.theme", cx));
    }
    let select = theme_select(root, current_id, current_family, window, cx);
    let theme_section = Section::new("Theme")
        .first()
        .description(description("Each theme comes in light and dark. Auto follows the system."))
        .action(mode_control)
        .row(Row::setting("appearance.theme", t).line(format!("{} themes, each drawn in its own colours.", act_ui::theme::THEMES.len()), t).action(select).build(marks, t));

    let ui = font_select(root, false, &state.ui_font, &marks.stop("appearance.ui-font", cx), window, cx);
    let mono = font_select(root, true, &state.mono_font, &marks.stop("appearance.mono-font", cx), window, cx);
    let fonts = Section::new("Fonts")
        .description(description(concat!("The families ", act_core::app_name!(), " ships, or any font installed on this computer.")))
        .row(Row::setting("appearance.ui-font", t).line("The window itself: its threads, its menus, and what the agent writes back.", t).action(ui).build(marks, t))
        .row(Row::setting("appearance.mono-font", t).line("Code, diffs, and every shell.", t).action(mono).build(marks, t));

    let reading = size_field(root, false, state.reading_size, window, cx);
    let terminal = size_field(root, true, state.terminal_size, window, cx);
    let sample = |text: &'static str, size: f64, mono: bool| {
        div()
            .mt(px(9.0))
            .text_size(px(size as f32))
            .line_height(relative(1.5))
            .text_color(if mono { t.code_ink } else { t.ink_soft })
            .when(mono, |line| line.font_family(theme.typography.mono_family.clone()))
            .child(text)
    };
    let sizes = Section::new("Text size")
        .description(description("The sidebar, the tabs, and the menus keep the size they were drawn at."))
        .row(Row::setting("appearance.reading-size", t).describe(sample("Ran the tests. Three failed in the parser.", state.reading_size, false)).action(reading).build(marks, t))
        .row(Row::setting("appearance.terminal-size", t).describe(sample("$ git status", state.terminal_size, true)).action(terminal).build(marks, t));
    page("Appearance", Some(description("The window's colours, fonts, and text sizes. Every choice applies the moment you make it.")), None, window, cx)
        .child(theme_section.build(marks, t))
        .child(fonts.build(marks, t))
        .child(sizes.build(marks, t))
        .into_any_element()
}
