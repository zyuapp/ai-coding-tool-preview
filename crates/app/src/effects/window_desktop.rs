//! The calls a view may make on the host, as `window-desktop.ts` serves the renderer's: reads answered
//! by whichever computer holds what they name, and frame calls that dress the window's native
//! surfaces. None of them changes workspace state or the user's files; anything that does is an
//! `AppCommand`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use act_core::domain::conversation::AttachedFileDraft;
use act_core::domain::diff::DiffRange;
use act_core::domain::engine::AgentEngine;
use act_host::computers::queries::ReadOwner;
pub use act_engines::types::AvailableCommand;
pub use act_host::git::diff::DiffPatchResult;
pub use act_host::git::Branches;
use gpui::App;

use super::{convert, Host};

/// How many paths one drop may name.
const MAX_DESCRIBED_FILES: usize = 20;

fn host(cx: &App) -> Arc<Host> {
    crate::workspace::workspace(cx).read(cx).host.clone()
}

/// Runs `work` on the host runtime; the answer can be awaited on the window's thread.
fn off_window<T: Send + 'static>(work: impl Future<Output = Result<T, String>> + Send + 'static) -> impl Future<Output = Result<T, String>> {
    let read = act_host::runtime().spawn(work);
    async move { read.await.map_err(|error| error.to_string()).and_then(|result| result) }
}

/// `branches`: a checkout's branches.
pub fn branches(workspace_id: String, cx: &App) -> impl Future<Output = Result<Branches, String>> {
    let host = host(cx);
    off_window(async move { host.branches(workspace_id).await })
}

/// `diffPatch`: one file's patch in a comparison.
pub fn diff_patch(workspace_id: String, range: &DiffRange, path: String, previous: Option<String>, ignore_whitespace: bool, cx: &App) -> impl Future<Output = DiffPatchResult> {
    let host = host(cx);
    let range = convert::<_, act_host::git::diff::DiffRange>(range);
    let read = off_window(async move {
        let range = range?;
        Ok(host.diff_patch(workspace_id, &range, &path, previous.as_deref(), ignore_whitespace).await)
    });
    async move { read.await.unwrap_or_else(|message| DiffPatchResult::Error { message }) }
}

/// `commits`: which of `hashes` the checkout holds, or None when it cannot answer.
pub fn commits(workspace_id: String, hashes: Vec<String>, cx: &App) -> impl Future<Output = Option<Vec<String>>> {
    let host = host(cx);
    let read = off_window(async move {
        let query = serde_json::json!({ "kind": "commits", "workspaceId": workspace_id, "hashes": hashes });
        let value = host.read_content(query, ReadOwner::Workspace(workspace_id)).await?;
        serde_json::from_value::<Vec<String>>(value).map_err(|error| error.to_string())
    });
    async move { read.await.ok() }
}

/// `commands`: the engine's commands in a checkout, or in the projectless workspace without one.
pub fn commands(workspace_id: Option<String>, engine: AgentEngine, cx: &App) -> impl Future<Output = Result<Vec<AvailableCommand>, String>> {
    let host = host(cx);
    off_window(async move {
        let runs = act_engines::engines::engine(engine).ok_or_else(|| format!("unknown engine `{}`", engine.as_str()))?;
        if let Some(id) = workspace_id {
            let query = serde_json::json!({ "kind": "commands", "workspaceId": id, "engine": engine });
            let value = host.read_content(query, ReadOwner::Workspace(id)).await?;
            if value.get("status").and_then(serde_json::Value::as_str) != Some("available") {
                return Err(value.get("message").and_then(serde_json::Value::as_str).unwrap_or("Commands unavailable").to_string());
            }
            return serde_json::from_value(value.get("commands").cloned().unwrap_or_default()).map_err(|error| error.to_string());
        }
        let root = host.services.workspaces.projectless().await.map_err(|error| error.to_string())?.root;
        runs.commands(&root, true).await.map_err(|error| error.to_string())
    })
}

/// `describeFiles`: what a drop names, each a file or folder that exists, at most twenty.
pub fn describe_files(paths: Vec<String>) -> impl Future<Output = Vec<AttachedFileDraft>> {
    let read = off_window(async move {
        let named: Vec<String> = paths.into_iter().take(MAX_DESCRIBED_FILES).collect();
        Ok(act_host::files::describe_files(&named).await.iter().filter_map(|draft| convert(draft).ok()).collect())
    });
    async move { read.await.unwrap_or_default() }
}

/// `readAttachment`: one of the app's own attachments as base64, refusing any other path.
pub fn read_attachment(path: String, cx: &App) -> impl Future<Output = Result<String, String>> {
    let host = host(cx);
    off_window(async move { host.services.attachments.read_kept(&path).await.map_err(|error| error.to_string()) })
}

/// `terminalSnapshot`: the screen of a shell on this computer.
pub fn terminal_screen(terminal_id: &str, cx: &App) -> Option<Arc<act_terminal::screen::Screen>> {
    host(cx).terminals.screen(terminal_id)
}

/// `readRemoteTerminal`: a paired computer's shell after a point.
pub fn remote_terminal(computer: String, terminal_id: String, cx: &App) -> act_terminal::view::RemoteRead {
    let host = host(cx);
    std::rc::Rc::new(move |after| {
        let (host, computer) = (host.clone(), computer.clone());
        let mut query = serde_json::json!({ "kind": "terminal-output", "terminalId": terminal_id });
        if let Some(after) = after {
            query["after"] = serde_json::json!(after);
        }
        let read = off_window(async move { host.read_content(query, ReadOwner::Computer(Some(computer))).await });
        Box::pin(async move {
            let value = read.await?;
            if value.is_null() {
                return Ok(None);
            }
            serde_json::from_value(value).map(Some).map_err(|_| "Invalid terminal output response.".to_string())
        })
    })
}

/// Part of `setTheme`: the colours shells on this computer draw in.
pub fn set_terminal_theme(theme: &act_terminal::theme::TerminalTheme, cx: &App) {
    host(cx).terminals.set_theme(theme);
}

/// `onDictationLevel`: the speech host whose levels the microphone draws.
pub fn dictation(cx: &App) -> &'static act_devices::dictation::host::DictationHost {
    super::dictation::host(&host(cx).sink)
}

/// Where the `message-image` read keeps a message's image and its preview, and which of them is
/// there to show.
pub struct MessageImage {
    pub original: PathBuf,
    pub shown: Option<PathBuf>,
}

pub fn message_image(file: &str, root: &str, message: &str, cx: &App) -> Option<MessageImage> {
    let (original, thumbnail) = host(cx).services.message_images.copy_paths(file, root, message)?;
    let shown = if thumbnail.exists() { Some(thumbnail) } else if original.exists() { Some(original.clone()) } else { None };
    Some(MessageImage { original, shown })
}

/// `message-image`: keeps a message's image so it previews, as the protocol read does on first sight.
pub fn preserve_message_image(file: String, root: String, message: String, cx: &App) -> impl Future<Output = Result<(), String>> {
    let store = host(cx).services.message_images.clone();
    off_window(async move { store.preserve(&file, &root, &message, None).await.map(|_| ()) })
}

/// The file behind an image source the viewer shows: a message image's kept copy, one of the app's
/// attachments, or a path from another profile.
pub fn image_file(source: &str, cx: &App) -> PathBuf {
    let host = host(cx);
    if let Some((file, root, message)) = act_host::image_download::message_image_reference(source) {
        let kept = host.services.message_images.copy_paths(&file, &root, &message).map(|(original, _)| original);
        return kept.filter(|kept| kept.exists()).unwrap_or_else(|| Path::new(&root).join(&file));
    }
    let attachments = &host.services.attachments;
    attachments.saved_path(source).unwrap_or_else(|| {
        let kept = attachments.directory().join(act_host::attachments::attachment_name(source));
        if kept.exists() || !Path::new(source).exists() {
            kept
        } else {
            PathBuf::from(source)
        }
    })
}

/// An image file's natural size, as the page reads `naturalWidth` and `naturalHeight`.
pub fn image_size(path: &Path) -> Option<(f32, f32)> {
    image::image_dimensions(path).ok().map(|(width, height)| (width as f32, height as f32))
}

/// The IANA zone this computer keeps time in, as `Intl.DateTimeFormat().resolvedOptions().timeZone` names it.
pub fn time_zone() -> String {
    if let Ok(zone) = std::env::var("TZ") {
        if !zone.is_empty() {
            return zone.trim_start_matches(':').to_string();
        }
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|link| {
            let link = link.to_string_lossy().into_owned();
            link.split("zoneinfo/").nth(1).map(String::from)
        })
        .unwrap_or_else(|| "UTC".into())
}
