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
