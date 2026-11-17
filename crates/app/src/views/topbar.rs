//! The bar over the conversation: the sidebar toggle, the thread's name and menu, and the panel
//! toggles (WorkspaceHeader.tsx, OpenInMenu.tsx).

use act_core::domain::project::project_name;
use act_core::state::WorkspaceState;
use act_core::workspace::location::project_for;
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize};
use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
use act_ui::overlay::{bounds_cell, bounds_probe, Align, BoundsCell, Placement};
use act_ui::style::{Corners, Raised};
use act_ui::style::Backdrop;
use act_ui::theme::ActiveTheme;
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{div, prelude::*, px, Context, FontWeight, SharedString, Window};

use super::chrome::{drag_layer, key_tip, no_drag, no_drag_shrink};
use super::common::{icon, rimmed, Ellipsis};
use super::letter_spacing::LetterSpacing;
use act_core::domain::platform::ComputerStatus;
use super::root::Root;
use super::sidebar_row::thread_menu_entries;
use crate::workspace::dispatch;

/// The heading's `letter-spacing`, in em.
const HEADING_TRACKING: f32 = -0.018;

pub struct TopbarState {
    pub menu_probe: BoundsCell,
    pub open_in_probe: BoundsCell,
    pub bar_probe: BoundsCell,
    /// How many applications the open "Open in" menu lists; None before the scan answers.
    pub open_in_shown: Option<Option<usize>>,
}

impl Default for TopbarState {
    fn default() -> Self {
        TopbarState { menu_probe: bounds_cell(), open_in_probe: bounds_cell(), bar_probe: bounds_cell(), open_in_shown: None }
    }
}

/// A round key of glass; pressed, a clear pane lifted off the bar.
fn control(id: &'static str, glyph: IconName, label: impl Into<SharedString>, tip: SharedString, pressed: bool, toggle: bool, cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    let t = cx.theme().tokens.clone();
    let mut access = gpui::AccessInfo::new(gpui::AccessRole::Button, label);
    if toggle {
        access = access.toggled(pressed);
    }
    let base = div()
        .id(id)
        .accessible(access)
        .relative()
        .size(px(34.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .transition(act_ui::style::touch())
        .text_color(if pressed { t.ink } else { t.quiet_strong })
        .child(act_ui::Icon::new(glyph).size(px(18.0)))
        .tip(tip);
    if pressed {
        lifted(base, &t)
    } else {
        base.hover(|style| style.bg(t.surface_hover).text_color(t.ink_dim))
    }
}

/// A pressed key: a clear pane lifted off the bar, edged in light inside a dark hairline.
fn lifted(key: gpui::Stateful<gpui::Div>, t: &act_ui::theme::Tokens) -> gpui::Stateful<gpui::Div> {
    let outline = gpui::BoxShadow { color: t.glass_float_outline.into(), offset: gpui::point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(0.5) };
    let drop = gpui::BoxShadow { color: act_ui::color::alpha(gpui::black().into(), 0.12).into(), offset: gpui::point(px(0.0), px(1.0)), blur_radius: px(2.0), spread_radius: px(0.0) };
    key.border_1().border_color(t.glass_edge).bg(t.glass_clear.background().tiled_from_padding_box()).glass_backdrop(t).shadow(vec![outline, drop]).rim(t.glass_rim, Corners::Full)
}

pub fn render(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> gpui::AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    if state.zen {
        return zen_titlebar(state, cx).into_any_element();
    }
    let current = state.current_thread();
    let project = match current {
        Some(thread) => project_for(state, Some(thread)),
        None => state.draft_project_id.as_ref().and_then(|id| state.project(id)),
    };
    let folder = project.map(|project| project.root.clone()).unwrap_or_default();
    let folder_label = project.map(project_name).unwrap_or_default();
    let sidebar_open = state.sidebar_open;
    let dock_open = act_core::state::front_dock(state).1.open;
    let session_open = state.session_panel_open && !dock_open;
    let floats = super::sidebar::floats(state, window);
    let inset = !sidebar_open || floats;
    let narrow = window.viewport_size().width <= px(620.0);
    let pad_left = if inset || narrow { 22.0f32.max(m.window_controls_inset) } else { 22.0 };
    let pad_right = if narrow { 16.0 } else { 22.0 };
    // The bar's container queries read its content box.
    let width = root.topbar.bar_probe.get().map(|bounds| f32::from(bounds.size.width) - pad_left - pad_right).unwrap_or(f32::MAX);
    // The width is last frame's, so a frame that changes it draws the bar again.
    let bar_probe = root.topbar.bar_probe.clone();
    let drawn = root.topbar.bar_probe.get().map(|bounds| bounds.size.width);
    cx.on_next_frame(window, move |_, _, cx| {
        if bar_probe.get().map(|bounds| bounds.size.width) != drawn {
            cx.notify();
        }
    });
    let host = state.computers.active.as_ref().and_then(|id| state.computers.paired.iter().find(|computer| &computer.link.id == id));

    let mut heading = div().min_w_0().flex().items_center().gap(px(6.0));
    let heading_name: SharedString = match current {
        Some(thread) => thread.title.clone().into(),
        None if folder.is_empty() => "New chat".into(),
        None => "New thread".into(),
    };
    let mut title_line = div().accessible(gpui::AccessInfo::new(gpui::AccessRole::Heading, heading_name)).min_w_0().max_w(px(720.0f32.min(0.58 * f32::from(window.viewport_size().width)))).flex().items_center().gap(px(6.0)).text_size(px(m.text_heading)).font_weight(FontWeight(590.0)).letter_spacing(HEADING_TRACKING).text_color(t.ink_soft).whitespace_nowrap();
    if let Some(host) = host {
        let offline = host.link.status != ComputerStatus::Connected;
        let reconnecting = host.link.status == ComputerStatus::Connecting;
        title_line = title_line.child(
            div()
                .flex_shrink()
                .min_w(px(40.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_color(t.muted)
                .font_weight(FontWeight(500.0))
                .child(div().flex().when(offline, |mark| mark.accessible(gpui::AccessInfo::new(gpui::AccessRole::Image, if reconnecting { "Reconnecting" } else { "Offline" }))).child(icon(if offline { IconName::MonitorOff } else { IconName::Monitor }, 18.0, if offline { t.warning } else { t.muted })))
                .child(div().min_w_0().ellipsis().child(host.link.name.clone())),
        );
        if offline {
            if reconnecting {
                title_line = title_line.child(div().flex_none().h(px(28.0)).letter_spacing(0.0).flex().items_center().px(px(8.0)).text_color(t.muted).text_size(px(m.text_meta)).font_weight(FontWeight(560.0)).child("Reconnecting…"));
            } else {
                let id = host.link.id.clone();
                title_line = title_line.child(no_drag(
                    Button::new("heading-reconnect", "Reconnect")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Sm)
                        .tip(format!("Reconnect to {}", host.link.name))
                        .on_click(move |_, _, cx| dispatch(AppCommand::ComputersReconnect { id: Some(id.clone()) }, cx)),
                ));
            }
        }
        title_line = title_line.child(div().flex_none().mr(px(2.0)).text_color(t.quiet).font_weight(FontWeight(400.0)).child("/"));
    }
    if !folder.is_empty() && width > 480.0 {
        let tip = match host {
            Some(host) => format!("{folder} on {}", host.link.name),
            None => folder.clone(),
        };
        title_line = title_line
            .child(div().id("heading-project").accessible(gpui::AccessInfo::new(gpui::AccessRole::Label, folder_label.clone())).flex_shrink().min_w(px(heading_width(&folder_label, 500.0, true, m.text_heading, window).min(heading_width("0", 500.0, false, m.text_heading, window) * 6.0))).ellipsis().text_color(t.muted).font_weight(FontWeight(500.0)).child(folder_label.clone()).tip(tip))
            .child(div().flex_none().mr(px(2.0)).text_color(t.quiet).font_weight(FontWeight(400.0)).child("/"));
    }
    match current {
        Some(thread) if root.renaming_heading.as_deref() == Some(thread.id.as_str()) => {
            // Sized to its text, as `field-sizing: content` does, from 12ch up to the line's width.
            let text = root.rename_input.as_ref().map(|(input, _)| input.read(cx).text().to_string()).unwrap_or_default();
            let fits = heading_width(&text, 590.0, false, m.text_heading, window) + 14.0;
            let least = heading_width("0", 590.0, false, m.text_heading, window) * 12.0;
            title_line = title_line.child(no_drag_shrink(div().flex().w(px(fits.max(least))).max_w_full().child(root.heading_rename_field(cx))));
        }
        Some(thread) => {
            let id = thread.id.clone();
            title_line = title_line.child(no_drag_shrink(
                div()
                    .id("heading-thread")
                    .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, thread.title.clone()))
                    .group("heading-thread")
                    .flex_shrink()
                    .min_w(px(heading_width("0", 590.0, false, m.text_heading, window) * 4.0))
                    .h(px(28.0))
                    .px(px(5.0))
                    .mx(px(-5.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .cursor_text()
                    .transition(gpui::Transition::ease(120).only(&[gpui::TransitionProperty::Background, gpui::TransitionProperty::Color]))
                    .hover(|style| style.bg(t.surface_hover))
                    .child(div().id("heading-thread-title").min_w_0().ellipsis().child(thread.title.clone()).tip_overflow(thread.title.clone()))
                    .when(width > 480.0, |title| title.child(div().id("heading-thread-pencil").flex_none().transition(gpui::Transition::ease(120)).text_color(t.quiet_soft).group_hover("heading-thread", |style| style.text_color(t.muted_strong)).child(act_ui::Icon::new(IconName::Pencil).size(px(14.0))).tip("Rename")))
                    .on_click(cx.listener(move |root, _, _, cx| {
                        root.renaming_heading = Some(id.clone());
                        root.start_rename(&id, cx);
                    })),
            ));
        }
        None => {
            title_line = title_line.child(div().child(if folder.is_empty() { "New chat" } else { "New thread" }));
        }
    }
    heading = heading.child(title_line);
    if let Some(thread) = current {
        let probe = root.topbar.menu_probe.clone();
        let id = thread.id.clone();
        let right_id = id.clone();
        let open = root.menus.is_open(&format!("thread-heading:{id}"));
        heading = heading.child(no_drag(
            div()
                .id("thread-heading-menu")
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, "Thread actions").expanded(open))
                .relative()
                .flex_none()
                .size(px(28.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.0))
                .cursor_pointer()
                .when(open, |button| button.bg(t.surface_hover))
                .text_color(if open { t.muted_strong } else { t.muted })
                .hover(|style| style.bg(t.surface_hover).text_color(t.muted_strong))
                .focus_visible(|style| style.bg(t.surface_hover).text_color(t.muted_strong))
                .child(div().relative().top(px(1.0)).child(act_ui::Icon::new(IconName::Ellipsis).size(px(18.0))))
                .child(bounds_probe(&probe))
                .tip("Thread actions")
                .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |root, _, window, cx| {
                    // A right-click opens the same menu, and never closes it.
                    if !root.menus.is_open(&format!("thread-heading:{right_id}")) {
                        open_heading_menu(root, &right_id, window, cx);
                    }
                }))
                .on_click(cx.listener(move |root, _, window, cx| {
                    if root.menus.is_open(&format!("thread-heading:{id}")) {
                        root.menus.close(cx);
                    } else {
                        open_heading_menu(root, &id, window, cx);
                    }
                })),
        ));
    }
    let mut heading_block = div().min_w_0().flex().items_center().gap(px(14.0)).child(heading);
    if folder.is_empty() {
        // The label gives way before the icon: it shortens with an ellipsis, and a narrow bar keeps the icon alone.
        let compact = width <= 620.0;
        let choose = rimmed(div(), t, 8.0)
            .id("choose-folder")
            .min_w_0()
            .flex_shrink()
            .max_w(px(720.0))
            .h(px(28.0))
            .border_1()
            .border_color(gpui::transparent_black())
            .when(compact, |button| button.w(px(28.0)).flex_none())
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .px(px(if compact { 0.0 } else { 10.0 }))
            .bg(t.control)
            .hover(|style| style.bg(t.control_hover).text_color(t.ink))
            .text_color(t.ink_dim)
            .text_size(px(m.text_meta))
            .line_height(gpui::relative(1.2))
            .font_weight(FontWeight(520.0))
            .cursor_pointer()
            .child(div().flex_none().child(act_ui::Icon::new(IconName::FolderOpen).size(px(14.0))))
            .when(!compact, |button| button.child(div().min_w_0().ellipsis().child("Choose a project folder")))
            .tip("Choose a project folder")
            .on_click(|_, _, cx| dispatch(AppCommand::ProjectOpen { start: false }, cx));
        heading_block = heading_block.child(no_drag_shrink(choose));
    }

    let left = div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(no_drag(
            IconButton::new("toggle-sidebar", IconName::PanelLeft, if sidebar_open { "Hide sidebar" } else { "Show sidebar" })
                .size(IconButtonSize::Lg)
                .pressed(sidebar_open)
                .tip(key_tip("Sidebar", "Mod+B"))
                .on_click(move |_, _, cx| dispatch(AppCommand::SetSidebarOpen { open: !sidebar_open }, cx)),
        ))
        .child(heading_block);

    let working = state.current_id.as_ref().and_then(|id| state.subagents.get(id)).map(|subagents| subagents.iter().filter(|subagent| subagent.status == act_core::domain::run::SubagentStatus::Working).count()).unwrap_or(0);
    let zen_available = current.is_some_and(|thread| !thread.messages.is_empty());
    let mut controls = div().flex().items_center().gap(px(4.0));
    if !folder.is_empty() && width > 400.0 {
        controls = controls.child(no_drag(open_in(root, state, cx)));
    }
    if !folder.is_empty() && zen_available && width > 400.0 {
        controls = controls.child(no_drag(control("zen-toggle", IconName::Focus, "Enter zen mode", zen_tip(state, "Zen mode"), false, false, cx).on_click(|_, _, cx| dispatch(AppCommand::SetZen { zen: true }, cx))));
    }
    let session_tip: SharedString = if working > 0 { format!("Session summary · {working} {} working", if working == 1 { "agent" } else { "agents" }).into() } else { "Session summary".into() };
    let session_label = format!("{} session summary{}", if session_open { "Hide" } else { "Show" }, if working > 0 { format!(", {working} working") } else { String::new() });
    let mut session = control("session-summary", IconName::ClipboardList, session_label, session_tip, session_open, true, cx);
    if working > 0 {
        session = session.w_auto().gap(px(5.0)).pl(px(9.0)).pr(px(12.0)).child(div().text_color(t.info).text_size(px(m.text_meta)).font_weight(FontWeight(650.0)).line_height(px(m.text_meta)).map(act_ui::style::tabular_nums).child(working.to_string()));
    }
    controls = controls
        .child(no_drag(session.on_click(move |_, _, cx| {
            dispatch(AppCommand::SetDockOpen { open: false }, cx);
            dispatch(AppCommand::SetSessionPanelOpen { open: !session_open }, cx);
        })))
        .child(no_drag(control("right-panel", IconName::PanelRight, if dock_open { "Hide right panel" } else { "Show right panel" }, key_tip("Right panel", "Mod+\\"), dock_open, true, cx).on_click(move |_, _, cx| dispatch(AppCommand::SetDockOpen { open: !dock_open }, cx))));

    div()
        .id("topbar")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::SectionHeader, ""))
        .relative()
        .flex_none()
        .h(px(m.chrome_head))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(if narrow { 8.0 } else { 24.0 }))
        .pt(px(12.0))
        .pb(px(10.0))
        .pr(px(pad_right))
        .pl(px(pad_left))
        .child(bounds_probe(&root.topbar.bar_probe))
        .child(drag_layer("topbar-drag"))
        .child(left)
        .child(controls)
        .into_any_element()
}

/// How wide `text` draws at `weight`, `tracked` by the heading's letter-spacing.
fn heading_width(text: &str, weight: f32, tracked: bool, size: f32, window: &Window) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let style = window.text_style();
    let mut font = style.font();
    font.weight = FontWeight(weight);
    if tracked {
        font.features = gpui::FontFeatures::tracking(HEADING_TRACKING);
    }
    let run = gpui::TextRun { len: text.len(), font, color: style.color, background_color: None, underline: None, strikethrough: None };
    f32::from(window.text_system().shape_line(SharedString::from(text.to_string()), px(size), &[run], None).width)
}

/// What zen mode leaves of the topbar: a strip to drag the window by, and the way back out.
fn zen_titlebar(state: &WorkspaceState, cx: &mut Context<Root>) -> impl IntoElement {
    let m = cx.theme().metrics.clone();
    div()
        .id("zen-titlebar")
        .relative()
        .flex_none()
        .h(px(m.chrome_head))
        .flex()
        .items_center()
        .justify_end()
        .px(px(22.0))
        .child(drag_layer("zen-drag"))
        .child(no_drag(
            IconButton::new("leave-zen", IconName::Focus, "Leave zen mode")
                .size(IconButtonSize::Lg)
                .icon_size(18.0)
                .tip(zen_tip(state, "Leave zen mode"))
                .on_click(|_, _, cx| dispatch(AppCommand::SetZen { zen: false }, cx)),
        ))
}

/// A tooltip naming the keystroke zen mode is bound to.
fn zen_tip(state: &WorkspaceState, label: &str) -> SharedString {
    let settings = act_core::domain::shortcuts::shortcut_settings(&state.shortcuts);
    match settings.iter().find(|row| row.action.id == "zen.toggle").and_then(|row| row.binding.clone()) {
        Some(binding) => key_tip(label, &binding),
        None => label.to_string().into(),
    }
}

/// Hands the thread's checkout to another application on the machine.
fn open_in(root: &mut Root, state: &WorkspaceState, cx: &mut Context<Root>) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    let probe = root.topbar.open_in_probe.clone();
    let apps = state.installed_apps.clone();
    let open = root.menus.is_open("workspace:open-in");
    let blocked = matches!(act_core::workspace::location::location_of(state, state.current_thread()), act_core::workspace::location::ThreadLocation::Creating);
    // The list the menu opened with is replaced once the scan it asked for answers.
    let shown = apps.as_ref().map(|apps| apps.len());
    if open && root.topbar.open_in_shown != Some(shown) {
        if let Some(menu) = root.menus.entity("workspace:open-in") {
            let entries = open_in_entries(apps.as_deref());
            menu.update(cx, |menu, cx| {
                menu.set_empty(if apps.is_some() { "No application found" } else { "Looking for applications…" });
                menu.set_entries(entries, cx);
            });
        }
    }
    root.topbar.open_in_shown = open.then_some(shown);
    div()
        .id("open-in")
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, "Open this folder in another application").expanded(open))
        .relative()
        .size(px(34.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .transition(act_ui::style::touch())
        .when(open, |button| {
            lifted(button, &t)
        })
        .text_color(if open { t.ink } else { t.quiet_strong })
        .when(blocked, |button| button.opacity(t.disabled_opacity))
        .when(!blocked && !open, |button| button.hover(|style| style.bg(t.surface_hover).text_color(t.ink_dim)))
        .child(act_ui::Icon::new(IconName::FolderCode).size(px(18.0)))
        .child(bounds_probe(&probe))
        .tip(if blocked { "Open in… · Worktree is still being created" } else { "Open in…" })
        .on_click(cx.listener(move |root, _, window, cx| {
            if blocked {
                return;
            }
            if root.menus.is_open("workspace:open-in") {
                root.menus.close(cx);
                return;
            }
            dispatch(AppCommand::AppList, cx);
            // `.open-in-popover` sits 42px under the 34px trigger's top, 8 below it.
            let mut anchor = probe.get().unwrap_or_default();
            anchor.size.height += px(8.0 - act_ui::overlay::ANCHOR_GAP);
            let entries = open_in_entries(apps.as_deref());
            let menu = Menu::build(entries, Placement::Anchor { anchor, align: Align::End }, window, cx, |menu| menu.min_width(px(208.0)).exempt(&probe).empty(if apps.is_some() { "No application found" } else { "Looking for applications…" }));
            root.menus.show("workspace:open-in", menu, cx);
        }))
}

pub fn open_in_entries(apps: Option<&[act_core::domain::platform::InstalledApp]>) -> Vec<MenuEntry> {
    let mut entries = Vec::new();
    for (kind, label, fallback) in [("editor", "Editors", IconName::Code), ("terminal", "Terminals", IconName::SquareTerminal), ("files", "Files", IconName::Folder)] {
        let items: Vec<MenuItem> = apps
            .unwrap_or(&[])
            .iter()
            .filter(|app| app.extra.get("kind").and_then(|value| value.as_str()) == Some(kind))
            .map(|app| {
                let id = app.id.clone();
                let label = app.extra.get("label").and_then(|value| value.as_str()).unwrap_or(&app.name).to_string();
                let item = MenuItem::new(label).on_select(move |_, cx| dispatch(AppCommand::AppOpenFolder { app_id: id.clone() }, cx));
                match app.icon.as_deref().and_then(app_icon) {
                    Some(image) => item.image(image),
                    None => item.icon(fallback),
                }
            })
            .collect();
        if !items.is_empty() {
            entries.push(MenuEntry::Group { label: label.into(), items });
        }
    }
    entries
}

thread_local! {
    static APP_ICONS: std::cell::RefCell<std::collections::HashMap<u64, std::sync::Arc<gpui::Image>>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// An application's icon from the `data:` URL the scan hands over, decoded once.
fn app_icon(url: &str) -> Option<std::sync::Arc<gpui::Image>> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let key = hasher.finish();
    if let Some(image) = APP_ICONS.with(|icons| icons.borrow().get(&key).cloned()) {
        return Some(image);
    }
    let encoded = url.strip_prefix("data:image/png;base64,")?;
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).ok()?;
    let image = std::sync::Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes));
    APP_ICONS.with(|icons| icons.borrow_mut().insert(key, image.clone()));
    Some(image)
}

/// The thread's actions under the heading's ellipsis (WorkspaceHeader's PopoverMenu).
fn open_heading_menu(root: &mut Root, id: &str, window: &mut Window, cx: &mut Context<Root>) {
    let probe = root.topbar.menu_probe.clone();
    let id = id.to_string();
    let state = root.workspace.read(cx).state.clone();
    let Some(thread) = state.thread(&id) else { return };
    let anchor = probe.get().unwrap_or_default();
    let weak = cx.entity().downgrade();
    let rename = id.clone();
    let entries = thread_menu_entries(&state, thread, false, move |_, cx| {
        if let Some(root) = weak.upgrade() {
            let id = rename.clone();
            root.update(cx, |root, cx| {
                root.renaming_heading = Some(id.clone());
                root.start_rename(&id, cx);
            });
        }
    });
    let mut entries = entries;
    entries.retain(|entry| !matches!(entry, MenuEntry::Item(item) if item.label.as_ref() == "Unsnooze"));
    let menu = Menu::build(entries, Placement::Anchor { anchor, align: Align::End }, window, cx, |menu| menu.top_level().exempt(&probe));
    root.menus.show(&format!("thread-heading:{id}"), menu, cx);
}
