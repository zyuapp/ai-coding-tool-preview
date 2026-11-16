//! Images a composer attaches, kept under `<userData>/attachments` by names of this app's own making.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_ATTACHMENT_ENCODED_BYTES: usize = (MAX_ATTACHMENT_BYTES as usize).div_ceil(3) * 4;
const MAX_CONTEXT_BYTES: u64 = 128_000;
pub const MAX_SCREENSHOT_CONTEXT_TEXT: usize = 12_000;
pub const MAX_SCREENSHOT_CONTEXT_NAME: usize = 512;
const CONTEXT_SUFFIX: &str = ".context.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AccessibilitySnapshot {
    Captured { text: String, truncated: bool },
    Unavailable { reason: String },
}

/// Context belongs to the captured pixels, including when an image is recalled or annotated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotContext {
    pub version: u8,
    pub platform: String,
    pub app: String,
    pub title: String,
    pub captured_at: i64,
    pub accessibility: AccessibilitySnapshot,
}

impl ScreenshotContext {
    pub fn is_valid(&self) -> bool {
        let accessibility = match &self.accessibility {
            AccessibilitySnapshot::Captured { text, .. } => !text.is_empty() && text.encode_utf16().count() <= MAX_SCREENSHOT_CONTEXT_TEXT,
            AccessibilitySnapshot::Unavailable { reason } => ["permission", "timeout", "busy", "window", "empty", "failed"].contains(&reason.as_str()),
        };
        self.version == 1
            && ["macos", "linux-x11", "linux-hyprland"].contains(&self.platform.as_str())
            && self.app.encode_utf16().count() <= MAX_SCREENSHOT_CONTEXT_NAME
            && self.title.encode_utf16().count() <= MAX_SCREENSHOT_CONTEXT_NAME
            && (0..=8_640_000_000_000_000).contains(&self.captured_at)
            && accessibility
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    #[error("Invalid attachment name.")]
    InvalidName,
    #[error("Attachment is empty or too large.")]
    Size,
    #[error("Attachment payload is not base64.")]
    NotBase64,
    #[error("That image is not one this app is keeping.")]
    NotKept,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub fn is_saved_attachment_name(name: &str) -> bool {
    name.strip_suffix(".png").is_some_and(|stem| !stem.is_empty() && stem.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
}

/// The last segment of a path, split on either separator.
pub fn attachment_name(file: &str) -> &str {
    file.rsplit(['/', '\\']).next().unwrap_or("")
}

pub fn is_base64(data: &str) -> bool {
    let body = data.trim_end_matches('=');
    !body.is_empty() && data.len() - body.len() <= 2 && body.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
}

#[derive(Debug, Clone)]
pub struct AttachmentStore {
    directory: PathBuf,
}

impl AttachmentStore {
    pub fn new(user_data: &Path) -> Self {
        Self { directory: user_data.join("attachments") }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Reads only a saved name, never an arbitrary path supplied by another computer.
    pub async fn read_saved(&self, name: &str) -> Result<Vec<u8>, AttachmentError> {
        if !is_saved_attachment_name(name) {
            return Err(AttachmentError::InvalidName);
        }
        let mut file = tokio::fs::File::open(self.directory.join(name)).await?;
        let metadata = file.metadata().await?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentError::Size);
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes).await?;
        Ok(bytes)
    }

    /// A caller may only name files this app wrote into the attachments directory; anything else is None.
    pub fn saved_path(&self, file: &str) -> Option<PathBuf> {
        let name = attachment_name(file);
        if !is_saved_attachment_name(name) {
            return None;
        }
        let saved = self.directory.join(name);
        (crate::paths::resolve(Path::new(file)) == saved).then_some(saved)
    }

    /// One of this app's own attachments as base64, refusing any other path.
    pub async fn read_kept(&self, file: &str) -> Result<String, AttachmentError> {
        let saved = self.saved_path(file).ok_or(AttachmentError::NotKept)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(tokio::fs::read(saved).await?))
    }

    /// Puts base64 PNG bytes in the attachments directory under a name of this app's own making.
    pub async fn write(&self, data: &str, context: Option<&ScreenshotContext>) -> Result<PathBuf, AttachmentError> {
        if data.is_empty() || data.len() > MAX_ATTACHMENT_ENCODED_BYTES {
            return Err(AttachmentError::Size);
        }
        if !is_base64(data) {
            return Err(AttachmentError::NotBase64);
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| AttachmentError::NotBase64)?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentError::Size);
        }
        tokio::fs::create_dir_all(&self.directory).await?;
        let file = self.directory.join(format!("{}.png", uuid::Uuid::new_v4()));
        tokio::fs::write(&file, bytes).await?;
        // Context is optional: a sidecar storage failure must not discard a successful screenshot.
        if let Some(context) = context {
            if let Ok(json) = serde_json::to_string(context) {
                let _ = tokio::fs::write(format!("{}{CONTEXT_SUFFIX}", file.to_string_lossy()), json).await;
            }
        }
        Ok(file)
    }

    /// Saves a composer's image. An annotated copy of a kept image inherits that image's context.
    pub async fn save(&self, data: &str, original: Option<&str>) -> Result<PathBuf, AttachmentError> {
        let context = match original {
            Some(original) => {
                if self.saved_path(original).is_none() {
                    return Err(AttachmentError::NotKept);
                }
                self.read_context(original).await
            }
            None => None,
        };
        self.write(data, context.as_ref()).await
    }

    /// Immutable context follows the PNG path across drafts, history, and app restarts.
    pub async fn read_context(&self, file: &str) -> Option<ScreenshotContext> {
        let saved = self.saved_path(file)?;
        let sidecar = format!("{}{CONTEXT_SUFFIX}", saved.to_string_lossy());
        let metadata = tokio::fs::metadata(&sidecar).await.ok()?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CONTEXT_BYTES {
            return None;
        }
        let context: ScreenshotContext = serde_json::from_slice(&tokio::fs::read(&sidecar).await.ok()?).ok()?;
        context.is_valid().then_some(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ScreenshotContext {
        ScreenshotContext { version: 1, platform: "macos".into(), app: "Safari".into(), title: "Docs".into(), captured_at: 1, accessibility: AccessibilitySnapshot::Unavailable { reason: "permission".into() } }
    }

    #[tokio::test]
    async fn an_annotated_copy_inherits_its_originals_context() {
        let dir = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(dir.path());
        let original = store.write("aGVsbG8=", Some(&context())).await.unwrap();
        let original = original.to_string_lossy().into_owned();
        assert_eq!(store.read_context(&original).await, Some(context()));
        let copy = store.save("aGk=", Some(&original)).await.unwrap();
        assert_eq!(store.read_context(&copy.to_string_lossy()).await, Some(context()));
        assert_eq!(store.read_kept(&copy.to_string_lossy()).await.unwrap(), "aGk=");
    }

    #[tokio::test]
    async fn refuses_paths_and_payloads_it_did_not_make() {
        let dir = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(dir.path());
        assert!(store.saved_path("/etc/passwd").is_none());
        assert!(store.saved_path(&format!("{}/../attachments/x.png", store.directory().to_string_lossy())).is_some());
        assert!(store.saved_path("/elsewhere/abc.png").is_none());
        assert!(matches!(store.write("not base64!", None).await, Err(AttachmentError::NotBase64)));
        assert!(matches!(store.save("aGk=", Some("/tmp/x.png")).await, Err(AttachmentError::NotKept)));
        assert!(matches!(store.read_saved("../x.png").await, Err(AttachmentError::InvalidName)));
    }
}
