//! Saving an image the window shows: the same retained original the preview displays, even when the
//! file it was taken from has since gone.

use std::path::{Path, PathBuf};

use futures::future::BoxFuture;

use crate::services::HostServices;

pub const ATTACHMENT_SCHEME: &str = "attachment";
pub const MESSAGE_IMAGE_SCHEME: &str = "message-image";

/// Whether a value names an image the window shows, as `attachment://file/…` or `message-image://file/…`.
pub fn is_image_source(value: &str) -> bool {
    value.len() <= 16_384 && (value.starts_with("attachment://file/") || value.starts_with("message-image://file/"))
}

/// The source a reply's linked image is shown and saved by (`messageImageUrl`).
pub fn message_image_url(file: &str, root: &str, message: &str, thumbnail: bool, task_id: Option<&str>) -> String {
    let mut url = url::Url::parse("message-image://file/").expect("static url");
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("path", file).append_pair("root", root).append_pair("message", message);
        if thumbnail {
            query.append_pair("thumbnail", "1");
        }
        if let Some(task_id) = task_id {
            query.append_pair("taskId", task_id);
        }
    }
    url.to_string()
}

/// The file, checkout root and message a `message-image://` source names.
pub fn message_image_reference(source: &str) -> Option<(String, String, String)> {
    let url = url::Url::parse(source).ok().filter(|url| url.scheme() == MESSAGE_IMAGE_SCHEME)?;
    let query = |name: &str| url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned());
    Some((query("path")?, query("root")?, query("message")?))
}

/// What a save writes, and the name the dialog suggests.
pub struct ImageDownload {
    pub original: PathBuf,
    /// Bytes to write instead of copying `original`, for an attachment another computer holds.
    pub bytes: Option<Vec<u8>>,
    pub extension: String,
    pub default_path: PathBuf,
}

fn downloads_directory() -> PathBuf {
    let home = crate::paths::home();
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

/// Finds the original a source names. `scoped` reads an attachment scoped to a thread, which may be
/// held by a paired computer, the way the preview reads it.
pub async fn prepare_image_download(services: &HostServices, source: &str, scoped: impl FnOnce(&str) -> Option<BoxFuture<'static, Result<Vec<u8>, String>>>) -> Result<ImageDownload, String> {
    if !is_image_source(source) {
        return Err("Invalid image reference.".into());
    }
    let url = url::Url::parse(source).map_err(|_| "Invalid image reference.".to_string())?;
    let query = |name: &str| url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned());
    let message_image = url.scheme() == MESSAGE_IMAGE_SCHEME;
    let original = if message_image {
        let (Some(file), Some(root), Some(message)) = (query("path"), query("root"), query("message")) else { return Err("This image is no longer available.".into()) };
        services.message_images.preserve(&file, &root, &message, None).await.map_err(|_| "This image is no longer available.".to_string())?
    } else {
        let name = percent_decode(url.path().trim_start_matches('/'));
        services.attachments.saved_path(&services.attachments.directory().join(name).to_string_lossy()).ok_or("This image is no longer available.")?
    };
    let bytes = if !message_image && query("taskId").is_some() {
        match scoped(source) {
            Some(read) => Some(read.await.map_err(|_| "This image is no longer available.".to_string())?),
            None => None,
        }
    } else {
        None
    };
    let extension = original.extension().map(|extension| extension.to_string_lossy().into_owned()).unwrap_or_else(|| "png".into());
    let default_path = downloads_directory().join(format!("image.{extension}"));
    Ok(ImageDownload { original, bytes, extension, default_path })
}

/// Writes the image where the user chose.
pub async fn save_image_download(download: &ImageDownload, destination: &Path) -> Result<(), String> {
    match &download.bytes {
        Some(bytes) => tokio::fs::write(destination, bytes).await.map_err(|error| error.to_string()),
        None => tokio::fs::copy(&download.original, destination).await.map(|_| ()).map_err(|error| error.to_string()),
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if let Some(byte) = bytes.get(index + 1..index + 3).and_then(|hex| std::str::from_utf8(hex).ok()).and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
