//! A conversation message as the `messages.data` column holds it (`src/domain/conversation.ts`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::enums::{DiffSide, MessageKind, MessageTone};
use super::js;

pub type Extra = Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    pub id: String,
    pub kind: MessageKind,
    pub text: String,
    /// A delivered artifact or its failure stays visible when intermediate work folds away.
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub artifact: bool,
    /// On assistant text, the last thing a run said.
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub answer: bool,
    /// On a user message, who sent it when the user did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<MessageTone>,
    /// Absolute paths of images sent with this message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<Annotation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pastes: Option<Vec<PastedText>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<AttachedFile>>,
    /// Written by a tick that surfaced nothing.
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub withdrawn: bool,
    #[serde(serialize_with = "js::num")]
    pub at: f64,
    #[serde(flatten)]
    pub extra: Extra,
}

impl ConversationMessage {
    /// Whether the message counts toward the thread's activity. Rows written while `withdrawn` was
    /// called `quiet` carry that name instead.
    pub fn audible(&self) -> bool {
        !(self.withdrawn || js::truthy(self.extra.get("quiet")))
    }

    pub fn has_attachments(&self) -> bool {
        self.attachments.as_ref().is_some_and(|attachments| !attachments.is_empty())
    }
}

/// Who put a user message in a thread when the user did not type it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MessageOrigin {
    /// Another thread, by the title it had when it sent the message.
    Thread(ThreadOrigin),
    Automation(AutomationOrigin),
    /// A coordinator's batch of news from its threads.
    Coordination(CoordinationOrigin),
    /// Written by a build that kept only a label.
    Legacy(LegacyOrigin),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationOrigin {
    #[serde(serialize_with = "js::num")]
    pub run_number: f64,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CoordinationOrigin {
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LegacyOrigin {
    pub label: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A piece of output the user highlighted, with their note on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: String,
    pub quote: String,
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// An anchor as stored. Stored anchors are never checked, so one this build cannot read is kept as
/// it was written.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Anchor {
    Read(AnnotationAnchor),
    Unread(Value),
}

impl Anchor {
    pub fn read(&self) -> Option<&AnnotationAnchor> {
        match self {
            Anchor::Read(anchor) => Some(anchor),
            Anchor::Unread(_) => None,
        }
    }
}

/// Where a drafted annotation stays visible until the send that carries it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AnnotationAnchor {
    Message(MessageAnchor),
    Diff(DiffAnchor),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageAnchor {
    pub message_id: String,
    #[serde(serialize_with = "js::num")]
    pub start: f64,
    #[serde(serialize_with = "js::num")]
    pub end: f64,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffAnchor {
    pub comparison: String,
    pub path: String,
    pub start: String,
    pub end: String,
    pub side: DiffSide,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A block of text pasted into a composer, held aside as a pill.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PastedText {
    pub id: String,
    pub text: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A file or folder a message names; the agent opens it from disk itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachedFile {
    pub id: String,
    pub path: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "js::is_false", deserialize_with = "js::flag")]
    pub folder: bool,
    #[serde(flatten)]
    pub extra: Extra,
}
