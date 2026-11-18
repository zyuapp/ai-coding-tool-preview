//! OpenCode, driven over the HTTP API and event feed of a private `opencode serve`.

pub mod catalog;
pub mod policy;
pub mod provider;
pub mod server;
pub mod services;
pub mod session;

pub use provider::OpenCodeAgentProvider;
pub use session::{OpenCodeSession, OPENCODE_DIALECT};

use std::sync::Arc;

use futures::future::{BoxFuture, FutureExt};

use crate::engines::Engine;
use crate::provider::AgentProvider;
use crate::session_pool::SessionPool;
use crate::tools::ToolHost;
use crate::types::{AvailableCommand, EngineReadiness, PlanUsage};

pub struct OpenCodeEngine;

impl Engine for OpenCodeEngine {
    fn provider(
        &self,
        pool: SessionPool,
        host: Option<Arc<dyn ToolHost>>,
        _generated_images: Option<String>,
    ) -> Arc<dyn AgentProvider> {
        Arc::new(OpenCodeAgentProvider::new(pool, host))
    }

    fn readiness(&self) -> BoxFuture<'static, EngineReadiness> {
        services::read_open_code_readiness().boxed()
    }

    fn update(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        services::update_open_code().boxed()
    }

    fn commands<'a>(
        &'a self,
        workspace_root: &'a str,
        _projectless: bool,
    ) -> BoxFuture<'a, anyhow::Result<Vec<AvailableCommand>>> {
        async move {
            services::open_code_services()
                .commands(workspace_root)
                .await
        }
        .boxed()
    }

    fn suggest_title<'a>(
        &'a self,
        text: &'a str,
        _images: &'a [String],
        model: Option<&'a str>,
    ) -> BoxFuture<'a, Option<String>> {
        async move { services::open_code_services().title(text, model).await }.boxed()
    }

    fn plan_usage(&self) -> BoxFuture<'static, PlanUsage> {
        async { services::open_code_services().plan_usage().await }.boxed()
    }

    fn close(&self) -> BoxFuture<'static, ()> {
        async { services::open_code_services().close().await }.boxed()
    }
}
