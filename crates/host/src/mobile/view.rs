//! The phone's view as JSON, and the patches that move it: what changed between two views, and that
//! change put back on the view a phone holds.

use serde_json::{json, Map, Value};

/// How far a row's last activity may drift before the list is worth sending again.
const ACTIVITY_DRIFT_MS: f64 = 60_000.0;

/// Every field of the open thread a delta carries as it stands: all but its id, transcript and tail.
const MOVING_KEYS: [&str; 25] = [
    "loading", "title", "projectId", "projectName", "worktreeId", "omitted", "status", "question", "approval", "queued", "settings", "location", "worktrees", "canMove", "changes", "reviewable", "branchRange", "members", "role", "background", "decisions", "suggestions", "limit", "computer", "stoppable",
];

const PLAIN_KEYS: [&str; 6] = ["theme", "engines", "upkeep", "dictation", "usage", "archived"];

fn get<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn same_rows(previous: &Value, next: &Value) -> bool {
    let (Some(previous), Some(next)) = (previous.as_array(), next.as_array()) else { return previous == next };
    previous.len() == next.len()
        && previous.iter().zip(next).all(|(before, after)| {
            let drift = (get(before, "lastActivityAt").as_f64().unwrap_or(0.0) - get(after, "lastActivityAt").as_f64().unwrap_or(0.0)).abs();
            if drift >= ACTIVITY_DRIFT_MS {
                return false;
            }
            let strip = |row: &Value| {
                let mut row = row.clone();
                if let Some(fields) = row.as_object_mut() {
                    fields.insert("lastActivityAt".into(), json!(0));
                }
                row
            };
            strip(before) == strip(after)
        })
}

fn same_groups(previous: &Value, next: &Value) -> bool {
    let (Some(previous), Some(next)) = (previous.as_array(), next.as_array()) else { return previous == next };
    previous.len() == next.len() && previous.iter().zip(next).all(|(group, other)| get(group, "projectId") == get(other, "projectId") && get(group, "name") == get(other, "name") && same_rows(get(group, "threads"), get(other, "threads")))
}

fn same_activity(previous: &Value, next: &Value) -> bool {
    ["priority", "running", "threads", "snoozed"].iter().all(|key| same_rows(get(previous, key), get(next, key)))
}

/// How the transcript moved: not at all, at its end, or some other way.
fn transcript_delta(previous: &[Value], next: &[Value], delta: &mut Map<String, Value>) {
    if previous.is_empty() {
        if !next.is_empty() {
            delta.insert("spliced".into(), json!({ "dropped": 0, "kept": 0, "messages": next }));
        }
        return;
    }
    let Some(dropped) = next.first().and_then(|first| previous.iter().position(|held| held == first)) else {
        delta.insert("messages".into(), Value::Array(next.to_vec()));
        return;
    };
    let mut kept = 0;
    while dropped + kept < previous.len() && kept < next.len() && previous[dropped + kept] == next[kept] {
        kept += 1;
    }
    let rest = previous.len() - dropped;
    if kept + 1 < rest {
        delta.insert("messages".into(), Value::Array(next.to_vec()));
        return;
    }
    if dropped == 0 && kept == rest && kept == next.len() {
        return;
    }
    delta.insert("spliced".into(), json!({ "dropped": dropped, "kept": kept, "messages": next[kept..] }));
}

fn diff_thread(previous: &Value, next: &Value) -> Option<Value> {
    if next.is_null() {
        return (!previous.is_null()).then(|| json!({ "kind": "closed" }));
    }
    if previous.is_null() || get(previous, "id") != get(next, "id") {
        return Some(json!({ "kind": "opened", "thread": next }));
    }
    let mut delta = Map::new();
    for key in MOVING_KEYS {
        if get(previous, key) != get(next, key) {
            delta.insert(key.into(), next.get(key).cloned().unwrap_or(Value::Null));
        }
    }
    let tail = get(previous, "streamingTail");
    let next_tail = get(next, "streamingTail");
    if tail != next_tail {
        match (tail.as_str(), next_tail.as_str()) {
            (Some(held), Some(grown)) if grown.starts_with(held) => {
                delta.insert("tail".into(), json!({ "from": held.encode_utf16().count(), "text": &grown[held.len()..] }));
            }
            _ => {
                delta.insert("streamingTail".into(), next_tail.clone());
            }
        }
    }
    let empty = Vec::new();
    transcript_delta(get(previous, "messages").as_array().unwrap_or(&empty), get(next, "messages").as_array().unwrap_or(&empty), &mut delta);
    (!delta.is_empty()).then(|| json!({ "kind": "changed", "id": get(next, "id"), "delta": delta }))
}

/// What changed between two views, or None when nothing did.
pub fn diff_mobile_view(previous: &Value, next: &Value) -> Option<Value> {
    let mut patch = Map::new();
    if !same_groups(get(previous, "groups"), get(next, "groups")) {
        patch.insert("groups".into(), get(next, "groups").clone());
    }
    if !same_activity(get(previous, "activity"), get(next, "activity")) {
        patch.insert("activity".into(), get(next, "activity").clone());
    }
    for key in PLAIN_KEYS {
        if get(previous, key) != get(next, key) {
            patch.insert(key.into(), get(next, key).clone());
        }
    }
    if let Some(thread) = diff_thread(get(previous, "thread"), get(next, "thread")) {
        patch.insert("thread".into(), thread);
    }
    for key in ["draft", "error"] {
        if get(previous, key) != get(next, key) {
            patch.insert(key.into(), get(next, key).clone());
        }
    }
    (!patch.is_empty()).then_some(Value::Object(patch))
}

/// Puts a patch back on the view a phone holds. None when it does not fit, so whoever holds the view
/// fetches it whole rather than drawing it wrong.
pub fn apply_mobile_patch(view: &Value, patch: &Value) -> Option<Value> {
    let mut next = view.as_object()?.clone();
    let changes = patch.as_object()?;
    for key in ["groups", "activity", "theme", "engines", "upkeep", "dictation", "usage", "archived"] {
        if let Some(value) = changes.get(key).filter(|value| !value.is_null()) {
            next.insert(key.into(), value.clone());
        }
    }
    for key in ["draft", "error"] {
        if let Some(value) = changes.get(key) {
            next.insert(key.into(), value.clone());
        }
    }
    let Some(thread_patch) = changes.get("thread").filter(|value| !value.is_null()) else { return Some(Value::Object(next)) };
    match get(thread_patch, "kind").as_str() {
        Some("closed") => {
            next.insert("thread".into(), Value::Null);
        }
        Some("opened") => {
            next.insert("thread".into(), get(thread_patch, "thread").clone());
        }
        _ => {
            let current = get(view, "thread");
            if current.is_null() || get(current, "id") != get(thread_patch, "id") {
                return Some(Value::Object(next));
            }
            let mut thread = current.as_object()?.clone();
            let delta = get(thread_patch, "delta").as_object()?;
            let held: Vec<Value> = get(current, "messages").as_array().cloned().unwrap_or_default();
            let mut transcript = delta.get("messages").and_then(Value::as_array).cloned().unwrap_or_else(|| held.clone());
            if let Some(spliced) = delta.get("spliced") {
                let dropped = get(spliced, "dropped").as_u64()? as usize;
                let kept = get(spliced, "kept").as_u64()? as usize;
                if held.len() < dropped + kept {
                    return None;
                }
                transcript = held[dropped..dropped + kept].to_vec();
                transcript.extend(get(spliced, "messages").as_array().cloned().unwrap_or_default());
            }
            let mut streaming = if delta.contains_key("streamingTail") { delta["streamingTail"].clone() } else { get(current, "streamingTail").clone() };
            if let Some(tail) = delta.get("tail") {
                let held_tail = get(current, "streamingTail").as_str()?;
                if held_tail.encode_utf16().count() as u64 != get(tail, "from").as_u64()? {
                    return None;
                }
                streaming = json!(format!("{held_tail}{}", get(tail, "text").as_str().unwrap_or("")));
            }
            for (key, value) in delta {
                if !matches!(key.as_str(), "spliced" | "messages" | "tail" | "streamingTail") {
                    thread.insert(key.clone(), value.clone());
                }
            }
            thread.insert("messages".into(), Value::Array(transcript));
            thread.insert("streamingTail".into(), streaming);
            next.insert("thread".into(), Value::Object(thread));
        }
    }
    Some(Value::Object(next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(messages: Value, tail: Value) -> Value {
        json!({ "groups": [], "activity": { "priority": [], "running": [], "threads": [], "snoozed": [] }, "theme": {}, "engines": {}, "upkeep": [], "dictation": false, "usage": {}, "archived": 0, "thread": { "id": "t", "title": "A", "messages": messages, "streamingTail": tail }, "draft": null, "error": null })
    }

    #[test]
    fn a_patch_moves_a_held_view_to_the_next_one() {
        let before = view(json!([{ "id": "1" }, { "id": "2" }, { "id": "3" }]), json!("Hel"));
        let after = view(json!([{ "id": "2" }, { "id": "3" }, { "id": "4" }]), json!("Hello"));
        let patch = diff_mobile_view(&before, &after).unwrap();
        assert_eq!(patch["thread"]["delta"]["spliced"], json!({ "dropped": 1, "kept": 2, "messages": [{ "id": "4" }] }));
        assert_eq!(patch["thread"]["delta"]["tail"], json!({ "from": 3, "text": "lo" }));
        assert_eq!(apply_mobile_patch(&before, &patch).unwrap(), after);
        assert!(diff_mobile_view(&after, &after).is_none());
        let other = view(json!([{ "id": "9" }]), json!(null));
        assert!(apply_mobile_patch(&other, &patch).is_none());
    }
}
