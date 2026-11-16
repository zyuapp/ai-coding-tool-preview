//! The Dock icon: a badge counting unread threads, and a menu that clears what it counts. Both are
//! AppKit's, so they are set on the main thread; elsewhere they do nothing.

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;
    use std::ptr;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{define_class, ffi, msg_send, sel, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::{NSObject, NSString};

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "ACTDockMenuTarget"]
        struct DockTarget;

        impl DockTarget {
            #[unsafe(method(markAllRead:))]
            fn mark_all_read(&self, _sender: Option<&AnyObject>) {
                let callback = DOCK.with(|dock| dock.borrow().mark_all_read.clone());
                if let Some(callback) = callback {
                    callback();
                }
            }
        }
    );

    impl DockTarget {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            unsafe { msg_send![Self::alloc(mtm), init] }
        }
    }

    #[derive(Default)]
    struct Dock {
        mark_all_read: Option<std::rc::Rc<dyn Fn()>>,
        has_unread: bool,
        menu: Option<Retained<NSMenu>>,
        target: Option<Retained<DockTarget>>,
        installed: bool,
    }

    thread_local! {
        static DOCK: RefCell<Dock> = RefCell::new(Dock::default());
    }

    /// The delegate's `applicationDockMenu:`, answering with the menu kept here.
    unsafe extern "C-unwind" fn application_dock_menu(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject) -> *mut NSMenu {
        DOCK.with(|dock| dock.borrow().menu.as_ref().map(|menu| Retained::as_ptr(menu) as *mut NSMenu).unwrap_or(ptr::null_mut()))
    }

    fn build_menu(mtm: MainThreadMarker, dock: &mut Dock) {
        let target = dock.target.get_or_insert_with(|| DockTarget::new(mtm)).clone();
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str("Mark All as Read"), Some(sel!(markAllRead:)), &NSString::from_str("")) };
        unsafe { item.setTarget(Some(&target)) };
        item.setEnabled(dock.has_unread);
        menu.addItem(&item);
        dock.menu = Some(menu);
    }

    /// Answers the Dock's ask for a menu from the app delegate, whichever class it is.
    fn hook_delegate(mtm: MainThreadMarker) -> bool {
        let app = NSApplication::sharedApplication(mtm);
        let Some(delegate) = app.delegate() else { return false };
        let object: &AnyObject = delegate.as_ref();
        let class: &AnyClass = object.class();
        let imp: Imp = unsafe { std::mem::transmute::<unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> *mut NSMenu, Imp>(application_dock_menu) };
        unsafe { ffi::class_replaceMethod(class as *const AnyClass as *mut AnyClass, sel!(applicationDockMenu:), imp, c"@@:@".as_ptr()) };
        true
    }

    pub fn install_dock_menu(on_mark_all_read: impl Fn() + 'static) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        DOCK.with(|dock| {
            let mut dock = dock.borrow_mut();
            dock.mark_all_read = Some(std::rc::Rc::new(on_mark_all_read));
            build_menu(mtm, &mut dock);
            if !dock.installed {
                dock.installed = hook_delegate(mtm);
            }
        });
    }

    pub fn set_unread_badge(count: usize) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let app = NSApplication::sharedApplication(mtm);
        let tile = app.dockTile();
        let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
        tile.setBadgeLabel(label.as_deref());
        DOCK.with(|dock| {
            let mut dock = dock.borrow_mut();
            if (count > 0) == dock.has_unread {
                return;
            }
            dock.has_unread = count > 0;
            if dock.mark_all_read.is_some() {
                build_menu(mtm, &mut dock);
            }
        });
    }
}

/// The Dock icon's menu, whose one item clears what the badge counts. Call on the main thread once the
/// app has launched; it takes over the app delegate's Dock menu.
pub fn install_dock_menu(on_mark_all_read: impl Fn() + 'static) {
    #[cfg(target_os = "macos")]
    mac::install_dock_menu(on_mark_all_read);
    #[cfg(not(target_os = "macos"))]
    drop(on_mark_all_read);
}

/// Shows `count` on the Dock icon, nothing for zero, and enables the menu's item while there is something unread.
pub fn set_unread_badge(count: usize) {
    #[cfg(target_os = "macos")]
    mac::set_unread_badge(count);
    #[cfg(not(target_os = "macos"))]
    let _ = count;
}
