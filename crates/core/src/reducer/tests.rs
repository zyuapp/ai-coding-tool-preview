use crate::commands::{AppCommand, TaskSend};
use crate::domain::engine::AgentEngine;
use crate::domain::platform::{WorkspaceKind, WorkspaceRecord};
use crate::domain::run::{ExecutionPolicy, RunStatus};
use crate::domain::thread::{Thread, ThreadOutcome};
use crate::effects::{EffectWork, RunCommand};
use crate::events::{RunEvent, RunReport, WorkspaceEvent};
use crate::reducer::run_queue::{begin_run, RunPickup};
use crate::reducer::run_state::ATTENDED_RUN;
use crate::reducer::{reduce_transition, Transition};
use crate::state::{dock_for, WorkspaceState};

fn task(id: &str) -> Thread {
    let mut thread = Thread::new(id, id, AgentEngine::Claude, ExecutionPolicy::Confirm, 1);
    thread.ranked_at = 1;
    thread
}

fn send() -> AppCommand {
    AppCommand::TaskSend(Box::new(TaskSend::default()))
}

fn prompt(text: &str) -> AppCommand {
    AppCommand::SetPrompt { task_id: None, prompt: text.into() }
}

fn running() -> WorkspaceState {
    let mut state = WorkspaceState::default();
    state.threads.push(task("task-a").into());
    state.current_id = Some("task-a".into());
    begin_run(&mut state, "task-a", "run-a", ATTENDED_RUN, None, None::<RunPickup>);
    state
}

fn status(task_id: &str, run_id: &str, sequence: i64, status: RunStatus) -> WorkspaceEvent {
    WorkspaceEvent::RunEvent { event: RunEvent { task_id: task_id.into(), run_id: run_id.into(), sequence, report: RunReport::RunStatus { status, message: None, limit: None } } }
}

fn projectless() -> WorkspaceRecord {
    WorkspaceRecord { id: "projectless".into(), kind: WorkspaceKind::Projectless, root: "/tmp".into() }
}

fn pending_of(transition: &Transition) -> Option<String> {
    transition.effects.iter().find_map(|effect| match &effect.work {
        EffectWork::ResolveRunWorkspace { pending_id, .. } => Some(pending_id.clone()),
        _ => None,
    })
}

fn started(transition: &Transition) -> Option<crate::effects::StartRunCommand> {
    transition.effects.iter().find_map(|effect| match &effect.work {
        EffectWork::StartRun { command } => Some((**command).clone()),
        _ => None,
    })
}

fn queue(state: &mut WorkspaceState, text: &str) {
    reduce_transition(state, prompt(text));
    reduce_transition(state, send());
}

#[test]
fn a_composer_send_waits_for_its_workspace_then_starts_and_clears_the_draft() {
    let mut state = WorkspaceState::default();
    reduce_transition(&mut state, prompt("Inspect the app"));
    let sending = reduce_transition(&mut state, send());
    let pending_id = pending_of(&sending).expect("a workspace is asked for");
    assert!(state.threads.is_empty(), "no task exists until the workspace resolves");
    let resolved = reduce_transition(&mut state, WorkspaceEvent::RunResolved { pending_id, workspace: projectless(), worktree: None });
    let command = started(&resolved).expect("the run starts");
    assert_eq!(command.prompt, "Inspect the app");
    assert_eq!(command.title, "Inspect the app");
    assert_eq!(command.workspace_id, "projectless");
    assert_eq!(state.threads[0].messages[0].text, "Inspect the app");
    assert_eq!(state.active_runs[&command.task_id].run_id, command.run_id);
    assert!(state.prompts.is_empty());
    assert!(state.pending_runs.is_empty());
    assert_eq!(state.current_id.as_deref(), Some(command.task_id.as_str()), "the send opens the thread it creates");
}

#[test]
fn a_second_send_is_ignored_while_the_first_is_still_resolving() {
    let mut state = WorkspaceState::default();
    reduce_transition(&mut state, prompt("Once"));
    reduce_transition(&mut state, send());
    let again = reduce_transition(&mut state, send());
    assert!(again.effects.is_empty());
    assert_eq!(state.pending_runs.len(), 1);
}

#[test]
fn a_message_typed_during_a_run_is_queued() {
    let mut state = running();
    reduce_transition(&mut state, prompt("Also run the tests"));
    let queued = reduce_transition(&mut state, send());
    assert!(queued.effects.is_empty(), "queueing waits for the run instead of resolving a workspace");
    assert_eq!(state.queued_messages["task-a"][0].held.text, "Also run the tests");
    assert!(state.prompts.is_empty());
    assert!(state.pending_runs.is_empty());
}

#[test]
fn steering_hands_a_queued_message_to_the_run_and_delivery_threads_it() {
    let mut state = running();
    queue(&mut state, "Check the tests too");
    let message_id = state.queued_messages["task-a"][0].held.id.clone();
    let steered = reduce_transition(&mut state, AppCommand::TaskSteerQueued { task_id: None, message_id: message_id.clone() });
    assert!(matches!(&steered.effects[0].work, EffectWork::SendRunCommand { command: RunCommand::Steer { run_id, prompt, .. } } if run_id == "run-a" && prompt == "Check the tests too"));
    assert!(reduce_transition(&mut state, AppCommand::TaskSteerQueued { task_id: None, message_id: message_id.clone() }).effects.is_empty(), "steering twice sends one command");
    reduce_transition(&mut state, AppCommand::TaskDropQueued { task_id: None, message_id: message_id.clone(), if_waiting: false });
    assert_eq!(state.queued_messages["task-a"].len(), 1, "a steered message can no longer be dropped");
    let delivered = WorkspaceEvent::RunEvent { event: RunEvent { task_id: "task-a".into(), run_id: "run-a".into(), sequence: 1, report: RunReport::QueuedDelivered { message_id } } };
    reduce_transition(&mut state, delivered);
    assert!(state.queued_messages.is_empty());
    assert_eq!(state.threads[0].messages.last().unwrap().text, "Check the tests too");
}

#[test]
fn a_finished_run_drains_its_queue_one_message_at_a_time() {
    let mut state = running();
    queue(&mut state, "Run the tests");
    queue(&mut state, "Then update the README");
    let finished = reduce_transition(&mut state, status("task-a", "run-a", 1, RunStatus::Succeeded));
    let pending_id = pending_of(&finished).expect("the drained queue asks for its workspace");
    assert_eq!(state.queued_messages["task-a"].len(), 2, "a message stays queued until its own run starts");
    let first = reduce_transition(&mut state, WorkspaceEvent::RunResolved { pending_id, workspace: projectless(), worktree: None });
    let command = started(&first).unwrap();
    assert_eq!(command.prompt, "Run the tests");
    assert_eq!(state.queued_messages["task-a"].iter().map(|message| message.held.text.as_str()).collect::<Vec<_>>(), ["Then update the README"]);
    let finished = reduce_transition(&mut state, status("task-a", &command.run_id, 1, RunStatus::Succeeded));
    let pending_id = pending_of(&finished).unwrap();
    let second = reduce_transition(&mut state, WorkspaceEvent::RunResolved { pending_id, workspace: projectless(), worktree: None });
    assert_eq!(started(&second).unwrap().prompt, "Then update the README");
    assert!(state.queued_messages.is_empty());
}

#[test]
fn stopping_a_run_hands_the_queue_back_to_the_composer() {
    let mut state = running();
    queue(&mut state, "Run the tests");
    let cancelled = reduce_transition(&mut state, status("task-a", "run-a", 1, RunStatus::Cancelled));
    assert!(pending_of(&cancelled).is_none());
    assert!(state.queued_messages.is_empty());
    assert_eq!(state.prompts.get("task-a").map(String::as_str), Some("Run the tests"));
    assert!(state.threads[0].run_ended_at.is_some());
}

#[test]
fn a_run_finishing_after_its_thread_was_archived_hands_the_queue_back() {
    let mut state = running();
    queue(&mut state, "Then update the docs");
    reduce_transition(&mut state, AppCommand::TaskArchive { task_id: "task-a".into() });
    let finished = reduce_transition(&mut state, status("task-a", "run-a", 2, RunStatus::Succeeded));
    assert!(pending_of(&finished).is_none(), "nothing runs on a thread the user archived");
    assert!(state.queued_messages.is_empty());
    assert_eq!(state.prompts.get("task-a").map(String::as_str), Some("Then update the docs"));
    assert_eq!(state.action_error, None);
}

#[test]
fn a_finished_run_leaves_a_verdict_unread_only_off_screen() {
    let mut state = running();
    state.threads.push(task("task-b").into());
    begin_run(&mut state, "task-b", "run-b", ATTENDED_RUN, None, None);
    reduce_transition(&mut state, status("task-a", "run-a", 1, RunStatus::Succeeded));
    reduce_transition(&mut state, status("task-b", "run-b", 1, RunStatus::Failed));
    let a = state.thread("task-a").unwrap();
    let b = state.thread("task-b").unwrap();
    assert_eq!(a.outcome, Some(ThreadOutcome::Finished));
    assert!(!a.outcome_unread, "the thread on screen cannot have been missed");
    assert_eq!(b.outcome, Some(ThreadOutcome::Failed));
    assert!(b.outcome_unread);
}

#[test]
fn stale_and_superseded_reports_are_dropped() {
    let mut state = running();
    let delta = |sequence: i64, run: &str| WorkspaceEvent::RunEvent { event: RunEvent { task_id: "task-a".into(), run_id: run.into(), sequence, report: RunReport::AssistantDelta { message_id: "m1".into(), text: "hi".into(), append: None, artifact: false } } };
    reduce_transition(&mut state, delta(2, "run-a"));
    reduce_transition(&mut state, delta(1, "run-a"));
    reduce_transition(&mut state, delta(3, "run-old"));
    assert_eq!(state.threads[0].messages.len(), 1);
    assert_eq!(state.threads[0].messages[0].text, "hi");
}

#[test]
fn back_and_forward_walk_visits_without_recording_them() {
    let mut state = WorkspaceState::default();
    for id in ["a", "b", "c"] {
        state.threads.push(task(id).into());
    }
    for id in ["a", "b", "c"] {
        reduce_transition(&mut state, AppCommand::TaskSelect { task_id: id.into() });
    }
    assert_eq!(state.history, ["a", "b", "c"]);
    reduce_transition(&mut state, AppCommand::GoBack);
    reduce_transition(&mut state, AppCommand::GoBack);
    assert_eq!(state.current_id.as_deref(), Some("a"));
    assert_eq!(state.history.len(), 3);
    reduce_transition(&mut state, AppCommand::GoForward);
    assert_eq!(state.current_id.as_deref(), Some("b"));
    reduce_transition(&mut state, AppCommand::TaskSelect { task_id: "a".into() });
    assert_eq!(state.history, ["a", "b", "a"], "a new visit drops the forward entries");
}

#[test]
fn escape_closes_the_nearest_layer_before_reaching_a_run() {
    let mut state = running();
    state.open_menu = Some("menu".into());
    let first = reduce_transition(&mut state, AppCommand::Escape);
    assert!(state.open_menu.is_none());
    assert!(first.effects.iter().all(|effect| !matches!(effect.work, EffectWork::SendRunCommand { .. })));
    let second = reduce_transition(&mut state, AppCommand::Escape);
    assert!(second.effects.iter().any(|effect| matches!(&effect.work, EffectWork::SendRunCommand { command: RunCommand::Cancel { run_id, .. } } if run_id == "run-a")));
}

#[test]
fn a_terminal_opens_in_the_thread_folder_and_closing_it_moves_to_its_neighbour() {
    let mut state = WorkspaceState::default();
    state.last_folder = Some("/repo".into());
    let opened = reduce_transition(&mut state, AppCommand::TerminalOpen { cwd: None });
    assert!(opened.effects.iter().any(|effect| matches!(&effect.work, EffectWork::TerminalStart { cwd, .. } if cwd == "/repo")));
    let first = dock_for(&state, crate::state::DRAFT_DOCK).terminals[0].id.clone();
    reduce_transition(&mut state, AppCommand::TerminalOpen { cwd: None });
    let dock = dock_for(&state, crate::state::DRAFT_DOCK);
    assert_eq!(dock.terminals.len(), 2);
    let second = dock.terminals[1].id.clone();
    assert_eq!(dock.tab, second);
    reduce_transition(&mut state, AppCommand::TerminalClose { terminal_id: second });
    assert_eq!(dock_for(&state, crate::state::DRAFT_DOCK).tab, first);
}

#[test]
fn settings_persist_only_real_changes() {
    let mut state = WorkspaceState::default();
    let set = reduce_transition(&mut state, AppCommand::SetTheme { theme: "nord".into() });
    assert!(set.effects.iter().any(|effect| matches!(effect.work, EffectWork::PersistPreferences { .. })));
    assert_eq!(state.theme_mode, crate::domain::appearance::ThemeMode::Dark);
    let again = reduce_transition(&mut state, AppCommand::SetTheme { theme: "nord".into() });
    assert!(again.effects.iter().all(|effect| !matches!(effect.work, EffectWork::PersistPreferences { .. })));
    let unknown = reduce_transition(&mut state, AppCommand::SetTheme { theme: "missing".into() });
    assert!(unknown.effects.is_empty());
}

#[test]
fn toasts_about_one_subject_replace_each_other_and_cap() {
    let mut state = WorkspaceState::default();
    let toast = |subject: Option<&str>| crate::domain::toast::Toast { id: 0, tone: crate::domain::toast::ToastTone::Progress, title: "t".into(), message: None, subject: subject.map(String::from), persistent: false, action: None, remember: None };
    crate::reducer::toasts::with_toast(&mut state, toast(Some("claude")), 0);
    crate::reducer::toasts::with_toast(&mut state, toast(Some("claude")), 0);
    assert_eq!(state.toasts.len(), 1);
    for _ in 0..6 {
        crate::reducer::toasts::with_toast(&mut state, toast(None), 0);
    }
    assert_eq!(state.toasts.len(), crate::domain::toast::MAX_TOASTS);
}

#[test]
fn archived_threads_beyond_retention_are_not_restored_and_cut_off_turns_end_stopped() {
    use crate::persisted::ThreadStoreData;
    let at = crate::util::now();
    let mut open = task("open");
    open.run_open = true;
    let mut old = task("old");
    old.archived_at = Some(at - crate::reducer::store::ARCHIVE_RETENTION_MS - 1);
    let data: ThreadStoreData = serde_json::from_value(serde_json::json!({
        "version": 2,
        "tasks": [serde_json::to_value(&open).unwrap(), serde_json::to_value(&old).unwrap()],
        "projects": [],
        "worktrees": [],
        "lastFolder": null,
    }))
    .expect("stored data reads");
    let mut state = WorkspaceState::default();
    reduce_transition(&mut state, WorkspaceEvent::StoreLoaded { data: Box::new(data), hidden_tasks: None, resuming: None });
    assert_eq!(state.threads.len(), 1);
    let restored = &state.threads[0];
    assert!(!restored.run_open);
    assert_eq!(restored.run_ended_as, Some(ThreadOutcome::Stopped));
    assert_eq!(state.current_id.as_deref(), Some("open"));
    assert!(state.restored);
}

#[test]
fn a_drop_skips_the_images_its_composer_already_holds() {
    let mut state = WorkspaceState::default();
    state.threads.push(task("task-a").into());
    state.current_id = Some("task-a".into());
    reduce_transition(&mut state, AppCommand::ImageAdd { task_id: None, path: "/kept/1.png".into(), label: "shot.png".into(), source: Some("/desk/shot.png".into()) });
    let dropped = |state: &mut WorkspaceState, task_id: Option<&str>| {
        let transition = reduce_transition(state, AppCommand::AttachmentsDrop { task_id: task_id.map(String::from), paths: vec!["/desk/shot.png".into()] });
        transition.effects.into_iter().find_map(|effect| match effect.work {
            EffectWork::DropAttachments { held, .. } => Some(held),
            _ => None,
        })
    };
    assert_eq!(dropped(&mut state, None), Some(vec!["/desk/shot.png".to_string()]));
    assert_eq!(dropped(&mut state, Some("chat-b")), Some(Vec::new()), "another composer holds nothing");
}
