//! Claude Code, driven directly over its stream-json protocol.

pub mod provider;
pub mod query;
pub mod services;
pub mod session;

pub use provider::{ClaudeAgentProvider, CLAUDE_DIALECT};
pub use query::{ClaudeOptions, ClaudeQuery, QueryOutput};
pub use session::ClaudeSession;

use std::sync::Arc;

use futures::future::{BoxFuture, FutureExt};

use crate::engines::Engine;
use crate::provider::AgentProvider;
use crate::session_pool::SessionPool;
use crate::tools::ToolHost;
use crate::types::{AvailableCommand, EngineReadiness, PlanUsage, SubagentMetadata};

pub struct ClaudeEngine;

impl Engine for ClaudeEngine {
    fn provider(
        &self,
        pool: SessionPool,
        _host: Option<Arc<dyn ToolHost>>,
        _generated_images: Option<String>,
    ) -> Arc<dyn AgentProvider> {
        Arc::new(ClaudeAgentProvider::new(pool))
    }

    fn readiness(&self) -> BoxFuture<'static, EngineReadiness> {
        services::read_claude_readiness().boxed()
    }

    fn update(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        services::update_claude().boxed()
    }

    fn commands<'a>(
        &'a self,
        workspace_root: &'a str,
        projectless: bool,
    ) -> BoxFuture<'a, anyhow::Result<Vec<AvailableCommand>>> {
        services::discover_claude_commands(workspace_root, projectless).boxed()
    }

    fn suggest_title<'a>(
        &'a self,
        text: &'a str,
        images: &'a [String],
        _model: Option<&'a str>,
    ) -> BoxFuture<'a, Option<String>> {
        services::suggest_task_title(text, images).boxed()
    }

    fn plan_usage(&self) -> BoxFuture<'static, PlanUsage> {
        services::read_plan_usage().boxed()
    }

    fn subagent_metadata<'a>(
        &'a self,
        id: &'a str,
        session_id: Option<&'a str>,
    ) -> BoxFuture<'a, Option<SubagentMetadata>> {
        services::read_claude_subagent_metadata(id, session_id)
            .map(Some)
            .boxed()
    }
}
