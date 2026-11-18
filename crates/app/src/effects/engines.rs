//! The engines a run can take, their sign-in and updates, and what their plans have left
//! (src/host/engine-effects.ts).

use std::sync::Arc;

use act_core::domain::engine::{AgentEngine, EngineStatus};
use act_core::events::WorkspaceEvent;
use act_engines::engines::EngineAccessHost;
use futures::FutureExt;
use serde_json::json;

use super::Sink;

pub struct Engines {
    access: EngineAccessHost,
}

impl Engines {
    pub fn new() -> Engines {
        let search_again = Arc::new(|| act_platform::search_path::adopt_login_shell_path().boxed());
        Engines { access: EngineAccessHost::new(search_again) }
    }

    pub async fn read(&self, sink: &Sink, refresh: bool) {
        let status = self.access.read(refresh).await;
        report_status(sink, status);
    }

    pub async fn sign_in(&self, sink: &Sink, engine: AgentEngine) {
        if !engine.is_known() {
            return;
        }
        let open: act_engines::engines::OpenUrl = Box::new(|url: String| {
            async move {
                act_host::open::open_url(&url);
                Ok(())
            }
            .boxed()
        });
        let status = self.access.sign_in(engine, open).await;
        report_status(sink, status);
    }

    pub async fn update(&self, sink: &Sink, engine: AgentEngine) {
        if !engine.is_known() {
            return;
        }
        let (status, error) = self.access.update(engine).await;
        match error {
            Some(message) => sink.event(WorkspaceEvent::EngineUpdateFailed { engine, message }),
            None => sink.event(WorkspaceEvent::EngineUpdated { engine, status }),
        }
    }
}

fn report_status(sink: &Sink, status: EngineStatus) {
    sink.event(WorkspaceEvent::EngineStatus { status });
}

pub async fn plan_usage(sink: &Sink, engine: AgentEngine, read: i64) {
    let Some(runs) = act_engines::engines::engine(engine) else { return };
    let usage = runs.plan_usage().await;
    sink.json(json!({ "type": "usage.reported", "engine": engine, "read": read, "usage": usage }));
}
