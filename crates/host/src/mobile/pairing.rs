//! Who has been let in, and who is being kept out: the paired devices, kept on disk only as hashes of
//! their tokens, and the one pairing code on screen. A wrong code is counted against its sender.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use act_core::domain::mobile::{PairedDeviceKind, PairedDeviceView};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::messages::is_phone_push;

pub const PAIRING_CODE_TTL_MS: i64 = 2 * 60 * 1_000;
const PAIRING_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const PAIRING_CODE_LENGTH: usize = 8;
pub const MAX_PAIRING_FAILURES: u32 = 5;
pub const PAIRING_LOCKOUT_MS: i64 = 5 * 60 * 1_000;
pub const MAX_PAIRED_DEVICES: usize = 16;
const UNNAMED_DEVICE: &str = "Phone";
const MAX_DEVICE_NAME: usize = 128;
/// How far `lastSeenAt` may drift before it is worth a write.
const SEEN_WRITE_INTERVAL_MS: i64 = 60 * 1_000;
/// How often a list that could not be read is read again, when a device that may be on it knocks.
const REREAD_INTERVAL_MS: i64 = 5 * 1_000;
const MAX_EDITS_PER_DEVICE: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhonePush {
    pub token: String,
    /// "production" or "sandbox".
    pub environment: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub id: String,
    pub kind: PairedDeviceKind,
    pub name: String,
    pub token_hash: String,
    pub paired_at: i64,
    pub last_seen_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PhonePush>,
}

impl PairedDevice {
    pub fn view(&self) -> PairedDeviceView {
        PairedDeviceView { id: self.id.clone(), kind: self.kind, name: self.name.clone(), paired_at: self.paired_at, last_seen_at: self.last_seen_at }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub code: String,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct PairingAttempts {
    failures: u32,
    locked_until: Option<i64>,
    last_failure_at: i64,
}

impl PairingAttempts {
    fn locked(&self, at: i64) -> bool {
        self.locked_until.is_some_and(|until| at < until)
    }

    fn stale(&self, at: i64) -> bool {
        !self.locked(at) && at - self.last_failure_at >= PAIRING_LOCKOUT_MS
    }

    /// One more wrong code. Reaching the limit shuts the door and starts the count again behind it.
    fn failed(self, at: i64) -> Self {
        let failures = self.failures + 1;
        if failures < MAX_PAIRING_FAILURES {
            return Self { failures, locked_until: self.locked_until, last_failure_at: at };
        }
        Self { failures: 0, locked_until: Some(at + PAIRING_LOCKOUT_MS), last_failure_at: at }
    }
}

/// What came of a phone's edit of a queued message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    Ok,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredEdit {
    request_id: String,
    outcome: EditOutcome,
}

impl StoredEdit {
    fn read(value: &Value) -> Option<Self> {
        let request_id = value.get("requestId")?.as_str()?.to_string();
        match value.get("ok")? {
            Value::Bool(true) => Some(Self { request_id, outcome: EditOutcome::Ok }),
            Value::Bool(false) => Some(Self { request_id, outcome: EditOutcome::Failed(value.get("message")?.as_str()?.to_string()) }),
            _ => None,
        }
    }

    fn write(&self) -> Value {
        match &self.outcome {
            EditOutcome::Ok => json!({ "requestId": self.request_id, "ok": true }),
            EditOutcome::Failed(message) => json!({ "requestId": self.request_id, "ok": false, "message": message }),
        }
    }
}

/// Why a pairing was refused, in the code the phone decides on and the words it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRefusal {
    /// "rate-limited", "expired-code", "unauthorized" or "internal".
    pub code: &'static str,
    pub message: String,
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the system provides randomness");
    bytes
}

/// Random characters from the unmistakable alphabet; 32 divides 256, so there is no bias.
pub fn generate_pairing_code() -> String {
    random_bytes::<PAIRING_CODE_LENGTH>().iter().map(|byte| PAIRING_ALPHABET[*byte as usize % PAIRING_ALPHABET.len()] as char).collect()
}

pub fn constant_time_equals(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter().zip(right).fold(0u8, |difference, (a, b)| difference | (a ^ b)) == 0
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn readable_name(name: &str) -> String {
    let trimmed: String = name.trim().chars().take(MAX_DEVICE_NAME).collect();
    if trimmed.is_empty() { UNNAMED_DEVICE.into() } else { trimmed }
}

fn stored_device(value: &Value) -> Option<PairedDevice> {
    let mut record = value.as_object()?.clone();
    if !record.contains_key("kind") {
        record.insert("kind".into(), json!("phone"));
    }
    let id_ok = record.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty());
    let hash_ok = record.get("tokenHash").and_then(Value::as_str).is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    let seen_ok = record.get("lastSeenAt").is_some_and(|seen| seen.is_null() || seen.is_number());
    let paired_ok = record.get("pairedAt").is_some_and(Value::is_number);
    if !id_ok || !hash_ok || !seen_ok || !paired_ok || !record.get("name").is_some_and(Value::is_string) {
        return None;
    }
    // A push token that does not read is dropped on its own; the device stays paired.
    if record.get("push").is_some_and(|push| !is_phone_push(push)) {
        record.remove("push");
    }
    serde_json::from_value(Value::Object(record)).ok()
}

/// The paired devices and the code on screen. Only hashes of device tokens are ever written.
pub struct PairingStore {
    file: PathBuf,
    devices: Vec<PairedDevice>,
    /// Records this version does not read, written back untouched.
    kept: Vec<Value>,
    edits: HashMap<String, Vec<StoredEdit>>,
    fields: Map<String, Value>,
    /// Set while the file is there but could not be read; nothing is written over it then.
    unreadable_since: Option<i64>,
    code: Option<PairingCode>,
    attempts: HashMap<String, PairingAttempts>,
}

enum Loaded {
    Nothing,
    Read { devices: Vec<PairedDevice>, kept: Vec<Value>, fields: Map<String, Value> },
}

impl PairingStore {
    pub fn open(file: impl Into<PathBuf>, at: i64) -> Self {
        let mut store = Self { file: file.into(), devices: Vec::new(), kept: Vec::new(), edits: HashMap::new(), fields: Map::new(), unreadable_since: None, code: None, attempts: HashMap::new() };
        store.load(at);
        store
    }

    /// Whether the list on disk is out of reach, so a device missing from it may only be missing for now.
    pub fn unreadable(&self) -> bool {
        self.unreadable_since.is_some()
    }

    fn load(&mut self, at: i64) {
        let Some(loaded) = self.read(at) else { return };
        self.unreadable_since = None;
        let (devices, kept, mut fields) = match loaded {
            Loaded::Nothing => (Vec::new(), Vec::new(), Map::new()),
            Loaded::Read { devices, kept, fields } => (devices, kept, fields),
        };
        let edits = fields.remove("edits");
        self.devices = devices;
        self.kept = kept;
        self.fields = fields;
        self.edits = HashMap::new();
        if let Some(Value::Object(edits)) = edits {
            for (device_id, list) in edits {
                if let Value::Array(list) = list {
                    let mut read: Vec<StoredEdit> = list.iter().filter_map(StoredEdit::read).collect();
                    let skip = read.len().saturating_sub(MAX_EDITS_PER_DEVICE);
                    read.drain(..skip);
                    self.edits.insert(device_id, read);
                }
            }
        }
    }

    /// No file is no device. A file that will not read is left alone; one that reads but is not a
    /// list is set aside under another name and the list starts over.
    fn read(&mut self, at: i64) -> Option<Loaded> {
        let text = match std::fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(Loaded::Nothing),
            Err(error) => {
                tracing::error!("Could not read the paired device list: {error}");
                self.unreadable_since = Some(at);
                return None;
            }
        };
        let parsed = serde_json::from_str::<Value>(&text).ok().and_then(|value| match value {
            Value::Object(mut stored) => match stored.remove("devices") {
                Some(Value::Array(records)) => Some((records, stored)),
                _ => None,
            },
            _ => None,
        });
        let Some((records, fields)) = parsed else {
            tracing::error!("The paired device list is damaged; setting it aside.");
            if let Err(error) = std::fs::rename(&self.file, format!("{}.damaged", self.file.to_string_lossy())) {
                tracing::error!("Could not set the damaged paired device list aside: {error}");
                self.unreadable_since = Some(at);
                return None;
            }
            return Some(Loaded::Nothing);
        };
        let mut devices = Vec::new();
        let mut kept = Vec::new();
        for record in records {
            match stored_device(&record) {
                Some(device) if devices.len() < MAX_PAIRED_DEVICES => devices.push(device),
                _ => kept.push(record),
            }
        }
        Some(Loaded::Read { devices, kept, fields })
    }

    fn retry_read(&mut self, at: i64) {
        if self.unreadable_since.is_some_and(|since| at - since >= REREAD_INTERVAL_MS) {
            self.load(at);
        }
    }

    /// Written beside and renamed over. Refused while the file on disk could not be read.
    fn write(&self) -> bool {
        if self.unreadable_since.is_some() {
            return false;
        }
        let mut stored = self.fields.clone();
        stored.insert("version".into(), json!(1));
        let mut devices: Vec<Value> = self.devices.iter().filter_map(|device| serde_json::to_value(device).ok()).collect();
        devices.extend(self.kept.iter().cloned());
        stored.insert("devices".into(), Value::Array(devices));
        if !self.edits.is_empty() {
            let edits: Map<String, Value> = self.edits.iter().map(|(device, list)| (device.clone(), Value::Array(list.iter().map(StoredEdit::write).collect()))).collect();
            stored.insert("edits".into(), Value::Object(edits));
        }
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = self.file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let staging = PathBuf::from(format!("{}.tmp", self.file.to_string_lossy()));
            write_private(&staging, Value::Object(stored).to_string().as_bytes())?;
            std::fs::rename(&staging, &self.file)
        })();
        if let Err(error) = &result {
            tracing::error!("Could not write the paired device list: {error}");
        }
        result.is_ok()
    }

    pub fn list(&self) -> &[PairedDevice] {
        &self.devices
    }

    pub fn views(&self) -> Vec<PairedDeviceView> {
        self.devices.iter().map(PairedDevice::view).collect()
    }

    /// Mints the code the QR carries. A second mint discards the first.
    pub fn mint(&mut self, at: i64) -> PairingCode {
        let code = PairingCode { code: generate_pairing_code(), created_at: at, expires_at: at + PAIRING_CODE_TTL_MS };
        self.code = Some(code.clone());
        code
    }

    /// The code on screen, or None once it has expired or been spent.
    pub fn pending(&mut self, at: i64) -> Option<PairingCode> {
        if self.code.as_ref().is_some_and(|code| at >= code.expires_at) {
            self.code = None;
        }
        self.code.clone()
    }

    pub fn discard_code(&mut self) {
        self.code = None;
    }

    /// Trades a code for a token of the device's own. A wrong guess leaves the code standing and is
    /// counted against `source`.
    pub fn redeem(&mut self, code: &str, device_name: &str, source: &str, at: i64, kind: PairedDeviceKind) -> Result<(PairedDevice, String), PairingRefusal> {
        let attempts = self.attempts.get(source).copied().filter(|held| !held.stale(at)).unwrap_or_default();
        if attempts.locked(at) {
            return Err(PairingRefusal { code: "rate-limited", message: "Too many wrong codes. Wait a few minutes and scan the code again.".into() });
        }
        let Some(pending) = self.pending(at) else {
            return Err(PairingRefusal { code: "expired-code", message: "That pairing code has expired. Show a new one on the computer.".into() });
        };
        if !constant_time_equals(pending.code.as_bytes(), code.trim().to_uppercase().as_bytes()) {
            self.attempts.insert(source.to_string(), attempts.failed(at));
            self.attempts.retain(|_, attempts| !attempts.stale(at));
            return Err(PairingRefusal { code: "expired-code", message: "That pairing code is wrong or has expired. Show a new one on the computer.".into() });
        }
        self.attempts.remove(source);
        self.retry_read(at);
        if self.unreadable() {
            return Err(PairingRefusal { code: "internal", message: "This computer could not read its paired devices. Try again in a moment.".into() });
        }
        if self.devices.len() >= MAX_PAIRED_DEVICES {
            return Err(PairingRefusal { code: "unauthorized", message: format!("This computer is already paired with {MAX_PAIRED_DEVICES} devices. Revoke one first.") });
        }
        self.code = None;
        let token = hex::encode(random_bytes::<32>());
        let device = PairedDevice { id: uuid::Uuid::new_v4().to_string(), kind, name: readable_name(device_name), token_hash: hash_token(&token), paired_at: at, last_seen_at: Some(at), push: None };
        self.devices.push(device.clone());
        self.write();
        Ok((device, token))
    }

    /// The device a token belongs to, or None when no stored hash matches it.
    pub fn authenticate(&mut self, token: &str, at: i64) -> Option<PairedDevice> {
        let hash = hash_token(token);
        let find = |devices: &[PairedDevice]| devices.iter().find(|device| constant_time_equals(device.token_hash.as_bytes(), hash.as_bytes())).cloned();
        let found = find(&self.devices);
        if found.is_some() || !self.unreadable() {
            return found;
        }
        self.retry_read(at);
        find(&self.devices)
    }

    pub fn mark_seen(&mut self, device_id: &str, at: i64) {
        let Some(device) = self.devices.iter_mut().find(|device| device.id == device_id) else { return };
        if device.last_seen_at.is_some_and(|seen| at - seen < SEEN_WRITE_INTERVAL_MS) {
            return;
        }
        device.last_seen_at = Some(at);
        self.write();
    }

    /// Phones that take notices while closed.
    pub fn push_targets(&self) -> Vec<(PairedDevice, PhonePush)> {
        self.devices.iter().filter(|device| device.kind == PairedDeviceKind::Phone).filter_map(|device| device.push.clone().map(|push| (device.clone(), push))).collect()
    }

    /// Where a phone now takes pushes, or that it no longer does. An older pairing of the same phone
    /// stops holding the token, so the phone is not told twice.
    pub fn set_push(&mut self, device_id: &str, push: Option<PhonePush>) {
        let Some(device) = self.devices.iter().find(|device| device.id == device_id) else { return };
        if device.kind != PairedDeviceKind::Phone || device.push == push {
            return;
        }
        for device in &mut self.devices {
            if device.id == device_id {
                device.push = push.clone();
            } else if push.as_ref().is_some_and(|push| device.push.as_ref().is_some_and(|held| held.token == push.token)) {
                device.push = None;
            }
        }
        self.write();
    }

    pub fn edit_outcome(&self, device_id: &str, request_id: &str) -> Option<EditOutcome> {
        self.edits.get(device_id)?.iter().find(|edit| edit.request_id == request_id).map(|edit| edit.outcome.clone())
    }

    pub fn remember_edit(&mut self, device_id: &str, request_id: &str, outcome: EditOutcome) {
        if !self.devices.iter().any(|device| device.id == device_id) {
            return;
        }
        let list = self.edits.entry(device_id.to_string()).or_default();
        list.retain(|edit| edit.request_id != request_id);
        list.push(StoredEdit { request_id: request_id.to_string(), outcome });
        let skip = list.len().saturating_sub(MAX_EDITS_PER_DEVICE);
        list.drain(..skip);
        self.write();
    }

    pub fn revoke(&mut self, device_id: &str) -> bool {
        let before = self.devices.len();
        self.devices.retain(|device| device.id != device_id);
        if self.devices.len() == before {
            return false;
        }
        self.edits.remove(device_id);
        self.write();
        true
    }
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_buys_one_device_and_wrong_guesses_lock_their_sender_out() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mobile-devices.v1.json");
        let mut store = PairingStore::open(&file, 0);
        let code = store.mint(0).code;
        for _ in 0..MAX_PAIRING_FAILURES {
            assert_eq!(store.redeem("WRONG000", "Phone", "1.2.3.4", 10, PairedDeviceKind::Phone).unwrap_err().code, "expired-code");
        }
        assert_eq!(store.redeem(&code, "Phone", "1.2.3.4", 20, PairedDeviceKind::Phone).unwrap_err().code, "rate-limited");
        let (device, token) = store.redeem(&code.to_lowercase(), "  My iPhone ", "5.6.7.8", 30, PairedDeviceKind::Phone).unwrap();
        assert_eq!(device.name, "My iPhone");
        assert!(store.redeem(&code, "Again", "5.6.7.8", 40, PairedDeviceKind::Phone).is_err());
        let mut reopened = PairingStore::open(&file, 50);
        assert_eq!(reopened.authenticate(&token, 50).map(|found| found.id), Some(device.id));
        assert!(!std::fs::read_to_string(&file).unwrap().contains(&token));
    }

    #[test]
    fn a_damaged_list_is_set_aside_and_records_this_build_cannot_read_survive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("devices.json");
        std::fs::write(&file, "{not json").unwrap();
        let mut store = PairingStore::open(&file, 0);
        assert!(dir.path().join("devices.json.damaged").exists());
        let code = store.mint(0).code;
        store.redeem(&code, "Phone", "s", 1, PairedDeviceKind::Phone).unwrap();
        let mut stored: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        stored["devices"].as_array_mut().unwrap().push(json!({ "id": "future", "kind": "watch" }));
        std::fs::write(&file, stored.to_string()).unwrap();
        let mut store = PairingStore::open(&file, 2);
        store.mark_seen(&store.list()[0].id.clone(), 10_000_000);
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(written.contains("\"future\""));
    }
}
