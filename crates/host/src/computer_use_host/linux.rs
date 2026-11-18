//! Linux computer use: whatever the session's display server lets the driver reach.

use super::{ComputerUsePermission, ComputerUsePermissions, LinuxRuntime};

pub const NEEDS_GRANTS: bool = false;

/// Which display server the session runs, from its environment.
fn display() -> &'static str {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
    let x11 = std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty());
    match (wayland, x11) {
        (true, true) => "xwayland",
        (true, false) => "wayland",
        (false, true) => "x11",
        (false, false) => "none",
    }
}

pub fn capability() -> Result<&'static str, String> {
    let display = display();
    if display == "none" {
        return Err("Computer use needs a graphical Linux session, but neither DISPLAY nor WAYLAND_DISPLAY is available.".into());
    }
    if display == "wayland" && std::env::var("CUA_DRIVER_RS_ENABLE_WAYLAND").as_deref() != Ok("1") {
        return Err(concat!("Native Wayland computer use is compositor-dependent and opt-in. Start ", act_core::app_name!(), " with CUA_DRIVER_RS_ENABLE_WAYLAND=1, or use an X11/XWayland session.").into());
    }
    Ok(display)
}

pub fn permissions(binary: Option<&std::path::Path>) -> ComputerUsePermissions {
    let display = match capability() {
        Err(message) => {
            return ComputerUsePermissions {
                accessibility: false,
                screen_recording: false,
                linux_runtime: Some(LinuxRuntime { status: "unavailable".into(), display: self::display().into(), message }),
            }
        }
        Ok(display) => display,
    };
    if !binary.is_some_and(|binary| binary.is_file()) {
        return ComputerUsePermissions {
            accessibility: false,
            screen_recording: false,
            linux_runtime: Some(LinuxRuntime { status: "unavailable".into(), display: display.into(), message: "The bundled CUA Driver executable is missing.".into() }),
        };
    }
    let limited = display == "xwayland" || display == "wayland";
    let message = match display {
        "x11" => "Computer use is ready for this X11 session.",
        "xwayland" => "X11 and XWayland apps are available. Native Wayland apps depend on the compositor and may not be reachable.",
        _ => "Native Wayland support is enabled, but its capabilities depend on the active compositor.",
    };
    ComputerUsePermissions {
        accessibility: true,
        screen_recording: true,
        linux_runtime: Some(LinuxRuntime { status: if limited { "limited" } else { "available" }.into(), display: display.into(), message: message.into() }),
    }
}

/// Nothing to grant: the session decides what the driver reaches.
pub fn request(_permission: ComputerUsePermission) {}

pub fn start_failure(display: &str, error: String) -> String {
    let session = match display {
        "wayland" => "this Wayland compositor",
        "xwayland" => "this Wayland/XWayland session",
        _ => "this X11 session",
    };
    format!("CUA Driver could not start in {session}: {error}")
}
