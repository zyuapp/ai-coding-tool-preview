//! What storage validation needs of the engine catalogue (`src/domain/agent-engine.ts`).

use super::js;

/// Engines in the order pickers list them.
pub const ENGINES: [&str; 3] = ["claude", "codex", "opencode"];

/// Every effort, deepest first.
pub const EFFORTS: [&str; 6] = ["ultra", "max", "xhigh", "high", "medium", "low"];

pub const EXECUTION_POLICIES: [&str; 5] = ["confirm", "plan", "allow-edits", "autonomous", "bypass"];

pub const THREAD_ROLES: [&str; 4] = ["coordinator", "implementer", "reviewer", "researcher"];

/// The models each engine ships knowing. OpenCode names its own once asked.
pub const BUILT_IN_MODELS: [(&str, &[&str]); 3] = [
    ("claude", &["fable", "opus", "sonnet", "haiku"]),
    ("codex", &["gpt-6-astra", "gpt-6.1-sol", "gpt-6.1-terra", "gpt-6-luna"]),
    ("opencode", &[]),
];

/// Models the catalogue replaced, each read as its successor.
pub const RETIRED_MODELS: [(&str, &str); 3] = [
    ("gpt-5.6-sol", "gpt-6.1-sol"),
    ("gpt-6-sol", "gpt-6.1-sol"),
    ("gpt-5.6-luna", "gpt-6-luna"),
];

/// Engines with a fast mode.
pub const FAST_MODE_ENGINES: [&str; 1] = ["codex"];

/// The engine every thread written before engines were recorded ran on.
pub const UNRECORDED_ENGINE: &str = "claude";

const MODEL_ID_LIMIT: usize = 256;

pub fn is_agent_engine(value: &str) -> bool {
    ENGINES.contains(&value)
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

/// The model a retired id is now read as; any other id is itself.
pub fn current_model(value: &str) -> &str {
    RETIRED_MODELS
        .iter()
        .find(|(retired, _)| *retired == value)
        .map(|(_, current)| *current)
        .unwrap_or(value)
}

/// `modelEngine(BUILT_IN_MODELS, model, preferred)`: the preferred engine where it ships the
/// model, else the first engine that does, else none.
pub fn built_in_model_engine(model: &str, preferred: Option<&str>) -> Option<&'static str> {
    let offering: Vec<&'static str> =
        BUILT_IN_MODELS.iter().filter(|(_, models)| models.contains(&model)).map(|(engine, _)| *engine).collect();
    preferred
        .and_then(|preferred| offering.iter().find(|engine| **engine == preferred).copied())
        .or_else(|| offering.first().copied())
}

pub fn has_fast_mode(engine: &str) -> bool {
    FAST_MODE_ENGINES.contains(&engine)
}
