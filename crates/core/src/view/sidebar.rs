//! Every list the sidebar draws, in the order it draws them.

use std::collections::{HashMap, HashSet};

use crate::domain::coordination::{coordinated_ids, threads_by_coordinator, waiting_on_user};
use crate::domain::project::Project;
use crate::domain::shortcuts::SLOT_COUNT;
use crate::domain::thread::Thread;
use crate::state::WorkspaceState;
use crate::util::Map;
use crate::workspace::activity::{thread_activity, thread_lists};
use crate::workspace::coordination::{coordination_sections, project_activity, waiting_line};
use crate::workspace::order::{order_refs, slot_thread_ids, ActivitySections};

/// One row's tag: which computer holds it, whether it can be reached, and whether it is being dialled again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadHost {
    pub id: String,
    pub name: String,
    pub offline: bool,
    pub reconnecting: bool,
}

/// Sidebar order for folders: `sort_index` wins so folders never move on their own.
pub fn order_projects(projects: &[Project]) -> Vec<Project> {
    let mut ordered = projects.to_vec();
    ordered.sort_by(|left, right| match (left.sort_index, right.sort_index) {
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    ordered
}

/// Freezes the order projects loaded in.
pub fn backfill_project_sort_index(projects: &mut [Project]) {
    if projects.iter().all(|project| project.sort_index.is_some()) {
        return;
    }
    let positions: HashMap<String, usize> = order_projects(projects).iter().enumerate().map(|(index, project)| (project.id.clone(), index)).collect();
    for project in projects.iter_mut() {
        if project.sort_index.is_none() {
            project.sort_index = Some(positions[&project.id] as f64);
        }
    }
}

pub fn next_project_sort_index(projects: &[Project]) -> f64 {
    projects.iter().map(|project| project.sort_index.unwrap_or(0.0)).fold(0.0, f64::min) - 1.0
}

/// Names a folder in the sidebar. An empty name takes the last one off. Returns whether it changed.
pub fn name_project(projects: &mut [Project], project_id: &str, name: Option<Option<&str>>) -> bool {
    let Some(name) = name else { return false };
    let wanted = name.map(str::trim).filter(|name| !name.is_empty()).map(String::from);
    let Some(project) = projects.iter_mut().find(|item| item.id == project_id) else { return false };
    if project.name == wanted {
        return false;
    }
    project.name = wanted;
    true
}

/// Each computer owns its project order; this computer first and paired computers together.
fn order_sidebar_projects(projects: &[Project], hosts: Option<&HashMap<String, ThreadHost>>) -> Vec<Project> {
    let Some(hosts) = hosts.filter(|hosts| !hosts.is_empty()) else { return order_projects(projects) };
    let mut groups: indexmap::IndexMap<Option<String>, Vec<Project>> = indexmap::IndexMap::new();
    groups.insert(None, Vec::new());
    for project in projects {
        groups.entry(hosts.get(&project.id).map(|host| host.id.clone())).or_default().push(project.clone());
    }
    groups.values().flat_map(|group| order_projects(group)).collect()
}

/// Every list the sidebar draws, in the order it draws them.
pub struct SidebarLists<'a> {
    pub projects: Vec<Project>,
    pub ordered_threads: Vec<&'a Thread>,
    pub threads_by_coordinator: Map<Vec<&'a Thread>>,
    pub waiting_lines: HashMap<String, String>,
    pub activity_threads: ActivitySections<'a>,
    pub activity_project: Option<String>,
    /// The threads Mod+1 through Mod+9 reach, in the order they are drawn.
    pub thread_slots: Vec<String>,
}

pub fn sidebar_lists<'a>(
    state: &WorkspaceState,
    projects: &[Project],
    visible_threads: &[&'a Thread],
    busy: &HashSet<String>,
    blocked: &HashSet<String>,
    project_hosts: Option<&HashMap<String, ThreadHost>>,
) -> SidebarLists<'a> {
    let ordered_threads = order_refs(visible_threads);
    let members = coordinated_ids(visible_threads);
    let by_coordinator = threads_by_coordinator(&ordered_threads);
    let listed: Vec<Project> = projects.iter().filter(|project| !project.unlisted).cloned().collect();
    let ordered = order_sidebar_projects(&listed, project_hosts);
    let activity_project = state.activity_project.clone().filter(|id| ordered.iter().any(|project| &project.id == id));
    let ranked = coordination_sections(visible_threads, busy, blocked, &state.starting_proposals);
    let mut waiting_lines = HashMap::new();
    for thread in visible_threads {
        let members_of: Vec<&Thread> = if members.contains(&thread.id) { Vec::new() } else { by_coordinator.get(&thread.id).cloned().unwrap_or_default() };
        if let Some(line) = waiting_line(&waiting_on_user(thread, &members_of, &state.starting_proposals), "you") {
            waiting_lines.insert(thread.id.clone(), line);
        }
    }
    let activity_threads = match &activity_project {
        Some(project) => project_activity(&ranked, visible_threads, project),
        None => ranked,
    };
    let thread_slots = slot_thread_ids(&state.sections, &activity_threads, SLOT_COUNT);
    SidebarLists { projects: ordered, ordered_threads, threads_by_coordinator: by_coordinator, waiting_lines, activity_threads, activity_project, thread_slots }
}

/// The threads Mod+1 through Mod+9 reach, in the sidebar's visible order.
pub fn thread_slots(state: &WorkspaceState) -> Vec<String> {
    let activity = thread_activity(state);
    let lists = thread_lists(state);
    sidebar_lists(state, &state.projects, &lists.visible_threads, &activity.ranked, &activity.blocked, None).thread_slots
}
