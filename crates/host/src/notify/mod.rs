//! Says what happened where the user already is: a desktop notification, silent, whose click brings
//! the thread forward. And whether the user has stepped away, which decides whether the phone is told.

/// How long the computer sits untouched before the user counts as away from it.
pub const AWAY_AFTER_SECONDS: f64 = 180.0;

/// The notice a thread raises.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadNotice {
    pub task_id: String,
    pub title: String,
    pub headline: String,
}

/// Shows a silent notification. `on_click` runs on a thread of its own when the user clicks it.
pub fn notify(title: &str, body: &str, on_click: Option<Box<dyn FnOnce() + Send>>) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        if os::show(&title, &body) {
            if let Some(on_click) = on_click {
                on_click();
            }
        }
    });
}

/// Names the application notifications come from; without one they are attributed to a stand-in.
pub fn set_notification_application(bundle_identifier: &str) {
    os::set_notification_application(bundle_identifier);
}

/// Whether the screen is locked right now.
pub fn screen_locked() -> bool {
    os::screen_locked()
}

/// Whether the user has stepped away from this computer: it is locked, or nobody has touched it for a while.
pub fn desktop_away() -> bool {
    os::screen_locked() || os::idle_seconds().is_some_and(|idle| idle >= AWAY_AFTER_SECONDS)
}

#[cfg(target_os = "macos")]
#[path = "mac.rs"]
mod os;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod os;
