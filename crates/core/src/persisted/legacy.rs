//! The thread store as builds before the SQLite database kept it: JSON strings under keys of the
//! window's own storage (`src/application/task-store.ts`, `parseThreadStore` and
//! `migrateV1ToV2` in `src/domain/thread-storage.ts`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::js;
use super::storage::{labelled_origin, typed_store, validate_thread_store_data, DecodedThreadStore};
use super::store::{ThreadStoreData, THREAD_STORE_VERSION};

/// Where a window keeps short strings between launches (`KeyValueStorage`).
pub trait KeyValueStorage {
    fn get_item(&self, key: &str) -> Option<String>;
    fn set_item(&mut self, key: &str, value: String) -> Result<(), String>;
}

pub struct StoreKeys {
    pub envelope: Option<&'static str>,
    pub tasks: &'static str,
    pub projects: &'static str,
    pub worktrees: Option<&'static str>,
    pub last_folder: &'static str,
}

pub const TASK_STORE_V1: StoreKeys = StoreKeys {
    envelope: None,
    tasks: "aicodingtool.tasks.v1",
    projects: "aicodingtool.projects.v1",
    worktrees: None,
    last_folder: "aicodingtool.last-folder.v1",
};

pub const TASK_STORE_V2: StoreKeys = StoreKeys {
    envelope: Some("aicodingtool.store.v2"),
    tasks: "aicodingtool.tasks.v2",
    projects: "aicodingtool.projects.v2",
    worktrees: Some("aicodingtool.worktrees.v2"),
    last_folder: "aicodingtool.last-folder.v2",
};

pub const LEGACY_TASK_STORE_V1: StoreKeys = StoreKeys {
    envelope: None,
    tasks: "threadline.tasks.v1",
    projects: "threadline.projects.v1",
    worktrees: None,
    last_folder: "threadline.last-folder.v1",
};

pub const LEGACY_TASK_STORE_V2: StoreKeys = StoreKeys {
    envelope: Some("threadline.store.v2"),
    tasks: "threadline.tasks.v2",
    projects: "threadline.projects.v2",
    worktrees: Some("threadline.worktrees.v2"),
    last_folder: "threadline.last-folder.v2",
};

/// The four stored strings, each JSON.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageValues {
    pub tasks: Option<String>,
    pub projects: Option<String>,
    /// Absent in everything written before a checkout could hold more than one thread.
    #[serde(default)]
    pub worktrees: Option<String>,
    pub last_folder: Option<String>,
}

impl StorageValues {
    fn is_empty(&self) -> bool {
        self.tasks.is_none() && self.projects.is_none() && self.worktrees.is_none() && self.last_folder.is_none()
    }

    fn read(storage: &dyn KeyValueStorage, keys: &StoreKeys) -> Self {
        StorageValues {
            tasks: storage.get_item(keys.tasks),
            projects: storage.get_item(keys.projects),
            worktrees: keys.worktrees.and_then(|key| storage.get_item(key)),
            last_folder: storage.get_item(keys.last_folder),
        }
    }

    fn none_written(&self) -> bool {
        self.tasks.is_none() && self.projects.is_none() && self.last_folder.is_none()
    }

    fn complete(&self) -> bool {
        self.tasks.is_some() && self.projects.is_some() && self.last_folder.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Corrupt,
    Storage,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ThreadStoreParseResult {
    Ok {
        data: ThreadStoreData,
        /// Threads this build cannot read, which stay on disk untouched.
        hidden_tasks: usize,
        source_version: u8,
        preserved_v1: Option<StorageValues>,
    },
    Failed {
        source_version: u8,
        error_kind: ErrorKind,
        errors: Vec<String>,
        preserved_v1: Option<StorageValues>,
        raw: StorageValues,
    },
}

impl ThreadStoreParseResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, ThreadStoreParseResult::Ok { .. })
    }
}

fn empty_data() -> ThreadStoreData {
    ThreadStoreData { version: THREAD_STORE_VERSION, tasks: Vec::new(), projects: Vec::new(), worktrees: Vec::new(), last_folder: None }
}

fn corrupt(source_version: u8, errors: Vec<String>, raw: StorageValues) -> ThreadStoreParseResult {
    ThreadStoreParseResult::Failed {
        source_version,
        error_kind: ErrorKind::Corrupt,
        errors,
        preserved_v1: if source_version == 1 { Some(raw.clone()) } else { None },
        raw,
    }
}

fn parse_json(value: &Option<String>, label: &str, errors: &mut Vec<String>) -> Value {
    match value {
        None => Value::Null,
        Some(text) => serde_json::from_str(text).unwrap_or_else(|_| {
            errors.push(format!("{label} is not valid JSON"));
            Value::Null
        }),
    }
}

pub fn parse_thread_store(raw: StorageValues) -> ThreadStoreParseResult {
    if raw.is_empty() {
        return ThreadStoreParseResult::Ok { data: empty_data(), hidden_tasks: 0, source_version: 0, preserved_v1: None };
    }
    match decode_v2(&raw) {
        Decoded::V2(values) => match validate_thread_store_data(values).and_then(|validated| typed_store(validated).map_err(|error| vec![error])) {
            Ok((data, hidden_tasks)) => ThreadStoreParseResult::Ok { data, hidden_tasks, source_version: 2, preserved_v1: None },
            Err(errors) => corrupt(2, errors, raw),
        },
        Decoded::Corrupt(errors) => ThreadStoreParseResult::Failed {
            source_version: 2,
            error_kind: ErrorKind::Corrupt,
            errors,
            preserved_v1: None,
            raw,
        },
        Decoded::None => migrate_v1_to_v2(raw),
    }
}

enum Decoded {
    None,
    V2(DecodedThreadStore),
    Corrupt(Vec<String>),
}

fn versioned(value: &Value) -> bool {
    js::record(Some(value)).is_some_and(|record| record.get("version").and_then(Value::as_f64) == Some(2.0))
}

fn versioned_with_value(value: &Value) -> bool {
    versioned(value) && value.get("value").is_some()
}

fn decode_v2(raw: &StorageValues) -> Decoded {
    if raw.is_empty() {
        return Decoded::None;
    }
    let mut errors = Vec::new();
    let tasks = parse_json(&raw.tasks, "tasks", &mut errors);
    let projects = parse_json(&raw.projects, "projects", &mut errors);
    let worktrees = parse_json(&raw.worktrees, "worktrees", &mut errors);
    let last_folder = parse_json(&raw.last_folder, "lastFolder", &mut errors);
    if ![&tasks, &projects, &last_folder].into_iter().any(versioned) {
        return Decoded::None;
    }
    if !errors.is_empty() {
        return Decoded::Corrupt(errors);
    }
    if ![&tasks, &projects, &last_folder].into_iter().all(versioned_with_value) {
        return Decoded::Corrupt(vec!["version 2 storage must contain all three versioned values".to_owned()]);
    }
    if !worktrees.is_null() && !versioned_with_value(&worktrees) {
        return Decoded::Corrupt(vec!["version 2 worktrees is not a versioned value".to_owned()]);
    }
    let take = |value: Value| match value {
        Value::Object(mut record) => record.remove("value").unwrap_or(Value::Null),
        _ => Value::Null,
    };
    Decoded::V2(DecodedThreadStore {
        tasks: take(tasks),
        projects: take(projects),
        worktrees: if worktrees.is_null() { Value::Null } else { take(worktrees) },
        last_folder: take(last_folder),
    })
}

const LEGACY_MODES: [(&str, &str); 4] =
    [("default", "confirm"), ("plan", "plan"), ("acceptEdits", "allow-edits"), ("auto", "autonomous")];

fn legacy_mode(value: Option<&Value>) -> Option<&'static str> {
    let mode = value?.as_str()?;
    LEGACY_MODES.iter().find(|(legacy, _)| *legacy == mode).map(|(_, policy)| *policy)
}

/// `root` without trailing separators, unless that leaves nothing.
pub fn normalize_project_root(root: &str) -> &str {
    let normalized = root.trim_end_matches(['/', '\\']);
    if normalized.is_empty() {
        root
    } else {
        normalized
    }
}

/// `encodeURIComponent`.
pub fn encode_uri_component(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub fn legacy_project_id(root: &str) -> String {
    format!("legacy-project-{}", encode_uri_component(normalize_project_root(root)))
}

pub fn migrate_v1_to_v2(raw: StorageValues) -> ThreadStoreParseResult {
    let mut errors = Vec::new();
    let tasks_value = parse_json(&raw.tasks, "tasks", &mut errors);
    let projects_value = parse_json(&raw.projects, "projects", &mut errors);
    let last_folder = raw.last_folder.clone();
    if !errors.is_empty() {
        return corrupt(1, errors, raw);
    }
    if !tasks_value.is_null() && !tasks_value.is_array() {
        errors.push("tasks must be an array".to_owned());
    }
    if !projects_value.is_null() && !projects_value.is_array() {
        errors.push("projects must be an array".to_owned());
    }
    if !errors.is_empty() {
        return corrupt(1, errors, raw);
    }
    let legacy_projects_value = projects_value.as_array().cloned().unwrap_or_default();
    if legacy_projects_value.iter().any(|value| !value.is_string()) {
        errors.push("projects must contain only strings".to_owned());
    }
    if !errors.is_empty() {
        return corrupt(1, errors, raw);
    }
    let legacy_tasks = tasks_value.as_array().cloned().unwrap_or_default();
    let mut roots: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut add_root = |root: &str| {
        let root = normalize_project_root(root).to_owned();
        if seen.insert(root.clone()) {
            roots.push(root);
        }
    };
    for project in legacy_projects_value.iter().filter_map(Value::as_str).filter(|root| !root.is_empty()) {
        add_root(project);
    }
    for value in &legacy_tasks {
        let Some(task) = value.as_object() else {
            errors.push("tasks contains a non-object value".to_owned());
            continue;
        };
        if let Some(folder) = task.get("folder").and_then(Value::as_str).filter(|folder| !folder.is_empty()) {
            add_root(folder);
        }
    }
    let projects: Vec<Value> = roots.iter().map(|root| json!({ "id": legacy_project_id(root), "root": root })).collect();
    let project_by_root: BTreeMap<String, String> = roots.iter().map(|root| (root.clone(), legacy_project_id(root))).collect();
    let tasks: Vec<Value> = legacy_tasks
        .iter()
        .enumerate()
        .filter_map(|(index, value)| migrate_legacy_thread(value, index, &project_by_root, &mut errors))
        .collect();
    if !errors.is_empty() {
        return corrupt(1, errors, raw);
    }
    match (typed(tasks), typed(projects)) {
        (Ok(tasks), Ok(projects)) => ThreadStoreParseResult::Ok {
            data: ThreadStoreData { version: THREAD_STORE_VERSION, tasks, projects, worktrees: Vec::new(), last_folder },
            hidden_tasks: 0,
            source_version: 1,
            preserved_v1: Some(raw),
        },
        (Err(error), _) | (_, Err(error)) => corrupt(1, vec![error.to_string()], raw),
    }
}

fn typed<T: serde::de::DeserializeOwned>(values: Vec<Value>) -> Result<Vec<T>, serde_json::Error> {
    values.into_iter().map(serde_json::from_value).collect()
}

fn migrate_legacy_thread(
    value: &Value,
    index: usize,
    project_by_root: &BTreeMap<String, String>,
    errors: &mut Vec<String>,
) -> Option<Value> {
    let Some(task) = value.as_object() else {
        errors.push(format!("tasks[{index}] must be an object"));
        return None;
    };
    let id = task.get("id").and_then(Value::as_str).filter(|id| !id.is_empty());
    let title = task.get("title").and_then(Value::as_str);
    let folder = task.get("folder").and_then(Value::as_str);
    let mode = legacy_mode(task.get("mode"));
    let changed_files = task
        .get("changedFiles")
        .and_then(Value::as_array)
        .filter(|files| files.iter().all(Value::is_string));
    let updated_at = task.get("updatedAt").filter(|value| value.is_number());
    if id.is_none() {
        errors.push(format!("tasks[{index}].id must be a non-empty string"));
    }
    if title.is_none() {
        errors.push(format!("tasks[{index}].title must be a string"));
    }
    if folder.is_none() {
        errors.push(format!("tasks[{index}].folder must be a string"));
    }
    if mode.is_none() {
        errors.push(format!("tasks[{index}].mode is invalid"));
    }
    if !task.get("messages").is_some_and(Value::is_array) {
        errors.push(format!("tasks[{index}].messages must be an array"));
    }
    if changed_files.is_none() {
        errors.push(format!("tasks[{index}].changedFiles must be an array of strings"));
    }
    if updated_at.is_none() {
        errors.push(format!("tasks[{index}].updatedAt must be a number"));
    }
    let messages = migrate_messages(task.get("messages"), index, errors);
    let (Some(id), Some(title), Some(folder), Some(mode), Some(changed_files), Some(updated_at), Some(messages)) =
        (id, title, folder, mode, changed_files, updated_at, messages)
    else {
        return None;
    };
    let session = task.get("sessionId");
    let continuation_status = match session {
        None => "none",
        Some(Value::String(session)) if !js::trim(session).is_empty() => "available",
        Some(_) => "invalid",
    };
    let mut migrated = Map::new();
    migrated.insert("id".into(), Value::from(id));
    migrated.insert("title".into(), Value::from(title));
    if !folder.is_empty() {
        let project = project_by_root
            .get(normalize_project_root(folder))
            .cloned()
            .unwrap_or_else(|| legacy_project_id(folder));
        migrated.insert("projectId".into(), Value::from(project));
    }
    migrated.insert("executionPolicy".into(), Value::from(mode));
    migrated.insert("engine".into(), Value::from("claude"));
    migrated.insert("messages".into(), Value::Array(messages));
    if continuation_status == "available" {
        let session = session.and_then(Value::as_str).unwrap_or_default();
        migrated.insert("continuation".into(), json!({ "provider": "claude", "value": session }));
    }
    migrated.insert("continuationStatus".into(), Value::from(continuation_status));
    migrated.insert(
        "lastChangeSnapshot".into(),
        json!({ "files": changed_files, "capturedAt": updated_at }),
    );
    migrated.insert("updatedAt".into(), updated_at.clone());
    Some(Value::Object(migrated))
}

fn migrate_messages(value: Option<&Value>, task_index: usize, errors: &mut Vec<String>) -> Option<Vec<Value>> {
    let messages = value?.as_array()?;
    let mut migrated = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let valid = message.as_object().filter(|message| {
            message.get("id").is_some_and(Value::is_string)
                && super::shapes::is_message_kind(message.get("kind"))
                && message.get("text").is_some_and(Value::is_string)
                && message.get("at").is_some_and(Value::is_number)
                && message.get("detail").is_none_or(Value::is_string)
        });
        let Some(message) = valid else {
            errors.push(format!("tasks[{task_index}].messages[{index}] is invalid"));
            continue;
        };
        let mut kept = Map::new();
        for key in ["id", "kind", "text", "detail", "at"] {
            if let Some(field) = message.get(key) {
                kept.insert(key.into(), field.clone());
            }
        }
        migrated.push(labelled_origin(Value::Object(kept)));
    }
    Some(migrated)
}

/// The store as the four versioned JSON strings a window's storage keeps.
pub fn serialize_thread_store(data: &ThreadStoreData) -> Result<StorageValues, serde_json::Error> {
    let versioned = |value: Value| serde_json::to_string(&json!({ "version": THREAD_STORE_VERSION, "value": value }));
    Ok(StorageValues {
        tasks: Some(versioned(serde_json::to_value(&data.tasks)?)?),
        projects: Some(versioned(serde_json::to_value(&data.projects)?)?),
        worktrees: Some(versioned(serde_json::to_value(&data.worktrees)?)?),
        last_folder: Some(versioned(serde_json::to_value(&data.last_folder)?)?),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskStoreSaveError {
    LoadRequired,
    Corrupt,
    Storage(String),
}

/// The thread store over a window's key-value storage.
pub struct TaskStore<S: KeyValueStorage> {
    storage: S,
    loaded: bool,
    writable: bool,
}

impl<S: KeyValueStorage> TaskStore<S> {
    pub fn new(storage: S) -> Self {
        TaskStore { storage, loaded: false, writable: false }
    }

    pub fn storage(&self) -> &S {
        &self.storage
    }

    pub fn into_storage(self) -> S {
        self.storage
    }

    /// The newest store the storage holds: the v2 envelope, else loose v2 keys, else v1 keys,
    /// else the same under the app's first name.
    pub fn load(&mut self) -> ThreadStoreParseResult {
        let mut envelope = self.storage.get_item(TASK_STORE_V2.envelope.expect("v2 has an envelope"));
        let mut values = None;
        if envelope.is_none() {
            let v2 = StorageValues::read(&self.storage, &TASK_STORE_V2);
            if v2.complete() {
                values = Some(v2);
            } else {
                let v1 = StorageValues::read(&self.storage, &TASK_STORE_V1);
                if !v2.none_written() || !v1.none_written() {
                    values = Some(v1);
                } else {
                    envelope = self.storage.get_item(LEGACY_TASK_STORE_V2.envelope.expect("v2 has an envelope"));
                    if envelope.is_none() {
                        let legacy_v2 = StorageValues::read(&self.storage, &LEGACY_TASK_STORE_V2);
                        values = Some(if legacy_v2.complete() {
                            legacy_v2
                        } else {
                            StorageValues::read(&self.storage, &LEGACY_TASK_STORE_V1)
                        });
                    }
                }
            }
        }
        let result = match envelope {
            Some(raw) => match parse_envelope(&raw) {
                Some(values) => parse_thread_store(values),
                None => ThreadStoreParseResult::Failed {
                    source_version: 2,
                    error_kind: ErrorKind::Corrupt,
                    errors: vec!["version 2 task storage envelope is invalid".to_owned()],
                    preserved_v1: None,
                    raw: StorageValues { tasks: Some(raw), ..StorageValues::default() },
                },
            },
            None => parse_thread_store(values.unwrap_or_default()),
        };
        self.loaded = true;
        self.writable = result.is_ok();
        result
    }

    pub fn save(&mut self, data: &ThreadStoreData) -> Result<StorageValues, TaskStoreSaveError> {
        if !self.loaded {
            return Err(TaskStoreSaveError::LoadRequired);
        }
        if !self.writable {
            return Err(TaskStoreSaveError::Corrupt);
        }
        let values = serialize_thread_store(data).map_err(|error| TaskStoreSaveError::Storage(error.to_string()))?;
        let envelope = serde_json::to_string(&values).map_err(|error| TaskStoreSaveError::Storage(error.to_string()))?;
        if let Err(error) = self.storage.set_item(TASK_STORE_V2.envelope.expect("v2 has an envelope"), envelope) {
            self.writable = false;
            return Err(TaskStoreSaveError::Storage(error));
        }
        Ok(values)
    }
}

fn parse_envelope(raw: &str) -> Option<StorageValues> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let record = value.as_object()?;
    let field = |key: &str| -> Option<Option<String>> {
        match record.get(key) {
            Some(Value::Null) => Some(None),
            Some(Value::String(text)) => Some(Some(text.clone())),
            _ => None,
        }
    };
    let worktrees = match record.get("worktrees") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return None,
    };
    Some(StorageValues { tasks: field("tasks")?, projects: field("projects")?, worktrees, last_folder: field("lastFolder")? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Memory(BTreeMap<String, String>);

    impl KeyValueStorage for Memory {
        fn get_item(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_item(&mut self, key: &str, value: String) -> Result<(), String> {
            self.0.insert(key.to_owned(), value);
            Ok(())
        }
    }

    #[test]
    fn v1_threads_migrate_with_their_folders_as_projects() {
        let mut storage = Memory::default();
        storage.0.insert(
            "aicodingtool.tasks.v1".into(),
            json!([{ "id": "t1", "title": "Fix", "folder": "/repo/", "mode": "acceptEdits", "changedFiles": ["a.rs"], "updatedAt": 5,
                     "sessionId": "s1", "messages": [{ "id": "m1", "kind": "user", "text": "hi", "detail": "Thread updates", "at": 4 }] }])
            .to_string(),
        );
        storage.0.insert("aicodingtool.projects.v1".into(), "[]".into());
        let mut store = TaskStore::new(storage);
        let ThreadStoreParseResult::Ok { data, source_version, .. } = store.load() else { panic!("store should load") };
        assert_eq!(source_version, 1);
        assert_eq!(data.projects[0].root, "/repo");
        let task = &data.tasks[0];
        assert_eq!(task.task.project_id.as_deref(), Some(data.projects[0].id.as_str()));
        assert_eq!(task.task.execution_policy.as_str(), "allow-edits");
        assert_eq!(task.task.continuation.as_ref().map(|continuation| continuation.value.as_str()), Some("s1"));
        assert!(matches!(task.messages[0].origin, Some(super::super::MessageOrigin::Coordination(_))));

        let saved = store.save(&data).expect("a loaded store saves");
        let reloaded = parse_thread_store(saved);
        assert!(matches!(reloaded, ThreadStoreParseResult::Ok { source_version: 2, .. }));
    }
}
