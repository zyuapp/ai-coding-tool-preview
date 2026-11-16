//! What Codex keeps on disk about a thread, as this app leaves it: the title it carries here and the
//! checkout its work belongs to. Codex writes the rollout these are recorded against at the first
//! turn, so everything is held until `began`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use super::app_server::{client_info, AppServerClient, AppServerError};
use super::server::{codex_app_server, ServerOptions};

/// Where a thread's work belongs: origin URL, branch and commit, each when known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitOrigin {
    pub origin_url: Option<String>,
    pub branch: Option<String>,
    pub sha: Option<String>,
}

pub type ReadOrigin = Arc<dyn Fn(String) -> BoxFuture<'static, GitOrigin> + Send + Sync>;

async fn git(root: &str, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !text.is_empty()).then_some(text)
}

/// Reads the checkout a thread runs in with git.
pub fn git_origin() -> ReadOrigin {
    Arc::new(|root| {
        Box::pin(async move {
            let (origin_url, branch, sha) = tokio::join!(
                git(&root, &["remote", "get-url", "origin"]),
                git(&root, &["symbolic-ref", "--short", "-q", "HEAD"]),
                git(&root, &["rev-parse", "HEAD"])
            );
            GitOrigin {
                origin_url,
                branch,
                sha,
            }
        })
    })
}

#[derive(Default)]
struct State {
    client: Option<AppServerClient>,
    thread_id: Option<String>,
    root: Option<String>,
    recorded: bool,
    stamped: bool,
    pending: Option<String>,
    written: Option<String>,
}

#[derive(Clone)]
pub struct CodexThreadRecord {
    state: Arc<Mutex<State>>,
    read_origin: ReadOrigin,
}

impl CodexThreadRecord {
    pub fn new(read_origin: ReadOrigin) -> Self {
        CodexThreadRecord {
            state: Arc::default(),
            read_origin,
        }
    }

    /// The thread now exists on the server, though Codex may still have nothing on disk for it.
    pub fn opened(&self, client: AppServerClient, thread_id: &str, root: &str) {
        let mut state = self.state.lock().unwrap();
        state.client = Some(client);
        state.thread_id = Some(thread_id.to_string());
        state.root = Some(root.to_string());
    }

    /// Codex has written the rollout, which the requests below are recorded against.
    pub fn began(&self) {
        {
            let mut state = self.state.lock().unwrap();
            if state.recorded {
                return;
            }
            state.recorded = true;
        }
        self.write();
        self.stamp();
    }

    /// Names the thread in Codex's history. A title that lands before the rollout waits for it.
    pub fn label(&self, title: &str) {
        {
            let mut state = self.state.lock().unwrap();
            if state.written.as_deref() == Some(title) {
                return;
            }
            state.pending = Some(title.to_string());
        }
        self.write();
    }

    fn write(&self) {
        let (client, thread_id, title) = {
            let mut state = self.state.lock().unwrap();
            let (Some(client), Some(thread_id)) = (state.client.clone(), state.thread_id.clone())
            else {
                return;
            };
            if !state.recorded {
                return;
            }
            let Some(title) = state.pending.take() else {
                return;
            };
            state.written = Some(title.clone());
            (client, thread_id, title)
        };
        let shared = self.state.clone();
        tokio::spawn(async move {
            if client
                .request(
                    "thread/name/set",
                    json!({ "threadId": thread_id, "name": title }),
                )
                .await
                .is_err()
            {
                let mut state = shared.lock().unwrap();
                if state.written.as_deref() == Some(title.as_str()) {
                    state.written = None;
                }
            }
        });
    }

    /// Records where the work belongs, once: the checkout does not move under a thread.
    fn stamp(&self) {
        let (client, thread_id, root) = {
            let mut state = self.state.lock().unwrap();
            let (Some(client), Some(thread_id), Some(root)) = (
                state.client.clone(),
                state.thread_id.clone(),
                state.root.clone(),
            ) else {
                return;
            };
            if state.stamped {
                return;
            }
            state.stamped = true;
            (client, thread_id, root)
        };
        let read_origin = self.read_origin.clone();
        tokio::spawn(async move {
            let origin = read_origin(root).await;
            if origin.origin_url.is_none() && origin.branch.is_none() && origin.sha.is_none() {
                return;
            }
            let _ = client.request("thread/metadata/update", json!({ "threadId": thread_id, "gitInfo": { "originUrl": origin.origin_url, "branch": origin.branch, "sha": origin.sha } })).await;
        });
    }
}

/// Continues a thread from disk. A thread archived in another Codex client is unarchived once
/// before the refusal is passed on.
pub async fn resume_thread(
    client: &AppServerClient,
    thread_id: &str,
    settings: &Value,
) -> Result<Value, AppServerError> {
    let mut params = settings.clone();
    params["threadId"] = json!(thread_id);
    params["excludeTurns"] = json!(true);
    match client.request("thread/resume", params.clone()).await {
        Ok(resumed) => Ok(resumed),
        Err(error @ AppServerError::Rpc { .. }) => {
            if client
                .request("thread/unarchive", json!({ "threadId": thread_id }))
                .await
                .is_err()
            {
                return Err(error);
            }
            client.request("thread/resume", params).await
        }
        Err(error) => Err(error),
    }
}

const FILE_TIMEOUT: Duration = Duration::from_secs(60);

/// Files a thread away in Codex's history, or brings it back, on a server of its own. Codex refuses
/// while another process still writes the thread, so its session has to be gone first.
pub async fn file_thread(thread_id: &str, archived: bool) -> anyhow::Result<()> {
    let command = codex_app_server(
        &[
            "--disable",
            "plugins",
            "--disable",
            "apps",
            "--disable",
            "hooks",
        ]
        .map(String::from),
        ServerOptions::default(),
    )
    .await?;
    let client = AppServerClient::connect(command);
    let filing = async {
        client.initialize(client_info()).await?;
        client
            .request(
                if archived {
                    "thread/archive"
                } else {
                    "thread/unarchive"
                },
                json!({ "threadId": thread_id }),
            )
            .await?;
        Ok::<_, anyhow::Error>(())
    };
    let result = match tokio::time::timeout(FILE_TIMEOUT, filing).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "Codex did not file thread {thread_id} away in time."
        )),
    };
    client.close().await;
    result
}
