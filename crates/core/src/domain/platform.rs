//! What the machine around the app offers: the browser panel, simulators, dictation, paired
//! computers, app updates, computer use, plan usage, the terminal command and workspaces.

use serde::{Deserialize, Serialize};

use super::engine::AgentEngine;
use crate::util::{js_len, Map};

// Browser.

/// One page the browser panel holds open. The reducer owns this record; the view lives in the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTab {
    pub id: String,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What a caller does to the page. Every target is a `ref` from that tab's latest snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum BrowserAction {
    Click {
        #[serde(rename = "ref")]
        reference: String,
    },
    Type {
        #[serde(rename = "ref")]
        reference: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        submit: Option<bool>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A navigation the user has to answer before a run may make it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserApproval {
    pub approval_id: String,
    pub url: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}

/// The workspace grants origins globally and autonomous browsing per task.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserPermissions {
    pub origins: Vec<String>,
    pub autonomous_task_ids: Vec<String>,
}

const LOCAL_HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "[::1]", "0.0.0.0"];

fn has_scheme(text: &str) -> bool {
    let Some(at) = text.find("://") else { return false };
    let scheme = &text[..at];
    let mut chars = scheme.chars();
    chars.next().is_some_and(|ch| ch.is_ascii_alphabetic()) && chars.all(|ch| ch.is_ascii_alphanumeric() || "+.-".contains(ch))
}

/// What the browser will load for what a caller typed, or None. Only http and https ever come back.
pub fn browser_url(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if has_scheme(trimmed) {
        trimmed.to_string()
    } else {
        let host = trimmed.split([':', '/', '?', '#']).next().unwrap_or("");
        format!("{}://{trimmed}", if LOCAL_HOSTS.contains(&host) { "http" } else { "https" })
    };
    let url = url::Url::parse(&candidate).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    url.host_str().filter(|host| !host.is_empty())?;
    Some(url.to_string())
}

/// Where the panel searches.
pub fn browser_search_url(query: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(query.trim().as_bytes()).collect::<String>().replace('+', "%20");
    format!("https://duckduckgo.com/?q={encoded}")
}

/// The origin an allowlist entry covers. None for anything that is not a loadable page.
pub fn browser_origin(address: &str) -> Option<String> {
    let parsed = url::Url::parse(address).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    Some(parsed.origin().ascii_serialization())
}

/// What a tab calls itself: the page's own title, else the site it is on, else that it is waiting.
pub fn browser_tab_title(tab: &BrowserTab) -> String {
    if !tab.title.is_empty() {
        return tab.title.clone();
    }
    match url::Url::parse(&tab.url) {
        Ok(parsed) => {
            let host = match (parsed.host_str(), parsed.port()) {
                (Some(host), Some(port)) => format!("{host}:{port}"),
                (Some(host), None) => host.to_string(),
                _ => String::new(),
            };
            if host.is_empty() {
                "New tab".into()
            } else {
                host
            }
        }
        Err(_) => "New tab".into(),
    }
}

pub fn describe_tab(tab: &BrowserTab) -> String {
    let label = if !tab.title.is_empty() { tab.title.as_str() } else if !tab.url.is_empty() { tab.url.as_str() } else { "Blank tab" };
    let mut parts = vec![format!("{label} [{}]", tab.id), tab.url.clone()];
    if tab.loading {
        parts.push("loading".into());
    }
    if let Some(error) = &tab.error {
        parts.push(error.clone());
    }
    parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ")
}

// Simulator.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SimulatorDeviceState {
    Booted,
    Booting,
    Shutdown,
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulatorDevice {
    pub udid: String,
    pub name: String,
    pub runtime: String,
    pub state: SimulatorDeviceState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SimulatorAvailability {
    Unknown,
    Available,
    Unsupported { reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SimulatorOrientation {
    Portrait,
    LandscapeLeft,
    LandscapeRight,
    PortraitUpsideDown,
}

impl SimulatorOrientation {
    /// How far clockwise, in quarter turns, the panel turns the device's portrait picture.
    pub fn turns(self) -> u8 {
        match self {
            SimulatorOrientation::Portrait => 0,
            SimulatorOrientation::LandscapeRight => 1,
            SimulatorOrientation::PortraitUpsideDown => 2,
            SimulatorOrientation::LandscapeLeft => 3,
        }
    }

    /// UIDeviceOrientation's own number.
    pub fn code(self) -> u8 {
        match self {
            SimulatorOrientation::Portrait => 1,
            SimulatorOrientation::PortraitUpsideDown => 2,
            SimulatorOrientation::LandscapeLeft => 3,
            SimulatorOrientation::LandscapeRight => 4,
        }
    }

    /// The orientation after a quarter turn: clockwise for 1, counterclockwise for -1.
    pub fn turned(self, direction: i8) -> SimulatorOrientation {
        const BY_TURNS: [SimulatorOrientation; 4] = [SimulatorOrientation::Portrait, SimulatorOrientation::LandscapeRight, SimulatorOrientation::PortraitUpsideDown, SimulatorOrientation::LandscapeLeft];
        BY_TURNS[((self.turns() as i8 + direction + 4) % 4) as usize]
    }
}

/// Where a point on the turned picture lands on the device's own portrait screen.
pub fn untilted(u: f64, v: f64, orientation: SimulatorOrientation) -> (f64, f64) {
    match orientation.turns() {
        1 => (v, 1.0 - u),
        2 => (1.0 - u, 1.0 - v),
        3 => (1.0 - v, u),
        _ => (u, v),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulatorDriver {
    pub task_id: String,
    pub udid: String,
    pub seq: i64,
}

/// The simulators this Mac has, kept for the whole app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulatorState {
    pub availability: SimulatorAvailability,
    pub devices: Vec<SimulatorDevice>,
    pub booting: Vec<String>,
    pub errors: Map<String>,
    pub driver: Option<SimulatorDriver>,
    pub orientations: Map<SimulatorOrientation>,
}

impl Default for SimulatorState {
    fn default() -> Self {
        SimulatorState { availability: SimulatorAvailability::Unknown, devices: Vec::new(), booting: Vec::new(), errors: Map::new(), driver: None, orientations: Map::new() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SimulatorButton {
    Home,
    Lock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TouchPhase {
    Down,
    Move,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasteboardPhase {
    Enter,
    Leave,
}

/// Positions are fractions of the screen from its top left unless `points` says otherwise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum SimulatorInput {
    Touch {
        phase: TouchPhase,
        x: f64,
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x2: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y2: Option<f64>,
    },
    Tap {
        x: f64,
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        points: Option<bool>,
    },
    TapElement {
        #[serde(rename = "ref")]
        reference: String,
    },
    Swipe {
        x: f64,
        y: f64,
        to_x: f64,
        to_y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        points: Option<bool>,
    },
    Button { button: SimulatorButton },
    Type { text: String },
    Key { usage: i64, down: bool },
    Shortcut { usage: i64 },
    Paste { text: String },
    Pasteboard { phase: PasteboardPhase },
    Rotate { orientation: SimulatorOrientation },
}

pub const MAX_SIMULATOR_TEXT: usize = 2_000;
pub const MAX_PASTE_TEXT: usize = 100_000;
pub const MAX_SWIPE_MS: f64 = 5_000.0;
const MAX_KEY_USAGE: i64 = 0xe7;
const MAX_POINTS: f64 = 10_000.0;

/// Inputs that move text between the device and the Mac's clipboard, which only a person sends.
pub fn uses_mac_clipboard(input: &SimulatorInput) -> bool {
    matches!(input, SimulatorInput::Shortcut { .. } | SimulatorInput::Paste { .. } | SimulatorInput::Pasteboard { .. })
}

fn fraction(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn positioned(points: Option<bool>, coordinates: &[f64]) -> bool {
    match points {
        None => coordinates.iter().all(|value| fraction(*value)),
        Some(true) => coordinates.iter().all(|value| value.is_finite() && (0.0..=MAX_POINTS).contains(value)),
        Some(false) => false,
    }
}

pub fn is_simulator_input(input: &SimulatorInput) -> bool {
    let key_usage = |usage: i64| usage > 0 && usage <= MAX_KEY_USAGE;
    match input {
        SimulatorInput::Touch { x, y, x2, y2, .. } => fraction(*x) && fraction(*y) && match (x2, y2) {
            (None, None) => true,
            (Some(x2), Some(y2)) => fraction(*x2) && fraction(*y2),
            _ => false,
        },
        SimulatorInput::Tap { x, y, points } => positioned(*points, &[*x, *y]),
        SimulatorInput::TapElement { reference } => !reference.is_empty() && js_len(reference) <= 64,
        SimulatorInput::Swipe { x, y, to_x, to_y, duration_ms, points } => {
            positioned(*points, &[*x, *y]) && positioned(*points, &[*to_x, *to_y]) && duration_ms.is_none_or(|duration| (0.0..=MAX_SWIPE_MS).contains(&duration))
        }
        SimulatorInput::Button { .. } | SimulatorInput::Rotate { .. } | SimulatorInput::Pasteboard { .. } => true,
        SimulatorInput::Type { text } => !text.is_empty() && js_len(text) <= MAX_SIMULATOR_TEXT,
        SimulatorInput::Key { usage, .. } | SimulatorInput::Shortcut { usage } => key_usage(*usage),
        SimulatorInput::Paste { text } => !text.is_empty() && js_len(text) <= MAX_PASTE_TEXT,
    }
}

/// The device a panel shows when its dock has none.
pub fn default_simulator<'a>(devices: &'a [SimulatorDevice], remembered: Option<&str>) -> Option<&'a SimulatorDevice> {
    devices
        .iter()
        .find(|device| Some(device.udid.as_str()) == remembered)
        .or_else(|| devices.iter().find(|device| device.state == SimulatorDeviceState::Booted))
        .or_else(|| devices.iter().find(|device| device.name.starts_with("iPhone")))
        .or(devices.first())
}

pub fn simulator_label(device: &SimulatorDevice) -> String {
    format!("{} · {}", device.name, device.runtime)
}

/// The USB HID usage of a physical key, by its code.
pub fn hid_usage(code: &str) -> Option<i64> {
    if let Some(letter) = code.strip_prefix("Key") {
        if letter.len() == 1 && letter.chars().all(|ch| ch.is_ascii_uppercase()) {
            return Some(4 + (letter.as_bytes()[0] - b'A') as i64);
        }
    }
    for (prefix, base) in [("Digit", 29), ("Numpad", 88)] {
        if let Some(digit) = code.strip_prefix(prefix) {
            if digit.len() == 1 && ('1'..='9').contains(&digit.chars().next().unwrap()) {
                return Some(base + digit.parse::<i64>().ok()?);
            }
        }
    }
    Some(match code {
        "Digit0" => 39,
        "Enter" | "NumpadEnter" => 40,
        "Escape" => 41,
        "Backspace" => 42,
        "Tab" => 43,
        "Space" => 44,
        "Minus" => 45,
        "Equal" => 46,
        "BracketLeft" => 47,
        "BracketRight" => 48,
        "Backslash" => 49,
        "Semicolon" => 51,
        "Quote" => 52,
        "Backquote" => 53,
        "Comma" => 54,
        "Period" => 55,
        "Slash" => 56,
        "IntlBackslash" => 100,
        "Home" => 74,
        "PageUp" => 75,
        "Delete" => 76,
        "End" => 77,
        "PageDown" => 78,
        "ArrowRight" => 79,
        "ArrowLeft" => 80,
        "ArrowDown" => 81,
        "ArrowUp" => 82,
        "NumpadDivide" => 84,
        "NumpadMultiply" => 85,
        "NumpadSubtract" => 86,
        "NumpadAdd" => 87,
        "Numpad0" => 98,
        "NumpadDecimal" => 99,
        "NumpadEqual" => 103,
        "ControlLeft" => 224,
        "ShiftLeft" => 225,
        "AltLeft" => 226,
        "ControlRight" => 228,
        "ShiftRight" => 229,
        "AltRight" => 230,
        _ => return None,
    })
}

/// The screen's corner radius as a fraction of its width.
pub fn screen_corner_radius(name: &str) -> f64 {
    if name.starts_with("iPad") {
        0.024
    } else {
        0.137
    }
}

// Dictation.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DictationModel {
    Unsupported,
    Missing,
    Downloading,
    Ready,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DictationPhase {
    Starting,
    Recording,
    Transcribing,
}

/// One recording. `key` is the draft its words go to, fixed when it began.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationSession {
    pub id: String,
    pub key: String,
    pub phase: DictationPhase,
    pub hands_free: bool,
    pub send: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DictationState {
    pub model: DictationModel,
    pub message: Option<String>,
    pub progress: Option<f64>,
    pub session: Option<DictationSession>,
    pub sends: i64,
}

impl Default for DictationState {
    fn default() -> Self {
        DictationState { model: DictationModel::Unsupported, message: None, progress: None, session: None, sends: 0 }
    }
}

pub const DICTATION_TAP_MS: i64 = 250;
pub const DICTATION_UNSUPPORTED: &str = "Dictation needs Linux or a Mac with Apple silicon.";
pub const DICTATION_MISSING: &str = "Download the speech model in Settings → General to dictate.";
pub const DICTATION_DOWNLOADING: &str = "The speech model is still downloading.";
pub const DICTATION_HEARD_NOTHING: &str = "Didn't catch anything.";

/// Why a hold cannot start, or None when it can.
pub fn dictation_refusal(state: &DictationState) -> Option<String> {
    match state.model {
        DictationModel::Ready => None,
        DictationModel::Missing => Some(DICTATION_MISSING.into()),
        DictationModel::Downloading => Some(DICTATION_DOWNLOADING.into()),
        DictationModel::Failed => Some(state.message.clone().unwrap_or_else(|| DICTATION_MISSING.into())),
        DictationModel::Unsupported => Some(state.message.clone().unwrap_or_else(|| DICTATION_UNSUPPORTED.into())),
    }
}

/// Spoken words follow whatever was typed, on the same line unless the typing ended one.
pub fn with_spoken_words(typed: &str, spoken: &str) -> String {
    let words = spoken.trim();
    if words.is_empty() {
        return typed.to_string();
    }
    if typed.trim().is_empty() {
        return words.to_string();
    }
    if typed.ends_with(char::is_whitespace) {
        format!("{typed}{words}")
    } else {
        format!("{typed} {words}")
    }
}

// Paired computers.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComputerStatus {
    Connecting,
    Connected,
    Offline,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredComputer {
    pub host: String,
    pub name: String,
    pub os: String,
}

/// A computer this one has paired with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerLink {
    pub id: String,
    pub name: String,
    pub host: String,
    pub status: ComputerStatus,
    pub error: Option<String>,
    pub paired_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComputerPairing {
    pub host: String,
    pub name: String,
    pub busy: bool,
    pub error: Option<String>,
}

/// Which computers' threads the sidebar draws: "all", "this", or one paired computer by id.
pub type ComputerFilter = String;

pub const MAX_COMPUTER_NAME: usize = 128;

pub fn away_status(status: ComputerStatus) -> &'static str {
    if status == ComputerStatus::Connecting {
        "reconnecting"
    } else {
        "offline"
    }
}

pub fn offline_message(name: &str, status: ComputerStatus, error: Option<&str>) -> String {
    let away = format!("{name} is {}.", away_status(status));
    match error {
        Some(error) => format!("{away} {error}"),
        None => away,
    }
}

/// The address as dialled, with a pasted scheme or trailing slash dropped.
pub fn computer_address(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let lowered = trimmed.to_lowercase();
    let mut address = trimmed;
    for scheme in ["https://", "http://", "wss://", "ws://"] {
        if lowered.starts_with(scheme) {
            address = &trimmed[scheme.len()..];
            break;
        }
    }
    let address = address.trim_end_matches('/');
    let pattern = regex::Regex::new(r"(?i)^(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*|\[[0-9a-f:.]+\])(?::(\d{1,5}))?$").ok()?;
    let found = pattern.captures(address)?;
    match found.get(1) {
        None => Some(address.to_string()),
        Some(port) => {
            let port: u32 = port.as_str().parse().ok()?;
            (port > 0 && port <= 65535).then(|| address.to_string())
        }
    }
}

// App updates.

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AppUpdate {
    #[default]
    Idle,
    Available { version: String },
    Downloading { version: String, percent: i64 },
    Ready { version: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppBuild {
    pub version: String,
    pub self_updating: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AppUpdateRequest {
    Waiting,
    Installing { version: String },
    Failed { message: String },
}

// Computer use.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComputerUsePermission {
    Accessibility,
    ScreenRecording,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinuxRuntime {
    pub status: String,
    pub display: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUsePermissions {
    pub accessibility: bool,
    pub screen_recording: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_runtime: Option<LinuxRuntime>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUseAccessState {
    pub permissions: Option<ComputerUsePermissions>,
    pub busy: Option<ComputerUsePermission>,
    pub error: Option<String>,
    pub requested: bool,
    pub restart_required: bool,
}

// Plan usage.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsagePeriod {
    Session,
    Week,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    pub utilization: Option<f64>,
    pub resets_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<UsagePeriod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum PlanUsage {
    Available { subscription: Option<String>, windows: Vec<UsageWindow> },
    NotApplicable,
    Unavailable { message: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanUsageState {
    pub read: i64,
    pub reports: indexmap::IndexMap<AgentEngine, PlanUsage>,
    pub reading: Vec<AgentEngine>,
    pub dismissed: Vec<String>,
}

pub fn format_share(utilization: Option<f64>) -> String {
    match utilization {
        None => "Not reported".into(),
        Some(share) => format!("{}% used", super::appearance::js_round(share.clamp(0.0, 100.0))),
    }
}

pub fn bar_share(utilization: Option<f64>) -> f64 {
    utilization.map(|share| share.clamp(0.0, 100.0)).unwrap_or(0.0)
}

pub fn plan_label(subscription: Option<&str>) -> Option<String> {
    let subscription = subscription.filter(|value| !value.is_empty())?;
    let label = match subscription {
        "prolite" => "Pro".to_string(),
        "self_serve_business_prolite" | "self_serve_business_usage_based" => "Business".to_string(),
        "enterprise_cbp_automation" | "enterprise_cbp_usage_based" => "Enterprise".to_string(),
        other => other
            .split('_')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    };
    Some(format!("{label} plan"))
}

pub const NEAR_LIMIT_SHARE: f64 = 80.0;

fn counts_model(window: &UsageWindow, label: &str) -> bool {
    match &window.model {
        None => true,
        Some(model) => model.to_lowercase().split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.')).any(|part| part == label.to_lowercase()),
    }
}

/// The windows a run on the model named `label` spends.
pub fn windows_for(usage: Option<&PlanUsage>, label: &str) -> Vec<UsageWindow> {
    match usage {
        Some(PlanUsage::Available { windows, .. }) => windows.iter().filter(|window| counts_model(window, label)).cloned().collect(),
        _ => Vec::new(),
    }
}

/// The window closest to running out.
pub fn tightest_window(windows: &[UsageWindow]) -> Option<UsageWindow> {
    let mut tightest: Option<&UsageWindow> = None;
    for window in windows {
        let Some(share) = window.utilization else { continue };
        if tightest.is_none_or(|best| share > best.utilization.unwrap_or(f64::MIN)) {
            tightest = Some(window);
        }
    }
    tightest.cloned()
}

pub fn window_span(window: &UsageWindow) -> Option<String> {
    let minutes = window.minutes.filter(|minutes| minutes.is_finite() && *minutes > 0.0)?;
    let whole = minutes as i64;
    if minutes == 10_080.0 {
        return Some("Weekly".into());
    }
    if minutes.fract() == 0.0 && whole % 1_440 == 0 {
        return Some(format!("{}d", whole / 1_440));
    }
    if minutes.fract() == 0.0 && whole % 60 == 0 {
        return Some(format!("{}h", whole / 60));
    }
    Some(format!("{minutes}m"))
}

pub fn session_and_week(windows: &[UsageWindow]) -> (Option<UsageWindow>, Option<UsageWindow>) {
    let of = |period: UsagePeriod| tightest_window(&windows.iter().filter(|window| window.period == Some(period)).cloned().collect::<Vec<_>>());
    (of(UsagePeriod::Session), of(UsagePeriod::Week))
}

/// What a warning about `window` is called once dismissed.
/// Names a warning until its window resets. Providers jitter the reset by fractions of a second
/// between reads, so it counts to the nearest minute.
pub fn usage_warning_key(engine: &str, window: &UsageWindow) -> String {
    let share = window.utilization.unwrap_or(0.0);
    let reset = window.resets_at.as_deref().unwrap_or("");
    let reset = chrono::DateTime::parse_from_rfc3339(reset).map_or_else(|_| reset.to_string(), |at| ((at.timestamp_millis() + 30_000).div_euclid(60_000)).to_string());
    format!("{engine}:{}:{reset}:{}", window.id, if share >= 100.0 { "reached" } else { "near" })
}

/// How many put-away usage warnings the window remembers between launches.
pub const MAX_DISMISSED_USAGE_WARNINGS: usize = 32;

// The terminal command.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliInstallState {
    Installed,
    Missing,
    Conflict,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    pub state: CliInstallState,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_path: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CliState {
    pub status: Option<CliStatus>,
    pub busy: bool,
    pub error: Option<String>,
}

pub use crate::identity::CLI_COMMAND;

// Workspaces.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    Project,
    Projectless,
    Worktree,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub id: String,
    pub kind: WorkspaceKind,
    pub root: String,
}

/// An application the machine has, as the window draws it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledApp {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_urls_only_load_web_pages() {
        assert_eq!(browser_url("localhost:3000").as_deref(), Some("http://localhost:3000/"));
        assert_eq!(browser_url("example.com").as_deref(), Some("https://example.com/"));
        assert_eq!(browser_url("file:///etc/passwd"), None);
        assert_eq!(browser_origin("https://a.com/x?y").as_deref(), Some("https://a.com"));
    }

    #[test]
    fn computer_addresses_drop_schemes_and_check_ports() {
        assert_eq!(computer_address("https://mac.tail.ts.net:8080/").as_deref(), Some("mac.tail.ts.net:8080"));
        assert_eq!(computer_address("host:70000"), None);
    }

    #[test]
    fn spoken_words_join_typed_text() {
        assert_eq!(with_spoken_words("hello", " world "), "hello world");
        assert_eq!(with_spoken_words("hello\n", "world"), "hello\nworld");
    }

    #[test]
    fn usage_warnings_outlast_reset_jitter() {
        let window = |resets_at: &str| UsageWindow { id: "seven_day".into(), label: String::new(), utilization: Some(91.0), resets_at: Some(resets_at.into()), model: None, period: None, minutes: None };
        let key = usage_warning_key("claude", &window("2026-11-21T21:59:59.581923+00:00"));
        assert_eq!(key, usage_warning_key("claude", &window("2026-11-21T22:00:00.104277+00:00")));
        assert_ne!(key, usage_warning_key("claude", &window("2026-11-28T21:59:59.581923+00:00")));
    }
}
