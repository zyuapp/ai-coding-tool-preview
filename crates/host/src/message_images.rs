//! Copies of the images a reply links to, kept under `<userData>/message-images` with a preview cut
//! from each, so the transcript still shows them once the original moves or its computer goes away.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine;
use futures::future::{BoxFuture, Shared};
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_MESSAGE_IMAGES: usize = 20;
const MAX_PIXELS: u64 = 64_000_000;
const PREVIEW_WIDTH: f64 = 640.0;
const PREVIEW_HEIGHT: f64 = 360.0;

pub fn mime_of(extension: &str) -> Option<&'static str> {
    match extension {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

pub fn is_message_image_file(file: &str) -> bool {
    let lower = file.to_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp"].iter().any(|extension| lower.ends_with(extension))
}

fn has_url_scheme(file: &str) -> bool {
    let Some((scheme, _)) = file.split_once("://") else { return false };
    let mut characters = scheme.chars();
    characters.next().is_some_and(|first| first.is_ascii_alphabetic()) && characters.all(|character| character.is_ascii_alphanumeric() || "+.-".contains(character))
}

/// Shared by the image reader and the paired computer's query boundary.
pub fn is_message_image_reference(file: &str, root: &str, message: &str) -> bool {
    file.encode_utf16().count() <= 4096 && is_message_image_file(file) && !has_url_scheme(file) && root.encode_utf16().count() <= 4096 && !message.is_empty() && message.encode_utf16().count() <= 256
}

/// Bounds the read on an open handle even if the original changes during it.
pub async fn read_image_file(file: &Path) -> Result<Vec<u8>, String> {
    let mut handle = tokio::fs::File::open(file).await.map_err(|error| error.to_string())?;
    let metadata = handle.metadata().await.map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_IMAGE_BYTES {
        return Err("Image is too large or unavailable.".into());
    }
    let mut bytes = vec![0u8; metadata.len() as usize];
    handle.read_exact(&mut bytes).await.map_err(|_| "Image changed while reading.".to_string())?;
    Ok(bytes)
}

/// A preview no larger than 640×360, or a refusal for an image too large to decode.
pub fn message_thumbnail(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|error| error.to_string())?;
    let (width, height) = reader.into_dimensions().map_err(|_| "Image cannot be previewed.".to_string())?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err("Image cannot be previewed.".into());
    }
    let decoded = image::load_from_memory(bytes).map_err(|_| "Image cannot be previewed.".to_string())?;
    let scale = (PREVIEW_WIDTH / f64::from(width)).min(PREVIEW_HEIGHT / f64::from(height)).min(1.0);
    let target = |side: u32| ((f64::from(side) * scale).round() as u32).max(1);
    let resized = decoded.resize_exact(target(width), target(height), image::imageops::FilterType::Triangle);
    let mut png = Vec::new();
    resized.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|error| error.to_string())?;
    Ok(png)
}

/// An image as the window reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageImage {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

/// The original as it crosses from the computer holding the thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageImageData {
    pub data: String,
    pub content_type: String,
}

/// Validates a transferred original before it is kept under the same key.
pub fn message_image_bytes(value: &MessageImageData, file: &str) -> Result<Vec<u8>, String> {
    let extension = Path::new(file).extension().map(|extension| extension.to_string_lossy().to_lowercase()).unwrap_or_default();
    let limit = (MAX_IMAGE_BYTES as usize).div_ceil(3) * 4;
    let valid = mime_of(&extension) == Some(value.content_type.as_str()) && !value.data.is_empty() && value.data.len() <= limit && value.data.len().is_multiple_of(4) && crate::attachments::is_base64(&value.data);
    if !valid {
        return Err("Invalid image bytes.".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(&value.data).map_err(|_| "Invalid image bytes.".to_string())?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("Image is too large or unavailable.".into());
    }
    Ok(bytes)
}

struct Request {
    file: String,
    root: String,
    key: String,
    extension: String,
}

type Copying = Shared<BoxFuture<'static, Result<PathBuf, String>>>;

/// Where copies go, and how a preview is cut from one. A host with no decoder keeps no previews.
pub struct MessageImageStore {
    directory: PathBuf,
    thumbnail: fn(&[u8]) -> Result<Vec<u8>, String>,
    pending: Mutex<HashMap<String, Copying>>,
    /// One decode at a time, so a reply of full-page captures cannot multiply peak memory use.
    decoding: Arc<tokio::sync::Mutex<()>>,
}

impl MessageImageStore {
    pub fn new(user_data: &Path) -> Arc<Self> {
        Self::with(user_data.join("message-images"), message_thumbnail)
    }

    pub fn with(directory: PathBuf, thumbnail: fn(&[u8]) -> Result<Vec<u8>, String>) -> Arc<Self> {
        Arc::new(Self { directory, thumbnail, pending: Mutex::new(HashMap::new()), decoding: Arc::new(tokio::sync::Mutex::new(())) })
    }

    fn request(file: &str, root: &str, message: &str) -> Result<Request, String> {
        if !is_message_image_reference(file, root, message) {
            return Err("Invalid image reference.".into());
        }
        let extension = Path::new(file).extension().map(|extension| extension.to_string_lossy().to_lowercase()).unwrap_or_default();
        let key = hex::encode(Sha256::digest(serde_json::to_string(&[message, file]).unwrap_or_default().as_bytes()));
        Ok(Request { file: file.to_string(), root: root.to_string(), key, extension })
    }

    /// Where the copy of a reply's image and its preview are kept, whether or not they exist yet.
    pub fn copy_paths(&self, file: &str, root: &str, message: &str) -> Option<(PathBuf, PathBuf)> {
        let request = Self::request(file, root, message).ok()?;
        Some((self.directory.join(format!("{}.{}", request.key, request.extension)), self.directory.join(format!("{}-thumb.png", request.key))))
    }

    /// Keeps a copy of the image a reply links to. `load` fetches the original from the computer that
    /// holds it; without it the file is read here. Concurrent asks for one image share one copy.
    pub async fn preserve(self: &Arc<Self>, file: &str, root: &str, message: &str, load: Option<BoxFuture<'static, Result<Vec<u8>, String>>>) -> Result<PathBuf, String> {
        let request = Self::request(file, root, message)?;
        let key = request.key.clone();
        let copying = {
            let mut pending = self.pending.lock().unwrap();
            pending.entry(key.clone()).or_insert_with(|| self.clone().copy(request, load).boxed().shared()).clone()
        };
        let result = copying.await;
        self.pending.lock().unwrap().remove(&key);
        result
    }

    /// Decodes one at a time and never stops at the first image that will not copy.
    pub async fn preserve_all(self: &Arc<Self>, files: &[String], root: &str, message: &str) -> Result<(), String> {
        if files.len() > MAX_MESSAGE_IMAGES {
            return Err("Invalid image references.".into());
        }
        for file in files {
            let _ = self.preserve(file, root, message, None).await;
        }
        Ok(())
    }

    async fn copy(self: Arc<Self>, request: Request, load: Option<BoxFuture<'static, Result<Vec<u8>, String>>>) -> Result<PathBuf, String> {
        let destination = self.directory.join(format!("{}.{}", request.key, request.extension));
        if tokio::fs::metadata(&destination).await.is_ok_and(|metadata| metadata.is_file()) {
            return Ok(destination);
        }
        // Network waits must not hold up this computer's thumbnail decoder.
        let remote = match load {
            Some(load) => Some(load.await?),
            None => None,
        };
        let _decoding = self.decoding.lock().await;
        let bytes = match remote {
            Some(bytes) => bytes,
            None => {
                let roots: Vec<String> = if request.root.is_empty() { Vec::new() } else { vec![request.root.clone()] };
                let original = crate::files::openable_file(&roots, &request.file).await.map_err(|error| error.0)?;
                read_image_file(&original).await?
            }
        };
        if bytes.is_empty() || bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err("Image is too large or unavailable.".into());
        }
        let thumbnail = self.thumbnail;
        let preview = tokio::task::spawn_blocking({
            let bytes = bytes.clone();
            move || thumbnail(&bytes)
        })
        .await
        .map_err(|error| error.to_string())?;
        tokio::fs::create_dir_all(&self.directory).await.map_err(|error| error.to_string())?;
        if let Ok(preview) = preview {
            tokio::fs::write(self.directory.join(format!("{}-thumb.png", request.key)), preview).await.map_err(|error| error.to_string())?;
        }
        let staging = PathBuf::from(format!("{}.tmp", destination.to_string_lossy()));
        tokio::fs::write(&staging, bytes).await.map_err(|error| error.to_string())?;
        tokio::fs::rename(&staging, &destination).await.map_err(|error| error.to_string())?;
        Ok(destination)
    }

    /// A reply's linked image, its preview or its original. A preview that could not be cut reads as the original.
    pub async fn read(self: &Arc<Self>, file: &str, root: &str, message: &str, thumbnail: bool) -> Result<MessageImage, String> {
        let request = Self::request(file, root, message)?;
        let original = self.preserve(file, root, message, None).await?;
        if thumbnail {
            match read_image_file(&self.directory.join(format!("{}-thumb.png", request.key))).await {
                Ok(bytes) => return Ok(MessageImage { bytes, content_type: "image/png" }),
                Err(_) if !self.directory.join(format!("{}-thumb.png", request.key)).exists() => {}
                Err(error) => return Err(error),
            }
        }
        Ok(MessageImage { bytes: read_image_file(&original).await?, content_type: mime_of(&request.extension).unwrap_or("application/octet-stream") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image).write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        bytes
    }

    #[test]
    fn a_preview_fits_inside_640_by_360() {
        let preview = image::load_from_memory(&message_thumbnail(&png(1280, 1440)).unwrap()).unwrap();
        assert_eq!((preview.width(), preview.height()), (320, 360));
        let small = image::load_from_memory(&message_thumbnail(&png(10, 10)).unwrap()).unwrap();
        assert_eq!((small.width(), small.height()), (10, 10));
    }

    #[test]
    fn references_must_name_a_local_image() {
        assert!(is_message_image_reference("shots/a.PNG", "/repo", "m1"));
        assert!(!is_message_image_reference("https://x/a.png", "/repo", "m1"));
        assert!(!is_message_image_reference("a.txt", "/repo", "m1"));
        assert!(!is_message_image_reference("a.png", "/repo", ""));
    }

    #[tokio::test]
    async fn keeps_a_copy_and_its_preview_once_the_original_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("shot.png"), png(800, 400)).unwrap();
        let store = MessageImageStore::new(&dir.path().join("data"));
        let root_text = root.to_string_lossy().into_owned();
        store.preserve_all(&["shot.png".into()], &root_text, "m1").await.unwrap();
        std::fs::remove_file(root.join("shot.png")).unwrap();
        let preview = store.read("shot.png", &root_text, "m1", true).await.unwrap();
        assert_eq!(image::load_from_memory(&preview.bytes).unwrap().width(), 640);
        let original = store.read("shot.png", &root_text, "m1", false).await.unwrap();
        assert_eq!((original.content_type, image::load_from_memory(&original.bytes).unwrap().width()), ("image/png", 800));
    }

    #[test]
    fn transferred_bytes_must_match_their_type() {
        let data = base64::engine::general_purpose::STANDARD.encode(png(1, 1));
        let good = MessageImageData { data: data.clone(), content_type: "image/png".into() };
        assert!(message_image_bytes(&good, "a.png").is_ok());
        assert!(message_image_bytes(&good, "a.jpg").is_err());
    }
}
