//! Coordinators and their threads: who works under whom, what they report, what waits on the user,
//! and the one place a coordinator is woken.

use crate::shared::Shared;
use std::collections::{HashMap, HashSet};

use crate::commands::{AppCommand, TaskSend};
use crate::domain::coordination::{
    can_join_coordinator, coordinator_of, decisions_full, is_coordinator, open_decisions, remote_lead_of, reopened_if_stuck, thread_model_of, with_answer, with_coordination_note, with_decision, with_memory_change, works_under_coordinator,
    CoordinationNote, CoordinationRole, Decision, HeldLead, MemoryChange, ThreadModelChoice, MAX_ANSWER,
};
use crate::domain::coordination::CoordinationReport;
use crate::domain::engine::{default_effort_for, effort_for_model, engine_has_model, model_engine, model_has_effort, model_takes_effort, unknown_model};
use crate::domain::message_origin::MessageOrigin;
use crate::domain::thread::{dismissed, Thread};
use crate::domain::thread_role::ThreadRole;
use crate::domain::workflow::WorkflowStatus;
use crate::effects::{Effect, EffectWork, RunCommand};
use crate::events::{CommandResult, CoordinationAnswer, RunReport, WorkspaceEvent, WorkspaceInput};
use crate::reducer::errors::{decisions_full_error, NO_COORDINATOR, NO_MEMORY, OUTSIDE_COORDINATION};
use crate::reducer::run_queue::{drained, pending_step, process_stopped, queued_for, resolve_workspace_effect, unsent, QueueIntent};
use crate::reducer::sending::{held_by_limit, reduce_sending, SendSaid};
use crate::reducer::{target_id, Step};
use crate::state::{CoordinationSend, PendingRun, RunOrigin, WorkspaceState};
use crate::util::{locale_number, now, random_id};
use crate::workspace::activity::{thread_activity, thread_busy};
use crate::workspace::coordinated_status::{can_resolve, checkout_answer, coordinated_thread_status, next_steps};
use crate::workspace::coordination::{coordination_members, coordination_role, coordinator_roster, coordinator_work, unheard_work, waiting_line, CoordinatorRoster};
use crate::workspace::coordination_messages::{coordination_update, with_news_quieted};
use crate::workspace::location::{leaving_thread_ids, project_for, worktree_for};
use crate::workspace::member_facts::{heard_facts, MemberFact};
use crate::workspace::notices::{announced, decision_headline, NoticeReason};
use crate::workspace::outstanding_stops::{stopped_all, without_lapsed_stops, StopAllCover, StopElsewhere};

const STEP_GONE: &str = "That step is no longer offered.";
const JOIN_REFUSED: &str = "Only a coordinator can take threads under it, and a coordinator cannot work under another.";

fn recorded(note_id: Option<String>) -> Step {
    Step::result(CommandResult::answered(CoordinationAnswer::recorded(note_id)))
}

/// A thread's own request turned down: nothing changes, and the reason answers the thread.
fn declined_with(reason: impl Into<String>) -> Step {
    Step::result(CommandResult::answered(CoordinationAnswer::refused(reason)))
}

fn closes_menu(state: &mut WorkspaceState) {
    state.open_menu = None;
}

pub fn reduce_coordination(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    match input {
        WorkspaceInput::Command(command) => match command {
            AppCommand::TaskSetCoordinator { task_id, coordinator_id } => set_coordinator(state, task_id, coordinator_id.as_deref()),
            AppCommand::TaskSetLeadAway { task_id, away } => {
                let Some(thread) = state.thread(task_id) else { return Step::none() };
                let Some(held) = thread.remote_lead.clone().filter(|_| thread.parent_id.is_none()) else { return Step::none() };
                if held.away == *away {
                    return Step::none();
                }
                let at = now();
                let lead = if *away { HeldLead { away: true, ..held } } else { HeldLead { away: false, back_at: Some(at), ..held } };
                if let Some(thread) = state.thread_mut(task_id) {
                    thread.remote_lead = Some(lead);
                    thread.updated_at = at;
                }
                Step::none()
            }
            AppCommand::TaskResolve { task_id } => {
                let Some(thread) = state.thread(task_id) else { return Step::none() };
                if !works_under_coordinator(&state.threads, Some(thread)) || thread.resolved_at.is_some() {
                    return Step::none();
                }
                let activity = thread_activity(state);
                if !can_resolve(coordinated_thread_status(thread, &activity.working, &activity.blocked).status) {
                    return Step::refuse("Finish or answer what this thread is waiting on first.");
                }
                let at = now();
                if let Some(thread) = state.thread_mut(task_id) {
                    thread.resolved_at = Some(at);
                    thread.updated_at = at;
                }
                dismissed(&mut state.threads, &[task_id.clone()].into());
                Step::none()
            }
            AppCommand::TaskReopen { task_id } => {
                let Some(thread) = state.thread(task_id) else { return Step::none() };
                if !works_under_coordinator(&state.threads, Some(thread)) || thread.resolved_at.is_none() {
                    return Step::none();
                }
                if let Some(thread) = state.thread_mut(task_id) {
                    thread.resolved_at = None;
                    thread.updated_at = now();
                }
                Step::none()
            }
            AppCommand::CoordinationSetThreadModel { task_id, engine, model, effort } => set_thread_model(state, task_id, *engine, model, *effort),
            AppCommand::CoordinationSetMuted { task_id, muted } => {
                if !is_coordinator(state.thread(task_id)) || state.thread(task_id).is_some_and(|lead| lead.muted == *muted) {
                    return Step::none();
                }
                if let Some(lead) = state.thread_mut(task_id) {
                    lead.muted = *muted;
                    lead.updated_at = now();
                }
                Step::none()
            }
            AppCommand::MemorySetPurpose { task_id, purpose } => remembered(state, task_id, &MemoryChange::Purpose { purpose: purpose.clone() }, false),
            AppCommand::MemoryAdd { task_id, text } => remembered(state, task_id, &MemoryChange::Remember { text: text.clone() }, false),
            AppCommand::MemoryEdit { task_id, note_id, text } => remembered(state, task_id, &MemoryChange::Edit { note_id: note_id.clone(), text: text.clone() }, false),
            AppCommand::MemoryForget { task_id, note_id } => remembered(state, task_id, &MemoryChange::Forget { note_id: note_id.clone() }, false),
            AppCommand::DecisionAnswer { task_id, decision_id, answer } => decision_answered(state, task_id, decision_id, answer),
            AppCommand::CoordinationNextStep { task_id, step } => next_step_taken(state, task_id, *step),
            AppCommand::CoordinationStopAll { task_id } => stop_all(state, task_id),
            _ => Step::none(),
        },
        WorkspaceInput::Event(event) => match event {
            WorkspaceEvent::CoordinationRemembered { task_id, change } => remembered(state, task_id, change, true),
            WorkspaceEvent::CoordinationReported { task_id, state: reported, summary } => {
                let Some(thread) = state.thread(task_id) else { return declined_with(NO_COORDINATOR) };
                let lead = coordinator_of(&state.threads, Some(thread)).map(|lead| lead.id.clone());
                if lead.is_none() && remote_lead_of(Some(thread)).is_none() {
                    return declined_with(NO_COORDINATOR);
                }
                let at = now();
                let report = CoordinationReport { state: *reported, summary: summary.clone(), at };
                if let Some(thread) = state.thread_mut(task_id) {
                    thread.report = Some(report.clone());
                    reopened_if_stuck(thread);
                }
                let Some(lead) = lead else { return recorded(None) };
                let run_id = state.active_runs.get(task_id).map(|run| run.run_id.clone());
                let effects = facts_heard_in(state, &lead, task_id, &[MemberFact::Reported { report, run_id }], at);
                Step::effects(effects).then(recorded(None))
            }
            WorkspaceEvent::CoordinationDecisionRaised { task_id, request, decision_id } => {
                let Some(thread) = state.thread(task_id) else { return declined_with(OUTSIDE_COORDINATION) };
                let lead = if is_coordinator(Some(thread)) { Some(thread.id.clone()) } else { coordinator_of(&state.threads, Some(thread)).map(|lead| lead.id.clone()) };
                if lead.is_none() && remote_lead_of(Some(thread)).is_none() {
                    return declined_with(OUTSIDE_COORDINATION);
                }
                if decisions_full(thread) {
                    return declined_with(decisions_full_error());
                }
                let at = now();
                let decision = Decision {
                    id: decision_id.clone().unwrap_or_else(random_id),
                    question: request.question.clone(),
                    context: request.context.clone().filter(|context| !context.is_empty()),
                    options: request.options.clone(),
                    raised_at: at,
                    answer: None,
                    answered_at: None,
                };
                if let Some(thread) = state.thread_mut(task_id) {
                    with_decision(thread, decision.clone());
                }
                let Some(lead) = lead else { return recorded(None) };
                if &lead == task_id {
                    let effects = match state.thread(&lead) {
                        Some(lead) => announced(state, lead, &decision_headline(&decision), NoticeReason::Decision),
                        None => Vec::new(),
                    };
                    return Step::effects(effects).then(recorded(None));
                }
                let run_id = state.active_runs.get(task_id).map(|run| run.run_id.clone());
                let effects = facts_heard_in(state, &lead, task_id, &[MemberFact::Raised { decision, run_id }], at);
                Step::effects(effects).then(recorded(None))
            }
            _ => Step::none(),
        },
    }
}

/// A coordinator hearing what one of its threads here just did, as the state now reads.
fn facts_heard_in(state: &mut WorkspaceState, lead_id: &str, member_id: &str, facts: &[MemberFact], at: i64) -> Vec<Effect> {
    let (Some(lead), Some(member)) = (state.thread(lead_id), state.thread(member_id)) else { return Vec::new() };
    let (notes, effects) = heard_facts(state, lead, member, facts, at);
    if let Some(lead) = state.thread_mut(lead_id) {
        for note in notes {
            with_coordination_note(lead, note);
        }
    }
    effects
}

fn set_coordinator(state: &mut WorkspaceState, task_id: &str, coordinator_id: Option<&str>) -> Step {
    let Some(thread) = state.thread(task_id) else { return Step::none() };
    let Some(coordinator_id) = coordinator_id else {
        if thread.parent_id.is_none() && thread.remote_lead.is_none() {
            return Step::none();
        }
        if let Some(thread) = state.thread_mut(task_id) {
            thread.parent_id = None;
            thread.remote_lead = None;
            thread.resolved_at = None;
            thread.updated_at = now();
        }
        closes_menu(state);
        return Step::none();
    };
    let lead = state.thread(coordinator_id);
    if !can_join_coordinator(thread, lead) {
        return Step::refuse(JOIN_REFUSED);
    }
    if thread.parent_id.as_deref() == Some(coordinator_id) && thread.remote_lead.is_none() {
        return Step::none();
    }
    if let Some(thread) = state.thread_mut(task_id) {
        thread.resolved_at = None;
        thread.remote_lead = None;
        thread.parent_id = Some(coordinator_id.into());
        thread.updated_at = now();
    }
    closes_menu(state);
    Step::none()
}

fn set_thread_model(state: &mut WorkspaceState, task_id: &str, engine: Option<crate::domain::engine::AgentEngine>, model: &str, effort: Option<crate::domain::run::AgentEffort>) -> Step {
    let Some(lead) = state.thread(task_id).filter(|lead| is_coordinator(Some(lead))).cloned() else { return Step::refuse("Only a coordinator chooses what its threads run on.") };
    let catalog = state.catalog();
    let Some(engine) = engine.or_else(|| model_engine(&catalog, model, Some(lead.engine))).filter(|engine| engine_has_model(&catalog, *engine, model)) else { return Step::refuse(unknown_model(model, engine)) };
    let effort = match effort {
        Some(effort) if model_has_effort(&catalog, engine, model, effort, None) => effort,
        _ => effort_for_model(&catalog, engine, model, thread_model_of(&catalog, &lead).and_then(|choice| choice.effort).unwrap_or(default_effort_for(engine)), None),
    };
    let choice = ThreadModelChoice { engine: Some(engine), model: model.into(), effort: model_takes_effort(&catalog, engine, model).then_some(effort) };
    if lead.thread_model.as_ref().is_some_and(|held| held.engine == Some(engine) && held.model == choice.model && held.effort == choice.effort) {
        return Step::none();
    }
    if let Some(lead) = state.thread_mut(task_id) {
        lead.thread_model = Some(choice);
        lead.updated_at = now();
    }
    Step::none()
}

/// Only a coordinator keeps a memory; a change that alters nothing writes nothing.
fn remembered(state: &mut WorkspaceState, task_id: &str, change: &MemoryChange, heard: bool) -> Step {
    let Some(lead) = state.thread(task_id).filter(|lead| is_coordinator(Some(lead))).cloned() else {
        return if heard { declined_with(NO_MEMORY) } else { Step::none() };
    };
    let at = now();
    let mut changed = lead.clone();
    let note_id = match with_memory_change(&mut changed, change, at, heard) {
        Ok(note_id) => note_id,
        Err(error) => return if heard { declined_with(error) } else { Step::refuse(error) },
    };
    if changed.memory != lead.memory {
        if let Some(thread) = state.thread_mut(task_id) {
            thread.memory = changed.memory;
            thread.updated_at = at;
        }
    }
    if heard {
        recorded(note_id)
    } else {
        Step::none()
    }
}

/// The answer is recorded, then said to the thread that asked, steered into a run already going.
fn decision_answered(state: &mut WorkspaceState, task_id: &str, decision_id: &str, answer: &str) -> Step {
    let Some(thread) = state.thread(task_id) else { return Step::none() };
    let Some(decision) = open_decisions(thread).into_iter().find(|item| item.id == decision_id).cloned() else { return Step::none() };
    let answer = answer.trim();
    if answer.is_empty() {
        return Step::none();
    }
    if crate::util::js_len(answer) > MAX_ANSWER {
        return Step::refuse(format!("An answer can be at most {} characters.", locale_number(MAX_ANSWER as i64)));
    }
    if thread.archived_at.is_some() {
        if let Some(thread) = state.thread_mut(task_id) {
            with_answer(thread, &decision.id, answer, now());
        }
        return Step::none();
    }
    let send = TaskSend { task_id: Some(task_id.into()), text: Some(format!("The user decided \"{}\": {answer}", decision.question)), steer: Some(true), ..TaskSend::default() };
    let sent = reduce_sending(state, &AppCommand::TaskSend(Box::new(send)), Some(SendSaid { answers: Some(decision.id.clone()), ..SendSaid::default() }));
    if sent.failed() {
        return sent;
    }
    if let Some(thread) = state.thread_mut(task_id) {
        with_answer(thread, &decision.id, answer, now());
    }
    sent
}

/// A next step is the user's own word to the thread, but one its coordinator offered.
fn next_step_taken(state: &mut WorkspaceState, task_id: &str, step: crate::domain::coordination::NextStep) -> Step {
    let Some(thread) = state.thread(task_id) else { return Step::refuse(STEP_GONE) };
    let drawn = match coordinator_of(&state.threads, Some(thread)) {
        Some(lead) => coordination_members(state, &lead.id).iter().any(|member| member.computer.is_none() && member.thread.id == thread.id),
        None => remote_lead_of(Some(thread)).is_some() && thread.archived_at.is_none(),
    };
    if !drawn {
        return Step::refuse(STEP_GONE);
    }
    let activity = thread_activity(state);
    let status = coordinated_thread_status(thread, &activity.working, &activity.blocked).status;
    if !next_steps(thread, status, checkout_answer(state, thread)).contains(&step) {
        return Step::refuse(STEP_GONE);
    }
    let send = TaskSend { task_id: Some(task_id.into()), text: Some(step.text().into()), ..TaskSend::default() };
    reduce_sending(state, &AppCommand::TaskSend(Box::new(send)), Some(SendSaid { step: true, ..SendSaid::default() }))
}

/// Stops a coordinator and every thread under it, here and on the paired computers that can.
fn stop_all(state: &mut WorkspaceState, lead_id: &str) -> Step {
    let Some(lead) = state.thread(lead_id).filter(|lead| is_coordinator(Some(lead))) else { return Step::none() };
    let work = coordinator_work(state, lead);
    let thread_ids: Vec<String> = work.threads.iter().map(|thread| thread.id.clone()).collect();
    let cancelled: Vec<(String, String)> = thread_ids.iter().filter_map(|id| state.active_runs.get(id).map(|run| (run.task_id.clone(), run.run_id.clone()))).collect();
    let starts: Vec<PendingRun> = work.starts.iter().map(|pending| (*pending).clone()).collect();
    let session_work = work.session_work.clone();
    let cover = StopAllCover {
        cancelled: cancelled.iter().map(|(_, run_id)| run_id.clone()).collect(),
        hushed: session_work.iter().map(|work| work.task_id.clone()).collect(),
        elsewhere: work.elsewhere.iter().map(StopElsewhere::from).collect(),
        landing: work.landing.iter().filter_map(|proposal| proposal.place.computer.as_ref().map(|computer| (proposal.id.clone(), computer.id.clone()))).collect(),
    };
    let outstanding = stopped_all(state, lead_id, cover, now());
    let mut effects: Vec<Effect> = cancelled.iter().map(|(task_id, run_id)| EffectWork::SendRunCommand { command: RunCommand::Cancel { task_id: task_id.clone(), run_id: run_id.clone() } }.into()).collect();
    effects.extend(outstanding);
    let mut step = Step::effects(effects);
    for work in &session_work {
        for process_id in &work.process_ids {
            step = step.and_then(state, |state| process_stopped(state, &work.task_id, process_id));
        }
    }
    for id in &thread_ids {
        step = step.and_then(state, |state| crate::reducer::runs::pause_cancelled(state, id));
    }
    for start in &starts {
        step = step.and_then(state, |state| unsent(state, start));
        if let Some(proposal_id) = start.coordination.as_ref().and_then(|send| send.proposal_id.clone()) {
            step = step.and_then(state, |state| crate::reducer::proposals::proposal_failure(state, lead_id, &proposal_id, None));
        }
        if let Some(task_id) = &start.task_id {
            step = step.and_then(state, |state| if state.active_runs.contains_key(task_id) { Step::none() } else { drained(state, task_id, QueueIntent::HandBack) });
        }
    }
    step
}

// Reconciling coordination after every input.

/// What `reconcile_coordination` needs of a thread as it stood before an input: the thread itself,
/// shared, so taking the facts copies nothing.
#[derive(Clone, Debug)]
pub struct ThreadFacts {
    pub thread: Shared<Thread>,
    pub coordinator: bool,
    pub coordination_role: Option<CoordinationRole>,
}

impl std::ops::Deref for ThreadFacts {
    type Target = Thread;
    fn deref(&self) -> &Thread {
        &self.thread
    }
}

/// Equal when everything a reconcile reads of the thread is.
impl PartialEq for ThreadFacts {
    fn eq(&self, other: &Self) -> bool {
        let same = |a: &Thread, b: &Thread| {
            a.id == b.id && a.parent_id == b.parent_id && a.role == b.role && a.archived_at == b.archived_at && a.remote_lead == b.remote_lead && a.coordination_notes == b.coordination_notes && a.limit_pause == b.limit_pause && a.worktree_id == b.worktree_id
        };
        self.coordinator == other.coordinator && self.coordination_role == other.coordination_role && (Shared::same(&self.thread, &other.thread) || same(&self.thread, &other.thread))
    }
}

/// What a run's activity read like before an input.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivityFacts {
    pub active_runs: HashMap<String, (String, &'static str)>,
    pub pending: HashMap<String, (Option<String>, bool)>,
    pub creating: HashSet<String>,
    pub leaving: HashSet<String>,
    pub workflows: HashSet<String>,
    pub subagents: HashSet<String>,
    pub queued: HashSet<String>,
}

/// The state a reconcile compares against, taken before an input is applied.
#[derive(Clone, Debug, Default)]
pub struct CoordinationBefore {
    pub threads: Vec<ThreadFacts>,
    pub activity: ActivityFacts,
    /// The coordinator the input stopped, filed away, took the role off, or whose run could not start.
    pub stopped: Option<String>,
    pub paired: crate::reducer::coordination_remote::PairedBefore,
}

/// Each thread's coordination role, reading the coordinators once rather than once per thread.
fn coordination_roles(threads: &[Shared<Thread>]) -> Vec<Option<CoordinationRole>> {
    let leads: HashSet<&str> = threads.iter().filter(|thread| is_coordinator(Some(thread))).map(|thread| thread.id.as_str()).collect();
    threads
        .iter()
        .map(|thread| {
            if is_coordinator(Some(thread)) {
                return Some(CoordinationRole::Coordinator);
            }
            let led = !leads.is_empty() && thread.role != Some(ThreadRole::Coordinator) && thread.parent_id.as_deref().is_some_and(|parent| leads.contains(parent));
            (led || remote_lead_of(Some(thread)).is_some()).then_some(CoordinationRole::Member)
        })
        .collect()
}

fn thread_facts(state: &WorkspaceState) -> Vec<ThreadFacts> {
    let roles = coordination_roles(&state.threads);
    state
        .threads
        .iter()
        .zip(roles)
        .map(|(thread, role)| ThreadFacts { thread: thread.clone(), coordinator: role == Some(CoordinationRole::Coordinator), coordination_role: role })
        .collect()
}

fn activity_facts(state: &WorkspaceState) -> ActivityFacts {
    ActivityFacts {
        active_runs: state.active_runs.iter().map(|(id, run)| (id.clone(), (run.run_id.clone(), run.status_name()))).collect(),
        pending: state.pending_runs.iter().map(|(id, pending)| (id.clone(), (pending.task_id.clone(), pending.creating_worktree))).collect(),
        creating: state.creating_worktrees.iter().cloned().collect(),
        leaving: leaving_thread_ids(state),
        workflows: state.workflows.iter().filter(|(_, items)| items.iter().any(|workflow| workflow.status == WorkflowStatus::Running)).map(|(id, _)| id.clone()).collect(),
        subagents: state.subagents.iter().filter(|(_, items)| items.iter().any(|subagent| subagent.status == crate::domain::run::SubagentStatus::Working)).map(|(id, _)| id.clone()).collect(),
        queued: state.queued_messages.iter().filter(|(_, items)| !items.is_empty()).map(|(id, _)| id.clone()).collect(),
    }
}

/// The coordinator an input stops, files away, takes the role off, or whose run could not start.
fn stopped_by(state: &WorkspaceState, input: &WorkspaceInput) -> Option<String> {
    match input {
        WorkspaceInput::Command(AppCommand::TaskArchive { task_id }) | WorkspaceInput::Command(AppCommand::CoordinationStopAll { task_id }) => Some(task_id.clone()),
        WorkspaceInput::Command(AppCommand::TaskSetRole { task_id, role, .. }) if *role != Some(ThreadRole::Coordinator) => {
            let task_id = target_id(state, task_id.as_deref())?;
            state.thread(&task_id).filter(|lead| lead.role == Some(ThreadRole::Coordinator)).map(|lead| lead.id.clone())
        }
        WorkspaceInput::Event(WorkspaceEvent::RunEvent { event }) => match &event.report {
            RunReport::RunStatus { status: crate::domain::run::RunStatus::Cancelled, .. } => (state.active_runs.get(&event.task_id).map(|run| run.run_id.as_str()) == Some(event.run_id.as_str())).then(|| event.task_id.clone()),
            _ => None,
        },
        WorkspaceInput::Event(WorkspaceEvent::RunUnresolved { pending_id, .. }) => state.pending_runs.get(pending_id).and_then(|pending| pending.task_id.clone()),
        WorkspaceInput::Command(AppCommand::LimitCancel { task_id }) => state.thread(task_id).filter(|thread| thread.limit_pause.is_some()).map(|_| task_id.clone()),
        _ => None,
    }
}

impl CoordinationBefore {
    pub fn of(state: &WorkspaceState, input: &WorkspaceInput, paired: crate::reducer::coordination_remote::PairedBefore) -> CoordinationBefore {
        CoordinationBefore { threads: thread_facts(state), activity: activity_facts(state), stopped: stopped_by(state, input), paired }
    }
}

/// One thread's facts before and after an input, where they differ.
struct FactsChange<'a> {
    id: String,
    before: Option<&'a ThreadFacts>,
    after: Option<ThreadFacts>,
}

fn facts_changes<'a>(before: &'a [ThreadFacts], after: Vec<ThreadFacts>) -> Vec<FactsChange<'a>> {
    // Threads still in the same order, as after almost every input, compare place by place.
    if before.len() == after.len() && before.iter().zip(&after).all(|(was, is)| Shared::same(&was.thread, &is.thread) || was.id == is.id) {
        return before.iter().zip(after).filter(|(was, is)| *was != is).map(|(was, is)| FactsChange { id: is.id.clone(), before: Some(was), after: Some(is) }).collect();
    }
    let mut previous: HashMap<&str, &ThreadFacts> = before.iter().map(|facts| (facts.id.as_str(), facts)).collect();
    let mut changes = Vec::new();
    for facts in after {
        let was = previous.remove(facts.id.as_str());
        if was != Some(&facts) {
            changes.push(FactsChange { id: facts.id.clone(), before: was, after: Some(facts) });
        }
    }
    for facts in before {
        if previous.contains_key(facts.id.as_str()) {
            changes.push(FactsChange { id: facts.id.clone(), before: Some(facts), after: None });
        }
    }
    changes
}

fn symmetric(before: &HashSet<String>, after: &HashSet<String>, changed: &mut HashSet<String>) {
    changed.extend(before.symmetric_difference(after).cloned());
}

fn activity_changes(before: &ActivityFacts, after: &ActivityFacts, threads: &[FactsChange]) -> HashSet<String> {
    let mut changed = HashSet::new();
    for (id, run) in &after.active_runs {
        if before.active_runs.get(id) != Some(run) {
            changed.insert(id.clone());
        }
    }
    for id in before.active_runs.keys() {
        if !after.active_runs.contains_key(id) {
            changed.insert(id.clone());
        }
    }
    for (id, pending) in &after.pending {
        if before.pending.get(id) != Some(pending) {
            changed.extend(pending.0.clone());
            changed.extend(before.pending.get(id).and_then(|was| was.0.clone()));
        }
    }
    for (id, pending) in &before.pending {
        if !after.pending.contains_key(id) {
            changed.extend(pending.0.clone());
        }
    }
    symmetric(&before.creating, &after.creating, &mut changed);
    symmetric(&before.leaving, &after.leaving, &mut changed);
    symmetric(&before.workflows, &after.workflows, &mut changed);
    symmetric(&before.subagents, &after.subagents, &mut changed);
    symmetric(&before.queued, &after.queued, &mut changed);
    for change in threads {
        match (change.before, &change.after) {
            (Some(was), Some(is)) if was.limit_pause == is.limit_pause && was.worktree_id == is.worktree_id => {}
            _ => {
                changed.insert(change.id.clone());
            }
        }
    }
    changed
}

/// The agent process is told of each thread whose coordination role an input changed.
fn role_changes(before: &[ThreadFacts], state: &WorkspaceState, threads: &[FactsChange]) -> Vec<Effect> {
    let shaped: HashSet<&str> = threads
        .iter()
        .filter(|change| matches!((change.before, &change.after), (Some(was), Some(is)) if was.role != is.role || was.archived_at != is.archived_at || was.parent_id != is.parent_id || was.remote_lead != is.remote_lead))
        .map(|change| change.id.as_str())
        .collect();
    if shaped.is_empty() {
        return Vec::new();
    }
    let mut read: Vec<&str> = shaped.iter().copied().collect();
    for thread in &state.threads {
        if thread.parent_id.as_deref().is_some_and(|parent| shaped.contains(parent)) && !read.contains(&thread.id.as_str()) {
            read.push(&thread.id);
        }
    }
    let mut effects = Vec::new();
    for id in read {
        let (Some(was), Some(after)) = (before.iter().find(|facts| facts.id == id), state.thread(id)) else { continue };
        let role = coordination_role(&state.threads, after);
        if was.coordination_role == role {
            continue;
        }
        effects.push(EffectWork::SendRunCommand { command: RunCommand::Role { task_id: id.into(), coordination_role: role } }.into());
    }
    effects
}

/// Each touched thread that is a coordinator, and the coordinator each one worked under before or after.
fn coordinators_of(before: &[ThreadFacts], state: &WorkspaceState, touched: &HashSet<String>) -> Vec<String> {
    let mut leads: Vec<String> = Vec::new();
    let mut add = |id: &str| {
        if !leads.iter().any(|lead| lead == id) {
            leads.push(id.into());
        }
    };
    for facts in before {
        if !touched.contains(&facts.id) {
            continue;
        }
        if facts.coordinator {
            add(&facts.id);
        } else if let Some(parent) = &facts.parent_id {
            add(parent);
        }
    }
    for thread in &state.threads {
        if !touched.contains(&thread.id) {
            continue;
        }
        if is_coordinator(Some(thread)) {
            add(&thread.id);
        } else if let Some(parent) = &thread.parent_id {
            add(parent);
        }
    }
    leads
}

/// After every input, each coordinator the input could have touched hears its news if it is due.
pub fn reconcile_coordination(before: &CoordinationBefore, state: &mut WorkspaceState) -> Vec<Effect> {
    if let Some(stopped) = &before.stopped {
        with_news_quieted(state, stopped);
    }
    let remote = crate::reducer::coordination_remote::remote_news(&before.paired, state);
    let after = thread_facts(state);
    let threads = facts_changes(&before.threads, after);
    without_lapsed_stops(state);
    let mut touched = activity_changes(&before.activity, &activity_facts(state), &threads);
    for change in &threads {
        match (change.before, &change.after) {
            (Some(was), Some(is)) if was.parent_id == is.parent_id && was.role == is.role && was.archived_at == is.archived_at && was.coordination_notes == is.coordination_notes => {}
            _ => {
                touched.insert(change.id.clone());
            }
        }
    }
    touched.extend(remote.leads.iter().cloned());
    let coordinator_flipped = threads.iter().any(|change| change.before.is_some_and(|was| was.coordinator) != change.after.as_ref().is_some_and(|is| is.coordinator));
    let mut effects = remote.effects;
    effects.extend(crate::reducer::coordination_remote::released(&before.paired, state, coordinator_flipped));
    effects.extend(role_changes(&before.threads, state, &threads));
    if touched.is_empty() {
        return effects;
    }
    for lead_id in coordinators_of(&before.threads, state, &touched) {
        effects.extend(deliver_coordination_notes(state, &lead_id));
    }
    effects
}

/// The news that wakes a coordinator: notes not quiet, and not written during a run of one of its
/// threads that is still working.
fn waking_news(state: &WorkspaceState, lead: &Thread, roster: &CoordinatorRoster, unheard: &HashSet<String>) -> Vec<CoordinationNote> {
    let notes: Vec<CoordinationNote> = lead.coordination_notes.iter().flatten().filter(|note| !note.quiet).cloned().collect();
    if !notes.iter().any(|note| note.run_id.is_some()) {
        return notes;
    }
    let going = |id: &str, run_id: &str| -> bool {
        if unheard.contains(id) {
            return true;
        }
        let Some(member) = roster.members.iter().find(|member| member.member.thread.id == id) else { return false };
        let runs = match member.member.holder() {
            Some(holder) => &holder.active_runs,
            None => &state.active_runs,
        };
        runs.get(id).is_some_and(|run| run.run_id == run_id) && member.working
    };
    notes.into_iter().filter(|note| note.run_id.as_deref().is_none_or(|run_id| !going(&note.thread_id, run_id))).collect()
}

/// Wakes a free coordinator with all its waiting notes at once, once none of its threads is working
/// or the news cannot wait. The notes stay until its run actually starts.
fn deliver_coordination_notes(state: &mut WorkspaceState, lead_id: &str) -> Vec<Effect> {
    let Some(lead) = state.thread(lead_id) else { return Vec::new() };
    let notes = lead.coordination_notes.clone().unwrap_or_default();
    if !is_coordinator(Some(lead)) || notes.is_empty() || thread_busy(state, lead_id) || !queued_for(state, lead_id).is_empty() || held_by_limit(state, lead_id, now()) {
        return Vec::new();
    }
    let here = thread_activity(state);
    let roster = coordinator_roster(state, lead, &here, None);
    let unheard = unheard_work(state, lead);
    let unheard_ids: HashSet<String> = unheard.iter().map(|(thread_id, _, _)| thread_id.clone()).collect();
    let news = waking_news(state, lead, &roster, &unheard_ids);
    if news.is_empty() {
        return Vec::new();
    }
    let working = roster.members.iter().filter(|member| member.working).count() + unheard.len();
    if working > 0 && !news.iter().any(|note| note.urgent) {
        return Vec::new();
    }
    let waiting = waiting_line(&roster.waiting, "the user");
    let (text, prompt) = coordination_update(&notes, working, waiting.as_deref());
    let project = project_for(state, Some(lead)).cloned();
    let lead = lead.clone();
    let mut pending = PendingRun::new(random_id(), random_id(), RunOrigin::Composer, text, prompt);
    pending.task_id = Some(lead_id.into());
    pending.project_id = project.as_ref().map(|project| project.id.clone());
    pending.message_origin = Some(MessageOrigin::Coordination);
    pending.coordination = Some(CoordinationSend { notes: Some(notes.iter().map(|note| note.id.clone()).collect()), ..CoordinationSend::default() });
    let effect = resolve_workspace_effect(&pending.id, Some(&lead), project.as_ref(), worktree_for(state, Some(&lead)), false, None);
    let step = pending_step(state, pending);
    let mut effects = step.effects;
    effects.push(effect.into());
    effects
}
