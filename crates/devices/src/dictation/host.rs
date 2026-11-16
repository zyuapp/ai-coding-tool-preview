//! Holding to talk: a helper records the microphone and runs Parakeet (dictation-host.ts). On macOS it
//! is the native FluidAudio program reading the model folder FluidAudio shares between apps; elsewhere
//! it is `aic-dictation-onnx`, which runs ONNX Runtime on a model in the user's data folder. Both
//! speak the same JSON lines.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_core::domain::platform::{DictationModel, DICTATION_UNSUPPORTED};
use base64::Engine;
use futures::future::{BoxFuture, FutureExt, Shared};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use super::models::{this_computers_place, DictationModelPlace};

/// The helper holds the model in memory, so it goes after this long without a hold.
const IDLE: Duration = Duration::from_secs(3 * 60);
const START_TIMEOUT: Duration = Duration::from_secs(10);
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(120);
pub const MICROPHONE_DENIED: &str = concat!(act_core::app_name!(), " can't hear you. Allow it in System Settings → Privacy & Security → Microphone.");
const UNREADABLE_RECORDING: &str = "That recording is too long or unreadable.";
/// The longest recording a phone may send: two minutes of 16 kHz, 16-bit mono WAV.
pub const MAX_DICTATION_AUDIO_BYTES: usize = 16_000 * 2 * 120 + 4_096;

/// Where the speech model stands.
#[derive(Clone, Debug, PartialEq)]
pub struct DictationModelStatus {
    pub model: DictationModel,
    pub message: Option<String>,
    pub progress: Option<f64>,
}

impl DictationModelStatus {
    fn of(model: DictationModel) -> Self {
        DictationModelStatus { model, message: None, progress: None }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum HelperMessage {
    Model {
        state: String,
        message: Option<String>,
        progress: Option<f64>,
    },
    Started {
        id: String,
    },
    Cancelled {
        id: String,
    },
    Level {
        id: String,
        value: f64,
    },
    Text {
        id: String,
        text: String,
    },
    Error {
        id: Option<String>,
        message: String,
    },
}

/// The helper to run, by the name a packaged app or a source build gives it. Core ML keeps one
/// compiled model per process name, so a source build's helper runs under a name of its own.
pub fn default_helper(packaged: bool) -> PathBuf {
    if cfg!(target_os = "macos") {
        crate::native::helper(if packaged { "aic-dictation" } else { "aic-dictation-dev" }, None)
    } else {
        crate::native::helper("aic-dictation-onnx", None)
    }
}

pub type MicrophoneAccess = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;
type ModelListener = Arc<dyn Fn(&DictationModelStatus) + Send + Sync>;
type LevelListener = Arc<dyn Fn(f64) + Send + Sync>;

#[derive(Clone, Default)]
pub struct DictationHostOptions {
    /// The helper; this build's own unless named.
    pub helper: Option<PathBuf>,
    /// Asks for the microphone. A host without one can only transcribe recordings it is handed.
    pub microphone: Option<MicrophoneAccess>,
    /// Where the model's files are; this computer's usual place unless named.
    pub models: Option<PathBuf>,
    /// Where this computer keeps the model; `Some(None)` where dictation can't run.
    pub place: Option<Option<DictationModelPlace>>,
}

impl DictationHostOptions {
    /// The desktop app's dictation: this build's helper, the system's microphone prompt, and the
    /// `AIC_DICTATION_MODELS` folder a source build may point at instead of the shared one.
    pub fn desktop() -> Self {
        let packaged = crate::native::packaged();
        let models = if packaged { None } else { std::env::var_os("AIC_DICTATION_MODELS").map(PathBuf::from) };
        DictationHostOptions {
            helper: Some(default_helper(packaged)),
            microphone: Some(Arc::new(|| super::microphone::microphone_access().boxed())),
            models,
            place: None,
        }
    }
}

struct Helper {
    lines: std::sync::mpsc::Sender<String>,
    child: Mutex<Option<std::process::Child>>,
}

struct State {
    helper: Option<Arc<Helper>>,
    recording: Option<String>,
    starting: Option<Shared<BoxFuture<'static, Result<(), String>>>>,
    status: DictationModelStatus,
    waiters: HashMap<String, oneshot::Sender<Result<String, String>>>,
    model_listeners: Vec<(u64, ModelListener)>,
    level_listener: Option<(u64, LevelListener)>,
    next_listener: u64,
    idle: Option<tokio::task::JoinHandle<()>>,
}

struct Inner {
    helper_file: PathBuf,
    microphone: Option<MicrophoneAccess>,
    models: Option<PathBuf>,
    place: Option<DictationModelPlace>,
    runtime: tokio::runtime::Handle,
    state: Mutex<State>,
}

/// One helper per process, started on first use and stopped after three idle minutes. Cheap to clone.
#[derive(Clone)]
pub struct DictationHost {
    inner: Arc<Inner>,
}

/// Keeps a listener until dropped.
pub struct Listening {
    id: u64,
    host: std::sync::Weak<Inner>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        if let Some(inner) = self.host.upgrade() {
            let mut state = inner.state.lock().unwrap();
            state.model_listeners.retain(|(id, _)| *id != self.id);
            if state.level_listener.as_ref().is_some_and(|(id, _)| *id == self.id) {
                state.level_listener = None;
            }
        }
    }
}

/// A phone's recording, checked to be the small WAV it says it is before it touches the disk.
pub fn wav_bytes(audio: &str) -> Result<Vec<u8>, String> {
    let max_base64 = MAX_DICTATION_AUDIO_BYTES.div_ceil(3) * 4;
    let shaped = !audio.is_empty()
        && audio.len() <= max_base64
        && audio.len().is_multiple_of(4)
        && audio.trim_end_matches('=').bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
        && audio.len() - audio.trim_end_matches('=').len() <= 2;
    if !shaped {
        return Err(UNREADABLE_RECORDING.into());
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(audio).map_err(|_| UNREADABLE_RECORDING.to_string())?;
    if bytes.len() > MAX_DICTATION_AUDIO_BYTES || bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(UNREADABLE_RECORDING.into());
    }
    Ok(bytes)
}

impl DictationHost {
    /// Must be called inside a Tokio runtime, or with `act_host::runtime()` running.
    pub fn new(options: DictationHostOptions) -> Self {
        let runtime = tokio::runtime::Handle::try_current().unwrap_or_else(|_| act_host::runtime().handle().clone());
        DictationHost {
            inner: Arc::new(Inner {
                helper_file: options.helper.unwrap_or_else(|| default_helper(crate::native::packaged())),
                microphone: options.microphone,
                models: options.models,
                place: options.place.unwrap_or_else(this_computers_place),
                runtime,
                state: Mutex::new(State {
                    helper: None,
                    recording: None,
                    starting: None,
                    status: DictationModelStatus::of(DictationModel::Unsupported),
                    waiters: HashMap::new(),
                    model_listeners: Vec::new(),
                    level_listener: None,
                    next_listener: 1,
                    idle: None,
                }),
            }),
        }
    }

    /// Where the speech model stands, told at once and again whenever it moves.
    pub fn on_model(&self, listener: impl Fn(&DictationModelStatus) + Send + Sync + 'static) -> Listening {
        let current = self.inner.read_status();
        self.inner.set_status(current);
        let listener: ModelListener = Arc::new(listener);
        let (id, status) = {
            let mut state = self.inner.state.lock().unwrap();
            let id = state.next_listener;
            state.next_listener += 1;
            state.model_listeners.push((id, listener.clone()));
            (id, state.status.clone())
        };
        listener(&status);
        Listening { id, host: Arc::downgrade(&self.inner) }
    }

    /// How loud the hold's microphone is, from 0 to 1, many times a second.
    pub fn on_level(&self, listener: impl Fn(f64) + Send + Sync + 'static) -> Listening {
        let mut state = self.inner.state.lock().unwrap();
        let id = state.next_listener;
        state.next_listener += 1;
        state.level_listener = Some((id, Arc::new(listener)));
        Listening { id, host: Arc::downgrade(&self.inner) }
    }

    pub fn status(&self) -> DictationModelStatus {
        self.inner.state.lock().unwrap().status.clone()
    }

    /// Starts the microphone for the hold `id`, resolving once it hears.
    pub async fn start(&self, id: &str) -> Result<(), String> {
        let inner = &self.inner;
        inner.usable()?;
        let Some(microphone) = inner.microphone.clone() else { return Err("Holding to talk needs the desktop app.".into()) };
        let started = {
            let mut state = inner.state.lock().unwrap();
            if state.recording.is_some() {
                return Err("Dictation is already listening.".into());
            }
            state.recording = Some(id.to_string());
            let host = inner.clone();
            let hold = id.to_string();
            let started = async move {
                if !microphone().await {
                    return Err(MICROPHONE_DENIED.to_string());
                }
                host.request("start", &hold, START_TIMEOUT, None).await.map(|_| ())
            }
            .boxed()
            .shared();
            state.starting = Some(started.clone());
            started
        };
        let outcome = started.clone().await;
        let mut state = inner.state.lock().unwrap();
        if state.starting.as_ref().is_some_and(|current| current.ptr_eq(&started)) {
            state.starting = None;
        }
        if outcome.is_err() {
            if state.recording.as_deref() == Some(id) {
                state.recording = None;
            }
            // A start still queued behind other work would otherwise begin recording after the hold gave up.
            if let Some(helper) = &state.helper {
                let _ = helper.lines.send(json!({ "op": "cancel", "id": id }).to_string());
            }
        }
        outcome
    }

    /// Stops the hold's microphone and answers with the words it heard.
    pub async fn stop(&self, id: &str) -> Result<String, String> {
        let inner = &self.inner;
        if !inner.holding(id).await {
            return Err("Nothing is listening.".into());
        }
        inner.state.lock().unwrap().recording = None;
        inner.level(0.0);
        inner.request("stop", id, TRANSCRIBE_TIMEOUT, None).await
    }

    pub async fn cancel(&self, id: &str) {
        let inner = &self.inner;
        if !inner.holding(id).await {
            return;
        }
        inner.state.lock().unwrap().recording = None;
        inner.level(0.0);
        let _ = inner.request("cancel", id, START_TIMEOUT, None).await;
    }

    pub async fn download(&self) -> Result<(), String> {
        let inner = &self.inner;
        if inner.read_status().model == DictationModel::Unsupported {
            return Err(DICTATION_UNSUPPORTED.into());
        }
        inner.set_status(DictationModelStatus { model: DictationModel::Downloading, message: None, progress: Some(0.0) });
        inner.keep_alive();
        inner.send(json!({ "op": "download" }));
        Ok(())
    }

    /// Stopping the helper is the only way to stop the download. Only an incomplete folder is
    /// removed, so a model another app finished meanwhile is kept.
    pub async fn cancel_download(&self) -> Result<(), String> {
        let inner = &self.inner;
        let helper = {
            let mut state = inner.state.lock().unwrap();
            if state.status.model != DictationModel::Downloading {
                return Ok(());
            }
            state.helper.take()
        };
        if let Some(helper) = helper {
            helper.kill();
        }
        let complete = inner.model_complete();
        if !complete {
            match tokio::fs::remove_dir_all(inner.model_folder()).await {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.to_string()),
                _ => {}
            }
        }
        inner.set_status(DictationModelStatus::of(if complete { DictationModel::Ready } else { DictationModel::Missing }));
        Ok(())
    }

    /// Turns a phone's recording, base64 WAV, into words.
    pub async fn transcribe_audio(&self, audio: &str) -> Result<String, String> {
        let inner = &self.inner;
        inner.usable()?;
        let bytes = wav_bytes(audio)?;
        let folder = std::env::temp_dir().join(format!("aic-dictation-{}", uuid::Uuid::new_v4().simple()));
        tokio::fs::create_dir_all(&folder).await.map_err(|error| error.to_string())?;
        let file = folder.join("speech.wav");
        let outcome = async {
            write_private(&file, &bytes).await.map_err(|error| error.to_string())?;
            inner.request("transcribe", &uuid::Uuid::new_v4().to_string(), TRANSCRIBE_TIMEOUT, Some(&file)).await
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&folder).await;
        outcome
    }

    /// Stops the helper, if one was ever started.
    pub fn stop_helper(&self) {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(idle) = state.idle.take() {
            idle.abort();
        }
        if let Some(helper) = state.helper.take() {
            helper.kill();
        }
    }
}

async fn write_private(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut handle = options.open(file).await?;
    handle.write_all(bytes).await?;
    handle.flush().await
}

impl Helper {
    fn kill(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Inner {
    fn model_folder(&self) -> PathBuf {
        self.models.clone().or_else(|| self.place.as_ref().map(|place| place.folder.clone())).unwrap_or_default()
    }

    fn model_complete(&self) -> bool {
        self.place.as_ref().is_some_and(|place| place.complete_in(&self.model_folder()))
    }

    /// What the disk says, without starting the helper.
    fn read_status(&self) -> DictationModelStatus {
        if self.place.is_none() {
            return DictationModelStatus { model: DictationModel::Unsupported, message: Some(DICTATION_UNSUPPORTED.into()), progress: None };
        }
        if !self.helper_file.is_file() {
            return DictationModelStatus { model: DictationModel::Unsupported, message: Some("This build of the app has no dictation helper.".into()), progress: None };
        }
        let current = self.state.lock().unwrap().status.clone();
        if current.model == DictationModel::Downloading {
            return current;
        }
        DictationModelStatus::of(if self.model_complete() { DictationModel::Ready } else { DictationModel::Missing })
    }

    fn set_status(&self, next: DictationModelStatus) {
        let listeners = {
            let mut state = self.state.lock().unwrap();
            if state.status == next {
                return;
            }
            state.status = next.clone();
            state.model_listeners.iter().map(|(_, listener)| listener.clone()).collect::<Vec<_>>()
        };
        for listener in listeners {
            listener(&next);
        }
    }

    fn usable(&self) -> Result<(), String> {
        let current = self.read_status();
        self.set_status(current.clone());
        if current.model != DictationModel::Ready {
            return Err(current.message.unwrap_or_else(|| "The speech model is not ready.".into()));
        }
        Ok(())
    }

    fn level(&self, value: f64) {
        let listener = self.state.lock().unwrap().level_listener.clone();
        if let Some((_, listener)) = listener {
            listener(value);
        }
    }

    /// Whether `id` is the hold listening, once a start still on its way has settled.
    async fn holding(&self, id: &str) -> bool {
        let starting = {
            let state = self.state.lock().unwrap();
            if state.recording.as_deref() != Some(id) {
                return false;
            }
            state.starting.clone()
        };
        if let Some(starting) = starting {
            let _ = starting.await;
        }
        self.state.lock().unwrap().recording.as_deref() == Some(id)
    }

    fn settle(&self, id: &str, outcome: Result<String, String>) {
        let waiter = self.state.lock().unwrap().waiters.remove(id);
        if let Some(waiter) = waiter {
            let _ = waiter.send(outcome);
        }
    }

    fn heard(&self, message: HelperMessage) {
        match message {
            HelperMessage::Model { state, message, progress } => {
                let status = match state.as_str() {
                    "failed" => DictationModelStatus {
                        model: DictationModel::Failed,
                        message: Some(message.unwrap_or_else(|| "The speech model could not be downloaded.".into())),
                        progress: None,
                    },
                    "downloading" => DictationModelStatus { model: DictationModel::Downloading, message: None, progress: progress.map(|value| value.clamp(0.0, 1.0)) },
                    "ready" => DictationModelStatus::of(DictationModel::Ready),
                    "missing" => DictationModelStatus::of(DictationModel::Missing),
                    _ => return,
                };
                self.set_status(status);
            }
            HelperMessage::Level { id, value } => {
                if self.state.lock().unwrap().recording.as_deref() == Some(id.as_str()) {
                    self.level(value);
                }
            }
            HelperMessage::Started { id } | HelperMessage::Cancelled { id } => self.settle(&id, Ok(String::new())),
            HelperMessage::Text { id, text } => self.settle(&id, Ok(text)),
            HelperMessage::Error { id: Some(id), message } => self.settle(&id, Err(message)),
            HelperMessage::Error { id: None, message } => eprintln!("Dictation helper: {message}"),
        }
    }

    fn gone(self: &Arc<Self>, helper: &Arc<Helper>, reason: &str) {
        let waiters = {
            let mut state = self.state.lock().unwrap();
            if !state.helper.as_ref().is_some_and(|current| Arc::ptr_eq(current, helper)) {
                return;
            }
            state.helper = None;
            state.recording = None;
            state.waiters.keys().cloned().collect::<Vec<_>>()
        };
        if self.state.lock().unwrap().status.model == DictationModel::Downloading {
            self.set_status(DictationModelStatus { model: DictationModel::Failed, message: Some(reason.into()), progress: None });
        }
        for id in waiters {
            self.settle(&id, Err(reason.into()));
        }
    }

    /// The helper reads its requests with blocking reads, so it gets ordinary pipes and this side
    /// talks to them from threads of its own.
    fn ensure_helper(self: &Arc<Self>) -> Arc<Helper> {
        let mut state = self.state.lock().unwrap();
        if let Some(helper) = &state.helper {
            return helper.clone();
        }
        let (lines, outgoing) = std::sync::mpsc::channel::<String>();
        let mut command = std::process::Command::new(&self.helper_file);
        if let Some(models) = &self.models {
            command.arg("--models").arg(models);
        }
        command.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
        let spawned = command.spawn();
        let helper = Arc::new(Helper { lines, child: Mutex::new(None) });
        state.helper = Some(helper.clone());
        drop(state);
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                self.gone(&helper, &format!("The dictation helper could not run: {error}"));
                return helper;
            }
        };
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        *helper.child.lock().unwrap() = Some(child);
        std::thread::spawn(move || {
            use std::io::Write;
            for line in outgoing {
                if writeln!(stdin, "{line}").and_then(|()| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        let inner = self.clone();
        let reading = helper.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<HelperMessage>(&line) {
                    Ok(message) => inner.heard(message),
                    Err(_) => eprintln!("Dictation helper said something unreadable."),
                }
            }
            let child = reading.child.lock().unwrap().take();
            if let Some(mut child) = child {
                let _ = child.wait();
            }
            inner.gone(&reading, "The dictation helper stopped.");
        });
        helper
    }

    fn send(self: &Arc<Self>, request: Value) {
        let helper = self.ensure_helper();
        let _ = helper.lines.send(request.to_string());
    }

    fn keep_alive(self: &Arc<Self>) {
        let inner = self.clone();
        let timer = self.runtime.spawn(async move {
            tokio::time::sleep(IDLE).await;
            let busy = {
                let state = inner.state.lock().unwrap();
                state.recording.is_some() || !state.waiters.is_empty() || state.status.model == DictationModel::Downloading
            };
            if busy {
                inner.keep_alive();
                return;
            }
            let helper = {
                let mut state = inner.state.lock().unwrap();
                state.idle = None;
                state.helper.take()
            };
            if let Some(helper) = helper {
                helper.kill();
            }
        });
        if let Some(previous) = self.state.lock().unwrap().idle.replace(timer) {
            previous.abort();
        }
    }

    async fn request(self: &Arc<Self>, op: &str, id: &str, timeout: Duration, path: Option<&Path>) -> Result<String, String> {
        self.keep_alive();
        let (sender, receiver) = oneshot::channel();
        self.state.lock().unwrap().waiters.insert(id.to_string(), sender);
        let mut request = json!({ "op": op, "id": id });
        if let Some(path) = path {
            request["path"] = json!(path.to_string_lossy());
        }
        self.send(request);
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err("The dictation helper stopped.".into()),
            Err(_) => {
                self.state.lock().unwrap().waiters.remove(id);
                Err("Dictation took too long.".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_small_wav_is_taken_from_a_phone() {
        let wav = base64::engine::general_purpose::STANDARD.encode(b"RIFF\0\0\0\0WAVEfmt ");
        assert!(wav_bytes(&wav).is_ok());
        let text = base64::engine::general_purpose::STANDARD.encode(b"hello there, not audio");
        assert_eq!(wav_bytes(&text), Err(UNREADABLE_RECORDING.into()));
        assert!(wav_bytes("not base64!").is_err());
    }
}
