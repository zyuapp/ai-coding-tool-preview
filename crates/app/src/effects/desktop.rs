//! Files and folders opened out of the window, the images it keeps and saves, the composer's
//! attachments, and notices (src/host/surface-effects.ts, attachment-effects.ts).

use std::sync::Arc;

use act_core::domain::conversation::{ImageAnnotationKind, OutgoingAttachment};
use act_core::events::{ThreadNotice, WorkspaceEvent};
use base64_lite::encode;
use serde_json::json;

use super::{Host, Sink};

pub async fn preserve_message_images(host: &Arc<Host>, text: &str, root: &str, message_id: &str) {
    let files: Vec<String> = act_markdown::images::message_images(text).into_iter().map(|image| image.path).collect();
    if files.is_empty() {
        return;
    }
    if let Err(error) = host.services.message_images.preserve_all(&files, root, message_id).await {
        eprintln!("Could not keep the message's images: {error}");
    }
}

pub async fn open_file(sink: &Sink, roots: Vec<String>, path: String, line: Option<i64>) {
    match act_host::files::openable_file(&roots, &path).await {
        Ok(file) => act_host::open::open_in_editor(&file, line.and_then(|line| u32::try_from(line).ok())),
        Err(error) => sink.failed(error.to_string()),
    }
}

/** An application scan that fails leaves the menu with nothing to offer rather than an error. */
pub async fn list_apps(sink: &Sink) {
    let apps = tokio::task::spawn_blocking(act_host::open::list_installed_apps).await.unwrap_or_default();
    let apps: Vec<serde_json::Value> = apps
        .into_iter()
        .map(|installed| {
            let icon = installed.icon.map(|bytes| format!("data:image/png;base64,{}", encode(&bytes)));
            json!({ "id": installed.app.id, "label": installed.app.label, "kind": installed.app.kind, "icon": icon })
        })
        .collect();
    sink.json(json!({ "type": "apps.listed", "apps": apps }));
}

pub async fn open_folder_in_app(sink: &Sink, root: String, app_id: String) {
    let folder = match act_host::files::file_in_checkout(&root, ".").await {
        Ok(folder) => folder,
        Err(error) => return sink.failed(error.to_string()),
    };
    if let Err(error) = tokio::task::spawn_blocking(move || act_host::open::open_folder_in_app(&app_id, &folder)).await.unwrap_or_else(|error| Err(error.to_string())) {
        sink.failed(error);
    }
}

/// Says what happened where the user already is; the click opens the thread.
pub fn announce(sink: &Sink, notice: ThreadNotice) {
    let click = sink.clone();
    let task_id = notice.task_id.clone();
    act_host::notify::notify(
        &notice.title,
        &notice.headline,
        Some(Box::new(move || {
            click.send(super::Inbound::Window(super::requests::WindowWork::Reveal));
            click.command(act_core::AppCommand::TaskSelect { task_id });
        })),
    );
}

/// Saves an image the window shows where the user picks, starting in Downloads (image-download.ts).
pub async fn download_image(host: &Arc<Host>, source: String) {
    let download = if act_host::image_download::is_image_source(&source) {
        act_host::image_download::prepare_image_download(&host.services, &source, |_| None).await
    } else {
        match host.services.attachments.saved_path(&source).or_else(|| Some(std::path::PathBuf::from(&source)).filter(|path| path.is_absolute() && path.exists())) {
            Some(original) => {
                let extension = original.extension().map(|extension| extension.to_string_lossy().into_owned()).unwrap_or_else(|| "png".into());
                let downloads = act_host::paths::home().join("Downloads");
                let default_path = if downloads.is_dir() { downloads } else { act_host::paths::home() }.join(format!("image.{extension}"));
                Ok(act_host::image_download::ImageDownload { original, bytes: None, extension, default_path })
            }
            None => Err("This image is no longer available.".to_string()),
        }
    };
    match download {
        Ok(download) => host.sink.send(super::Inbound::Window(super::requests::WindowWork::SaveImage(download))),
        Err(error) => host.sink.failed(error),
    }
}

/// A staged image without marks is already on disk; one with marks arrives drawn into its source.
pub async fn send_attachments(host: &Arc<Host>, task_id: Option<String>, steer: Option<bool>, text: Option<String>, attachments: Vec<OutgoingAttachment>) {
    let mut saved = Vec::new();
    for attachment in &attachments {
        let path = match (&attachment.path, attachment.annotations.is_empty()) {
            (Some(path), true) => path.clone(),
            _ => {
                let data = attachment.source.split_once(',').map(|(_, data)| data).unwrap_or(&attachment.source);
                match host.services.attachments.save(data, attachment.path.as_deref()).await {
                    Ok(path) => path.to_string_lossy().into_owned(),
                    Err(error) => return host.sink.event(WorkspaceEvent::AttachmentsFailed { task_id, message: error.to_string() }),
                }
            }
        };
        let labels: Vec<String> = attachment.annotations.iter().filter(|annotation| annotation.kind == ImageAnnotationKind::Box).map(|annotation| annotation.text.clone()).collect();
        saved.push(json!({ "path": path, "labels": labels, "context": attachment.context }));
    }
    let ids: Vec<String> = attachments.iter().map(|attachment| attachment.id.clone()).collect();
    host.sink.json(json!({ "type": "attachments.saved", "taskId": task_id, "steer": steer, "text": text, "ids": ids, "attachments": saved }));
}

/// The third-party notices this build ships, in the system's viewer.
pub fn open_licenses(sink: &Sink) {
    let exe = std::env::current_exe().ok();
    let found = exe.iter().flat_map(|exe| exe.ancestors()).flat_map(|dir| [dir.join("Resources/legal/THIRD-PARTY-NOTICES.txt"), dir.join("assets/legal/THIRD-PARTY-NOTICES.txt")]).find(|file| file.is_file());
    match found {
        Some(file) => {
            act_host::open::open_url(&format!("file://{}", file.display()));
        }
        None => sink.failed("This build has no license notices to show."),
    }
}

/// Standard base64, enough for icon data URLs.
mod base64_lite {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let value = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
            out.push(TABLE[(value >> 18) as usize & 63] as char);
            out.push(TABLE[(value >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { TABLE[(value >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { TABLE[value as usize & 63] as char } else { '=' });
        }
        out
    }
}

/// Opens a web page in the user's browser; anything but http and https stays shut, as main.ts's `webPageUrl`.
pub fn open_link(url: &str) {
    let scheme = url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase());
    let web = url.len() <= 8_192 && matches!(scheme.as_deref(), Some("http" | "https"));
    if web {
        act_host::open::open_url(url);
    }
}
