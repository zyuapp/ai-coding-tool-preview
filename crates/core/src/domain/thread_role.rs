//! The part a thread plays among the others the user runs side by side.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadRole {
    Coordinator,
    Implementer,
    Reviewer,
    Researcher,
}

pub const THREAD_ROLES: [ThreadRole; 4] = [ThreadRole::Coordinator, ThreadRole::Implementer, ThreadRole::Reviewer, ThreadRole::Researcher];

impl ThreadRole {
    pub fn label(self) -> &'static str {
        match self {
            ThreadRole::Coordinator => "Coordinator",
            ThreadRole::Implementer => "Implementer",
            ThreadRole::Reviewer => "Reviewer",
            ThreadRole::Researcher => "Researcher",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ThreadRole::Coordinator => "coordinator",
            ThreadRole::Implementer => "implementer",
            ThreadRole::Reviewer => "reviewer",
            ThreadRole::Researcher => "researcher",
        }
    }
}
