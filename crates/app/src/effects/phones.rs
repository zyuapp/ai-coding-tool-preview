//! A phone's requests: a projection of the workspace to read, and the window's own command path
//! to write through, checked at this boundary first (src/host/mobile-bridge.ts).

use std::sync::Arc;
use std::time::Duration;

use act_core::state::{draining_queued, editable_queued, NEW_THREAD_DRAFT, QUEUED_SENT};
use act_core::validation::REMOTE_UNSUPPORTED;
use act_core::view::mobile::{mobile_archived_threads, mobile_thread_history, mobile_thread_read, phone_edits_queued, phone_reaches_thread, project_mobile_view, remote_member_holder, says_queued_gone};
use act_core::workspace::location::thread_workspace_id;
use act_core::{AppCommand, CommandResult, WorkspaceInput, WorkspaceState};
use act_host::computers::queries::ReadOwner;
use act_host::mobile::relay::{MobileOp, MobileRequest};
use act_host::thread_requests::{with_state, RequestHost};
use serde_json::{json, Value};

use super::Host;

pub const MOBILE_REFUSED: &str = "That command is not one a phone may send.";
const MOBILE_QUEUED_NOT_WORDS: &str = "A message with images, files, pasted text or notes can only be edited on the computer.";
const MOBILE_NO_CHECKOUT: &str = "This thread has no checkout to review.";
const MOBILE_NO_COMMANDS: &str = "That computer does not say what this thread's commands are.";
const MOBILE_EDIT_WAIT: i64 = 5_000;
const MOBILE_EDIT_MARGIN: i64 = 1_500;
const MOBILE_EDIT_TOO_LATE: &str = "Your computer did not answer in time. Try again.";
const MOBILE_EDIT_LATE: &str = "That computer has not answered yet. If it takes the message out, its words wait in this computer's composer.";
const MOBILE_DRAFT_BUSY: &str = "The last message from this new thread is still on its way. Send again once its thread opens.";

fn no_thread(task_id: &str) -> String {
    format!("No thread has the ID {task_id}.")
}

fn view(requests: &dyn RequestHost) -> Value {
    with_state(requests, |state| project_mobile_view(state, act_core::util::now()).to_json().unwrap_or(Value::Null))
}

/// Answers one phone request: a snapshot, a query, or a command.
pub async fn answer(host: &Arc<Host>, request: MobileRequest) -> Result<Value, String> {
    let requests = host.requests.clone();
    match request.op {
        MobileOp::Snapshot => Ok(view(requests.as_ref())),
        MobileOp::Query { query } => {
            if !act_host::mobile::messages::is_mobile_query(&query) {
                return Err(MOBILE_REFUSED.into());
            }
            answer_query(host, query).await
        }
        MobileOp::Command { command } => {
            let command = act_core::validation::phone_command(&command).map_err(|_| MOBILE_REFUSED.to_string())?;
            if let Some(task_id) = command.task_id() {
                let task_id = task_id.to_string();
                if !with_state(requests.as_ref(), |state| phone_reaches_thread(state, &task_id)) {
                    return Err(no_thread(&task_id));
                }
            }
            if with_state(requests.as_ref(), |state| touches_draft(state, &command) && draft_sending(state)) {
                return Err(MOBILE_DRAFT_BUSY.into());
            }
            if let AppCommand::TaskEditQueued { task_id, message_id } = &command {
                answer_edit(requests.clone(), task_id.clone(), message_id.clone(), request.answer_by).await?;
                return Ok(view(requests.as_ref()));
            }
            for command in with_state(requests.as_ref(), |state| phone_commands(state, command)) {
                let accepted = requests.execute(WorkspaceInput::Command(command)).accepted.await;
                if let CommandResult::Failed { message, .. } = accepted {
                    return Err(message);
                }
            }
            Ok(view(requests.as_ref()))
        }
    }
}

fn text(query: &Value, key: &str) -> String {
    query.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

async fn answer_query(host: &Arc<Host>, query: Value) -> Result<Value, String> {
    let requests = host.requests.clone();
    let kind = text(&query, "kind");
    let task_id = text(&query, "taskId");
    match kind.as_str() {
        "archived" => Ok(with_state(requests.as_ref(), |state| serde_json::to_value(mobile_archived_threads(state)).unwrap_or(Value::Null))),
        "thread" | "history" => {
            let before = query.get("before").and_then(Value::as_u64).unwrap_or(0) as usize;
            let read = |state: &WorkspaceState| -> Option<Value> {
                if kind == "thread" {
                    mobile_thread_read(state, &task_id).and_then(|read| read.to_json().ok())
                } else {
                    mobile_thread_history(state, &task_id, before).and_then(|read| read.to_json().ok())
                }
            };
            let remote = with_state(requests.as_ref(), |state| remote_member_holder(state, &task_id).map(|holder| holder.state.as_deref().map(read)));
            if let Some(remote) = remote {
                return remote.flatten().ok_or_else(|| no_thread(&task_id));
            }
            if !with_state(requests.as_ref(), |state| state.thread(&task_id).is_some()) {
                return Err(no_thread(&task_id));
            }
            requests.hydrate(task_id.clone()).await;
            with_state(requests.as_ref(), read).ok_or_else(|| no_thread(&task_id))
        }
        "directories" => act_host::files::list_directories(&text(&query, "prefix")).await.map(|list| json!(list)).map_err(|error| error.0),
        "transcribe" => {
            let audio = text(&query, "audio");
            let dictation = super::dictation::host(&host.sink);
            dictation.transcribe_audio(&audio).await.map(|text| json!({ "text": text }))
        }
        "image" => linked_image(host, &query).await,
        "commands" => composer_commands(host, query.get("taskId").and_then(Value::as_str).map(String::from)).await,
        "diff-summary" | "diff-patch" => {
            let workspace_id = with_state(requests.as_ref(), |state| -> Result<String, String> {
                let thread = state.thread(&task_id).ok_or_else(|| no_thread(&task_id))?;
                thread_workspace_id(state, Some(thread)).ok_or_else(|| MOBILE_NO_CHECKOUT.to_string())
            })?;
            let range = serde_json::from_value(query.get("range").cloned().unwrap_or(Value::Null)).map_err(|error| error.to_string())?;
            if kind == "diff-summary" {
                let summary = act_host::git::diff::diff_summary(&workspace_id, &range, &host.services.workspaces, true).await;
                return serde_json::to_value(summary).map_err(|error| error.to_string());
            }
            let mut read = json!({ "kind": "diff-patch", "workspaceId": workspace_id, "range": query.get("range"), "path": text(&query, "path"), "ignoreWhitespace": true });
            if let Some(previous) = query.get("previousPath").filter(|previous| previous.is_string()) {
                read["previousPath"] = previous.clone();
            }
            host.read_content(read, ReadOwner::Workspace(workspace_id)).await
        }
        _ => Err(MOBILE_REFUSED.into()),
    }
}

/// The "/" menu the desktop's composer would show for the thread, or for the draft's project.
async fn composer_commands(host: &Arc<Host>, task_id: Option<String>) -> Result<Value, String> {
    let requests = host.requests.clone();
    let found = with_state(requests.as_ref(), |state| -> Result<(Option<String>, String, bool), String> {
        let remote = task_id.as_deref().and_then(|id| remote_member_holder(state, id)).and_then(|holder| holder.state.as_deref());
        let holder = remote.unwrap_or(state);
        let thread = match &task_id {
            Some(id) => Some(holder.thread(id).ok_or_else(|| no_thread(id))?),
            None => None,
        };
        let engine = thread.map(|thread| thread.engine).unwrap_or(state.draft_engine);
        let workspace_id = match thread {
            Some(thread) => thread_workspace_id(holder, Some(thread)),
            None => state.draft_project_id.as_deref().and_then(|id| state.project(id)).and_then(|project| project.workspace_id.clone()),
        };
        let engine = serde_json::to_value(engine).ok().and_then(|engine| engine.as_str().map(String::from)).unwrap_or_default();
        Ok((workspace_id, engine, remote.is_some()))
    })?;
    let (workspace_id, engine, remote) = found;
    let workspace_id = match workspace_id {
        Some(id) => id,
        None if remote => return Ok(json!({ "status": "error", "message": MOBILE_NO_COMMANDS })),
        None => host.services.workspaces.projectless().await.map_err(|error| error.to_string())?.id,
    };
    host.read_content(json!({ "kind": "commands", "workspaceId": workspace_id, "engine": engine }), ReadOwner::Workspace(workspace_id)).await
}

/// Only an image the named reply links is read, from the checkout of the thread that holds it.
async fn linked_image(host: &Arc<Host>, query: &Value) -> Result<Value, String> {
    let requests = host.requests.clone();
    let task_id = text(query, "taskId");
    let remote = with_state(requests.as_ref(), |state| remote_member_holder(state, &task_id).is_some_and(|holder| holder.state.is_some()));
    if !remote {
        if !with_state(requests.as_ref(), |state| state.thread(&task_id).is_some()) {
            return Err(no_thread(&task_id));
        }
        requests.hydrate(task_id.clone()).await;
    }
    let file = act_host::message_links::message_image_path(&text(query, "href"));
    let message_id = text(query, "messageId");
    let found = with_state(requests.as_ref(), |state| {
        let holder = if remote { remote_member_holder(state, &task_id).and_then(|holder| holder.state.as_deref()) } else { Some(state) }?;
        let thread = holder.thread(&task_id)?;
        let message = thread.messages.iter().find(|message| message.id == message_id)?;
        if message.kind != act_core::domain::conversation::MessageKind::Assistant {
            return None;
        }
        let file = file.clone()?;
        if !act_markdown::images::message_images(&message.text).iter().any(|image| image.path == file) {
            return None;
        }
        Some((file, act_core::workspace::location::thread_workspace_root(holder, Some(thread)).unwrap_or_default(), thread.id.clone()))
    });
    let Some((file, root, thread_id)) = found else { return Err("That reply links no such image.".into()) };
    let read = json!({ "kind": "message-image", "path": file, "root": root, "message": message_id, "thumbnail": query.get("preview") == Some(&Value::Bool(true)) });
    host.read_content(read, ReadOwner::Thread(Some(thread_id))).await
}

/// Whether a phone's command writes or sends the new thread's draft, which the computer has open.
fn touches_draft(state: &WorkspaceState, command: &AppCommand) -> bool {
    if act_core::reducer::sending::prompt_key(state) != NEW_THREAD_DRAFT {
        return false;
    }
    match command {
        AppCommand::SetPrompt { task_id, .. } => task_id.is_none(),
        AppCommand::AttachmentsSend { task_id, .. } => task_id.is_none(),
        AppCommand::TaskSend(send) => send.task_id.is_none() && send.text.is_none(),
        _ => false,
    }
}

fn draft_sending(state: &WorkspaceState) -> bool {
    state.attachment_send_for(None).busy || state.pending_runs.values().any(|pending| pending.draft_key.as_deref() == Some(NEW_THREAD_DRAFT))
}

/// A phone's images leave its composer at once, so a send carries its words, even none. One from
/// the new thread's draft goes as the composer's own draft send does.
fn phone_commands(state: &WorkspaceState, command: AppCommand) -> Vec<AppCommand> {
    let AppCommand::AttachmentsSend { task_id, steer, text, attachments } = command else { return vec![command] };
    if task_id.is_none() && act_core::reducer::sending::prompt_key(state) == NEW_THREAD_DRAFT {
        return vec![AppCommand::SetPrompt { task_id: None, prompt: text.unwrap_or_default() }, AppCommand::AttachmentsSend { task_id, steer, text: None, attachments }];
    }
    vec![AppCommand::AttachmentsSend { task_id, steer, text: Some(text.unwrap_or_default()), attachments }]
}

/// The phone hears the edit went through only once the computer holding the queue says the message
/// left it; one that answers too late leaves its words in this computer's composer.
async fn answer_edit(requests: Arc<dyn RequestHost>, task_id: Option<String>, message_id: String, answer_by: i64) -> Result<(), String> {
    let wait = MOBILE_EDIT_WAIT.min(answer_by - act_core::util::now() - MOBILE_EDIT_MARGIN);
    if wait <= 0 {
        return Err(MOBILE_EDIT_TOO_LATE.into());
    }
    let Some(task_id) = task_id else { return Err(QUEUED_SENT.into()) };
    let edited = with_state(requests.as_ref(), |state| -> Result<String, String> {
        let remote = remote_member_holder(state, &task_id);
        if remote.is_some_and(|remote| !says_queued_gone(remote)) {
            return Err(REMOTE_UNSUPPORTED.into());
        }
        let holder = remote.and_then(|remote| remote.state.as_deref()).unwrap_or(state);
        let queued = holder.queued_messages.get(&task_id).map(Vec::as_slice).unwrap_or(&[]);
        let message = editable_queued(queued, &message_id).map_err(String::from)?;
        if draining_queued(holder, &task_id, &message.held.id) {
            return Err(QUEUED_SENT.into());
        }
        if !phone_edits_queued(message) {
            return Err(MOBILE_QUEUED_NOT_WORDS.into());
        }
        Ok(message.held.text.clone())
    })?;
    let drop = AppCommand::TaskDropQueued { task_id: Some(task_id.clone()), message_id, if_waiting: true };
    let execution = requests.execute(WorkspaceInput::Command(drop));
    let outcome = async move {
        match execution.accepted.await {
            CommandResult::Failed { message, ok } => CommandResult::Failed { message, ok },
            _ => execution.completed.await,
        }
    };
    let mut outcome = Box::pin(outcome);
    match tokio::time::timeout(Duration::from_millis(wait as u64), &mut outcome).await {
        Ok(CommandResult::Failed { message, .. }) => Err(message),
        Ok(_) => Ok(()),
        Err(_) => {
            act_host::runtime().spawn(async move {
                if !outcome.await.is_ok() {
                    return;
                }
                let typed = with_state(requests.as_ref(), |state| state.prompts.get(&task_id).cloned().unwrap_or_default());
                let prompt = [edited, typed].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n\n");
                requests.execute(WorkspaceInput::Command(AppCommand::SetPrompt { task_id: Some(task_id), prompt }));
            });
            Err(MOBILE_EDIT_LATE.into())
        }
    }
}
