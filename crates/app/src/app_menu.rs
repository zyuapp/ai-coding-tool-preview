//! The menu bar and the Dock icon's menu (app-menu.ts, dock-menu.ts): the platform's default menus
//! with the app's own help items, page zoom in View, and the unread badge.

use std::time::Duration;

use act_core::AppCommand;
use gpui::{actions, App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType};

use crate::workspace::{dispatch, workspace};

actions!(
    app_menu,
    [
        About,
        CheckForUpdates,
        OpenSourceLicenses,
        Hide,
        HideOthers,
        ShowAll,
        Quit,
        CloseWindow,
        ResetZoom,
        ZoomIn,
        ZoomOut,
        ToggleFullScreen,
        Minimize,
        ZoomWindow,
        BringAllToFront,
        MarkAllRead,
        NothingUnread,
        UpdateChecking,
    ]
);

/// A context no element sets, so a binding there only names a menu item's keys.
const MENU_ONLY: &str = "MenuOnly";
use act_core::identity::APP_NAME;

pub fn install(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-w", CloseWindow, Some(MENU_ONLY)),
        KeyBinding::new("cmd-+", ZoomIn, None),
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd-shift-=", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
        KeyBinding::new("cmd-0", ResetZoom, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    cx.on_action(|_: &About, cx| show_about(cx));
    cx.on_action(|_: &CheckForUpdates, cx| dispatch(AppCommand::AppCheckForUpdates { quiet: false }, cx));
    cx.on_action(|_: &OpenSourceLicenses, cx| dispatch(AppCommand::AppOpenSourceLicenses, cx));
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |window, _| window.remove_window()));
    cx.on_action(|_: &ZoomIn, cx| crate::zoom::zoom_in(cx));
    cx.on_action(|_: &ZoomOut, cx| crate::zoom::zoom_out(cx));
    cx.on_action(|_: &ResetZoom, cx| crate::zoom::reset(cx));
    cx.on_action(|_: &ToggleFullScreen, cx| with_active_window(cx, |window, _| window.toggle_fullscreen()));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |window, _| window.minimize_window()));
    cx.on_action(|_: &ZoomWindow, cx| with_active_window(cx, |window, _| window.zoom_window()));
    cx.on_action(|_: &BringAllToFront, cx| bring_all_to_front(cx));
    cx.on_action(|_: &MarkAllRead, cx| dispatch(AppCommand::TaskReadAll { local_only: None }, cx));
    cx.set_menus(menus(false));
    set_dock_menu(false, cx);
    set_app_icon();
}

/// The Dock and About panel show the app's icon, which an unbundled build has none of.
#[cfg(target_os = "macos")]
fn set_app_icon() {
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{MainThreadMarker, NSData};

    let bundled = std::env::current_exe().ok().is_some_and(|exe| exe.parent().is_some_and(|dir| dir.ends_with("Contents/MacOS")));
    if bundled {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(include_bytes!("../../../assets/icon.png"));
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else { return };
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
}

#[cfg(not(target_os = "macos"))]
fn set_app_icon() {}

/// While the updater looks, its menu item says so and cannot be chosen: its action has no handler then.
pub fn set_update_checking(checking: bool, cx: &mut App) {
    cx.set_menus(menus(checking));
}

fn menus(checking: bool) -> Vec<Menu> {
    let check_for_updates = || if checking { MenuItem::action("Checking for Updates…", UpdateChecking) } else { MenuItem::action("Check for Updates…", CheckForUpdates) };
    let licenses = || MenuItem::action("Open Source Licenses…", OpenSourceLicenses);
    let mac = cfg!(target_os = "macos");
    let mut menus = Vec::new();
    if mac {
        menus.push(Menu {
            name: APP_NAME.into(),
            items: vec![
                MenuItem::action(format!("About {APP_NAME}"), About),
                MenuItem::separator(),
                check_for_updates(),
                licenses(),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(format!("Hide {APP_NAME}"), Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action(format!("Quit {APP_NAME}"), Quit),
            ],
        });
    }
    menus.push(Menu { name: "File".into(), items: vec![if mac { MenuItem::action("Close Window", CloseWindow) } else { MenuItem::action("Quit", Quit) }] });
    menus.push(Menu {
        name: "Edit".into(),
        items: vec![
            MenuItem::os_action("Undo", act_ui::input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", act_ui::input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", act_ui::input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", act_ui::input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", act_ui::input::Paste, OsAction::Paste),
            MenuItem::action("Delete", act_ui::input::Delete),
            MenuItem::os_action("Select All", act_ui::input::SelectAll, OsAction::SelectAll),
        ],
    });
    menus.push(Menu {
        name: "View".into(),
        items: vec![
            MenuItem::action("Actual Size", ResetZoom),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::separator(),
            MenuItem::action("Toggle Full Screen", ToggleFullScreen),
        ],
    });
    let mut window = vec![MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", ZoomWindow)];
    if mac {
        window.extend([MenuItem::separator(), MenuItem::action("Bring All to Front", BringAllToFront)]);
    } else {
        window.push(MenuItem::action("Close", CloseWindow));
    }
    menus.push(Menu { name: "Window".into(), items: window });
    if !mac {
        menus.push(Menu { name: "Help".into(), items: vec![check_for_updates(), licenses()] });
    }
    menus
}

/// The Dock menu's one item is greyed out while nothing is unread: its action has no handler then.
fn set_dock_menu(has_unread: bool, cx: &mut App) {
    let item = if has_unread { MenuItem::action("Mark All as Read", MarkAllRead) } else { MenuItem::action("Mark All as Read", NothingUnread) };
    cx.set_dock_menu(vec![item]);
}

/// Keeps the Dock badge on the unread count, checked at most a few times a second.
pub fn watch_unread(cx: &mut App) {
    let workspace = workspace(cx);
    cx.spawn(async move |cx| {
        let mut shown: Option<usize> = None;
        loop {
            let count = cx.update(|cx| {
                let state = workspace.read(cx).state.clone();
                state.restored.then(|| act_core::workspace::activity::thread_lists(&state).unread_count + act_core::view::workspace::remote_unread_count(&state))
            });
            let Ok(count) = count else { break };
            if let Some(count) = count.filter(|count| Some(*count) != shown) {
                let had = shown.is_some_and(|shown| shown > 0);
                shown = Some(count);
                act_host::dock::set_unread_badge(count);
                if had != (count > 0) {
                    let _ = cx.update(|cx| set_dock_menu(count > 0, cx));
                }
            }
            cx.background_executor().timer(Duration::from_millis(300)).await;
        }
    })
    .detach();
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window, &mut App) + 'static) {
    let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else { return };
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, cx| f(window, cx));
    });
}

fn bring_all_to_front(cx: &mut App) {
    cx.activate(true);
    cx.defer(|cx| {
        for window in cx.windows() {
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
    });
}

#[cfg(target_os = "macos")]
fn show_about(cx: &mut App) {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{MainThreadMarker, NSDictionary, NSString};

    let Some(mtm) = MainThreadMarker::new() else { return };
    cx.activate(true);
    let app = NSApplication::sharedApplication(mtm);
    let keys = [NSString::from_str("ApplicationName"), NSString::from_str("ApplicationVersion"), NSString::from_str("Version")];
    let values: [Retained<AnyObject>; 3] = [NSString::from_str(APP_NAME).into(), NSString::from_str(crate::startup::APP_VERSION).into(), NSString::from_str("").into()];
    let keys: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
    let options = NSDictionary::from_retained_objects(&keys, &values);
    unsafe {
        let _: () = msg_send![&*app, orderFrontStandardAboutPanelWithOptions: &*options];
    }
}

#[cfg(not(target_os = "macos"))]
fn show_about(_cx: &mut App) {}
