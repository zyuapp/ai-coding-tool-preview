use std::sync::Arc;

use futures::future::{BoxFuture, FutureExt};
use serde_json::json;

use crate::provider::{AgentProvider, ProviderResult, RunInput};
use crate::session_pool::{session_of, Filings, SessionOpener, SessionPool};
use crate::tools::ToolHost;
use crate::types::{handle_for, AgentEngine, Continuation, CoordinationRole};

use super::server::{connect_open_code, OpenCodeConnect, ServerOptions};
use super::session::{open_code_brief, OpenCodeSession};

/// Everything a session's server is started with. Model, effort, mode and grants are the session's
/// own to change, so only the instructions and the tools offered count.
fn session_key(input: &RunInput) -> String {
    open_code_brief(input).identity.to_string()
}

/// Runs threads on OpenCode servers, one per thread's session.
#[derive(Clone)]
pub struct OpenCodeAgentProvider {
    pool: SessionPool,
    filings: Filings,
    connect: OpenCodeConnect,
    host: Option<Arc<dyn ToolHost>>,
}

impl OpenCodeAgentProvider {
    pub fn new(pool: SessionPool, host: Option<Arc<dyn ToolHost>>) -> Self {
        OpenCodeAgentProvider {
            pool,
            filings: Filings::default(),
            connect: connect_open_code(),
            host,
        }
    }

    pub fn with_connect(
        pool: SessionPool,
        host: Option<Arc<dyn ToolHost>>,
        connect: OpenCodeConnect,
    ) -> Self {
        OpenCodeAgentProvider {
            pool,
            filings: Filings::default(),
            connect,
            host,
        }
    }

    fn session(&self, task_id: &str) -> Option<Arc<OpenCodeSession>> {
        session_of::<OpenCodeSession>(&self.pool, task_id)
    }
}

impl AgentProvider for OpenCodeAgentProvider {
    fn execute(&self, input: RunInput) -> BoxFuture<'static, ProviderResult> {
        let key = session_key(&input);
        let (pool, connect, host) = (self.pool.clone(), self.connect.clone(), self.host.clone());
        let filing = self.filings.pending(&input.task_id);
        async move {
            if let Some(filing) = filing {
                filing.await;
            }
            let session_key = key.clone();
            pool.execute(
                input,
                key,
                SessionOpener {
                    open: move |hooks| OpenCodeSession::new(session_key, connect, host, hooks),
                    start: |_: &Arc<OpenCodeSession>| {},
                },
            )
            .await
        }
        .boxed()
    }

    /// OpenCode's own shells are not listed to the app, so there is nothing of theirs to stop.
    fn stop_process(&self, _task_id: &str, _process_id: &str) -> bool {
        false
    }

    fn label_thread(&self, task_id: &str, title: &str) -> bool {
        match self.session(task_id) {
            Some(session) => {
                session.label(title);
                true
            }
            None => false,
        }
    }

    /// Files the session away in OpenCode's own history too, or brings it back: on the thread's own
    /// server while it has one, else on one started for that alone.
    fn archive_thread(
        &self,
        task_id: &str,
        continuation: Option<Continuation>,
        archived: bool,
    ) -> bool {
        let handle = handle_for(continuation.as_ref(), AgentEngine::Opencode).map(String::from);
        if continuation.is_some() && handle.is_none() {
            return false;
        }
        let session = self.session(task_id);
        let Some(session_id) =
            handle.or_else(|| session.as_ref().and_then(|session| session.session_id()))
        else {
            return false;
        };
        let (pool, connect, task) = (self.pool.clone(), self.connect.clone(), task_id.to_string());
        self.filings.file(
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

    fn retire_out_of_role(&self, task_id: &str, role: Option<CoordinationRole>) {
        tokio::spawn(self.pool.retire_out_of_role(task_id, role));
    }

    fn close_all(&self) {
        self.pool.close_all();
    }
}
