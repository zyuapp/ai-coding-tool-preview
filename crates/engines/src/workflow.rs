//! The workflow tree Claude Code sends on `task_progress`, read defensively so a shape change costs
//! the panel a field, not the run.

use serde_json::Value;

use crate::types::{WorkflowAgent, WorkflowAgentState, WorkflowPhase};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowProgress {
    pub phases: Vec<WorkflowPhase>,
    pub agents: Vec<WorkflowAgent>,
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(String::from)
}

fn count(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

/// A queued agent reports `start` before it has a start time; only a time means it is really running.
fn agent_state(value: &Value, started_at: Option<f64>) -> WorkflowAgentState {
    match value.as_str() {
        Some("done") => WorkflowAgentState::Done,
        Some("error") => WorkflowAgentState::Error,
        Some("progress") => WorkflowAgentState::Running,
        _ if started_at.is_none() => WorkflowAgentState::Queued,
        _ => WorkflowAgentState::Running,
    }
}

fn read_agent(entry: &Value) -> Option<WorkflowAgent> {
    let index = count(&entry["index"])?;
    let started_at = count(&entry["startedAt"]);
    Some(WorkflowAgent {
        index,
        label: text(&entry["label"]).unwrap_or_else(|| format!("Agent {}", index + 1.0)),
        state: agent_state(&entry["state"], started_at),
        phase_index: count(&entry["phaseIndex"]),
        phase_title: text(&entry["phaseTitle"]),
        agent_id: text(&entry["agentId"]),
        agent_type: text(&entry["agentType"]),
        model: text(&entry["model"]),
        isolation: entry["isolation"]
            .as_str()
            .filter(|value| *value == "worktree" || *value == "remote")
            .map(String::from),
        cached: (entry["cached"] == true).then_some(true),
        attempt: count(&entry["attempt"]),
        last_attempt_reason: text(&entry["lastAttemptReason"]),
        last_tool_name: text(&entry["lastToolName"]),
        last_tool_summary: text(&entry["lastToolSummary"]),
        prompt_preview: text(&entry["promptPreview"]),
        result_preview: text(&entry["resultPreview"]),
        error: text(&entry["error"]),
        queued_at: count(&entry["queuedAt"]),
        started_at,
        duration_ms: count(&entry["durationMs"]),
        last_progress_at: count(&entry["lastProgressAt"]),
        tokens: count(&entry["tokens"]),
        tool_calls: count(&entry["toolCalls"]),
    })
}

/// Reads one progress payload whole. The workflow republishes its entire tree each time, so the
/// result replaces what came before.
pub fn parse_workflow_progress(value: &Value) -> Option<WorkflowProgress> {
    let items = value.as_array()?;
    let mut phases = Vec::new();
    let mut agents: Vec<WorkflowAgent> = Vec::new();
    for entry in items.iter().filter(|item| item.is_object()) {
        match entry["type"].as_str() {
            Some("workflow_phase") => {
                if let (Some(index), Some(title)) = (count(&entry["index"]), text(&entry["title"]))
                {
                    phases.push(WorkflowPhase {
                        index: index as i64,
                        title,
                    });
                }
            }
            Some("workflow_agent") => {
                if let Some(agent) = read_agent(entry) {
                    match agents.iter_mut().find(|known| known.index == agent.index) {
                        Some(known) => *known = agent,
                        None => agents.push(agent),
                    }
                }
            }
            _ => {}
        }
    }
    Some(WorkflowProgress { phases, agents })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_later_entry_for_an_agent_replaces_the_earlier() {
        let progress = parse_workflow_progress(&json!([
            { "type": "workflow_phase", "index": 0, "title": "Plan" },
            { "type": "workflow_agent", "index": 0, "state": "start" },
            { "type": "workflow_agent", "index": 0, "state": "start", "startedAt": 5 },
            { "type": "workflow_agent", "label": "no index" },
        ]))
        .unwrap();
        assert_eq!(progress.phases.len(), 1);
        assert_eq!(progress.agents.len(), 1);
        assert_eq!(progress.agents[0].state, WorkflowAgentState::Running);
        assert_eq!(progress.agents[0].label, "Agent 1");
        assert!(parse_workflow_progress(&json!({})).is_none());
    }
}
