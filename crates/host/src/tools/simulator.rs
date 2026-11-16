//! The device the thread's own Simulator panel shows: writes are commands, reads are the screen.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Value};

use super::definition::{bind_tools, names_of, report, BoundTool, ToolDefinition};
use super::schema::{boolean, enumeration, number, string, Args, Field, Input};
use super::server::{Grants, Offers, ToolRun, ToolServer};
use super::types::*;

pub const SIMULATOR_SERVER_NAME: &str = "aicodingtool-simulator";

pub trait SimulatorBridge: Send + Sync {
    fn command(&self, write: SimulatorWrite) -> BoxFuture<'_, Result<(), String>>;
    fn read(&self, read: SimulatorRead) -> BoxFuture<'_, Result<SimulatorReadResult, String>>;
}

/// A first boot of a fresh device can take minutes; one already up answers at once.
const BOOT_WAIT_MS: u64 = 240_000;
/// Long enough for a tap's animation to land before the screen is read back.
const SETTLE: Duration = Duration::from_millis(400);
const MAX_DEPTH_INDENT: usize = 8;
const SIMULATOR_ERROR: &str = "Simulator error: ";
pub const SIMULATOR_ORIENTATIONS: [&str; 4] = ["portrait", "landscape-left", "landscape-right", "portrait-upside-down"];

fn point(axis: &str) -> Field {
    number().min(0).max(10_000).describe(format!("{axis} in points from the screen's top left, as simulator_read measures it."))
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// A number as JavaScript prints it: whole numbers without a fraction.
fn js_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 { format!("{}", value as i64) } else { format!("{value}") }
}

fn element_line(element: &SimulatorElement) -> String {
    let [x, y, width, height] = element.frame;
    let label = element.label.as_deref().map(|label| format!(" {}", quoted(label))).unwrap_or_default();
    let value = element.value.as_deref().map(|value| format!(" = {}", quoted(value))).unwrap_or_default();
    let identifier = element.identifier.as_deref().map(|identifier| format!(" #{identifier}")).unwrap_or_default();
    format!(
        "{}[{}] {}{label}{value}{identifier} ({},{} {}x{})",
        "  ".repeat(element.depth.min(MAX_DEPTH_INDENT)),
        element.reference,
        element.role,
        js_number(x),
        js_number(y),
        js_number(width),
        js_number(height)
    )
}

fn snapshot_text(snapshot: &SimulatorSnapshot) -> String {
    let heading = format!("{} on {} — screen {}x{} points", snapshot.application.as_deref().unwrap_or("Unknown app"), snapshot.device, js_number(snapshot.width), js_number(snapshot.height));
    let elements = if snapshot.elements.is_empty() { "The app on screen exposes no accessibility elements.".into() } else { snapshot.elements.iter().map(element_line).collect::<Vec<_>>().join("\n") };
    format!("{heading}\n{elements}")
}

pub fn read_text(result: &SimulatorReadResult) -> String {
    match result {
        SimulatorReadResult::NoSimulator { reason } => reason.clone(),
        SimulatorReadResult::Status { device, booted, error } => match error.as_deref().filter(|error| !error.is_empty()) {
            Some(error) => format!("{} could not start: {error}", device.label()),
            None if *booted => format!("{} is running.", device.label()),
            None => format!("{} is still starting.", device.label()),
        },
        SimulatorReadResult::Snapshot { snapshot, shot } => {
            let mut parts = Vec::new();
            if let Some(shot) = shot {
                parts.push(format!("Screenshot saved to {} ({}x{} pixels). Open that file to see the screen.", shot.path, shot.width, shot.height));
            }
            parts.push(snapshot_text(snapshot));
            parts.join("\n\n")
        }
    }
}

/// Every gesture answers with the screen it left behind, read once the animation it started has landed.
async fn act(bridge: &dyn SimulatorBridge, input: Value) -> Result<String, String> {
    bridge.command(json!({ "type": "simulator.input", "input": input })).await?;
    tokio::time::sleep(SETTLE).await;
    Ok(read_text(&bridge.read(SimulatorRead::Snapshot { screenshot: false }).await?))
}

pub fn simulator_tools() -> Vec<ToolDefinition<dyn SimulatorBridge>> {
    vec![
        ToolDefinition {
            name: "simulator_open",
            description: "Start an iOS simulator in the AICodingTool Simulator panel and wait until it is running. It runs headless, so the user keeps using their computer while you test. Answers with the device's UDID: build for it with `xcodebuild -destination 'id=<UDID>'`, then `xcrun simctl install <UDID> <path.app>` and `xcrun simctl launch <UDID> <bundle id>`.".into(),
            input: Input::new(vec![("device", string().max(200).optional().describe("A device name such as \"iPhone 17 Pro\", or a UDID. Defaults to the device the panel already shows, else one that is running, else the newest iPhone."))]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(SIMULATOR_ERROR, async {
                        let mut command = json!({ "type": "simulator.open" });
                        if let Some(device) = args.text("device").filter(|device| !device.is_empty()) {
                            command["device"] = json!(device);
                        }
                        bridge.command(command).await?;
                        let status = bridge.read(SimulatorRead::Status { timeout_ms: BOOT_WAIT_MS }).await?;
                        match &status {
                            SimulatorReadResult::Status { booted: true, device, .. } => Ok(format!("{} UDID: {}. Read the screen with simulator_read.", read_text(&status), device.udid)),
                            _ => Err(read_text(&status)),
                        }
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "simulator_read",
            description: "Read the simulator's screen: the app on screen as accessibility elements, each with a ref, its role, label, value and frame in points, plus a screenshot file. Refs are only valid until the next read of the screen.".into(),
            input: Input::new(vec![("screenshot", boolean().optional().describe("Also save a screenshot to look at. Defaults to true."))]),
            read_only: true,
            run: |bridge, args| async move { report(SIMULATOR_ERROR, async { Ok(read_text(&bridge.read(SimulatorRead::Snapshot { screenshot: args.flag("screenshot") != Some(false) }).await?)) }).await }.boxed(),
        },
        ToolDefinition {
            name: "simulator_tap",
            description: "Tap the simulator's screen, on an element by the ref the latest simulator_read gave it, or at a point. Answers with the elements on screen afterwards.".into(),
            input: Input::new(vec![
                ("ref", string().max(64).optional().describe("The element's ref from the latest simulator_read.")),
                ("x", point("Horizontal position").optional()),
                ("y", point("Vertical position").optional()),
            ]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(SIMULATOR_ERROR, async {
                        if let Some(reference) = args.text("ref").filter(|reference| !reference.is_empty()) {
                            return act(bridge.as_ref(), json!({ "kind": "tap-element", "ref": reference })).await;
                        }
                        let (Some(x), Some(y)) = (args.get("x"), args.get("y")) else { return Err("Give a ref, or both x and y.".into()) };
                        act(bridge.as_ref(), json!({ "kind": "tap", "x": x, "y": y, "points": true })).await
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "simulator_swipe",
            description: "Drag a finger across the simulator's screen from one point to another, to scroll, swipe between pages, or pull down. Answers with the elements on screen afterwards.".into(),
            input: Input::new(vec![
                ("x", point("Starting horizontal position")),
                ("y", point("Starting vertical position")),
                ("toX", point("Ending horizontal position")),
                ("toY", point("Ending vertical position")),
                ("durationMs", number().min(50).max(MAX_SWIPE_MS).optional().describe("How long the drag takes. Defaults to 300; shorter flings further.")),
            ]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(SIMULATOR_ERROR, async {
                        let mut input = json!({ "kind": "swipe", "x": args.get("x"), "y": args.get("y"), "toX": args.get("toX"), "toY": args.get("toY"), "points": true });
                        if let Some(duration) = args.get("durationMs") {
                            input["durationMs"] = duration.clone();
                        }
                        act(bridge.as_ref(), input).await
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "simulator_type",
            description: "Type text into whatever field has the keyboard in the simulator; tap the field first. Characters a US keyboard lacks, such as emoji or accents, are pasted through the device's pasteboard. Answers with the elements on screen afterwards.".into(),
            input: Input::new(vec![("text", string().min(1).max(MAX_SIMULATOR_TEXT - 1).describe("The text to type.")), ("submit", boolean().optional().describe("Press Return afterwards."))]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(SIMULATOR_ERROR, async {
                        let mut text = args.text("text").unwrap_or_default();
                        if args.flag("submit") == Some(true) {
                            text.push('\n');
                        }
                        act(bridge.as_ref(), json!({ "kind": "type", "text": text })).await
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "simulator_button",
            description: "Press a hardware button on the simulator: home goes to the home screen, lock locks or wakes it. Answers with the elements on screen afterwards.".into(),
            input: Input::new(vec![("button", enumeration(&["home", "lock"]))]),
            read_only: false,
            run: |bridge, args| async move { report(SIMULATOR_ERROR, act(bridge.as_ref(), json!({ "kind": "button", "button": args.text("button") }))).await }.boxed(),
        },
        ToolDefinition {
            name: "simulator_rotate",
            description: "Turn the simulator to portrait or landscape, to check how the app lays out each way. Answers with the elements on screen afterwards, measured the way the app now draws them.".into(),
            input: Input::new(vec![("orientation", enumeration(&SIMULATOR_ORIENTATIONS).describe("How to hold the device. landscape-left has the top of the screen on the left."))]),
            read_only: false,
            run: |bridge, args| async move { report(SIMULATOR_ERROR, act(bridge.as_ref(), json!({ "kind": "rotate", "orientation": args.text("orientation") }))).await }.boxed(),
        },
    ]
}

/// `read_only` keeps only the read, for a run that may look at the device but never drive it.
pub fn bound_simulator_tools(bridge: Arc<dyn SimulatorBridge>, read_only: bool) -> Vec<BoundTool> {
    let mut definitions = simulator_tools();
    if read_only {
        definitions.retain(|tool| tool.read_only);
    }
    bind_tools(bridge, &definitions)
}

const SIMULATOR_INSTRUCTIONS: &str = "To check an iOS app on a simulator, use the AICodingTool simulator tools: simulator_open starts a headless simulator in the Simulator panel and gives its UDID to build, install and launch on with xcodebuild and xcrun simctl; then simulator_read, simulator_tap, simulator_swipe, simulator_type and simulator_rotate drive it. Never open Simulator.app or drive it with computer use: the panel leaves the user's screen, mouse and focus alone.";
const SIMULATOR_READING_INSTRUCTIONS: &str = "Read the iOS simulator in the AICodingTool Simulator panel with simulator_read rather than guessing at it. Starting and driving it are work for a thread.";

pub fn server() -> ToolServer {
    ToolServer {
        name: SIMULATOR_SERVER_NAME,
        definitions: names_of(&simulator_tools()),
        // Reading the simulator's screen changes nothing; booting it and tapping it do.
        grants: Grants::ReadOnly,
        // A coordinator reads the simulator's screen and never taps it.
        serve: |run: &ToolRun| Some(bound_simulator_tools(run.simulator.clone()?, run.coordination_role == Some(CoordinationRole::Coordinator))),
        brief: |offers: &Offers, _, _| {
            if !offers.any() {
                return Vec::new();
            }
            vec![if offers.has("simulator_open") { SIMULATOR_INSTRUCTIONS } else { SIMULATOR_READING_INSTRUCTIONS }.to_string()]
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_indent_by_depth_and_print_whole_points() {
        let element = SimulatorElement { reference: "3".into(), role: "Button".into(), label: Some("Save".into()), value: None, identifier: Some("save".into()), frame: [10.0, 20.5, 100.0, 44.0], depth: 12 };
        assert_eq!(element_line(&element), format!("{}[3] Button \"Save\" #save (10,20.5 100x44)", "  ".repeat(8)));
    }
}
