//! Where the app's own plugin lives on disk, set once at launch. Unset means no app skills.

use std::path::Path;
use std::sync::RwLock;

/// The plugin name both engines qualify the app's skills with.
pub const APP_PLUGIN_NAME: &str = "aicodingtool";

static ROOT: RwLock<Option<String>> = RwLock::new(None);

pub fn set_app_plugin_root(directory: Option<String>) {
    *ROOT.write().unwrap() = directory;
}

pub fn app_plugin_root() -> Option<String> {
    ROOT.read().unwrap().clone()
}

/// The plugin's skills folder, which Codex and OpenCode read as an extra skill root.
pub fn app_skills_root() -> Option<String> {
    app_plugin_root().map(|root| {
        Path::new(&root)
            .join("skills")
            .to_string_lossy()
            .into_owned()
    })
}

/// `aicodingtool:review-thread` and `review-thread` are the same app skill.
pub fn unqualified_skill_name(name: &str) -> &str {
    name.strip_prefix(&format!("{APP_PLUGIN_NAME}:"))
        .unwrap_or(name)
}
