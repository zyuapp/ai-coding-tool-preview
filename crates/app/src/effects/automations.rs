//! Scheduled runs: the scheduler over the task database, the ticks it fires into the window, and
//! the changes the window asks of it (src/host/automation-effects.ts).

use std::sync::{Arc, OnceLock};

use act_core::domain::automation::{AutomationDraft, AutomationPatch};
use act_core::domain::run::RunStatus;
use act_core::events::AutomationAck;
use act_host::automation::dispatch::AutomationRuns;
use act_host::automation::scheduler::{AutomationScheduler, AutomationStore, RunNow, SchedulerOptions};
use act_host::automation::{Automation, AutomationError, AutomationRunStatus};
use act_store::service::TaskDatabaseService;
use serde_json::{json, Value};

use super::{convert, Host, Sink};

/// The scheduler's store, which is the task database answered inline.
struct DatabaseStore(TaskDatabaseService);

impl AutomationStore for DatabaseStore {
    fn list_automations(&self) -> Result<Vec<Automation>, AutomationError> {
        let stored = self.0.list_automations().wait().map_err(|error| AutomationError::new(error.to_string()))?;
        stored.iter().map(|automation| convert(automation).map_err(AutomationError::new)).collect()
    }

    fn save_automation(&self, automation: &Automation) -> Result<(), AutomationError> {
        let stored = convert(automation).map_err(AutomationError::new)?;
        self.0.save_automation(stored).wait().map_err(|error| AutomationError::new(error.to_string()))
    }

    fn delete_automation(&self, id: &str) -> Result<(), AutomationError> {
        self.0.delete_automation(id.to_string()).wait().map_err(|error| AutomationError::new(error.to_string()))
    }
}

#[derive(Default)]
pub struct Automations {
    scheduler: OnceLock<AutomationScheduler>,
    runs: OnceLock<Arc<AutomationRuns>>,
}

impl Automations {
    /// Starts the scheduler once the database is open, and lists what it holds.
    pub fn start(&self, host: Arc<Host>) {
        let Some(database) = host.database.get() else { return };
        let fire_sink = host.sink.clone();
        let runs = AutomationRuns::new(move |fire| {
            fire_sink.json(json!({ "type": "automation.fired", "fire": fire }));
            true
        });
        let listen_sink = host.sink.clone();
        let options = SchedulerOptions {
            listener: Some(Arc::new(move |views| listen_sink.json(json!({ "type": "automations.changed", "automations": views })))),
            now: None,
        };
        let scheduler = AutomationScheduler::new(Arc::new(DatabaseStore(database.clone())), runs.as_dispatch(), options);
        let _ = self.runs.set(runs);
        if self.scheduler.set(scheduler).is_err() {
            return;
        }
        let sink = host.sink.clone();
        let runner = host.clone();
        runner.spawn(async move {
            let Some(scheduler) = host.automations.scheduler.get() else { return };
            if let Err(error) = scheduler.start().await {
                sink.failed(error.to_string());
            }
            let views = scheduler.list();
            sink.json(json!({ "type": "automations.changed", "automations": views }));
        });
    }

    /// The scheduler once the database is open.
    pub fn scheduler_now(&self) -> Option<AutomationScheduler> {
        self.scheduler.get().cloned()
    }

    fn scheduler(&self, sink: &Sink) -> Option<&AutomationScheduler> {
        let scheduler = self.scheduler.get();
        if scheduler.is_none() {
            sink.failed("Automation scheduler is not ready.");
        }
        scheduler
    }

    pub async fn save(&self, sink: &Sink, task_id: String, draft: AutomationDraft) {
        let Some(scheduler) = self.scheduler(sink) else { return };
        let mut value = serde_json::to_value(&draft).unwrap_or(Value::Null);
        value["taskId"] = json!(task_id);
        match serde_json::from_value(value) {
            Ok(draft) => {
                if let Err(error) = scheduler.save(draft, None).await {
                    sink.failed(error.to_string());
                }
            }
            Err(error) => sink.failed(error.to_string()),
        }
    }

    pub async fn update(&self, sink: &Sink, task_id: String, patch: AutomationPatch) {
        let Some(scheduler) = self.scheduler(sink) else { return };
        match convert(&patch) {
            Ok(patch) => {
                if let Err(error) = scheduler.update(&task_id, patch, None).await {
                    sink.failed(error.to_string());
                }
            }
            Err(error) => sink.failed(error),
        }
    }

    pub async fn delete(&self, sink: &Sink, task_id: String) {
        let Some(scheduler) = self.scheduler(sink) else { return };
        if let Err(error) = scheduler.remove(&task_id).await {
            sink.failed(error.to_string());
        }
    }

    pub async fn run_now(&self, sink: &Sink, task_id: String) {
        let Some(scheduler) = self.scheduler(sink) else { return };
        match scheduler.run_now(&task_id).await {
            Ok(RunNow::Busy) | Ok(RunNow::Ran(AutomationRunStatus::Skipped)) => sink.failed(act_core::reducer::errors::BUSY_AUTOMATION_ERROR),
            Ok(_) => {}
            Err(error) => sink.failed(error.to_string()),
        }
    }

    pub fn acknowledge(&self, ack: AutomationAck) {
        if let Some(runs) = self.runs.get() {
            runs.acknowledge(&ack.run_id, ack.started);
        }
    }

    /// A run reached its end, which a tick may be waiting on.
    pub fn settle(&self, run_id: &str, status: RunStatus) {
        let Some(runs) = self.runs.get() else { return };
        if let Ok(status) = convert::<_, AutomationRunStatus>(&status) {
            runs.settle(run_id, status);
        }
    }
}
