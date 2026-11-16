//! What a run's simulator tools read: the device its own dock shows, where it stands, and its screen
//! (`readSimulator` in thread-requests.ts). The app supplies the state and waits for boots to settle.

use act_core::domain::platform::{SimulatorAvailability, SimulatorDevice, SimulatorDeviceState};
use act_core::state::{dock_for, dock_owner, WorkspaceState};
use act_host::tools::types::{SimulatorDevice as ToolDevice, SimulatorReadResult};

use super::host::SimulatorHost;

/// The device a thread's own dock shows, which is the only one its run drives.
pub fn dock_simulator(state: &WorkspaceState, task_id: &str) -> Option<SimulatorDevice> {
    let udid = dock_for(state, &dock_owner(state, Some(task_id))).simulator_id.clone()?;
    state.simulator.devices.iter().find(|device| device.udid == udid).cloned()
}

fn tool_device(device: &SimulatorDevice) -> ToolDevice {
    let state = match device.state {
        SimulatorDeviceState::Booted => "booted",
        SimulatorDeviceState::Booting => "booting",
        SimulatorDeviceState::Shutdown => "shutdown",
        SimulatorDeviceState::Other => "other",
    };
    ToolDevice { udid: device.udid.clone(), name: device.name.clone(), runtime: device.runtime.clone(), state: state.into() }
}

/// Why a read cannot go ahead, or the device it reads.
pub fn readable(state: &WorkspaceState, task_id: &str) -> Result<SimulatorDevice, Box<SimulatorReadResult>> {
    let refuse = |reason: &str| Box::new(SimulatorReadResult::NoSimulator { reason: reason.into() });
    if let SimulatorAvailability::Unsupported { reason } = &state.simulator.availability {
        return Err(refuse(reason));
    }
    if state.simulator.devices.is_empty() {
        return Err(refuse("This Mac has no iOS simulators. Add one in Xcode."));
    }
    dock_simulator(state, task_id).ok_or_else(|| refuse("Open the simulator first with simulator_open."))
}

/// Whether a status read still waits: the dock's device has a boot that has not answered.
pub fn boot_pending(state: &WorkspaceState, task_id: &str) -> bool {
    dock_simulator(state, task_id).is_some_and(|device| state.simulator.booting.contains(&device.udid))
}

/// Where the dock's device stands, once its boot has settled or the wait ran out.
pub fn status(state: &WorkspaceState, task_id: &str) -> SimulatorReadResult {
    let device = match readable(state, task_id) {
        Ok(device) => device,
        Err(result) => return *result,
    };
    let error = state.simulator.errors.get(&device.udid).cloned();
    SimulatorReadResult::Status { booted: device.state == SimulatorDeviceState::Booted, device: tool_device(&device), error }
}

/// The app on the dock's device's screen, with a screenshot when asked.
pub async fn snapshot(host: &SimulatorHost, device: &SimulatorDevice, screenshot: bool) -> Result<SimulatorReadResult, String> {
    if device.state != SimulatorDeviceState::Booted {
        return Ok(SimulatorReadResult::NoSimulator { reason: format!("{} is not running yet. Call simulator_open and wait for it.", device.name) });
    }
    let describe = host.describe(&device.udid, &device.name);
    let capture = async {
        if screenshot {
            host.capture(&device.udid).await.map(Some)
        } else {
            Ok(None)
        }
    };
    let (snapshot, shot) = futures::join!(describe, capture);
    Ok(SimulatorReadResult::Snapshot { snapshot: snapshot?, shot: shot? })
}
