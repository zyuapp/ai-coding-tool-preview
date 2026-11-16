//! What a window keeps between launches, as one JSON object of string values
//! (`src/main/json-storage.ts`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use act_core::persisted::legacy::KeyValueStorage;
use serde_json::Value;

/// The file a profile keeps its window's storage in.
pub const WINDOW_STORAGE_FILE: &str = "window.v1.json";

/// The view preferences the window stores under its storage.
pub const VIEW_PREFERENCES_KEY: &str = "aicodingtool.view-preferences.v1";

/// Composer drafts by thread, stored under the window's storage.
pub const DRAFT_PROMPTS_KEY: &str = "aicodingtool.draft-prompts.v1";

/// Every write lands whole: the file is replaced, never appended to, so a crash mid-write leaves
/// the last one.
#[derive(Debug)]
pub struct JsonStorage {
    file: PathBuf,
    values: BTreeMap<String, String>,
}

impl JsonStorage {
    /// Reads the file once; a missing or unreadable one is empty, and non-string values are ignored.
    pub fn open(file: impl Into<PathBuf>) -> Self {
        let file = file.into();
        let values = read_values(&file);
        JsonStorage { file, values }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) -> std::io::Result<()> {
        self.values.insert(key.to_owned(), value.into());
        self.write()
    }

    /// Takes on values from elsewhere without overwriting any already held. True when any was new.
    pub fn adopt(&mut self, incoming: impl IntoIterator<Item = (String, String)>) -> std::io::Result<bool> {
        let mut changed = false;
        for (key, value) in incoming {
            if self.values.contains_key(&key) {
                continue;
            }
            self.values.insert(key, value);
            changed = true;
        }
        if changed {
            self.write()?;
        }
        Ok(changed)
    }

    fn write(&self) -> std::io::Result<()> {
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut draft = self.file.clone().into_os_string();
        draft.push(format!(".{}.tmp", std::process::id()));
        let draft = PathBuf::from(draft);
        std::fs::write(&draft, serde_json::to_string(&self.values).map_err(std::io::Error::other)?)?;
        std::fs::rename(&draft, &self.file)
    }
}

impl KeyValueStorage for JsonStorage {
    fn get_item(&self, key: &str) -> Option<String> {
        self.get(key).map(str::to_owned)
    }

    fn set_item(&mut self, key: &str, value: String) -> Result<(), String> {
        self.set(key, value).map_err(|error| error.to_string())
    }
}

fn read_values(file: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(file) else { return BTreeMap::new() };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => map
            .into_iter()
            .filter_map(|(key, value)| match value {
                Value::String(text) => Some((key, text)),
                _ => None,
            })
            .collect(),
        _ => BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_survive_a_reopen_and_adopt_keeps_what_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested").join(WINDOW_STORAGE_FILE);
        let mut storage = JsonStorage::open(&file);
        storage.set("a", "1").unwrap();
        let changed = storage.adopt([("a".to_owned(), "x".to_owned()), ("b".to_owned(), "2".to_owned())]).unwrap();
        assert!(changed);
        let reopened = JsonStorage::open(&file);
        assert_eq!(reopened.get("a"), Some("1"));
        assert_eq!(reopened.get("b"), Some("2"));
        assert_eq!(std::fs::read_dir(file.parent().unwrap()).unwrap().count(), 1);
    }
}
