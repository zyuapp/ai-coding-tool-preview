use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{json, Value};

use crate::pooled_provider::{PooledProvider, SessionEngine};
use crate::provider::RunInput;
use crate::session_pool::{SessionHooks, SessionPool};
use crate::tools::{grants_tool, ToolHost, ToolReach};
use crate::types::{handle_for, AgentEngine, ComputerUseRunConfig, Continuation, RunOperation};

use super::app_server::{connect_app_server, CodexConnect};
use super::images::codex_image_output;
use super::session::{codex_brief, CodexSession, ImageOutput};
use super::thread_record::{file_thread, git_origin, ReadOrigin};

/// Everything a session is built with. App tools and computer use are granted unasked per process,
/// so what decides those grants counts too.
fn session_key(input: &RunInput) -> String {
    let mut identity = codex_brief(input)
        .identity
        .as_array()
        .cloned()
        .unwrap_or_default();
    identity.push(json!(
        matches!(input.computer_use, ComputerUseRunConfig::Available { .. })
            && grants_tool(ToolReach::ComputerUse, Some(input.scope()))
    ));
    identity.push(json!(grants_tool(
        ToolReach::Workspace,
        Some(input.scope())
    )));
    identity.push(match input.operation {
        Some(RunOperation::Review { .. }) => json!([input.model, input.effort, input.policy]),
        _ => Value::Null,
    });
    Value::Array(identity).to_string()
}

#[derive(Clone)]
pub struct CodexProviderOptions {
    pub connect: CodexConnect,
    pub read_origin: ReadOrigin,
    pub image_output: ImageOutput,
    /// Serves the app's tools to every session.
    pub host: Option<Arc<dyn ToolHost>>,
}

impl CodexProviderOptions {
    /// The real app server, git, and generated images saved under `generated_images`.
    pub fn new(host: Option<Arc<dyn ToolHost>>, generated_images: Option<String>) -> Self {
        CodexProviderOptions {
            connect: connect_app_server(),
            read_origin: git_origin(),
            image_output: Arc::new(move |item, root| {
                let directory = generated_images.clone();
                async move { codex_image_output(&item, &root, directory.as_deref()).await }.boxed()
            }),
            host,
        }
    }
}

/// How Codex opens a thread's session: one warm app server per thread.
pub struct CodexSessions {
    options: CodexProviderOptions,
}

/// Runs threads on Codex app servers.
pub type CodexAgentProvider = PooledProvider<CodexSessions>;

impl PooledProvider<CodexSessions> {
    pub fn new(pool: SessionPool, options: CodexProviderOptions) -> Self {
        PooledProvider::with_engine(pool, CodexSessions { options })
    }
}

impl SessionEngine for CodexSessions {
    type Session = CodexSession;

    fn session_key(&self, input: &RunInput) -> String {
        session_key(input)
    }

    fn open(&self, key: String, hooks: SessionHooks) -> Arc<CodexSession> {
        let options = &self.options;
        CodexSession::new(
            key,
            options.connect.clone(),
            options.host.clone(),
            hooks,
            options.read_origin.clone(),
            options.image_output.clone(),
        )
    }

    fn stop_process(session: &Arc<CodexSession>, process_id: &str) -> bool {
        session.stop_process(process_id);
        true
    }

    fn label(session: &Arc<CodexSession>, title: &str) -> bool {
        session.label(title);
        true
    }

    /// Files the thread away in Codex's own history too, or brings it back. Archiving waits out the
    /// thread's session, which holds the thread open until its cancelled turn has answered.
    fn archive_thread(
        provider: &CodexAgentProvider,
        task_id: &str,
        continuation: Option<Continuation>,
        archived: bool,
    ) -> bool {
        let handle = handle_for(continuation.as_ref(), AgentEngine::Codex).map(String::from);
        if continuation.is_some() && handle.is_none() {
            return false;
        }
        let session = provider.session(task_id);
        if handle.is_none() && !(archived && session.is_some()) {
            return false;
        }
        let pool = provider.pool().clone();
        let task = task_id.to_string();
        provider.file(
            task_id,
            async move {
                if archived {
                    pool.retire(&task).await;
                }
                if let Some(thread_id) =
                    handle.or_else(|| session.and_then(|session| session.thread_id()))
                {
                    file_thread(&thread_id, archived).await?;
                }
                Ok(())
            }
            .boxed(),
        );
        true
    }
}
