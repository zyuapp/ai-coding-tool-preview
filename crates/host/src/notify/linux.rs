//! Linux notifications, and idleness and the lock screen as the login session reports them.

pub fn set_notification_application(_bundle_identifier: &str) {}

/// Shows one notification and waits for it to be answered; true when it was clicked.
pub fn show(title: &str, body: &str) -> bool {
    let app_id = crate::linux_protocol::LINUX_APP_ID;
    let Ok(handle) = notify_rust::Notification::new()
        .appname(act_core::identity::APP_NAME)
        .icon(app_id)
        .summary(title)
        .body(body)
        .action("default", "Open")
        .hint(notify_rust::Hint::DesktopEntry(app_id.into()))
        .hint(notify_rust::Hint::SuppressSound(true))
        .show()
    else {
        return false;
    };
    let mut clicked = false;
    handle.wait_for_action(|action| clicked = action == "default");
    clicked
}

/// The login session's own hints, which every desktop's idle and lock tracking reports to.
fn session_hint(name: &str) -> Option<String> {
    let session = std::env::var("XDG_SESSION_ID").ok()?;
    let output = act_platform::process::std_command("loginctl").args(["show-session", &session, "-p", name, "--value"]).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Only whether the session counts itself idle is known here, which reads as away.
pub fn idle_seconds() -> Option<f64> {
    (session_hint("IdleHint")? == "yes").then_some(super::AWAY_AFTER_SECONDS)
}

pub fn screen_locked() -> bool {
    session_hint("LockedHint").as_deref() == Some("yes")
}
