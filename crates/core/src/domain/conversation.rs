//! Messages in a thread, and what a composer attaches to one.

use crate::shared::Shared;
use serde::{Deserialize, Serialize};

use super::message_origin::{origin_fields, MessageOrigin};
use crate::util::{now, random_id};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffSide {
    Old,
    New,
}

/// Where a drafted annotation stays visible until the send that carries it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum AnnotationAnchor {
    Message { message_id: String, start: i64, end: i64 },
    Diff { comparison: String, path: String, start: String, end: String, side: DiffSide },
}

/// A piece of the assistant's output the user highlighted, with their note on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: String,
    pub quote: String,
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<AnnotationAnchor>,
}

/// A block of text pasted into a composer, held aside as a pill.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PastedText {
    pub id: String,
    pub text: String,
}

pub const MAX_ATTACHMENTS: usize = 6;
pub const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_ATTACHMENT_ENCODED_BYTES: usize = MAX_ATTACHMENT_BYTES.div_ceil(3) * 4;
pub const MAX_ATTACHED_FILES: usize = 10;

/// A file or folder the user dropped or pasted into a composer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachedFile {
    pub id: String,
    pub path: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub folder: bool,
}

/// One before the composer gives it an id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachedFileDraft {
    pub path: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub folder: bool,
}

/// An image waiting in a composer, already written to the attachments directory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StagedImage {
    pub id: String,
    pub path: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageTone {
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    pub id: String,
    pub kind: MessageKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub artifact: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub answer: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<MessageTone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<Annotation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pastes: Option<Vec<PastedText>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<AttachedFile>>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub withdrawn: bool,
    pub at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenshotPlatform {
    Macos,
    LinuxX11,
    LinuxHyprland,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AccessibilitySnapshot {
    Captured { text: String, truncated: bool },
    Unavailable { reason: String },
}

/// Context belongs to the captured pixels, including when an image is recalled or annotated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotContext {
    pub version: u8,
    pub platform: ScreenshotPlatform,
    pub app: String,
    pub title: String,
    pub captured_at: i64,
    pub accessibility: AccessibilitySnapshot,
}

/// Images sent into a run, grouped by path with one positional label per annotation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunAttachment {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ScreenshotContext>,
    pub labels: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageAnnotationKind {
    Box,
    Arrow,
}

/// Geometry is normalized to 0..1 of the image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageAnnotation {
    pub kind: ImageAnnotationKind,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub text: String,
}

/// An image a composer is about to send, before anything of it is on disk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutgoingAttachment {
    pub id: String,
    /// A data URL.
    pub source: String,
    pub annotations: Vec<ImageAnnotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ScreenshotContext>,
}

fn non_empty<T>(items: Option<Vec<T>>) -> Option<Vec<T>> {
    items.filter(|items| !items.is_empty())
}

pub fn create_conversation_message(kind: MessageKind, text: impl Into<String>, detail: Option<String>) -> ConversationMessage {
    conversation_message(random_id(), kind, text, detail)
}

/// A message under an id the caller already has, such as the one an engine streams it under.
pub fn conversation_message(id: String, kind: MessageKind, text: impl Into<String>, detail: Option<String>) -> ConversationMessage {
    ConversationMessage {
        id,
        kind,
        text: text.into(),
        artifact: false,
        answer: false,
        origin: None,
        detail,
        tone: None,
        attachments: None,
        annotations: None,
        pastes: None,
        files: None,
        withdrawn: false,
        at: now(),
    }
}

/// What a user message carries as sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SentMessage {
    pub text: String,
    pub origin: Option<MessageOrigin>,
    pub attachments: Option<Vec<String>>,
    pub annotations: Option<Vec<Annotation>>,
    pub pastes: Option<Vec<PastedText>>,
    pub files: Option<Vec<AttachedFile>>,
}

/// A user message as sent: typed by the user, or by whoever its origin names.
pub fn create_sent_message(sent: SentMessage) -> ConversationMessage {
    let (origin, detail) = origin_fields(sent.origin);
    let mut message = create_conversation_message(MessageKind::User, sent.text, detail);
    message.attachments = non_empty(sent.attachments);
    message.annotations = non_empty(sent.annotations);
    message.pastes = non_empty(sent.pastes);
    message.files = non_empty(sent.files);
    message.origin = origin;
    message
}

pub fn create_failure_message(text: impl Into<String>) -> ConversationMessage {
    let mut message = create_conversation_message(MessageKind::System, text, None);
    message.tone = Some(MessageTone::Error);
    message
}

/// A message the composer can put back: what was typed, and what rode along with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecalledMessage {
    pub text: String,
    pub annotations: Vec<Annotation>,
    pub pastes: Vec<PastedText>,
    pub files: Vec<AttachedFile>,
    pub attachments: Vec<String>,
}

/// What the composer offers back on up-arrow: prompts the user sent themselves, oldest first.
pub fn sent_prompts(messages: &[Shared<ConversationMessage>]) -> Vec<RecalledMessage> {
    messages
        .iter()
        .filter(|message| message.kind == MessageKind::User && message.origin.is_none())
        .map(|message| RecalledMessage {
            text: message.text.clone(),
            annotations: message.annotations.clone().unwrap_or_default(),
            pastes: message.pastes.clone().unwrap_or_default(),
            files: message.files.clone().unwrap_or_default(),
            attachments: message.attachments.clone().unwrap_or_default(),
        })
        .collect()
}
