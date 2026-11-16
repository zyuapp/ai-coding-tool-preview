//! Reads that are content rather than state, answered by whichever computer holds what they name:
//! this one, or the paired one holding the thread or checkout.

use std::sync::Arc;

use act_core::view::computers::{computer_of_thread, computer_of_workspace};
use act_core::WorkspaceState;
use base64::Engine;
use futures::future::BoxFuture;
use serde_json::{json, Value};

use crate::attachments::{MAX_ATTACHMENT_BYTES, MAX_ATTACHMENT_ENCODED_BYTES};
use crate::git::diff::{diff_patch, DiffRange};
use crate::message_images::{message_image_bytes, MessageImageData};
use crate::mobile::messages::is_computer_query;
use crate::services::HostServices;

const MAX_DIRECTORIES: usize = 20;
const MAX_PATH_LENGTH: usize = 4_096;

/// What only the window or a crate above this one can answer.
pub trait QueryHost: Send + Sync + 'static {
    /// A thread read or listing from this computer's own threads.
    fn threads(&self, query: Value) -> BoxFuture<'static, Result<Value, String>>;
    /// A terminal's output after a point, or Null when there is no such terminal.
    fn terminal_output(&self, terminal_id: String, after: Option<u64>) -> BoxFuture<'static, Result<Value, String>>;
    /// The engine's slash commands and skills in a checkout, as `{status, commands}`.
    fn commands(&self, root: String, projectless: bool, engine: String) -> BoxFuture<'static, Result<Value, String>>;
    fn state(&self, read: &mut dyn FnMut(&WorkspaceState));
    /// The line to a paired computer, for a read another computer holds.
    fn ask_computer(&self, computer_id: String, query: Value) -> BoxFuture<'static, Result<Value, String>>;
}

/// What a read names: a thread or checkout wherever it is held, or a computer by id ("this" or None for this one).
pub enum ReadOwner {
    Thread(Option<String>),
    Workspace(String),
    Computer(Option<String>),
}

fn refuse(what: &str) -> String {
    format!("Invalid {what} response.")
}

fn is_strings(value: &Value, max: usize) -> bool {
    value.as_array().is_some_and(|items| items.iter().all(|item| item.as_str().is_some_and(|text| text.len() <= max)))
}

fn is_failure(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("error") && value.get("message").is_some_and(Value::is_string)
}

fn failed(message: impl Into<String>) -> Value {
    json!({ "status": "error", "message": message.into() })
}

/// Reads that carry their own failure in their result.
fn fails_in_result(kind: &str) -> bool {
    matches!(kind, "diff-patch" | "branches" | "commands")
}

pub struct ComputerReads {
    services: Arc<HostServices>,
    host: Arc<dyn QueryHost>,
}

impl ComputerReads {
    pub fn new(services: Arc<HostServices>, host: Arc<dyn QueryHost>) -> Self {
        Self { services, host }
    }

    fn state<R>(&self, read: impl FnOnce(&WorkspaceState) -> R) -> R {
        let mut read = Some(read);
        let mut result = None;
        self.host.state(&mut |state| {
            if let Some(read) = read.take() {
                result = Some(read(state));
            }
        });
        result.expect("the host reads its state")
    }

    async fn resolved_root(&self, workspace_id: &str) -> Result<(String, bool), String> {
        let resolution = self.services.workspaces.resolve(workspace_id).await.map_err(|error| error.to_string())?;
        match resolution {
            crate::workspaces::WorkspaceResolution::Available { workspace } => Ok((workspace.root.clone(), workspace.kind == crate::workspaces::WorkspaceKind::Projectless)),
            crate::workspaces::WorkspaceResolution::Unavailable { reason, .. } => Err(format!("Workspace is unavailable ({}).", reason.as_str())),
        }
    }

    /// This computer's own answer to a read, in the form the line carries.
    pub async fn answer(&self, query: Value) -> Result<Value, String> {
        let kind = query.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
        let answered = self.here(&query).await;
        match answered {
            Err(message) if fails_in_result(&kind) => Ok(failed(message)),
            other => other,
        }
    }

    async fn here(&self, query: &Value) -> Result<Value, String> {
        if !is_computer_query(query) {
            return Err(format!("Invalid {} query.", query.get("kind").and_then(Value::as_str).unwrap_or("undefined")));
        }
        let text = |key: &str| query.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        match query.get("kind").and_then(Value::as_str).unwrap_or("") {
            "thread-read" | "thread-list" => self.host.threads(query.clone()).await,
            "terminal-output" => self.host.terminal_output(text("terminalId"), query.get("after").and_then(Value::as_u64)).await,
            "directories" => crate::files::list_directories(&text("prefix")).await.map(|list| json!(list)).map_err(|error| error.0),
            "attachment" => {
                let bytes = self.services.attachments.read_saved(&text("name")).await.map_err(|error| error.to_string())?;
                Ok(json!(base64::engine::general_purpose::STANDARD.encode(bytes)))
            }
            "message-image" => {
                let image = self.services.message_images.read(&text("path"), &text("root"), &text("message"), query.get("thumbnail") == Some(&Value::Bool(true))).await?;
                Ok(json!({ "data": base64::engine::general_purpose::STANDARD.encode(image.bytes), "contentType": image.content_type }))
            }
            "diff-patch" => {
                let range: DiffRange = serde_json::from_value(query.get("range").cloned().unwrap_or(Value::Null)).map_err(|error| error.to_string())?;
                let previous = query.get("previousPath").and_then(Value::as_str);
                let result = diff_patch(&text("workspaceId"), &range, &text("path"), &self.services.workspaces, previous, query.get("ignoreWhitespace") == Some(&Value::Bool(true))).await;
                serde_json::to_value(result).map_err(|error| error.to_string())
            }
            "branches" => {
                let (root, _) = self.resolved_root(&text("workspaceId")).await?;
                let branches = crate::git::list_branches(std::path::Path::new(&root)).await.map_err(|error| error.to_string())?;
                Ok(json!({ "status": "available", "branches": branches.branches, "remotes": branches.remotes, "current": branches.current }))
            }
            "commits" => {
                let Ok(resolution) = self.services.workspaces.resolve(&text("workspaceId")).await else { return Ok(json!([])) };
                let crate::workspaces::WorkspaceResolution::Available { workspace } = resolution else { return Ok(json!([])) };
                let hashes: Vec<String> = serde_json::from_value(query.get("hashes").cloned().unwrap_or(Value::Null)).unwrap_or_default();
                crate::git::known_commits(std::path::Path::new(&workspace.root), &hashes).await.map(|known| json!(known)).map_err(|error| error.to_string())
            }
            "commands" => {
                let (root, projectless) = self.resolved_root(&text("workspaceId")).await?;
                self.host.commands(root, projectless, text("engine")).await
            }
            kind => Err(format!("Invalid {kind} query.")),
        }
    }

    /// The holder's reply as this computer uses it; anything malformed is refused.
    fn received(&self, kind: &str, value: Value, query: &Value) -> Result<Value, String> {
        match kind {
            "thread-read" | "thread-list" => Ok(value),
            "terminal-output" => (value.is_null() || value.is_object()).then_some(value).ok_or_else(|| refuse("terminal output")),
            "directories" => (is_strings(&value, MAX_PATH_LENGTH) && value.as_array().is_some_and(|list| list.len() <= MAX_DIRECTORIES && list.iter().all(|item| !item.as_str().unwrap_or("").contains('\0')))).then_some(value).ok_or_else(|| refuse("directory")),
            "attachment" => {
                let text = value.as_str().filter(|text| !text.is_empty() && text.len() <= MAX_ATTACHMENT_ENCODED_BYTES && crate::attachments::is_base64(text)).ok_or_else(|| refuse("attachment"))?;
                let bytes = base64::engine::general_purpose::STANDARD.decode(text).map_err(|_| refuse("attachment"))?;
                (!bytes.is_empty() && bytes.len() as u64 <= MAX_ATTACHMENT_BYTES).then_some(value).ok_or_else(|| refuse("attachment"))
            }
            "diff-patch" => {
                let ok = match value.get("status").and_then(Value::as_str) {
                    Some("available") => value.get("patch").is_some_and(Value::is_string),
                    Some("too-large") => value.get("limit").is_some_and(Value::is_number),
                    _ => is_failure(&value),
                };
                ok.then_some(value).ok_or_else(|| refuse("patch"))
            }
            "branches" => {
                let ok = if value.get("status").and_then(Value::as_str) == Some("available") {
                    is_strings(value.get("branches").unwrap_or(&Value::Null), MAX_PATH_LENGTH) && is_strings(value.get("remotes").unwrap_or(&Value::Null), MAX_PATH_LENGTH) && value.get("current").is_some_and(|current| current.is_null() || current.is_string())
                } else {
                    is_failure(&value)
                };
                ok.then_some(value).ok_or_else(|| refuse("branch"))
            }
            "commits" => {
                let asked: Vec<&str> = query.get("hashes").and_then(Value::as_array).map(|hashes| hashes.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                let ok = value.as_array().is_some_and(|known| known.iter().all(|hash| hash.as_str().is_some_and(|hash| asked.contains(&hash))));
                ok.then_some(value).ok_or_else(|| refuse("commit"))
            }
            "commands" => {
                let ok = if value.get("status").and_then(Value::as_str) == Some("available") {
                    value.get("commands").and_then(Value::as_array).is_some_and(|commands| {
                        commands.iter().all(|command| {
                            command.get("name").is_some_and(Value::is_string) && command.get("description").is_some_and(Value::is_string) && command.get("argumentHint").is_some_and(Value::is_string) && command.get("aliases").is_none_or(|aliases| is_strings(aliases, MAX_PATH_LENGTH))
                        })
                    })
                } else {
                    is_failure(&value)
                };
                ok.then_some(value).ok_or_else(|| refuse("command"))
            }
            _ => Ok(value),
        }
    }

    fn holder(&self, owner: &ReadOwner) -> Result<Option<String>, String> {
        match owner {
            ReadOwner::Thread(thread) => Ok(self.state(|state| computer_of_thread(state, thread.as_deref()).map(|computer| computer.link.id.clone()))),
            ReadOwner::Workspace(workspace) => Ok(self.state(|state| computer_of_workspace(state, workspace).map(|computer| computer.link.id.clone()))),
            ReadOwner::Computer(None) => Ok(None),
            ReadOwner::Computer(Some(computer)) if computer == "this" => Ok(None),
            ReadOwner::Computer(Some(computer)) if computer.is_empty() || computer.len() > 256 => Err("Invalid computer.".into()),
            ReadOwner::Computer(Some(computer)) => Ok(Some(computer.clone())),
        }
    }

    /// A read answered by whichever computer holds what it names, in one shape however it fails. A
    /// message image another computer holds is kept here, so its previews are cut here.
    pub async fn read(&self, query: Value, owner: ReadOwner) -> Result<Value, String> {
        let kind = query.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
        let result = async {
            if !is_computer_query(&query) {
                return Err(format!("Invalid {kind} query."));
            }
            let Some(computer) = self.holder(&owner)? else { return self.here(&query).await };
            if kind == "message-image" {
                let text = |key: &str| query.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                let (path, root, message) = (text("path"), text("root"), text("message"));
                let original = json!({ "kind": "message-image", "path": path, "root": root, "message": message });
                let host = self.host.clone();
                let file = path.clone();
                let load: BoxFuture<'static, Result<Vec<u8>, String>> = Box::pin(async move {
                    let answer = host.ask_computer(computer, original).await?;
                    let data: MessageImageData = serde_json::from_value(answer).map_err(|_| "Invalid message image response.".to_string())?;
                    message_image_bytes(&data, &file)
                });
                self.services.message_images.preserve(&path, &root, &message, Some(load)).await?;
                let image = self.services.message_images.read(&path, &root, &message, query.get("thumbnail") == Some(&Value::Bool(true))).await?;
                return Ok(json!({ "data": base64::engine::general_purpose::STANDARD.encode(image.bytes), "contentType": image.content_type }));
            }
            let answer = self.host.ask_computer(computer, query.clone()).await?;
            self.received(&kind, answer, &query)
        }
        .await;
        match result {
            Err(message) if fails_in_result(&kind) => Ok(failed(message)),
            other => other,
        }
    }
}
