//! A tick's hand-off to the window, which owns the transcript: fire, wait for the window to say the
//! run started, then wait for that run to settle.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::{Automation, AutomationRunStatus, ExecutionPolicy, TickKind};

/// How long the window has to say whether the run started.
pub const AUTOMATION_ACK_TIMEOUT: Duration = Duration::from_secs(30);
/// Far longer than any honest scheduled run, so only a run that never reports back reaches it.
pub const AUTOMATION_SETTLE_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

/// The tick as the window receives it. The scheduler owns the run ID so it can correlate the run back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationFire {
    pub automation_id: String,
    pub task_id: String,
    pub run_id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ExecutionPolicy>,
    pub run_number: u64,
    /// The tick may settle without surfacing, if the run says it looked and found nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<bool>,
    /// Nobody is watching a cron tick, so its run may answer its own approvals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unattended: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_when: Option<String>,
}

/// The sentence only travels with a tick that may act on it: a watched run is framed as any other.
pub fn automation_fire(automation: &Automation, run_id: &str, tick: TickKind) -> AutomationFire {
    AutomationFire {
        automation_id: automation.id.clone(),
        task_id: automation.task_id.clone(),
        run_id: run_id.to_string(),
        prompt: automation.prompt.clone(),
        policy: automation.policy,
        run_number: automation.run_count + 1,
        quiet: tick.quiet.then_some(true),
        unattended: tick.unattended.then_some(true),
        surface_when: if tick.quiet { automation.surface_when.clone() } else { None },
        ends_when: automation.ends_when.clone(),
    }
}

/// The window's answer to a fired tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationAck {
    pub automation_id: String,
    pub run_id: String,
    pub started: bool,
}

#[derive(Default)]
struct Pending {
    acknowledge: Option<oneshot::Sender<bool>>,
    settle: Option<oneshot::Sender<AutomationRunStatus>>,
}

/// Hands ticks to the window. `fire` delivers one and answers false when nothing could take it.
pub struct AutomationRuns {
    fire: Box<dyn Fn(AutomationFire) -> bool + Send + Sync>,
    pending: Mutex<HashMap<String, Pending>>,
}

impl AutomationRuns {
    pub fn new(fire: impl Fn(AutomationFire) -> bool + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { fire: Box::new(fire), pending: Mutex::new(HashMap::new()) })
    }

    /// Fires the tick, then waits for the run it started to settle. A tick the window declined, or never
    /// answered, is skipped; a run that never settles is called failed.
    pub async fn dispatch(&self, automation: Automation, tick: TickKind) -> AutomationRunStatus {
        self.dispatch_within(automation, tick, AUTOMATION_ACK_TIMEOUT, AUTOMATION_SETTLE_TIMEOUT).await
    }

    async fn dispatch_within(&self, automation: Automation, tick: TickKind, ack_timeout: Duration, settle_timeout: Duration) -> AutomationRunStatus {
        let run_id = uuid::Uuid::new_v4().to_string();
        let (acknowledge, acknowledged) = oneshot::channel();
        let (settle, settled) = oneshot::channel();
        // Armed before the tick leaves so a run that settles immediately still reports back.
        self.pending.lock().unwrap().insert(run_id.clone(), Pending { acknowledge: Some(acknowledge), settle: Some(settle) });
        let status = async {
            if !(self.fire)(automation_fire(&automation, &run_id, tick)) {
                return AutomationRunStatus::Skipped;
            }
            let started = matches!(tokio::time::timeout(ack_timeout, acknowledged).await, Ok(Ok(true)));
            if !started {
                return AutomationRunStatus::Skipped;
            }
            match tokio::time::timeout(settle_timeout, settled).await {
                Ok(Ok(status)) => status,
                _ => AutomationRunStatus::Failed,
            }
        }
        .await;
        self.pending.lock().unwrap().remove(&run_id);
        status
    }

    /// The window's answer to a fired tick: whether it started a run for it.
    pub fn acknowledge(&self, run_id: &str, started: bool) {
        if let Some(sender) = self.pending.lock().unwrap().get_mut(run_id).and_then(|pending| pending.acknowledge.take()) {
            let _ = sender.send(started);
        }
    }

    /// A run reached a terminal status. Runs no tick is waiting on are ignored.
    pub fn settle(&self, run_id: &str, status: AutomationRunStatus) {
        if let Some(sender) = self.pending.lock().unwrap().get_mut(run_id).and_then(|pending| pending.settle.take()) {
            let _ = sender.send(status);
        }
    }

    /// The scheduler's dispatch, bound to this hand-off.
    pub fn as_dispatch(self: &Arc<Self>) -> super::scheduler::AutomationDispatch {
        let runs = self.clone();
        Arc::new(move |automation, tick| {
            let runs = runs.clone();
            Box::pin(async move { runs.dispatch(automation, tick).await })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn automation() -> Automation {
        serde_json::from_value(serde_json::json!({
            "id": "a", "taskId": "t", "prompt": "p", "schedule": "* * * * *", "paused": false,
            "createdAt": 0, "updatedAt": 0, "runCount": 4, "surfaceWhen": "found"
        }))
        .unwrap()
    }

    #[test]
    fn only_a_quiet_tick_carries_its_sentence() {
        let quiet = automation_fire(&automation(), "r", TickKind { quiet: true, unattended: true });
        assert_eq!((quiet.run_number, quiet.surface_when.as_deref(), quiet.quiet), (5, Some("found"), Some(true)));
        let loud = automation_fire(&automation(), "r", TickKind { quiet: false, unattended: false });
        assert_eq!((loud.surface_when, loud.quiet, loud.unattended), (None, None, None));
    }

    #[tokio::test]
    async fn waits_for_the_acknowledged_run_to_settle() {
        let fired: Arc<Mutex<Option<String>>> = Arc::default();
        let runs = AutomationRuns::new({
            let fired = fired.clone();
            move |fire| {
                *fired.lock().unwrap() = Some(fire.run_id);
                true
            }
        });
        let dispatching = tokio::spawn({
            let runs = runs.clone();
            async move { runs.dispatch(automation(), TickKind { quiet: false, unattended: true }).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let run_id = fired.lock().unwrap().clone().unwrap();
        runs.acknowledge(&run_id, true);
        runs.settle(&run_id, AutomationRunStatus::Cancelled);
        assert_eq!(dispatching.await.unwrap(), AutomationRunStatus::Cancelled);
    }

    #[tokio::test]
    async fn an_unanswered_or_declined_tick_is_skipped() {
        let runs = AutomationRuns::new(|_| true);
        let status = runs.dispatch_within(automation(), TickKind { quiet: false, unattended: true }, Duration::from_millis(20), Duration::from_secs(1)).await;
        assert_eq!(status, AutomationRunStatus::Skipped);
        let refused = AutomationRuns::new(|_| false);
        assert_eq!(refused.dispatch(automation(), TickKind { quiet: false, unattended: true }).await, AutomationRunStatus::Skipped);
    }
}
