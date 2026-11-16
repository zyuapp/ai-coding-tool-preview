//! The engines a run can take, their sign-in and updates, and what their plans have left
//! (src/host/engine-effects.ts).

use std::sync::Arc;

use act_core::domain::engine::AgentEngine;
use act_core::events::WorkspaceEvent;
use act_engines::engines::EngineAccessHost;
use futures::FutureExt;
use serde_json::json;

use super::{convert, Sink};

pub struct Engines {
    access: EngineAccessHost,
}

impl Engines {
    pub fn new() -> Engines {
        let search_again = Arc::new(|| act_host::login_path::adopt_login_shell_path().boxed());
        Engines { access: EngineAccessHost::new(search_again) }
    }

    pub async fn read(&self, sink: &Sink, refresh: bool) {
        let status = self.access.read(refresh).await;
        report_status(sink, &status);
    }

    pub async fn sign_in(&self, sink: &Sink, engine: AgentEngine) {
        let Ok(engine) = convert(&engine) else { return };
        let open: act_engines::engines::OpenUrl = Box::new(|url: String| {
            async move {
                act_host::open::open_url(&url);
                Ok(())
            }
            .boxed()
        });
        let status = self.access.sign_in(engine, open).await;
        report_status(sink, &status);
    }

    pub async fn update(&self, sink: &Sink, engine: AgentEngine) {
        let Ok(engine_id) = convert(&engine) else { return };
        let (status, error) = self.access.update(engine_id).await;
        match error {
            Some(message) => sink.event(WorkspaceEvent::EngineUpdateFailed { engine, message }),
            None => sink.json(json!({ "type": "engine.updated", "engine": engine, "status": status })),
        }
    }
}

fn report_status<T: serde::Serialize>(sink: &Sink, status: &T) {
    sink.json(json!({ "type": "engine.status", "status": status }));
}

pub async fn plan_usage(sink: &Sink, engine: AgentEngine, read: i64) {
    let Ok(engine_id) = convert(&engine) else { return };
    let usage = act_engines::engines::plan_usage(engine_id).await;
    sink.json(json!({ "type": "usage.reported", "engine": engine, "read": read, "usage": usage }));
}
