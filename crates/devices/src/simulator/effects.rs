//! The reducer's simulator effects, performed on the host and answered as workspace inputs
//! (simulator-effects.ts).

use std::sync::Arc;
use std::time::Duration;

use act_core::domain::platform::{SimulatorAvailability, SimulatorInput};
use act_core::effects::EffectWork;
use act_core::events::{WorkspaceEvent, WorkspaceInput};
use act_core::AppCommand;

use super::host::SimulatorHost;

/// How long a run counts as driving a device after its last gesture.
pub const DRIVING: Duration = Duration::from_millis(4_000);

/// Sends an input back to the window's reducer, from any thread.
pub type Dispatch = Arc<dyn Fn(WorkspaceInput) + Send + Sync>;

/// Performs a `simulator.*` effect. Returns false for any other effect, which it leaves alone.
pub fn perform(host: &SimulatorHost, work: &EffectWork, dispatch: Dispatch) -> bool {
    let host = host.clone();
    let runtime = act_host::runtime();
    match work.clone() {
        EffectWork::SimulatorList => {
            runtime.spawn(async move {
                let event = match host.list().await {
                    Ok((availability, devices)) => WorkspaceEvent::SimulatorListed { availability, devices },
                    Err(reason) => WorkspaceEvent::SimulatorListed { availability: SimulatorAvailability::Unsupported { reason }, devices: Vec::new() },
                };
                dispatch(event.into());
            });
        }
        EffectWork::SimulatorBoot { udid } => {
            runtime.spawn(async move {
                let event = match host.boot(&udid).await {
                    Ok(()) => WorkspaceEvent::SimulatorBooted { udid },
                    Err(message) => WorkspaceEvent::SimulatorFailed { udid, message },
                };
                dispatch(event.into());
            });
        }
        EffectWork::SimulatorRelease { udid } => {
            runtime.spawn(async move {
                host.shutdown(&udid).await;
                dispatch(AppCommand::SimulatorRefresh.into());
            });
        }
        EffectWork::SimulatorInput { udid, input, seq, .. } => {
            // Queued now, so inputs reach the device in the order the reducer asked for them.
            let sent = host.input(&udid, input.clone());
            runtime.spawn(async move {
                match sent.await {
                    Ok(()) => {
                        if let SimulatorInput::Rotate { orientation } = input {
                            dispatch(WorkspaceEvent::SimulatorTurned { udid, orientation }.into());
                        }
                    }
                    Err(message) => dispatch(WorkspaceEvent::SimulatorRefused { message }.into()),
                }
                if let Some(seq) = seq {
                    tokio::time::sleep(DRIVING).await;
                    dispatch(WorkspaceEvent::SimulatorIdle { seq }.into());
                }
            });
        }
        _ => return false,
    }
    true
}

/// What the host does once a window starts: read the devices this Mac has.
pub fn subscribe(dispatch: &Dispatch) {
    dispatch(AppCommand::SimulatorRefresh.into());
}
