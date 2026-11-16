//! What a run's reports do to its thread and to the feeds it reports into.

use crate::shared::Shared;
use crate::domain::conversation::{create_conversation_message, create_failure_message, ConversationMessage, MessageKind};
use crate::domain::coordination::with_memory_unheard;
use crate::domain::engine::default_model_for;
use crate::domain::question::PendingQuestion;
use crate::domain::run::{RunStatus, Subagent, SubagentReport, SubagentStatus, SubagentActivity};
use crate::domain::thread::{run_outcome, ContinuationStatus, ContextUsage, Thread};
use crate::domain::workflow::{Workflow, WorkflowStatus};
use crate::events::{CompactionTrigger, RunEvent, RunReport, ThreadEvent, ThreadReport};
use crate::state::{ActiveRun, ApprovalView, RunOperationKind, RunOrigin, RunPhase, StreamingTail, ThreadMark, ThreadRunStatus, WorkspaceState};
use crate::util::now;

/// Who a run answers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunProvenance {
    pub origin: RunOrigin,
    pub quiet: bool,
    pub operation: Option<RunOperationKind>,
    pub users_own: bool,
}

pub const ATTENDED_RUN: RunProvenance = RunProvenance { origin: RunOrigin::Composer, quiet: false, operation: None, users_own: false };

/// A run is open on its thread until it ends; compacting and a quiet scheduled run are not turns.
pub fn opened_turn(thread: &mut Thread, provenance: &RunProvenance) {
    if provenance.operation == Some(RunOperationKind::Compact) || (provenance.origin == RunOrigin::Automation && provenance.quiet) {
        return;
    }
    thread.run_open = true;
    thread.ranked_at = now();
}

/// Puts a thread back where a tick found it: its messages withdrawn and the moments it moved rolled back.
pub fn withdraw_run(thread: &mut Thread, from: usize, before: &ThreadMark) {
    for message in thread.messages.iter_mut().skip(from) {
        message.withdrawn = true;
    }
    thread.run_ended_at = before.run_ended_at;
    thread.run_ended_as = before.run_ended_as;
    thread.outcome = before.outcome;
    thread.outcome_unread = before.outcome_unread;
    thread.updated_at = before.updated_at;
    if let Some(ranked) = before.ranked_at {
        thread.ranked_at = ranked;
    }
    if before.report.is_some() && thread.report.as_ref().is_none_or(|report| report.state == crate::domain::coordination::CoordinationState::Working) {
        thread.report = before.report.clone();
    }
}

/// Scheduled runs carry their own framing: nobody is present to answer a question or end the loop.
pub fn automation_run_prompt(prompt: &str, run_number: i64, surface_when: Option<&str>, ends_when: Option<&str>) -> String {
    let stop = match ends_when {
        Some(ends) => format!("This automation ends when: {ends} Once that holds, call the aicodingtool-automation stop tool to end it."),
        None => "If the automation's stop condition is now met, call the aicodingtool-automation stop tool to end it.".to_string(),
    };
    let framing = format!("This is automated run #{run_number} of this task's automation, started by AICodingTool's scheduler with no user watching. {stop}");
    let quiet = surface_when.map(|surface| format!("\n\nThis run is quiet: it settles without reaching the user unless you say otherwise. Surface it when: {surface} If that is what you found, call the aicodingtool-automation notify tool with a headline before you finish. If it is not, call nothing_to_report with what you checked. Call neither and the run surfaces as an ordinary one, which is what a run that could not do its job should do.")).unwrap_or_default();
    format!("{prompt}\n\n---\n{framing}{quiet}")
}

/// A run moved to another phase: what the phase it left carried goes with it.
pub fn with_run_phase(state: &mut WorkspaceState, thread_id: &str, phase: RunPhase) {
    if let Some(run) = state.active_runs.get_mut(thread_id) {
        run.phase = phase;
    }
}

/// The run's approval is answered: it goes, and the run is running again.
pub fn with_approval_answered(state: &mut WorkspaceState, thread_id: &str) {
    let Some(run_id) = state.active_runs.get(thread_id).map(|run| run.run_id.clone()) else { return };
    state.approvals.shift_remove(&run_id);
    with_run_phase(state, thread_id, RunPhase::Running);
}

pub fn with_run_status(state: &mut WorkspaceState, thread_id: &str, status: ThreadRunStatus) {
    if status == ThreadRunStatus::Idle {
        state.run_statuses.shift_remove(thread_id);
    } else {
        state.run_statuses.insert(thread_id.to_string(), status);
    }
}

pub fn with_streaming_tail(state: &mut WorkspaceState, thread_id: &str, tail: Option<StreamingTail>) {
    match tail {
        Some(tail) => {
            state.streaming_tails.insert(thread_id.to_string(), tail);
        }
        None => {
            state.streaming_tails.shift_remove(thread_id);
        }
    }
}

pub fn with_workflows(state: &mut WorkspaceState, thread_id: &str, workflows: Vec<Workflow>) {
    if workflows.is_empty() {
        state.workflows.shift_remove(thread_id);
    } else {
        state.workflows.insert(thread_id.to_string(), workflows);
    }
}

pub fn with_subagents(state: &mut WorkspaceState, thread_id: &str, subagents: Vec<Subagent>) {
    if subagents.is_empty() {
        state.subagents.shift_remove(thread_id);
    } else {
        state.subagents.insert(thread_id.to_string(), subagents);
    }
}

fn new_subagent(id: &str, description: &str) -> Subagent {
    Subagent {
        id: id.into(),
        description: description.into(),
        agent_type: None,
        session_scoped: false,
        status: SubagentStatus::Working,
        stopping: false,
        last_tool_name: None,
        summary: None,
        total_tokens: None,
        started_at: now(),
        finished_at: None,
        activity: Vec::new(),
        model: None,
        effort: None,
        prompt: None,
    }
}

fn update_subagent(state: &mut WorkspaceState, thread_id: &str, subagent_id: &str, update: impl FnOnce(Option<&Subagent>) -> Subagent) {
    let mut held = state.subagents.get(thread_id).cloned().unwrap_or_default();
    match held.iter().position(|subagent| subagent.id == subagent_id) {
        Some(index) => {
            let replaced = update(Some(&held[index]));
            if replaced == held[index] {
                return;
            }
            held[index] = replaced;
        }
        None => held.push(update(None)),
    }
    with_subagents(state, thread_id, held);
}

fn metadata(target: &mut Subagent, model: &Option<String>, effort: &Option<String>, prompt: &Option<String>) {
    if model.is_some() {
        target.model = model.clone();
    }
    if effort.is_some() {
        target.effort = effort.clone();
    }
    if prompt.is_some() {
        target.prompt = prompt.clone();
    }
}

/// Shared subagent state changes, whether a run carries them or a session reports them later.
fn apply_subagent_report(state: &mut WorkspaceState, thread_id: &str, report: &SubagentReport) {
    match report {
        SubagentReport::Metadata { id, model, effort, prompt } => update_subagent(state, thread_id, id, |existing| {
            let mut next = existing.cloned().unwrap_or_else(|| new_subagent(id, "Subagent"));
            let kept_prompt = existing.and_then(|existing| existing.prompt.clone());
            metadata(&mut next, model, effort, prompt);
            if kept_prompt.is_some() {
                next.prompt = kept_prompt;
            }
            next
        }),
        SubagentReport::Started { id, description, agent_type, session_scoped, model, effort, prompt } => update_subagent(state, thread_id, id, |existing| {
            let mut next = existing.cloned().unwrap_or_else(|| new_subagent(id, description));
            next.finished_at = None;
            next.last_tool_name = None;
            next.stopping = false;
            let kept_prompt = existing.and_then(|existing| existing.prompt.clone());
            metadata(&mut next, model, effort, prompt);
            if kept_prompt.is_some() {
                next.prompt = kept_prompt;
            }
            next.description = description.clone();
            if agent_type.as_ref().is_some_and(|kind| !kind.is_empty()) {
                next.agent_type = agent_type.clone();
            }
            if *session_scoped {
                next.session_scoped = true;
            }
            next.status = SubagentStatus::Working;
            next.started_at = existing.map(|existing| existing.started_at).unwrap_or_else(now);
            next
        }),
        SubagentReport::Status { id, status, summary } => update_subagent(state, thread_id, id, |existing| {
            let mut next = existing.cloned().unwrap_or_else(|| new_subagent(id, "Subagent"));
            next.finished_at = None;
            if *status == SubagentStatus::Working {
                next.last_tool_name = None;
                next.status = SubagentStatus::Working;
            } else {
                next.stopping = false;
                next.status = SubagentStatus::Idle;
            }
            if summary.as_ref().is_some_and(|summary| !summary.is_empty()) {
                next.summary = summary.clone();
            }
            next
        }),
        SubagentReport::Progress { id, description, agent_type, last_tool_name, summary, total_tokens } => update_subagent(state, thread_id, id, |existing| {
            let mut next = existing.cloned().unwrap_or_else(|| new_subagent(id, description));
            next.description = description.clone();
            if agent_type.as_ref().is_some_and(|kind| !kind.is_empty()) {
                next.agent_type = agent_type.clone();
            }
            if last_tool_name.as_ref().is_some_and(|tool| !tool.is_empty()) {
                next.last_tool_name = last_tool_name.clone();
            }
            if summary.as_ref().is_some_and(|summary| !summary.is_empty()) {
                next.summary = summary.clone();
            }
            next.total_tokens = Some(existing.and_then(|existing| existing.total_tokens).unwrap_or(0).max(*total_tokens));
            next
        }),
        SubagentReport::Activity { id, activity_id, kind, title, text } => update_subagent(state, thread_id, id, |existing| {
            let base = existing.cloned().unwrap_or_else(|| new_subagent(id, "Subagent"));
            if base.activity.iter().any(|item| &item.id == activity_id) {
                return base;
            }
            let mut next = base;
            next.activity.push(SubagentActivity { id: activity_id.clone(), kind: *kind, title: title.clone().filter(|title| !title.is_empty()), text: text.clone(), at: now() });
            next
        }),
        SubagentReport::Finished { id, status, summary } => update_subagent(state, thread_id, id, |existing| {
            let mut next = existing.cloned().unwrap_or_else(|| new_subagent(id, "Subagent"));
            next.stopping = false;
            next.status = *status;
            next.summary = if summary.is_empty() { existing.and_then(|existing| existing.summary.clone()) } else { Some(summary.clone()) };
            next.finished_at = Some(now());
            next
        }),
    }
}

fn update_workflow(state: &mut WorkspaceState, thread_id: &str, id: &str, update: impl FnOnce(Option<&Workflow>) -> Workflow) {
    let mut workflows = state.workflows.get(thread_id).cloned().unwrap_or_default();
    match workflows.iter().position(|workflow| workflow.id == id) {
        Some(index) => workflows[index] = update(Some(&workflows[index])),
        None => workflows.push(update(None)),
    }
    with_workflows(state, thread_id, workflows);
}

fn blank_workflow(id: &str) -> Workflow {
    Workflow { id: id.into(), name: id.into(), description: "Dynamic workflow".into(), status: WorkflowStatus::Running, phases: Vec::new(), agents: Vec::new(), total_tokens: 0, total_tool_calls: 0, started_at: now(), finished_at: None, summary: None, stopping: false }
}

/// What the agent process reports about work that outlives the run that started it.
pub fn apply_thread_event(state: &mut WorkspaceState, event: &ThreadEvent) {
    let task_id = event.task_id.as_str();
    match &event.report {
        ThreadReport::Subagent(report) => apply_subagent_report(state, task_id, report),
        ThreadReport::BackgroundChanged { processes } => {
            let stopping: std::collections::HashSet<String> = state.background_processes.get(task_id).into_iter().flatten().filter(|process| process.stopping).map(|process| process.id.clone()).collect();
            let processes: Vec<_> = processes.iter().cloned().map(|mut process| {
                if stopping.contains(&process.id) {
                    process.stopping = true;
                }
                process
            }).collect();
            if processes.is_empty() {
                state.background_processes.shift_remove(task_id);
            } else {
                state.background_processes.insert(task_id.into(), processes);
            }
        }
        ThreadReport::GoalChanged { goal } => match goal {
            Some(goal) => {
                state.goals.insert(task_id.into(), goal.clone());
            }
            None => {
                state.goals.shift_remove(task_id);
            }
        },
        ThreadReport::ResourceStopped { .. } => {}
        ThreadReport::WorkflowStarted { id, name, description } => update_workflow(state, task_id, id, |existing| {
            let mut next = existing.cloned().unwrap_or_else(|| blank_workflow(id));
            next.name = name.clone();
            next.description = description.clone();
            next.status = WorkflowStatus::Running;
            next
        }),
        ThreadReport::WorkflowProgress { id, phases, agents, total_tokens, total_tool_calls } => update_workflow(state, task_id, id, |existing| {
            let mut next = existing.cloned().unwrap_or_else(|| blank_workflow(id));
            next.phases = phases.clone();
            next.agents = agents.clone();
            next.total_tokens = *total_tokens;
            next.total_tool_calls = *total_tool_calls;
            next
        }),
        ThreadReport::WorkflowFinished { id, status, summary } => update_workflow(state, task_id, id, |existing| {
            let mut next = existing.cloned().unwrap_or_else(|| blank_workflow(id));
            next.status = *status;
            next.finished_at = Some(now());
            next.stopping = false;
            if !summary.is_empty() {
                next.summary = Some(summary.clone());
            }
            next
        }),
    }
}

/// How a thread reads once its turn ended with `status`.
pub fn turn_end_status(status: RunStatus) -> ThreadRunStatus {
    if status == RunStatus::Cancelled {
        ThreadRunStatus::Stopped
    } else {
        ThreadRunStatus::Idle
    }
}

/// The thread a turn ending with `status` leaves: no run open, and how and when it ended.
pub fn ended_turn(thread: &mut Thread, status: RunStatus) {
    thread.run_open = false;
    let ended = run_outcome(status);
    if let Some(last) = thread.messages.last_mut() {
        if last.kind == MessageKind::Assistant && !last.artifact && !last.answer {
            last.answer = true;
        }
    }
    let at = now();
    thread.run_ended_at = Some(at);
    thread.ranked_at = at;
    if ended.is_some() {
        thread.run_ended_as = ended;
    }
}

/// A thread's turn ending, however it ends.
pub fn with_turn_ended(state: &mut WorkspaceState, task_id: &str, status: RunStatus) {
    with_run_status(state, task_id, turn_end_status(status));
    if let Some(thread) = state.thread_mut(task_id) {
        ended_turn(thread, status);
    }
}

/// Compaction does not replace how the last turn ended, its verdict, nor its report.
pub fn restored_testimony(thread: &mut Thread, before: &ThreadMark) {
    thread.outcome = before.outcome;
    thread.outcome_unread = before.outcome_unread;
    thread.run_ended_at = before.run_ended_at;
    thread.run_ended_as = before.run_ended_as;
    if let Some(ranked) = before.ranked_at {
        thread.ranked_at = ranked;
    }
    if before.report.is_some() {
        thread.report = before.report.clone();
    }
}

/// A run reaching its end: the run record goes, and what it left behind is settled on its thread.
fn apply_run_finished(state: &mut WorkspaceState, event: &RunEvent, status: RunStatus, message: Option<&str>) {
    let task_id = event.task_id.as_str();
    with_streaming_tail(state, task_id, None);
    state.active_runs.shift_remove(task_id);
    with_turn_ended(state, task_id, status);
    state.approvals.shift_remove(&event.run_id);
    if let Some(subagents) = state.subagents.get(task_id).cloned() {
        if subagents.iter().any(|subagent| subagent.status == SubagentStatus::Working && !subagent.session_scoped) {
            let ended = match status {
                RunStatus::Succeeded => SubagentStatus::Completed,
                RunStatus::Failed => SubagentStatus::Failed,
                _ => SubagentStatus::Stopped,
            };
            let subagents = subagents.into_iter().map(|mut subagent| {
                if subagent.status == SubagentStatus::Working && !subagent.session_scoped {
                    subagent.status = ended;
                    subagent.finished_at = Some(now());
                }
                subagent
            }).collect();
            with_subagents(state, task_id, subagents);
        }
    }
    if status != RunStatus::Succeeded {
        if let Some(workflows) = state.workflows.get_mut(task_id) {
            for workflow in workflows.iter_mut().filter(|workflow| workflow.status == WorkflowStatus::Running) {
                workflow.status = WorkflowStatus::Stopped;
                workflow.finished_at = Some(now());
                workflow.stopping = false;
            }
        }
    }
    if status == RunStatus::Failed {
        if let Some(message) = message {
            if let Some(thread) = state.thread_mut(task_id) {
                thread.messages.push(Shared::new(create_failure_message(message)));
                thread.updated_at = now();
            }
        }
    }
}

/// The message a block of a streamed answer belongs to: a new part once another message lands after it.
fn answer_part(run: &ActiveRun, last: Option<&ConversationMessage>, message_id: &str) -> (String, i64) {
    let Some(parts) = run.answer_parts.as_ref().and_then(|parts| parts.get(message_id)).copied() else { return (message_id.to_string(), 0) };
    let id = if parts > 0 { format!("{message_id}:{parts}") } else { message_id.to_string() };
    if last.is_some_and(|last| last.id == id) {
        (id, parts)
    } else {
        (format!("{message_id}:{}", parts + 1), parts + 1)
    }
}

/// The message the latest block of a streamed answer landed on.
pub fn answer_part_id(run: Option<&ActiveRun>, message_id: &str) -> String {
    match run.and_then(|run| run.answer_parts.as_ref()).and_then(|parts| parts.get(message_id)) {
        Some(parts) if *parts > 0 => format!("{message_id}:{parts}"),
        _ => message_id.to_string(),
    }
}

fn locale_number(value: i64) -> String {
    let digits = value.abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn update_thread(state: &mut WorkspaceState, task_id: &str, update: impl FnOnce(&mut Thread)) {
    if let Some(thread) = state.thread_mut(task_id) {
        update(thread);
    }
}

/// Folds one run report into its thread and the run's own record. Reports for a run that is not the
/// thread's live one, or that arrive out of order, are dropped. `status` overrides a terminal status.
pub fn apply_run_event(state: &mut WorkspaceState, event: &RunEvent) {
    let task_id = event.task_id.as_str();
    let Some(found) = state.active_runs.get(task_id) else { return };
    if event.run_id != found.run_id || event.sequence <= found.sequence {
        return;
    }
    let through_retry = matches!(found.phase, RunPhase::Retrying { .. })
        && !matches!(event.report, RunReport::Retrying { .. } | RunReport::QueuedDelivered { .. } | RunReport::QueuedSteerFailed { .. } | RunReport::ContextSpent { .. });
    if through_retry {
        with_run_phase(state, task_id, RunPhase::Running);
    }
    if let Some(run) = state.active_runs.get_mut(task_id) {
        run.sequence = event.sequence;
    }
    let active = state.active_runs.get(task_id).cloned().expect("run present");
    match &event.report {
        RunReport::Retrying { notice } => with_run_phase(state, task_id, RunPhase::Retrying { retry: notice.clone() }),
        RunReport::RunStarted { .. } => {
            let carried: Vec<Workflow> = state.workflows.get(task_id).into_iter().flatten().filter(|workflow| workflow.status == WorkflowStatus::Running).cloned().collect();
            with_streaming_tail(state, task_id, None);
            with_workflows(state, task_id, carried);
        }
        RunReport::RunStatus { status, message, .. } => match status {
            RunStatus::Running => with_approval_answered(state, task_id),
            RunStatus::AwaitingApproval => with_run_phase(state, task_id, RunPhase::AwaitingApproval),
            _ => apply_run_finished(state, event, *status, message.as_deref()),
        },
        RunReport::QuestionRequested { request_id, request } => {
            let questions: Vec<PendingQuestion> = request
                .questions
                .iter()
                .map(|question| PendingQuestion { question: question.clone(), run_id: event.run_id.clone(), request_id: request_id.clone(), question_id: question.id.clone(), blocking: request.blocking, answer: None, submitting: false })
                .collect();
            let messages: Vec<ConversationMessage> = questions
                .iter()
                .map(|question| {
                    let mut text = question.question.question.clone();
                    if !question.question.options.is_empty() {
                        let options: Vec<String> = question.question.options.iter().map(|option| if option.description.is_empty() { format!("- {}", option.label) } else { format!("- {}: {}", option.label, option.description) }).collect();
                        text.push_str("\n\n");
                        text.push_str(&options.join("\n"));
                    }
                    create_conversation_message(MessageKind::Assistant, text, None)
                })
                .collect();
            if let Some(run) = state.active_runs.get_mut(task_id) {
                run.questions.get_or_insert_with(Vec::new).extend(questions);
            }
            update_thread(state, task_id, |thread| {
                thread.messages.extend((messages).into_iter().map(Shared::new));
                thread.updated_at = now();
            });
        }
        RunReport::QuestionAnswered { request_id, question_id, text } => {
            if let Some(run) = state.active_runs.get_mut(task_id) {
                let kept: Vec<PendingQuestion> = active.questions.clone().unwrap_or_default().into_iter().filter(|question| &question.request_id != request_id || &question.question_id != question_id).collect();
                run.questions = Some(kept);
            }
            let text = text.clone();
            update_thread(state, task_id, |thread| {
                thread.messages.push(Shared::new(create_conversation_message(MessageKind::User, text, None)));
                thread.updated_at = now();
            });
        }
        RunReport::QuestionClosed { request_id } => {
            if let Some(run) = state.active_runs.get_mut(task_id) {
                let kept: Vec<PendingQuestion> = active.questions.clone().unwrap_or_default().into_iter().filter(|question| &question.request_id != request_id).collect();
                run.questions = Some(kept);
            }
        }
        RunReport::AssistantTail { message_id, text } => {
            if text.is_empty() {
                with_streaming_tail(state, task_id, None);
            } else {
                let last = state.thread(task_id).and_then(|thread| thread.messages.last());
                let (id, _) = answer_part(&active, last.map(|last| &**last), message_id);
                with_streaming_tail(state, task_id, Some(StreamingTail { message_id: id, text: text.clone() }));
            }
        }
        RunReport::AssistantDelta { message_id, text, append, artifact } => {
            let last = state.thread(task_id).and_then(|thread| thread.messages.last()).cloned();
            let (part_id, parts) = if *artifact { (message_id.clone(), 0) } else { answer_part(&active, last.as_deref(), message_id) };
            let tracked = *artifact || active.answer_parts.as_ref().and_then(|held| held.get(message_id)).copied() == Some(parts);
            if !tracked {
                if let Some(run) = state.active_runs.get_mut(task_id) {
                    run.answer_parts.get_or_insert_with(Default::default).insert(message_id.clone(), parts);
                }
            }
            if !artifact {
                with_streaming_tail(state, task_id, None);
            }
            let append = append.unwrap_or(false);
            let artifact = *artifact;
            let continues = last.as_ref().is_some_and(|last| last.kind == MessageKind::Assistant && last.id == part_id);
            let after_answer = last.as_ref().is_some_and(|last| last.kind == MessageKind::Assistant && !last.artifact);
            drop(last);
            update_thread(state, task_id, |thread| {
                match continues {
                    true => {
                        if let Some(tail) = thread.messages.last_mut() {
                            if !append {
                                tail.text.push('\n');
                            }
                            tail.text.push_str(text);
                        }
                    }
                    false => {
                        let mut message = crate::domain::conversation::conversation_message(part_id.clone(), MessageKind::Assistant, text.clone(), None);
                        message.artifact = artifact;
                        if artifact && after_answer {
                            let at = thread.messages.len() - 1;
                            thread.messages.insert(at, Shared::new(message));
                        } else {
                            thread.messages.push(Shared::new(message));
                        }
                    }
                }
                thread.updated_at = now();
            });
        }
        RunReport::ContextUsage { tokens, limit, model } => {
            if active.warming {
                if let Some(run) = state.active_runs.get_mut(task_id) {
                    run.warming = false;
                }
            }
            let usage = ContextUsage { tokens: *tokens, limit: *limit, model: model.clone() };
            update_thread(state, task_id, |thread| {
                if thread.context_usage.as_ref() != Some(&usage) {
                    thread.context_usage = Some(usage);
                }
            });
        }
        RunReport::ContextSpent { tokens } => {
            let tokens = *tokens;
            update_thread(state, task_id, |thread| {
                let Some(model) = thread.model.clone().or_else(|| default_model_for(thread.engine).map(String::from)) else { return };
                let usage = thread.usage.get_or_insert_with(Default::default);
                let total = usage.get(&model).copied().unwrap_or(0).saturating_add(tokens).min(9_007_199_254_740_991);
                usage.insert(model, total);
            });
        }
        RunReport::CompactionStatus { compacting, error } => {
            with_run_phase(state, task_id, if *compacting { RunPhase::Compacting } else { RunPhase::Running });
            if let Some(error) = error {
                let error = error.clone();
                update_thread(state, task_id, |thread| {
                    thread.messages.push(Shared::new(create_failure_message(error)));
                    thread.updated_at = now();
                });
            }
        }
        RunReport::Compacted { trigger, pre_tokens, post_tokens } => {
            with_run_phase(state, task_id, RunPhase::Running);
            let trigger = match trigger {
                CompactionTrigger::Manual => "manual",
                CompactionTrigger::Auto => "auto",
            };
            let text = match post_tokens {
                None => format!("Context {trigger}-compacted at {} tokens.", locale_number(*pre_tokens)),
                Some(post) => format!("Context {trigger}-compacted: {} → {} tokens.", locale_number(*pre_tokens), locale_number(*post)),
            };
            let post = *post_tokens;
            update_thread(state, task_id, |thread| {
                with_memory_unheard(thread);
                thread.messages.push(Shared::new(create_conversation_message(MessageKind::System, text, None)));
                if let (Some(usage), Some(post)) = (thread.context_usage.as_mut(), post) {
                    usage.tokens = post;
                }
                thread.updated_at = now();
            });
        }
        RunReport::ToolIntent { intent } => {
            let detail = serde_json::to_string_pretty(&intent.input).unwrap_or_default();
            let name = intent.name.clone();
            update_thread(state, task_id, |thread| {
                thread.messages.push(Shared::new(create_conversation_message(MessageKind::Tool, name, Some(detail))));
                thread.updated_at = now();
            });
        }
        RunReport::ApprovalRequested { approval_id, intent, title, description } => {
            let input = match &intent.input {
                serde_json::Value::Object(fields) => fields.clone(),
                other => {
                    let mut wrapped = serde_json::Map::new();
                    wrapped.insert("value".into(), other.clone());
                    wrapped
                }
            };
            state.approvals.insert(
                event.run_id.clone(),
                ApprovalView { approval_id: approval_id.clone(), task_id: task_id.into(), run_id: event.run_id.clone(), title: title.clone(), description: description.clone(), tool_name: intent.name.clone(), input },
            );
        }
        RunReport::ContinuationUpdated { continuation } => {
            let continuation = continuation.clone();
            update_thread(state, task_id, |thread| {
                thread.inherited_continuation = false;
                thread.continuation = Some(continuation);
                thread.continuation_status = ContinuationStatus::Available;
                thread.updated_at = now();
            });
        }
        RunReport::ContinuationLost => update_thread(state, task_id, |thread| {
            thread.continuation = None;
            thread.continuation_status = ContinuationStatus::Invalid;
            thread.updated_at = now();
        }),
        RunReport::ComputerUseSetupRequired | RunReport::QueuedDelivered { .. } | RunReport::QueuedSteerFailed { .. } => {}
    }
}
