//! The devices `simctl` lists, how the picker groups them, and what a read of the screen keeps.

use act_core::domain::platform::{SimulatorDevice, SimulatorDeviceState};
use act_core::domain::settings_catalog::ranked_matches;
use serde::Deserialize;
use std::collections::BTreeMap;

fn device_state(state: &str) -> SimulatorDeviceState {
    match state {
        "Booted" => SimulatorDeviceState::Booted,
        "Booting" => SimulatorDeviceState::Booting,
        "Shutdown" => SimulatorDeviceState::Shutdown,
        _ => SimulatorDeviceState::Other,
    }
}

fn last_part(identifier: &str) -> &str {
    identifier.rsplit('.').next().unwrap_or(identifier)
}

/// "com.apple.CoreSimulator.SimRuntime.iOS-26-4" reads as "iOS 26.4".
pub fn runtime_name(identifier: &str) -> String {
    let mut parts = last_part(identifier).split('-');
    let platform = parts.next().unwrap_or_default();
    format!("{platform} {}", parts.collect::<Vec<_>>().join("."))
}

fn runtime_version(identifier: &str) -> Vec<f64> {
    last_part(identifier).split('-').skip(1).map(|part| part.parse::<f64>().unwrap_or(f64::NAN)).collect()
}

fn newer(left: &[f64], right: &[f64]) -> std::cmp::Ordering {
    for index in 0..left.len().max(right.len()) {
        let difference = right.get(index).copied().unwrap_or(0.0) - left.get(index).copied().unwrap_or(0.0);
        if difference != 0.0 && !difference.is_nan() {
            return if difference > 0.0 { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less };
        }
    }
    std::cmp::Ordering::Equal
}

#[derive(Deserialize)]
struct Listed {
    #[serde(default)]
    devices: BTreeMap<String, Vec<ListedDevice>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedDevice {
    udid: String,
    name: String,
    state: String,
    is_available: Option<bool>,
}

/// iOS devices from the newest runtime down, iPhones before iPads within each.
pub fn parse_device_list(json: &str) -> Result<Vec<SimulatorDevice>, serde_json::Error> {
    let listed: Listed = serde_json::from_str(json)?;
    let mut runtimes: Vec<&String> = listed.devices.keys().filter(|runtime| runtime.contains(".SimRuntime.iOS-")).collect();
    runtimes.sort_by(|left, right| newer(&runtime_version(left), &runtime_version(right)));
    let mut devices = Vec::new();
    for runtime in runtimes {
        let mut listed_here: Vec<&ListedDevice> = listed.devices.get(runtime).map(|list| list.iter().filter(|device| device.is_available != Some(false)).collect()).unwrap_or_default();
        listed_here.sort_by_key(|device| !device.name.starts_with("iPhone"));
        devices.extend(listed_here.into_iter().map(|device| SimulatorDevice {
            udid: device.udid.clone(),
            name: device.name.clone(),
            runtime: runtime_name(runtime),
            state: device_state(&device.state),
        }));
    }
    Ok(devices)
}

#[derive(Clone, Debug, PartialEq)]
pub struct SimulatorGroup {
    pub label: String,
    pub running: bool,
    pub devices: Vec<SimulatorDevice>,
}

/// The devices a picker lists, narrowed by what is typed: the ones running first, then the rest under
/// the runtime they run, newest runtime first.
pub fn simulator_groups(devices: &[SimulatorDevice], query: &str) -> Vec<SimulatorGroup> {
    let ranked = ranked_matches(devices, query, |device| vec![device.name.clone(), device.runtime.clone(), format!("{} {}", device.runtime, device.name)]);
    // A query that names a device outright puts it first, ahead of the longer names it is the start of.
    let wanted = query.trim().to_lowercase();
    let exact: Vec<SimulatorDevice> = ranked.iter().filter(|device| device.name.to_lowercase() == wanted).cloned().collect();
    let matched: Vec<SimulatorDevice> = exact.iter().cloned().chain(ranked.into_iter().filter(|device| !exact.iter().any(|other| other.udid == device.udid))).collect();
    let is_running = |device: &SimulatorDevice| matches!(device.state, SimulatorDeviceState::Booted | SimulatorDeviceState::Booting);
    let running: Vec<SimulatorDevice> = matched.iter().filter(|device| is_running(device)).cloned().collect();
    let mut groups = Vec::new();
    if !running.is_empty() {
        groups.push(SimulatorGroup { label: "Running".into(), running: true, devices: running });
    }
    for device in matched.iter().filter(|device| !is_running(device)) {
        match groups.iter_mut().find(|group: &&mut SimulatorGroup| !group.running && group.label == device.runtime) {
            Some(group) => group.devices.push(device.clone()),
            None => groups.push(SimulatorGroup { label: device.runtime.clone(), running: false, devices: vec![device.clone()] }),
        }
    }
    // While searching, the group holding the best match leads, so Enter takes what heads the list.
    if !wanted.is_empty() {
        let position = |group: &SimulatorGroup| matched.iter().position(|device| device.udid == group.devices[0].udid).unwrap_or(usize::MAX);
        groups.sort_by_key(position);
    }
    groups
}

/// Text a US keyboard cannot type, which is pasted whole instead, with the returns at its end still
/// pressed so a submit still submits. None for text the keyboard types.
pub fn pasted_text(text: &str) -> Option<(String, String)> {
    if text.chars().all(|ch| matches!(ch, '\x20'..='\x7e' | '\n' | '\t')) {
        return None;
    }
    let paste = text.trim_end_matches('\n').to_string();
    let returns = text[paste.len()..].to_string();
    Some((paste, returns))
}

/// Roles worth listing even with nothing to say, since they are what a tap lands on.
const ACTIONABLE_ROLES: [&str; 16] = [
    "AXButton", "AXTextField", "AXSecureTextField", "AXTextArea", "AXSearchField", "AXSwitch", "AXCheckBox", "AXSlider", "AXLink", "AXCell", "AXMenuItem",
    "AXPopUpButton", "AXTab", "AXRadioButton", "AXIncrementor", "AXKey",
];

/// One element as the helper describes it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawElement {
    pub role: Option<String>,
    pub label: Option<String>,
    pub value: Option<String>,
    pub identifier: Option<String>,
    pub frame: Option<Vec<f64>>,
    pub depth: Option<f64>,
}

impl RawElement {
    pub fn frame_at(&self, index: usize) -> f64 {
        self.frame.as_ref().and_then(|frame| frame.get(index).copied()).unwrap_or(0.0)
    }
}

/// What a read lists: what has something to say or can be tapped, and is on the screen at all.
pub fn listed(element: &RawElement, width: f64, height: f64) -> bool {
    let (x, y, w, h) = (element.frame_at(0), element.frame_at(1), element.frame_at(2), element.frame_at(3));
    if w <= 0.0 || h <= 0.0 || x + w <= 0.0 || y + h <= 0.0 || x >= width || y >= height {
        return false;
    }
    let says = |field: &Option<String>| field.as_deref().is_some_and(|text| !text.is_empty());
    says(&element.label) || says(&element.value) || says(&element.identifier) || ACTIONABLE_ROLES.contains(&element.role.as_deref().unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_list_newest_runtime_first_with_iphones_leading() {
        let json = r#"{"devices":{
            "com.apple.CoreSimulator.SimRuntime.iOS-18-5":[{"udid":"a","name":"iPad mini","state":"Shutdown","isAvailable":true},{"udid":"b","name":"iPhone 16 Pro","state":"Booted","isAvailable":true}],
            "com.apple.CoreSimulator.SimRuntime.watchOS-11-0":[{"udid":"w","name":"Watch","state":"Shutdown"}],
            "com.apple.CoreSimulator.SimRuntime.iOS-26-4":[{"udid":"c","name":"iPhone 17","state":"Shutdown","isAvailable":false},{"udid":"d","name":"iPhone 17 Pro","state":"Booting"}]
        }}"#;
        let devices = parse_device_list(json).unwrap();
        let summary: Vec<(&str, &str)> = devices.iter().map(|device| (device.udid.as_str(), device.runtime.as_str())).collect();
        assert_eq!(summary, [("d", "iOS 26.4"), ("b", "iOS 18.5"), ("a", "iOS 18.5")]);
        assert_eq!(devices[0].state, SimulatorDeviceState::Booting);
    }

    fn device(udid: &str, name: &str, runtime: &str, state: SimulatorDeviceState) -> SimulatorDevice {
        SimulatorDevice { udid: udid.into(), name: name.into(), runtime: runtime.into(), state }
    }

    #[test]
    fn groups_put_running_first_and_an_exact_name_ahead_of_longer_ones() {
        use SimulatorDeviceState::*;
        let devices = [
            device("1", "iPhone 17 Pro", "iOS 26.4", Shutdown),
            device("2", "iPhone 17 Pro Max", "iOS 26.4", Shutdown),
            device("3", "iPhone 16 Pro", "iOS 18.5", Booted),
            device("4", "iPad Air", "iOS 18.5", Shutdown),
        ];
        let groups = simulator_groups(&devices, "");
        let labels: Vec<&str> = groups.iter().map(|group| group.label.as_str()).collect();
        assert_eq!(labels, ["Running", "iOS 26.4", "iOS 18.5"]);
        let searched = simulator_groups(&devices, "iphone 17 pro max");
        assert_eq!(searched[0].devices[0].udid, "2");
        let searched = simulator_groups(&devices, "iPhone 17 Pro");
        assert_eq!(searched[0].devices.iter().map(|device| device.udid.as_str()).collect::<Vec<_>>(), ["1", "2"]);
    }

    #[test]
    fn only_text_a_keyboard_cannot_type_is_pasted_keeping_trailing_returns() {
        assert_eq!(pasted_text("hello\n"), None);
        assert_eq!(pasted_text("héllo\n\n"), Some(("héllo".into(), "\n\n".into())));
    }
}
