//! What storage validation needs of the engine catalogue (`src/domain/agent-engine.ts`).

use super::js;
use crate::domain::engine::{capabilities_for, AgentEngine, AGENT_ENGINES};

pub use crate::domain::engine::current_model;

/// Every effort, deepest first.
pub const EFFORTS: [&str; 6] = ["ultra", "max", "xhigh", "high", "medium", "low"];

pub const EXECUTION_POLICIES: [&str; 5] = ["confirm", "plan", "allow-edits", "autonomous", "bypass"];

pub const THREAD_ROLES: [&str; 4] = ["coordinator", "implementer", "reviewer", "researcher"];

/// The engine every thread written before engines were recorded ran on.
pub const UNRECORDED_ENGINE: &str = "claude";

const MODEL_ID_LIMIT: usize = 256;

pub fn is_agent_engine(value: &str) -> bool {
    AgentEngine::parse(value).is_some()
}

pub fn is_agent_effort(value: &str) -> bool {
    EFFORTS.contains(&value)
}

pub fn is_execution_policy(value: &str) -> bool {
    EXECUTION_POLICIES.contains(&value)
}

pub fn is_thread_role(value: &str) -> bool {
    THREAD_ROLES.contains(&value)
}

/// A well-formed model id, known here or not: no whitespace or control characters, and never a
/// name every JS object inherits.
pub fn is_model_id(value: &str) -> bool {
    !value.is_empty()
        && js::length(value) <= MODEL_ID_LIMIT
        && value.chars().all(|character| !js::is_js_whitespace(character) && !character.is_control())
        && !js::OBJECT_PROTOTYPE_KEYS.contains(&value)
}

/// `modelEngine(BUILT_IN_MODELS, model, preferred)`: the preferred engine where it ships the
/// model, else the first engine that does, else none.
pub fn built_in_model_engine(model: &str, preferred: Option<&str>) -> Option<&'static str> {
    let offering: Vec<&'static str> = AGENT_ENGINES
        .iter()
        .filter(|engine| engine.spec().models.iter().any(|built_in| built_in.id == model))
        .map(|engine| engine.as_str())
        .collect();
    preferred
        .and_then(|preferred| offering.iter().find(|engine| **engine == preferred).copied())
        .or_else(|| offering.first().copied())
}

pub fn has_fast_mode(engine: &str) -> bool {
    AgentEngine::parse(engine).is_some_and(|engine| capabilities_for(engine).fast_mode)
}
