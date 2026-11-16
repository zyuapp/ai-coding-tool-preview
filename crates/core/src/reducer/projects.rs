//! The sidebar's folders: opening, adding, naming, moving and removing them.

use crate::commands::{AppCommand, ProjectAddKey};
use crate::domain::platform::{ComputerStatus, WorkspaceRecord};
use crate::domain::project::{legacy_project_id, same_root, Project};
use crate::domain::sidebar::SettingsSection;
use crate::effects::EffectWork;
use crate::events::{CommandResult, WorkspaceEvent};
use crate::reducer::errors::{PROJECT_WORKTREES_ERROR, RUNNING_PROJECT_ERROR};
use crate::reducer::threads::new_thread;
use crate::reducer::Step;
use crate::state::{ProjectAddState, ProjectEdit, WorkspaceState};
use crate::util::now;
use crate::view::sidebar::{name_project, next_project_sort_index};

/// A folder just opened, listed under the project it already is or a new one. Returns its id.
pub fn opened_in(state: &mut WorkspaceState, workspace: &WorkspaceRecord, unlisted: bool) -> String {
    let legacy = legacy_project_id(&workspace.root);
    let existing = state.projects.iter().position(|project| project.id == legacy || same_root(&project.root, &workspace.root));
    match existing {
        Some(index) => {
            let project = &mut state.projects[index];
            let keep_unlisted = unlisted && project.unlisted;
            project.root = workspace.root.clone();
            project.workspace_id = Some(workspace.id.clone());
            project.unlisted = keep_unlisted;
            project.id.clone()
        }
        None => {
            let sort_index = next_project_sort_index(&state.projects);
            state.projects.insert(0, Project { id: legacy.clone(), root: workspace.root.clone(), name: None, workspace_id: Some(workspace.id.clone()), sort_index: Some(sort_index), unlisted });
            legacy
        }
    }
}

/// A folder just opened: the window leaves any thread for a draft there.
pub fn project_opened(state: &mut WorkspaceState, workspace: &WorkspaceRecord, unlisted: bool) -> Step {
    let id = opened_in(state, workspace, unlisted);
    state.current_id = None;
    state.draft_project_id = Some(id);
    state.last_folder = Some(workspace.root.clone());
    state.action_error = None;
    Step::none()
}

fn start_in(state: &mut WorkspaceState, workspace: &WorkspaceRecord) -> Step {
    let opened = project_opened(state, workspace, true);
    let project = state.draft_project_id.clone();
    opened.then(new_thread(state, project.as_deref(), None))
}

fn changed(state: &mut WorkspaceState, mut add: ProjectAddState, query: bool) -> Step {
    let request = state.project_add_sequence + 1;
    add.request = request;
    add.suggestions.clear();
    add.selected = -1;
    add.error = None;
    let effect = (query && !add.root.is_empty()).then(|| EffectWork::ProjectAddDirectories { prefix: add.root.clone(), computer_id: add.computer_id.clone(), request });
    state.project_add_sequence = request;
    state.project_add = Some(add);
    match effect {
        Some(effect) => Step::effect(effect),
        None => Step::none(),
    }
}

fn submit(state: &mut WorkspaceState) -> Step {
    let Some(add) = state.project_add.clone() else { return Step::none() };
    if add.saving || add.root.trim().is_empty() {
        return Step::none();
    }
    let root = add.root.trim().to_string();
    let computer_id = (add.computer_id != "this").then(|| add.computer_id.clone());
    let route = crate::reducer::computers::route_input(state, &AppCommand::ProjectAdd { root: root.clone(), computer_id: computer_id.clone(), start: add.start }.into());
    let forward = match route {
        crate::reducer::computers::InputRoute::Refuse { message, .. } => {
            state.project_add = Some(ProjectAddState { error: Some(message), ..add });
            return Step::none();
        }
        crate::reducer::computers::InputRoute::Computer { computer, inputs, .. } => Some((computer, inputs)),
        crate::reducer::computers::InputRoute::Local if computer_id.is_some() => {
            state.project_add = Some(ProjectAddState { error: Some("That computer is no longer paired.".into()), ..add });
            return Step::none();
        }
        crate::reducer::computers::InputRoute::Local => None,
    };
    let request = state.project_add_sequence + 1;
    state.project_add_sequence = request;
    state.project_add = Some(ProjectAddState { request, saving: true, error: None, suggestions: Vec::new(), selected: -1, ..add.clone() });
    match forward {
        Some((id, inputs)) => Step::effect(EffectWork::ComputerForward { id, inputs, project_add_request: Some(request), draft: None, proposal: None, silent: false }),
        None => Step::effect(EffectWork::AddProject { root, request: Some(request), start: add.start }),
    }
}

pub fn reduce_project_command(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    let add = state.project_add.clone();
    match command {
        AppCommand::ProjectOpen { start } => {
            let filter_own = state.computers.filter == "all" || state.computers.filter == "this";
            if !start && filter_own && !state.computers.paired.iter().any(|computer| computer.link.status == ComputerStatus::Connected) {
                return Step::effect(EffectWork::PickProject);
            }
            let computer_id = if state.computers.filter == "all" { "this".to_string() } else { state.computers.filter.clone() };
            changed(state, ProjectAddState { request: 0, computer_id, root: String::new(), suggestions: Vec::new(), selected: -1, saving: false, error: None, start: *start }, false)
        }
        AppCommand::ProjectAdd { root, start, .. } => Step::effect(EffectWork::AddProject { root: root.clone(), request: None, start: *start }),
        AppCommand::AddProjectClose => {
            state.project_add = None;
            Step::none()
        }
        AppCommand::AddProjectDevice { computer_id } => {
            let Some(add) = add.filter(|add| !add.saving) else { return Step::none() };
            if computer_id != "this" && !state.computers.paired.iter().any(|computer| &computer.link.id == computer_id && computer.link.status == ComputerStatus::Connected) {
                return Step::none();
            }
            changed(state, ProjectAddState { computer_id: computer_id.clone(), root: String::new(), ..add }, false)
        }
        AppCommand::AddProjectPath { root } => match add.filter(|add| !add.saving) {
            Some(add) => changed(state, ProjectAddState { root: root.clone(), ..add }, true),
            None => Step::none(),
        },
        AppCommand::AddProjectPick => match add.filter(|add| !add.saving && add.computer_id == "this") {
            Some(add) => Step::effect(EffectWork::ProjectAddPick { request: add.request }),
            None => Step::none(),
        },
        AppCommand::AddProjectSubmit => submit(state),
        AppCommand::AddProjectAccept { index } => {
            let Some(add) = add.filter(|add| !add.saving) else { return Step::none() };
            let Some(root) = usize::try_from(*index).ok().and_then(|index| add.suggestions.get(index).cloned()) else { return Step::none() };
            changed(state, ProjectAddState { root, ..add }, false)
        }
        AppCommand::AddProjectKey { key } => {
            let Some(mut add) = add else { return Step::none() };
            if *key == ProjectAddKey::Escape {
                if add.suggestions.is_empty() {
                    state.project_add = None;
                    return Step::none();
                }
                return changed(state, add, false);
            }
            if add.saving {
                return Step::none();
            }
            if matches!(key, ProjectAddKey::ArrowDown | ProjectAddKey::ArrowUp) {
                let length = add.suggestions.len() as i64;
                if length == 0 {
                    return Step::none();
                }
                let down = *key == ProjectAddKey::ArrowDown;
                add.selected = if add.selected < 0 { if down { 0 } else { length - 1 } } else { (add.selected + if down { 1 } else { length - 1 }) % length };
                state.project_add = Some(add);
                return Step::none();
            }
            let typed = *key == ProjectAddKey::Enter && add.selected < 0 && add.suggestions.iter().any(|root| same_root(root, &add.root));
            if !add.suggestions.is_empty() && !typed {
                return reduce_project_command(state, &AppCommand::AddProjectAccept { index: add.selected.max(0) });
            }
            if *key == ProjectAddKey::Enter {
                submit(state)
            } else {
                Step::none()
            }
        }
        AppCommand::EditProject { project_id } => {
            match project_id.as_ref().filter(|id| !id.is_empty()) {
                None => state.project_edit = None,
                Some(project_id) => {
                    if state.project(project_id).is_some() {
                        state.project_edit = Some(ProjectEdit { project_id: project_id.clone(), name: None, saving: false, error: None });
                        state.open_menu = None;
                    }
                }
            }
            Step::none()
        }
        AppCommand::ProjectEdit { project_id, name, root } => {
            let Some(project) = state.project(project_id).cloned() else { return Step::none() };
            let root = root.as_deref().map(str::trim).filter(|root| !root.is_empty() && !same_root(root, &project.root)).map(String::from);
            match root {
                None => {
                    name_project(&mut state.projects, &project.id, name.as_ref().map(|name| name.as_deref()));
                    state.project_edit = None;
                    state.open_menu = None;
                    state.action_error = None;
                    Step::none()
                }
                Some(root) => {
                    state.project_edit = Some(ProjectEdit { project_id: project.id.clone(), name: name.clone(), saving: true, error: None });
                    Step::effect(EffectWork::RegisterProject { project_id: project.id, root })
                }
            }
        }
        AppCommand::ProjectRemove { project_id } => {
            if state.threads.iter().any(|thread| thread.project_id.as_ref() == Some(project_id) && state.active_runs.contains_key(&thread.id)) {
                state.action_error_page = Some(SettingsSection::Projects);
                return Step::refuse(RUNNING_PROJECT_ERROR);
            }
            if state.worktrees.iter().any(|worktree| &worktree.project_id == project_id) {
                state.action_error_page = Some(SettingsSection::Projects);
                return Step::refuse(PROJECT_WORKTREES_ERROR);
            }
            let leaving: Vec<String> = state.threads.iter().filter(|thread| thread.project_id.as_ref() == Some(project_id)).map(|thread| thread.id.clone()).collect();
            let effects = crate::reducer::dock::retire_automations(state, &leaving);
            let root = state.project(project_id).map(|project| project.root.clone());
            let on_screen = state.current_thread().is_some_and(|thread| thread.project_id.as_ref() == Some(project_id));
            state.projects.retain(|project| &project.id != project_id);
            if state.project_edit.as_ref().is_some_and(|edit| &edit.project_id == project_id) {
                state.project_edit = None;
            }
            for thread in state.threads.iter_mut().filter(|thread| thread.project_id.as_ref() == Some(project_id)) {
                thread.project_id = None;
                if thread.archived_at.is_none() {
                    thread.archived_at = Some(now());
                }
            }
            if on_screen {
                state.current_id = None;
            }
            if state.draft_project_id.as_ref() == Some(project_id) {
                state.draft_project_id = None;
            }
            if root.is_some() && root == state.last_folder {
                state.last_folder = None;
            }
            state.open_menu = None;
            state.action_error = None;
            state.action_error_page = None;
            Step::effects(effects)
        }
        _ => Step::none(),
    }
}

pub fn reduce_project_event(state: &mut WorkspaceState, event: &WorkspaceEvent) -> Step {
    let add = state.project_add.clone();
    match event {
        WorkspaceEvent::ProjectOpened { workspace, unlisted } => project_opened(state, workspace, *unlisted),
        WorkspaceEvent::ProjectRegistered { project_id, workspace } => {
            let Some(project) = state.project(project_id).cloned() else {
                state.project_edit = None;
                return Step::none();
            };
            if let Some(item) = state.projects.iter_mut().find(|item| item.id == project.id) {
                item.root = workspace.root.clone();
                item.workspace_id = Some(workspace.id.clone());
            }
            let pending_name = state.project_edit.as_ref().filter(|edit| edit.project_id == project.id).and_then(|edit| edit.name.clone());
            name_project(&mut state.projects, &project.id, pending_name.as_ref().map(|name| name.as_deref()));
            state.project_edit = None;
            if state.last_folder.as_deref().is_some_and(|folder| same_root(folder, &project.root)) {
                state.last_folder = Some(workspace.root.clone());
            }
            state.open_menu = None;
            state.action_error = None;
            Step::none()
        }
        WorkspaceEvent::ProjectRegisterFailed { project_id, message } => {
            let Some(edit) = state.project_edit.as_mut().filter(|edit| &edit.project_id == project_id) else { return Step::refuse(message.clone()) };
            edit.saving = false;
            edit.error = Some(message.clone());
            Step::result(CommandResult::failed(message.clone()))
        }
        WorkspaceEvent::ProjectAdded { workspace, request, start } => {
            if *start {
                match request {
                    None => return start_in(state, workspace),
                    Some(request) if add.as_ref().is_some_and(|add| add.request == *request) => {
                        state.project_add = None;
                        return start_in(state, workspace);
                    }
                    _ => return Step::none(),
                }
            }
            if request.is_some() && add.as_ref().map(|add| add.request) == *request {
                state.project_add = None;
                return project_opened(state, workspace, false);
            }
            opened_in(state, workspace, false);
            state.last_folder = Some(workspace.root.clone());
            state.action_error = None;
            Step::none()
        }
        WorkspaceEvent::ProjectAddFinished { request, error } => {
            let Some(request) = request else {
                return match error {
                    Some(error) => Step::refuse(error.clone()),
                    None => Step::none(),
                };
            };
            let Some(add) = add.filter(|add| add.request == *request) else {
                return Step::result(match error {
                    Some(error) => CommandResult::failed(error.clone()),
                    None => CommandResult::ok(),
                });
            };
            if let Some(error) = error {
                state.project_add = Some(ProjectAddState { saving: false, error: Some(error.clone()), ..add });
                return Step::result(CommandResult::failed(error.clone()));
            }
            state.project_add = None;
            let mut step = Step::result(CommandResult::ok());
            if add.start && add.computer_id != "this" {
                let left = state.computers.active.clone();
                state.computers.active = Some(add.computer_id.clone());
                state.computers.dropped = None;
                if let Some(left) = left.filter(|left| left != &add.computer_id) {
                    step.effects.push(EffectWork::ComputerForward { id: left, inputs: vec![AppCommand::SetFocused { focused: false }.into()], project_add_request: None, draft: None, proposal: None, silent: false }.into());
                }
            }
            step
        }
        WorkspaceEvent::ProjectPathPicked { request, root, error } => {
            let Some(add) = add.filter(|add| add.request == *request && !add.saving) else { return Step::none() };
            if let Some(error) = error {
                state.project_add = Some(ProjectAddState { error: Some(error.clone()), ..add });
                return Step::none();
            }
            match root {
                Some(root) => changed(state, ProjectAddState { root: root.clone(), ..add }, false),
                None => Step::none(),
            }
        }
        WorkspaceEvent::ProjectDirectories { request, directories, error } => {
            let Some(add) = add.filter(|add| add.request == *request && !add.saving) else { return Step::none() };
            state.project_add = Some(ProjectAddState { suggestions: directories.clone(), selected: -1, error: error.clone(), ..add });
            Step::none()
        }
        _ => Step::none(),
    }
}
