use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::json;

use crate::pooled_provider::{PooledProvider, SessionEngine};
use crate::provider::RunInput;
use crate::session_pool::{SessionHooks, SessionPool};
use crate::tools::ToolHost;
use crate::types::{handle_for, AgentEngine, Continuation};

use super::server::{connect_open_code, OpenCodeConnect, ServerOptions};
use super::session::{open_code_brief, OpenCodeSession};

/// Everything a session's server is started with. Model, effort, mode and grants are the session's
/// own to change, so only the instructions and the tools offered count.
fn session_key(input: &RunInput) -> String {
    open_code_brief(input).identity.to_string()
}

/// How OpenCode opens a thread's session: one server per session.
pub struct OpenCodeSessions {
    connect: OpenCodeConnect,
    host: Option<Arc<dyn ToolHost>>,
}

/// Runs threads on OpenCode servers.
pub type OpenCodeAgentProvider = PooledProvider<OpenCodeSessions>;

impl PooledProvider<OpenCodeSessions> {
    pub fn new(pool: SessionPool, host: Option<Arc<dyn ToolHost>>) -> Self {
        Self::with_connect(pool, host, connect_open_code())
    }

    pub fn with_connect(
        pool: SessionPool,
        host: Option<Arc<dyn ToolHost>>,
        connect: OpenCodeConnect,
    ) -> Self {
        PooledProvider::with_engine(pool, OpenCodeSessions { connect, host })
    }
}

impl SessionEngine for OpenCodeSessions {
    type Session = OpenCodeSession;

    fn session_key(&self, input: &RunInput) -> String {
        session_key(input)
    }

    fn open(&self, key: String, hooks: SessionHooks) -> Arc<OpenCodeSession> {
        OpenCodeSession::new(key, self.connect.clone(), self.host.clone(), hooks)
    }

    fn label(session: &Arc<OpenCodeSession>, title: &str) -> bool {
        session.label(title);
        true
    }

    /// Files the session away in OpenCode's own history too, or brings it back: on the thread's own
    /// server while it has one, else on one started for that alone.
    fn archive_thread(
        provider: &OpenCodeAgentProvider,
        task_id: &str,
        continuation: Option<Continuation>,
        archived: bool,
    ) -> bool {
        let handle = handle_for(continuation.as_ref(), AgentEngine::Opencode).map(String::from);
        if continuation.is_some() && handle.is_none() {
            return false;
        }
        let session = provider.session(task_id);
        let Some(session_id) =
            handle.or_else(|| session.as_ref().and_then(|session| session.session_id()))
        else {
            return false;
        };
        let (pool, connect, task) = (
            provider.pool().clone(),
            provider.engine().connect.clone(),
            task_id.to_string(),
        );
        provider.file(
            task_id,
            async move {
                let filed = match &session {
                    Some(session) => session.file(&session_id, archived).await?,
                    None => false,
                };
                if !filed {
                    let cwd = std::env::current_dir()
                        .map(|dir| dir.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| "/".into());
                    let client = connect(ServerOptions { cwd, config: None }).await?;
                    let result = client
                        .request(
                            "PATCH",
                            &format!("/api/session/{session_id}"),
                            Some(json!({ "archived": archived })),
                            None,
                        )
                        .await;
                    client.close().await;
                    result?;
                }
                if archived {
                    pool.retire(&task).await;
                }
                Ok(())
            }
            .boxed(),
        );
        true
    }
}
