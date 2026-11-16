//! Who may use which page: the user anything, a run only the origins the user allowed, unless the
//! run was started to browse on its own.

use std::collections::HashSet;

use act_core::domain::platform::BrowserPermissions;

pub const APPROVAL_ERROR: &str = "This site needs browser approval before the agent can use it.";
pub const SCRIPT_APPROVAL_ERROR: &str = "This site needs browser approval.";

#[derive(Default, Clone, Debug)]
pub struct Permissions {
    origins: HashSet<String>,
    autonomous: HashSet<String>,
}

pub fn origin_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let origin = parsed.origin();
            if origin.is_tuple() { origin.ascii_serialization() } else { String::new() }
        }
        Err(_) => String::new(),
    }
}

/// Only web pages load in the panel, so nothing reaches the file system or another app's scheme.
pub fn is_web_url(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("http:") || lower.starts_with("https:")
}

impl Permissions {
    pub fn configure(&mut self, permissions: &BrowserPermissions) {
        self.origins = permissions.origins.iter().cloned().collect();
        self.autonomous = permissions.autonomous_task_ids.iter().cloned().collect();
    }

    pub fn clear_origins(&mut self) {
        self.origins.clear();
    }

    pub fn permitted(&self, task_id: Option<&str>, url: &str) -> bool {
        match task_id {
            None => true,
            Some(task) => self.autonomous.contains(task) || self.origins.contains(&origin_of(url)),
        }
    }

    /// The origins a script must find itself on, or None when the caller may run it anywhere.
    pub fn script_origins(&self, task_id: Option<&str>) -> Option<Vec<String>> {
        match task_id {
            Some(task) if !self.autonomous.contains(task) => {
                let mut origins: Vec<String> = self.origins.iter().cloned().collect();
                origins.sort();
                Some(origins)
            }
            _ => None,
        }
    }

    pub fn require(&self, task_id: Option<&str>, url: &str) -> Result<(), String> {
        if self.permitted(task_id, url) { Ok(()) } else { Err(APPROVAL_ERROR.into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_reach_only_allowed_origins_unless_autonomous() {
        let mut permissions = Permissions::default();
        permissions.configure(&BrowserPermissions { origins: vec!["https://a.com".into()], autonomous_task_ids: vec!["free".into()] });
        assert!(permissions.permitted(None, "https://b.com/"));
        assert!(permissions.permitted(Some("run"), "https://a.com/path"));
        assert!(!permissions.permitted(Some("run"), "https://b.com/"));
        assert!(!permissions.permitted(Some("run"), "http://a.com/"));
        assert!(permissions.permitted(Some("free"), "https://b.com/"));
        assert!(permissions.script_origins(Some("free")).is_none());
        assert_eq!(permissions.script_origins(Some("run")), Some(vec!["https://a.com".to_string()]));
    }
}
