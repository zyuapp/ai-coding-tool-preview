//! Short cards in the corner of the window.

use serde::{Deserialize, Serialize};

use super::engine::AgentEngine;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToastTone {
    Update,
    Progress,
    Success,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToastCommand {
    #[serde(rename = "engine.update")]
    EngineUpdate { engine: AgentEngine },
}

/// The one button a toast can carry, and the command it sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToastAction {
    pub label: String,
    pub command: ToastCommand,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Toast {
    pub id: i64,
    pub tone: ToastTone,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// A later toast about the same thing takes its place rather than stacking under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub persistent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ToastAction>,
    /// Kept once the user closes the toast, so the same one is not raised again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember: Option<String>,
}

pub const TOAST_LIFETIME_MS: i64 = 5_000;
pub const MAX_TOASTS: usize = 4;
pub const MAX_REMEMBERED_TOASTS: usize = 50;
