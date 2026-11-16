//! GTK on GPUI's main thread: started once, on the display server GPUI uses, and its main context
//! run from GPUI's own loop, quickly while it has work and slowly while it has none.

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_int, c_void};
use std::time::Duration;

const BUSY: Duration = Duration::from_millis(2);
const IDLE: Duration = Duration::from_millis(8);
const ASLEEP: Duration = Duration::from_millis(32);
/// Idle turns before the loop slows to `ASLEEP`.
const DROWSY_TURNS: u32 = 250;

thread_local! {
    static STARTED: RefCell<Option<Result<(), String>>> = const { RefCell::new(None) };
    static ACCESSIBLE_HOLDS: Cell<usize> = const { Cell::new(0) };
}

/// Starts GTK, once. Every later call answers how that went.
pub fn ensure(cx: &mut gpui::App) -> Result<(), String> {
    if let Some(started) = STARTED.with(|started| started.borrow().clone()) {
        return started;
    }
    let started = start();
    STARTED.with(|state| *state.borrow_mut() = Some(started.clone()));
    if started.is_ok() {
        cx.spawn(async move |cx| {
            let mut idle_turns = 0u32;
            loop {
                let wait = if pump() {
                    idle_turns = 0;
                    BUSY
                } else {
                    idle_turns = idle_turns.saturating_add(1);
                    if idle_turns > DROWSY_TURNS { ASLEEP } else { IDLE }
                };
                cx.background_executor().timer(wait).await;
            }
        })
        .detach();
    }
    started
}

/// Whether GPUI draws through X11, where pages can be placed inside its window.
pub fn on_x11() -> bool {
    gpui::guess_compositor() == "X11"
}

fn start() -> Result<(), String> {
    // GTK would otherwise set the C locale from the environment for the whole process.
    gtk::disable_setlocale();
    // GTK names its windows (sign-in popups) after the program; they belong with the app's window.
    gtk::glib::set_prgname(Some(act_host::linux_protocol::LINUX_APP_ID));
    gtk::gdk::set_allowed_backends(if on_x11() { "x11" } else { "wayland,x11" });
    gtk::init().map_err(|error| format!("Web pages need GTK, which did not start: {error}"))?;
    // GTK lists itself with assistive tools as an app of its own; it stays unlisted until it shows a page.
    bridge(false);
    Ok(())
}

/// Turns GTK's accessibility bridge on or off.
fn bridge(on: bool) {
    type Init = unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int;
    type Cleanup = unsafe extern "C" fn();
    let symbol = if on { c"atk_bridge_adaptor_init" } else { c"atk_bridge_adaptor_cleanup" };
    let function = unsafe { libc::dlsym(libc::RTLD_DEFAULT, symbol.as_ptr()) };
    if function.is_null() {
        return;
    }
    unsafe {
        if on {
            std::mem::transmute::<*mut c_void, Init>(function)(std::ptr::null_mut(), std::ptr::null_mut());
        } else {
            std::mem::transmute::<*mut c_void, Cleanup>(function)();
        }
    }
}

/// Keeps GTK's pages reachable by assistive tools while held.
pub struct AccessibleHold(());

impl AccessibleHold {
    pub fn new() -> Self {
        if ACCESSIBLE_HOLDS.with(|holds| holds.replace(holds.get() + 1)) == 0 {
            bridge(true);
        }
        AccessibleHold(())
    }
}

impl Default for AccessibleHold {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AccessibleHold {
    fn drop(&mut self) {
        if ACCESSIBLE_HOLDS.with(|holds| holds.replace(holds.get() - 1)) == 1 {
            bridge(false);
        }
    }
}

/// Runs whatever GTK has ready. True when there was something.
pub fn pump() -> bool {
    let mut ran = false;
    let mut turns = 0;
    while gtk::events_pending() && turns < 200 {
        gtk::main_iteration_do(false);
        ran = true;
        turns += 1;
    }
    ran
}
