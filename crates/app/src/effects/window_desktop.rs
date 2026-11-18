//! The calls a view may make on the host, as `window-desktop.ts` serves the renderer's: reads answered
//! by whichever computer holds what they name, and frame calls that dress the window's native
//! surfaces. None of them changes workspace state or the user's files; anything that does is an
//! `AppCommand`.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use act_core::domain::conversation::AttachedFileDraft;
use act_core::domain::diff::DiffRange;
use act_core::domain::engine::AgentEngine;
use act_host::computers::queries::ReadOwner;
pub use act_engines::types::AvailableCommand;
pub use act_host::attachments::attachment_name;
pub use act_host::git::diff::DiffPatchResult;
pub use act_host::git::Branches;
pub use act_host::image_download::message_image_url;
use gpui::{App, Global};

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

/// A message's linked image as its preview strip shows it.
#[derive(Clone)]
pub enum MessagePreview {
    Reading,
    Shown(PathBuf),
    Unavailable,
}

/// Each linked image's preview, by the path its kept copy takes.
#[derive(Default)]
struct MessagePreviews(HashMap<PathBuf, MessagePreview>);

impl Global for MessagePreviews {}

/// `message-image`: a message's image to preview, its kept thumbnail or copy, keeping one on first
/// sight as the protocol read does. Each image is looked for once, off the window, which draws again
/// once it is found.
pub fn message_preview(file: &str, root: &str, message: &str, cx: &mut App) -> Option<MessagePreview> {
    let host = host(cx);
    let (original, thumbnail) = host.services.message_images.copy_paths(file, root, message)?;
    let previews = &mut cx.default_global::<MessagePreviews>().0;
    if let Some(known) = previews.get(&original) {
        return Some(known.clone());
    }
    previews.insert(original.clone(), MessagePreview::Reading);
    let store = host.services.message_images.clone();
    let (file, root, message, key) = (file.to_string(), root.to_string(), message.to_string(), original.clone());
    let read = off_window(async move {
        let kept = || [&thumbnail, &original].into_iter().find(|path| path.exists()).cloned();
        if let Some(path) = kept() {
            return Ok(path);
        }
        store.preserve(&file, &root, &message, None).await?;
        kept().ok_or_else(|| "The kept image is missing.".to_string())
    });
    cx.spawn(async move |cx| {
        let preview = read.await.map_or(MessagePreview::Unavailable, MessagePreview::Shown);
        cx.update(|cx| {
            cx.default_global::<MessagePreviews>().0.insert(key, preview);
            cx.refresh_windows();
        });
    })
    .detach();
    Some(MessagePreview::Reading)
}

/// How long an attached image another computer could not give stays unavailable before it is asked for again.
const ATTACHMENT_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

/// Each attached image's preview, by the path its message names.
#[derive(Default)]
struct AttachmentPreviews(HashMap<String, MessagePreview>);

impl Global for AttachmentPreviews {}

/// `attachment`: an image a message carries. The path it names is the holder's, so a thread another
/// computer holds has its images read from that computer once, off the window, and kept here under
/// the same name, which `image_file` then finds.
pub fn attachment_preview(file: &str, task_id: &str, cx: &mut App) -> MessagePreview {
    if let Some(known) = cx.default_global::<AttachmentPreviews>().0.get(file) {
        return known.clone();
    }
    let local = image_file(file, cx);
    let name = attachment_name(file).to_string();
    let first = if local.exists() {
        MessagePreview::Shown(local)
    } else if act_host::attachments::is_saved_attachment_name(&name) {
        MessagePreview::Reading
    } else {
        MessagePreview::Unavailable
    };
    cx.default_global::<AttachmentPreviews>().0.insert(file.to_string(), first.clone());
    if !matches!(first, MessagePreview::Reading) {
        return first;
    }
    let host = host(cx);
    let query = serde_json::json!({ "kind": "attachment", "name": name });
    let owner = ReadOwner::Thread(Some(task_id.to_string()));
    let read = off_window(async move {
        let value = host.read_content(query, owner).await?;
        let data = value.as_str().ok_or_else(|| "Invalid attachment response.".to_string())?;
        host.services.attachments.keep(&name, data).await.map_err(|error| error.to_string())
    });
    let key = file.to_string();
    cx.spawn(async move |cx| {
        let preview = read.await.map_or(MessagePreview::Unavailable, MessagePreview::Shown);
        let failed = matches!(preview, MessagePreview::Unavailable);
        cx.update(|cx| {
            cx.default_global::<AttachmentPreviews>().0.insert(key.clone(), preview);
            cx.refresh_windows();
        });
        // A computer that was away may be back, so a failed read is tried again once it has rested.
        if failed {
            cx.background_executor().timer(ATTACHMENT_RETRY).await;
            cx.update(|cx| {
                cx.default_global::<AttachmentPreviews>().0.remove(&key);
                cx.refresh_windows();
            });
        }
    })
    .detach();
    first
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

/// When a schedule next runs as the scheduler reads it, or Err when the scheduler would turn it down.
pub fn schedule_next_run(schedule: &str) -> Result<Option<chrono::DateTime<chrono::Utc>>, ()> {
    let parsed = act_host::automation::schedule::Schedule::parse(schedule, None).map_err(|_| ())?;
    let next = parsed.next_after(chrono::Utc::now());
    if act_host::automation::scheduler::assert_schedule(schedule, None).is_err() && next.is_some() {
        return Err(());
    }
    Ok(next)
}

/// The moment a one-shot schedule names, in this computer's zone.
pub fn one_shot_instant(schedule: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    act_host::automation::schedule::parse_iso_instant(schedule, None)
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

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&text[at + 1..at + 3], 16) {
                out.push(byte);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Files copied in the Finder, which a paste attaches rather than typing their names.
#[cfg(target_os = "macos")]
pub fn pasteboard_files() -> Vec<PathBuf> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL};
    let board = NSPasteboard::generalPasteboard();
    let Some(items) = board.pasteboardItems() else { return Vec::new() };
    // SAFETY: the pasteboard type is a static AppKit string.
    let kind = unsafe { NSPasteboardTypeFileURL };
    let mut paths = Vec::new();
    for item in items.iter() {
        if let Some(url) = item.stringForType(kind) {
            let url = url.to_string();
            if let Some(path) = url.strip_prefix("file://") {
                let path = path.strip_prefix("localhost").unwrap_or(path);
                let path = percent_decode(path);
                paths.push(PathBuf::from(if path.len() > 1 { path.trim_end_matches('/') } else { &path }));
            }
        }
    }
    paths
}

#[cfg(not(target_os = "macos"))]
pub fn pasteboard_files() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn file_urls_decode_to_paths() {
        assert_eq!(super::percent_decode("/Users/me/My%20File%E2%80%99s.png"), "/Users/me/My File\u{2019}s.png");
        assert_eq!(super::percent_decode("/tmp/a%2"), "/tmp/a%2");
    }
}
