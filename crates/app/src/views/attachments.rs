//! Files handed to a composer by the file panel, a drop or a paste, sent on as commands
//! (dropped-files.ts).

use std::path::PathBuf;

use act_core::commands::PastedImage;
use act_core::AppCommand;
use base64::Engine;
use gpui::App;

use crate::workspace::dispatch;

/// `task` is the dock tab whose composer takes them; None is the page's composer.
pub fn attach_paths(paths: Vec<PathBuf>, task: Option<String>, cx: &mut App) {
    let paths = paths.into_iter().map(|path| path.to_string_lossy().into_owned()).collect();
    dispatch(AppCommand::AttachmentsDrop { task_id: task, paths }, cx);
}

/// Images pasted from the clipboard, kept like dropped ones.
pub fn attach_images(images: Vec<gpui::Image>, task: Option<String>, cx: &mut App) {
    let images = images.into_iter().map(|image| PastedImage { data: base64::engine::general_purpose::STANDARD.encode(&image.bytes), png: image.format == gpui::ImageFormat::Png }).collect();
    dispatch(AppCommand::AttachmentsPasteImages { task_id: task, images }, cx);
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
