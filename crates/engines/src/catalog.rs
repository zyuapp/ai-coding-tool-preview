//! The models this build ships knowing for Claude and Codex, and what engines read off them during a
//! run: context windows and whether a model takes an effort. OpenCode names its models itself.

use crate::types::{AgentEffort, AgentEngine, EffortSpec, ExecutionPolicy, ModelSpec};

fn effort_spec(id: AgentEffort) -> EffortSpec {
    let (description, delegates) = match id {
        AgentEffort::Ultra => (Some("Splits the work across agents".to_string()), true),
        _ => (None, false),
    };
    EffortSpec {
        id,
        label: id.label().to_string(),
        description,
        delegates,
    }
}

/// The named efforts as specs, always deepest first however they were named.
pub fn efforts(ids: &[AgentEffort]) -> Vec<EffortSpec> {
    AgentEffort::DEEPEST_FIRST
        .into_iter()
        .filter(|id| ids.contains(id))
        .map(effort_spec)
        .collect()
}

const THROUGH_MAX: [AgentEffort; 5] = [
    AgentEffort::Max,
    AgentEffort::Xhigh,
    AgentEffort::High,
    AgentEffort::Medium,
    AgentEffort::Low,
];
const THROUGH_ULTRA: [AgentEffort; 6] = AgentEffort::DEEPEST_FIRST;

/// The default context window in Codex's model catalogue, used until the server reports its own.
pub const CODEX_CONTEXT_WINDOW: u64 = 272_000;

fn model(
    id: &str,
    label: &str,
    description: &str,
    context_window: u64,
    efforts_of: &[AgentEffort],
) -> ModelSpec {
    ModelSpec {
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        context_window,
        efforts: efforts(efforts_of),
        manual_compaction: true,
        provider: None,
    }
}

/// The built-in model list of an engine; empty for one whose models are discovered.
pub fn built_in_models(engine: AgentEngine) -> Vec<ModelSpec> {
    match engine {
        AgentEngine::Claude => vec![
            model(
                "fable",
                "Fable",
                "Most capable for demanding work",
                1_000_000,
                &THROUGH_MAX,
            ),
            model(
                "opus",
                "Opus",
                "Best for complex reasoning",
                1_000_000,
                &THROUGH_MAX,
            ),
            model(
                "sonnet",
                "Sonnet",
                "Balanced speed and capability",
                1_000_000,
                &THROUGH_MAX,
            ),
            model(
                "haiku",
                "Haiku",
                "Fastest for lightweight work",
                200_000,
                &[],
            ),
        ],
        AgentEngine::Codex => vec![
            model(
                "gpt-6-astra",
                "Astra",
                "Most capable model for complex, demanding work",
                CODEX_CONTEXT_WINDOW,
                &THROUGH_ULTRA,
            ),
            model(
                "gpt-6.1-sol",
                "Sol",
                "Strong coding model for everyday work",
                CODEX_CONTEXT_WINDOW,
                &THROUGH_ULTRA,
            ),
            model(
                "gpt-6.1-terra",
                "Terra",
                "Balanced agentic coding model for everyday work",
                CODEX_CONTEXT_WINDOW,
                &THROUGH_ULTRA,
            ),
            model(
                "gpt-6-luna",
                "Luna",
                "Efficient model for lightweight work",
                CODEX_CONTEXT_WINDOW,
                &THROUGH_MAX,
            ),
        ],
        AgentEngine::OpenCode => Vec::new(),
    }
}

/// The default this build ships for the engine; none for one whose models are only known once it names them.
pub fn default_model(engine: AgentEngine) -> Option<&'static str> {
    match engine {
        AgentEngine::Claude => Some("opus"),
        AgentEngine::Codex => Some("gpt-6.1-sol"),
        AgentEngine::OpenCode => None,
    }
}

/// A model the engine does not offer is measured against the engine's default model.
pub fn context_window_limit(engine: AgentEngine, model_id: &str) -> u64 {
    let models = built_in_models(engine);
    if let Some(found) = models.iter().find(|spec| spec.id == model_id) {
        return found.context_window;
    }
    default_model(engine)
        .and_then(|id| models.iter().find(|spec| spec.id == id))
        .or_else(|| models.first())
        .map(|spec| spec.context_window)
        .unwrap_or(0)
}

/// False for a model that reasons at one depth, whose threads are run without an effort.
pub fn model_takes_effort(engine: AgentEngine, model_id: &str) -> bool {
    built_in_models(engine)
        .iter()
        .any(|spec| spec.id == model_id && !spec.efforts.is_empty())
}

/// An effort Claude does not offer lands on its default, so a foreign one never reaches the CLI.
pub fn claude_effort(effort: AgentEffort) -> AgentEffort {
    if THROUGH_MAX.contains(&effort) {
        effort
    } else {
        AgentEffort::High
    }
}

/// The panels and controls an engine can feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineCapabilities {
    pub fast_mode: bool,
    pub workflows: bool,
    pub subagents: bool,
    pub subagent_metadata: bool,
    pub review: bool,
    pub chrome_browser: bool,
    pub policies: Vec<ExecutionPolicy>,
    pub unattended_policy: ExecutionPolicy,
}

pub fn capabilities(engine: AgentEngine) -> EngineCapabilities {
    match engine {
        AgentEngine::Claude => EngineCapabilities {
            fast_mode: false,
            workflows: true,
            subagents: true,
            subagent_metadata: true,
            review: false,
            chrome_browser: true,
            policies: ExecutionPolicy::ALL.to_vec(),
            unattended_policy: ExecutionPolicy::Autonomous,
        },
        AgentEngine::Codex => EngineCapabilities {
            fast_mode: true,
            workflows: false,
            subagents: true,
            subagent_metadata: true,
            review: true,
            chrome_browser: false,
            policies: ExecutionPolicy::ALL.to_vec(),
            unattended_policy: ExecutionPolicy::Autonomous,
        },
        AgentEngine::OpenCode => EngineCapabilities {
            fast_mode: false,
            workflows: false,
            subagents: false,
            subagent_metadata: false,
            review: false,
            chrome_browser: false,
            policies: vec![
                ExecutionPolicy::Confirm,
                ExecutionPolicy::Plan,
                ExecutionPolicy::AllowEdits,
                ExecutionPolicy::Bypass,
            ],
            unattended_policy: ExecutionPolicy::Bypass,
        },
    }
}

/// True for a well-formed model id: at most 256 characters, no whitespace or control characters.
pub fn is_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 256
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_models_measure_against_the_engine_default() {
        assert_eq!(context_window_limit(AgentEngine::Claude, "haiku"), 200_000);
        assert_eq!(
            context_window_limit(AgentEngine::Claude, "claude-unknown"),
            1_000_000
        );
        assert_eq!(context_window_limit(AgentEngine::OpenCode, "x/y"), 0);
        assert!(!model_takes_effort(AgentEngine::Claude, "haiku"));
        assert_eq!(claude_effort(AgentEffort::Ultra), AgentEffort::High);
    }
}
