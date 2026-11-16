//! Files and images handed to a composer: an image is kept in the app's own images so it previews
//! and can be drawn on; anything else is named where it sits (dropped-files.ts).

use std::io::Cursor;
use std::path::Path;

use act_core::commands::PastedImage;
use act_core::AppCommand;
use base64::Engine;

use super::Host;

const MAX_IMAGE_EDGE: u32 = 4_096;

fn is_image(path: &Path) -> bool {
    matches!(path.extension().and_then(|ext| ext.to_str()).map(str::to_lowercase).as_deref(), Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff"))
}

/// Image bytes as base64 PNG, scaled to fit the largest edge the app keeps. A PNG small enough is kept as it is.
fn png_payload(bytes: &[u8], png: bool) -> Option<String> {
    let image = image::load_from_memory(bytes).ok()?;
    let largest = image.width().max(image.height());
    if png && largest <= MAX_IMAGE_EDGE {
        return Some(base64::engine::general_purpose::STANDARD.encode(bytes));
    }
    let image = if largest > MAX_IMAGE_EDGE { image.resize(MAX_IMAGE_EDGE, MAX_IMAGE_EDGE, image::imageops::FilterType::Lanczos3) } else { image };
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(out.into_inner()))
}

fn file_payload(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let png = path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("png"));
    png_payload(&bytes, png)
}

/// Keeps each image of a drop and names the rest; `held` are sources the composer already shows.
pub async fn drop_files(host: &Host, task_id: Option<String>, paths: Vec<String>, held: Vec<String>) {
    let mut held = held;
    let mut named = Vec::new();
    for source in paths {
        if held.contains(&source) {
            continue;
        }
        let path = std::path::PathBuf::from(&source);
        if is_image(&path) {
            let payload = tokio::task::spawn_blocking({
                let path = path.clone();
                move || file_payload(&path)
            })
            .await
            .ok()
            .flatten();
            if let Some(payload) = payload {
                if let Ok(saved) = host.services.attachments.write(&payload, None).await {
                    let label = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                    held.push(source.clone());
                    host.sink.command(AppCommand::ImageAdd { task_id: task_id.clone(), path: saved.to_string_lossy().into_owned(), label, source: Some(source) });
                    continue;
                }
            }
        }
        named.push(source);
    }
    if named.is_empty() {
        return;
    }
    let files = super::window_desktop::describe_files(named).await;
    if !files.is_empty() {
        host.sink.command(AppCommand::FileAttach { task_id, files });
    }
}

/// Keeps pasted images; one that cannot be read leaves a notice on the composer.
pub async fn paste_images(host: &Host, task_id: Option<String>, images: Vec<PastedImage>) {
    for image in images {
        let payload = tokio::task::spawn_blocking(move || base64::engine::general_purpose::STANDARD.decode(&image.data).ok().and_then(|bytes| png_payload(&bytes, image.png))).await.ok().flatten();
        let Some(payload) = payload else {
            host.sink.command(AppCommand::AttachmentsNotice { task_id: task_id.clone(), message: Some("Could not read the pasted image.".into()) });
            continue;
        };
        match host.services.attachments.write(&payload, None).await {
            Ok(saved) => host.sink.command(AppCommand::ImageAdd { task_id: task_id.clone(), path: saved.to_string_lossy().into_owned(), label: "Pasted image".into(), source: None }),
            Err(error) => host.sink.command(AppCommand::AttachmentsNotice { task_id: task_id.clone(), message: Some(error.to_string()) }),
        }
    }
}
