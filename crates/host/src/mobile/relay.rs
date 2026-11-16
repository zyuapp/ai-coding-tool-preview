//! A phone's requests on their way to the window, which holds workspace state, and the window's
//! answers on their way back. A request the window never answers times out rather than hanging.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

pub const MOBILE_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
/// A recording may take its computer two minutes to write down, and loading the speech model first.
pub const MOBILE_TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(130);
/// Codex starts a server to list its skills, which may take the whole twenty seconds it is given.
pub const MOBILE_COMMANDS_TIMEOUT: Duration = Duration::from_secs(22);

/// One phone request for the window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum MobileOp {
    Snapshot,
    Command { command: Value },
    Query { query: Value },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileRequest {
    pub request_id: String,
    pub session_id: String,
    /// When the server stops waiting for the answer, in epoch milliseconds.
    pub answer_by: i64,
    #[serde(flatten)]
    pub op: MobileOp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileResponse {
    pub request_id: String,
    pub result: Result<Value, String>,
}

/// Hands a request to the window; false when there is no window to hand it to.
pub type SendToWindow = Arc<dyn Fn(MobileRequest) -> bool + Send + Sync>;

pub struct MobileRelay {
    send: SendToWindow,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>,
    /// How long a request, a recording and a command list are each waited on.
    patience: (Duration, Duration, Duration),
}

fn is_view(value: &Value) -> bool {
    value.get("groups").is_some_and(Value::is_array) && value.get("thread").is_some_and(|thread| thread.is_null() || thread.is_object())
}

impl MobileRelay {
    pub fn new(send: SendToWindow) -> Arc<Self> {
        Self::waiting(send, MOBILE_REQUEST_TIMEOUT, MOBILE_TRANSCRIBE_TIMEOUT, MOBILE_COMMANDS_TIMEOUT)
    }

    pub fn waiting(send: SendToWindow, request: Duration, transcribe: Duration, commands: Duration) -> Arc<Self> {
        Arc::new(Self { send, pending: Mutex::new(HashMap::new()), patience: (request, transcribe, commands) })
    }

    pub async fn snapshot(&self, session_id: &str) -> Result<Value, String> {
        let view = self.ask(session_id, MobileOp::Snapshot, self.patience.0).await?;
        if !is_view(&view) {
            return Err("Your computer sent a view this phone could not read.".into());
        }
        Ok(view)
    }

    pub async fn command(&self, session_id: &str, command: Value) -> Result<(), String> {
        self.ask(session_id, MobileOp::Command { command }, self.patience.0).await.map(|_| ())
    }

    /// The window's own answer, passed through as it came.
    pub async fn query(&self, session_id: &str, query: Value) -> Result<Value, String> {
        let patience = match query.get("kind").and_then(Value::as_str) {
            Some("transcribe") => self.patience.1,
            Some("commands") => self.patience.2,
            _ => self.patience.0,
        };
        self.ask(session_id, MobileOp::Query { query }, patience).await
    }

    /// The window's answer to one request. An answer to nothing pending is a late one, and is dropped.
    pub fn answer(&self, response: MobileResponse) {
        if let Some(waiting) = self.pending.lock().unwrap().remove(&response.request_id) {
            let _ = waiting.send(response.result);
        }
    }

    /// Every waiting request gives up at once.
    pub fn fail_all(&self, message: &str) {
        let waiting: Vec<_> = self.pending.lock().unwrap().drain().collect();
        for (_, settle) in waiting {
            let _ = settle.send(Err(message.to_string()));
        }
    }

    async fn ask(&self, session_id: &str, op: MobileOp, patience: Duration) -> Result<Value, String> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (settle, settled) = oneshot::channel();
        self.pending.lock().unwrap().insert(request_id.clone(), settle);
        let request = MobileRequest { request_id: request_id.clone(), session_id: session_id.to_string(), answer_by: super::now() + patience.as_millis() as i64, op };
        if !(self.send)(request) {
            self.pending.lock().unwrap().remove(&request_id);
            return Err(concat!("The ", act_core::app_name!(), " window is not open on your computer.").into());
        }
        match tokio::time::timeout(patience, settled).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("The phone bridge was turned off.".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&request_id);
                Err("Your computer did not answer in time. Try again.".into())
            }
        }
    }
}
