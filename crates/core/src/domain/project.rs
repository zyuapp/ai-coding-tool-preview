//! Projects: folders the user works in.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_index: Option<f64>,
    /// Opened only to start a thread, so the sidebar leaves it out until a thread is started there.
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub unlisted: bool,
}

/// Where a moved thread lands: a slot in a project's threads, or among those with no project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDropTarget {
    pub project_id: Option<String>,
    pub index: i64,
}

pub const MAX_RECENT_PROJECTS: usize = 5;

/// The recently picked project ids with `project_id` moved to the front, newest first.
pub fn remember_project(recent: &[String], project_id: &str) -> Vec<String> {
    std::iter::once(project_id.to_string()).chain(recent.iter().filter(|id| *id != project_id).cloned()).take(MAX_RECENT_PROJECTS).collect()
}

/// How a folder names itself: its last path segment.
pub fn folder_name(root: &str) -> String {
    let trimmed = root.trim_end_matches('/');
    if trimmed.is_empty() {
        return root.to_string();
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

/// What a project is called everywhere in the UI.
pub fn project_name(project: &Project) -> String {
    project.name.clone().unwrap_or_else(|| folder_name(&project.root))
}

pub fn normalize_project_root(root: &str) -> String {
    let normalized = root.trim_end_matches(['/', '\\']);
    if normalized.is_empty() {
        root.to_string()
    } else {
        normalized.to_string()
    }
}

pub fn same_root(left: &str, right: &str) -> bool {
    normalize_project_root(left) == normalize_project_root(right)
}

fn encode_uri_component(text: &str) -> String {
    let mut out = String::new();
    for byte in text.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || "-_.!~*'()".contains(ch) {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn legacy_project_id(root: &str) -> String {
    format!("legacy-project-{}", encode_uri_component(&normalize_project_root(root)))
}

/// A project as something outside the app may name it: its folder name, its path, or its id.
pub fn matching_projects<'a>(projects: &'a [Project], reference: &str) -> Vec<&'a Project> {
    let wanted = reference.trim();
    if let Some(exact) = projects.iter().find(|project| project.id == wanted || same_root(&project.root, wanted)) {
        return vec![exact];
    }
    let lowered = wanted.to_lowercase();
    projects.iter().filter(|project| [project_name(project), folder_name(&project.root)].iter().any(|label| label.to_lowercase() == lowered)).collect()
}

pub fn find_project<'a>(projects: &'a [Project], reference: &str) -> Result<&'a Project, String> {
    let named = matching_projects(projects, reference);
    if named.len() == 1 {
        return Ok(named[0]);
    }
    if named.len() > 1 {
        let roots: Vec<&str> = named.iter().map(|project| project.root.as_str()).collect();
        return Err(format!("More than one open project is named \"{reference}\": {}. Name the folder path instead.", roots.join(", ")));
    }
    let open: Vec<String> = projects.iter().map(|project| format!("{} ({})", project_name(project), project.root)).collect();
    let open = if open.is_empty() { "none".to_string() } else { open.join(", ") };
    Err(format!("No project matches \"{reference}\". Open projects: {open}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_ignore_trailing_slashes() {
        assert_eq!(folder_name("/a/b/"), "b");
        assert_eq!(folder_name("/"), "/");
    }

    #[test]
    fn remembered_projects_move_to_front_and_cap() {
        let recent: Vec<String> = ["a", "b", "c", "d", "e"].iter().map(|s| s.to_string()).collect();
        assert_eq!(remember_project(&recent, "c"), vec!["c", "a", "b", "d", "e"]);
        assert_eq!(remember_project(&recent, "z").len(), 5);
    }
}
