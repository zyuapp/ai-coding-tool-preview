use crate::commands::AppCommand;
use crate::domain::engine::AgentEngine;
use crate::domain::platform::{DictationModel, SimulatorAvailability, SimulatorDevice, SimulatorDeviceState};
use crate::domain::project::Project;
use crate::domain::run::{ExecutionPolicy, RunStatus};
use crate::domain::thread::Thread;
use crate::domain::worktree::{ManagedWorktree, Worktree, WorktreeDestination, WorktreeStatus};
use crate::effects::EffectWork;
use crate::events::{CommandResult, RunEvent, RunReport, WorkspaceEvent, WorktreeSnapshotResult};
use crate::reducer::reduce_transition;
use crate::reducer::run_queue::{begin_run, RunPickup};
use crate::reducer::run_state::ATTENDED_RUN;
use crate::state::{dock_for, WorkspaceState, SIMULATOR_PANEL};

fn task(id: &str) -> Thread {
    Thread::new(id, id, AgentEngine::Claude, ExecutionPolicy::Confirm, 1)
}

fn with_task() -> WorkspaceState {
    let mut state = WorkspaceState::default();
    state.threads.push(task("task-1").into());
    state.current_id = Some("task-1".into());
    state
}

#[test]
fn a_run_is_asked_once_about_an_origin_the_user_never_visited() {
    let mut state = with_task();
    reduce_transition(&mut state, AppCommand::BrowserOpen { task_id: Some("task-1".into()), url: "https://dash.example.com/metrics".into(), tab_id: None, new_tab: None });
    let approval = state.browser_approval.clone().expect("the user is asked");
    let blank = dock_for(&state, "task-1").browser_tabs[0].clone();
    assert_eq!(blank.url, "", "the ask gets a tab of its own and loads nothing into it");
    assert_eq!(approval.tab_id.as_deref(), Some(blank.id.as_str()));
    assert_eq!(dock_for(&state, "task-1").tab, blank.id);

    let mut blocked = state.clone();
    reduce_transition(&mut blocked, AppCommand::BrowserDecide { allow: false, approval_id: approval.approval_id.clone() });
    assert!(blocked.browser_approval.is_none());
    assert!(dock_for(&blocked, "task-1").browser_tabs.is_empty(), "a tab that only carried the ask goes with it");

    reduce_transition(&mut state, AppCommand::BrowserDecide { allow: true, approval_id: approval.approval_id });
    assert_eq!(state.browser_origins, vec!["https://dash.example.com".to_string()]);
    assert_eq!(dock_for(&state, "task-1").browser_tabs[0].url, "https://dash.example.com/metrics");
    let again = reduce_transition(&mut state, AppCommand::BrowserOpen { task_id: Some("task-1".into()), url: "https://dash.example.com/other".into(), tab_id: None, new_tab: None });
    assert!(state.browser_approval.is_none(), "an allowed origin is not asked about twice");
    assert!(again.effects.iter().any(|effect| matches!(effect.work, EffectWork::BrowserNavigate { .. })));
}

#[test]
fn the_users_own_visit_opens_a_tab_and_allows_its_origin_but_a_trusted_run_allows_nothing() {
    let mut state = WorkspaceState::default();
    reduce_transition(&mut state, AppCommand::BrowserOpen { task_id: None, url: "github.com/zyuapp".into(), tab_id: None, new_tab: None });
    assert_eq!(state.browser_origins, vec!["https://github.com".to_string()]);
    let dock = dock_for(&state, "draft");
    assert!(dock.open);
    assert_eq!(dock.tab, dock.browser_tabs[0].id);

    let refused = reduce_transition(&mut state, AppCommand::BrowserOpen { task_id: None, url: "file:///etc/passwd".into(), tab_id: None, new_tab: None });
    assert!(refused.result.as_ref().and_then(CommandResult::message).is_some_and(|message| message.contains("not a page the browser can open")));

    let mut trusted = WorkspaceState::default();
    let mut thread = task("task-1");
    thread.execution_policy = ExecutionPolicy::Autonomous;
    trusted.threads.push(thread.into());
    reduce_transition(&mut trusted, AppCommand::BrowserOpen { task_id: Some("task-1".into()), url: "https://example.com".into(), tab_id: None, new_tab: None });
    assert!(trusted.browser_approval.is_none());
    assert!(trusted.browser_origins.is_empty(), "acting without asking is not the user saying yes");
}

fn device(udid: &str, state: SimulatorDeviceState) -> SimulatorDevice {
    SimulatorDevice { udid: udid.into(), name: format!("iPhone {udid}"), runtime: "iOS 19".into(), state }
}

#[test]
fn the_simulator_boots_the_device_a_dock_shows_and_lets_go_of_one_no_dock_shows() {
    let mut state = with_task();
    state.simulator.availability = SimulatorAvailability::Unsupported { reason: "The simulator needs Xcode on a Mac.".into() };
    let refused = reduce_transition(&mut state, AppCommand::SimulatorOpen { task_id: None, device: None });
    assert_eq!(refused.result.as_ref().and_then(CommandResult::message), Some("The simulator needs Xcode on a Mac."));

    state.simulator.availability = SimulatorAvailability::Available;
    state.simulator.devices = vec![device("a", SimulatorDeviceState::Shutdown)];
    let opened = reduce_transition(&mut state, AppCommand::SimulatorOpen { task_id: None, device: None });
    assert!(opened.effects.iter().any(|effect| matches!(&effect.work, EffectWork::SimulatorBoot { udid } if udid == "a") && effect.follow_up));
    assert_eq!(dock_for(&state, "task-1").simulator_id.as_deref(), Some("a"));
    let starting = reduce_transition(&mut state, AppCommand::SimulatorInput { task_id: Some("task-1".into()), udid: None, input: crate::domain::platform::SimulatorInput::Type { text: "hi".into() } });
    assert_eq!(starting.result.as_ref().and_then(CommandResult::message), Some("The simulator is still starting."));

    let closed = reduce_transition(&mut state, AppCommand::CloseDockPanel { panel: SIMULATOR_PANEL.into() });
    assert!(closed.effects.iter().any(|effect| matches!(&effect.work, EffectWork::SimulatorRelease { udid } if udid == "a")));
}

#[test]
fn dictated_words_join_the_draft_they_began_on() {
    let mut state = with_task();
    state.dictation.model = DictationModel::Ready;
    state.set_prompt("task-1", "Fix");
    let started = reduce_transition(&mut state, AppCommand::DictationStart);
    let id = match &started.effects[0].work {
        EffectWork::DictationStart { id } => id.clone(),
        other => panic!("{other:?}"),
    };
    reduce_transition(&mut state, AppCommand::DictationStop { send: None });
    let focus = state.composer_focus;
    reduce_transition(&mut state, WorkspaceEvent::DictationTranscribed { id: id.clone(), text: " the login page ".into() });
    assert_eq!(state.prompts.get("task-1").map(String::as_str), Some("Fix the login page"));
    assert!(state.dictation.session.is_none());
    assert_eq!(state.composer_focus, focus + 1, "the draft on screen takes the caret");

    reduce_transition(&mut state, AppCommand::DictationStart);
    let silent = state.dictation.session.clone().unwrap().id;
    reduce_transition(&mut state, WorkspaceEvent::DictationTranscribed { id: silent, text: "  ".into() });
    assert_eq!(state.attachment_sends.get("").and_then(|send| send.error.as_deref()), Some("Didn't catch anything."));
}

#[test]
fn a_microphone_that_fails_to_start_frees_the_hold_and_says_why() {
    let mut state = with_task();
    state.dictation.model = DictationModel::Ready;
    reduce_transition(&mut state, AppCommand::DictationStart);
    let id = state.dictation.session.clone().unwrap().id;
    reduce_transition(&mut state, AppCommand::DictationHandsFree);
    reduce_transition(&mut state, WorkspaceEvent::DictationFailed { id: "older".into(), message: "stale".into() });
    assert!(state.dictation.session.is_some(), "an earlier hold's answer leaves this one alone");
    reduce_transition(&mut state, WorkspaceEvent::DictationFailed { id, message: "The microphone did not start.".into() });
    assert!(state.dictation.session.is_none());
    assert_eq!(state.attachment_sends.get("").and_then(|send| send.error.as_deref()), Some("The microphone did not start."));
}

fn project_state() -> WorkspaceState {
    let mut state = WorkspaceState::default();
    state.projects.push(Project { id: "p".into(), root: "/repo".into(), name: None, workspace_id: Some("ws".into()), sort_index: None, unlisted: false });
    let mut thread = task("t");
    thread.project_id = Some("p".into());
    state.threads.push(thread.into());
    state.worktrees.push(Worktree { id: "w1".into(), name: Some("Login".into()), project_id: "p".into(), root: "/wt/w1".into(), workspace_id: "ws-w1".into(), base_commit: "abcdef123".into(), created_at: 1, last_used_at: 1 });
    state
}

#[test]
fn a_thread_moving_itself_goes_when_its_turn_ends_and_says_so() {
    let mut state = project_state();
    begin_run(&mut state, "t", "run-1", ATTENDED_RUN, None, None::<RunPickup>);
    let asked = reduce_transition(&mut state, AppCommand::TaskMoveWorktree { task_id: Some("t".into()), destination: WorktreeDestination::Worktree { id: "w1".into() }, after_run: true, sender: None, then: Some("Run the tests".into()) });
    assert!(asked.result.as_ref().is_none_or(CommandResult::is_ok));
    assert!(state.moves_after_run.contains_key("t"));
    assert!(state.thread("t").unwrap().worktree_id.is_none(), "it waits for the run to end");
    reduce_transition(&mut state, WorkspaceEvent::RunEvent { event: RunEvent { task_id: "t".into(), run_id: "run-1".into(), sequence: 1, report: RunReport::RunStatus { status: RunStatus::Succeeded, message: None, limit: None } } });
    let thread = state.thread("t").unwrap();
    assert_eq!(thread.worktree_id.as_deref(), Some("w1"));
    assert!(thread.messages.iter().any(|message| message.text == "Moved to Login. Existing file changes stay in the previous checkout."));
    assert!(state.moves_after_run.is_empty());
    let started = state.pending_runs.values().any(|pending| pending.task_id.as_deref() == Some("t") && pending.text == "Run the tests");
    assert!(started, "what it said it goes on with starts there");
}

#[test]
fn deleting_a_checkout_files_away_its_threads_and_keeps_their_history() {
    let mut state = project_state();
    state.thread_mut("t").unwrap().worktree_id = Some("w1".into());
    state.managed_worktrees = Some(vec![ManagedWorktree { id: "w1".into(), root: "/wt/w1".into(), repository: Some("/repo".into()), branch: None, status: WorktreeStatus { changed_files: Some(0), comparison: None } }]);
    let asked = reduce_transition(&mut state, AppCommand::WorktreeDelete { task_id: None, root: Some("/wt/w1".into()), worktree_id: None, missing_only: None });
    assert!(asked.effects.iter().any(|effect| matches!(&effect.work, EffectWork::DeleteWorktree { title, .. } if title == "w1")));
    assert_eq!(state.deleting_worktrees, vec!["/wt/w1".to_string()]);
    let snapshot = WorktreeSnapshotResult { commit: Some("0123456789".into()), short_commit: Some("0123456".into()), reference: Some("refs/aicodingtool/w1".into()) };
    reduce_transition(&mut state, WorkspaceEvent::WorktreeDeleted { worktree_id: "w1".into(), root: "/wt/w1".into(), snapshot, missing_only: None });
    assert!(state.worktrees.is_empty());
    assert!(state.deleting_worktrees.is_empty());
    assert_eq!(state.worktree_management_notice.as_deref(), Some("Deleted /wt/w1. Recover it with git show refs/aicodingtool/w1."));
    let thread = state.thread("t").unwrap();
    assert!(thread.worktree_id.is_none());
    let note = thread.messages.last().unwrap();
    assert_eq!(note.text, "Worktree deleted. Loose work was committed as 0123456 first.");
    assert_eq!(note.detail.as_deref(), Some("Recover it with git show refs/aicodingtool/w1"));
}
