//! Saves an image Codex generated where the app keeps generated images, and links it in the thread.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::Value;

pub const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_FILE_PATH: usize = 4_096;
const MAX_FILE_CANDIDATES: usize = 256;

pub fn is_message_image_file(file: &str) -> bool {
    let lower = file.to_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp"]
        .iter()
        .any(|extension| lower.ends_with(extension))
}

fn candidates(roots: &[String], named: &str) -> Vec<PathBuf> {
    let segments: Vec<&str> = named
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let resolve = |root: &str, tail: &str| super::home::normalize(&Path::new(root).join(tail));
    let mut found: Vec<PathBuf> = if Path::new(named).is_absolute() {
        vec![super::home::normalize(Path::new(named))]
    } else {
        roots.iter().map(|root| resolve(root, named)).collect()
    };
    for root in roots {
        for start in 1..segments.len() {
            found.push(resolve(root, &segments[start..].join("/")));
        }
    }
    let mut unique = Vec::new();
    for path in found {
        if !unique.contains(&path) {
            unique.push(path);
        }
    }
    unique.truncate(MAX_FILE_CANDIDATES);
    unique
}

/// The file a path names, looked for in the given checkouts before being taken as written.
pub async fn openable_file(roots: &[String], candidate: &str) -> anyhow::Result<PathBuf> {
    if candidate.is_empty() || candidate.len() > MAX_FILE_PATH {
        anyhow::bail!("Invalid file path.");
    }
    let named = match candidate.strip_prefix("~/") {
        Some(rest) => crate::claude::services::home_dir()
            .join(rest)
            .to_string_lossy()
            .into_owned(),
        None => candidate.to_string(),
    };
    for target in candidates(roots, &named) {
        if tokio::fs::metadata(&target)
            .await
            .is_ok_and(|meta| meta.is_file())
        {
            return Ok(tokio::fs::canonicalize(&target).await.unwrap_or(target));
        }
    }
    anyhow::bail!("AI Coding Tool could not find {candidate} on this machine.")
}

async fn read_image_file(file: &Path) -> anyhow::Result<Vec<u8>> {
    let meta = tokio::fs::metadata(file).await?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_IMAGE_BYTES {
        anyhow::bail!("Image is too large or unavailable.");
    }
    Ok(tokio::fs::read(file).await?)
}

fn percent_encode_segment(segment: &str) -> String {
    segment
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// What the thread shows for a finished image item: a link to the saved file, or why there is none.
pub async fn codex_image_output(
    item: &Value,
    root: &str,
    directory: Option<&str>,
) -> anyhow::Result<String> {
    if item["failure"]["type"] == "usageLimitExceeded" {
        return Ok(
            "Image generation failed: the Codex image-generation usage limit has been reached."
                .into(),
        );
    }
    match item["status"].as_str() {
        Some("failed") => {
            return Ok("Image generation failed. Codex did not return an image.".into())
        }
        Some("cancelled" | "interrupted") => return Ok("Image generation was cancelled.".into()),
        _ => {}
    }
    let result = item["result"].as_str().unwrap_or_default();
    let saved = item["savedPath"].as_str().filter(|path| !path.is_empty());
    if saved.is_none() && result.is_empty() {
        return Ok("Image generation finished without returning an image.".into());
    }
    let Some(directory) = directory.filter(|dir| Path::new(dir).is_absolute()) else {
        anyhow::bail!("Generated image storage is unavailable.")
    };
    let mut bytes = None;
    let mut extension = ".png".to_string();
    if let Some(saved) = saved {
        let read = async {
            if !is_message_image_file(saved) {
                anyhow::bail!("Unsupported image file type.");
            }
            let roots: Vec<String> = if root.is_empty() {
                Vec::new()
            } else {
                vec![root.to_string()]
            };
            read_image_file(&openable_file(&roots, saved).await?).await
        }
        .await;
        match read {
            Ok(read) => {
                bytes = Some(read);
                extension = Path::new(saved)
                    .extension()
                    .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
                    .unwrap_or_default();
            }
            Err(error) if result.is_empty() => return Err(error),
            Err(_) => {}
        }
    }
    let bytes = match bytes {
        Some(bytes) => bytes,
        None => {
            if result.len() as u64 > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
                anyhow::bail!("Image is too large.");
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(result)
                .map_err(|_| anyhow::anyhow!("Image payload is not base64."))?;
            if decoded.is_empty() || decoded.len() as u64 > MAX_IMAGE_BYTES {
                anyhow::bail!("Image is empty or too large.");
            }
            decoded
        }
    };
    tokio::fs::create_dir_all(directory).await?;
    let file = Path::new(directory).join(format!("{}{extension}", uuid::Uuid::new_v4()));
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
        .await?;
    tokio::fs::write(&file, bytes).await?;
    let href = file
        .to_string_lossy()
        .split('/')
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("/");
    Ok(format!("[Generated image](<{href}>)"))
}
