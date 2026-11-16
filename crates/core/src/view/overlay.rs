//! The window over a paired computer's thread: that computer's state, under this window's own
//! chrome, settings, dialogs and drafts (workspace-view-overlay.ts).

use crate::domain::find::FindTarget;
use crate::domain::platform::{SimulatorAvailability, SimulatorState};
use crate::state::WorkspaceState;

/// What the window shows while a paired computer is on screen, or None while it shows its own.
pub fn overlaid_state(window: &WorkspaceState) -> Option<WorkspaceState> {
    let remote = window.selected_computer()?.state.as_deref()?;
    let mut shown = remote.clone();
    own(&mut shown, window);
    Some(shown)
}

/// Copies what the window keeps for itself whichever computer's thread is on screen.
fn own(shown: &mut WorkspaceState, window: &WorkspaceState) {
    macro_rules! keep {
        ($($field:ident),* $(,)?) => { $(shown.$field = window.$field.clone();)* };
    }
    keep!(
        project_add, project_add_sequence, project_edit, managed_worktrees, worktree_settings, worktree_management_loading,
        worktree_management_error, worktree_management_notice, worktree_move, history, history_index,
        // Drafts stay where they are typed, so the other computer never hears a keystroke.
        prompts, annotations, pastes, images, files, composer_focus, annotation_reveal, attachment_sends,
        reading_points, overview_folds, fork_name, archive_prompt, open_menu, sections, subagent_groups,
        theme, theme_mode, ui_font, mono_font, reading_size, terminal_size, activity_project, sidebar_open, session_panel_open,
        capture_sound, capture_focus, chrome_browser, computer_use, browser_tools, memory_protection, notifications,
        settings_open, settings_section, settings_focus, usage_computer, favorite_models, recent_projects, shortcuts,
        capturing_shortcut, desktop_shortcut_unavailable, jump, zen, browser_origins, browser_import, dictation,
        installed_apps, focused, viewing_image, viewing_gallery, cli, plan_usage, computer_use_permissions, computer_use_setup,
        automation_edits, remote, remote_checking, app_update, app_build, app_update_request, engine_upgrades_owed,
        toasts, toast_sequence, toasts_held, dismissed_toasts, computers, storage_error, hidden_threads, restored, writable,
    );
    // Terminal search runs in this window's terminal, over the output it has received.
    let terminal = |find: &Option<crate::state::FindState>| find.as_ref().is_some_and(|find| matches!(find.target, FindTarget::Terminal { .. }));
    if terminal(&window.find) {
        shown.find = window.find.clone();
        shown.find_results = window.find_results.clone();
    } else if terminal(&shown.find) {
        shown.find = None;
        shown.find_results = None;
    }
    // Browser pages and simulators belong to the native window of the computer running them.
    for dock in shown.docks.values_mut() {
        dock.browser_tabs.clear();
        dock.browser_tab_id = None;
        dock.simulator_id = None;
    }
    shown.browser_approval = None;
    shown.simulator_device = None;
    shown.simulator = SimulatorState { availability: SimulatorAvailability::Unsupported { reason: "The simulator only runs on the computer you are using.".into() }, ..SimulatorState::default() };
    if window.action_error.is_some() {
        shown.action_error = window.action_error.clone();
        shown.action_error_page = window.action_error_page;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::platform::{ComputerLink, ComputerStatus};
    use crate::state::PairedComputer;

    #[test]
    fn a_paired_computers_thread_shows_under_this_windows_drafts_and_settings() {
        let mut remote = WorkspaceState::default();
        remote.current_id = Some("far".into());
        remote.prompts.insert("far".into(), "typed there".into());
        let mut window = WorkspaceState::default();
        window.current_id = Some("near".into());
        window.prompts.insert("far".into(), "typed here".into());
        window.sidebar_open = false;
        window.computers.active = Some("studio".into());
        let link = ComputerLink { id: "studio".into(), name: "Studio".into(), host: "studio.local".into(), status: ComputerStatus::Connected, error: None, paired_at: 1, capabilities: None };
        window.computers.paired = vec![PairedComputer { link, state: Some(Box::new(remote)) }];
        let shown = overlaid_state(&window).expect("the paired computer is on screen");
        assert_eq!(shown.current_id.as_deref(), Some("far"));
        assert_eq!(shown.prompts.get("far").map(String::as_str), Some("typed here"));
        assert!(!shown.sidebar_open);
        window.computers.active = None;
        assert!(overlaid_state(&window).is_none());
    }
}
