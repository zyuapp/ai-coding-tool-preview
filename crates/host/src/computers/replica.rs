//! Another computer's workspace as this one holds it: the wire state its patches build, read into this
//! build's own state with defaults for what an older build leaves out.

use act_core::domain::engine::current_model;
use act_core::domain::thread::{thread_last_output_at, Thread};
use act_core::persisted::storage::{labelled_origin, readable_remote_thread};
use act_core::state::ThreadDock;
use act_core::WorkspaceState;
use serde_json::{json, Map, Value};

use crate::workspace_patches::{apply_workspace_patches, decode_sets, WorkspacePatch};

/// What a replica made of one update.
#[derive(Debug)]
pub enum ReplicaUpdate {
    State(Box<WorkspaceState>),
    /// Nothing new: an update already applied.
    Unchanged,
    /// The patches do not fit what is held; the state is to be fetched whole.
    Resync,
}

fn thread_defaults() -> Value {
    json!({ "title": "Untitled thread", "executionPolicy": "confirm", "messages": [], "continuationStatus": "none", "lastChangeSnapshot": { "files": [], "capturedAt": 0 }, "updatedAt": 0, "rankedAt": 0 })
}

/// Fills what `value` leaves out from `template`, recursing into records the template describes.
fn fill(value: &mut Value, template: &Value) {
    let (Value::Object(fields), Value::Object(defaults)) = (&mut *value, template) else { return };
    for (key, fallback) in defaults {
        match fields.get_mut(key) {
            None => {
                fields.insert(key.clone(), fallback.clone());
            }
            Some(held) if held.is_object() && fallback.is_object() => fill(held, fallback),
            Some(_) => {}
        }
    }
}

fn fill_items(list: Option<&mut Value>, template: &Value, required: &[&str]) -> Result<(), String> {
    let Some(Value::Array(items)) = list else { return Ok(()) };
    for item in items {
        if !item.is_object() {
            return Err("Invalid remote workspace item.".into());
        }
        if required.iter().any(|key| item.get(*key).and_then(Value::as_str).is_none_or(str::is_empty)) {
            return Err("Missing remote workspace identity.".into());
        }
        fill(item, template);
    }
    Ok(())
}

/// Reads another build's workspace JSON as this build's state. An engine, effort or mode only a
/// newer build knows is kept as sent.
pub fn read_remote_workspace(wire: &Value) -> Result<WorkspaceState, String> {
    act_core::util::read_as_sent(|| read_workspace(wire))
}

fn read_workspace(wire: &Value) -> Result<WorkspaceState, String> {
    if !wire.is_object() {
        return Err("Invalid remote workspace.".into());
    }
    let mut state = wire.clone();
    decode_sets(&mut state);
    let defaults = WorkspaceState::default().to_json().map_err(|error| error.to_string())?;
    fill(&mut state, &defaults);
    fill_items(state.get_mut("threads"), &thread_defaults(), &["id", "engine"])?;
    fill_items(state.get_mut("projects"), &json!({}), &["id", "root"])?;
    fill_items(state.get_mut("sideChats"), &json!({ "error": null }), &["id", "sourceThreadId"])?;
    let dock = serde_json::to_value(ThreadDock::default()).map_err(|error| error.to_string())?;
    if let Some(Value::Object(docks)) = state.get_mut("docks") {
        for held in docks.values_mut() {
            fill(held, &dock);
        }
    }
    if let Some(Value::Array(threads)) = state.get_mut("threads") {
        threads.iter_mut().for_each(with_legacy_origins);
    }
    let mut read = match WorkspaceState::from_json(&state) {
        Ok(read) => read,
        Err(_) => salvaged(state)?,
    };
    for thread in &mut read.threads {
        known_models(thread);
        if thread.ranked_at == 0 {
            thread.ranked_at = thread_last_output_at(thread);
        }
    }
    Ok(read)
}

/// User messages an older build only labelled, given the origin the label names.
fn with_legacy_origins(thread: &mut Value) {
    let Some(Value::Array(messages)) = thread.get_mut("messages") else { return };
    for message in messages.iter_mut() {
        let labelled = message.get("origin").is_none() && message.get("detail").is_some_and(Value::is_string) && message.get("kind").is_none_or(|kind| kind == "user");
        if labelled {
            *message = labelled_origin(std::mem::take(message));
        }
    }
}

/// A retired model a thread still names reads as its successor.
fn known_models(thread: &mut Thread) {
    let renamed = |model: &mut String| {
        let current = current_model(model);
        if current != model {
            *model = current.to_string();
        }
    };
    if let Some(model) = &mut thread.model {
        renamed(model);
    }
    if let Some(choice) = &mut thread.thread_model {
        renamed(&mut choice.model);
    }
}

/// A state with something this build cannot read, read as far as it can: each thread read as
/// another build's, a field of a thread it cannot read left out, and a thread or other part it
/// still cannot read left out, rather than losing the whole computer.
fn salvaged(mut state: Value) -> Result<WorkspaceState, String> {
    if let Some(Value::Array(threads)) = state.get_mut("threads") {
        let read: Vec<Value> = std::mem::take(threads).into_iter().map(readable_remote_thread).filter_map(readable_thread).collect();
        *threads = read;
    }
    let defaults = WorkspaceState::default().to_json().map_err(|error| error.to_string())?;
    if WorkspaceState::from_json(&state).is_err() {
        if let (Value::Object(fields), Value::Object(empty)) = (&mut state, &defaults) {
            let keys: Vec<String> = fields.keys().cloned().collect();
            for key in keys {
                let mut probe = defaults.clone();
                probe[&key] = fields[&key].clone();
                if WorkspaceState::from_json(&probe).is_err() {
                    match empty.get(&key) {
                        Some(fallback) => fields.insert(key, fallback.clone()),
                        None => fields.remove(&key),
                    };
                }
            }
        }
    }
    WorkspaceState::from_json(&state).map_err(|error| format!("Invalid remote workspace: {error}"))
}

/// A thread with each field this build cannot read left out, or None when even its core cannot be read.
fn readable_thread(mut thread: Value) -> Option<Value> {
    if Thread::from_json(&thread).is_ok() {
        return Some(thread);
    }
    const CORE: [&str; 9] = ["id", "title", "engine", "executionPolicy", "messages", "continuationStatus", "lastChangeSnapshot", "updatedAt", "rankedAt"];
    let Value::Object(fields) = &mut thread else { return None };
    let mut core: Map<String, Value> = fields.iter().filter(|(key, _)| CORE.contains(&key.as_str())).map(|(key, value)| (key.clone(), value.clone())).collect();
    for (key, fallback) in [("executionPolicy", "confirm"), ("continuationStatus", "none")] {
        let mut probe = core.clone();
        probe.insert(key.into(), Value::from(fallback));
        let readable = |value: &Value| {
            let mut tried = probe.clone();
            tried.insert(key.into(), value.clone());
            Thread::from_json(&Value::Object(tried)).is_ok()
        };
        if core.get(key).is_some_and(|value| !readable(value)) {
            core.insert(key.into(), Value::from(fallback));
            fields.insert(key.into(), Value::from(fallback));
        }
    }
    Thread::from_json(&Value::Object(core.clone())).ok()?;
    let readable = |key: &str, value: &Value| {
        let mut probe = core.clone();
        probe.insert(key.into(), value.clone());
        Thread::from_json(&Value::Object(probe)).is_ok()
    };
    let optional: Vec<String> = fields.keys().filter(|key| !CORE.contains(&key.as_str())).cloned().collect();
    for key in optional {
        if readable(&key, &fields[&key]) {
            continue;
        }
        let kept: Vec<Value> = match fields.remove(&key) {
            Some(Value::Array(items)) => items.into_iter().filter_map(|item| readable_item(item, |item| readable(&key, &Value::Array(vec![item.clone()])))).collect(),
            _ => Vec::new(),
        };
        if !kept.is_empty() {
            fields.insert(key, Value::Array(kept));
        }
    }
    Thread::from_json(&thread).is_ok().then_some(thread)
}

/// A list item as this build can read it: as sent, else without the engine and effort only a newer build knows.
fn readable_item(item: Value, readable: impl Fn(&Value) -> bool) -> Option<Value> {
    if readable(&item) {
        return Some(item);
    }
    let Value::Object(mut fields) = item else { return None };
    fields.remove("engine");
    fields.remove("effort");
    let item = Value::Object(fields);
    readable(&item).then_some(item)
}

/// Keeps the wire state and its revision apart from the state read from it.
#[derive(Default)]
pub struct RemoteWorkspaceReplica {
    wire: Option<Value>,
    revision: Option<u64>,
}

impl RemoteWorkspaceReplica {
    /// Applies `{revision, state}` or `{revision, patches}`. Unreadable state is an error, so the line
    /// can say so rather than loop.
    pub fn apply(&mut self, update: &Map<String, Value>) -> Result<ReplicaUpdate, String> {
        let revision = update.get("revision").and_then(Value::as_f64).map(|revision| revision as u64).unwrap_or(0);
        if let Some(state) = update.get("state") {
            self.wire = Some(state.clone());
        } else {
            let next = self.revision.map(|held| held + 1);
            match (&mut self.wire, next) {
                (Some(wire), Some(next)) if revision == next => {
                    let patches: Vec<WorkspacePatch> = serde_json::from_value(update.get("patches").cloned().unwrap_or(Value::Null)).map_err(|_| "Invalid workspace patch.".to_string())?;
                    if apply_workspace_patches(wire, &patches).is_err() {
                        return Ok(ReplicaUpdate::Resync);
                    }
                }
                _ if self.revision.is_some_and(|held| revision <= held) => return Ok(ReplicaUpdate::Unchanged),
                _ => return Ok(ReplicaUpdate::Resync),
            }
        }
        let state = read_remote_workspace(self.wire.as_ref().expect("a wire state"))?;
        self.revision = Some(revision);
        Ok(ReplicaUpdate::State(Box::new(state)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_hosts_state_reads_with_defaults_and_patches_follow_it() {
        let mut replica = RemoteWorkspaceReplica::default();
        let wire = json!({ "threads": [{ "id": "t1", "engine": "claude", "title": "Hello" }], "projects": [{ "id": "p", "root": "/r" }], "docks": { "t1": { "open": true } } });
        let ReplicaUpdate::State(state) = replica.apply(json!({ "revision": 4, "state": wire }).as_object().unwrap()).unwrap() else { panic!("a state") };
        assert_eq!((state.threads[0].title.as_str(), state.docks["t1"].open), ("Hello", true));
        let patch = json!({ "revision": 5, "patches": [{ "path": ["threads", 0, "title"], "value": "Renamed" }] });
        let ReplicaUpdate::State(state) = replica.apply(patch.as_object().unwrap()).unwrap() else { panic!("a state") };
        assert_eq!(state.threads[0].title, "Renamed");
        assert!(matches!(replica.apply(json!({ "revision": 9, "patches": [] }).as_object().unwrap()).unwrap(), ReplicaUpdate::Resync));
        assert!(matches!(replica.apply(json!({ "revision": 5, "patches": [] }).as_object().unwrap()).unwrap(), ReplicaUpdate::Unchanged));
    }

    #[test]
    fn what_a_newer_build_sends_costs_only_the_part_this_one_cannot_read() {
        let wire = json!({ "threads": [
            { "id": "known", "engine": "claude", "role": "designer", "effort": "galaxy", "messages": [{ "id": "m", "kind": "assistant", "text": "Done.", "at": 40 }] },
            { "id": "far", "engine": "gemini" },
            { "id": "newer", "engine": "claude", "executionPolicy": "yolo", "proposals": [{ "id": "p1", "title": "One", "prompt": "Do it", "place": {}, "proposedAt": 1 }, { "id": "p3", "title": "Three", "prompt": "Do it", "place": {}, "proposedAt": 1, "engine": "gemini" }] },
            { "id": "old", "engine": "claude", "model": "gpt-6-sol", "messages": [{ "id": "u", "kind": "user", "text": "hi", "detail": "Thread updates", "at": 5 }] },
        ] });
        let state = read_remote_workspace(&wire).unwrap();
        assert_eq!(state.threads.iter().map(|thread| thread.id.as_str()).collect::<Vec<_>>(), ["known", "far", "newer", "old"], "a thread on an engine this build does not know is still listed, to be read");
        assert_eq!(state.threads[1].engine.as_str(), "gemini");
        let newer = &state.threads[2];
        assert_eq!(newer.proposals.as_ref().map(|proposals| proposals.iter().map(|proposal| proposal.id.as_str()).collect::<Vec<_>>()), Some(vec!["p1", "p3"]), "a suggestion on an engine this build does not know is still shown");
        assert_eq!(newer.execution_policy.as_str(), "yolo", "a mode only a newer build knows is kept as sent");
        assert_eq!((state.threads[0].role, state.threads[0].ranked_at, state.threads[0].effort.map(|effort| effort.as_str())), (None, 40, Some("galaxy")), "an unknown role is left out, an unknown effort kept, and a thread an older build sent ranks by when it last spoke");
        assert!(serde_json::from_value::<Thread>(json!({ "id": "x", "title": "t", "engine": "gemini", "executionPolicy": "confirm", "messages": [], "continuationStatus": "none", "lastChangeSnapshot": { "files": [], "capturedAt": 0 }, "updatedAt": 0 })).is_err(), "only another build's state reads unknown engines");
        assert_eq!(state.threads[3].model.as_deref(), Some("gpt-6.1-sol"));
        assert!(state.threads[3].messages[0].origin.is_some(), "a labelled message reads by the origin its label names");
    }

    #[test]
    fn an_engine_only_a_newer_build_reports_is_listed_by_its_id_and_updated_there() {
        let ready = json!({ "access": "ready", "version": "1.0.0", "latest": "1.1.0", "fix": "gemini update" });
        let state = read_remote_workspace(&json!({ "engineStatus": { "claude": { "access": "ready" }, "gemini": ready } })).unwrap();
        let paired = act_core::state::PairedComputer { link: act_core::domain::platform::ComputerLink { id: "linux".into(), name: "Linux".into(), host: "linux.ts.net".into(), status: act_core::domain::platform::ComputerStatus::Connected, error: None, paired_at: 1, capabilities: None }, state: Some(Box::new(state)) };
        let upkeep = act_core::view::settings::computer_upkeep(&[paired], None);
        let items: Vec<(&str, &str)> = upkeep[0].items.iter().map(|item| (item.key.as_str(), item.label.as_str())).collect();
        assert_eq!(&items[1..], [("claude", "Claude"), ("gemini", "gemini")]);
        let gemini = &upkeep[0].items[2];
        assert!(gemini.action.is_none() && gemini.state.text.ends_with("update it there"), "{:?}", gemini.state);
    }
}
