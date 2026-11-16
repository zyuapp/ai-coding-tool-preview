//! The computers this one is paired with, and a line to each. Every change to who is paired or how a
//! line stands is pushed whole, and every state a computer publishes is pushed as it arrives.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use act_core::domain::platform::{ComputerLink, ComputerStatus, DiscoveredComputer};
use act_core::WorkspaceState;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::client::{ComputerClient, ComputerClientOptions, Credential};
use super::discovery::discover_computers;
use crate::mobile::tailscale::{tailnet_peers, TailnetPeer};
use crate::notify::ThreadNotice;

pub const MAX_COMPUTER_NAME: usize = 128;
const PAIRING_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a computer is unreachable before Tailscale is asked whether it now goes by another name.
const RELOCATE_AFTER: Duration = Duration::from_secs(60);
/// How often Tailscale is asked at most, however many computers are down.
const RELOCATE_EVERY: Duration = Duration::from_secs(5 * 60);

/// A paired computer as kept on disk. `label` is what the user here calls it; `node` is Tailscale's
/// stable id for the machine, which finds it again when its tailnet name moves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredComputer {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub token: String,
    pub paired_at: i64,
}

struct Held {
    stored: StoredComputer,
    /// Which line this is, so a status from a line since replaced is not taken for this one's.
    key: String,
    capabilities: Option<Vec<String>>,
    client: ComputerClient,
    status: ComputerStatus,
    error: Option<String>,
    down_since: Option<tokio::time::Instant>,
}

pub type Connect = Arc<dyn Fn(ComputerClientOptions) -> ComputerClient + Send + Sync>;
pub type Peers = Arc<dyn Fn() -> BoxFuture<'static, Result<Vec<TailnetPeer>, String>> + Send + Sync>;

pub struct ComputerLinksOptions {
    pub file: PathBuf,
    /// What this computer calls itself until the user chooses a name.
    pub device_name: String,
    pub on_changed: Arc<dyn Fn(Vec<ComputerLink>) + Send + Sync>,
    pub on_state: Arc<dyn Fn(String, Arc<WorkspaceState>) + Send + Sync>,
    pub on_notice: Arc<dyn Fn(String, ThreadNotice) + Send + Sync>,
    pub connect: Option<Connect>,
    pub peers: Option<Peers>,
}

#[derive(Default)]
struct LinksState {
    held: Vec<Held>,
    own_name: Option<String>,
    looked_at: Option<tokio::time::Instant>,
    looking: bool,
    relocating: Option<tokio::task::JoinHandle<()>>,
}

pub struct ComputerLinks {
    options: ComputerLinksOptions,
    state: Mutex<LinksState>,
    me: Weak<ComputerLinks>,
}

fn read_stored(file: &PathBuf) -> (Option<String>, Vec<StoredComputer>) {
    let parsed = std::fs::read_to_string(file).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok()).unwrap_or(Value::Null);
    let name = parsed.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).map(str::to_string);
    let computers = parsed.get("computers").and_then(Value::as_array).map(|list| list.iter().filter_map(|item| serde_json::from_value(item.clone()).ok()).collect()).unwrap_or_default();
    (name, computers)
}

/// What this computer calls itself, as kept in the links file, or `fallback` when none was chosen.
pub fn stored_computer_name(file: &PathBuf, fallback: &str) -> String {
    read_stored(file).0.unwrap_or_else(|| fallback.to_string())
}

/// A name as it is kept: trimmed and cut to length, so an empty one is no name at all.
fn chosen(name: &str) -> Option<String> {
    let trimmed: String = name.trim().chars().take(MAX_COMPUTER_NAME).collect();
    (!trimmed.is_empty()).then_some(trimmed)
}

impl ComputerLinks {
    pub fn new(options: ComputerLinksOptions) -> Arc<Self> {
        let own_name = read_stored(&options.file).0;
        Arc::new_cyclic(|me| Self { options, state: Mutex::new(LinksState { own_name, ..LinksState::default() }), me: me.clone() })
    }

    fn connect(&self) -> Connect {
        self.options.connect.clone().unwrap_or_else(|| Arc::new(ComputerClient::connect))
    }

    fn write(&self) {
        let state = self.state.lock().unwrap();
        let mut stored = serde_json::json!({ "version": 1, "computers": state.held.iter().map(|held| &held.stored).collect::<Vec<_>>() });
        if let Some(name) = &state.own_name {
            stored["name"] = serde_json::json!(name);
        }
        drop(state);
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = self.options.file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let staging = PathBuf::from(format!("{}.tmp", self.options.file.to_string_lossy()));
            crate::mobile::pairing::write_private(&staging, stored.to_string().as_bytes())?;
            std::fs::rename(&staging, &self.options.file)
        })();
        if let Err(error) = result {
            tracing::error!("Could not write the paired computers: {error}");
        }
    }

    pub fn name(&self) -> String {
        self.state.lock().unwrap().own_name.clone().unwrap_or_else(|| self.options.device_name.clone())
    }

    pub fn links(&self) -> Vec<ComputerLink> {
        self.state
            .lock()
            .unwrap()
            .held
            .iter()
            .map(|held| ComputerLink { id: held.stored.id.clone(), name: held.stored.label.clone().unwrap_or_else(|| held.stored.name.clone()), host: held.stored.host.clone(), status: held.status, error: held.error.clone(), paired_at: held.stored.paired_at, capabilities: held.capabilities.clone() })
            .collect()
    }

    fn announce(&self) {
        (self.options.on_changed)(self.links());
    }

    /// Opens the computer's line. A status from a line since replaced is not taken for this one's.
    fn hold(&self, stored: StoredComputer) {
        let id = stored.id.clone();
        let key = uuid::Uuid::new_v4().to_string();
        let me = self.me.clone();
        let (status_me, status_id, status_key) = (me.clone(), id.clone(), key.clone());
        let (capabilities_me, capabilities_id) = (me.clone(), id.clone());
        let (state_me, state_id) = (me.clone(), id.clone());
        let (notice_me, notice_id) = (me.clone(), id.clone());
        let (name_me, name_id) = (me, id.clone());
        let client = (self.connect())(ComputerClientOptions {
            host: stored.host.clone(),
            device_name: self.options.device_name.clone(),
            credential: Credential::Token(stored.token.clone()),
            url: None,
            on_status: Arc::new(move |status, error| {
                let Some(links) = status_me.upgrade() else { return };
                let learn = {
                    let mut state = links.state.lock().unwrap();
                    let Some(held) = state.held.iter_mut().find(|held| held.stored.id == status_id && held.key == status_key) else { return };
                    if status == ComputerStatus::Connected {
                        held.down_since = None;
                    } else if held.down_since.is_none() {
                        held.down_since = Some(tokio::time::Instant::now());
                    }
                    if held.status == status && held.error == error {
                        return;
                    }
                    held.status = status;
                    held.error = error;
                    status == ComputerStatus::Connected && held.stored.node.is_none()
                };
                links.announce();
                if learn {
                    tokio::spawn(async move { links.relocate(true).await });
                }
            }),
            on_paired: Arc::new(|_, _, _| {}),
            on_state: Arc::new(move |workspace| {
                if let Some(links) = state_me.upgrade() {
                    (links.options.on_state)(state_id.clone(), workspace);
                }
            }),
            on_capabilities: Arc::new(move |capabilities| {
                let Some(links) = capabilities_me.upgrade() else { return };
                let found = {
                    let mut state = links.state.lock().unwrap();
                    match state.held.iter_mut().find(|held| held.stored.id == capabilities_id) {
                        Some(held) => {
                            held.capabilities = capabilities;
                            true
                        }
                        None => false,
                    }
                };
                if found {
                    links.announce();
                }
            }),
            on_notice: Arc::new(move |notice| {
                if let Some(links) = notice_me.upgrade() {
                    (links.options.on_notice)(notice_id.clone(), notice);
                }
            }),
            on_name: Arc::new(move |announced| {
                let Some(links) = name_me.upgrade() else { return };
                let changed = {
                    let mut state = links.state.lock().unwrap();
                    match state.held.iter_mut().find(|held| held.stored.id == name_id) {
                        Some(held) if held.stored.name != announced => {
                            held.stored.name = announced;
                            true
                        }
                        _ => false,
                    }
                };
                if changed {
                    links.write();
                    links.announce();
                }
            }),
        });
        let mut state = self.state.lock().unwrap();
        state.held.retain(|held| held.stored.id != id);
        state.held.push(Held { stored, key, capabilities: None, client, status: ComputerStatus::Connecting, error: None, down_since: Some(tokio::time::Instant::now()) });
    }

    /// Opens a line to every stored computer and starts looking for ones that moved.
    pub fn start(&self) {
        for computer in read_stored(&self.options.file).1 {
            let present = self.state.lock().unwrap().held.iter().any(|held| held.stored.id == computer.id);
            if !present {
                self.hold(computer);
            }
        }
        let me = self.me.clone();
        let mut state = self.state.lock().unwrap();
        if state.relocating.is_none() {
            state.relocating = Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(RELOCATE_AFTER);
                interval.tick().await;
                loop {
                    interval.tick().await;
                    let Some(links) = me.upgrade() else { return };
                    links.relocate(false).await;
                }
            }));
        }
        drop(state);
        self.announce();
    }

    /// Dials now: every computer, or the one named, whatever its backoff or refusal had it waiting on.
    pub fn reconnect(&self, id: Option<&str>) {
        {
            let mut state = self.state.lock().unwrap();
            for held in state.held.iter().filter(|held| id.is_none_or(|id| held.stored.id == id)) {
                held.client.reconnect();
            }
            state.looked_at = None;
        }
        if let Some(links) = self.me.upgrade() {
            tokio::spawn(async move { links.relocate(false).await });
        }
    }

    /// A machine renamed on the tailnet keeps its node id but answers to a new name; a computer down
    /// long enough is looked up by that id. `learn` only fills in ids for computers that lack one.
    async fn relocate(&self, learn: bool) {
        let lost: Vec<String> = {
            let mut state = self.state.lock().unwrap();
            let lost: Vec<String> = state
                .held
                .iter()
                .filter(|held| if learn { held.stored.node.is_none() } else { held.status != ComputerStatus::Connected && held.down_since.is_some_and(|since| since.elapsed() >= RELOCATE_AFTER) })
                .map(|held| held.stored.id.clone())
                .collect();
            let recently = state.looked_at.is_some_and(|at| at.elapsed() < if learn { Duration::ZERO } else { RELOCATE_EVERY });
            if lost.is_empty() || state.looking || recently {
                return;
            }
            state.looking = true;
            state.looked_at = Some(tokio::time::Instant::now());
            lost
        };
        let found = match &self.options.peers {
            Some(peers) => peers().await,
            None => tailnet_peers().await,
        };
        self.state.lock().unwrap().looking = false;
        let Ok(found) = found else { return };
        let mut moved: Vec<(StoredComputer, String)> = Vec::new();
        let mut change = false;
        {
            let mut state = self.state.lock().unwrap();
            let hosts: Vec<(String, String)> = state.held.iter().map(|held| (held.stored.id.clone(), held.stored.host.clone())).collect();
            for id in lost {
                let Some(held) = state.held.iter_mut().find(|held| held.stored.id == id) else { continue };
                let peer = match &held.stored.node {
                    Some(node) => found.iter().find(|peer| &peer.id == node),
                    None => found.iter().find(|peer| peer.host == held.stored.host),
                };
                let Some(peer) = peer.filter(|peer| !peer.id.is_empty()) else { continue };
                if held.stored.node.is_none() {
                    held.stored.node = Some(peer.id.clone());
                    change = true;
                }
                if peer.host == held.stored.host || hosts.iter().any(|(other, host)| *other != id && *host == peer.host) {
                    continue;
                }
                held.client.stop();
                moved.push((held.stored.clone(), peer.host.clone()));
                change = true;
            }
        }
        for (stored, host) in moved {
            self.hold(StoredComputer { host, ..stored });
        }
        if change {
            self.write();
            self.announce();
        }
    }

    /// What this computer calls itself from now on. Empty goes back to the machine's own name.
    pub fn rename(&self, next: &str) {
        let chosen_name = chosen(next);
        {
            let mut state = self.state.lock().unwrap();
            if chosen_name == state.own_name {
                return;
            }
            state.own_name = chosen_name;
        }
        self.write();
        self.announce();
    }

    /// What the user here calls a paired computer. Empty, or its own name, goes back to what it announces.
    pub fn label(&self, id: &str, next: &str) {
        {
            let mut state = self.state.lock().unwrap();
            let Some(held) = state.held.iter_mut().find(|held| held.stored.id == id) else { return };
            let label = chosen(next).filter(|label| *label != held.stored.name);
            if label == held.stored.label {
                return;
            }
            held.stored.label = label;
        }
        self.write();
        self.announce();
    }

    pub async fn discover(&self) -> Result<Vec<DiscoveredComputer>, String> {
        discover_computers().await
    }

    /// Trades the code for a token over a line of its own, then opens the computer's line with the token.
    pub async fn pair(&self, host: &str, name: &str, code: &str) -> Result<(), String> {
        if self.state.lock().unwrap().held.iter().any(|held| held.stored.host == host) {
            return Err(format!("{name} is already paired."));
        }
        let (device_id, token) = self.trade(host, code).await?;
        self.hold(StoredComputer { id: device_id, name: name.to_string(), label: None, host: host.to_string(), node: None, token, paired_at: chrono::Utc::now().timestamp_millis() });
        self.write();
        self.announce();
        Ok(())
    }

    async fn trade(&self, host: &str, code: &str) -> Result<(String, String), String> {
        let (settle, settled) = tokio::sync::mpsc::unbounded_channel::<Result<(String, String), String>>();
        let failing = settle.clone();
        let client = (self.connect())(ComputerClientOptions {
            host: host.to_string(),
            device_name: self.name(),
            credential: Credential::Code(code.to_string()),
            url: None,
            on_status: Arc::new(move |status, error| {
                if status == ComputerStatus::Offline {
                    let _ = failing.send(Err(error.unwrap_or_else(|| "The other computer could not be reached.".into())));
                }
            }),
            on_paired: Arc::new(move |device_id, _, token| {
                let _ = settle.send(Ok((device_id, token)));
            }),
            on_state: Arc::new(|_| {}),
            on_capabilities: Arc::new(|_| {}),
            on_notice: Arc::new(|_| {}),
            on_name: Arc::new(|_| {}),
        });
        let mut settled = settled;
        let outcome = tokio::time::timeout(PAIRING_TIMEOUT, settled.recv()).await;
        client.stop();
        match outcome {
            Ok(Some(result)) => result,
            Ok(None) => Err("The other computer could not be reached.".into()),
            Err(_) => Err("The other computer did not answer the code in time.".into()),
        }
    }

    pub fn forget(&self, id: &str) {
        let removed = {
            let mut state = self.state.lock().unwrap();
            let Some(index) = state.held.iter().position(|held| held.stored.id == id) else { return };
            state.held.remove(index)
        };
        removed.client.stop();
        self.write();
        self.announce();
    }

    pub async fn send(&self, id: &str, inputs: Vec<Value>) -> Result<Value, String> {
        let client = self.client(id)?;
        client.send(inputs).await
    }

    pub async fn query(&self, id: &str, query: Value) -> Result<Value, String> {
        let client = self.client(id)?;
        client.query(query).await
    }

    fn client(&self, id: &str) -> Result<ComputerClient, String> {
        self.state.lock().unwrap().held.iter().find(|held| held.stored.id == id).map(|held| held.client.clone()).ok_or_else(|| "That computer is no longer paired.".to_string())
    }

    /// Hangs up on every computer; `start` brings them back.
    pub fn stop(&self) {
        let held = {
            let mut state = self.state.lock().unwrap();
            if let Some(relocating) = state.relocating.take() {
                relocating.abort();
            }
            std::mem::take(&mut state.held)
        };
        for held in held {
            held.client.stop();
        }
        self.announce();
    }
}
