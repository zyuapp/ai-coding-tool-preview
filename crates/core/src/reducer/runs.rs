//! A run's life: the checkout it resolves to, what it reports, and how it ends.

use crate::shared::Shared;
use crate::commands::AppCommand;
use crate::domain::conversation::{create_conversation_message, create_sent_message, MessageKind, SentMessage};
use crate::domain::coordination::{as_coordinator, is_coordinator, wants_fresh_session, with_memory_heard, with_coordination_note, without_coordination_notes, without_proposals, CoordinationNote};
use crate::domain::engine::{capabilities_for, no_model, thread_model, unknown_model};
use crate::domain::question::is_review_target;
use crate::domain::run::RunStatus;
use crate::domain::thread::{clamp_title, Thread};
use crate::domain::usage_limit::{limit_lines, resume_prompt, resumes_on_its_own};
use crate::domain::worktree::{CreatedWorktree, Worktree};
use crate::domain::platform::WorkspaceRecord;
use crate::effects::{EffectWork, RunOperation};
use crate::events::{AutomationAck, CommandResult, RunEvent, RunReport, ThreadEvent, WorkspaceEvent, WorkspaceInput};
use crate::reducer::errors::{WORKTREE_CREATING_ERROR, WORKTREE_RELEASING_ERROR};
use crate::reducer::run_queue::{
    attended_run, begin_run, cancelling, drained, pending_step, process_stopped, queued_for, resolve_workspace_effect, run_model_of, set_queued, side_channel_for, start_run_command, unsent,
    with_delivered_message, with_steering_failure, without_started_draft, QueueIntent, RunPickup,
};
use crate::reducer::run_state::{answer_part_id, apply_run_event, apply_thread_event, with_approval_answered, RunProvenance, ATTENDED_RUN};
use crate::reducer::settlement::{settle_turn, settled_after_testimony, turn_ended, turn_settled, turn_testimony, Settlement, StopIntent, TurnEnd};
use crate::reducer::{finish, follow_ups, rejected, target_id, Step, Transition};
use crate::state::{normalize_draft, thread_mark, ActiveRun, AgentDraftChange, PendingOperation, PendingRun, ReviewPicker, ReviewStep, ResumeAfter, RunOperationKind, RunOrigin, WorkspaceState, DRAFT_DOCK};
use crate::util::{now, random_id};
use crate::workspace::activity::{session_holds_work, thread_busy, thread_to_begin};
use crate::workspace::location::{leaving_thread_ids, project_for, thread_workspace_id, thread_workspace_root, worktree_by_id, worktree_for};
use crate::workspace::notices::{announced, posted_headline, settled_headline, waiting_headline_tool, NoticeReason, RunSurfacing};
use crate::workspace::order::next_sort_index;

/// How long after Stop all stopped a thread's session work the agent's own turn is still its answer.
const STOP_ANSWER_MS: i64 = 2 * 60_000;

pub fn answers_in_time(session_at: i64, at: i64) -> bool {
    at - session_at <= STOP_ANSWER_MS
}

/// The engine's compaction and review belong to a session it can act on.
pub fn compactable_session(state: &WorkspaceState, thread: &Thread) -> bool {
    if thread.resource_stop.is_some() {
        return false;
    }
    let Some(model) = thread_model(&state.catalog(), thread.engine, thread.model.as_deref()) else { return false };
    crate::domain::engine::model_supports_manual_compaction(&state.catalog(), thread.engine, &model)
        && thread.continuation.as_ref().is_some_and(|continuation| continuation.provider == thread.engine.as_str())
        && thread.context_usage.is_some()
}

/// A session reviews changes when its engine can, its engine holds it, and the thread works in a checkout.
pub fn reviewable_session(state: &WorkspaceState, thread: &Thread) -> bool {
    thread.resource_stop.is_none() && capabilities_for(thread.engine).review && thread.continuation.as_ref().is_some_and(|continuation| continuation.provider == thread.engine.as_str()) && thread_workspace_id(state, Some(thread)).is_some()
}

fn reviewable_thread(state: &WorkspaceState, task_id: Option<&str>) -> Option<Thread> {
    let thread = state.thread(task_id?)?;
    (reviewable_session(state, thread) && !thread_busy(state, &thread.id)).then(|| thread.clone())
}

fn new_pending(task_id: &str, project_id: Option<String>, operation: PendingOperation) -> PendingRun {
    let mut pending = PendingRun::new(random_id(), random_id(), RunOrigin::Composer, "", "");
    pending.task_id = Some(task_id.into());
    pending.project_id = project_id;
    pending.operation = Some(operation);
    pending
}

pub fn reduce_run_control(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    match command {
        AppCommand::ResourceResume { task_id } => crate::reducer::resource_stops::resource_resumed(state, task_id),
        AppCommand::ReviewClose => {
            state.review_picker = None;
            Step::none()
        }
        AppCommand::ReviewSetStep { step } => {
            let Some(picker) = state.review_picker.clone() else { return Step::none() };
            if Some(&picker.task_id) != state.current_id.as_ref() || reviewable_thread(state, Some(&picker.task_id)).is_none() {
                return Step::none();
            }
            state.review_picker = Some(ReviewPicker { task_id: picker.task_id, step: *step });
            Step::none()
        }
        AppCommand::ReviewOpen { task_id } => {
            let Some(thread) = reviewable_thread(state, target_id(state, task_id.as_deref()).as_deref()) else { return Step::none() };
            state.review_picker = Some(ReviewPicker { task_id: thread.id, step: ReviewStep::Targets });
            state.action_error = None;
            Step::none()
        }
        AppCommand::ReviewStart { task_id, target } => {
            let Some(thread) = reviewable_thread(state, target_id(state, task_id.as_deref()).as_deref()) else { return Step::none() };
            if !is_review_target(target) {
                return Step::none();
            }
            let project = project_for(state, Some(&thread)).cloned();
            let pending = new_pending(&thread.id, project.as_ref().map(|project| project.id.clone()), PendingOperation::Review { target: target.clone() });
            let effect = resolve_workspace_effect(&pending.id, Some(&thread), project.as_ref(), worktree_for(state, Some(&thread)), false, None);
            state.review_picker = None;
            pending_step(state, pending).with(effect)
        }
        AppCommand::RunCompact { task_id } => {
            let Some(thread) = target_id(state, task_id.as_deref()).and_then(|id| state.thread(&id).cloned()) else { return Step::none() };
            if !compactable_session(state, &thread) || thread_busy(state, &thread.id) {
                return Step::none();
            }
            if state.creating_worktrees.contains(&thread.id) {
                return Step::refuse(WORKTREE_CREATING_ERROR);
            }
            if leaving_thread_ids(state).contains(&thread.id) {
                return Step::refuse(WORKTREE_RELEASING_ERROR);
            }
            let project = project_for(state, Some(&thread)).cloned();
            let pending = new_pending(&thread.id, project.as_ref().map(|project| project.id.clone()), PendingOperation::Compact);
            let effect = resolve_workspace_effect(&pending.id, Some(&thread), project.as_ref(), worktree_for(state, Some(&thread)), false, None);
            pending_step(state, pending).with(effect)
        }
        AppCommand::RunCancel { task_id } => match target_id(state, task_id.as_deref()) {
            Some(task_id) if state.active_runs.contains_key(&task_id) => cancelling(state, &task_id),
            _ => Step::none(),
        },
        AppCommand::RunStopProcess { task_id, process_id } => match target_id(state, task_id.as_deref()) {
            Some(task_id) => process_stopped(state, &task_id, process_id),
            None => Step::none(),
        },
        AppCommand::RunDecide { allow, task_id, run_id, approval_id } => {
            let Some(active) = state.active_runs.get(task_id).cloned() else { return Step::none() };
            let Some(approval) = state.approvals.get(&active.run_id).cloned() else { return Step::none() };
            if &active.run_id != run_id || &approval.approval_id != approval_id {
                return Step::none();
            }
            with_approval_answered(state, task_id);
            attended_run(state, task_id);
            Step::effect(EffectWork::SendRunCommand { command: crate::effects::RunCommand::Approval { task_id: task_id.clone(), run_id: active.run_id, approval_id: approval.approval_id, allow: *allow } })
        }
        _ => Step::none(),
    }
}

/// What the agent process reports about work that outlives the run that started it.
pub fn thread_event(state: &mut WorkspaceState, event: &ThreadEvent) -> Step {
    if let crate::events::ThreadReport::ResourceStopped { stop } = &event.report {
        return crate::reducer::resource_stops::resource_stopped(state, &event.task_id, stop);
    }
    if state.thread(&event.task_id).is_none() {
        return Step::none();
    }
    apply_thread_event(state, event);
    Step::none()
}

fn ack(pending: &PendingRun, started: bool) -> Vec<crate::effects::Effect> {
    match &pending.automation_id {
        Some(automation_id) => vec![EffectWork::AutomationAck { ack: AutomationAck { automation_id: automation_id.clone(), run_id: pending.run_id.clone(), started } }.into()],
        None => Vec::new(),
    }
}

/// A run whose workspace could not be resolved never starts: its message goes back where it came from.
pub fn reduce_run_unresolved(state: &mut WorkspaceState, pending_id: &str, message: &str) -> Step {
    let Some(pending) = state.pending_runs.get(pending_id).cloned() else { return Step::none() };
    let dropped = unsent(state, &pending);
    if pending.origin == RunOrigin::Automation {
        return dropped.then(Step::effects(ack(&pending, false)));
    }
    if let (Some(proposal_id), Some(lead_id)) = (pending.coordination.as_ref().and_then(|send| send.proposal_id.clone()), pending.coordination.as_ref().and_then(|send| send.coordinator_id.clone())) {
        state.starting_proposals.retain(|id| id != &proposal_id);
        if let Some(lead) = state.thread_mut(&lead_id) {
            crate::domain::coordination::with_proposal_error(lead, &proposal_id, Some(message.to_string()));
        }
        return dropped.then(Step::result(CommandResult::failed(message)));
    }
    if let Some(chat) = pending.task_id.as_ref().and_then(|id| state.side_chats.iter_mut().find(|chat| &chat.id == id)) {
        chat.error = Some(message.to_string());
        return dropped.then(Step::result(CommandResult::failed(message)));
    }
    dropped.then(Step::refuse(message))
}

/// Takes a thread out of line and starts it: with the first message written to it while it waited, or to carry on.
pub fn resumed(state: &mut WorkspaceState, task_id: &str) -> Step {
    let Some(thread) = state.thread(task_id).cloned() else { return Step::none() };
    let Some(pause) = thread.limit_pause.clone() else { return Step::none() };
    if thread_busy(state, task_id) {
        return Step::none();
    }
    if let Some(thread) = state.thread_mut(task_id) {
        thread.limit_pause = None;
    }
    let mut queued = queued_for(state, task_id);
    if !queued.is_empty() {
        queued[0].held.prompt = resume_prompt(&pause, Some(&queued[0].held.prompt));
        set_queued(state, task_id, queued);
        return drained(state, task_id, QueueIntent::Resume);
    }
    let project = project_for(state, Some(&thread)).cloned();
    let mut pending = new_pending(task_id, project.as_ref().map(|project| project.id.clone()), PendingOperation::Resume { after: ResumeAfter::Limit });
    pending.prompt = resume_prompt(&pause, None);
    pending.warming = true;
    let effect = resolve_workspace_effect(&pending.id, Some(&thread), project.as_ref(), worktree_for(state, Some(&thread)), false, None);
    state.pending_runs.insert(pending.id.clone(), pending);
    Step::effect(effect)
}

/// Takes a thread out of line, handing what was written to it back to the composer.
pub fn pause_cancelled(state: &mut WorkspaceState, task_id: &str) -> Step {
    let Some(pause) = state.thread(task_id).and_then(|thread| thread.limit_pause.clone()) else { return Step::none() };
    if let Some(thread) = state.thread_mut(task_id) {
        thread.limit_pause = None;
    }
    if !resumes_on_its_own(pause.window) {
        return drained(state, task_id, QueueIntent::HandBack);
    }
    turn_ended(state, task_id, RunStatus::Cancelled);
    let settlement = settle_turn(TurnEnd { status: RunStatus::Cancelled, message: None, limit: None, run: None, stop: Some(StopIntent::User) });
    turn_settled(state, task_id, &settlement, None, now())
}

fn warming(state: &WorkspaceState, engine: crate::domain::engine::AgentEngine) -> bool {
    let engine_of = |task_id: Option<&str>| task_id.and_then(|id| state.thread(id)).map(|thread| thread.engine);
    state.active_runs.values().any(|run| run.warming && engine_of(Some(&run.task_id)) == Some(engine)) || state.pending_runs.values().any(|pending| pending.warming && engine_of(pending.task_id.as_deref()) == Some(engine))
}

fn next_reset(state: &WorkspaceState, after: i64) -> Option<i64> {
    limit_lines(&state.threads).values().flatten().filter_map(|thread| thread.limit_pause.as_ref().map(|pause| pause.resets_at)).filter(|resets| *resets > after).min()
}

fn may_advance(input: &WorkspaceInput) -> bool {
    match input {
        WorkspaceInput::Event(WorkspaceEvent::RunEvent { event }) => matches!(event.report, RunReport::ContextUsage { .. } | RunReport::RunStatus { .. }),
        WorkspaceInput::Event(WorkspaceEvent::LimitsElapsed { .. } | WorkspaceEvent::StoreLoaded { .. } | WorkspaceEvent::StoreAbsent | WorkspaceEvent::RunUnresolved { .. }) => true,
        WorkspaceInput::Command(AppCommand::TaskSend(_) | AppCommand::LimitCancel { .. } | AppCommand::SetFocused { focused: true }) => true,
        _ => false,
    }
}

/// Deadlines are durable; resuming threads and the one timer watching for the next reset belong to the reducer.
/// The next usage-limit reset before an input, which `reconcile_limit_pauses` compares the new one with.
pub struct LimitsBefore {
    at: i64,
    reset: Option<i64>,
    paused: bool,
}

impl LimitsBefore {
    pub fn of(state: &WorkspaceState, input: &WorkspaceInput) -> LimitsBefore {
        let at = match input {
            WorkspaceInput::Event(WorkspaceEvent::LimitsElapsed { at }) => *at,
            _ => now(),
        };
        LimitsBefore { at, reset: next_reset(state, at), paused: state.threads.iter().any(|thread| thread.limit_pause.is_some()) }
    }
}

pub fn reconcile_limit_pauses(previous: &LimitsBefore, state: &mut WorkspaceState, input: &WorkspaceInput, transition: &mut Transition) {
    let check = may_advance(input);
    let at = previous.at;
    if !check && !previous.paused && !state.threads.iter().any(|thread| thread.limit_pause.is_some()) {
        return;
    }
    let before = previous.reset;
    if matches!(input, WorkspaceInput::Event(WorkspaceEvent::StoreLoaded { .. })) {
        for thread in state.threads.clone() {
            let held = thread.limit_pause.as_ref().and_then(|pause| pause.held.clone()).unwrap_or_default();
            if !held.is_empty() && state.queued_messages.get(&thread.id).is_none_or(Vec::is_empty) {
                set_queued(state, &thread.id, held.into_iter().map(|held| crate::state::QueuedMessage { held, steering: false }).collect());
            }
        }
    }
    if check {
        let leaving = leaving_thread_ids(state);
        let lines: Vec<(crate::domain::engine::AgentEngine, Vec<String>)> = limit_lines(&state.threads).into_iter().map(|(engine, line)| (engine, line.into_iter().filter(|thread| thread.limit_pause.as_ref().is_some_and(|pause| pause.resets_at <= at)).map(|thread| thread.id.clone()).collect())).collect();
        for (engine, due) in lines {
            if warming(state, engine) {
                continue;
            }
            let Some(next) = due.into_iter().find(|id| !leaving.contains(id) && !state.creating_worktrees.contains(id)) else { continue };
            let step = resumed(state, &next);
            let resumed = finish(state, step);
            transition.effects.extend(resumed.effects);
        }
    }
    for thread in state.threads.iter_mut() {
        let Some(pause) = thread.limit_pause.as_ref() else { continue };
        let queued: Vec<_> = state.queued_messages.get(&thread.id).cloned().unwrap_or_default();
        let held: Vec<_> = queued.into_iter().map(|message| message.held).collect();
        let same = pause.held.as_ref().map(|held_ids| held_ids.iter().map(|message| &message.id).eq(held.iter().map(|message| &message.id))).unwrap_or(held.is_empty());
        if same {
            continue;
        }
        if let Some(pause) = thread.limit_pause.as_mut() {
            pause.held = (!held.is_empty()).then_some(held);
        }
    }
    let next = next_reset(state, at);
    if next == before && !(check && next.is_some()) {
        return;
    }
    transition.effects.push(EffectWork::ScheduleLimitReset { at: next }.into());
}

/// How a run's turn ends, if this event ends it.
fn turn_ending(state: &WorkspaceState, active: &ActiveRun, event: &RunEvent) -> Option<Settlement> {
    let RunReport::RunStatus { status, message, limit } = &event.report else { return None };
    if matches!(status, RunStatus::Running | RunStatus::AwaitingApproval) {
        return None;
    }
    let stop = if state.quiet_stops.contains(&active.run_id) { Some(StopIntent::StopAll) } else if active.cancelling { Some(StopIntent::User) } else { None };
    Some(settle_turn(TurnEnd { status: *status, message: message.clone(), limit: limit.clone(), run: Some(active), stop }))
}

fn run_notice(thread: &Thread, active: &ActiveRun, event: &RunEvent, surfacing: Option<RunSurfacing>) -> Option<(String, NoticeReason)> {
    if active.origin == RunOrigin::Composer {
        match &event.report {
            RunReport::ApprovalRequested { intent, .. } => return Some((waiting_headline_tool(&intent.name), NoticeReason::Waiting)),
            RunReport::QuestionRequested { request, .. } => return request.questions.first().map(|question| (question.question.clone(), NoticeReason::Waiting)),
            _ => {}
        }
    }
    let message = match &event.report {
        RunReport::RunStatus { message, .. } => message.as_deref(),
        _ => None,
    };
    let headline = settled_headline(surfacing, message)?;
    if surfacing == Some(RunSurfacing::Failed) {
        return Some((headline, NoticeReason::Failed));
    }
    let posted = if is_coordinator(Some(thread)) { posted_headline(thread, active.messages_before) } else { None };
    Some((posted.unwrap_or(headline), NoticeReason::Finished))
}

fn image_reference(text: &str) -> bool {
    let lowered = text.to_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp"].iter().any(|extension| {
        lowered.match_indices(extension).any(|(at, _)| lowered[at + extension.len()..].chars().next().is_none_or(|next| !(next.is_alphanumeric() || next == '_')))
    })
}

/// A run starting in the workspace it resolved to, and what it reports.
pub fn reduce_runs(state: &mut WorkspaceState, event: &WorkspaceEvent) -> crate::reducer::Applied {
    match event {
        WorkspaceEvent::RunResolved { pending_id, workspace, worktree } => run_resolved(state, pending_id, workspace, worktree.as_ref()).into(),
        WorkspaceEvent::RunEvent { event } => run_event(state, event).into(),
        _ => Step::none().into(),
    }
}

fn run_resolved(state: &mut WorkspaceState, pending_id: &str, workspace: &WorkspaceRecord, worktree: Option<&CreatedWorktree>) -> Transition {
    let Some(pending) = state.pending_runs.shift_remove(pending_id) else { return Transition::default() };
    if let Some(project_id) = &pending.project_id {
        if let Some(project) = state.projects.iter_mut().find(|project| &project.id == project_id) {
            if project.workspace_id.is_none() && workspace.kind == crate::domain::platform::WorkspaceKind::Project && workspace.root == project.root {
                project.workspace_id = Some(workspace.id.clone());
            }
        }
    }
    if pending.task_id.as_deref().and_then(|id| state.thread(id)).is_some_and(|thread| thread.resource_stop.is_some()) {
        if let (Some(worktree), Some(project_id)) = (worktree, &pending.project_id) {
            with_used_worktree(state, Some(worktree.in_project(project_id)), Some(&worktree.id));
        }
        if pending.origin == RunOrigin::Automation {
            return Transition { effects: ack(&pending, false), result: None };
        }
        let step = unsent(state, &pending);
        let mut transition = finish(state, step);
        transition.result = rejected(state, crate::reducer::resource_stops::MEMORY_STOP_ERROR).result;
        return transition;
    }
    if let Some(gone) = model_gone(state, &pending) {
        return gone;
    }
    match &pending.operation {
        Some(PendingOperation::Compact) => start_compaction(state, &pending, workspace),
        Some(PendingOperation::Review { .. }) => start_review(state, &pending, workspace),
        Some(PendingOperation::Resume { .. }) => start_resume(state, &pending, workspace),
        None if pending.origin == RunOrigin::Automation => start_automation_run(state, &pending, workspace, worktree),
        None => start_composer_run(state, &pending, workspace, worktree),
    }
}

fn run_event(state: &mut WorkspaceState, reported: &RunEvent) -> Transition {
    let task_id = reported.task_id.clone();
    let ends = matches!(reported.report, RunReport::RunStatus { status: RunStatus::Cancelled | RunStatus::Failed | RunStatus::Succeeded, .. });
    if !ends && state.thread(&task_id).is_some_and(|thread| thread.resource_stop.is_some()) {
        return Transition::default();
    }
    let opening = matches!(reported.report, RunReport::RunStarted { agent_initiated: true }) && !state.active_runs.contains_key(&task_id) && state.thread(&task_id).is_some();
    if opening {
        begin_run(state, &task_id, &reported.run_id, ATTENDED_RUN, None, Some(RunPickup::Agent));
    }
    let Some(active) = state.active_runs.get(&task_id).cloned() else { return Transition::default() };
    if reported.run_id != active.run_id || reported.sequence <= active.sequence {
        return Transition::default();
    }
    let ending = turn_ending(state, &active, reported);
    let mut event = reported.clone();
    if let (Some(ending), RunReport::RunStatus { status, message, limit }) = (&ending, &mut event.report) {
        if *status != ending.status {
            *status = ending.status;
            *message = None;
            *limit = None;
        }
    }
    let workspace_id = thread_workspace_id(state, state.thread(&task_id));
    apply_run_event(state, &event);
    if let RunReport::AssistantDelta { message_id, text, artifact, .. } = &event.report {
        let message_id = if *artifact { message_id.clone() } else { answer_part_id(state.active_runs.get(&task_id), message_id) };
        let thread = state.thread(&task_id);
        let message = thread.and_then(|thread| thread.messages.iter().find(|message| message.id == message_id));
        if let (Some(message), true) = (message, image_reference(text)) {
            let root = thread_workspace_root(state, thread).unwrap_or_default();
            return Transition { effects: vec![EffectWork::PreserveMessageImages { text: message.text.clone(), root, message_id }.into()], result: None };
        }
        return Transition::default();
    }
    if let Some(ending) = &ending {
        if ending.restores_testimony {
            let step = turn_settled(state, &task_id, ending, Some(&active), now());
            return finish(state, step);
        }
        turn_testimony(state, &task_id, ending, Some(&active));
    }
    if matches!(event.report, RunReport::ComputerUseSetupRequired) {
        state.computer_use_setup = true;
    }
    let mut said = Vec::new();
    if let Some(thread) = state.thread(&task_id).cloned() {
        if !ending.as_ref().is_some_and(|ending| ending.dismisses) {
            if let Some((headline, reason)) = run_notice(&thread, &active, &event, ending.as_ref().and_then(|ending| ending.surfacing)) {
                said = announced(state, &thread, &headline, reason);
            }
        }
    }
    match &event.report {
        RunReport::QueuedDelivered { message_id } => with_delivered_message(state, &task_id, message_id),
        RunReport::QueuedSteerFailed { message_id, message } => with_steering_failure(state, &task_id, message_id, message),
        _ => {}
    }
    let finished = ending.as_ref().is_some_and(|ending| matches!(ending.status, RunStatus::Succeeded | RunStatus::Failed));
    let mut environment = Vec::new();
    if finished {
        let reviews = workspace_id.as_ref().map(|workspace_id| crate::reducer::misc::reread_reviews_of(state, &task_id, workspace_id)).unwrap_or_default();
        let reread = turn_pull_request_read(state, &task_id);
        if let Some(workspace_id) = &workspace_id {
            environment.push(crate::effects::Effect::follow_up(EffectWork::RefreshEnvironment { workspace_id: workspace_id.clone(), task_id: Some(task_id.clone()), run_id: Some(event.run_id.clone()) }));
            environment.extend(reviews);
            environment.extend(reread.map(|effect| crate::effects::Effect { follow_up: true, ..effect }));
        }
    }
    let Some(ending) = ending else {
        let mut effects = environment;
        effects.extend(said);
        return Transition { effects, result: None };
    };
    let moved = crate::reducer::misc::move_after_run_settled(state, &task_id, ending.status);
    let drained = settled_after_testimony(state, &task_id, &ending, Some(&active), now());
    let mut effects = environment;
    effects.extend(said);
    effects.extend(moved.effects);
    let drained = finish(state, drained);
    effects.extend(drained.effects);
    Transition { effects, result: None }
}

/// The read a thread under a coordinator asks for as its turn ends, if its work is or has been on a pull request.
fn turn_pull_request_read(state: &mut WorkspaceState, task_id: &str) -> Option<crate::effects::Effect> {
    let thread = state.thread(task_id)?;
    if !crate::domain::coordination::works_under_coordinator(&state.threads, Some(thread)) {
        return None;
    }
    let delivers = thread.brief.as_ref().is_some_and(|brief| brief.delivers == crate::domain::coordination::Delivery::PullRequest);
    if !delivers && thread.pull_request.is_none() {
        return None;
    }
    crate::reducer::pull_requests::member_pull_request_read(state, task_id)
}

/// Every run on a thread goes to the thread's model, which its engine must still offer here.
fn model_gone(state: &mut WorkspaceState, pending: &PendingRun) -> Option<Transition> {
    let thread = state.thread(pending.task_id.as_deref()?)?.clone();
    let model = thread_model(&state.catalog(), thread.engine, thread.model.as_deref());
    if model.as_ref().is_some_and(|model| state.engine_offers_model(thread.engine, model)) {
        return None;
    }
    if pending.origin == RunOrigin::Automation {
        return Some(Transition { effects: ack(pending, false), result: None });
    }
    let step = unsent(state, pending);
    let mut transition = finish(state, step);
    let refused = rejected(state, match model {
        None => no_model(thread.engine),
        Some(model) => unknown_model(&model, None),
    });
    transition.result = refused.result;
    Some(transition)
}

fn start_compaction(state: &mut WorkspaceState, pending: &PendingRun, workspace: &WorkspaceRecord) -> Transition {
    let Some(thread) = pending.task_id.as_deref().and_then(|id| state.thread(id)).cloned() else { return Transition::default() };
    let Some(usage) = thread.context_usage.clone() else { return Transition::default() };
    if !compactable_session(state, &thread) || state.active_runs.contains_key(&thread.id) {
        return Transition::default();
    }
    let mut command = start_run_command(state, &thread, &pending.run_id, "", &workspace.id, None);
    command.operation = Some(RunOperation::Compact { pre_tokens: usage.tokens });
    begin_run(state, &thread.id, &pending.run_id, RunProvenance { operation: Some(RunOperationKind::Compact), ..ATTENDED_RUN }, None, None);
    Transition { effects: vec![EffectWork::StartRun { command: Box::new(command) }.into()], result: None }
}

fn start_review(state: &mut WorkspaceState, pending: &PendingRun, workspace: &WorkspaceRecord) -> Transition {
    let Some(thread) = reviewable_thread(state, pending.task_id.as_deref()) else { return Transition::default() };
    let Some(PendingOperation::Review { target }) = &pending.operation else { return Transition::default() };
    let mut command = start_run_command(state, &thread, &pending.run_id, "", &workspace.id, None);
    command.operation = Some(RunOperation::Review { target: target.clone() });
    if thread.inherited_continuation {
        command.fork_continuation = Some(true);
    }
    begin_run(state, &thread.id, &pending.run_id, RunProvenance { operation: Some(RunOperationKind::Review), ..ATTENDED_RUN }, None, None);
    Transition { effects: vec![EffectWork::StartRun { command: Box::new(command) }.into()], result: None }
}

/// A thread picked back up after its usage limit lifted or an update restarted the app.
fn start_resume(state: &mut WorkspaceState, pending: &PendingRun, workspace: &WorkspaceRecord) -> Transition {
    let Some(thread) = thread_to_begin(state, pending.task_id.as_deref()).cloned() else { return Transition::default() };
    let after = match pending.operation {
        Some(PendingOperation::Resume { after }) => Some(after),
        _ => None,
    };
    let update = after == Some(ResumeAfter::Update);
    let mut command = start_run_command(state, &thread, &pending.run_id, &pending.prompt, &workspace.id, None);
    if thread.inherited_continuation && thread.continuation.is_some() {
        command.fork_continuation = Some(true);
    }
    side_channel_for(state, &thread, &mut command);
    if let Some(thread) = state.thread_mut(&thread.id) {
        thread.messages.push(Shared::new(create_conversation_message(MessageKind::System, match after {
            Some(ResumeAfter::Memory) => "Resuming after a memory stop. Check interrupted actions before repeating them.",
            Some(ResumeAfter::Update) => "App updated. Resuming.",
            _ => "Usage limit reset. Resuming.",
        }, None)));
        thread.updated_at = now();
    }
    begin_run(state, &thread.id, &pending.run_id, ATTENDED_RUN, None, Some(if update { RunPickup::CarryingOn } else { RunPickup::Warming }));
    Transition { effects: vec![EffectWork::StartRun { command: Box::new(command) }.into()], result: None }
}

/// A checkout used by a run: recorded when it was made on the way, and marked as just used.
fn with_used_worktree(state: &mut WorkspaceState, created: Option<Worktree>, used: Option<&str>) {
    if let Some(created) = created {
        if !state.worktrees.iter().any(|worktree| worktree.id == created.id) {
            state.worktrees.push(created);
        }
    }
    if let Some(used) = used {
        if let Some(worktree) = state.worktrees.iter_mut().find(|worktree| worktree.id == used) {
            worktree.last_used_at = now();
        }
    }
}

fn start_automation_run(state: &mut WorkspaceState, pending: &PendingRun, workspace: &WorkspaceRecord, worktree: Option<&CreatedWorktree>) -> Transition {
    let Some(task_id) = pending.task_id.clone() else { return Transition::default() };
    let Some(thread) = thread_to_begin(state, Some(&task_id)).cloned() else { return Transition { effects: ack(pending, false), result: None } };
    let mut message = create_sent_message(SentMessage { text: pending.text.clone(), origin: pending.message_origin.clone(), ..SentMessage::default() });
    message.withdrawn = pending.quiet;
    let created = match (worktree, &thread.project_id) {
        (Some(worktree), Some(project_id)) => {
            let mut made = worktree.in_project(project_id);
            if made.name.is_none() {
                made.name = Some(thread.title.clone());
            }
            Some(made)
        }
        _ => None,
    };
    let entered = created.clone().or_else(|| worktree_for(state, Some(&thread)).cloned());
    let command = {
        let mut command = start_run_command(state, &thread, &pending.run_id, &pending.prompt, &workspace.id, Some(pending.policy.unwrap_or(thread.execution_policy)));
        command.unattended = pending.unattended;
        command
    };
    let mark = thread_mark(Some(&thread));
    with_used_worktree(state, created, entered.as_ref().map(|worktree| worktree.id.as_str()));
    if let Some(thread) = state.thread_mut(&task_id) {
        if let Some(entered) = &entered {
            thread.worktree_id = Some(entered.id.clone());
            thread.worktree_entered_at = Some(thread.worktree_entered_at.unwrap_or_else(now));
        }
        thread.messages.push(Shared::new(message));
        thread.updated_at = now();
    }
    begin_run(state, &task_id, &pending.run_id, RunProvenance { origin: RunOrigin::Automation, quiet: pending.quiet, operation: None, users_own: false }, Some(mark), None);
    let mut effects: Vec<crate::effects::Effect> = vec![EffectWork::StartRun { command: Box::new(command) }.into()];
    effects.extend(ack(pending, true));
    Transition { effects, result: None }
}

/// What a thread a send names is called when nobody named it: its first words, or what came with them.
pub fn thread_title_for(text: &str, attachments: usize) -> String {
    clamp_title(if !text.is_empty() {
        text.to_string()
    } else if attachments == 1 {
        "Screenshot".into()
    } else {
        format!("{attachments} screenshots")
    }
    .as_str())
}

fn paste_title(pastes: &[crate::domain::conversation::PastedText]) -> String {
    let Some(first) = pastes.first() else { return String::new() };
    first.text.split('\n').map(str::trim).find(|line| !line.is_empty()).unwrap_or("").to_string()
}

fn file_title(files: &[crate::domain::conversation::AttachedFile]) -> String {
    match files.len() {
        0 => String::new(),
        1 => clamp_title(&files[0].name),
        n => clamp_title(&format!("{n} files")),
    }
}

/// What a new thread under a coordinator reads after its first message.
pub fn brief_prompt(brief: Option<&crate::domain::coordination::ThreadBrief>, memory: &str) -> String {
    if brief.is_none() && memory.is_empty() {
        return String::new();
    }
    let mut lines = vec!["---".to_string()];
    if let Some(brief) = brief {
        lines.push("Brief from the coordinator that started this thread:".into());
        lines.push(format!("The user's words: {}", brief.intent));
        lines.push(format!("Done when: {}", brief.done_when));
        let report = if brief.delivers == crate::domain::coordination::Delivery::Report { " (no code changes)" } else { "" };
        lines.push(format!("Delivers: {}{report}", brief.delivers.label()));
    }
    if brief.is_some() && !memory.is_empty() {
        lines.push(String::new());
    }
    if !memory.is_empty() {
        lines.push(memory.to_string());
    }
    lines.join("\n")
}

/// A thread as its run starts under a coordinator, what it is told first, and what the run is sent.
fn coordinated_start(state: &WorkspaceState, pending: &PendingRun, located: Thread, existing: bool) -> (Thread, Vec<crate::domain::conversation::ConversationMessage>, String, bool) {
    let coordinator_role = pending.role == Some(crate::domain::thread_role::ThreadRole::Coordinator);
    let lead = if existing || coordinator_role { None } else { pending.coordination.as_ref().and_then(|send| send.coordinator_id.as_deref()).and_then(|id| state.thread(id)) };
    let remote_lead = if existing || coordinator_role { None } else { pending.coordination.as_ref().and_then(|send| send.lead.clone()) };
    let mut opened = located;
    if !existing && coordinator_role {
        as_coordinator(&state.catalog(), &mut opened);
    }
    let joined_lead = lead.is_some() || remote_lead.is_some();
    if let Some(lead) = lead {
        opened.parent_id = Some(lead.id.clone());
    } else if let Some(remote) = remote_lead {
        opened.remote_lead = Some(crate::domain::coordination::HeldLead { thread_id: remote.thread_id, title: remote.title, computer: remote.computer, proposal_id: remote.proposal_id, away: false, back_at: None });
    }
    let coordinating = is_coordinator(Some(&opened));
    let fresh = !pending.warming && wants_fresh_session(&opened, session_holds_work(state, &opened.id));
    let mut thread = opened.clone();
    if coordinating {
        if let Some(notes) = &thread.coordination_notes {
            let heard: std::collections::HashSet<String> = notes.iter().map(|note| note.id.clone()).collect();
            without_coordination_notes(&mut thread, &heard);
        }
        with_memory_heard(&mut thread);
    }
    thread.resolved_at = None;
    if fresh {
        thread.continuation = None;
        thread.inherited_continuation = false;
        thread.context_usage = None;
        thread.continuation_status = crate::domain::thread::ContinuationStatus::None;
    }
    let brief = pending.coordination.as_ref().and_then(|send| send.brief.as_ref());
    let memory = pending.coordination.as_ref().and_then(|send| send.memory.clone()).unwrap_or_default();
    let standing = if joined_lead { brief_prompt(brief, &memory) } else { brief_prompt(brief, "") };
    let sent = if standing.is_empty() { pending.prompt.clone() } else { format!("{}\n\n{standing}", pending.prompt) };
    let notice = if fresh { vec![create_conversation_message(MessageKind::System, "Continued in a fresh session to keep this coordinator's context small.", None)] } else { Vec::new() };
    let seeded = fresh || (coordinating && thread.continuation.is_none() && (!opened.messages.is_empty() || opened.history_summary.is_some()));
    let carried = pending.coordination.as_ref().and_then(|send| send.notes.clone()).unwrap_or_default();
    let prompt = if coordinating { crate::workspace::coordination_messages::coordinator_prompt(state, &opened, &sent, &carried, seeded) } else { sent };
    (thread, notice, prompt, fresh)
}

fn start_composer_run(state: &mut WorkspaceState, pending: &PendingRun, workspace: &WorkspaceRecord, worktree: Option<&CreatedWorktree>) -> Transition {
    let existing = pending.task_id.as_deref().and_then(|id| state.thread(id)).cloned();
    if pending.task_id.is_some() && existing.is_none() {
        return rejected(state, "This thread was removed before its message could be sent.");
    }
    if existing.as_ref().is_some_and(|thread| thread.archived_at.is_some()) {
        let step = unsent(state, pending);
        finish(state, step);
        return rejected(state, "This thread was archived before its message could be sent. Restore it from Archived threads in Settings to send it again.");
    }
    if pending.task_id.as_ref().is_some_and(|id| state.active_runs.contains_key(id)) {
        let step = unsent(state, pending);
        finish(state, step);
        return rejected(state, "This thread started another run before its message could be sent. Send the message again.");
    }
    let created = match (worktree, &pending.project_id) {
        (Some(worktree), Some(project_id)) => Some(worktree.in_project(project_id)),
        _ => None,
    };
    let arriving = created.clone().or_else(|| worktree_for(state, existing.as_ref()).cloned()).or_else(|| worktree_by_id(state, pending.worktree_id.as_deref()).cloned());
    let entering = arriving.is_some() && existing.as_ref().is_none_or(|thread| thread.worktree_entered_at.is_none());
    let engine = pending.engine.unwrap_or(state.draft_engine);
    let model = pending.model.clone().unwrap_or_else(|| state.draft_model.clone());
    if existing.is_none() && !state.engine_offers_model(engine, &model) {
        let step = unsent(state, pending);
        finish(state, step);
        return rejected(state, unknown_model(&model, None));
    }
    let draft = state.draft();
    let choice = normalize_draft(
        &state.catalog(),
        AgentDraftChange {
            engine,
            model: Some(model),
            effort: pending.effort.unwrap_or(draft.effort),
            policy: if pending.role == Some(crate::domain::thread_role::ThreadRole::Reviewer) { capabilities_for(engine).unattended_policy } else { draft.policy },
            fast_mode: draft.fast_mode,
        },
        (pending.role == Some(crate::domain::thread_role::ThreadRole::Coordinator)).then_some(crate::domain::coordination::CoordinationRole::Coordinator),
    );
    let thread = match &existing {
        Some(thread) => thread.clone(),
        None => {
            let at = now();
            let title = match &pending.title {
                Some(title) => clamp_title(title),
                None => {
                    let named = if !pending.text.is_empty() { pending.text.clone() } else { let pasted = paste_title(pending.pastes.as_deref().unwrap_or(&[])); if !pasted.is_empty() { pasted } else { file_title(pending.files.as_deref().unwrap_or(&[])) } };
                    thread_title_for(&named, pending.attachments.len())
                }
            };
            let mut thread = Thread::new(random_id(), title, choice.engine, choice.policy, at);
            thread.project_id = pending.project_id.clone();
            thread.model = Some(choice.model.clone());
            thread.effort = Some(choice.effort);
            if capabilities_for(choice.engine).fast_mode {
                thread.fast_mode = Some(choice.fast_mode);
            }
            thread.role = pending.role;
            thread.brief = pending.coordination.as_ref().and_then(|send| send.brief.clone());
            thread.last_change_snapshot.captured_at = at;
            thread.sort_index = Some(next_sort_index(&state.threads));
            thread
        }
    };
    let created = created.map(|mut created| {
        if created.name.is_none() {
            created.name = Some(thread.title.clone());
        }
        created
    });
    let message = create_sent_message(SentMessage {
        text: pending.text.clone(),
        origin: pending.message_origin.clone(),
        attachments: Some(pending.attachments.clone()),
        annotations: pending.annotations.clone(),
        pastes: pending.pastes.clone(),
        files: pending.files.clone(),
    });
    let arrival = match &arriving {
        Some(arriving) if existing.as_ref().and_then(|thread| thread.worktree_id.as_deref()) != Some(arriving.id.as_str()) => vec![create_conversation_message(
            MessageKind::System,
            format!("Moved into a worktree at {}", arriving.root),
            Some(format!("Detached at {}", &arriving.base_commit[..arriving.base_commit.len().min(7)])),
        )],
        _ => Vec::new(),
    };
    let mut located = thread.clone();
    if let Some(arriving) = &arriving {
        located.worktree_id = Some(arriving.id.clone());
        located.worktree_entered_at = Some(located.worktree_entered_at.unwrap_or_else(now));
    }
    let inherited = located.inherited_continuation;
    let (mut updated, notice, prompt, fresh) = coordinated_start(state, pending, located, existing.is_some());
    updated.messages.extend((arrival).into_iter().map(Shared::new));
    updated.messages.extend((notice).into_iter().map(Shared::new));
    updated.messages.push(Shared::new(message));
    updated.updated_at = now();
    let thread_id = updated.id.clone();
    match state.threads.iter().position(|item| item.id == thread_id) {
        Some(index) => state.threads[index] = updated.clone().into(),
        None => state.threads.insert(0, updated.clone().into()),
    }
    if existing.is_none() {
        if let Some(proposal_id) = pending.coordination.as_ref().and_then(|send| send.proposal_id.clone()) {
            let lead_id = pending.coordination.as_ref().and_then(|send| send.coordinator_id.clone());
            if let Some(lead) = lead_id.and_then(|id| state.thread_mut(&id)) {
                let proposal = lead.proposals.iter().flatten().find(|item| item.id == proposal_id).cloned();
                without_proposals(lead, &[proposal_id.clone()].into());
                if let (Some(proposal), true) = (proposal, is_coordinator(Some(lead))) {
                    with_coordination_note(lead, CoordinationNote { id: random_id(), thread_id: thread_id.clone(), text: format!("The user started your suggested thread \"{}\" [{thread_id}].", proposal.title), at: now(), urgent: false, run_id: None, quiet: true });
                }
            }
            state.starting_proposals.retain(|id| id != &proposal_id);
        }
    }
    let focusing = existing.is_none() && pending.draft_key.is_some();
    let handed = if focusing { state.diffs.get(DRAFT_DOCK).cloned() } else { None };
    if focusing {
        state.draft_branch = None;
        state.draft_worktree = false;
        state.draft_worktree_id = None;
        state.draft_role = None;
        crate::reducer::dock::hand_over_draft_dock(state, &thread_id);
        state.current_id = Some(thread_id.clone());
    }
    with_used_worktree(state, created, arriving.as_ref().map(|worktree| worktree.id.as_str()));
    begin_run(state, &thread_id, &pending.run_id, RunProvenance { users_own: pending.users_own, ..ATTENDED_RUN }, None, pending.warming.then_some(RunPickup::Warming));
    if let Some(queued_ids) = &pending.queued_ids {
        let rest = queued_for(state, &thread_id).into_iter().filter(|message| !queued_ids.contains(&message.held.id)).collect();
        set_queued(state, &thread_id, rest);
    }
    let titling: Vec<crate::effects::Effect> = if existing.is_some() || pending.title.is_some() || (pending.text.is_empty() && pending.attachments.is_empty()) {
        Vec::new()
    } else {
        vec![EffectWork::SuggestTitle { task_id: thread_id.clone(), engine: updated.engine, model: run_model_of(state, &updated), text: pending.text.clone(), attachments: pending.attachments.clone() }.into()]
    };
    let mut command = start_run_command(state, &updated, &pending.run_id, &prompt, &workspace.id, None);
    if (entering || inherited) && updated.continuation.is_some() {
        command.fork_continuation = Some(true);
    }
    command.fresh_session = fresh;
    side_channel_for(state, &updated, &mut command);
    let reviewing = match handed {
        Some(diff) => crate::reducer::misc::read_diff_from(state, &thread_id, &workspace.id, &diff.range),
        None => Vec::new(),
    };
    if let Some(draft_key) = &pending.draft_key {
        without_started_draft(state, draft_key, pending);
    }
    let mut effects: Vec<crate::effects::Effect> = vec![EffectWork::StartRun { command: Box::new(command) }.into()];
    effects.extend(titling);
    effects.extend(follow_ups(reviewing));
    Transition { effects, result: Some(CommandResult::ok_with_task(thread_id)) }
}

/// The run a thread has going, which stopping or archiving reaches.
pub fn active_run(state: &WorkspaceState, task_id: &str) -> Option<ActiveRun> {
    state.active_runs.get(task_id).cloned()
}

pub fn drained_after(state: &mut WorkspaceState, task_id: &str) -> Step {
    drained(state, task_id, QueueIntent::Deliver)
}

pub fn resolving_effect(state: &WorkspaceState, pending: &PendingRun) -> EffectWork {
    let thread = pending.task_id.as_deref().and_then(|id| state.thread(id));
    let project = project_for(state, thread);
    resolve_workspace_effect(&pending.id, thread, project, worktree_for(state, thread), false, None)
}
