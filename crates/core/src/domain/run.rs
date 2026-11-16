//! Runs and what they report: policies, efforts, sessions, subagents, background work.

use serde::{Deserialize, Serialize};

/// `Other` is a mode only a newer build on a paired computer knows, shown as sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ExecutionPolicy {
    #[default]
    Confirm,
    Plan,
    AllowEdits,
    Autonomous,
    Bypass,
    Other(&'static str),
}

crate::util::as_sent_serde!(ExecutionPolicy, "mode");

pub const EXECUTION_POLICIES: [ExecutionPolicy; 5] = [ExecutionPolicy::Confirm, ExecutionPolicy::Plan, ExecutionPolicy::AllowEdits, ExecutionPolicy::Autonomous, ExecutionPolicy::Bypass];

/// The modes a picker offers, in the order it lists them. Plan is left out.
pub const POLICY_CHOICES: [ExecutionPolicy; 4] = [ExecutionPolicy::Autonomous, ExecutionPolicy::Bypass, ExecutionPolicy::AllowEdits, ExecutionPolicy::Confirm];

impl ExecutionPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionPolicy::Confirm => "confirm",
            ExecutionPolicy::Plan => "plan",
            ExecutionPolicy::AllowEdits => "allow-edits",
            ExecutionPolicy::Autonomous => "autonomous",
            ExecutionPolicy::Bypass => "bypass",
            ExecutionPolicy::Other(name) => name,
        }
    }

    /// One of this build's modes, or None.
    pub fn parse(value: &str) -> Option<ExecutionPolicy> {
        EXECUTION_POLICIES.into_iter().find(|policy| policy.as_str() == value)
    }

    pub fn read(value: &str) -> ExecutionPolicy {
        ExecutionPolicy::parse(value).unwrap_or_else(|| ExecutionPolicy::Other(crate::util::intern(value)))
    }

    pub fn is_known(self) -> bool {
        !matches!(self, ExecutionPolicy::Other(_))
    }

    /// How a mode reads wherever it is named.
    pub fn label(self) -> &'static str {
        match self {
            ExecutionPolicy::Autonomous => "Auto",
            ExecutionPolicy::Bypass => "Bypass",
            ExecutionPolicy::AllowEdits => "Edits",
            ExecutionPolicy::Confirm => "Confirm",
            ExecutionPolicy::Plan => "Plan",
            ExecutionPolicy::Other(name) => name,
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ExecutionPolicy::Autonomous => "Only ask for potentially unsafe actions",
            ExecutionPolicy::Bypass => "Use tools and change files without asking",
            ExecutionPolicy::AllowEdits => "Apply file edits without asking",
            ExecutionPolicy::Confirm => "Ask before using tools or changing files",
            ExecutionPolicy::Plan => "Plan the work without doing it",
            ExecutionPolicy::Other(_) => "",
        }
    }
}

/// How much reasoning a run asks for. `Other` is an effort only a newer build knows, kept as sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum AgentEffort {
    Low,
    Medium,
    #[default]
    High,
    Xhigh,
    Max,
    Ultra,
    Other(&'static str),
}

crate::util::as_sent_serde!(AgentEffort, "effort");

impl AgentEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentEffort::Low => "low",
            AgentEffort::Medium => "medium",
            AgentEffort::High => "high",
            AgentEffort::Xhigh => "xhigh",
            AgentEffort::Max => "max",
            AgentEffort::Ultra => "ultra",
            AgentEffort::Other(name) => name,
        }
    }

    /// One of this build's efforts, or None.
    pub fn parse(value: &str) -> Option<AgentEffort> {
        [AgentEffort::Low, AgentEffort::Medium, AgentEffort::High, AgentEffort::Xhigh, AgentEffort::Max, AgentEffort::Ultra].into_iter().find(|effort| effort.as_str() == value)
    }

    pub fn read(value: &str) -> AgentEffort {
        AgentEffort::parse(value).unwrap_or_else(|| AgentEffort::Other(crate::util::intern(value)))
    }

    pub fn is_known(self) -> bool {
        !matches!(self, AgentEffort::Other(_))
    }
}

pub const DEFAULT_EFFORT: AgentEffort = AgentEffort::High;

/// An engine's own handle on a thread, named by the engine that made it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Continuation {
    pub provider: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolIntent {
    pub tool_id: String,
    pub name: String,
    #[serde(default)]
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Running,
    AwaitingApproval,
    Succeeded,
    Failed,
    Cancelled,
}

/// The engine's request failed and it is trying again on its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryNotice {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentStatus {
    Working,
    Idle,
    Completed,
    Failed,
    Stopped,
}

impl SubagentStatus {
    pub fn label(self) -> &'static str {
        match self {
            SubagentStatus::Working => "Working",
            SubagentStatus::Idle => "Idle",
            SubagentStatus::Completed => "Completed",
            SubagentStatus::Failed => "Failed",
            SubagentStatus::Stopped => "Stopped",
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, SubagentStatus::Working | SubagentStatus::Idle)
    }

    /// Failures first, then the work still going.
    fn rank(self) -> u8 {
        match self {
            SubagentStatus::Failed => 0,
            SubagentStatus::Working => 1,
            SubagentStatus::Idle => 2,
            SubagentStatus::Stopped => 3,
            SubagentStatus::Completed => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityKind {
    Text,
    Tool,
}

/// Provider-neutral updates for one delegated agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum SubagentReport {
    #[serde(rename = "subagent.started")]
    Started {
        id: String,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_type: Option<String>,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        session_scoped: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    #[serde(rename = "subagent.metadata")]
    Metadata {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    #[serde(rename = "subagent.status")]
    Status {
        id: String,
        status: SubagentStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    #[serde(rename = "subagent.progress")]
    Progress {
        id: String,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        total_tokens: i64,
    },
    #[serde(rename = "subagent.activity")]
    Activity {
        id: String,
        activity_id: String,
        kind: ActivityKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        text: String,
    },
    #[serde(rename = "subagent.finished")]
    Finished { id: String, status: SubagentStatus, summary: String },
}

/// Every foldable group of subagents: the sidebar's own list, and one heading per status in the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentGroup {
    Sidebar,
    Working,
    Idle,
    Failed,
    Stopped,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentGroups {
    pub sidebar: bool,
    pub working: bool,
    pub idle: bool,
    pub failed: bool,
    pub stopped: bool,
    pub completed: bool,
}

impl Default for SubagentGroups {
    fn default() -> Self {
        SubagentGroups { sidebar: true, working: true, idle: true, failed: true, stopped: true, completed: true }
    }
}

impl SubagentGroups {
    pub fn get(&self, group: SubagentGroup) -> bool {
        match group {
            SubagentGroup::Sidebar => self.sidebar,
            SubagentGroup::Working => self.working,
            SubagentGroup::Idle => self.idle,
            SubagentGroup::Failed => self.failed,
            SubagentGroup::Stopped => self.stopped,
            SubagentGroup::Completed => self.completed,
        }
    }

    pub fn set(&mut self, group: SubagentGroup, open: bool) {
        match group {
            SubagentGroup::Sidebar => self.sidebar = open,
            SubagentGroup::Working => self.working = open,
            SubagentGroup::Idle => self.idle = open,
            SubagentGroup::Failed => self.failed = open,
            SubagentGroup::Stopped => self.stopped = open,
            SubagentGroup::Completed => self.completed = open,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentActivity {
    pub id: String,
    pub kind: ActivityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub text: String,
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subagent {
    pub id: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub session_scoped: bool,
    pub status: SubagentStatus,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub stopping: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub activity: Vec<SubagentActivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// One steady line per subagent: what it is doing now, or what it came back with.
pub fn subagent_line(subagent: &Subagent) -> String {
    if subagent.status == SubagentStatus::Working {
        return match &subagent.last_tool_name {
            Some(tool) => format!("Using {tool}"),
            None => "Working".into(),
        };
    }
    match &subagent.summary {
        Some(summary) => summary.split('\n').next().unwrap_or("").to_string(),
        None => subagent.status.label().into(),
    }
}

/// Failures first, then the work still going.
pub fn order_subagents(subagents: &[Subagent]) -> Vec<Subagent> {
    let mut ordered = subagents.to_vec();
    ordered.sort_by_key(|subagent| subagent.status.rank());
    ordered
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundProcessKind {
    Shell,
    Monitor,
}

/// A process owned by one thread session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundProcess {
    pub id: String,
    pub kind: BackgroundProcessKind,
    pub description: String,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub stopping: bool,
}

pub fn process_label(process: &BackgroundProcess) -> &'static str {
    if process.stopping {
        return "Stopping";
    }
    match process.kind {
        BackgroundProcessKind::Shell => "Shell",
        BackgroundProcessKind::Monitor => "Monitor",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus {
    Active,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveGoal {
    pub objective: String,
    pub status: GoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
}
