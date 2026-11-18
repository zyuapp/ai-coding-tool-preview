//! What a run reads off the models this build ships knowing (`act_core::domain::engine`).

use act_core::domain::engine::{self, built_in_models};

use crate::types::AgentEngine;

pub use act_core::domain::engine::claude_effort;

/// A model the engine does not offer is measured against the engine's default model.
pub fn context_window_limit(engine: AgentEngine, model: &str) -> u64 {
    engine::context_window_limit(built_in_models(), engine, model).max(0) as u64
}

/// False for a model that reasons at one depth, whose threads are run without an effort.
pub fn model_takes_effort(engine: AgentEngine, model: &str) -> bool {
    engine::model_takes_effort(built_in_models(), engine, model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentEffort;

    #[test]
    fn unknown_models_measure_against_the_engine_default() {
        assert_eq!(context_window_limit(AgentEngine::Claude, "haiku"), 200_000);
        assert_eq!(
            context_window_limit(AgentEngine::Claude, "claude-unknown"),
            1_000_000
        );
        assert_eq!(context_window_limit(AgentEngine::Opencode, "x/y"), 0);
        assert!(!model_takes_effort(AgentEngine::Claude, "haiku"));
        assert_eq!(claude_effort(AgentEffort::Ultra), AgentEffort::High);
    }
}
