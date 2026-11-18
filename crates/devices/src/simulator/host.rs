//! Drives headless iOS simulators through one native helper: lists and boots devices, streams their
//! screens, sends input, reads the accessibility tree, and takes screenshots (simulator-host.ts).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_core::domain::platform::{untilted, SimulatorAvailability, SimulatorButton, SimulatorDevice, SimulatorDeviceState, SimulatorInput, SimulatorOrientation, TouchPhase, PasteboardPhase};
use act_platform::process::{Exec, RunError};
use act_host::tools::types::{SimulatorElement, SimulatorShot, SimulatorSnapshot};
use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use act_platform::process::Detach;
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, watch};

use super::devices::{listed, parse_device_list, pasted_text, RawElement};
use super::frames::{FrameReader, HelperMessage, SimulatorFrame};

const REQUEST_TIMEOUT: Duration = Duration::from_millis(15_000);
const BOOT_TIMEOUT: Duration = Duration::from_millis(240_000);
const MAX_SHOTS: usize = 20;
const COMMAND_KEY: i64 = 227;
pub const KEY_C: i64 = 6;
pub const KEY_V: i64 = 25;
pub const KEY_X: i64 = 27;
/// How long the device takes to handle a paste; a key that reaches it sooner loses the paste or meets a paste prompt.
const PASTE_SETTLE: Duration = Duration::from_millis(300);
/// How long a copy on the device may take to reach its pasteboard.
const COPY_WAIT: Duration = Duration::from_millis(1_500);
const COPY_POLL: Duration = Duration::from_millis(100);
const LISTING_BUFFER: usize = 16 * 1024 * 1024;

pub type SimulatorResult<T> = Result<T, String>;
pub type FrameListener = Arc<dyn Fn(&SimulatorFrame) + Send + Sync>;
/// Puts text on the Mac's clipboard.
pub type ClipboardWriter = Arc<dyn Fn(String) + Send + Sync>;

/// Where a touch is, with a second finger when it is a pinch.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fingers {
    x: f64,
    y: f64,
    second: Option<(f64, f64)>,
}

impl Fingers {
    fn write(&self, body: &mut Map<String, Value>) {
        body.insert("x".into(), json!(self.x));
        body.insert("y".into(), json!(self.y));
        if let Some((x2, y2)) = self.second {
            body.insert("x2".into(), json!(x2));
            body.insert("y2".into(), json!(y2));
        }
    }
}

#[derive(Default)]
struct Moves {
    queued: bool,
    next: Option<Fingers>,
}

struct Snapshot {
    id: u64,
    width: f64,
    height: f64,
    elements: Vec<SimulatorElement>,
}

type Turn = Shared<BoxFuture<'static, ()>>;
type Answer = oneshot::Sender<SimulatorResult<Map<String, Value>>>;

fn input_queue(state: &mut State) -> &mut HashMap<String, Turn> {
    &mut state.input_work
}

fn pasteboard_queue(state: &mut State) -> &mut HashMap<String, Turn> {
    &mut state.pasteboard_work
}

#[derive(Default)]
struct State {
    unavailable: Option<String>,
    shot_home: Option<PathBuf>,
    shots: VecDeque<PathBuf>,
    watchers: HashMap<String, usize>,
    snapshots: HashMap<String, Snapshot>,
    moves: HashMap<String, Arc<Mutex<Moves>>>,
    /// Each device's screen in points, learned from its accessibility root.
    screens: HashMap<String, (f64, f64)>,
    /// How each device is held, as the last rotation sent to it left it.
    orientations: HashMap<String, SimulatorOrientation>,
    /// Devices this app started, which it also shuts down once nothing shows them.
    started_here: HashSet<String>,
    /// Devices whose input is already kept off the Mac's default microphone.
    quieted: HashMap<String, Shared<BoxFuture<'static, ()>>>,
    /// What each device's pasteboard held when this app last read or set it.
    pasteboards: HashMap<String, String>,
    input_work: HashMap<String, Turn>,
    pasteboard_work: HashMap<String, Turn>,
}

struct Helper {
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Mutex<HashMap<u64, Answer>>,
    ready: watch::Receiver<Option<SimulatorResult<()>>>,
    child: Mutex<Option<Child>>,
}

impl Helper {
    fn fail_all(&self, error: &str) {
        for (_, sender) in self.pending.lock().unwrap().drain() {
            let _ = sender.send(Err(error.to_string()));
        }
    }
}

/// Settings a host is made with.
#[derive(Clone, Default)]
pub struct SimulatorHostOptions {
    /// The helper binary; found beside the app or where this build put it unless named.
    pub helper: Option<PathBuf>,
    /// What brings text copied on a device to the Mac. `pbcopy` unless given.
    pub clipboard: Option<ClipboardWriter>,
}

struct Inner {
    helper_path: PathBuf,
    clipboard: ClipboardWriter,
    runtime: tokio::runtime::Handle,
    state: Mutex<State>,
    helper: Mutex<Option<Arc<Helper>>>,
    starting: tokio::sync::Mutex<()>,
    developer_dir: tokio::sync::OnceCell<Option<String>>,
    next_request: AtomicU64,
    next_snapshot: AtomicU64,
    frame_listener: Mutex<Option<FrameListener>>,
    subscribers: Mutex<Vec<Subscriber>>,
    next_subscriber: AtomicU64,
}

/// One helper serves every device. Cheap to clone; clones share it.
#[derive(Clone)]
pub struct SimulatorHost {
    inner: Arc<Inner>,
}

struct Subscriber {
    id: u64,
    udid: String,
    sender: std::sync::mpsc::SyncSender<SimulatorFrame>,
    /// A frame was dropped, so deltas are skipped until the next keyframe.
    lagging: bool,
}

/// How many frames a reader may fall behind before frames are skipped to the next keyframe.
const FRAME_BACKLOG: usize = 8;

/// Keeps one device's frames coming until dropped, which also ends the receiver.
pub struct FrameSubscription {
    id: u64,
    host: std::sync::Weak<Inner>,
}

impl Drop for FrameSubscription {
    fn drop(&mut self) {
        if let Some(inner) = self.host.upgrade() {
            inner.subscribers.lock().unwrap().retain(|subscriber| subscriber.id != self.id);
        }
    }
}

pub fn default_helper_path() -> PathBuf {
    crate::native::helper("aic-simulator", option_env!("ACT_SIMULATOR_HELPER_BUILT"))
}

fn pbcopy() -> ClipboardWriter {
    Arc::new(|text: String| {
        act_host::runtime().spawn(async move {
            let _ = act_platform::tools::copy_text(&text, REQUEST_TIMEOUT).await;
        });
    })
}

/// A failed command worded as Node words it, so what a person reads is the tool's own complaint.
pub(crate) fn run_failure(command: &str, args: &[&str], error: &RunError) -> String {
    let line = std::iter::once(command).chain(args.iter().copied()).collect::<Vec<_>>().join(" ");
    match error {
        RunError::Exit { stderr, .. } => format!("Command failed: {line}\n{}", String::from_utf8_lossy(stderr)).trim_end().to_string(),
        other => format!("Command failed: {line}: {other}"),
    }
}

fn ratio(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

impl SimulatorHost {
    /// Must be called inside a Tokio runtime, or with `act_host::runtime()` running.
    pub fn new(options: SimulatorHostOptions) -> Self {
        let runtime = tokio::runtime::Handle::try_current().unwrap_or_else(|_| act_host::runtime().handle().clone());
        Self {
            inner: Arc::new(Inner {
                helper_path: options.helper.unwrap_or_else(default_helper_path),
                clipboard: options.clipboard.unwrap_or_else(pbcopy),
                runtime,
                state: Mutex::default(),
                helper: Mutex::new(None),
                starting: tokio::sync::Mutex::new(()),
                developer_dir: tokio::sync::OnceCell::new(),
                next_request: AtomicU64::new(1),
                next_snapshot: AtomicU64::new(1),
                frame_listener: Mutex::new(None),
                subscribers: Mutex::new(Vec::new()),
                next_subscriber: AtomicU64::new(1),
            }),
        }
    }

    pub fn helper_path(&self) -> &Path {
        &self.inner.helper_path
    }

    /// Hands every frame the helper streams to `listener` (startSimulatorHost).
    pub fn set_frame_listener(&self, listener: Option<FrameListener>) {
        *self.inner.frame_listener.lock().unwrap() = listener;
    }

    /// Frames for one device. A reader that falls behind misses frames until the next keyframe.
    pub fn subscribe_frames(&self, udid: &str) -> (FrameSubscription, std::sync::mpsc::Receiver<SimulatorFrame>) {
        let (sender, frames) = std::sync::mpsc::sync_channel(FRAME_BACKLOG);
        let id = self.inner.next_subscriber.fetch_add(1, Ordering::Relaxed);
        self.inner.subscribers.lock().unwrap().push(Subscriber { id, udid: udid.to_string(), sender, lagging: false });
        (FrameSubscription { id, host: Arc::downgrade(&self.inner) }, frames)
    }

    pub async fn availability(&self) -> SimulatorAvailability {
        self.inner.availability().await
    }

    pub async fn list(&self) -> SimulatorResult<(SimulatorAvailability, Vec<SimulatorDevice>)> {
        let availability = self.inner.availability().await;
        if availability != SimulatorAvailability::Available {
            return Ok((availability, Vec::new()));
        }
        let args = ["simctl", "list", "devices", "available", "-j"];
        let output = Exec::new("xcrun").args(args).max_buffer(LISTING_BUFFER).run().await.map_err(|error| run_failure("xcrun", &args, &error))?;
        let devices = parse_device_list(&output.stdout_text()).map_err(|error| error.to_string())?;
        // A device that went down boots again held upright.
        let mut state = self.inner.state.lock().unwrap();
        for device in devices.iter().filter(|device| device.state != SimulatorDeviceState::Booted) {
            state.orientations.remove(&device.udid);
            state.quieted.remove(&device.udid);
            state.pasteboards.remove(&device.udid);
        }
        Ok((availability, devices))
    }

    /// Boots a device that is down and waits until it is up; one already up answers at once.
    pub async fn boot(&self, udid: &str) -> SimulatorResult<()> {
        self.inner.state.lock().unwrap().started_here.insert(udid.to_string());
        let args = ["simctl", "bootstatus", udid, "-b"];
        Exec::new("xcrun").args(args).timeout(BOOT_TIMEOUT).run().await.map_err(|error| run_failure("xcrun", &args, &error))?;
        self.inner.quiet(udid).await;
        Ok(())
    }

    /// A device this app started goes down; one started elsewhere is left running.
    pub async fn shutdown(&self, udid: &str) {
        {
            let mut state = self.inner.state.lock().unwrap();
            if !state.started_here.remove(udid) {
                return;
            }
            state.quieted.remove(udid);
            state.orientations.remove(udid);
            state.pasteboards.remove(udid);
        }
        let _ = Exec::new("xcrun").args(["simctl", "shutdown", udid]).run().await;
    }

    pub fn input(&self, udid: &str, input: SimulatorInput) -> BoxFuture<'static, SimulatorResult<()>> {
        Inner::input(&self.inner, udid, input)
    }

    /// The app on screen as its accessibility tree, each element given a ref only this read's taps accept.
    pub async fn describe(&self, udid: &str, device: &str) -> SimulatorResult<SimulatorSnapshot> {
        let inner = &self.inner;
        inner.upright(udid).await;
        let result = inner.request(json!({ "op": "describe", "udid": udid }), REQUEST_TIMEOUT).await?;
        let raw: Vec<RawElement> = result.get("elements").and_then(|value| serde_json::from_value(value.clone()).ok()).unwrap_or_default();
        let (width, height) = raw.first().map(|root| (root.frame_at(2), root.frame_at(3))).unwrap_or((0.0, 0.0));
        let id = inner.next_snapshot.fetch_add(1, Ordering::Relaxed);
        let elements: Vec<SimulatorElement> = raw
            .iter()
            .skip(1)
            .filter(|element| listed(element, width, height))
            .enumerate()
            .map(|(index, element)| SimulatorElement {
                reference: format!("{id}.{}", index + 1),
                role: element.role.as_deref().unwrap_or("AXUnknown").trim_start_matches("AX").to_string(),
                label: element.label.clone().filter(|text| !text.is_empty()),
                value: element.value.clone().filter(|text| !text.is_empty()),
                identifier: element.identifier.clone().filter(|text| !text.is_empty()),
                frame: [element.frame_at(0).round(), element.frame_at(1).round(), element.frame_at(2).round(), element.frame_at(3).round()],
                depth: (element.depth.unwrap_or(1.0) - 1.0).max(0.0) as usize,
            })
            .collect();
        {
            let mut state = inner.state.lock().unwrap();
            state.snapshots.insert(udid.to_string(), Snapshot { id, width, height, elements: elements.clone() });
            if width > 0.0 && height > 0.0 {
                state.screens.insert(udid.to_string(), (width, height));
            }
        }
        Ok(SimulatorSnapshot {
            udid: udid.to_string(),
            device: device.to_string(),
            application: result.get("application").and_then(Value::as_str).map(str::to_string),
            width: width.round(),
            height: height.round(),
            elements,
        })
    }

    /// A PNG of the screen in a folder of this host's own, which keeps the latest twenty.
    pub async fn capture(&self, udid: &str) -> SimulatorResult<SimulatorShot> {
        let inner = &self.inner;
        inner.upright(udid).await;
        let home = inner.shot_home()?;
        let file = home.join(format!("{}.png", uuid::Uuid::new_v4()));
        let result = inner.request(json!({ "op": "screenshot", "udid": udid, "path": file.to_string_lossy() }), REQUEST_TIMEOUT).await?;
        let stale: Vec<PathBuf> = {
            let mut state = inner.state.lock().unwrap();
            state.shots.push_back(file.clone());
            let mut stale = Vec::new();
            while state.shots.len() > MAX_SHOTS {
                stale.extend(state.shots.pop_front());
            }
            stale
        };
        for path in stale {
            let _ = tokio::fs::remove_file(path).await;
        }
        let number = |key: &str| result.get(key).and_then(Value::as_f64).unwrap_or(0.0) as u32;
        Ok(SimulatorShot { path: file.to_string_lossy().into_owned(), width: number("width"), height: number("height") })
    }

    /// Streams a device's screen while at least one view watches it.
    pub async fn watch(&self, udid: &str) -> SimulatorResult<()> {
        let inner = &self.inner;
        *inner.state.lock().unwrap().watchers.entry(udid.to_string()).or_default() += 1;
        let result = async {
            inner.upright(udid).await;
            let quiet = inner.quiet(udid);
            inner.runtime.spawn(quiet);
            inner.request(json!({ "op": "watch", "udid": udid }), REQUEST_TIMEOUT).await.map(|_| ())
        }
        .await;
        if result.is_err() {
            self.unwatch(udid).await;
        }
        result
    }

    pub async fn unwatch(&self, udid: &str) {
        let inner = &self.inner;
        {
            let mut state = inner.state.lock().unwrap();
            let count = state.watchers.get(udid).copied().unwrap_or(0).saturating_sub(1);
            if count > 0 {
                state.watchers.insert(udid.to_string(), count);
            } else {
                state.watchers.remove(udid);
            }
        }
        if inner.helper.lock().unwrap().is_some() {
            let _ = inner.request(json!({ "op": "unwatch", "udid": udid }), REQUEST_TIMEOUT).await;
        }
    }

    /// How the host last turned a device, for one it has turned.
    pub fn orientation(&self, udid: &str) -> Option<SimulatorOrientation> {
        self.inner.state.lock().unwrap().orientations.get(udid).copied()
    }

    /// Forgets every device's turn and read, stops the helper, and removes the screenshots (stopSimulatorHost).
    pub fn stop(&self) {
        let inner = &self.inner;
        *inner.frame_listener.lock().unwrap() = None;
        let home = {
            let mut state = inner.state.lock().unwrap();
            state.watchers.clear();
            // A new window starts with every device upright in its record, so the devices are turned to match when next used.
            state.orientations.clear();
            state.quieted.clear();
            state.screens.clear();
            state.snapshots.clear();
            state.shots.clear();
            state.shot_home.take()
        };
        if let Some(helper) = inner.helper.lock().unwrap().take() {
            if let Some(mut child) = helper.child.lock().unwrap().take() {
                let _ = child.start_kill();
            }
        }
        if let Some(home) = home {
            let _ = std::fs::remove_dir_all(home);
        }
    }

    /// Devices this app started go down with it, finishing after the app has gone.
    pub fn shutdown_started(&self) {
        let started: Vec<String> = self.inner.state.lock().unwrap().started_here.drain().collect();
        for udid in started {
            let mut command = act_platform::process::std_command("xcrun");
            command.args(["simctl", "shutdown", &udid]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).own_group();
            let _ = command.spawn();
        }
    }

    /// What closing the window or quitting does: the helper stops and the devices this app started go down.
    pub fn stop_with_window(&self) {
        self.stop();
        self.shutdown_started();
    }
}

impl Inner {
    /// The Xcode the command line tools point at. Command line tools alone carry no simulators.
    async fn xcode(&self) -> Option<String> {
        self.developer_dir
            .get_or_init(|| async {
                let output = Exec::new("xcode-select").args(["-p"]).run().await.ok()?;
                let dir = output.stdout_text().trim().to_string();
                (!dir.is_empty() && Path::new(&dir).join("Library/PrivateFrameworks/SimulatorKit.framework").exists()).then_some(dir)
            })
            .await
            .clone()
    }

    async fn availability(&self) -> SimulatorAvailability {
        if !cfg!(target_os = "macos") {
            return SimulatorAvailability::Unsupported { reason: "The simulator is only on macOS.".into() };
        }
        if let Some(reason) = self.state.lock().unwrap().unavailable.clone() {
            return SimulatorAvailability::Unsupported { reason };
        }
        if !self.helper_path.is_file() {
            return SimulatorAvailability::Unsupported { reason: "This build of the app has no simulator helper.".into() };
        }
        if self.xcode().await.is_none() {
            return SimulatorAvailability::Unsupported { reason: "Install Xcode to use the simulator.".into() };
        }
        SimulatorAvailability::Available
    }

    fn shot_home(&self) -> SimulatorResult<PathBuf> {
        let mut state = self.state.lock().unwrap();
        if let Some(home) = &state.shot_home {
            return Ok(home.clone());
        }
        let home = std::env::temp_dir().join(format!("aicodingtool-simulator-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
        state.shot_home = Some(home.clone());
        Ok(home)
    }

    /// Keeps a running device off the headset in use, once per boot. Sound still works if this fails.
    fn quiet(self: &Arc<Self>, udid: &str) -> Shared<BoxFuture<'static, ()>> {
        let mut state = self.state.lock().unwrap();
        if let Some(pending) = state.quieted.get(udid) {
            return pending.clone();
        }
        let inner = self.clone();
        let device = udid.to_string();
        let pending = async move {
            if let Err(error) = inner.request(json!({ "op": "quiet-audio", "udid": device }), REQUEST_TIMEOUT).await {
                inner.state.lock().unwrap().quieted.remove(&device);
                eprintln!("[simulator] audio input left on the default device: {error}");
            }
        }
        .boxed()
        .shared();
        state.quieted.insert(udid.to_string(), pending.clone());
        pending
    }

    fn deliver(self: &Arc<Self>, helper: &Arc<Helper>, ready: &watch::Sender<Option<SimulatorResult<()>>>, message: HelperMessage) {
        let json = match message {
            HelperMessage::Frame(frame) => {
                if let Some(listener) = self.frame_listener.lock().unwrap().clone() {
                    listener(&frame);
                }
                let starts_over = matches!(frame, SimulatorFrame::Config { .. } | SimulatorFrame::Chunk { key: true, .. });
                let mut subscribers = self.subscribers.lock().unwrap();
                subscribers.retain_mut(|subscriber| {
                    if subscriber.udid != frame.udid() || (subscriber.lagging && !starts_over) {
                        return true;
                    }
                    match subscriber.sender.try_send(frame.clone()) {
                        Ok(()) => {
                            subscriber.lagging = false;
                            true
                        }
                        Err(std::sync::mpsc::TrySendError::Full(_)) => {
                            subscriber.lagging = true;
                            true
                        }
                        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => false,
                    }
                });
                return;
            }
            HelperMessage::Json(json) => json,
        };
        match json.get("event").and_then(Value::as_str) {
            Some("ready") => {
                let _ = ready.send(Some(Ok(())));
                return;
            }
            Some("unavailable") => {
                let reason = json.get("reason").and_then(Value::as_str).unwrap_or("The simulator is unavailable.").to_string();
                self.state.lock().unwrap().unavailable = Some(reason.clone());
                let _ = ready.send(Some(Err(reason)));
                return;
            }
            _ => {}
        }
        let Some(id) = json.get("id").and_then(Value::as_u64) else { return };
        let Some(sender) = helper.pending.lock().unwrap().remove(&id) else { return };
        let outcome = if json.get("ok") == Some(&Value::Bool(true)) {
            Ok(json.get("result").and_then(Value::as_object).cloned().unwrap_or_default())
        } else {
            Err(json.get("error").and_then(Value::as_str).unwrap_or("The simulator refused that.").to_string())
        };
        let _ = sender.send(outcome);
    }

    /// One helper serves every device. A crash fails what it held, and the next request starts another.
    async fn started(self: &Arc<Self>) -> SimulatorResult<Arc<Helper>> {
        let _gate = self.starting.lock().await;
        let current = self.helper.lock().unwrap().clone();
        let helper = match current {
            Some(helper) => helper,
            None => self.spawn_helper().await?,
        };
        let mut ready = helper.ready.clone();
        drop(_gate);
        let outcome = ready.wait_for(Option::is_some).await.map(|value| value.clone()).ok().flatten();
        match outcome {
            Some(Ok(())) => Ok(helper),
            Some(Err(reason)) => Err(reason),
            None => Err("The simulator helper stopped.".into()),
        }
    }

    async fn spawn_helper(self: &Arc<Self>) -> SimulatorResult<Arc<Helper>> {
        let dir = self.xcode().await.ok_or_else(|| "Install Xcode to use the simulator.".to_string())?;
        let mut child = act_platform::process::command(&self.helper_path)
            .arg(&dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (ready_sender, ready) = watch::channel(None);
        let helper = Arc::new(Helper { stdin: tokio::sync::Mutex::new(stdin), pending: Mutex::new(HashMap::new()), ready, child: Mutex::new(Some(child)) });
        *self.helper.lock().unwrap() = Some(helper.clone());

        let inner = self.clone();
        let reading = helper.clone();
        self.runtime.spawn(async move {
            let mut reader = FrameReader::new();
            let mut chunk = vec![0u8; 256 * 1024];
            loop {
                let read = match stdout.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let mut messages = Vec::new();
                if reader.push(&chunk[..read], |message| messages.push(message)).is_err() {
                    eprintln!("[simulator] The simulator helper sent a malformed frame.");
                    if let Some(mut child) = reading.child.lock().unwrap().take() {
                        let _ = child.start_kill();
                    }
                    break;
                }
                for message in messages {
                    inner.deliver(&reading, &ready_sender, message);
                }
            }
            inner.helper_gone(&reading, &ready_sender);
        });
        self.runtime.spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr);
            let mut text = String::new();
            use tokio::io::AsyncBufReadExt;
            while matches!(lines.read_line(&mut text).await, Ok(read) if read > 0) {
                eprintln!("[simulator] {}", text.trim_end());
                text.clear();
            }
        });
        Ok(helper)
    }

    fn helper_gone(self: &Arc<Self>, gone: &Arc<Helper>, ready: &watch::Sender<Option<SimulatorResult<()>>>) {
        {
            let mut current = self.helper.lock().unwrap();
            if current.as_ref().is_some_and(|helper| Arc::ptr_eq(helper, gone)) {
                *current = None;
            }
        }
        if let Some(mut child) = gone.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
        let error = "The simulator helper stopped.";
        ready.send_if_modified(|value| {
            if value.is_none() {
                *value = Some(Err(error.into()));
                true
            } else {
                false
            }
        });
        gone.fail_all(error);
        let watchers: Vec<(String, usize)> = {
            let mut state = self.state.lock().unwrap();
            state.quieted.clear();
            state.watchers.iter().map(|(udid, count)| (udid.clone(), *count)).collect()
        };
        // Streams a view still watches pick up again in a new helper.
        if !watchers.is_empty() && self.state.lock().unwrap().unavailable.is_none() {
            let inner = self.clone();
            self.runtime.spawn(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                for (udid, count) in watchers {
                    for _ in 0..count {
                        let _ = inner.request(json!({ "op": "watch", "udid": udid }), REQUEST_TIMEOUT).await;
                    }
                }
            });
        }
    }

    async fn request(self: &Arc<Self>, body: Value, timeout: Duration) -> SimulatorResult<Map<String, Value>> {
        let helper = self.started().await?;
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        helper.pending.lock().unwrap().insert(id, sender);
        let mut line = match body {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        line.insert("id".into(), json!(id));
        let mut text = Value::Object(line).to_string();
        text.push('\n');
        if helper.stdin.lock().await.write_all(text.as_bytes()).await.is_err() {
            helper.pending.lock().unwrap().remove(&id);
            return Err("The simulator helper stopped.".into());
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err("The simulator helper stopped.".into()),
            Err(_) => {
                helper.pending.lock().unwrap().remove(&id);
                Err("The simulator did not answer in time.".into())
            }
        }
    }

    /// Runs `work` after every earlier piece of work queued for the device, in the order it was queued.
    fn in_turn<T: Send + 'static>(
        self: &Arc<Self>,
        queue: fn(&mut State) -> &mut HashMap<String, Turn>,
        udid: &str,
        work: BoxFuture<'static, SimulatorResult<T>>,
    ) -> BoxFuture<'static, SimulatorResult<T>> {
        let (sender, receiver) = oneshot::channel();
        let mut state = self.state.lock().unwrap();
        let before = queue(&mut state).get(udid).cloned();
        let turn: Turn = async move {
            if let Some(before) = before {
                before.await;
            }
            let _ = sender.send(work.await);
        }
        .boxed()
        .shared();
        queue(&mut state).insert(udid.to_string(), turn.clone());
        drop(state);
        let inner = self.clone();
        let device = udid.to_string();
        let tail = turn.clone();
        self.runtime.spawn(async move {
            tail.clone().await;
            let mut state = inner.state.lock().unwrap();
            let map = queue(&mut state);
            if map.get(&device).is_some_and(|current| current.ptr_eq(&tail)) {
                map.remove(&device);
            }
        });
        async move { receiver.await.unwrap_or_else(|_| Err("The simulator helper stopped.".into())) }.boxed()
    }

    fn input(self: &Arc<Self>, udid: &str, input: SimulatorInput) -> BoxFuture<'static, SimulatorResult<()>> {
        let udid_owned = udid.to_string();
        match input {
            SimulatorInput::Pasteboard { phase } => {
                let inner = self.clone();
                self.in_turn(pasteboard_queue, udid, async move { inner.sync_pasteboard(&udid_owned, phase).await }.boxed())
            }
            SimulatorInput::Shortcut { usage } if usage == KEY_C || usage == KEY_X => {
                let inner = self.clone();
                async move { inner.copy_out(&udid_owned, usage).await }.boxed()
            }
            SimulatorInput::Touch { phase: TouchPhase::Move, x, y, x2, y2 } => self.move_to(udid, Fingers { x, y, second: x2.zip(y2) }),
            other => {
                // A finger landing or lifting ends what a waiting move may still be folded into.
                if matches!(other, SimulatorInput::Touch { .. }) {
                    self.state.lock().unwrap().moves.remove(udid);
                }
                let inner = self.clone();
                self.in_turn(input_queue, udid, async move { inner.perform(&udid_owned, other).await }.boxed())
            }
        }
    }

    /// A drag streams moves faster than the device takes them, so a move waiting its turn only ever sends the newest position.
    fn move_to(self: &Arc<Self>, udid: &str, fingers: Fingers) -> BoxFuture<'static, SimulatorResult<()>> {
        let moves = self.state.lock().unwrap().moves.entry(udid.to_string()).or_default().clone();
        {
            let mut pending = moves.lock().unwrap();
            pending.next = Some(fingers);
            if pending.queued {
                return futures::future::ready(Ok(())).boxed();
            }
            pending.queued = true;
        }
        let inner = self.clone();
        let device = udid.to_string();
        self.in_turn(
            input_queue,
            udid,
            async move {
                let next = {
                    let mut pending = moves.lock().unwrap();
                    pending.queued = false;
                    pending.next.take()
                };
                if let Some(next) = next {
                    let mut body = Map::new();
                    body.insert("op".into(), json!("touch"));
                    body.insert("udid".into(), json!(device));
                    body.insert("phase".into(), json!("move"));
                    next.write(&mut body);
                    inner.request(Value::Object(body), REQUEST_TIMEOUT).await?;
                }
                Ok(())
            }
            .boxed(),
        )
    }

    /// The first time the host uses a device it turns it upright, so the picture and every tap agree. A
    /// device that will not turn is taken as upright, so its taps still land.
    async fn upright(self: &Arc<Self>, udid: &str) {
        {
            let mut state = self.state.lock().unwrap();
            if state.orientations.contains_key(udid) {
                return;
            }
            state.orientations.insert(udid.to_string(), SimulatorOrientation::Portrait);
        }
        let _ = self.request(json!({ "op": "rotate", "udid": udid, "orientation": SimulatorOrientation::Portrait.code() }), REQUEST_TIMEOUT).await;
    }

    /// A point as the app on screen measures it, moved onto the device's portrait screen.
    fn on_device(&self, udid: &str, u: f64, v: f64, screen: (f64, f64)) -> (f64, f64) {
        let orientation = self.state.lock().unwrap().orientations.get(udid).copied().unwrap_or(SimulatorOrientation::Portrait);
        if screen.0 > screen.1 {
            untilted(u, v, orientation)
        } else {
            (u, v)
        }
    }

    async fn screen_points(self: &Arc<Self>, udid: &str) -> SimulatorResult<(f64, f64)> {
        if let Some(known) = self.state.lock().unwrap().screens.get(udid) {
            return Ok(*known);
        }
        let result = self.request(json!({ "op": "describe", "udid": udid }), REQUEST_TIMEOUT).await?;
        let root: Option<RawElement> = result.get("elements").and_then(Value::as_array).and_then(|elements| elements.first()).and_then(|root| serde_json::from_value(root.clone()).ok());
        let size = root.map(|root| (root.frame_at(2), root.frame_at(3))).filter(|(width, height)| *width != 0.0 && *height != 0.0);
        let size = size.ok_or_else(|| "Could not measure the simulator's screen.".to_string())?;
        self.state.lock().unwrap().screens.insert(udid.to_string(), size);
        Ok(size)
    }

    /// A position as fractions of the screen, from points when it was given in points.
    async fn fractions(self: &Arc<Self>, udid: &str, x: f64, y: f64, points: Option<bool>) -> SimulatorResult<(f64, f64)> {
        if points != Some(true) {
            return Ok((x, y));
        }
        let screen = self.screen_points(udid).await?;
        Ok(self.on_device(udid, ratio(x / screen.0), ratio(y / screen.1), screen))
    }

    /// The middle of an element from the latest read of this device; a ref from any older read is refused.
    fn element_center(&self, udid: &str, reference: &str) -> SimulatorResult<(f64, f64)> {
        let (center, screen) = {
            let state = self.state.lock().unwrap();
            let read_id = reference.split('.').next().and_then(|part| part.parse::<u64>().ok());
            let snapshot = state.snapshots.get(udid).filter(|snapshot| Some(snapshot.id) == read_id);
            let Some(snapshot) = snapshot else {
                return Err(format!("The ref {reference} is from an older read of the screen. Read it again, then tap."));
            };
            let element = snapshot.elements.iter().find(|element| element.reference == reference).filter(|_| reference.contains('.'));
            let Some(element) = element else { return Err(format!("The latest read has no element {reference}.")) };
            let [x, y, width, height] = element.frame;
            ((ratio((x + width / 2.0) / snapshot.width), ratio((y + height / 2.0) / snapshot.height)), (snapshot.width, snapshot.height))
        };
        Ok(self.on_device(udid, center.0, center.1, screen))
    }

    async fn perform(self: &Arc<Self>, udid: &str, input: SimulatorInput) -> SimulatorResult<()> {
        if !matches!(input, SimulatorInput::Rotate { .. }) {
            self.upright(udid).await;
        }
        match input {
            SimulatorInput::Touch { phase, x, y, x2, y2 } => {
                let mut body = Map::new();
                body.insert("op".into(), json!("touch"));
                body.insert("udid".into(), json!(udid));
                body.insert("phase".into(), serde_json::to_value(phase).unwrap_or_default());
                Fingers { x, y, second: x2.zip(y2) }.write(&mut body);
                self.request(Value::Object(body), REQUEST_TIMEOUT).await?;
            }
            SimulatorInput::Tap { x, y, points } => {
                let (x, y) = self.fractions(udid, x, y, points).await?;
                self.request(json!({ "op": "tap", "udid": udid, "x": x, "y": y }), REQUEST_TIMEOUT).await?;
            }
            SimulatorInput::TapElement { reference } => {
                let (x, y) = self.element_center(udid, &reference)?;
                self.request(json!({ "op": "tap", "udid": udid, "x": x, "y": y }), REQUEST_TIMEOUT).await?;
            }
            SimulatorInput::Swipe { x, y, to_x, to_y, duration_ms, points } => {
                let from = self.fractions(udid, x, y, points).await?;
                let to = self.fractions(udid, to_x, to_y, points).await?;
                let duration = duration_ms.unwrap_or(300.0);
                let body = json!({ "op": "swipe", "udid": udid, "x": from.0, "y": from.1, "toX": to.0, "toY": to.1, "durationMs": duration });
                self.request(body, REQUEST_TIMEOUT + Duration::from_millis(duration_ms.unwrap_or(0.0) as u64)).await?;
            }
            SimulatorInput::Button { button } => {
                let name = match button {
                    SimulatorButton::Home => "home",
                    SimulatorButton::Lock => "lock",
                };
                self.request(json!({ "op": "button", "udid": udid, "button": name }), REQUEST_TIMEOUT).await?;
            }
            SimulatorInput::Type { text } => self.type_text(udid, &text).await?,
            SimulatorInput::Key { usage, down } => {
                self.request(json!({ "op": "key", "udid": udid, "usage": usage, "down": down }), REQUEST_TIMEOUT).await?;
            }
            SimulatorInput::Shortcut { usage } => self.shortcut(udid, usage).await?,
            SimulatorInput::Paste { text } => self.paste(udid, &text).await?,
            SimulatorInput::Rotate { orientation } => {
                self.request(json!({ "op": "rotate", "udid": udid, "orientation": orientation.code() }), REQUEST_TIMEOUT).await?;
                let mut state = self.state.lock().unwrap();
                state.orientations.insert(udid.to_string(), orientation);
                // The app redraws for the new way up, so what was read of it before no longer holds.
                state.screens.remove(udid);
                state.snapshots.remove(udid);
            }
            SimulatorInput::Pasteboard { .. } => {}
        }
        Ok(())
    }

    /// Presses ⌘ around one key, letting go of it even when the device refuses the key.
    async fn shortcut(self: &Arc<Self>, udid: &str, usage: i64) -> SimulatorResult<()> {
        let key = |usage: i64, down: bool| json!({ "op": "key", "udid": udid, "usage": usage, "down": down });
        self.request(key(COMMAND_KEY, true), REQUEST_TIMEOUT).await?;
        let pressed = async {
            self.request(key(usage, true), REQUEST_TIMEOUT).await?;
            self.request(key(usage, false), REQUEST_TIMEOUT).await.map(|_| ())
        }
        .await;
        let _ = self.request(key(COMMAND_KEY, false), REQUEST_TIMEOUT).await;
        pressed
    }

    async fn type_text(self: &Arc<Self>, udid: &str, text: &str) -> SimulatorResult<()> {
        let pasted = pasted_text(text);
        if let Some((paste, returns)) = &pasted {
            self.paste(udid, paste).await?;
            if returns.is_empty() {
                return Ok(());
            }
        }
        let typed = pasted.map(|(_, returns)| returns).unwrap_or_else(|| text.to_string());
        let timeout = REQUEST_TIMEOUT + Duration::from_millis(typed.encode_utf16().count() as u64 * 40);
        self.request(json!({ "op": "type", "udid": udid, "text": typed }), timeout).await.map(|_| ())
    }

    async fn read_pasteboard(&self, udid: &str) -> SimulatorResult<String> {
        let args = ["simctl", "pbpaste", udid];
        let output = Exec::new("xcrun").args(args).env("LANG", "en_US.UTF-8").timeout(REQUEST_TIMEOUT).max_buffer(LISTING_BUFFER).run().await.map_err(|error| run_failure("xcrun", &args, &error))?;
        Ok(output.stdout_text())
    }

    async fn write_pasteboard(&self, udid: &str, text: &str) -> SimulatorResult<()> {
        let args = ["simctl", "pbcopy", udid];
        Exec::new("xcrun").args(args).env("LANG", "en_US.UTF-8").input(Some(text.as_bytes())).timeout(REQUEST_TIMEOUT).run().await.map_err(|error| run_failure("xcrun", &args, &error))?;
        self.state.lock().unwrap().pasteboards.insert(udid.to_string(), text.to_string());
        Ok(())
    }

    /// Puts text on the device's pasteboard and pastes it into whatever has the keyboard, holding the
    /// device's next input until the paste lands.
    async fn paste(self: &Arc<Self>, udid: &str, text: &str) -> SimulatorResult<()> {
        let inner = self.clone();
        let device = udid.to_string();
        let text = text.to_string();
        self.in_turn(
            pasteboard_queue,
            udid,
            async move {
                inner.write_pasteboard(&device, &text).await?;
                inner.shortcut(&device, KEY_V).await
            }
            .boxed(),
        )
        .await?;
        tokio::time::sleep(PASTE_SETTLE).await;
        Ok(())
    }

    /// ⌘C or ⌘X on the device, then what it copied onto the Mac's clipboard once it reaches the device's
    /// pasteboard. A copy that changes nothing leaves the Mac's clipboard alone.
    async fn copy_out(self: &Arc<Self>, udid: &str, usage: i64) -> SimulatorResult<()> {
        let inner = self.clone();
        let device = udid.to_string();
        let before = self
            .in_turn(
                input_queue,
                udid,
                async move {
                    let reader = inner.clone();
                    let reading = device.clone();
                    let current = inner.in_turn(pasteboard_queue, &device, async move { reader.read_pasteboard(&reading).await }.boxed()).await?;
                    inner.shortcut(&device, usage).await?;
                    Ok(current)
                }
                .boxed(),
            )
            .await?;
        let inner = self.clone();
        let device = udid.to_string();
        self.in_turn(
            pasteboard_queue,
            udid,
            async move {
                let until = tokio::time::Instant::now() + COPY_WAIT;
                while tokio::time::Instant::now() < until {
                    tokio::time::sleep(COPY_POLL).await;
                    let text = inner.read_pasteboard(&device).await?;
                    if text == before {
                        continue;
                    }
                    inner.state.lock().unwrap().pasteboards.insert(device.clone(), text.clone());
                    if !text.is_empty() {
                        (inner.clipboard)(text);
                    }
                    return Ok(());
                }
                Ok(())
            }
            .boxed(),
        )
        .await
    }

    /// Notes what the device's pasteboard holds, and on leaving brings anything copied on the device since to the Mac.
    async fn sync_pasteboard(&self, udid: &str, phase: PasteboardPhase) -> SimulatorResult<()> {
        let text = self.read_pasteboard(udid).await?;
        let known = self.state.lock().unwrap().pasteboards.insert(udid.to_string(), text.clone());
        if phase == PasteboardPhase::Leave && known.is_some_and(|known| !text.is_empty() && text != known) {
            (self.clipboard)(text);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_failures_read_like_node() {
        let error = RunError::Exit { program: "xcrun".into(), code: Some(1), stdout: Vec::new(), stderr: b"Invalid device: X\n".to_vec() };
        assert_eq!(run_failure("xcrun", &["simctl", "boot", "X"], &error), "Command failed: xcrun simctl boot X\nInvalid device: X");
    }
}
