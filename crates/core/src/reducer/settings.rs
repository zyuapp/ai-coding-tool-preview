//! Settings: appearance, keystrokes, and the switches that change what a run may do.

use crate::commands::AppCommand;
use crate::domain::appearance::{mono_font_by_id, size_by_value, theme_by_id, theme_for, theme_or_default, ui_font_by_id, variant_for, ThemeMode, READING_SIZE, TERMINAL_SIZE};
use crate::domain::engine::{capabilities_for, is_favorite, is_model_id, AgentEngine, EngineModel};
use crate::domain::project::remember_project as remembered;
use crate::domain::run::SubagentActivity;
use crate::domain::shortcuts::{shortcut_action, shortcut_problem, with_shortcut, ShortcutOverrides};
use crate::domain::sidebar::SettingsSection;
use crate::effects::{Effect, EffectWork};
use crate::events::DesktopShortcutUnavailable;
use crate::reducer::{take_keys, target_id, Step};
use crate::state::{dock_mut, dock_owner, SettingMark, WorkspaceState};
use crate::workspace::preferences::view_preferences;

/// Settings stop waiting for a keystroke the moment they are no longer the thing in front.
pub fn stop_capture(state: &WorkspaceState) -> Vec<Effect> {
    if state.capturing_shortcut.is_none() {
        Vec::new()
    } else {
        vec![EffectWork::CaptureShortcut { capturing: false }.into()]
    }
}

/// Settings shown on a page and the control something named, or put away.
pub fn settings_shown(state: &mut WorkspaceState, open: bool, section: Option<SettingsSection>, setting_id: Option<&str>) -> Step {
    let usage = if open && section == Some(SettingsSection::Usage) { Some(state.paired_id(state.computers.active.as_deref())) } else { None };
    let mut effects = if open { take_keys() } else { stop_capture(state) };
    if open && section == Some(SettingsSection::Worktrees) && !state.worktree_management_loading {
        effects.push(EffectWork::ListWorktrees.into());
    }
    let visit = state.settings_focus.as_ref().map(|focus| focus.visit).unwrap_or(0) + 1;
    state.settings_open = open;
    state.settings_section = if open { section } else { None };
    state.settings_focus = match (open, section, setting_id) {
        (true, Some(_), Some(id)) if !id.is_empty() => Some(SettingMark { id: id.into(), visit }),
        _ => None,
    };
    if let Some(usage) = usage {
        state.usage_computer = usage;
    }
    if !open {
        state.capturing_shortcut = None;
    }
    if open && section == Some(SettingsSection::Worktrees) {
        state.worktree_management_loading = true;
    }
    if !open {
        let bulk = state.worktree_settings.bulk.take();
        state.worktree_settings = crate::state::WorktreeSettingsState { bulk, ..Default::default() };
    }
    if !open || section.is_some() {
        state.computer_use_setup = false;
    }
    if open {
        let owner = dock_owner(state, None);
        let dock = dock_mut(state, &owner);
        dock.open = false;
        dock.expanded = false;
    }
    Step::effects(effects)
}

/// Opening a child loads its log and fills settings that older app versions did not capture.
fn inspect_subagent(state: &WorkspaceState, task_id: Option<String>, subagent_id: &str) -> Vec<Effect> {
    let Some(task_id) = task_id else { return Vec::new() };
    let Some(subagent) = state.subagents.get(&task_id).and_then(|held| held.iter().find(|candidate| candidate.id == subagent_id)) else { return Vec::new() };
    let mut effects = Vec::new();
    if subagent.activity.is_empty() {
        effects.push(EffectWork::LoadSubagentActivity { task_id: task_id.clone(), subagent_id: subagent_id.into() }.into());
    }
    let thread = state.thread(&task_id);
    if let Some(thread) = thread {
        if capabilities_for(thread.engine).subagent_metadata && (subagent.model.is_none() || subagent.effort.is_none()) {
            effects.push(EffectWork::LoadSubagentMetadata { task_id, subagent_id: subagent_id.into(), engine: thread.engine, session_id: thread.continuation.as_ref().map(|continuation| continuation.value.clone()) }.into());
        }
    }
    effects
}

fn persist_now(state: &WorkspaceState) -> Effect {
    EffectWork::PersistPreferences { preferences: Box::new(view_preferences(state)) }.into()
}

pub fn model_favorite(state: &mut WorkspaceState, engine: AgentEngine, model: &str, favorite: bool) -> Step {
    if !is_model_id(model) || is_favorite(&state.favorite_models, engine, model) == favorite {
        return Step::none();
    }
    state.favorite_models.retain(|pinned| pinned.engine != engine || pinned.model != model);
    if favorite {
        state.favorite_models.push(EngineModel { engine, model: model.into() });
    }
    Step::persisting()
}

pub fn remember_project(state: &mut WorkspaceState, project_id: &str) -> Step {
    if state.recent_projects.first().map(String::as_str) == Some(project_id) {
        return Step::none();
    }
    state.recent_projects = remembered(&state.recent_projects, project_id);
    Step::persisting()
}

fn set_shortcut(state: &mut WorkspaceState, action: &str, binding: Option<&str>) -> Step {
    if shortcut_action(action).is_none() {
        return Step::none();
    }
    if let Some(problem) = binding.and_then(shortcut_problem) {
        let effects = stop_capture(state);
        state.capturing_shortcut = None;
        return Step::effects(effects).then(Step::refuse(problem));
    }
    let shortcuts: ShortcutOverrides = with_shortcut(&state.shortcuts, action, binding);
    let stopped = stop_capture(state);
    state.shortcuts = shortcuts.clone();
    state.capturing_shortcut = None;
    state.action_error = None;
    let mut effects = vec![persist_now(state), EffectWork::ApplyShortcuts { overrides: shortcuts }.into()];
    effects.extend(stopped);
    Step::effects(effects)
}

pub fn shortcut_captured(state: &mut WorkspaceState, binding: Option<String>) -> Step {
    let Some(action) = state.capturing_shortcut.clone() else { return Step::none() };
    match binding {
        None => {
            let effects = stop_capture(state);
            state.capturing_shortcut = None;
            Step::effects(effects)
        }
        Some(binding) => set_shortcut(state, &action, Some(&binding)),
    }
}

pub fn shortcut_unavailable(state: &mut WorkspaceState, refusal: &DesktopShortcutUnavailable) -> Step {
    if state.desktop_shortcut_unavailable.as_ref().is_some_and(|current| current.binding == refusal.binding && current.message == refusal.message) {
        return Step::none();
    }
    state.desktop_shortcut_unavailable = Some(refusal.clone());
    Step::none()
}

pub fn subagent_activity_loaded(state: &mut WorkspaceState, task_id: &str, subagent_id: &str, activity: &[SubagentActivity]) -> Step {
    let Some(held) = state.subagents.get_mut(task_id) else { return Step::none() };
    let Some(subagent) = held.iter_mut().find(|subagent| subagent.id == subagent_id) else { return Step::none() };
    let stored: std::collections::HashSet<&str> = activity.iter().map(|item| item.id.as_str()).collect();
    let mut merged = activity.to_vec();
    merged.extend(subagent.activity.iter().filter(|item| !stored.contains(item.id.as_str())).cloned());
    subagent.activity = merged;
    Step::none()
}

pub fn reduce_settings(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::SetTheme { theme } => {
            let Some(chosen) = theme_by_id(theme) else { return Step::none() };
            let mode = match chosen.variant {
                crate::domain::appearance::ThemeVariant::Dark => ThemeMode::Dark,
                crate::domain::appearance::ThemeVariant::Light => ThemeMode::Light,
            };
            if state.theme == chosen.id && state.theme_mode == mode {
                return Step::none();
            }
            state.theme = chosen.id.into();
            state.theme_mode = mode;
            Step::persisting()
        }
        AppCommand::SetThemeFamily { family, system_dark } => {
            let chosen = theme_for(family, variant_for(state.theme_mode, *system_dark));
            if chosen.family != family || state.theme == chosen.id {
                return Step::none();
            }
            state.theme = chosen.id.into();
            Step::persisting()
        }
        AppCommand::SetThemeMode { mode, system_dark } => {
            let chosen = theme_for(theme_or_default(&state.theme).family, variant_for(*mode, *system_dark));
            if state.theme_mode == *mode && state.theme == chosen.id {
                return Step::none();
            }
            state.theme_mode = *mode;
            state.theme = chosen.id.into();
            Step::persisting()
        }
        AppCommand::SystemScheme { dark } => {
            if state.theme_mode != ThemeMode::Auto {
                return Step::none();
            }
            let chosen = theme_for(theme_or_default(&state.theme).family, variant_for(ThemeMode::Auto, *dark));
            if state.theme != chosen.id {
                state.theme = chosen.id.into();
            }
            Step::none()
        }
        AppCommand::SetUiFont { font } => {
            if ui_font_by_id(font).is_none() || &state.ui_font == font {
                return Step::none();
            }
            state.ui_font = font.clone();
            Step::persisting()
        }
        AppCommand::SetMonoFont { font } => {
            if mono_font_by_id(font).is_none() || &state.mono_font == font {
                return Step::none();
            }
            state.mono_font = font.clone();
            Step::persisting()
        }
        AppCommand::SetReadingSize { size } => {
            let Some(size) = size_by_value(&READING_SIZE, &serde_json::json!(size)) else { return Step::none() };
            if state.reading_size == size {
                return Step::none();
            }
            state.reading_size = size;
            Step::persisting()
        }
        AppCommand::SetTerminalSize { size } => {
            let Some(size) = size_by_value(&TERMINAL_SIZE, &serde_json::json!(size)) else { return Step::none() };
            if state.terminal_size == size {
                return Step::none();
            }
            state.terminal_size = size;
            Step::persisting()
        }
        AppCommand::SetActivityProject { project_id } => {
            if state.activity_project == *project_id {
                return Step::none();
            }
            state.activity_project = project_id.clone();
            Step::persisting()
        }
        AppCommand::SetSidebarOpen { open } => {
            if state.sidebar_open == *open {
                return Step::none();
            }
            state.sidebar_open = *open;
            Step::persisting()
        }
        AppCommand::SetUsageComputer { computer_id } => {
            let usage = state.paired_id(computer_id.as_deref());
            if usage != *computer_id || usage == state.usage_computer {
                return Step::none();
            }
            state.usage_computer = usage;
            Step::none()
        }
        AppCommand::SetSectionOpen { section, open } => {
            if state.sections.get(*section) == *open {
                return Step::none();
            }
            state.sections.set(*section, *open);
            Step::persisting()
        }
        AppCommand::SetSubagentGroup { group, open } => {
            if state.subagent_groups.get(*group) == *open {
                return Step::none();
            }
            state.subagent_groups.set(*group, *open);
            Step::persisting()
        }
        AppCommand::FocusComposer { recover } => {
            state.composer_focus += 1;
            if *recover == Some(true) && !state.focused {
                Step::none()
            } else {
                Step::effects(take_keys())
            }
        }
        AppCommand::SetShortcut { action, binding } => set_shortcut(state, action, binding.as_deref()),
        AppCommand::ResetShortcuts => {
            let stopped = stop_capture(state);
            state.shortcuts = ShortcutOverrides::new();
            state.capturing_shortcut = None;
            state.action_error = None;
            let mut effects = vec![persist_now(state), EffectWork::ApplyShortcuts { overrides: ShortcutOverrides::new() }.into()];
            effects.extend(stopped);
            Step::effects(effects)
        }
        AppCommand::CaptureShortcut { action } => {
            if action.as_deref().is_some_and(|action| shortcut_action(action).is_none()) || state.capturing_shortcut == *action {
                return Step::none();
            }
            state.capturing_shortcut = action.clone();
            state.action_error = None;
            Step::effect(EffectWork::CaptureShortcut { capturing: action.is_some() })
        }
        AppCommand::InspectSubagent { task_id, subagent_id } => Step::effects(inspect_subagent(state, target_id(state, task_id.as_deref()), subagent_id)),
        AppCommand::SetCaptureOptions { options } => {
            if options.sound == state.capture_sound && options.focus == state.capture_focus {
                return Step::none();
            }
            state.capture_sound = options.sound;
            state.capture_focus = options.focus;
            Step::effects(vec![persist_now(state), EffectWork::ApplyCaptureOptions { options: *options }.into()])
        }
        AppCommand::SetMemoryProtection { enabled } => {
            state.memory_protection = *enabled;
            Step::persisting().with(EffectWork::SendRunCommand { command: crate::effects::RunCommand::MemoryProtection { enabled: *enabled } })
        }
        AppCommand::SetChromeBrowser { enabled } | AppCommand::SetComputerUse { enabled } | AppCommand::SetBrowserTools { enabled } | AppCommand::SetNotifications { enabled } => {
            let field = match command {
                AppCommand::SetChromeBrowser { .. } => &mut state.chrome_browser,
                AppCommand::SetComputerUse { .. } => &mut state.computer_use,
                AppCommand::SetBrowserTools { .. } => &mut state.browser_tools,
                _ => &mut state.notifications,
            };
            if *field == *enabled {
                return Step::none();
            }
            *field = *enabled;
            Step::persisting()
        }
        AppCommand::SetSessionPanelOpen { open } => {
            if state.session_panel_open == *open {
                return Step::none();
            }
            state.session_panel_open = *open;
            Step::persisting()
        }
        AppCommand::SetSettingsOpen { open, section, setting_id } => settings_shown(state, *open, *section, setting_id.as_deref()),
        _ => Step::none(),
    }
}
