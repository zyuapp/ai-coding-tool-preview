//! The checkouts Settings lists: every directory the app keeps, the threads in each, and what can go.

use std::collections::{HashMap, HashSet};

use crate::domain::project::project_name;
use crate::domain::worktree::{ManagedWorktree, WorktreeStatus};
use crate::state::WorkspaceState;
use crate::util::{last_segment, locale_cmp};

#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeThread {
    pub id: String,
    pub title: String,
    pub archived: bool,
}

/// One checkout as the Settings list draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeSettingsView {
    pub id: String,
    pub root: String,
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub status: WorktreeStatus,
    pub name: String,
    pub title: String,
    pub project: String,
    pub project_key: String,
    pub available: bool,
    pub recorded: bool,
    pub threads: Vec<WorktreeThread>,
    pub busy: bool,
    /// The checkout is on its way out, whether Settings or its own thread asked.
    pub deleting: bool,
    /// Nothing would be lost: no uncommitted changes, every commit on the default branch, no run active.
    pub safe: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeProjectOption {
    pub key: String,
    pub name: String,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeSettingsPage {
    pub loading: bool,
    pub total: usize,
    pub project: Option<String>,
    pub projects: Vec<WorktreeProjectOption>,
    pub available: Vec<WorktreeSettingsView>,
    pub missing: Vec<WorktreeSettingsView>,
    pub missing_open: bool,
    pub expanded_threads: Vec<String>,
    pub confirmation: Option<WorktreeSettingsView>,
    pub safe: Vec<WorktreeSettingsView>,
    pub safe_confirmation: Option<Vec<WorktreeSettingsView>>,
}

pub fn safe_to_delete(worktree: &WorktreeSettingsView) -> bool {
    worktree.available && worktree.repository.is_some() && worktree.status.changed_files == Some(0) && worktree.status.comparison.as_ref().is_some_and(|comparison| comparison.ahead == 0) && !worktree.busy && !worktree.deleting
}

/// Every checkout on disk and every one a thread records, or None before the list has been read.
pub fn worktree_settings_views(state: &WorkspaceState, busy: &HashSet<String>) -> Option<Vec<WorktreeSettingsView>> {
    let managed = state.managed_worktrees.as_ref()?;
    let on_disk: HashSet<&str> = managed.iter().map(|worktree| worktree.root.as_str()).collect();
    let mut threads_by_worktree: HashMap<&str, Vec<WorktreeThread>> = HashMap::new();
    for thread in &state.threads {
        let Some(worktree_id) = thread.worktree_id.as_deref() else { continue };
        threads_by_worktree.entry(worktree_id).or_default().push(WorktreeThread { id: thread.id.clone(), title: thread.title.clone(), archived: thread.archived_at.is_some() });
    }
    let mut directories: Vec<ManagedWorktree> = managed.clone();
    for worktree in &state.worktrees {
        if on_disk.contains(worktree.root.as_str()) {
            continue;
        }
        directories.push(ManagedWorktree {
            id: worktree.id.clone(),
            root: worktree.root.clone(),
            repository: state.project(&worktree.project_id).map(|project| project.root.clone()),
            branch: None,
            status: WorktreeStatus { changed_files: None, comparison: None },
        });
    }
    let deleting: HashSet<&str> = state.deleting_worktrees.iter().map(String::as_str).collect();
    let releasing: HashSet<&str> = state.releasing_worktrees.iter().map(String::as_str).collect();
    let mut views: Vec<WorktreeSettingsView> = directories
        .into_iter()
        .map(|directory| {
            let record = state.worktrees.iter().find(|worktree| worktree.root == directory.root).or_else(|| state.worktrees.iter().find(|worktree| worktree.id == directory.id));
            let threads = threads_by_worktree.get(record.map(|record| record.id.as_str()).unwrap_or(directory.id.as_str())).cloned().unwrap_or_default();
            let project = match record {
                Some(record) => state.project(&record.project_id),
                None => state.projects.iter().find(|project| Some(&project.root) == directory.repository.as_ref()),
            };
            let name = last_segment(&directory.root).unwrap_or(&directory.id).to_string();
            let title = record.and_then(|record| record.name.clone()).or_else(|| directory.branch.clone()).or_else(|| threads.first().map(|thread| thread.title.clone())).unwrap_or_else(|| name.clone());
            let project_label = match project {
                Some(project) => project_name(project),
                None => directory.repository.as_deref().and_then(last_segment).unwrap_or("Unknown project").to_string(),
            };
            let project_key = project.map(|project| project.id.clone()).or_else(|| directory.repository.clone()).unwrap_or_else(|| "unknown".into());
            let mut view = WorktreeSettingsView {
                available: on_disk.contains(directory.root.as_str()),
                recorded: record.is_some(),
                busy: threads.iter().any(|thread| busy.contains(&thread.id)),
                deleting: deleting.contains(directory.root.as_str()) || threads.iter().any(|thread| releasing.contains(thread.id.as_str())),
                threads,
                name,
                title,
                project: project_label,
                project_key,
                id: directory.id,
                root: directory.root,
                repository: directory.repository,
                branch: directory.branch,
                status: directory.status,
                safe: false,
            };
            view.safe = safe_to_delete(&view);
            view
        })
        .collect();
    views.sort_by(|left, right| locale_cmp(&left.project, &right.project).then_with(|| locale_cmp(&left.title, &right.title)).then_with(|| locale_cmp(&left.root, &right.root)));
    Some(views)
}

pub fn worktree_settings_page(state: &WorkspaceState, worktrees: Option<&[WorktreeSettingsView]>) -> WorktreeSettingsPage {
    let settings = &state.worktree_settings;
    let mut projects: indexmap::IndexMap<String, WorktreeProjectOption> = state.projects.iter().map(|project| (project.id.clone(), WorktreeProjectOption { key: project.id.clone(), name: project_name(project), count: 0 })).collect();
    for worktree in worktrees.unwrap_or(&[]) {
        projects.entry(worktree.project_key.clone()).or_insert_with(|| WorktreeProjectOption { key: worktree.project_key.clone(), name: worktree.project.clone(), count: 0 }).count += 1;
    }
    let selected = settings.project.clone().filter(|project| projects.contains_key(project));
    let visible: Vec<&WorktreeSettingsView> = worktrees.unwrap_or(&[]).iter().filter(|worktree| selected.as_ref().is_none_or(|project| &worktree.project_key == project)).collect();
    let available: Vec<WorktreeSettingsView> = visible.iter().filter(|worktree| worktree.available).map(|worktree| (*worktree).clone()).collect();
    let missing: Vec<WorktreeSettingsView> = visible.iter().filter(|worktree| !worktree.available).map(|worktree| (*worktree).clone()).collect();
    let confirmation = visible.iter().find(|worktree| Some(&worktree.root) == settings.confirming.as_ref() && !worktree.deleting).map(|worktree| (*worktree).clone());
    let safe: Vec<WorktreeSettingsView> = available.iter().filter(|worktree| worktree.safe).cloned().collect();
    let mut options: Vec<WorktreeProjectOption> = projects.into_values().collect();
    options.sort_by(|left, right| locale_cmp(&left.name, &right.name));
    WorktreeSettingsPage {
        loading: state.worktree_management_loading,
        total: worktrees.map(|worktrees| worktrees.len()).unwrap_or(0),
        project: selected,
        projects: options,
        missing_open: settings.missing_open.unwrap_or(available.is_empty() && !missing.is_empty()),
        expanded_threads: settings.expanded_threads.clone(),
        confirmation,
        safe_confirmation: (settings.confirming_safe && !safe.is_empty()).then(|| safe.clone()),
        safe,
        available,
        missing,
    }
}
