//! Sets the Dock badge and menu on a bare AppKit app and reads them back.

#[cfg(target_os = "macos")]
fn main() {
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSMenu};

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "ACTDockExampleDelegate"]
        struct Delegate;
    );

    let mtm = MainThreadMarker::new().expect("the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(mtm), init] };
    let _: () = unsafe { msg_send![&app, setDelegate: &*delegate] };

    act_host::dock::install_dock_menu(|| println!("marked all read"));
    let menu_item = |app: &NSApplication| {
        let menu: Option<Retained<NSMenu>> = unsafe { msg_send![&*delegate, applicationDockMenu: app] };
        let item = menu.and_then(|menu| menu.itemAtIndex(0)).expect("a menu item");
        (item.title().to_string(), item.isEnabled())
    };
    act_host::dock::set_unread_badge(3);
    let badge = app.dockTile().badgeLabel().map(|label| label.to_string());
    println!("badge {badge:?} menu {:?}", menu_item(&app));
    act_host::dock::set_unread_badge(0);
    let badge = app.dockTile().badgeLabel().map(|label| label.to_string());
    println!("badge {badge:?} menu {:?}", menu_item(&app));
}

#[cfg(not(target_os = "macos"))]
fn main() {}
