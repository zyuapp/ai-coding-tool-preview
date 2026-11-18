//! Engine-neutral values the engines read and report. Each serializes to the same JSON shape the
//! TypeScript app used, so persisted records and IPC payloads stay compatible.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use act_core::domain::engine::{
    AgentEngine, EffortSpec, EngineAccess, EngineReadiness, ModelSpec,
};
pub use act_core::domain::run::AgentEffort;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionPolicy {
    Confirm,
    Plan,
    AllowEdits,
    Autonomous,
    Bypass,
}

impl ExecutionPolicy {
    pub const ALL: [ExecutionPolicy; 5] = [
        ExecutionPolicy::Confirm,
        ExecutionPolicy::Plan,
        ExecutionPolicy::AllowEdits,
        ExecutionPolicy::Autonomous,
        ExecutionPolicy::Bypass,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunChannel {
    Main,
    Side,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoordinationRole {
    Coordinator,
    Member,
}

/// An engine's own handle on a thread, named by the engine that made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Continuation {
    pub provider: String,
    pub value: String,
}

impl Continuation {
    pub fn new(engine: AgentEngine, value: impl Into<String>) -> Self {
        Continuation {
            provider: engine.as_str().to_string(),
            value: value.into(),
        }
    }
}

/// The handle a continuation holds for `engine`; another engine's continuation means nothing to it.
pub fn handle_for(continuation: Option<&Continuation>, engine: AgentEngine) -> Option<&str> {
    continuation
        .filter(|c| c.provider == engine.as_str())
        .map(|c| c.value.as_str())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolIntent {
    pub tool_id: String,
    pub name: String,
    pub input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_path: Option<String>,
}

/// The engine's request failed and it is trying again on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryNotice {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentLiveStatus {
    Working,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentTerminalStatus {
    Completed,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityKind {
    Text,
    Tool,
}

/// Provider-neutral updates for one delegated agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SubagentReport {
    #[serde(rename = "subagent.started", rename_all = "camelCase")]
    Started {
        id: String,
        description: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_type: Option<String>,
        #[serde(skip_serializing_if = "std::ops::Not::not", default)]
        session_scoped: bool,
        #[serde(flatten)]
        metadata: SubagentMetadata,
    },
    #[serde(rename = "subagent.metadata")]
    Metadata {
        id: String,
        #[serde(flatten)]
        metadata: SubagentMetadata,
    },
    #[serde(rename = "subagent.status")]
    Status {
        id: String,
        status: SubagentLiveStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    #[serde(rename = "subagent.progress", rename_all = "camelCase")]
    Progress {
        id: String,
        description: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_tool_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        total_tokens: u64,
    },
    #[serde(rename = "subagent.activity", rename_all = "camelCase")]
    Activity {
        id: String,
        activity_id: String,
        kind: ActivityKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        text: String,
    },
    #[serde(rename = "subagent.finished")]
    Finished {
        id: String,
        status: SubagentTerminalStatus,
        summary: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundProcessKind {
    Shell,
    Monitor,
}

/// A process owned by one thread session, republished whole on every change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundProcess {
    pub id: String,
    pub kind: BackgroundProcessKind,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BackgroundReport {
    #[serde(rename = "background.changed")]
    Changed { processes: Vec<BackgroundProcess> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowAgentState {
    Queued,
    Running,
    Done,
    Error,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPhase {
    pub index: i64,
    pub title: String,
}

/// A count JavaScript reported, written back as an integer when it is one, as JavaScript writes it.
fn whole_number<S: serde::Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        serializer.serialize_i64(*value as i64)
    } else {
        serializer.serialize_f64(*value)
    }
}

fn whole_numbers<S: serde::Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => whole_number(value, serializer),
        None => serializer.serialize_none(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgent {
    #[serde(serialize_with = "whole_number")]
    pub index: f64,
    pub label: String,
    pub state: WorkflowAgentState,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "whole_numbers"
    )]
    pub phase_index: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "whole_numbers"
    )]
    pub attempt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attempt_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tool_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "whole_numbers"
    )]
    pub queued_at: Option<f64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "whole_numbers"
    )]
    pub started_at: Option<f64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "whole_numbers"
    )]
    pub duration_ms: Option<f64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "whole_numbers"
    )]
    pub last_progress_at: Option<f64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "whole_numbers"
    )]
    pub tokens: Option<f64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "whole_numbers"
    )]
    pub tool_calls: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowEndStatus {
    Completed,
    Failed,
    Stopped,
}

/// What a workflow reports as it runs. It belongs to the thread, not to a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkflowReport {
    #[serde(rename = "workflow.started")]
    Started {
        id: String,
        name: String,
        description: String,
    },
    #[serde(rename = "workflow.progress", rename_all = "camelCase")]
    Progress {
        id: String,
        phases: Vec<WorkflowPhase>,
        agents: Vec<WorkflowAgent>,
        total_tokens: u64,
        total_tool_calls: u64,
    },
    #[serde(rename = "workflow.finished")]
    Finished {
        id: String,
        status: WorkflowEndStatus,
        summary: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus {
    Active,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveGoal {
    pub objective: String,
    pub status: GoalStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iterations: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GoalReport {
    #[serde(rename = "goal.changed")]
    Changed { goal: Option<ActiveGoal> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LimitWindow {
    Session,
    Weekly,
}

/// The account ran out of its plan's allowance and gets it back at `resets_at` (epoch ms).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimit {
    #[serde(serialize_with = "whole_number")]
    pub resets_at: f64,
    pub window: LimitWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsagePeriod {
    Session,
    Week,
}

/// One rate-limit window as the usage panel draws it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    pub utilization: Option<f64>,
    pub resets_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period: Option<UsagePeriod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes: Option<f64>,
}

/// What one engine can say about the plan behind its account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum PlanUsage {
    Available {
        subscription: Option<String>,
        windows: Vec<UsageWindow>,
    },
    NotApplicable,
    Unavailable {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionRequest {
    pub questions: Vec<AgentQuestion>,
    pub blocking: bool,
}

impl QuestionRequest {
    /// A request the app can show: 1..=100 questions with distinct non-empty ids.
    pub fn is_valid(&self) -> bool {
        if self.questions.is_empty() || self.questions.len() > 100 {
            return false;
        }
        let mut ids = std::collections::HashSet::new();
        self.questions
            .iter()
            .all(|question| !question.id.is_empty() && ids.insert(question.id.clone()))
    }
}

pub type QuestionAnswers = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerUseMcp {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ComputerUseRunConfig {
    Available { mcp: ComputerUseMcp },
    SetupRequired,
    Unavailable { message: String },
}

impl ComputerUseRunConfig {
    pub fn status(&self) -> &'static str {
        match self {
            ComputerUseRunConfig::Available { .. } => "available",
            ComputerUseRunConfig::SetupRequired => "setup-required",
            ComputerUseRunConfig::Unavailable { .. } => "unavailable",
        }
    }

    pub fn mcp(&self) -> Option<&ComputerUseMcp> {
        match self {
            ComputerUseRunConfig::Available { mcp } => Some(mcp),
            _ => None,
        }
    }
}

/// Work a run performs without sending its prompt as a model turn. A review target is Codex's own
/// `ReviewTarget`, passed through as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RunOperation {
    #[serde(rename_all = "camelCase")]
    Compact {
        pre_tokens: u64,
    },
    Review {
        target: Value,
    },
}

/// A slash command or skill an engine offers the composer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommand {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    /// Other names the command answers to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Running,
    AwaitingApproval,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionTrigger {
    Manual,
    Auto,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_counts_are_written_as_integers() {
        let limit = UsageLimit {
            resets_at: 1_794_704_400_000.0,
            window: LimitWindow::Weekly,
        };
        assert_eq!(
            serde_json::to_string(&limit).unwrap(),
            r#"{"resetsAt":1794704400000,"window":"weekly"}"#
        );
        let agent: WorkflowAgent = serde_json::from_value(serde_json::json!({
            "index": 1, "label": "a", "state": "running", "tokens": 2.5, "startedAt": 1794478727475u64
        }))
        .unwrap();
        let written = serde_json::to_value(&agent).unwrap();
        assert!(written["index"].is_i64() && written["startedAt"].is_i64());
        assert_eq!(written["tokens"], 2.5);
    }

    #[test]
    fn reports_serialize_in_the_app_wire_shape() {
        let started = SubagentReport::Started {
            id: "a".into(),
            description: "d".into(),
            agent_type: None,
            session_scoped: true,
            metadata: SubagentMetadata {
                model: Some("m".into()),
                ..Default::default()
            },
        };
        let json = serde_json::to_value(&started).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "subagent.started", "id": "a", "description": "d", "sessionScoped": true, "model": "m"})
        );
        let usage = PlanUsage::NotApplicable;
        assert_eq!(
            serde_json::to_value(&usage).unwrap(),
            serde_json::json!({"status": "not-applicable"})
        );
        let op: RunOperation =
            serde_json::from_value(serde_json::json!({"type": "compact", "preTokens": 5})).unwrap();
        assert_eq!(op, RunOperation::Compact { pre_tokens: 5 });
    }
}
