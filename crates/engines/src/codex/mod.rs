//! Codex, driven over its app-server JSON-RPC protocol.

pub mod app_server;
pub mod config;
pub mod home;
pub mod home_config;
pub mod images;
pub mod plan_usage;
pub mod policy;
pub mod provider;
pub mod questions;
pub mod server;
pub mod services;
pub mod session;
pub mod shared_auth;
pub mod skills;
pub mod spend;
pub mod subagents;
pub mod thread_record;

pub use provider::{CodexAgentProvider, CodexProviderOptions};
pub use session::{CodexSession, CODEX_DIALECT};

use std::sync::Arc;

use futures::future::{BoxFuture, FutureExt};

use crate::engines::{Engine, OpenUrl};
use crate::provider::AgentProvider;
use crate::session_pool::SessionPool;
use crate::tools::ToolHost;
use crate::types::{AvailableCommand, EngineAccess, EngineReadiness, PlanUsage, SubagentMetadata};

pub struct CodexEngine;

impl Engine for CodexEngine {
    fn provider(
        &self,
        pool: SessionPool,
        host: Option<Arc<dyn ToolHost>>,
        generated_images: Option<String>,
    ) -> Arc<dyn AgentProvider> {
        Arc::new(CodexAgentProvider::new(
            pool,
            CodexProviderOptions::new(host, generated_images),
        ))
    }

    fn readiness(&self) -> BoxFuture<'static, EngineReadiness> {
        services::read_codex_readiness().boxed()
    }

    fn update(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        services::update_codex().boxed()
    }

    fn commands<'a>(
        &'a self,
        workspace_root: &'a str,
        _projectless: bool,
    ) -> BoxFuture<'a, anyhow::Result<Vec<AvailableCommand>>> {
        services::codex_commands(workspace_root).boxed()
    }

    fn suggest_title<'a>(
        &'a self,
        text: &'a str,
        images: &'a [String],
        _model: Option<&'a str>,
    ) -> BoxFuture<'a, Option<String>> {
        services::suggest_codex_title(text, images).boxed()
    }

    fn plan_usage(&self) -> BoxFuture<'static, PlanUsage> {
        plan_usage::read_codex_plan_usage().boxed()
    }

    fn sign_in(&self, open_url: OpenUrl) -> BoxFuture<'static, anyhow::Result<EngineAccess>> {
        services::sign_in_to_codex(open_url).boxed()
    }

    fn subagent_metadata<'a>(
        &'a self,
        id: &'a str,
        _session_id: Option<&'a str>,
    ) -> BoxFuture<'a, Option<SubagentMetadata>> {
        services::read_codex_subagent_metadata(id).map(Some).boxed()
    }
}
