//! Files the app ships beside its binary: a bundle's `Contents/Resources` (on Linux the package's
//! `Resources` beside `bin`), else the source checkout's `assets`.

use std::path::PathBuf;

pub fn resource(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundled = exe.parent()?.parent()?.join("Resources").join(name);
    if bundled.exists() {
        return Some(bundled);
    }
    exe.ancestors().map(|dir| dir.join("assets").join(name)).find(|path| path.exists())
}

/// The app's own Claude plugin, whose skills every engine is offered.
pub fn app_plugin_root() -> Option<String> {
    resource("app-plugin").map(|path| path.to_string_lossy().into_owned())
}
