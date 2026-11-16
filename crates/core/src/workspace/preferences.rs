//! Presentation choices the window remembers between launches: what is written, how a stored copy
//! is read back, and how it lands in the state.

use serde_json::Value;

use crate::contracts::preferences::ViewPreferences;
use crate::domain::appearance::{mono_font_by_id, mono_font_or_default, size_by_value, size_or_default, theme_by_id, theme_or_default, ui_font_by_id, ui_font_or_default, ThemeMode, READING_SIZE, TERMINAL_SIZE};
use crate::domain::engine::{current_model, is_model_id, saved_model_engine, AgentEngine, EngineModel};
use crate::domain::project::MAX_RECENT_PROJECTS;
use crate::domain::run::{SubagentGroup, SubagentGroups};
use crate::domain::shortcuts::{shortcut_action, shortcut_overrides, shortcut_problem, ShortcutOverrides};
use crate::domain::sidebar::{SidebarSection, SidebarSections};
use crate::domain::terminal::{is_import_site, is_import_source_id, BrowserImportMemory, MAX_IMPORT_SITES};
use crate::domain::toast::MAX_REMEMBERED_TOASTS;
use crate::state::{WorkspaceState, DRAFT_DOCK, MAX_DISCARDED_LEADS};
use crate::util::{js_len, Map};

pub const VIEW_PREFERENCES_KEY: &str = "aicodingtool.view-preferences.v1";
pub const DRAFT_PROMPTS_KEY: &str = "aicodingtool.draft-prompts.v1";

const MAX_REMEMBERED: usize = 50;

/// The preferences as the state holds them now.
pub fn view_preferences(state: &WorkspaceState) -> ViewPreferences {
    let thread_ids: std::collections::HashSet<&str> = state.threads.iter().map(|thread| thread.id.as_str()).collect();
    let mut browser_tabs: Map<Vec<String>> = Map::new();
    for (owner, dock) in &state.docks {
        if owner != DRAFT_DOCK && !thread_ids.contains(owner.as_str()) {
            continue;
        }
        let urls: Vec<String> = dock.browser_tabs.iter().map(|tab| tab.url.clone()).filter(|url| !url.is_empty()).collect();
        if !urls.is_empty() {
            browser_tabs.insert(owner.clone(), urls);
        }
    }
    ViewPreferences {
        favorite_models: Some(state.favorite_models.clone()),
        theme: state.theme.clone(),
        theme_mode: state.theme_mode,
        ui_font: state.ui_font.clone(),
        mono_font: state.mono_font.clone(),
        reading_size: state.reading_size,
        terminal_size: state.terminal_size,
        session_panel_open: state.session_panel_open,
        sidebar_open: state.sidebar_open,
        capture_sound: state.capture_sound,
        capture_focus: state.capture_focus,
        activity_project: state.activity_project.clone().filter(|id| !id.is_empty()),
        simulator_device: state.simulator_device.clone().filter(|id| !id.is_empty()),
        sections: state.sections,
        subagent_groups: state.subagent_groups,
        chrome_browser: state.chrome_browser,
        computer_use: state.computer_use,
        browser_tools: state.browser_tools,
        memory_protection: state.memory_protection,
        notifications: state.notifications,
        shortcuts: Some(state.shortcuts.clone()),
        browser_tabs: Some(browser_tabs),
        browser_origins: Some(state.browser_origins.clone()),
        browser_import: state.browser_import.last.clone(),
        dismissed_toasts: (!state.dismissed_toasts.is_empty()).then(|| state.dismissed_toasts.clone()),
        recent_projects: (!state.recent_projects.is_empty()).then(|| state.recent_projects.clone()),
        discarded_leads: (!state.computers.discarded_leads.is_empty()).then(|| state.computers.discarded_leads.clone()),
    }
}

/// Lands loaded preferences in the state, each held to what this build can still draw.
pub fn apply_view_preferences(state: &mut WorkspaceState, preferences: &ViewPreferences) {
    state.theme = theme_or_default(&preferences.theme).id.to_string();
    state.theme_mode = preferences.theme_mode;
    state.ui_font = ui_font_or_default(&preferences.ui_font).id;
    state.mono_font = mono_font_or_default(&preferences.mono_font).id;
    state.reading_size = size_or_default(&READING_SIZE, &serde_json::json!(preferences.reading_size));
    state.terminal_size = size_or_default(&TERMINAL_SIZE, &serde_json::json!(preferences.terminal_size));
    state.session_panel_open = preferences.session_panel_open;
    state.capture_sound = preferences.capture_sound;
    state.capture_focus = preferences.capture_focus;
    state.chrome_browser = preferences.chrome_browser;
    state.computer_use = preferences.computer_use;
    state.browser_tools = preferences.browser_tools;
    state.memory_protection = preferences.memory_protection;
    state.notifications = preferences.notifications;
    state.sidebar_open = preferences.sidebar_open;
    state.activity_project = preferences.activity_project.clone();
    state.simulator_device = preferences.simulator_device.clone();
    state.sections = preferences.sections;
    state.subagent_groups = preferences.subagent_groups;
    state.favorite_models = preferences.favorite_models.clone().unwrap_or_default();
    state.shortcuts = preferences.shortcuts.clone().unwrap_or_default();
    state.browser_origins = preferences.browser_origins.clone().unwrap_or_default();
    state.dismissed_toasts = preferences.dismissed_toasts.clone().unwrap_or_default();
    state.recent_projects = preferences.recent_projects.clone().unwrap_or_default();
}

fn url_list(value: &Value) -> Option<Vec<String>> {
    let items = value.as_array()?;
    Some(items.iter().filter_map(Value::as_str).filter(|url| !url.is_empty() && js_len(url) <= 8_192).map(String::from).take(MAX_REMEMBERED).collect())
}

fn favorite_models(value: Option<&Value>) -> Vec<EngineModel> {
    let Some(entries) = value.and_then(Value::as_array) else { return Vec::new() };
    let mut pinned: Map<EngineModel> = Map::new();
    for entry in entries {
        let (model, engine) = match entry {
            Value::String(model) => (Some(model.as_str()), None),
            Value::Object(saved) => (saved.get("model").and_then(Value::as_str), Some(saved.get("engine"))),
            _ => continue,
        };
        let Some(model) = model.map(current_model) else { continue };
        if !is_model_id(model) {
            continue;
        }
        let engine = match engine {
            Some(named) => named.and_then(Value::as_str).and_then(AgentEngine::parse),
            None => saved_model_engine(model),
        };
        if let Some(engine) = engine {
            pinned.insert(format!("{}\u{0}{model}", engine.as_str()), EngineModel { engine, model: model.to_string() });
        }
    }
    pinned.into_values().collect()
}

fn bindings(value: &Value) -> Option<ShortcutOverrides> {
    let stored = value.as_object()?;
    let mut overrides = ShortcutOverrides::new();
    for (action, binding) in stored {
        if shortcut_action(action).is_none() {
            continue;
        }
        match binding {
            Value::Null => {
                overrides.insert(action.clone(), None);
            }
            Value::String(binding) if shortcut_problem(binding).is_none() => {
                overrides.insert(action.clone(), Some(binding.clone()));
            }
            _ => {}
        }
    }
    Some(shortcut_overrides(&overrides))
}

/// Reads stored preferences, laying what is valid over the defaults an empty workspace has.
pub fn read_view_preferences(raw: Option<&str>) -> ViewPreferences {
    let mut preferences = view_preferences(&WorkspaceState::default());
    let Some(Value::Object(value)) = raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok()) else { return preferences };
    let get = |key: &str| value.get(key);
    preferences.favorite_models = Some(favorite_models(get("favoriteModels")));
    if let Some(theme) = get("theme").and_then(Value::as_str).filter(|id| theme_by_id(id).is_some()) {
        preferences.theme = theme.into();
    }
    if let Some(mode) = get("themeMode").and_then(|mode| serde_json::from_value::<ThemeMode>(mode.clone()).ok()) {
        preferences.theme_mode = mode;
    }
    if let Some(font) = get("uiFont").and_then(Value::as_str).filter(|id| ui_font_by_id(id).is_some()) {
        preferences.ui_font = font.into();
    }
    if let Some(font) = get("monoFont").and_then(Value::as_str).filter(|id| mono_font_by_id(id).is_some()) {
        preferences.mono_font = font.into();
    }
    if let Some(size) = get("readingSize").and_then(|size| size_by_value(&READING_SIZE, size)) {
        preferences.reading_size = size;
    }
    if let Some(size) = get("terminalSize").and_then(|size| size_by_value(&TERMINAL_SIZE, size)) {
        preferences.terminal_size = size;
    }
    let flag = |key: &str| get(key).and_then(Value::as_bool);
    if let Some(open) = flag("sessionPanelOpen") {
        preferences.session_panel_open = open;
    }
    if let Some(on) = flag("captureSound") {
        preferences.capture_sound = on;
    }
    if let Some(on) = flag("chromeBrowser") {
        preferences.chrome_browser = on;
    }
    if let Some(on) = flag("computerUse") {
        preferences.computer_use = on;
    }
    if let Some(on) = flag("memoryProtection") {
        preferences.memory_protection = on;
    }
    if let Some(on) = flag("browserTools") {
        preferences.browser_tools = on;
    }
    if let Some(on) = flag("notifications") {
        preferences.notifications = on;
    }
    if let Some(on) = flag("captureFocus") {
        preferences.capture_focus = on;
    }
    if let Some(open) = flag("sidebarOpen") {
        preferences.sidebar_open = open;
    }
    if let Some(project) = get("activityProject").and_then(Value::as_str).filter(|id| !id.is_empty()) {
        preferences.activity_project = Some(project.into());
    }
    if let Some(device) = get("simulatorDevice").and_then(Value::as_str).filter(|id| !id.is_empty()) {
        preferences.simulator_device = Some(device.into());
    }
    if let Some(stored) = get("sections").and_then(Value::as_object) {
        let mut sections = SidebarSections::default();
        for section in [SidebarSection::Priority, SidebarSection::Running, SidebarSection::Threads, SidebarSection::Snoozed] {
            let key = serde_json::to_value(section).ok().and_then(|key| key.as_str().map(String::from)).unwrap_or_default();
            if let Some(open) = stored.get(&key).and_then(Value::as_bool) {
                sections.set(section, open);
            }
        }
        preferences.sections = sections;
    }
    if let Some(stored) = get("subagentGroups").and_then(Value::as_object) {
        let mut groups = SubagentGroups::default();
        for group in [SubagentGroup::Sidebar, SubagentGroup::Working, SubagentGroup::Idle, SubagentGroup::Failed, SubagentGroup::Stopped, SubagentGroup::Completed] {
            let key = serde_json::to_value(group).ok().and_then(|key| key.as_str().map(String::from)).unwrap_or_default();
            if let Some(open) = stored.get(&key).and_then(Value::as_bool) {
                groups.set(group, open);
            }
        }
        preferences.subagent_groups = groups;
    }
    if let Some(shortcuts) = get("shortcuts").and_then(bindings) {
        preferences.shortcuts = Some(shortcuts);
    }
    if let Some(stored) = get("browserTabs").and_then(Value::as_object) {
        let mut tabs = Map::new();
        for (owner, urls) in stored.iter().take(MAX_REMEMBERED) {
            if let Some(list) = url_list(urls).filter(|list| !list.is_empty()) {
                tabs.insert(owner.clone(), list);
            }
        }
        preferences.browser_tabs = Some(tabs);
    }
    if let Some(origins) = get("browserOrigins").and_then(url_list) {
        preferences.browser_origins = Some(origins);
    }
    if let Some(memory) = get("browserImport").and_then(|memory| serde_json::from_value::<BrowserImportMemory>(memory.clone()).ok()) {
        if is_import_source_id(&memory.source_id) && !memory.sites.is_empty() && memory.sites.len() <= MAX_IMPORT_SITES && memory.sites.iter().all(|site| is_import_site(site)) {
            preferences.browser_import = Some(memory);
        }
    }
    let keys = |key: &str| -> Option<Vec<String>> { get(key).and_then(Value::as_array).map(|items| items.iter().filter_map(Value::as_str).filter(|id| !id.is_empty() && js_len(id) <= 256).map(String::from).collect()) };
    if let Some(dismissed) = keys("dismissedToasts") {
        let skip = dismissed.len().saturating_sub(MAX_REMEMBERED_TOASTS);
        preferences.dismissed_toasts = Some(dismissed.into_iter().skip(skip).collect());
    }
    if let Some(leads) = keys("discardedLeads").filter(|leads| !leads.is_empty()) {
        let skip = leads.len().saturating_sub(MAX_DISCARDED_LEADS);
        preferences.discarded_leads = Some(leads.into_iter().skip(skip).collect());
    }
    if let Some(recent) = keys("recentProjects") {
        let mut unique: Vec<String> = Vec::new();
        for id in recent {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        unique.truncate(MAX_RECENT_PROJECTS);
        if !unique.is_empty() {
            preferences.recent_projects = Some(unique);
        }
    }
    preferences
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_preferences_keep_only_what_this_build_reads() {
        let raw = r#"{"theme":"nord","themeMode":"light","readingSize":"large","favoriteModels":["gpt-6-sol",{"engine":"claude","model":"opus"}],"shortcuts":{"thread.jump":"Mod+N","run.allow":null},"recentProjects":["a","a","b"]}"#;
        let preferences = read_view_preferences(Some(raw));
        assert_eq!(preferences.theme, "nord");
        assert_eq!(preferences.theme_mode, ThemeMode::Light);
        assert_eq!(preferences.reading_size, 17.0);
        let favorites = preferences.favorite_models.unwrap();
        assert_eq!(favorites[0].model, "gpt-6.1-sol");
        assert_eq!(favorites[0].engine, AgentEngine::Codex);
        let shortcuts = preferences.shortcuts.unwrap();
        assert!(!shortcuts.contains_key("thread.jump"));
        assert_eq!(shortcuts.get("run.allow"), Some(&None));
        assert_eq!(preferences.recent_projects.unwrap(), vec!["a", "b"]);
    }
}
