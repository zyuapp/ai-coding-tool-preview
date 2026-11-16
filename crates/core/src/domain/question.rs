//! Questions a run puts to the user, and what a native review looks at.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// A question the agent can leave open while continuing its work.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionRequest {
    pub questions: Vec<AgentQuestion>,
    pub blocking: bool,
}

/// A question as the thread holds it while it waits: where it came from and the answer typed so far.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingQuestion {
    #[serde(flatten)]
    pub question: AgentQuestion,
    pub run_id: String,
    pub request_id: String,
    pub question_id: String,
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub submitting: bool,
}

/// What Codex reviews when the user starts a native review turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ReviewTarget {
    UncommittedChanges,
    BaseBranch { branch: String },
    Commit { sha: String, title: Option<String> },
    Custom { instructions: String },
}

fn bounded(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && crate::util::js_len(value) <= limit
}

pub fn is_review_target(target: &ReviewTarget) -> bool {
    match target {
        ReviewTarget::UncommittedChanges => true,
        ReviewTarget::BaseBranch { branch } => bounded(branch, 4_096),
        ReviewTarget::Commit { sha, title } => bounded(sha, 256) && title.as_deref().is_none_or(|title| bounded(title, 10_000)),
        ReviewTarget::Custom { instructions } => bounded(instructions, 1_000_000),
    }
}

pub fn is_question_request(request: &QuestionRequest) -> bool {
    if request.questions.is_empty() || request.questions.len() > 100 {
        return false;
    }
    let mut ids = std::collections::HashSet::new();
    request.questions.iter().all(|question| !question.id.is_empty() && ids.insert(question.id.clone()))
}
