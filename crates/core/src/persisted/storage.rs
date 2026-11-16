//! Reading stored threads the way `src/domain/thread-storage.ts` does: migrations from older
//! builds, fields this build cannot read dropped one by one, and a thread it cannot read at all
//! counted and left on disk rather than failing the store.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::catalog;
use super::js::{self, absent, finite, non_empty_string, record};
use super::shapes::*;
use super::store::{LoadedTaskStore, StoredThread, ThreadStoreData, THREAD_STORE_VERSION};

/// The store's values once decoded from their transport, before validation.
#[derive(Clone, Debug, Default)]
pub struct DecodedThreadStore {
    pub tasks: Value,
    pub projects: Value,
    /// `Null` in everything written before checkouts had records of their own.
    pub worktrees: Value,
    pub last_folder: Value,
}

/// The store as raw records that passed validation, with the threads left out counted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValidatedThreadStore {
    pub tasks: Vec<Value>,
    pub projects: Vec<Value>,
    pub worktrees: Vec<Value>,
    pub last_folder: Option<String>,
    pub hidden_tasks: usize,
}

/// Validates decoded v2 values. A thread this build cannot read is counted and left out; a
/// project, checkout or last folder it cannot read fails the whole store.
pub fn validate_thread_store_data(values: DecodedThreadStore) -> Result<ValidatedThreadStore, Vec<String>> {
    let mut errors = Vec::new();
    let mut hidden_tasks = 0;
    if !values.tasks.is_array() {
        errors.push("v2 tasks must be an array".to_owned());
    }
    if !values.projects.is_array() {
        errors.push("v2 projects must be an array".to_owned());
    }
    if !values.worktrees.is_null() && !values.worktrees.is_array() {
        errors.push("v2 worktrees must be an array".to_owned());
    }
    if !values.last_folder.is_null() && !values.last_folder.is_string() {
        errors.push("v2 lastFolder must be a string or null".to_owned());
    }
    let projects: Vec<Value> = match &values.projects {
        Value::Array(items) => items.iter().filter(|item| is_project(item)).cloned().collect(),
        _ => Vec::new(),
    };
    let stored: Vec<Value> = match &values.worktrees {
        Value::Array(items) => {
            let valid: Vec<Value> = items.iter().filter(|item| is_worktree(item)).cloned().collect();
            if valid.len() != items.len() {
                errors.push("v2 worktrees contains an invalid value".to_owned());
            }
            valid
        }
        _ => Vec::new(),
    };
    let mut lifted = Vec::new();
    let readable: Vec<Option<Value>> = match values.tasks {
        Value::Array(items) => items.into_iter().map(|item| sanitize_v2_thread(item, &mut lifted)).collect(),
        _ => Vec::new(),
    };
    let total = readable.len();
    let tasks: Vec<Value> = readable.into_iter().flatten().collect();
    hidden_tasks += total - tasks.len();
    if let Value::Array(items) = &values.projects {
        if projects.len() != items.len() {
            errors.push("v2 projects contains an invalid value".to_owned());
        }
    }
    let project_ids: HashSet<&str> = projects.iter().filter_map(|project| project["id"].as_str()).collect();
    let task_count = tasks.len();
    let placed: Vec<Value> = tasks
        .into_iter()
        .filter(|task| match task.get("projectId") {
            Some(id) if js::truthy(Some(id)) => id.as_str().is_some_and(|id| project_ids.contains(id)),
            _ => true,
        })
        .collect();
    hidden_tasks += task_count - placed.len();
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut seen = HashSet::new();
    let worktrees: Vec<Value> = stored
        .into_iter()
        .chain(lifted)
        .filter(|worktree| seen.insert(worktree["id"].as_str().unwrap_or_default().to_owned()))
        .filter(|worktree| worktree["projectId"].as_str().is_some_and(|id| project_ids.contains(id)))
        .collect();
    let worktree_ids: HashSet<String> =
        worktrees.iter().filter_map(|worktree| worktree["id"].as_str().map(str::to_owned)).collect();
    Ok(ValidatedThreadStore {
        tasks: placed.into_iter().map(|task| claiming(task, &worktree_ids)).collect(),
        projects,
        worktrees,
        last_folder: values.last_folder.as_str().map(str::to_owned),
        hidden_tasks,
    })
}

/// Turns validated records into typed ones. A record that passed validation and still does not
/// fit its type is counted with the hidden threads, as one this build cannot read.
pub fn typed_store(validated: ValidatedThreadStore) -> Result<(ThreadStoreData, usize), String> {
    let mut hidden = validated.hidden_tasks;
    let mut tasks = Vec::with_capacity(validated.tasks.len());
    for task in validated.tasks {
        match serde_json::from_value::<StoredThread>(task) {
            Ok(task) => tasks.push(task),
            Err(_) => hidden += 1,
        }
    }
    let projects = validated
        .projects
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("v2 projects contains an invalid value: {error}"))?;
    let worktrees = validated
        .worktrees
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("v2 worktrees contains an invalid value: {error}"))?;
    Ok((
        ThreadStoreData { version: THREAD_STORE_VERSION, tasks, projects, worktrees, last_folder: validated.last_folder },
        hidden,
    ))
}

/// Validates and types a decoded store in one step.
pub fn load_thread_store(values: DecodedThreadStore) -> Result<LoadedTaskStore, String> {
    let validated = validate_thread_store_data(values).map_err(|errors| errors.join(" "))?;
    let (data, hidden_tasks) = typed_store(validated)?;
    Ok(LoadedTaskStore { data, hidden_tasks, unloaded_task_ids: None, resume_after_update: None })
}

/// A conversation loaded on its own goes through the same migrations and validation.
pub fn parse_stored_conversation_messages(values: Vec<Value>) -> Result<Vec<super::ConversationMessage>, String> {
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let message = renamed_message_fields(value);
            if !is_conversation_message(&message) {
                return Err(format!("Stored message {} is unreadable.", index + 1));
            }
            serde_json::from_value(message).map_err(|_| format!("Stored message {} is unreadable.", index + 1))
        })
        .collect()
}

/// Null for a thread this build cannot read, which the caller counts and leaves out. Checkouts a
/// thread carried inside itself are pushed onto `lifted`.
pub fn sanitize_v2_thread(raw: Value, lifted: &mut Vec<Value>) -> Option<Value> {
    let retired = without_unreadable_fields(renamed_fields(drop_retired_settings(recorded_engine(raw))), &[]);
    let mut value = if retired.is_object() { lift_embedded_worktree(retired, lifted) } else { retired };
    if !is_thread_base(&value) {
        return None;
    }
    let task = value.as_object_mut().expect("a thread is a record");
    match task.get("continuation") {
        Some(continuation) if !is_continuation(Some(continuation)) => {
            task.remove("continuation");
            task.insert("continuationStatus".into(), Value::from("invalid"));
            return Some(settle_stored_subagents(value));
        }
        Some(_) => {
            if task.get("continuationStatus").and_then(Value::as_str) != Some("available") {
                return None;
            }
        }
        None => {
            if !matches!(task.get("continuationStatus").and_then(Value::as_str), Some("none" | "invalid")) {
                return None;
            }
        }
    }
    Some(settle_stored_subagents(value))
}

fn recorded_engine(mut value: Value) -> Value {
    if let Value::Object(task) = &mut value {
        if !task.contains_key("engine") {
            task.insert("engine".into(), Value::from(catalog::UNRECORDED_ENGINE));
        }
    }
    value
}

/// Threads written before the picker dropped "default" and the context window fall back to the
/// current defaults; the attention dot they may carry is retired.
fn drop_retired_settings(mut value: Value) -> Value {
    let Value::Object(task) = &mut value else { return value };
    let default_model = task.get("model").and_then(Value::as_str) == Some("default");
    if !task.contains_key("contextWindow") && !task.contains_key("attention") && !task.contains_key("attentionRead") && !default_model {
        return value;
    }
    task.remove("contextWindow");
    task.remove("attention");
    task.remove("attentionRead");
    if default_model {
        task.remove("model");
    }
    value
}

fn current_model_value(value: &Value) -> Value {
    match value {
        Value::String(model) => Value::from(catalog::current_model(model)),
        other => other.clone(),
    }
}

/// Fields renamed since older builds wrote them; whatever the thread already carries under the
/// current name wins.
pub fn renamed_fields(value: Value) -> Value {
    let Value::Object(original) = value else { return value };
    let mut task = original.clone();
    if let Some(model) = task.get("model") {
        let current = current_model_value(model);
        if &current != model {
            task.insert("model".into(), current);
        }
    }
    if let Some(usage) = task.get("usage") {
        if let Some(current) = current_usage(usage) {
            task.insert("usage".into(), current);
        }
    }
    if let Some(Value::Object(choice)) = task.get("threadModel") {
        if let Some(model) = choice.get("model") {
            let current = current_model_value(model);
            if &current != model {
                let mut choice = choice.clone();
                choice.insert("model".into(), current);
                task.insert("threadModel".into(), Value::Object(choice));
            }
        }
    }
    if let Some(Value::Array(proposals)) = task.get_mut("proposals") {
        for proposal in proposals.iter_mut() {
            if let Value::Object(proposal) = proposal {
                if let Some(model) = proposal.get("model") {
                    let current = current_model_value(model);
                    if &current != model {
                        proposal.insert("model".into(), current);
                    }
                }
            }
        }
    }
    if original.contains_key("silencedKeys") {
        // Rebuilt from the record as it was read, as the TypeScript build does.
        let mut renamed = original.clone();
        let handled = renamed.remove("silencedKeys").expect("checked above");
        if !renamed.contains_key("handledIssues") {
            renamed.insert("handledIssues".into(), handled);
        }
        task = renamed;
    }
    if let Some(notes) = task.remove("crewNotes") {
        if !task.contains_key("coordinationNotes") {
            task.insert("coordinationNotes".into(), notes);
        }
    }
    if let Some(Value::Object(pause)) = task.get_mut("limitPause") {
        if let Some(Value::Array(held)) = pause.get_mut("held") {
            for message in held.iter_mut() {
                if message.is_object() {
                    *message = labelled_origin(std::mem::take(message));
                }
            }
        }
    }
    if let Some(Value::Array(messages)) = task.get_mut("messages") {
        for message in messages.iter_mut() {
            *message = renamed_message_fields(std::mem::take(message));
        }
    }
    Value::Object(task)
}

/// What a thread spent, counted toward the model each spend is now known as. A malformed id is dropped.
fn current_usage(value: &Value) -> Option<Value> {
    let Value::Object(entries) = value else { return None };
    if entries.keys().all(|model| catalog::is_model_id(model) && catalog::current_model(model) == model) {
        return None;
    }
    let mut usage = Map::new();
    for (recorded, tokens) in entries {
        let model = catalog::current_model(recorded);
        if !catalog::is_model_id(model) {
            continue;
        }
        let total = match (usage.get(model), tokens) {
            (Some(Value::Number(counted)), Value::Number(tokens)) => {
                js::number_value(counted.as_f64().unwrap_or(0.0) + tokens.as_f64().unwrap_or(0.0))
            }
            _ => tokens.clone(),
        };
        usage.insert(model.to_owned(), total);
    }
    Some(Value::Object(usage))
}

/// Messages written while `withdrawn` was called `quiet`, and labelled ones given their origin.
pub fn renamed_message_fields(value: Value) -> Value {
    if !value.is_object() {
        return value;
    }
    let mut labelled = labelled_origin(value);
    let message = labelled.as_object_mut().expect("checked above");
    if let Some(quiet) = message.remove("quiet") {
        if !message.contains_key("withdrawn") {
            message.insert("withdrawn".into(), quiet);
        }
    }
    labelled
}

/// A user message an older build labelled gets the origin its label names.
pub fn labelled_origin(mut value: Value) -> Value {
    let Value::Object(message) = &mut value else { return value };
    let user = match message.get("kind") {
        None => true,
        Some(kind) => kind.as_str() == Some("user"),
    };
    if user && !message.contains_key("origin") {
        if let Some(Value::String(detail)) = message.get("detail") {
            let origin = legacy_origin(detail);
            message.insert("origin".into(), origin);
        }
    }
    value
}

const FROM: &str = "From ";
const SEPARATOR: &str = " · ";
const COORDINATION: &str = "Thread updates";

/// Where a message an older build labelled came from, read from the label it wrote.
pub fn legacy_origin(detail: &str) -> Value {
    let mut origin = Map::new();
    if detail == COORDINATION {
        origin.insert("kind".into(), Value::from("coordination"));
        return Value::Object(origin);
    }
    if let Some(digits) = detail.strip_prefix("Automation run #") {
        if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
            origin.insert("kind".into(), Value::from("automation"));
            origin.insert("runNumber".into(), js::number_value(digits.parse::<f64>().unwrap_or(f64::NAN)));
            return Value::Object(origin);
        }
    }
    if let Some(sender) = detail.strip_prefix(FROM) {
        origin.insert("kind".into(), Value::from("thread"));
        let end = sender.rfind(SEPARATOR);
        let thread_id = end.map(|end| &sender[end + SEPARATOR.len()..]).unwrap_or("");
        if !thread_id.is_empty() && !thread_id.chars().any(js::is_js_whitespace) {
            origin.insert("threadId".into(), Value::from(thread_id));
            origin.insert("title".into(), Value::from(&sender[..end.expect("an id follows a separator")]));
        } else {
            origin.insert("title".into(), Value::from(sender));
        }
        return Value::Object(origin);
    }
    origin.insert("kind".into(), Value::from("legacy"));
    origin.insert("label".into(), Value::from(detail));
    Value::Object(origin)
}

type Readable = fn(Option<&Value>) -> bool;

fn finite_field(value: Option<&Value>) -> bool {
    finite(value)
}

fn true_field(value: Option<&Value>) -> bool {
    js::is_true(value)
}

/// Fields a thread can do without, each checked on its own.
const DROPPABLE_FIELDS: [(&str, Readable); 15] = [
    ("resourceStop", is_resource_stop),
    ("brief", is_thread_brief),
    ("threadModel", is_thread_model_choice),
    ("memory", is_coordinator_memory),
    ("report", is_coordination_report),
    ("resolvedAt", finite_field),
    ("pullRequest", is_pull_request_link),
    ("muted", true_field),
    ("runOpen", true_field),
    ("usage", is_thread_usage),
    ("remoteLead", is_held_lead),
    ("remoteHeard", is_remote_heard),
    ("remoteWorking", is_remote_working),
    ("remoteStops", is_remote_stops),
    ("caughtStarts", is_caught_starts),
];

/// Lists a thread can do without, each item checked on its own.
type ReadableItem = fn(&Value) -> bool;

const DROPPABLE_ITEMS: [(&str, ReadableItem); 1] = [("proposals", is_thread_proposal)];

fn droppable_fields_readable(task: &Map<String, Value>) -> bool {
    DROPPABLE_FIELDS.iter().all(|(field, readable)| absent(task.get(*field)) || readable(task.get(*field)))
        && DROPPABLE_ITEMS.iter().all(|(field, readable)| match task.get(*field) {
            None => true,
            Some(Value::Array(items)) => items.iter().all(readable),
            Some(_) => false,
        })
}

/// Drops each field and list item this build cannot read, except those named in `as_sent`.
fn without_unreadable_fields(mut value: Value, as_sent: &[&str]) -> Value {
    let Value::Object(task) = &mut value else { return value };
    for (field, readable) in DROPPABLE_FIELDS {
        if as_sent.contains(&field) || absent(task.get(field)) || readable(task.get(field)) {
            continue;
        }
        task.remove(field);
    }
    for (field, readable) in DROPPABLE_ITEMS {
        if as_sent.contains(&field) {
            continue;
        }
        let Some(items) = task.get(field) else { continue };
        if matches!(items, Value::Array(items) if items.iter().all(readable)) {
            continue;
        }
        let kept: Vec<Value> = match task.remove(field) {
            Some(Value::Array(items)) => items.into_iter().filter(readable).collect(),
            _ => Vec::new(),
        };
        if !kept.is_empty() {
            task.insert(field.into(), Value::Array(kept));
        }
    }
    value
}

/// Threads written while a checkout belonged to exactly one thread carry it inside themselves.
fn lift_embedded_worktree(mut value: Value, lifted: &mut Vec<Value>) -> Value {
    let Value::Object(task) = &mut value else { return value };
    if !matches!(task.get("worktree"), Some(Value::Object(_))) {
        return value;
    }
    let Some(Value::Object(mut worktree)) = task.remove("worktree") else { unreachable!() };
    let entered_at = worktree.remove("enteredAt");
    match task.get("projectId") {
        Some(project) => worktree.insert("projectId".into(), project.clone()),
        None => worktree.remove("projectId"),
    };
    let candidate = Value::Object(worktree);
    if !is_worktree(&candidate) {
        return value;
    }
    task.insert("worktreeId".into(), candidate["id"].clone());
    if let Some(entered_at) = entered_at.filter(|entered_at| finite(Some(entered_at))) {
        task.insert("worktreeEnteredAt".into(), entered_at);
    }
    lifted.push(candidate);
    value
}

fn is_thread_base(value: &Value) -> bool {
    let Some(task) = record(Some(value)) else { return false };
    let optional = |field: &str, check: &dyn Fn(Option<&Value>) -> bool| absent(task.get(field)) || check(task.get(field));
    let engine = task.get("engine");
    let engine_name = engine.and_then(Value::as_str);
    droppable_fields_readable(task)
        && non_empty_string(task.get("id"))
        && non_empty_string(task.get("title"))
        && optional("titleByUser", &|value| matches!(value, Some(Value::Bool(_))))
        && optional("role", &is_thread_role)
        && optional("parentId", &non_empty_string)
        && optional("decisions", &|value| matches!(value, Some(Value::Array(items)) if items.iter().all(is_decision)))
        && optional("coordinationNotes", &|value| {
            matches!(value, Some(Value::Array(items)) if items.iter().all(is_coordination_note))
        })
        && optional("projectId", &non_empty_string)
        && is_execution_policy(task.get("executionPolicy"))
        && is_agent_engine(engine)
        && optional("model", &|value| match value {
            Some(Value::String(model)) => {
                catalog::is_model_id(model)
                    && catalog::built_in_model_engine(model, engine_name).or(engine_name) == engine_name
            }
            _ => false,
        })
        && optional("fastMode", &|value| {
            engine_name.is_some_and(catalog::has_fast_mode) && matches!(value, Some(Value::Bool(_)))
        })
        && optional("effort", &is_agent_effort)
        && optional("contextUsage", &is_context_usage)
        && matches!(task.get("messages"), Some(Value::Array(messages)) if messages.iter().all(is_conversation_message))
        && optional("subagents", &|value| matches!(value, Some(Value::Array(items)) if items.iter().all(is_subagent)))
        && record(task.get("lastChangeSnapshot")).is_some_and(|snapshot| {
            matches!(snapshot.get("files"), Some(Value::Array(files)) if files.iter().all(Value::is_string))
                && finite(snapshot.get("capturedAt"))
        })
        && optional("sortIndex", &finite)
        && optional("outcome", &is_thread_outcome)
        && optional("outcomeUnread", &js::is_true)
        && optional("findings", &|value| matches!(value, Some(Value::Array(items)) if items.iter().all(is_automation_finding)))
        && optional("lastFindingAt", &finite)
        && optional("handledIssues", &|value| {
            matches!(value, Some(Value::Array(items)) if items.iter().all(|key| non_empty_string(Some(key))))
        })
        && optional("snoozedUntil", &|value| finite(value) && value.and_then(Value::as_f64).is_some_and(|at| at > 0.0))
        && optional("limitPause", &|value| {
            is_limit_pause(value)
                && match value.and_then(|pause| pause.get("held")) {
                    Some(Value::Array(held)) => held.iter().all(is_held_message),
                    _ => true,
                }
        })
        && optional("lastChecked", &|value| {
            record(value).is_some_and(|checked| finite(checked.get("at")) && non_empty_string(checked.get("note")))
        })
        && optional("worktreeId", &non_empty_string)
        && optional("worktreeEnteredAt", &finite)
        && optional("inheritedContinuation", &js::is_true)
        && optional("runEndedAt", &finite)
        && optional("runEndedAs", &is_thread_outcome)
        && optional("createdAt", &finite)
        && optional("rankedAt", &finite)
        && finite(task.get("updatedAt"))
        && optional("archivedAt", &finite)
}

/// No provider session survives an app restart, so work saved mid-turn cannot still be working.
fn settle_stored_subagents(mut value: Value) -> Value {
    let updated_at = value.get("updatedAt").cloned().unwrap_or(Value::Null);
    if let Some(Value::Array(subagents)) = value.get_mut("subagents") {
        for subagent in subagents.iter_mut().filter_map(Value::as_object_mut) {
            if subagent.get("status").and_then(Value::as_str) != Some("working") {
                continue;
            }
            subagent.insert("status".into(), Value::from("stopped"));
            if !subagent.contains_key("finishedAt") {
                subagent.insert("finishedAt".into(), updated_at.clone());
            }
        }
    }
    value
}

/// A reference to a checkout that is not there is dropped rather than refusing the whole store.
fn claiming(mut task: Value, worktree_ids: &HashSet<String>) -> Value {
    let Value::Object(record) = &mut task else { return task };
    match record.get("worktreeId") {
        Some(id) if js::truthy(Some(id)) => {
            if id.as_str().is_some_and(|id| worktree_ids.contains(id)) {
                return task;
            }
        }
        _ => return task,
    }
    record.remove("worktreeId");
    record.remove("worktreeEnteredAt");
    task
}

/// What a thread from another build keeps as sent where storage would drop it.
const SHOWN_AS_SENT: [&str; 2] = ["threadModel", "proposals"];

/// A thread another computer's build sent, read honestly: what that build knows and this one does
/// not is kept as sent; a field whose shape this build cannot read at all is left out; and a role or
/// verdict this build has no mark for is left out.
pub fn readable_remote_thread(thread: Value) -> Value {
    let mut read = without_unreadable_fields(renamed_fields(thread), &SHOWN_AS_SENT);
    let Value::Object(task) = &mut read else { return read };
    if let Some(choice) = task.get("threadModel") {
        if !record(Some(choice)).is_some_and(|choice| choice.get("model").is_some_and(Value::is_string)) {
            task.remove("threadModel");
        }
    }
    if let Some(proposals) = task.get("proposals") {
        let shown: Vec<Value> = match proposals {
            Value::Array(items) => items
                .iter()
                .filter(|proposal| {
                    let Some(fields) = record(Some(proposal)) else { return false };
                    if !(absent(fields.get("engine")) || fields.get("engine").is_some_and(Value::is_string)) {
                        return false;
                    }
                    let mut stripped = fields.clone();
                    stripped.remove("engine");
                    stripped.remove("model");
                    stripped.remove("effort");
                    is_thread_proposal(&Value::Object(stripped))
                })
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        let complete = matches!(proposals, Value::Array(items) if items.len() == shown.len());
        if shown.is_empty() {
            task.remove("proposals");
        } else if !complete {
            task.insert("proposals".into(), Value::Array(shown));
        }
    }
    if task.contains_key("role") && !is_thread_role(task.get("role")) {
        task.remove("role");
    }
    if task.contains_key("outcome") && !is_thread_outcome(task.get("outcome")) {
        task.remove("outcome");
    }
    if !js::truthy(task.get("rankedAt")) {
        let at = last_output_at(task);
        task.insert("rankedAt".into(), js::number_value(at));
    }
    read
}

/// A property read as JS reads it off a value of any shape.
enum JsRead<'a> {
    Undefined,
    Value(&'a Value),
    /// A method every string or array inherits, which reads as NaN.
    Method,
}

impl JsRead<'_> {
    fn nullish(&self) -> bool {
        matches!(self, JsRead::Undefined | JsRead::Value(Value::Null))
    }

    fn number(&self) -> f64 {
        match self {
            JsRead::Value(value) => js::to_number(value),
            _ => f64::NAN,
        }
    }
}

/// `value.at`, the one property of a message `threadLastOutputAt` reads.
fn read_at(value: Option<&Value>) -> JsRead<'_> {
    match value {
        Some(Value::Object(record)) => record.get("at").map_or(JsRead::Undefined, JsRead::Value),
        Some(Value::String(_) | Value::Array(_)) => JsRead::Method,
        _ => JsRead::Undefined,
    }
}

/// `threadLastOutputAt` on a raw thread record, with JS's coercions, so a record of any shape
/// ranks where the TypeScript build ranks it.
fn last_output_at(task: &Map<String, Value>) -> f64 {
    let summary = task.get("historySummary").filter(|summary| js::truthy(Some(summary)));
    let messages = task.get("messages");
    let first_message = match messages {
        Some(Value::Array(items)) => items.first().map(|message| read_at(Some(message))).unwrap_or(JsRead::Undefined),
        Some(Value::String(text)) if !text.is_empty() => JsRead::Method,
        Some(Value::Object(record)) => read_at(record.get("0")),
        _ => JsRead::Undefined,
    };
    let candidates = [
        task.get("createdAt").map_or(JsRead::Undefined, JsRead::Value),
        summary.and_then(|summary| summary.get("firstMessageAt")).map_or(JsRead::Undefined, JsRead::Value),
        first_message,
        task.get("updatedAt").map_or(JsRead::Undefined, JsRead::Value),
    ];
    let created = candidates.into_iter().find(|candidate| !candidate.nullish()).map_or(f64::NAN, |candidate| candidate.number());
    let audible = match summary {
        Some(summary) => match summary.get("lastAudibleAt") {
            None | Some(Value::Null) => 0.0,
            Some(at) => js::to_number(at),
        },
        None => match messages {
            Some(Value::Array(items)) => items
                .iter()
                .rev()
                .find(|message| !js::truthy(message.get("withdrawn")))
                .map_or(0.0, |message| read_at(Some(message)).number()),
            Some(Value::String(text)) if !text.is_empty() => f64::NAN,
            _ => 0.0,
        },
    };
    let ended = match task.get("runEndedAt") {
        None | Some(Value::Null) => 0.0,
        Some(at) => js::to_number(at),
    };
    js_max(&[created, audible, ended])
}

/// `Math.max`: NaN when any argument is.
fn js_max(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::NEG_INFINITY, |max, value| if max.is_nan() || value.is_nan() { f64::NAN } else { max.max(value) })
}

