//! Presentation choices the window remembers between launches.

use serde::{Deserialize, Serialize};

use crate::domain::appearance::ThemeMode;
use crate::domain::engine::EngineModel;
use crate::domain::run::SubagentGroups;
use crate::domain::shortcuts::ShortcutOverrides;
use crate::domain::sidebar::SidebarSections;
use crate::domain::terminal::BrowserImportMemory;
use crate::util::Map;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favorite_models: Option<Vec<EngineModel>>,
    pub theme: String,
    pub theme_mode: ThemeMode,
    pub ui_font: String,
    pub mono_font: String,
    pub reading_size: f64,
    pub terminal_size: f64,
    pub session_panel_open: bool,
    pub sidebar_open: bool,
    pub capture_sound: bool,
    pub capture_focus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulator_device: Option<String>,
    pub sections: SidebarSections,
    pub subagent_groups: SubagentGroups,
    pub chrome_browser: bool,
    pub computer_use: bool,
    pub browser_tools: bool,
    /// Whether excessive engine memory stops its owning thread. On when absent.
    #[serde(default = "crate::util::yes")]
    pub memory_protection: bool,
    pub notifications: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcuts: Option<ShortcutOverrides>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_tabs: Option<Map<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_origins: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_import: Option<BrowserImportMemory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_toasts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_usage_warnings: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_projects: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discarded_leads: Option<Vec<String>>,
}
