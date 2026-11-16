//! Shape checks on stored JSON, ported one for one from the TypeScript guards (`isProject`,
//! `isWorktree`, `isAutomation`, `isThreadBrief`, …). Each reads the raw value exactly as the
//! TypeScript build does, so both builds accept and refuse the same records.

use serde_json::{Map, Value};

use super::catalog;
use super::js::{self, absent, finite, is_true, non_empty_string, record, safe_integer, string};

pub const MAX_BRIEF_FIELD: usize = 4_000;
pub const MAX_SUMMARY: usize = 500;
pub const MAX_QUESTION: usize = 300;
pub const MAX_DECISION_CONTEXT: usize = 2_000;
pub const MAX_OPTION_LABEL: usize = 120;
pub const MAX_OPTION_DESCRIPTION: usize = 300;
pub const MAX_OPTIONS: usize = 5;
pub const MAX_ANSWER: usize = 4_000;
pub const MAX_AUTOMATION_PROMPT: usize = 100_000;
pub const MAX_AUTOMATION_SCHEDULE: usize = 200;
pub const MAX_AUTOMATION_TIMEZONE: usize = 100;
pub const MAX_SURFACE_WHEN: usize = 500;
pub const MAX_ENDS_WHEN: usize = 500;

const PULL_REQUEST_STATES: [&str; 4] = ["draft", "open", "merged", "closed"];
const PULL_REQUEST_CHECKS: [&str; 3] = ["pending", "passing", "failing"];
const PULL_REQUEST_REVIEWS: [&str; 3] = ["approved", "changes-requested", "required"];
const DELIVERIES: [&str; 3] = ["pull-request", "commit", "report"];
const COORDINATION_STATES: [&str; 4] = ["working", "blocked", "done", "failed"];
const AUTOMATION_RUN_STATUSES: [&str; 5] = ["succeeded", "failed", "cancelled", "skipped", "missed"];
const SUBAGENT_STATUSES: [&str; 5] = ["working", "idle", "completed", "failed", "stopped"];
const MESSAGE_KINDS: [&str; 4] = ["user", "assistant", "tool", "system"];

fn str_in(value: Option<&Value>, allowed: &[&str]) -> bool {
    matches!(value, Some(Value::String(text)) if allowed.contains(&text.as_str()))
}

fn str_with(value: Option<&Value>, check: impl Fn(&str) -> bool) -> bool {
    matches!(value, Some(Value::String(text)) if check(text))
}

/// `text(value, max)` in coordination.ts: a non-blank string no longer than `max`.
fn text(value: Option<&Value>, max: usize) -> bool {
    matches!(value, Some(Value::String(text)) if !js::trim(text).is_empty() && js::length(text) <= max)
}

/// `isText` in automation.ts: a non-empty string no longer than `max`.
fn automation_text(value: Option<&Value>, max: usize) -> bool {
    matches!(value, Some(Value::String(text)) if !text.is_empty() && js::length(text) <= max)
}

fn optional(value: Option<&Value>, check: impl Fn(Option<&Value>) -> bool) -> bool {
    absent(value) || check(value)
}

fn array_every(value: Option<&Value>, check: impl Fn(&Value) -> bool) -> bool {
    matches!(value, Some(Value::Array(items)) if items.iter().all(check))
}

fn non_negative(value: Option<&Value>) -> bool {
    value.and_then(Value::as_f64).is_some_and(|number| number >= 0.0)
}

fn positive(value: Option<&Value>) -> bool {
    finite(value) && value.and_then(Value::as_f64).is_some_and(|number| number > 0.0)
}

pub fn is_execution_policy(value: Option<&Value>) -> bool {
    str_with(value, catalog::is_execution_policy)
}

pub fn is_agent_engine(value: Option<&Value>) -> bool {
    str_with(value, catalog::is_agent_engine)
}

pub fn is_agent_effort(value: Option<&Value>) -> bool {
    str_with(value, catalog::is_agent_effort)
}

pub fn is_model_id(value: Option<&Value>) -> bool {
    str_with(value, catalog::is_model_id)
}

pub fn is_thread_role(value: Option<&Value>) -> bool {
    str_with(value, catalog::is_thread_role)
}

pub fn is_thread_outcome(value: Option<&Value>) -> bool {
    str_in(value, &["finished", "failed", "stopped"])
}

pub fn is_message_kind(value: Option<&Value>) -> bool {
    str_in(value, &MESSAGE_KINDS)
}

pub fn is_project(value: &Value) -> bool {
    let Some(project) = record(Some(value)) else { return false };
    non_empty_string(project.get("id"))
        && non_empty_string(project.get("root"))
        && optional(project.get("name"), non_empty_string)
        && optional(project.get("workspaceId"), non_empty_string)
        && optional(project.get("sortIndex"), finite)
        && optional(project.get("unlisted"), is_true)
}

pub fn is_worktree(value: &Value) -> bool {
    let Some(worktree) = record(Some(value)) else { return false };
    non_empty_string(worktree.get("id"))
        && optional(worktree.get("name"), non_empty_string)
        && non_empty_string(worktree.get("projectId"))
        && non_empty_string(worktree.get("root"))
        && non_empty_string(worktree.get("workspaceId"))
        && non_empty_string(worktree.get("baseCommit"))
        && finite(worktree.get("createdAt"))
        && finite(worktree.get("lastUsedAt"))
}

fn is_timestamp(value: Option<&Value>) -> bool {
    safe_integer(value) && non_negative(value)
}

pub fn is_automation(value: &Value) -> bool {
    let Some(automation) = record(Some(value)) else { return false };
    automation_text(automation.get("id"), 256)
        && automation_text(automation.get("taskId"), 256)
        && automation_text(automation.get("prompt"), MAX_AUTOMATION_PROMPT)
        && automation_text(automation.get("schedule"), MAX_AUTOMATION_SCHEDULE)
        && optional(automation.get("timezone"), |value| automation_text(value, MAX_AUTOMATION_TIMEZONE))
        && optional(automation.get("policy"), is_execution_policy)
        && optional(automation.get("surfaceWhen"), |value| automation_text(value, MAX_SURFACE_WHEN))
        && optional(automation.get("endsWhen"), |value| automation_text(value, MAX_ENDS_WHEN))
        && matches!(automation.get("paused"), Some(Value::Bool(_)))
        && is_timestamp(automation.get("createdAt"))
        && is_timestamp(automation.get("updatedAt"))
        && is_timestamp(automation.get("runCount"))
        && optional(automation.get("lastRunAt"), is_timestamp)
        && optional(automation.get("lastStatus"), |value| str_in(value, &AUTOMATION_RUN_STATUSES))
        && optional(automation.get("lastStatusAt"), is_timestamp)
        && optional(automation.get("consecutiveDeclines"), is_timestamp)
        && optional(automation.get("overrunCount"), is_timestamp)
}

pub fn is_thread_brief(value: Option<&Value>) -> bool {
    let Some(brief) = record(value) else { return false };
    text(brief.get("intent"), MAX_BRIEF_FIELD)
        && text(brief.get("doneWhen"), MAX_BRIEF_FIELD)
        && str_in(brief.get("delivers"), &DELIVERIES)
}

pub fn is_thread_model_choice(value: Option<&Value>) -> bool {
    let Some(choice) = record(value) else { return false };
    optional(choice.get("engine"), is_agent_engine)
        && is_model_id(choice.get("model"))
        && optional(choice.get("effort"), is_agent_effort)
}

fn is_memory_note(value: &Value) -> bool {
    let Some(note) = record(Some(value)) else { return false };
    text(note.get("id"), 200) && string(note.get("text")) && finite(note.get("at"))
}

pub fn is_coordinator_memory(value: Option<&Value>) -> bool {
    let Some(memory) = record(value) else { return false };
    optional(memory.get("purpose"), string)
        && optional(memory.get("purposeCleared"), safe_integer)
        && array_every(memory.get("notes"), is_memory_note)
        && safe_integer(memory.get("revision"))
        && non_negative(memory.get("revision"))
        && optional(memory.get("heard"), finite)
}

fn is_proposal_place(value: Option<&Value>) -> bool {
    let Some(place) = record(value) else { return false };
    optional(place.get("project"), string)
        && optional(place.get("worktree"), |value| matches!(value, Some(Value::Bool(_))))
        && optional(place.get("worktreeId"), string)
        && optional(place.get("computer"), |value| {
            record(value).is_some_and(|computer| text(computer.get("id"), 256) && string(computer.get("name")))
        })
}

pub fn is_thread_proposal(value: &Value) -> bool {
    let Some(proposal) = record(Some(value)) else { return false };
    text(proposal.get("id"), 200)
        && string(proposal.get("title"))
        && optional(proposal.get("named"), is_true)
        && string(proposal.get("prompt"))
        && optional(proposal.get("brief"), is_thread_brief)
        && is_proposal_place(proposal.get("place"))
        && optional(proposal.get("engine"), is_agent_engine)
        && optional(proposal.get("model"), is_model_id)
        && optional(proposal.get("effort"), is_agent_effort)
        && optional(proposal.get("role"), is_thread_role)
        && optional(proposal.get("from"), |value| text(value, 200))
        && finite(proposal.get("proposedAt"))
        && optional(proposal.get("error"), string)
        && optional(proposal.get("carried"), is_true)
}

fn is_remote_sender(value: Option<&Value>) -> bool {
    let Some(sender) = record(value) else { return false };
    matches!(sender.get("threadId"), Some(Value::String(id)) if !id.is_empty())
        && string(sender.get("title"))
        && string(sender.get("computer"))
}

pub fn is_held_lead(value: Option<&Value>) -> bool {
    if !is_remote_sender(value) {
        return false;
    }
    let lead = record(value).expect("checked above");
    optional(lead.get("proposalId"), |value| text(value, 200))
        && optional(lead.get("away"), is_true)
        && optional(lead.get("backAt"), finite)
}

fn values_every(value: Option<&Value>, check: impl Fn(&Value) -> bool) -> bool {
    record(value).is_some_and(|map| map.values().all(check))
}

pub fn is_remote_heard(value: Option<&Value>) -> bool {
    values_every(value, |heard| finite(Some(heard)))
}

pub fn is_remote_stops(value: Option<&Value>) -> bool {
    values_every(value, |stop| {
        record(Some(stop)).is_some_and(|stop| {
            string(stop.get("computer"))
                && optional(stop.get("runId"), string)
                && optional(stop.get("answer"), |answer| {
                    record(answer).is_some_and(|answer| finite(answer.get("at")) && optional(answer.get("runId"), string))
                })
        })
    })
}

pub fn is_caught_starts(value: Option<&Value>) -> bool {
    values_every(value, |start| {
        record(Some(start)).is_some_and(|start| string(start.get("computer")) && optional(start.get("threadId"), string))
    })
}

pub fn is_remote_working(value: Option<&Value>) -> bool {
    values_every(value, |work| {
        record(Some(work)).is_some_and(|work| string(work.get("computer")) && string(work.get("title")))
    })
}

pub fn is_coordination_report(value: Option<&Value>) -> bool {
    let Some(report) = record(value) else { return false };
    str_in(report.get("state"), &COORDINATION_STATES)
        && text(report.get("summary"), MAX_SUMMARY)
        && finite(report.get("at"))
}

fn is_decision_option(value: &Value) -> bool {
    let Some(option) = record(Some(value)) else { return false };
    text(option.get("label"), MAX_OPTION_LABEL)
        && optional(option.get("description"), |value| text(value, MAX_OPTION_DESCRIPTION))
        && optional(option.get("recommended"), is_true)
}

fn is_decision_shape(decision: &Map<String, Value>) -> bool {
    text(decision.get("question"), MAX_QUESTION)
        && optional(decision.get("context"), |value| text(value, MAX_DECISION_CONTEXT))
        && matches!(decision.get("options"), Some(Value::Array(options)) if options.len() <= MAX_OPTIONS && options.iter().all(is_decision_option))
}

pub fn is_decision(value: &Value) -> bool {
    let Some(decision) = record(Some(value)) else { return false };
    is_decision_shape(decision)
        && text(decision.get("id"), 200)
        && finite(decision.get("raisedAt"))
        && optional(decision.get("answer"), |value| text(value, MAX_ANSWER))
        && optional(decision.get("answeredAt"), finite)
}

pub fn is_coordination_note(value: &Value) -> bool {
    let Some(note) = record(Some(value)) else { return false };
    text(note.get("id"), 200)
        && text(note.get("threadId"), 200)
        && string(note.get("text"))
        && finite(note.get("at"))
        && optional(note.get("urgent"), is_true)
        && optional(note.get("runId"), |value| text(value, 200))
        && optional(note.get("quiet"), is_true)
}

pub fn is_pull_request_link(value: Option<&Value>) -> bool {
    let Some(link) = record(value) else { return false };
    js::integer(link.get("number"))
        && positive(link.get("number"))
        && matches!(link.get("url"), Some(Value::String(url)) if !url.is_empty())
        && optional(link.get("state"), |value| str_in(value, &PULL_REQUEST_STATES))
        && optional(link.get("title"), string)
        && optional(link.get("checks"), |value| str_in(value, &PULL_REQUEST_CHECKS))
        && optional(link.get("review"), |value| str_in(value, &PULL_REQUEST_REVIEWS))
        && optional(link.get("conflicts"), is_true)
}

pub fn is_thread_usage(value: Option<&Value>) -> bool {
    let Some(usage) = record(value) else { return false };
    usage.iter().all(|(model, tokens)| catalog::is_model_id(model) && safe_integer(Some(tokens)) && non_negative(Some(tokens)))
}

pub fn is_resource_stop(value: Option<&Value>) -> bool {
    let Some(stop) = record(value) else { return false };
    safe_integer(stop.get("at")) && non_negative(stop.get("at")) && safe_integer(stop.get("bytes")) && non_negative(stop.get("bytes")) && str_in(stop.get("cleanup"), &["stopping", "complete", "incomplete"])
}

pub fn is_limit_pause(value: Option<&Value>) -> bool {
    let Some(pause) = record(value) else { return false };
    positive(pause.get("resetsAt"))
        && str_in(pause.get("window"), &["session", "weekly"])
        && positive(pause.get("pausedAt"))
        && optional(pause.get("nudgedAt"), positive)
        && optional(pause.get("workflow"), is_true)
        && optional(pause.get("held"), |value| matches!(value, Some(Value::Array(_))))
}

pub fn is_automation_finding(value: &Value) -> bool {
    let Some(finding) = record(Some(value)) else { return false };
    non_empty_string(finding.get("id"))
        && non_empty_string(finding.get("headline"))
        && optional(finding.get("detail"), string)
        && optional(finding.get("key"), non_empty_string)
        && finite(finding.get("at"))
        && optional(finding.get("read"), is_true)
}

pub fn is_context_usage(value: Option<&Value>) -> bool {
    let Some(usage) = record(value) else { return false };
    finite(usage.get("tokens")) && non_negative(usage.get("tokens")) && positive(usage.get("limit")) && non_empty_string(usage.get("model"))
}

pub fn is_continuation(value: Option<&Value>) -> bool {
    let Some(continuation) = record(value) else { return false };
    non_empty_string(continuation.get("provider")) && non_empty_string(continuation.get("value"))
}

fn is_activity(value: &Value) -> bool {
    let Some(item) = record(Some(value)) else { return false };
    non_empty_string(item.get("id"))
        && str_in(item.get("kind"), &["text", "tool"])
        && optional(item.get("title"), string)
        && string(item.get("text"))
        && finite(item.get("at"))
}

pub fn is_subagent(value: &Value) -> bool {
    let Some(subagent) = record(Some(value)) else { return false };
    non_empty_string(subagent.get("id"))
        && string(subagent.get("description"))
        && optional(subagent.get("agentType"), string)
        && optional(subagent.get("sessionScoped"), is_true)
        && str_in(subagent.get("status"), &SUBAGENT_STATUSES)
        && optional(subagent.get("lastToolName"), string)
        && optional(subagent.get("summary"), string)
        && optional(subagent.get("totalTokens"), |value| finite(value) && non_negative(value))
        && finite(subagent.get("startedAt"))
        && optional(subagent.get("finishedAt"), finite)
        && array_every(subagent.get("activity"), is_activity)
}

pub fn is_message_origin(value: Option<&Value>) -> bool {
    let Some(origin) = record(value) else { return false };
    match origin.get("kind").and_then(Value::as_str) {
        Some("thread") => {
            string(origin.get("title"))
                && optional(origin.get("threadId"), non_empty_string)
                && optional(origin.get("computer"), non_empty_string)
        }
        Some("automation") => finite(origin.get("runNumber")),
        Some("coordination") => true,
        Some("legacy") => string(origin.get("label")),
        _ => false,
    }
}

fn is_annotation(value: &Value) -> bool {
    let Some(annotation) = record(Some(value)) else { return false };
    non_empty_string(annotation.get("id")) && non_empty_string(annotation.get("quote")) && string(annotation.get("note"))
}

fn is_pasted_text(value: &Value) -> bool {
    let Some(paste) = record(Some(value)) else { return false };
    non_empty_string(paste.get("id")) && string(paste.get("text"))
}

fn is_attached_file(value: &Value) -> bool {
    let Some(file) = record(Some(value)) else { return false };
    non_empty_string(file.get("id"))
        && non_empty_string(file.get("path"))
        && non_empty_string(file.get("name"))
        && optional(file.get("folder"), is_true)
}

fn non_empty_strings(value: Option<&Value>) -> bool {
    array_every(value, |item| non_empty_string(Some(item)))
}

pub fn is_conversation_message(value: &Value) -> bool {
    let Some(message) = record(Some(value)) else { return false };
    non_empty_string(message.get("id"))
        && is_message_kind(message.get("kind"))
        && string(message.get("text"))
        && optional(message.get("artifact"), is_true)
        && optional(message.get("answer"), is_true)
        && optional(message.get("origin"), |origin| {
            message.get("kind").and_then(Value::as_str) == Some("user") && is_message_origin(origin)
        })
        && optional(message.get("detail"), string)
        && optional(message.get("tone"), |tone| str_in(tone, &["error"]))
        && optional(message.get("attachments"), non_empty_strings)
        && optional(message.get("annotations"), |value| array_every(value, is_annotation))
        && optional(message.get("pastes"), |value| array_every(value, is_pasted_text))
        && optional(message.get("files"), |value| array_every(value, is_attached_file))
        && optional(message.get("withdrawn"), is_true)
        && finite(message.get("at"))
}

pub fn is_held_message(value: &Value) -> bool {
    let Some(message) = record(Some(value)) else { return false };
    non_empty_string(message.get("id"))
        && string(message.get("text"))
        && string(message.get("prompt"))
        && non_empty_strings(message.get("attachments"))
        && optional(message.get("annotations"), |value| array_every(value, is_annotation))
        && optional(message.get("pastes"), |value| array_every(value, is_pasted_text))
        && optional(message.get("files"), |value| array_every(value, is_attached_file))
        && optional(message.get("origin"), is_message_origin)
        && optional(message.get("detail"), string)
        && optional(message.get("answers"), non_empty_string)
        && optional(message.get("usersOwn"), is_true)
}
