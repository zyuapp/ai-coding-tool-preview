//! The sidebar's foldable lists, and the pages Settings draws.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarSection {
    Priority,
    Running,
    Threads,
    Snoozed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarSections {
    pub priority: bool,
    pub running: bool,
    pub threads: bool,
    pub snoozed: bool,
}

impl Default for SidebarSections {
    fn default() -> Self {
        SidebarSections { priority: true, running: true, threads: true, snoozed: true }
    }
}

impl SidebarSections {
    pub fn get(&self, section: SidebarSection) -> bool {
        match section {
            SidebarSection::Priority => self.priority,
            SidebarSection::Running => self.running,
            SidebarSection::Threads => self.threads,
            SidebarSection::Snoozed => self.snoozed,
        }
    }

    pub fn set(&mut self, section: SidebarSection, open: bool) {
        match section {
            SidebarSection::Priority => self.priority = open,
            SidebarSection::Running => self.running = open,
            SidebarSection::Threads => self.threads = open,
            SidebarSection::Snoozed => self.snoozed = open,
        }
    }
}

/// Every page Settings draws, in the order its sidebar lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettingsSection {
    General,
    Appearance,
    Usage,
    Engines,
    Projects,
    Worktrees,
    Shortcuts,
    ComputerUse,
    Browser,
    Phone,
    Archive,
}

pub const SETTINGS_SECTIONS: [SettingsSection; 11] = [
    SettingsSection::General,
    SettingsSection::Appearance,
    SettingsSection::Usage,
    SettingsSection::Engines,
    SettingsSection::Projects,
    SettingsSection::Worktrees,
    SettingsSection::Shortcuts,
    SettingsSection::ComputerUse,
    SettingsSection::Browser,
    SettingsSection::Phone,
    SettingsSection::Archive,
];
