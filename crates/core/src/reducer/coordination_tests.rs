use crate::commands::AppCommand;
use crate::domain::coordination::{CoordinationNote, CoordinationState, DecisionRequest, HeldLead, MemoryChange, ProposalPlace, ThreadProposal};
use crate::domain::engine::AgentEngine;
use crate::domain::platform::{ComputerLink, ComputerStatus};
use crate::domain::run::{ExecutionPolicy, RunStatus};
use crate::domain::thread::Thread;
use crate::domain::thread_role::ThreadRole;
use crate::domain::usage_limit::{LimitPause, LimitWindow};
use crate::effects::{EffectWork, RunCommand};
use crate::events::{CommandResult, CoordinationAnswer, RunEvent, RunReport, WorkspaceEvent, WorkspaceInput};
use crate::reducer::computers::{route_input, InputRoute};
use crate::reducer::run_queue::{begin_run, RunPickup};
use crate::reducer::run_state::ATTENDED_RUN;
use crate::reducer::{reduce_transition, Transition};
use crate::state::{PairedComputer, WorkspaceState};

fn task(id: &str) -> Thread {
    let mut thread = Thread::new(id, id, AgentEngine::Claude, ExecutionPolicy::Confirm, 1);
    thread.ranked_at = 1;
    thread
}

fn lead() -> Thread {
    let mut thread = task("lead");
    thread.role = Some(ThreadRole::Coordinator);
    thread.title = "Lead".into();
    thread
}

fn member(id: &str) -> Thread {
    let mut thread = task(id);
    thread.parent_id = Some("lead".into());
    thread
}

fn note(id: &str, text: &str, urgent: bool) -> CoordinationNote {
    CoordinationNote { id: id.into(), thread_id: "one".into(), text: text.into(), at: 1, urgent, run_id: None, quiet: false }
}

fn running(state: &mut WorkspaceState, task_id: &str, run_id: &str) {
    begin_run(state, task_id, run_id, ATTENDED_RUN, None, None::<RunPickup>);
}

fn status(task_id: &str, run_id: &str, sequence: i64, status: RunStatus) -> WorkspaceEvent {
    WorkspaceEvent::RunEvent { event: RunEvent { task_id: task_id.into(), run_id: run_id.into(), sequence, report: RunReport::RunStatus { status, message: None, limit: None } } }
}

/// A free coordinator holding news while `two` still works.
fn holding(notes: Vec<CoordinationNote>) -> WorkspaceState {
    let mut state = WorkspaceState::default();
    let mut coordinator = lead();
    coordinator.coordination_notes = Some(notes);
    state.threads = vec![coordinator.into(), member("one").into(), member("two").into()];
    running(&mut state, "two", "run-2");
    state
}

fn wakes(state: &WorkspaceState) -> Vec<&crate::state::PendingRun> {
    state.pending_runs.values().filter(|pending| pending.task_id.as_deref() == Some("lead")).collect()
}

fn woken(state: &WorkspaceState, transition: &Transition) -> String {
    let runs = wakes(state);
    assert_eq!(runs.len(), 1, "the coordinator wakes once");
    let id = runs[0].id.clone();
    assert!(transition.effects.iter().any(|effect| matches!(&effect.work, EffectWork::ResolveRunWorkspace { pending_id, .. } if *pending_id == id)));
    runs[0].text.clone()
}

#[test]
fn a_coordinator_wakes_once_its_last_working_thread_stops_however_it_stops() {
    for ended in [RunStatus::Succeeded, RunStatus::Failed, RunStatus::Cancelled] {
        let mut state = holding(vec![note("n1", "\"One\" ended its turn.", false)]);
        reduce_transition(&mut state, AppCommand::SetFocused { focused: false });
        assert!(wakes(&state).is_empty(), "nothing wakes while a thread works");
        let transition = reduce_transition(&mut state, status("two", "run-2", 1, ended));
        let text = woken(&state, &transition);
        assert!(text.starts_with("\"One\" ended its turn."), "{text}");
        assert!(state.thread("lead").unwrap().coordination_notes.as_ref().is_some_and(|notes| notes.iter().any(|note| note.id == "n1")), "notes stay until the run starts");
    }
}

#[test]
fn urgent_news_wakes_a_coordinator_whose_threads_still_work_and_a_stopped_one_stays_asleep() {
    let mut state = holding(Vec::new());
    let transition = reduce_transition(&mut state, WorkspaceEvent::CoordinationReported { task_id: "one".into(), state: CoordinationState::Blocked, summary: "no key".into() });
    assert_eq!(woken(&state, &transition), "\"one\" reported blocked: no key");
    assert!(wakes(&state)[0].prompt.contains("One other thread is still working"));

    let mut state = holding(vec![note("n2", "\"One\" failed.", true)]);
    running(&mut state, "lead", "run-l");
    reduce_transition(&mut state, status("lead", "run-l", 1, RunStatus::Cancelled));
    assert!(wakes(&state).is_empty(), "stopping it keeps what it holds quiet");
    assert!(state.thread("lead").unwrap().coordination_notes.iter().flatten().all(|note| note.quiet));
}

#[test]
fn held_news_reaches_the_coordinator_when_its_last_thread_leaves_its_usage_limit_line() {
    let mut state = WorkspaceState::default();
    let mut coordinator = lead();
    coordinator.coordination_notes = Some(vec![note("n1", "\"One\" ended its turn.", false)]);
    let mut paused = member("two");
    paused.limit_pause = Some(LimitPause { resets_at: crate::util::now() + 3_600_000, window: LimitWindow::Session, paused_at: 1, nudged_at: None, workflow: false, held: None });
    state.threads = vec![coordinator.into(), member("one").into(), paused.into()];
    let transition = reduce_transition(&mut state, AppCommand::LimitCancel { task_id: "two".into() });
    let text = woken(&state, &transition);
    assert!(text.contains("\"One\" ended its turn.") && text.contains("\"two\" was stopped"), "{text}");
}

#[test]
fn a_report_is_kept_and_heard_by_its_coordinator_and_a_thread_on_its_own_is_told_why_not() {
    let mut state = WorkspaceState::default();
    state.threads = vec![lead().into(), member("worker").into(), task("alone").into()];
    running(&mut state, "worker", "run-1");
    let reported = reduce_transition(&mut state, WorkspaceEvent::CoordinationReported { task_id: "worker".into(), state: CoordinationState::Blocked, summary: "need DB credentials".into() });
    assert_eq!(reported.result, Some(CommandResult::answered(CoordinationAnswer::recorded(None))));
    let notes = state.thread("lead").unwrap().coordination_notes.clone().unwrap();
    assert_eq!(notes[0].text, "\"worker\" reported blocked: need DB credentials");
    assert!(notes[0].urgent);
    assert_eq!(notes[0].run_id.as_deref(), Some("run-1"));
    assert!(reported.effects.iter().any(|effect| matches!(&effect.work, EffectWork::AnnounceThread { notice } if notice.headline == "Blocked: need DB credentials")));
    assert!(wakes(&state).is_empty(), "news from a run still going waits for its end");

    let declined = reduce_transition(&mut state, WorkspaceEvent::CoordinationReported { task_id: "alone".into(), state: CoordinationState::Done, summary: "x".into() });
    assert!(matches!(declined.result, Some(CommandResult::Ok { answer: Some(CoordinationAnswer::Refused { .. }), .. })));
    assert!(state.thread("alone").unwrap().report.is_none());
}

#[test]
fn a_decision_closes_only_once_its_answer_is_on_its_way() {
    let mut state = WorkspaceState::default();
    state.threads = vec![lead().into(), member("worker").into()];
    reduce_transition(&mut state, WorkspaceEvent::CoordinationDecisionRaised { task_id: "worker".into(), request: DecisionRequest { question: "Ship it?".into(), context: None, options: Vec::new() }, decision_id: Some("d1".into()) });
    assert_eq!(state.thread("worker").unwrap().decisions.as_ref().unwrap().len(), 1);
    assert!(state.thread("lead").unwrap().coordination_notes.as_ref().unwrap()[0].text.contains("asked the user to decide: Ship it?"));
    let answered = reduce_transition(&mut state, AppCommand::DecisionAnswer { task_id: "worker".into(), decision_id: "d1".into(), answer: "  yes ".into() });
    assert!(answered.result.as_ref().is_none_or(CommandResult::is_ok));
    let decision = &state.thread("worker").unwrap().decisions.as_ref().unwrap()[0];
    assert_eq!(decision.answer.as_deref(), Some("yes"));
    let pending = state.pending_runs.values().find(|pending| pending.task_id.as_deref() == Some("worker")).expect("the answer is sent to the thread");
    assert_eq!(pending.text, "The user decided \"Ship it?\": yes");
    assert_eq!(pending.answers.as_deref(), Some("d1"));
}

#[test]
fn memory_changes_from_the_user_are_unread_and_the_coordinators_own_are_answered() {
    let mut state = WorkspaceState::default();
    state.threads = vec![lead().into(), task("plain").into()];
    reduce_transition(&mut state, AppCommand::MemoryAdd { task_id: "lead".into(), text: "Use pnpm".into() });
    let memory = state.thread("lead").unwrap().memory.clone().unwrap();
    assert_eq!(memory.notes[0].text, "Use pnpm");
    let own = reduce_transition(&mut state, WorkspaceEvent::CoordinationRemembered { task_id: "lead".into(), change: MemoryChange::Remember { text: "Ship on Fridays".into() } });
    assert_eq!(own.result, Some(CommandResult::answered(CoordinationAnswer::recorded(Some("n2".into())))));
    let refused = reduce_transition(&mut state, WorkspaceEvent::CoordinationRemembered { task_id: "plain".into(), change: MemoryChange::Remember { text: "x".into() } });
    assert!(matches!(refused.result, Some(CommandResult::Ok { answer: Some(CoordinationAnswer::Refused { .. }), .. })));
    let user = reduce_transition(&mut state, AppCommand::MemoryForget { task_id: "lead".into(), note_id: "n9".into() });
    assert_eq!(user.result.as_ref().and_then(CommandResult::message), Some("No note has the ID n9."));
}

#[test]
fn stop_all_cancels_every_run_under_the_coordinator_and_is_heard_quietly() {
    let mut state = WorkspaceState::default();
    state.threads = vec![lead().into(), member("one").into(), member("two").into()];
    running(&mut state, "lead", "run-l");
    running(&mut state, "one", "run-1");
    let stopped = reduce_transition(&mut state, AppCommand::CoordinationStopAll { task_id: "lead".into() });
    let cancels: Vec<&str> = stopped.effects.iter().filter_map(|effect| match &effect.work {
        EffectWork::SendRunCommand { command: RunCommand::Cancel { task_id, .. } } => Some(task_id.as_str()),
        _ => None,
    }).collect();
    assert_eq!(cancels, vec!["lead", "one"]);
    assert_eq!(state.quiet_stops, vec!["run-l".to_string(), "run-1".to_string()]);
    reduce_transition(&mut state, status("one", "run-1", 1, RunStatus::Cancelled));
    let heard = state.thread("lead").unwrap().coordination_notes.clone().unwrap_or_default();
    assert!(heard.iter().all(|note| note.quiet), "what Stop all stopped is heard quietly");
    reduce_transition(&mut state, status("lead", "run-l", 1, RunStatus::Cancelled));
    assert!(wakes(&state).is_empty(), "nothing it stopped wakes the coordinator");
}

#[test]
fn taking_the_role_off_tells_the_agent_process_of_every_thread_whose_role_changed() {
    let mut state = WorkspaceState::default();
    state.threads = vec![lead().into(), member("one").into()];
    let changed = reduce_transition(&mut state, AppCommand::TaskSetRole { task_id: Some("lead".into()), role: None, sender: None });
    let roles: Vec<(String, Option<crate::domain::coordination::CoordinationRole>)> = changed.effects.iter().filter_map(|effect| match &effect.work {
        EffectWork::SendRunCommand { command: RunCommand::Role { task_id, coordination_role } } => Some((task_id.clone(), *coordination_role)),
        _ => None,
    }).collect();
    assert!(roles.contains(&("lead".into(), None)));
    assert!(roles.contains(&("one".into(), None)));
}

fn proposal(id: &str, title: &str) -> ThreadProposal {
    ThreadProposal { id: id.into(), title: title.into(), named: false, prompt: "Do it".into(), brief: None, place: ProposalPlace::default(), engine: None, model: None, effort: None, role: None, from: None, proposed_at: 1, error: None, carried: false }
}

#[test]
fn declined_suggestions_go_and_their_coordinator_hears_so_quietly() {
    let mut state = WorkspaceState::default();
    state.threads = vec![lead().into()];
    reduce_transition(&mut state, WorkspaceEvent::CoordinationProposed { task_id: "lead".into(), proposals: vec![proposal("p1", "Fix login"), proposal("p2", "Write docs")] });
    assert_eq!(state.thread("lead").unwrap().proposals.as_ref().unwrap().len(), 2);
    reduce_transition(&mut state, AppCommand::ProposalDismiss { task_id: "lead".into(), proposal_id: None, proposal_ids: None });
    let lead = state.thread("lead").unwrap();
    assert!(lead.proposals.is_none());
    let heard = lead.coordination_notes.as_ref().unwrap();
    assert_eq!(heard[0].text, "The user declined the suggested threads \"Fix login\" and \"Write docs\".");
    assert!(heard[0].quiet);
}

fn link(id: &str, status: ComputerStatus) -> ComputerLink {
    ComputerLink { id: id.into(), name: id.into(), host: format!("{id}.local"), status, error: None, paired_at: 1, capabilities: None }
}

fn far() -> Thread {
    let mut thread = task("far");
    thread.title = "Far work".into();
    thread.created_at = Some(10);
    thread.remote_lead = Some(HeldLead { thread_id: "lead".into(), title: "Lead".into(), computer: "This Mac".into(), proposal_id: None, away: false, back_at: None });
    thread
}

fn coordinating(holder: WorkspaceState) -> WorkspaceState {
    let mut state = WorkspaceState::default();
    state.threads = vec![lead().into()];
    state.computers.paired = vec![PairedComputer { link: link("studio", ComputerStatus::Connected), state: Some(Box::new(holder)) }];
    state
}

fn notes_of(state: &WorkspaceState) -> Vec<String> {
    state.thread("lead").and_then(|lead| lead.coordination_notes.clone()).unwrap_or_default().into_iter().map(|note| note.text).collect()
}

#[test]
fn a_thread_on_a_paired_computer_is_heard_arriving_reporting_and_ending_once_each() {
    let mut state = coordinating(WorkspaceState::default());
    let mut holder = WorkspaceState::default();
    holder.threads = vec![far().into()];
    running(&mut holder, "far", "run-far");
    reduce_transition(&mut state, WorkspaceEvent::ComputerState { id: "studio".into(), state: Box::new(holder.clone()) });
    assert_eq!(notes_of(&state), vec!["\"Far work\" started on studio."]);
    assert!(wakes(&state).is_empty(), "showing up is heard quietly");
    assert_eq!(state.thread("lead").unwrap().remote_heard.as_ref().and_then(|heard| heard.get("far")).copied(), Some(10));
    assert!(state.thread("lead").unwrap().remote_working.as_ref().is_some_and(|working| working.contains_key("far")));

    crate::util::set_fixed_now(Some(1_000));
    holder.thread_mut("far").unwrap().report = Some(crate::domain::coordination::CoordinationReport { state: CoordinationState::Done, summary: "Shipped".into(), at: 500 });
    reduce_transition(&mut state, WorkspaceEvent::ComputerState { id: "studio".into(), state: Box::new(holder.clone()) });
    reduce_transition(&mut state, WorkspaceEvent::ComputerState { id: "studio".into(), state: Box::new(holder.clone()) });
    let reports: Vec<String> = notes_of(&state).into_iter().filter(|text| text.contains("reported done")).collect();
    assert_eq!(reports, vec!["\"Far work\" reported done: Shipped".to_string()], "the same report is heard once");
    assert!(wakes(&state).is_empty(), "its run still going there holds the wake");

    holder.active_runs.clear();
    let thread = holder.thread_mut("far").unwrap();
    thread.run_ended_at = Some(600);
    thread.run_ended_as = Some(crate::domain::thread::ThreadOutcome::Finished);
    let ended = reduce_transition(&mut state, WorkspaceEvent::ComputerState { id: "studio".into(), state: Box::new(holder) });
    crate::util::set_fixed_now(None);
    assert!(notes_of(&state).iter().any(|text| text == "\"Far work\" ended its turn."));
    assert!(woken(&state, &ended).contains("\"Far work\" reported done: Shipped"));
    assert!(state.thread("lead").unwrap().remote_working.is_none(), "it is no longer kept as working");
}

#[test]
fn a_coordinator_that_loses_its_role_tells_its_threads_elsewhere_it_is_away() {
    let mut holder = WorkspaceState::default();
    holder.threads = vec![far().into()];
    let mut state = coordinating(holder);
    let changed = reduce_transition(&mut state, AppCommand::TaskSetRole { task_id: Some("lead".into()), role: None, sender: None });
    let told = changed.effects.iter().find_map(|effect| match &effect.work {
        EffectWork::ComputerForward { id, inputs, silent: true, .. } => Some((id.clone(), inputs.clone())),
        _ => None,
    });
    let (id, inputs) = told.expect("the paired computer is told");
    assert_eq!(id, "studio");
    assert_eq!(inputs, vec![WorkspaceInput::from(AppCommand::TaskSetLeadAway { task_id: "far".into(), away: true })]);
}

#[test]
fn a_command_about_a_paired_computers_thread_goes_there_and_an_offline_one_is_refused() {
    let mut holder = WorkspaceState::default();
    holder.threads = vec![task("far").into()];
    let mut state = WorkspaceState::default();
    state.computers.paired = vec![PairedComputer { link: link("studio", ComputerStatus::Connected), state: Some(Box::new(holder.clone())) }];
    let rename = AppCommand::TaskRename { task_id: "far".into(), title: "New".into() };
    assert!(matches!(route_input(&state, &rename.clone().into()), InputRoute::Computer { ref computer, also: false, .. } if computer == "studio"));
    let forwarded = reduce_transition(&mut state, rename.clone());
    assert!(forwarded.effects.iter().any(|effect| matches!(&effect.work, EffectWork::ComputerForward { id, .. } if id == "studio")));
    assert!(state.thread("far").is_none(), "nothing changes here");

    let select = reduce_transition(&mut state, AppCommand::TaskSelect { task_id: "far".into() });
    assert_eq!(state.computers.active.as_deref(), Some("studio"), "selecting puts the computer on screen");
    assert!(select.effects.iter().any(|effect| matches!(&effect.work, EffectWork::ComputerForward { inputs, .. } if inputs.first() == Some(&WorkspaceInput::from(AppCommand::SetFocused { focused: true })))));

    state.computers.paired[0].link.status = ComputerStatus::Offline;
    let refused = reduce_transition(&mut state, rename);
    assert_eq!(refused.result.as_ref().and_then(CommandResult::message), Some("studio is offline."));
}

#[test]
fn looking_again_has_each_connected_computer_check_for_updates() {
    let mut state = WorkspaceState::default();
    state.computers.paired = ["studio", "laptop"].map(|id| PairedComputer { link: link(id, ComputerStatus::Connected), state: Some(Box::new(WorkspaceState::default())) }).to_vec();
    state.computers.paired[1].link.status = ComputerStatus::Offline;
    let step = reduce_transition(&mut state, AppCommand::ComputersDiscover);
    let checked: Vec<(&str, &Vec<WorkspaceInput>)> = step.effects.iter().filter_map(|effect| match &effect.work {
        EffectWork::ComputerForward { id, inputs, .. } => Some((id.as_str(), inputs)),
        _ => None,
    }).collect();
    assert_eq!(checked.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ["studio"]);
    assert!(checked[0].1.contains(&AppCommand::AppCheckForUpdates { quiet: true }.into()));
    assert!(checked[0].1.contains(&AppCommand::EngineRead { refresh: Some(true) }.into()));
}

#[test]
fn a_project_added_on_a_paired_computer_goes_there_as_its_own() {
    let mut state = WorkspaceState::default();
    state.computers.paired = vec![PairedComputer { link: link("studio", ComputerStatus::Connected), state: Some(Box::new(WorkspaceState::default())) }];
    reduce_transition(&mut state, AppCommand::ProjectOpen { start: false });
    reduce_transition(&mut state, AppCommand::AddProjectDevice { computer_id: "studio".into() });
    reduce_transition(&mut state, AppCommand::AddProjectPath { root: "/srv/app".into() });
    let submitted = reduce_transition(&mut state, AppCommand::AddProjectSubmit);
    let sent = submitted.effects.iter().find_map(|effect| match &effect.work {
        EffectWork::ComputerForward { id, inputs, .. } if id == "studio" => Some(inputs.clone()),
        _ => None,
    });
    assert_eq!(sent, Some(vec![AppCommand::ProjectAdd { root: "/srv/app".into(), computer_id: None, start: false }.into()]), "the other computer adds it as its own");
}

#[test]
fn a_thread_on_an_engine_only_a_newer_build_knows_is_read_only_and_a_send_for_it_is_refused() {
    let mut holder = WorkspaceState::default();
    let mut far = Thread::new("g1", "Gemini work", AgentEngine::Other("gemini"), ExecutionPolicy::Confirm, 1);
    far.ranked_at = 1;
    holder.threads = vec![far.into()];
    holder.current_id = Some("g1".into());
    let mut state = WorkspaceState::default();
    state.computers.paired = vec![PairedComputer { link: link("linux", ComputerStatus::Connected), state: Some(Box::new(holder)) }];
    state.computers.active = Some("linux".into());
    let unopenable = crate::view::workspace::derive_view(&state).unopenable;
    assert_eq!(unopenable.as_deref(), Some("Runs on gemini, which this version can't open. Update to work in it here."));
    reduce_transition(&mut state, AppCommand::SetPrompt { task_id: None, prompt: "hello".into() });
    let sent = reduce_transition(&mut state, AppCommand::TaskSend(Default::default()));
    assert!(!sent.effects.iter().any(|effect| matches!(effect.work, EffectWork::ComputerForward { .. })), "nothing goes to that computer");
    assert_eq!(state.action_error, unopenable);
}
