//! Dynamic workflows a run drives: phases, agents, and the timeline drawn from them.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowAgentState {
    Queued,
    Running,
    Done,
    Error,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkflowPhase {
    pub index: i64,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowIsolation {
    Worktree,
    Remote,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgent {
    pub index: i64,
    pub label: String,
    pub state: WorkflowAgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<WorkflowIsolation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowStatus {
    Running,
    Completed,
    Failed,
    Stopped,
}

/// A dynamic workflow the run is driving. Each record is a snapshot rather than something to amend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: WorkflowStatus,
    pub phases: Vec<WorkflowPhase>,
    pub agents: Vec<WorkflowAgent>,
    pub total_tokens: i64,
    pub total_tool_calls: i64,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub stopping: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowGroup {
    pub key: String,
    pub title: String,
    pub agents: Vec<WorkflowAgent>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarPlace {
    pub left: f64,
    pub width: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct WorkflowBar {
    pub queue: Option<BarPlace>,
    pub run: Option<BarPlace>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkflowSpan {
    pub start: i64,
    pub end: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowTick {
    pub at: i64,
    pub label: String,
    pub left: f64,
}

const TICK_STEPS: [i64; 10] = [5_000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000, 1_800_000, 3_600_000];
const MAX_TICKS: i64 = 6;
const MIN_BAR_WIDTH: f64 = 0.6;

/// A workflow that ended took its unfinished agents with it.
pub fn agent_state_in(workflow: &Workflow, agent: &WorkflowAgent) -> WorkflowAgentState {
    if workflow.status == WorkflowStatus::Running {
        return agent.state;
    }
    match agent.state {
        WorkflowAgentState::Queued | WorkflowAgentState::Running => WorkflowAgentState::Stopped,
        other => other,
    }
}

pub fn workflow_now(workflow: &Workflow, clock: i64) -> i64 {
    workflow.finished_at.unwrap_or(clock)
}

/// How many agents finished, and how many of those failed.
pub fn workflow_agent_counts(workflow: &Workflow) -> (usize, usize) {
    let done = workflow.agents.iter().filter(|agent| matches!(agent.state, WorkflowAgentState::Done | WorkflowAgentState::Error)).count();
    let failed = workflow.agents.iter().filter(|agent| agent.state == WorkflowAgentState::Error).count();
    (done, failed)
}

/// Phase order first, then spawn order, with anything never given a phase left to the end.
pub fn workflow_groups(workflow: &Workflow) -> Vec<WorkflowGroup> {
    let mut agents = workflow.agents.clone();
    agents.sort_by_key(|agent| agent.index);
    let mut by_phase: indexmap::IndexMap<i64, Vec<WorkflowAgent>> = indexmap::IndexMap::new();
    let mut loose = Vec::new();
    for agent in agents {
        match agent.phase_index {
            None => loose.push(agent),
            Some(index) => by_phase.entry(index).or_default().push(agent),
        }
    }
    let mut phases = workflow.phases.clone();
    phases.sort_by_key(|phase| phase.index);
    let seen: std::collections::HashSet<i64> = phases.iter().map(|phase| phase.index).collect();
    let mut extra: Vec<i64> = by_phase.keys().copied().filter(|index| !seen.contains(index)).collect();
    extra.sort();
    for index in extra {
        let title = by_phase[&index].first().and_then(|agent| agent.phase_title.clone()).unwrap_or_else(|| format!("Phase {}", index + 1));
        phases.push(WorkflowPhase { index, title });
    }
    let mut groups: Vec<WorkflowGroup> = phases
        .iter()
        .filter_map(|phase| by_phase.get(&phase.index).map(|agents| WorkflowGroup { key: format!("phase-{}", phase.index), title: phase.title.clone(), agents: agents.clone() }))
        .collect();
    if !loose.is_empty() {
        groups.push(WorkflowGroup { key: "loose".into(), title: "Agents".into(), agents: loose });
    }
    groups
}

pub fn workflow_agent_end(agent: &WorkflowAgent, now: i64) -> i64 {
    if !matches!(agent.state, WorkflowAgentState::Done | WorkflowAgentState::Error) {
        return now;
    }
    if let (Some(started), Some(duration)) = (agent.started_at, agent.duration_ms) {
        return started + duration;
    }
    agent.last_progress_at.or(agent.started_at).or(agent.queued_at).unwrap_or(now)
}

pub fn workflow_span(workflow: &Workflow, now: i64) -> WorkflowSpan {
    let mut start = workflow.started_at;
    let mut end = workflow.finished_at.unwrap_or(now);
    for agent in &workflow.agents {
        if let Some(queued) = agent.queued_at {
            start = start.min(queued);
        }
        if let Some(started) = agent.started_at {
            start = start.min(started);
        }
        end = end.max(workflow_agent_end(agent, now));
    }
    WorkflowSpan { start, end: end.max(start + 1) }
}

fn place(from: i64, to: i64, span: WorkflowSpan) -> BarPlace {
    let total = (span.end - span.start) as f64;
    let left = ((from - span.start) as f64 / total * 100.0).clamp(0.0, 100.0);
    let width = MIN_BAR_WIDTH.max((100.0 - left).min((to - from) as f64 / total * 100.0));
    BarPlace { left, width }
}

pub fn workflow_bar(agent: &WorkflowAgent, span: WorkflowSpan, now: i64) -> WorkflowBar {
    let started = agent.started_at.or(if agent.state == WorkflowAgentState::Queued { None } else { agent.queued_at });
    let queue = match agent.queued_at {
        Some(queued) if started.is_none_or(|started| queued < started) => Some(place(queued, started.unwrap_or(now), span)),
        _ => None,
    };
    let run = started.map(|started| place(started, workflow_agent_end(agent, now), span));
    WorkflowBar { queue, run }
}

pub fn workflow_ticks(span: WorkflowSpan) -> Vec<WorkflowTick> {
    let total = span.end - span.start;
    let step = TICK_STEPS.iter().copied().find(|candidate| total as f64 / *candidate as f64 <= MAX_TICKS as f64).unwrap_or_else(|| (total as f64 / MAX_TICKS as f64).ceil() as i64).max(1);
    let mut ticks = Vec::new();
    let mut at = 0;
    while at <= total {
        ticks.push(WorkflowTick { at, label: format_elapsed(at), left: at as f64 / total as f64 * 100.0 });
        at += step;
    }
    ticks
}

pub fn format_elapsed(ms: i64) -> String {
    let seconds = ((ms as f64 / 1_000.0) + 0.5).floor().max(0.0) as i64;
    let hours = seconds / 3_600;
    let minutes = seconds % 3_600 / 60;
    let rest = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{rest:02}")
    } else {
        format!("{minutes}:{rest:02}")
    }
}

pub fn workflow_agent_duration(agent: &WorkflowAgent, now: i64) -> Option<i64> {
    agent.started_at.or(agent.queued_at).map(|started| workflow_agent_end(agent, now) - started)
}

/// One steady line per agent: what it is doing now, or what it came back with.
pub fn workflow_agent_note(agent: &WorkflowAgent, state: WorkflowAgentState) -> String {
    match state {
        WorkflowAgentState::Error => agent.error.clone().unwrap_or_else(|| "Failed".into()),
        WorkflowAgentState::Stopped => "Stopped with the run".into(),
        WorkflowAgentState::Queued => "Queued".into(),
        WorkflowAgentState::Running => agent.last_tool_name.as_ref().map(|tool| format!("Using {tool}")).unwrap_or_else(|| "Working".into()),
        WorkflowAgentState::Done => {
            if agent.cached == Some(true) {
                return "Replayed from an earlier run".into();
            }
            agent.result_preview.as_deref().and_then(|preview| preview.split('\n').find(|line| !line.trim().is_empty())).map(String::from).unwrap_or_else(|| "Done".into())
        }
    }
}

pub fn workflow_status_label(workflow: &Workflow) -> &'static str {
    if workflow.stopping {
        return "Stopping";
    }
    match workflow.status {
        WorkflowStatus::Running => "Running",
        WorkflowStatus::Completed => "Completed",
        WorkflowStatus::Failed => "Failed",
        WorkflowStatus::Stopped => "Stopped",
    }
}

pub fn workflow_label(workflow: &Workflow) -> String {
    format!("{} · {}/{} agents", workflow_status_label(workflow), workflow_agent_counts(workflow).0, workflow.agents.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_reads_minutes_and_hours() {
        assert_eq!(format_elapsed(65_000), "1:05");
        assert_eq!(format_elapsed(3_725_000), "1:02:05");
    }
}
