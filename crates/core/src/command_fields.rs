//! The window's check on every field of a command, as each feature declares it beside its commands
//! (src/contracts/*-commands.ts): what a field holds beyond the shape its type reads. Every command a
//! paired computer, an agent or a phone sends passes it.

use serde_json::Value;

use crate::commands::AppCommand;
use crate::domain::coordination::{MAX_MEMORY_PROMPT, MAX_PROPOSALS, MAX_PROPOSAL_TITLE};
use crate::domain::terminal::MAX_TERMINAL_INPUT;
use crate::util::js_len;
use crate::validation::checks::{MAX_ID_LENGTH, MAX_PATH_LENGTH, MAX_PROMPT_LENGTH};
use crate::validation::parse_command;

/// A command as the window takes it from outside: its type reads it and every field holds.
pub fn view_input(value: &Value) -> Result<AppCommand, String> {
    let command = parse_command(value)?;
    if !fields_hold(value, &command) {
        return Err(format!("{} carries a field this app cannot take.", command.type_name()));
    }
    Ok(command)
}

/// Whether every field of a command that its type reads holds what the window takes.
pub fn fields_hold(value: &Value, command: &AppCommand) -> bool {
    let Some(fields) = value.as_object() else { return false };
    let kind = fields.get("type").and_then(Value::as_str).unwrap_or("");
    let read = serde_json::to_value(command).unwrap_or(Value::Null);
    null_only_where_nullable(value, &read) && typed_hold(command) && CHECKS.iter().find(|(declared, _)| *declared == kind).is_none_or(|(_, rules)| rules.iter().all(|(name, rule)| rule(fields.get(*name))))
}

/// An optional field is left out rather than sent as null, and a field that may be null is sent: a
/// null this build writes back without was never one the field takes, and one it writes back as null
/// was there to be sent.
fn null_only_where_nullable(sent: &Value, read: &Value) -> bool {
    match (sent, read) {
        (Value::Object(sent), Value::Object(read)) => {
            sent.iter().all(|(key, value)| match read.get(key) {
                None => !value.is_null(),
                Some(held) => null_only_where_nullable(value, held),
            }) && read.iter().all(|(key, held)| !held.is_null() || sent.contains_key(key))
        }
        (Value::Array(sent), Value::Array(read)) => sent.iter().zip(read).all(|(value, held)| null_only_where_nullable(value, held)),
        // A record is never sent as a list, though its type would read one.
        (Value::Array(_), Value::Object(_)) => false,
        _ => true,
    }
}

/// The checks a field's own type carries, run on what was read.
fn typed_hold(command: &AppCommand) -> bool {
    use crate::domain::automation::{is_automation_draft, is_automation_patch};
    match command {
        AppCommand::AutomationSave { draft, .. } => is_automation_draft(draft),
        AppCommand::AutomationUpdate { patch, .. } => is_automation_patch(patch),
        AppCommand::SimulatorInput { input, .. } => crate::domain::platform::is_simulator_input(input),
        AppCommand::DiffSetRange { range } => crate::domain::diff::is_diff_range(range),
        AppCommand::TaskSend(send) => send.brief.as_ref().is_none_or(crate::domain::coordination::is_thread_brief),
        _ => true,
    }
}

type Check = fn(Option<&Value>) -> bool;

fn text(value: Option<&Value>, max: usize) -> bool {
    value.and_then(Value::as_str).is_some_and(|text| js_len(text) <= max)
}

fn words(value: Option<&Value>, max: usize) -> bool {
    text(value, max) && value.and_then(Value::as_str).is_some_and(|text| !text.trim().is_empty())
}

fn optional_true(value: Option<&Value>) -> bool {
    value.is_none_or(|value| value == &Value::Bool(true))
}

fn step(value: Option<&Value>) -> bool {
    value.and_then(Value::as_f64).is_some_and(|step| step == 1.0 || step == -1.0)
}

fn device_id(value: Option<&Value>) -> bool {
    text(value, MAX_ID_LENGTH) && value.and_then(Value::as_str).is_some_and(|id| !id.is_empty())
}

fn optional_device_id(value: Option<&Value>) -> bool {
    value.is_none() || device_id(value)
}

fn start_title(value: Option<&Value>) -> bool {
    value.is_none() || words(value, MAX_PROPOSAL_TITLE)
}

fn model_id(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(crate::domain::engine::is_model_id)
}

fn optional_model_id(value: Option<&Value>) -> bool {
    value.is_none() || model_id(value)
}

fn path_text(value: Option<&Value>) -> bool {
    text(value, MAX_PATH_LENGTH) && value.and_then(Value::as_str).is_some_and(|path| !path.contains('\0'))
}

fn image_source(value: Option<&Value>) -> bool {
    text(value, 16_384) && value.and_then(Value::as_str).is_some_and(|source| ["attachment://file/", "message-image://file/"].iter().any(|scheme| source.starts_with(scheme)))
}

fn integer_within(value: Option<&Value>, low: f64, high: f64) -> bool {
    value.and_then(Value::as_f64).is_some_and(|number| number.fract() == 0.0 && number >= low && number <= high)
}

fn items(value: Option<&Value>, low: usize, high: usize, item: Check) -> bool {
    value.and_then(Value::as_array).is_some_and(|items| (low..=high).contains(&items.len()) && items.iter().all(|value| item(Some(value))))
}

fn field<'a>(value: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    value.and_then(|value| value.get(name))
}

/// Who on another computer asked: a thread and a computer each named by a device id.
fn sender(value: Option<&Value>) -> bool {
    value.is_none() || (device_id(field(value, "threadId")) && device_id(field(value, "computer")))
}

fn lead(value: Option<&Value>) -> bool {
    value.is_none() || (sender(value) && optional_device_id(field(value, "proposalId")))
}

fn proposal_ids(value: Option<&Value>) -> bool {
    value.is_none() || items(value, 1, MAX_PROPOSALS, |_| true)
}

/// A screenshot's annotations and source, as an outgoing attachment carries them.
fn outgoing_attachments(value: Option<&Value>) -> bool {
    let limit = crate::domain::conversation::MAX_ATTACHMENT_ENCODED_BYTES + 256;
    value.and_then(Value::as_array).is_some_and(|attachments| attachments.iter().all(|attachment| text(attachment.get("source"), limit)))
}

fn review_target(value: Option<&Value>) -> bool {
    let bounded = |name: &str, max: usize| text(field(value, name), max);
    match field(value, "type").and_then(Value::as_str) {
        Some("baseBranch") => bounded("branch", 4_096),
        Some("commit") => bounded("sha", 256) && (field(value, "title") == Some(&Value::Null) || bounded("title", 10_000)),
        Some("custom") => bounded("instructions", 1_000_000),
        _ => true,
    }
}

/// Each command's fields whose check is stricter than their type.
static CHECKS: &[(&str, &[(&str, Check)])] = &[
    ("app.check-for-updates", &[("quiet", optional_true)]),
    ("image.open", &[("source", image_source), ("gallery", |value| value.is_none() || items(value, 0, 500, image_source))]),
    ("image.step", &[("delta", step)]),
    ("file.attach", &[("files", |value| value.and_then(Value::as_array).is_some_and(|files| files.iter().all(|file| optional_true(file.get("folder")))))]),
    ("file.recall", &[("files", |value| value.and_then(Value::as_array).is_some_and(|files| files.iter().all(|file| optional_true(file.get("folder")))))]),
    ("browser.go", &[("delta", step)]),
    ("browser-import.choose", &[("sourceId", |value| value == Some(&Value::Null) || (text(value, 1_024) && value.and_then(Value::as_str).is_some_and(|id| !id.is_empty())))]),
    ("browser-import.toggle", &[("site", |value| value.and_then(Value::as_str).is_some_and(crate::domain::terminal::is_import_site))]),
    ("browser-import.filter", &[("text", |value| text(value, 256))]),
    ("computers.pair", &[("host", |value| value.and_then(Value::as_str).is_some_and(|host| crate::domain::platform::computer_address(host).as_deref() == Some(host)))]),
    ("diff.open-commit", &[("commit", |value| value.and_then(Value::as_str).is_some_and(crate::domain::diff::is_commit_hash))]),
    ("project.open", &[("start", optional_true)]),
    ("project.add", &[("root", |value| path_text(value) && words(value, MAX_PATH_LENGTH)), ("computerId", optional_device_id), ("start", optional_true)]),
    ("view.add-project-device", &[("computerId", device_id)]),
    ("view.add-project-path", &[("root", path_text)]),
    ("view.add-project-accept", &[("index", |value| integer_within(value, 0.0, 19.0))]),
    ("review.start", &[("target", review_target)]),
    ("terminal.input", &[("data", |value| text(value, MAX_TERMINAL_INPUT))]),
    ("terminal.resize", &[("cols", |value| integer_within(value, 1.0, 1_000.0)), ("rows", |value| integer_within(value, 1.0, 1_000.0))]),
    ("task.set-role", &[("sender", sender)]),
    ("coordination.set-thread-model", &[("model", model_id)]),
    ("proposal.start", &[("proposalIds", proposal_ids)]),
    ("proposal.dismiss", &[("proposalIds", proposal_ids)]),
    ("task.snooze", &[("hours", |value| value.and_then(Value::as_f64).is_some_and(|hours| [1.0, 4.0, 24.0, 72.0, 168.0].contains(&hours)))]),
    ("task.fork", &[("title", start_title)]),
    ("task.set-model", &[("model", optional_model_id)]),
    ("task.move-worktree", &[("afterRun", optional_true), ("sender", sender), ("then", |value| value.is_none() || words(value, MAX_PROMPT_LENGTH))]),
    (
        "task.send",
        &[
            ("model", optional_model_id),
            ("title", start_title),
            ("sender", sender),
            ("lead", lead),
            ("memory", |value| value.is_none() || text(value, MAX_MEMORY_PROMPT)),
        ],
    ),
    ("attachments.send", &[("attachments", outgoing_attachments)]),
    ("task.drop-queued", &[("ifWaiting", optional_true)]),
    ("view.fork-confirm", &[("title", |value| words(value, MAX_PROPOSAL_TITLE))]),
    ("view.find-step", &[("delta", step)]),
    ("view.jump-step", &[("delta", step)]),
    ("view.set-model-favorite", &[("model", model_id)]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_field_that_may_be_null_is_sent_and_one_left_optional_is_never_null() {
        assert!(view_input(&json!({ "type": "task.set-coordinator", "taskId": "a", "coordinatorId": null })).is_ok());
        assert!(view_input(&json!({ "type": "task.set-coordinator", "taskId": "a" })).is_err());
        assert!(view_input(&json!({ "type": "task.send", "lead": { "threadId": "t", "title": "T", "computer": "c" } })).is_ok());
        assert!(view_input(&json!({ "type": "task.send", "lead": { "threadId": "t", "title": "T", "computer": "c", "proposalId": null } })).is_err());
        assert!(view_input(&json!({ "type": "task.move-worktree", "destination": [1] })).is_err(), "a record is never a list");
    }

    #[test]
    fn fields_are_held_to_what_their_feature_declares() {
        assert!(view_input(&json!({ "type": "task.snooze", "taskId": "a", "hours": 72 })).is_ok());
        assert!(view_input(&json!({ "type": "task.snooze", "taskId": "a", "hours": 2 })).is_err());
        assert!(view_input(&json!({ "type": "computers.pair", "host": "127.0.0.1:8787", "name": "Mac", "code": "" })).is_ok());
        assert!(view_input(&json!({ "type": "computers.pair", "host": "https://mac.tail.ts.net", "name": "Mac", "code": "" })).is_err());
        assert!(view_input(&json!({ "type": "terminal.resize", "terminalId": "t", "cols": 1001, "rows": 24 })).is_err());
        assert!(view_input(&json!({ "type": "image.open", "source": "file:///etc/passwd" })).is_err());
        assert!(view_input(&json!({ "type": "task.send", "brief": { "intent": " ", "doneWhen": "done", "delivers": "commit" } })).is_err());
    }
}
