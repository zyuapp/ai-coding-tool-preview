//! What each composer holds before it sends: text, highlights, pastes, images and files, per thread.

use crate::commands::AppCommand;
use crate::domain::conversation::{Annotation, AnnotationAnchor, AttachedFile, PastedText, StagedImage, MAX_ATTACHED_FILES, MAX_ATTACHMENTS};
use crate::reducer::run_queue::clamp_quote;
use crate::reducer::sending::prompt_key;
use crate::effects::EffectWork;
use crate::reducer::Step;
use crate::state::{tab_holder_of, AnnotationReveal, AttachmentSendState, WorkspaceState};
use crate::util::random_id;

fn set_list<T>(map: &mut crate::util::Map<Vec<T>>, key: &str, items: Vec<T>) {
    if items.is_empty() {
        map.shift_remove(key);
    } else {
        map.insert(key.to_string(), items);
    }
}

/// One composer's drafts of each kind given, with an entry dropped once nothing is left in it.
pub fn set_drafts(state: &mut WorkspaceState, key: &str, annotations: Option<Vec<Annotation>>, pastes: Option<Vec<PastedText>>, images: Option<Vec<StagedImage>>, files: Option<Vec<AttachedFile>>) {
    if let Some(annotations) = annotations {
        set_list(&mut state.annotations, key, annotations);
    }
    if let Some(pastes) = pastes {
        set_list(&mut state.pastes, key, pastes);
    }
    if let Some(images) = images {
        set_list(&mut state.images, key, images);
    }
    if let Some(files) = files {
        set_list(&mut state.files, key, files);
    }
}

/// Keeps only the drafts each predicate accepts.
pub fn retain_drafts(state: &mut WorkspaceState, key: &str, annotation: impl Fn(&Annotation) -> bool, paste: impl Fn(&PastedText) -> bool, image: impl Fn(&StagedImage) -> bool, file: impl Fn(&AttachedFile) -> bool) {
    let annotations = state.annotations.get(key).cloned().unwrap_or_default().into_iter().filter(|item| annotation(item)).collect();
    let pastes = state.pastes.get(key).cloned().unwrap_or_default().into_iter().filter(|item| paste(item)).collect();
    let images = state.images.get(key).cloned().unwrap_or_default().into_iter().filter(|item| image(item)).collect();
    let files = state.files.get(key).cloned().unwrap_or_default().into_iter().filter(|item| file(item)).collect();
    set_drafts(state, key, Some(annotations), Some(pastes), Some(images), Some(files));
}

/// A composer that has just sent: text, annotations, pastes, images, and attached files all go.
pub fn cleared_draft(state: &mut WorkspaceState, key: &str) {
    state.set_prompt(key, "");
    set_drafts(state, key, Some(Vec::new()), Some(Vec::new()), Some(Vec::new()), Some(Vec::new()));
}

const TOO_MANY_IMAGES_ERROR: &str = "You can attach up to 6 images.";
const TOO_MANY_FILES_ERROR: &str = "You can attach up to 10 files.";

/// Asks the transcript that drafted an annotation to show its passage.
fn reveal_annotation(state: &mut WorkspaceState, key: &str, annotation_id: &str) {
    let anchor = state.annotations.get(key).and_then(|items| items.iter().find(|item| item.id == annotation_id)).and_then(|item| item.anchor.clone());
    let Some(AnnotationAnchor::Message { message_id, .. }) = anchor else { return };
    let count = state.annotation_reveal.as_ref().map(|reveal| reveal.count).unwrap_or(0) + 1;
    state.annotation_reveal = Some(AnnotationReveal { task_id: key.into(), annotation_id: annotation_id.into(), message_id, count });
}

/// The caret goes to the composer that took an annotation, a tab's own when the thread is a tab.
fn focus_after_annotation(state: &mut WorkspaceState, key: &str) {
    match tab_holder_of(state, key) {
        Some(holder) => crate::reducer::dock::focused_tab(state, &holder, key),
        None => state.composer_focus += 1,
    }
}

pub fn reduce_composer(state: &mut WorkspaceState, command: &AppCommand) -> Step {
    let key = command.task_id().map(String::from).unwrap_or_else(|| prompt_key(state));
    match command {
        AppCommand::AnnotationAdd { quote, note, anchor, .. } => {
            let quote = clamp_quote(quote);
            if quote.is_empty() {
                return Step::none();
            }
            let mut items = state.annotations.get(&key).cloned().unwrap_or_default();
            items.push(Annotation { id: random_id(), quote, note: note.clone().unwrap_or_default(), anchor: anchor.clone() });
            set_list(&mut state.annotations, &key, items);
            if !matches!(anchor, Some(AnnotationAnchor::Diff { .. })) {
                focus_after_annotation(state, &key);
            }
            Step::none()
        }
        AppCommand::AnnotationNote { annotation_id, note, .. } => {
            if let Some(item) = state.annotations.get_mut(&key).and_then(|items| items.iter_mut().find(|item| &item.id == annotation_id)) {
                item.note = note.clone();
            }
            Step::none()
        }
        AppCommand::AnnotationRemove { annotation_id, .. } => {
            let items = state.annotations.get(&key).cloned().unwrap_or_default().into_iter().filter(|item| &item.id != annotation_id).collect();
            set_list(&mut state.annotations, &key, items);
            Step::none()
        }
        AppCommand::AnnotationRecall { annotations, .. } => {
            set_list(&mut state.annotations, &key, annotations.clone());
            Step::none()
        }
        AppCommand::AnnotationReveal { annotation_id, .. } => {
            reveal_annotation(state, &key, annotation_id);
            Step::none()
        }
        AppCommand::PasteAdd { text, .. } => {
            if text.is_empty() {
                return Step::none();
            }
            let mut items = state.pastes.get(&key).cloned().unwrap_or_default();
            items.push(PastedText { id: random_id(), text: text.clone() });
            set_list(&mut state.pastes, &key, items);
            Step::none()
        }
        AppCommand::PasteRemove { paste_id, .. } => {
            let items = state.pastes.get(&key).cloned().unwrap_or_default().into_iter().filter(|item| &item.id != paste_id).collect();
            set_list(&mut state.pastes, &key, items);
            Step::none()
        }
        AppCommand::PasteRecall { pastes, .. } => {
            set_list(&mut state.pastes, &key, pastes.clone());
            Step::none()
        }
        AppCommand::ImageAdd { path, label, source, .. } => {
            let source = &source.clone().filter(|source| !source.is_empty());
            let held = state.images.get(&key).cloned().unwrap_or_default();
            if path.is_empty() || source.as_ref().is_some_and(|source| held.iter().any(|image| image.source.as_ref() == Some(source))) {
                return Step::none();
            }
            if held.len() >= MAX_ATTACHMENTS {
                return Step::refuse(TOO_MANY_IMAGES_ERROR);
            }
            let mut images = held;
            images.push(StagedImage { id: random_id(), path: path.clone(), label: label.clone(), source: source.clone() });
            set_list(&mut state.images, &key, images);
            state.composer_focus += 1;
            Step::none()
        }
        AppCommand::ImageRemove { image_id, .. } => {
            let items = state.images.get(&key).cloned().unwrap_or_default().into_iter().filter(|item| &item.id != image_id).collect();
            set_list(&mut state.images, &key, items);
            Step::none()
        }
        AppCommand::ImageRecall { paths, .. } => {
            let images = paths.iter().map(|path| StagedImage { id: random_id(), path: path.clone(), label: String::new(), source: None }).collect();
            set_list(&mut state.images, &key, images);
            Step::none()
        }
        AppCommand::FileAttach { files, .. } => {
            let held = state.files.get(&key).cloned().unwrap_or_default();
            let mut fresh: Vec<crate::domain::conversation::AttachedFileDraft> = Vec::new();
            for file in files {
                if file.path.is_empty() || file.name.is_empty() || held.iter().any(|item| item.path == file.path) || fresh.iter().any(|item| item.path == file.path) {
                    continue;
                }
                fresh.push(file.clone());
            }
            if fresh.is_empty() {
                return Step::none();
            }
            let room = MAX_ATTACHED_FILES.saturating_sub(held.len());
            if room == 0 {
                return Step::refuse(TOO_MANY_FILES_ERROR);
            }
            let overflow = fresh.len() > room;
            let mut attached = held;
            attached.extend(fresh.into_iter().take(room).map(|file| AttachedFile { id: random_id(), path: file.path, name: file.name, folder: file.folder }));
            set_list(&mut state.files, &key, attached);
            if overflow {
                return Step::refuse(TOO_MANY_FILES_ERROR);
            }
            state.composer_focus += 1;
            Step::none()
        }
        AppCommand::FileDetach { file_id, .. } => {
            let items = state.files.get(&key).cloned().unwrap_or_default().into_iter().filter(|item| &item.id != file_id).collect();
            set_list(&mut state.files, &key, items);
            Step::none()
        }
        AppCommand::FileRecall { files, .. } => {
            set_list(&mut state.files, &key, files.clone());
            Step::none()
        }
        AppCommand::SetPrompt { prompt, .. } => {
            state.set_prompt(&key, prompt);
            Step::none()
        }
        AppCommand::ReadingPoint { task_id, point } => {
            let well_formed = point.as_ref().is_none_or(|point| !point.anchor.is_empty() && point.depth.is_finite());
            if !well_formed || state.thread(task_id).is_none() {
                return Step::none();
            }
            if state.reading_points.get(task_id).cloned().flatten() == *point {
                return Step::none();
            }
            state.reading_points.insert(task_id.clone(), point.clone());
            Step::none()
        }
        AppCommand::AttachmentsDrop { task_id, paths } => {
            if paths.is_empty() {
                return Step::none();
            }
            let key = task_id.clone().unwrap_or_else(|| prompt_key(state));
            let held = state.images.get(&key).map(|images| images.iter().filter_map(|image| image.source.clone()).collect()).unwrap_or_default();
            Step::effect(EffectWork::DropAttachments { task_id: task_id.clone(), paths: paths.clone(), held })
        }
        AppCommand::AttachmentsPasteImages { task_id, images } => {
            if images.is_empty() {
                return Step::none();
            }
            Step::effect(EffectWork::PasteImages { task_id: task_id.clone(), images: images.clone() })
        }
        AppCommand::AttachmentsNotice { task_id, message } => {
            let composer = task_id.clone().unwrap_or_else(|| crate::state::MAIN_COMPOSER.to_string());
            let mut send = state.attachment_sends.get(&composer).cloned().unwrap_or_default();
            if send.error == *message {
                return Step::none();
            }
            send.error = message.clone();
            state.attachment_sends.insert(composer, send);
            Step::none()
        }
        _ => Step::none(),
    }
}

/// Where one composer's images stand between its send and the run.
pub fn attachment_send(state: &WorkspaceState, key: Option<&str>) -> AttachmentSendState {
    state.attachment_send_for(key)
}
