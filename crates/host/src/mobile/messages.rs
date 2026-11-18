//! What a phone or another computer may send the bridge. The socket is the security boundary, so
//! every field is read defensively before anything reaches the window.

use serde_json::{Map, Value};

use super::{MAX_DEVICE_NAME_LENGTH, MAX_ID_LENGTH, MAX_PATH_LENGTH, MAX_TOKEN_LENGTH};

/// The oldest phone app this computer serves, and the protocol it speaks.
pub const MOBILE_PROTOCOL_VERSION: u64 = 8;
pub const OLDEST_PHONE_PROTOCOL_VERSION: u64 = 8;
pub const COMPUTER_PROTOCOL_VERSION: u64 = 2;
/// How many inputs one computer request may carry.
const MAX_INPUTS: usize = 16;
const MAX_DICTATION_AUDIO_BYTES: usize = 16_000 * 2 * MAX_DICTATION_SECONDS + 4_096;
const MAX_DICTATION_SECONDS: usize = 120;

pub(crate) fn is_string(value: Option<&Value>, max: usize) -> bool {
    value.and_then(Value::as_str).is_some_and(|text| !text.is_empty() && text.encode_utf16().count() <= max)
}

pub(crate) fn is_id(value: Option<&Value>) -> bool {
    is_string(value, MAX_ID_LENGTH)
}

pub(crate) fn is_count(value: Option<&Value>) -> bool {
    value.and_then(Value::as_f64).is_some_and(|number| number.fract() == 0.0 && number >= 0.0 && number.is_finite())
}

fn is_bool(value: Option<&Value>) -> bool {
    value.is_some_and(Value::is_boolean)
}

fn absent(value: Option<&Value>) -> bool {
    value.is_none()
}

pub fn is_diff_range(value: Option<&Value>) -> bool {
    let Some(range) = value.and_then(Value::as_object) else { return false };
    match range.get("kind").and_then(Value::as_str) {
        Some("uncommitted") => true,
        Some("commit") => range.get("commit").and_then(Value::as_str).is_some_and(|commit| (7..=40).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit())),
        Some("branches") => range.get("base").and_then(Value::as_str).is_some_and(|base| !base.is_empty()) && range.get("compare").is_some_and(|compare| compare.is_null() || compare.is_string()),
        _ => false,
    }
}

fn is_base64(text: &str) -> bool {
    let trimmed = text.trim_end_matches('=');
    text.len() - trimmed.len() <= 2 && !trimmed.is_empty() && trimmed.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
}

fn is_dictation_audio(value: Option<&Value>) -> bool {
    let limit = MAX_DICTATION_AUDIO_BYTES.div_ceil(3) * 4;
    value.and_then(Value::as_str).is_some_and(|audio| !audio.is_empty() && audio.len() <= limit && audio.len() % 4 == 0 && is_base64(audio))
}

pub fn is_mobile_query(value: &Value) -> bool {
    let Some(query) = value.as_object() else { return false };
    let field = |name: &str| query.get(name);
    match field("kind").and_then(Value::as_str) {
        Some("history") => is_id(field("taskId")) && field("before").and_then(Value::as_f64).is_some_and(|before| before.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(&before)),
        Some("directories") => field("prefix").and_then(Value::as_str).is_some_and(|prefix| prefix.len() <= MAX_PATH_LENGTH && !prefix.contains('\0')),
        Some("thread") => is_id(field("taskId")),
        Some("archived") => true,
        Some("commands") => absent(field("taskId")) || is_id(field("taskId")),
        Some("transcribe") => is_dictation_audio(field("audio")),
        Some("image") => is_id(field("taskId")) && is_id(field("messageId")) && is_string(field("href"), MAX_PATH_LENGTH) && (absent(field("preview")) || is_bool(field("preview"))),
        Some("diff-summary") => is_id(field("taskId")) && is_diff_range(field("range")),
        Some("diff-patch") => is_id(field("taskId")) && is_diff_range(field("range")) && is_string(field("path"), MAX_PATH_LENGTH) && (absent(field("previousPath")) || is_string(field("previousPath"), MAX_PATH_LENGTH)),
        _ => false,
    }
}

pub fn is_phone_push(value: &Value) -> bool {
    let Some(push) = value.as_object() else { return false };
    push.get("token").and_then(Value::as_str).is_some_and(|token| (64..=200).contains(&token.len()) && token.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        && matches!(push.get("environment").and_then(Value::as_str), Some("production" | "sandbox"))
}

/// Whether a phone's message is one it may send, read field by field.
pub fn is_mobile_client_message(value: &Value) -> bool {
    let Some(message) = value.as_object() else { return false };
    let field = |name: &str| message.get(name);
    match field("kind").and_then(Value::as_str) {
        Some("pair") => is_count(field("version")) && (absent(field("oldestComputer")) || is_count(field("oldestComputer"))) && is_string(field("code"), MAX_TOKEN_LENGTH) && is_string(field("deviceName"), MAX_DEVICE_NAME_LENGTH),
        Some("resume") => is_count(field("version")) && (absent(field("oldestComputer")) || is_count(field("oldestComputer"))) && is_string(field("token"), MAX_TOKEN_LENGTH) && (absent(field("sessionId")) || is_id(field("sessionId"))) && is_count(field("lastSequence")),
        Some("command") => is_id(field("requestId")) && field("command").is_some_and(|command| act_core::validation::phone_command(command).is_ok()),
        Some("query") => is_id(field("requestId")) && field("query").is_some_and(is_mobile_query),
        Some("pong" | "ping") => is_count(field("at")),
        Some("push-token") => field("push").is_some_and(|push| push.is_null() || is_phone_push(push)),
        _ => false,
    }
}

const COMMIT_CHECKS: usize = 64;

fn query_fields(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "thread-read" => &["threadId", "limit"],
        "thread-list" => &["project", "archived", "idleForMs", "search", "attachments", "limit"],
        "terminal-output" => &["terminalId", "after"],
        "directories" => &["prefix"],
        "attachment" => &["name"],
        "message-image" => &["path", "root", "message", "thumbnail"],
        "diff-patch" => &["workspaceId", "range", "path", "previousPath", "ignoreWhitespace"],
        "branches" => &["workspaceId"],
        "commits" => &["workspaceId", "hashes"],
        "commands" => &["workspaceId", "engine"],
        _ => return None,
    })
}

const QUERY_KINDS: [&str; 10] = ["thread-read", "thread-list", "terminal-output", "directories", "attachment", "message-image", "diff-patch", "branches", "commits", "commands"];

/// The capability names this build advertises: every command and query, and each of their fields.
pub fn computer_capabilities() -> Vec<String> {
    let mut names = act_core::validation::computer_capabilities();
    for kind in QUERY_KINDS {
        names.push(format!("query:{kind}"));
        for field in query_fields(kind).unwrap_or(&[]) {
            names.push(format!("query:{kind}:{field}"));
        }
    }
    names
}

fn optional(value: Option<&Value>, check: impl Fn(Option<&Value>) -> bool) -> bool {
    value.is_none() || check(value)
}

fn small_count(value: Option<&Value>, max: f64) -> bool {
    is_count(value) && value.and_then(Value::as_f64).is_some_and(|number| number <= max)
}

pub fn is_computer_query(value: &Value) -> bool {
    let Some(query) = value.as_object() else { return false };
    let field = |name: &str| query.get(name);
    let Some(kind) = field("kind").and_then(Value::as_str) else { return false };
    match kind {
        "thread-read" => is_id(field("threadId")) && optional(field("limit"), |limit| small_count(limit, 200.0)),
        "thread-list" => {
            optional(field("project"), |value| is_string(value, MAX_PATH_LENGTH))
                && optional(field("archived"), is_bool)
                && optional(field("idleForMs"), is_count)
                && optional(field("search"), |value| is_string(value, 1_000))
                && optional(field("attachments"), is_bool)
                && optional(field("limit"), |limit| small_count(limit, 200.0))
        }
        "terminal-output" => is_id(field("terminalId")) && optional(field("after"), is_count),
        "directories" => field("prefix").and_then(Value::as_str).is_some_and(|prefix| prefix.len() <= MAX_PATH_LENGTH && !prefix.contains('\0')),
        "attachment" => field("name").and_then(Value::as_str).is_some_and(|name| name.len() <= MAX_ID_LENGTH && crate::attachments::is_saved_attachment_name(name)),
        "message-image" => {
            let text = |name: &str| field(name).and_then(Value::as_str);
            optional(field("thumbnail"), is_bool) && matches!((text("path"), text("root"), text("message")), (Some(path), Some(root), Some(message)) if crate::message_images::is_message_image_reference(path, root, message))
        }
        "diff-patch" => is_id(field("workspaceId")) && is_diff_range(field("range")) && is_string(field("path"), MAX_PATH_LENGTH) && optional(field("previousPath"), |value| is_string(value, MAX_PATH_LENGTH)) && optional(field("ignoreWhitespace"), is_bool),
        "branches" => is_id(field("workspaceId")),
        "commits" => is_id(field("workspaceId")) && field("hashes").and_then(Value::as_array).is_some_and(|hashes| !hashes.is_empty() && hashes.len() <= COMMIT_CHECKS && hashes.iter().all(|hash| hash.as_str().is_some_and(|hash| (7..=40).contains(&hash.len()) && hash.bytes().all(|byte| byte.is_ascii_hexdigit())))),
        "commands" => is_id(field("workspaceId")) && field("engine").and_then(Value::as_str).and_then(act_core::domain::engine::AgentEngine::parse).is_some(),
        _ => false,
    }
}

/// Whether another computer's message is one it may send. Its inputs are only commands a window could send.
pub fn is_computer_client_message(value: &Value) -> bool {
    let Some(message) = value.as_object() else { return false };
    let field = |name: &str| message.get(name);
    match field("kind").and_then(Value::as_str) {
        Some("pair") => is_count(field("version")) && is_string(field("code"), MAX_TOKEN_LENGTH) && is_string(field("deviceName"), MAX_DEVICE_NAME_LENGTH),
        Some("resume") => is_count(field("version")) && is_string(field("token"), MAX_TOKEN_LENGTH) && optional(field("sessionId"), is_id) && is_count(field("lastSequence")),
        Some("transfer") => is_id(field("requestId")),
        Some("input") => is_id(field("requestId")) && field("inputs").and_then(Value::as_array).is_some_and(|inputs| !inputs.is_empty() && inputs.len() <= MAX_INPUTS && inputs.iter().all(|input| act_core::validation::parse_command(input).is_ok() && act_core::validation::outside_fields_hold(input))),
        Some("query") => is_id(field("requestId")) && field("query").is_some_and(is_computer_query),
        Some("pong") => is_count(field("at")),
        _ => false,
    }
}

/// Only the envelope is checked: the state inside is the other computer's own derivation.
pub fn is_computer_server_message(value: &Value) -> bool {
    let Some(message) = value.as_object() else { return false };
    let field = |name: &str| message.get(name);
    if !is_count(field("sequence")) {
        return false;
    }
    match field("kind").and_then(Value::as_str) {
        Some("paired") => is_id(field("deviceId")) && is_string(field("deviceName"), MAX_DEVICE_NAME_LENGTH) && is_string(field("token"), MAX_TOKEN_LENGTH),
        Some("workspace") => {
            optional(field("sessionId"), is_id)
                && field("update").and_then(Value::as_object).is_some_and(|update| is_count(update.get("revision")) && (update.get("state").is_some_and(Value::is_object) || update.get("patches").is_some_and(Value::is_array)))
        }
        Some("result") => is_id(field("requestId")) && field("result").and_then(Value::as_object).is_some_and(|result| is_bool(result.get("ok")) && is_count(result.get("revision"))),
        Some("answer") => is_id(field("requestId")) && ((field("ok") == Some(&Value::Bool(true)) && field("result").is_some()) || (field("ok") == Some(&Value::Bool(false)) && field("message").is_some_and(Value::is_string))),
        Some("error") => field("code").is_some_and(Value::is_string) && field("message").is_some_and(Value::is_string),
        Some("notice") => field("notice").and_then(Value::as_object).is_some_and(|notice| is_id(notice.get("taskId")) && notice.get("title").is_some_and(Value::is_string) && notice.get("headline").is_some_and(Value::is_string)),
        Some("capabilities") => {
            field("capabilities").and_then(Value::as_array).is_some_and(|names| names.len() <= 4096 && names.iter().all(|name| name.as_str().is_some_and(|name| !name.is_empty() && name.len() <= 256)))
                && optional(field("instance"), |instance| is_string(instance, 256))
        }
        Some("name") => is_string(field("name"), MAX_DEVICE_NAME_LENGTH),
        Some("ping") => is_count(field("at")),
        _ => false,
    }
}

/// A request a guard turned away, by the id it can still be answered under.
pub fn rejected_request(value: &Value, kinds: [&str; 2]) -> Option<(String, String)> {
    let message = value.as_object()?;
    let kind = message.get("kind").and_then(Value::as_str).filter(|kind| kinds.contains(kind))?;
    let request_id = message.get("requestId").and_then(Value::as_str).filter(|id| !id.is_empty() && id.len() <= 256)?;
    Some((kind.to_string(), request_id.to_string()))
}

/// Whether a computer's request carries a transfer, which gets the longer deadline.
pub fn is_computer_transfer(message: &Map<String, Value>) -> bool {
    match message.get("kind").and_then(Value::as_str) {
        Some("input") => message.get("inputs").and_then(Value::as_array).is_some_and(|inputs| {
            inputs.iter().any(|input| input.get("type").and_then(Value::as_str) == Some("attachments.send") && input.get("attachments").and_then(Value::as_array).is_some_and(|attachments| !attachments.is_empty()))
        }),
        Some("query") => matches!(message.get("query").and_then(|query| query.get("kind")).and_then(Value::as_str), Some("attachment" | "message-image")),
        _ => false,
    }
}

fn requirements(prefix: &str, value: &Value, discriminator: &str) -> Vec<String> {
    let mut names = vec![prefix.to_string()];
    if let Some(fields) = value.as_object() {
        names.extend(fields.keys().filter(|key| key.as_str() != discriminator).map(|key| format!("{prefix}:{key}")));
    }
    names
}

/// Whether a computer whose build named `capabilities` takes this command. None is a build from before
/// capabilities were named, which is taken at its word.
pub fn supports_command(capabilities: Option<&[String]>, command: &Value) -> bool {
    let Some(capabilities) = capabilities else { return true };
    let kind = command.get("type").and_then(Value::as_str).unwrap_or("");
    requirements(&format!("command:{kind}"), command, "type").iter().all(|name| capabilities.contains(name))
}

pub fn supports_query(capabilities: Option<&[String]>, query: &Value) -> bool {
    let Some(capabilities) = capabilities else { return true };
    let kind = query.get("kind").and_then(Value::as_str).unwrap_or("");
    requirements(&format!("query:{kind}"), query, "kind").iter().all(|name| capabilities.contains(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_phone_is_held_to_what_it_may_send() {
        assert!(is_mobile_client_message(&json!({ "kind": "pair", "version": 8, "code": "ABCD1234", "deviceName": "iPhone" })));
        assert!(is_mobile_client_message(&json!({ "kind": "resume", "version": 8, "token": "t", "lastSequence": 3 })));
        assert!(!is_mobile_client_message(&json!({ "kind": "resume", "version": 8, "token": "t", "lastSequence": -1 })));
        assert!(is_mobile_client_message(&json!({ "kind": "query", "requestId": "r", "query": { "kind": "diff-summary", "taskId": "t", "range": { "kind": "commit", "commit": "abcdef1" } } })));
        assert!(!is_mobile_client_message(&json!({ "kind": "query", "requestId": "r", "query": { "kind": "diff-summary", "taskId": "t", "range": { "kind": "commit", "commit": "nothex!" } } })));
        assert!(is_mobile_client_message(&json!({ "kind": "push-token", "push": null })));
        assert!(!is_mobile_client_message(&json!({ "kind": "push-token", "push": { "token": "xyz", "environment": "production" } })));
    }

    #[test]
    fn a_computer_needs_every_field_it_sets_named_as_a_capability() {
        let capabilities = vec!["command:task.send".to_string(), "command:task.send:text".to_string()];
        assert!(supports_command(Some(&capabilities), &json!({ "type": "task.send", "text": "hi" })));
        assert!(!supports_command(Some(&capabilities), &json!({ "type": "task.send", "text": "hi", "sender": {} })));
        assert!(supports_command(None, &json!({ "type": "anything" })));
    }
}
