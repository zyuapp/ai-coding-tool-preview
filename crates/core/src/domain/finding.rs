//! What a scheduled run said it found, which outlives every run after it.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationFinding {
    pub id: String,
    pub headline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What the finding is about, stable across runs, so the same one is not raised twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub at: i64,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub read: bool,
}

pub const MAX_FINDINGS: usize = 10;
pub const MAX_HANDLED_ISSUES: usize = 50;
pub const MAX_HEADLINE: usize = 200;
pub const MAX_DETAIL: usize = 10_000;
pub const MAX_FINDING_KEY: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastChecked {
    pub at: i64,
    pub note: String,
}
