//! The engines that run threads, the models each offers, and what each model takes.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::coordination::CoordinationRole;
use super::run::{AgentEffort, ExecutionPolicy, EXECUTION_POLICIES, POLICY_CHOICES};

/// The backends that can run a task. Each offers its own models, efforts, and context windows.
/// `Other` is an engine only a newer build on a paired computer knows: its threads are shown read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentEngine {
    Claude,
    Codex,
    Opencode,
    Other(&'static str),
}

impl AgentEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentEngine::Claude => "claude",
            AgentEngine::Codex => "codex",
            AgentEngine::Opencode => "opencode",
            AgentEngine::Other(name) => name,
        }
    }

    /// One of this build's engines, or None.
    pub fn parse(value: &str) -> Option<AgentEngine> {
        match value {
            "claude" => Some(AgentEngine::Claude),
            "codex" => Some(AgentEngine::Codex),
            "opencode" => Some(AgentEngine::Opencode),
            _ => None,
        }
    }

    /// The engine as another build sent it, kept as sent when this build does not know it.
    pub fn read(value: &str) -> AgentEngine {
        AgentEngine::parse(value).unwrap_or_else(|| AgentEngine::Other(crate::util::intern(value)))
    }

    /// False for an engine only a newer build knows.
    pub fn is_known(self) -> bool {
        !matches!(self, AgentEngine::Other(_))
    }
}

crate::util::as_sent_serde!(AgentEngine, "engine");

pub const DEFAULT_ENGINE: AgentEngine = AgentEngine::Claude;
/// Every engine, in the order pickers list them.
pub const AGENT_ENGINES: [AgentEngine; 3] = [AgentEngine::Claude, AgentEngine::Codex, AgentEngine::Opencode];

/// A model id as an engine names it.
pub type AgentModel = String;

/// The default engine's own default, which a draft lands on when its engine has none to offer.
pub const DEFAULT_MODEL: &str = "opus";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffortSpec {
    pub id: AgentEffort,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// An effort that hands work to the engine's own agents, which a coordinator never takes.
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub delegates: bool,
}

/// An empty `efforts` is a model that takes no effort at all.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub id: AgentModel,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub context_window: i64,
    pub efforts: Vec<EffortSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_compaction: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// The panels and controls an engine can feed; one that cannot is not drawn for its threads.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineCapabilities {
    pub fast_mode: bool,
    pub workflows: bool,
    pub subagents: bool,
    pub subagent_metadata: bool,
    pub review: bool,
    pub chrome_browser: bool,
    pub policies: &'static [ExecutionPolicy],
    pub unattended_policy: ExecutionPolicy,
}

const OPENCODE_POLICIES: [ExecutionPolicy; 4] = [ExecutionPolicy::Confirm, ExecutionPolicy::Plan, ExecutionPolicy::AllowEdits, ExecutionPolicy::Bypass];

/// Asking least first. A mode an engine lacks falls to the nearest one after it.
const POLICY_ORDER: [ExecutionPolicy; 4] = [ExecutionPolicy::Bypass, ExecutionPolicy::Autonomous, ExecutionPolicy::AllowEdits, ExecutionPolicy::Confirm];

pub fn capabilities_for(engine: AgentEngine) -> EngineCapabilities {
    match engine {
        AgentEngine::Claude => EngineCapabilities { fast_mode: false, workflows: true, subagents: true, subagent_metadata: true, review: false, chrome_browser: true, policies: &EXECUTION_POLICIES, unattended_policy: ExecutionPolicy::Autonomous },
        AgentEngine::Codex => EngineCapabilities { fast_mode: true, workflows: false, subagents: true, subagent_metadata: true, review: true, chrome_browser: false, policies: &EXECUTION_POLICIES, unattended_policy: ExecutionPolicy::Autonomous },
        AgentEngine::Opencode => EngineCapabilities { fast_mode: false, workflows: false, subagents: false, subagent_metadata: false, review: false, chrome_browser: false, policies: &OPENCODE_POLICIES, unattended_policy: ExecutionPolicy::Bypass },
        AgentEngine::Other(_) => EngineCapabilities { fast_mode: false, workflows: false, subagents: false, subagent_metadata: false, review: false, chrome_browser: false, policies: &[], unattended_policy: ExecutionPolicy::Confirm },
    }
}

/// Whether an engine can take a run right now, or what stands in the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineAccess {
    Ready,
    SignedOut,
    Unavailable,
    Missing,
    Outdated,
}

/// What the app found out about an engine on this machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineReadiness {
    pub access: EngineAccess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// The catalogue models the installed command can run. Absent means it runs all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<AgentModel>>,
    /// The models the engine names for itself, in place of the ones this build ships knowing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<Vec<ModelSpec>>,
}

impl EngineReadiness {
    pub fn ready() -> Self {
        EngineReadiness { access: EngineAccess::Ready, version: None, required: None, latest: None, fix: None, models: None, catalog: None }
    }
}

/// The readiness of the engines whose readiness can change. One not named is always ready.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EngineStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<EngineReadiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<EngineReadiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode: Option<EngineReadiness>,
    /// Engines only a newer build on a paired computer knows, by its own ids.
    #[serde(flatten, default)]
    pub others: std::collections::BTreeMap<String, EngineReadiness>,
}

impl EngineStatus {
    pub fn get(&self, engine: AgentEngine) -> Option<&EngineReadiness> {
        match engine {
            AgentEngine::Claude => self.claude.as_ref(),
            AgentEngine::Codex => self.codex.as_ref(),
            AgentEngine::Opencode => self.opencode.as_ref(),
            AgentEngine::Other(name) => self.others.get(name),
        }
    }

    pub fn set(&mut self, engine: AgentEngine, readiness: Option<EngineReadiness>) {
        match engine {
            AgentEngine::Claude => self.claude = readiness,
            AgentEngine::Codex => self.codex = readiness,
            AgentEngine::Opencode => self.opencode = readiness,
            AgentEngine::Other(name) => {
                match readiness {
                    Some(readiness) => self.others.insert(name.to_string(), readiness),
                    None => self.others.remove(name),
                };
            }
        }
    }
}

/// What is wrong with an engine's install, said once for the composer, the model menu, and Settings.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineNotice {
    pub blocking: bool,
    pub message: String,
    pub fix: Option<String>,
    pub updatable: bool,
}

pub fn engine_label(engine: AgentEngine) -> &'static str {
    match engine {
        AgentEngine::Claude => "Claude",
        AgentEngine::Codex => "Codex",
        AgentEngine::Opencode => "OpenCode",
        AgentEngine::Other(name) => name,
    }
}

/// What is wrong with this engine on this machine, or nothing when it is fine.
pub fn engine_notice(engine: AgentEngine, readiness: &EngineReadiness) -> Option<EngineNotice> {
    let label = engine_label(engine);
    let named = format!("{label} {}", readiness.version.as_deref().unwrap_or("on this machine"));
    let fix = readiness.fix.clone();
    let updatable = readiness.fix.is_some();
    match readiness.access {
        EngineAccess::Missing => return Some(EngineNotice { blocking: true, message: format!("{label} is not installed."), fix, updatable: false }),
        EngineAccess::Outdated => {
            return Some(EngineNotice { blocking: true, message: format!("{named} is too old. Update to {}.", readiness.required.as_deref().unwrap_or("undefined")), fix, updatable })
        }
        EngineAccess::Unavailable => return Some(EngineNotice { blocking: true, message: format!("{label} is installed but would not start."), fix: None, updatable: false }),
        _ => {}
    }
    if readiness.access == EngineAccess::Ready && readiness.catalog.as_ref().is_some_and(|catalog| catalog.is_empty()) {
        return Some(EngineNotice { blocking: true, message: format!("{label} has no models. Connect a provider first."), fix, updatable: false });
    }
    if let Some(required) = &readiness.required {
        return Some(EngineNotice { blocking: false, message: format!("{named} is behind {required}, so some models are hidden."), fix, updatable });
    }
    None
}

pub fn engine_status_word(readiness: &EngineReadiness) -> &'static str {
    match readiness.access {
        EngineAccess::Missing => "Not installed",
        EngineAccess::Outdated => "Too old to run",
        EngineAccess::Unavailable => "Will not start",
        EngineAccess::SignedOut => "Signed out",
        EngineAccess::Ready if readiness.required.is_some() => "Needs update",
        EngineAccess::Ready if readiness.latest.is_some() => "Update available",
        EngineAccess::Ready => "Ready",
    }
}

pub fn engine_status_line(engine: AgentEngine, readiness: &EngineReadiness) -> String {
    if let Some(notice) = engine_notice(engine, readiness) {
        return notice.message;
    }
    if readiness.access == EngineAccess::SignedOut {
        return format!("Sign in to run threads on {}.", engine_label(engine));
    }
    if let Some(latest) = &readiness.latest {
        let how = if readiness.fix.is_some() { "" } else { "; update it the way you installed it" };
        return format!("Version {}. {latest} is available{how}.", readiness.version.as_deref().unwrap_or("undefined"));
    }
    match &readiness.version {
        Some(version) => format!("Version {version}"),
        None => "Installed and ready.".into(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineUpdate {
    pub target: String,
    pub version: Option<String>,
    pub command: Option<String>,
}

pub fn engine_update(readiness: &EngineReadiness) -> Option<EngineUpdate> {
    let target = readiness.latest.clone().or_else(|| readiness.required.clone())?;
    if readiness.access == EngineAccess::Missing {
        return None;
    }
    Some(EngineUpdate { target, version: readiness.version.clone(), command: readiness.fix.clone() })
}

/// Why this engine cannot take a run, in the words the user needs to clear it.
pub fn engine_blocker(engine: AgentEngine, readiness: &EngineReadiness) -> Option<String> {
    let notice = engine_notice(engine, readiness)?;
    if !notice.blocking {
        return None;
    }
    if notice.updatable {
        return Some(format!("{} Update it in Settings.", notice.message));
    }
    Some(match notice.fix {
        Some(fix) => format!("{} Run `{fix}` to fix it.", notice.message),
        None => notice.message,
    })
}

pub fn engine_is_blocked(engine: AgentEngine, readiness: &EngineReadiness) -> bool {
    engine_notice(engine, readiness).is_some_and(|notice| notice.blocking)
}

pub fn engine_needs_attention(status: Option<&EngineStatus>) -> bool {
    AGENT_ENGINES.iter().any(|engine| status.and_then(|status| status.get(*engine)).is_some_and(|readiness| engine_notice(*engine, readiness).is_some()))
}

fn effort_spec(id: AgentEffort) -> EffortSpec {
    let (label, description, delegates) = match id {
        AgentEffort::Ultra => ("Ultra", Some("Splits the work across agents"), true),
        AgentEffort::Max => ("Max", None, false),
        AgentEffort::Xhigh => ("Extra high", None, false),
        AgentEffort::High => ("High", None, false),
        AgentEffort::Medium => ("Medium", None, false),
        AgentEffort::Low => ("Low", None, false),
        AgentEffort::Other(name) => (name, None, false),
    };
    EffortSpec { id, label: label.into(), description: description.map(Into::into), delegates }
}

/// Every effort, deepest first.
pub const AGENT_EFFORTS: [AgentEffort; 6] = [AgentEffort::Ultra, AgentEffort::Max, AgentEffort::Xhigh, AgentEffort::High, AgentEffort::Medium, AgentEffort::Low];

/// The named efforts as specs, always deepest first however they were named.
pub fn efforts(ids: &[AgentEffort]) -> Vec<EffortSpec> {
    AGENT_EFFORTS.iter().filter(|id| ids.contains(id)).map(|id| effort_spec(*id)).collect()
}

fn efforts_through_max() -> Vec<EffortSpec> {
    efforts(&[AgentEffort::Max, AgentEffort::Xhigh, AgentEffort::High, AgentEffort::Medium, AgentEffort::Low])
}

fn efforts_through_ultra() -> Vec<EffortSpec> {
    efforts(&AGENT_EFFORTS)
}

pub fn effort_label(value: AgentEffort) -> &'static str {
    match value {
        AgentEffort::Ultra => "Ultra",
        AgentEffort::Max => "Max",
        AgentEffort::Xhigh => "Extra high",
        AgentEffort::High => "High",
        AgentEffort::Medium => "Medium",
        AgentEffort::Low => "Low",
        AgentEffort::Other(name) => name,
    }
}

/// The default context window in Codex's model catalogue, used until the server reports its own.
const CODEX_CONTEXT_WINDOW: i64 = 272_000;

fn model(id: &str, label: &str, description: &str, context_window: i64, efforts: Vec<EffortSpec>) -> ModelSpec {
    ModelSpec { id: id.into(), label: label.into(), description: description.into(), context_window, efforts, manual_compaction: Some(true), provider: None }
}

fn built_in_list(engine: AgentEngine) -> Vec<ModelSpec> {
    match engine {
        AgentEngine::Claude => vec![
            model("fable", "Fable", "Most capable for demanding work", 1_000_000, efforts_through_max()),
            model("opus", "Opus", "Best for complex reasoning", 1_000_000, efforts_through_max()),
            model("sonnet", "Sonnet", "Balanced speed and capability", 1_000_000, efforts_through_max()),
            model("haiku", "Haiku", "Fastest for lightweight work", 200_000, Vec::new()),
        ],
        AgentEngine::Codex => vec![
            model("gpt-6-astra", "Astra", "Most capable model for complex, demanding work", CODEX_CONTEXT_WINDOW, efforts_through_ultra()),
            model("gpt-6.1-sol", "Sol", "Strong coding model for everyday work", CODEX_CONTEXT_WINDOW, efforts_through_ultra()),
            model("gpt-6.1-terra", "Terra", "Balanced agentic coding model for everyday work", CODEX_CONTEXT_WINDOW, efforts_through_ultra()),
            model("gpt-6-luna", "Luna", "Efficient model for lightweight work", CODEX_CONTEXT_WINDOW, efforts_through_max()),
        ],
        AgentEngine::Opencode | AgentEngine::Other(_) => Vec::new(),
    }
}

/// Whether the engine ships knowing its models, or only learns them once it names them.
fn models_built_in(engine: AgentEngine) -> bool {
    !matches!(engine, AgentEngine::Opencode)
}

/// The default this build ships for the engine; none for one whose models are only known once it names them.
pub fn default_model_for(engine: AgentEngine) -> Option<&'static str> {
    match engine {
        AgentEngine::Claude => Some(DEFAULT_MODEL),
        AgentEngine::Codex => Some("gpt-6.1-sol"),
        AgentEngine::Opencode | AgentEngine::Other(_) => None,
    }
}

pub fn default_effort_for(_engine: AgentEngine) -> AgentEffort {
    AgentEffort::High
}

const MODEL_ID_LIMIT: usize = 256;
const OBJECT_PROTOTYPE_NAMES: [&str; 12] = [
    "constructor", "__proto__", "__defineGetter__", "__defineSetter__", "hasOwnProperty", "__lookupGetter__", "__lookupSetter__", "isPrototypeOf", "propertyIsEnumerable", "toString", "valueOf", "toLocaleString",
];

/// True for a well-formed model id, known here or not.
pub fn is_model_id(value: &str) -> bool {
    !value.is_empty()
        && crate::util::js_len(value) <= MODEL_ID_LIMIT
        && value.chars().all(|ch| !ch.is_whitespace() && !ch.is_control())
        && !OBJECT_PROTOTYPE_NAMES.contains(&value)
}

/// Models the catalogue replaced, each read as its successor wherever a saved setting still names it.
pub fn current_model(value: &str) -> &str {
    match value {
        "gpt-5.6-sol" | "gpt-6-sol" => "gpt-6.1-sol",
        "gpt-5.6-luna" => "gpt-6-luna",
        other => other,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogEntry {
    pub engine: AgentEngine,
    pub spec: ModelSpec,
    pub efforts: Vec<EffortSpec>,
    pub coordinator_efforts: Vec<EffortSpec>,
}

/// The models each engine offers, and what each of them takes. A model is its engine and its id together.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ModelCatalog {
    pub models: HashMap<AgentEngine, Vec<ModelSpec>>,
    entries: HashMap<(AgentEngine, String), usize>,
    /// Every entry, in catalogue order.
    list: Vec<CatalogEntry>,
}

impl ModelCatalog {
    pub fn from_lists(lists: impl Fn(AgentEngine) -> Vec<ModelSpec>) -> ModelCatalog {
        let mut catalog = ModelCatalog::default();
        for engine in AGENT_ENGINES {
            let mut kept = Vec::new();
            for spec in lists(engine) {
                if !is_model_id(&spec.id) {
                    continue;
                }
                let key = (engine, spec.id.clone());
                if catalog.entries.contains_key(&key) {
                    continue;
                }
                let coordinator_efforts = spec.efforts.iter().filter(|effort| !effort.delegates).cloned().collect();
                catalog.entries.insert(key, catalog.list.len());
                catalog.list.push(CatalogEntry { engine, efforts: spec.efforts.clone(), coordinator_efforts, spec: spec.clone() });
                kept.push(spec);
            }
            catalog.models.insert(engine, kept);
        }
        catalog
    }

    pub fn entry(&self, engine: AgentEngine, model: &str) -> Option<&CatalogEntry> {
        self.entries.get(&(engine, model.to_string())).map(|index| &self.list[*index])
    }

    /// Every engine's entry for an id, in the order engines are listed.
    pub fn by_id(&self, model: &str) -> Vec<&CatalogEntry> {
        let mut found: Vec<&CatalogEntry> = self.list.iter().filter(|entry| entry.spec.id == model).collect();
        found.sort_by_key(|entry| AGENT_ENGINES.iter().position(|engine| *engine == entry.engine));
        found
    }

    pub fn entries(&self) -> &[CatalogEntry] {
        &self.list
    }
}

/// The models this build ships knowing, which stand for every engine that names none of its own.
pub fn built_in_models() -> &'static ModelCatalog {
    static CATALOG: std::sync::OnceLock<ModelCatalog> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| ModelCatalog::from_lists(built_in_list))
}

/// The catalogue a machine's engines make up: each engine's own list where it names one, else the built-in one.
pub fn engine_catalog(status: Option<&EngineStatus>) -> std::borrow::Cow<'static, ModelCatalog> {
    let named = |engine: AgentEngine| status.and_then(|status| status.get(engine)).and_then(|readiness| readiness.catalog.clone());
    if !AGENT_ENGINES.iter().any(|engine| named(*engine).is_some()) {
        return std::borrow::Cow::Borrowed(built_in_models());
    }
    std::borrow::Cow::Owned(ModelCatalog::from_lists(|engine| named(engine).unwrap_or_else(|| built_in_list(engine))))
}

pub fn catalog_has_model(catalog: &ModelCatalog, model: &str) -> bool {
    !catalog.by_id(model).is_empty()
}

/// A model as the engine that runs it offers it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EngineModel {
    pub engine: AgentEngine,
    pub model: AgentModel,
}

pub fn engine_has_model(catalog: &ModelCatalog, engine: AgentEngine, model: &str) -> bool {
    catalog.entry(engine, model).is_some()
}

/// True once an engine's models are known: it ships knowing them, or main has named them.
pub fn engine_models_known(status: Option<&EngineStatus>, engine: AgentEngine) -> bool {
    models_built_in(engine) || status.and_then(|status| status.get(engine)).is_some_and(|readiness| readiness.catalog.is_some())
}

/// The engine for a model named without one: `preferred` where it offers the model, else the first that does.
pub fn model_engine(catalog: &ModelCatalog, model: &str, preferred: Option<AgentEngine>) -> Option<AgentEngine> {
    let entries = catalog.by_id(model);
    entries.iter().find(|entry| Some(entry.engine) == preferred).or(entries.first()).map(|entry| entry.engine)
}

/// The engine a model saved without one belongs to.
pub fn saved_model_engine(model: &str) -> Option<AgentEngine> {
    model_engine(built_in_models(), model, None).or_else(|| AGENT_ENGINES.into_iter().find(|engine| !models_built_in(*engine)))
}

pub fn is_favorite(favorites: &[EngineModel], engine: AgentEngine, model: &str) -> bool {
    favorites.iter().any(|favorite| favorite.engine == engine && favorite.model == model)
}

pub fn unknown_model(model: &str, engine: Option<AgentEngine>) -> String {
    match engine {
        None => format!("No engine here offers the {model} model. Pick another model."),
        Some(engine) => format!("{} here does not offer the {model} model. Pick another model.", engine_label(engine)),
    }
}

pub fn no_model(engine: AgentEngine) -> String {
    format!("{} has not named a model for this thread yet. Pick a model.", engine_label(engine))
}

fn plain_name(name: &str) -> String {
    name.to_lowercase().chars().filter(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()).collect()
}

/// The model a name means, on `engine` when one is named.
pub fn resolve_model(catalog: &ModelCatalog, name: &str, favorites: &[EngineModel], engine: Option<AgentEngine>) -> Result<EngineModel, String> {
    let on = |entries: Vec<&CatalogEntry>| -> Vec<CatalogEntry> { entries.into_iter().filter(|entry| engine.is_none_or(|engine| entry.engine == engine)).cloned().collect() };
    let exact = on(catalog.by_id(name));
    let plain = plain_name(name);
    let matches = if !exact.is_empty() || plain.is_empty() {
        exact
    } else {
        on(catalog.entries().iter().collect())
            .into_iter()
            .filter(|entry| {
                let tail = entry.spec.id.rsplit('/').next().unwrap_or(&entry.spec.id);
                plain_name(tail) == plain || plain_name(&entry.spec.label) == plain
            })
            .collect()
    };
    let pinned: Vec<CatalogEntry> = matches.iter().filter(|entry| is_favorite(favorites, entry.engine, &entry.spec.id)).cloned().collect();
    let meant = if pinned.is_empty() { matches } else { pinned };
    if meant.len() == 1 {
        return Ok(EngineModel { engine: meant[0].engine, model: meant[0].spec.id.clone() });
    }
    if meant.len() > 1 {
        let ids: std::collections::HashSet<&str> = meant.iter().map(|entry| entry.spec.id.as_str()).collect();
        let shared = ids.len() < meant.len();
        let named: Vec<String> = meant.iter().map(|entry| if shared { format!("{} on {}", entry.spec.id, engine_label(entry.engine)) } else { entry.spec.id.clone() }).collect();
        let advice = if shared { "Name the engine as well, or pin the one you mean in the model picker." } else { "Name one of them, or pin the one you mean in the model picker." };
        return Err(format!("{name} could mean {}. {advice}", named.join(", ")));
    }
    Err(unknown_model(name, engine))
}

pub fn model_supports_manual_compaction(catalog: &ModelCatalog, engine: AgentEngine, model: &str) -> bool {
    catalog.entry(engine, model).is_some_and(|entry| entry.spec.manual_compaction == Some(true))
}

pub fn models_for(catalog: &ModelCatalog, engine: AgentEngine) -> &[ModelSpec] {
    catalog.models.get(&engine).map(Vec::as_slice).unwrap_or(&[])
}

pub fn engine_has_effort(catalog: &ModelCatalog, engine: AgentEngine, effort: AgentEffort) -> bool {
    models_for(catalog, engine).iter().any(|model| model.efforts.iter().any(|spec| spec.id == effort))
}

/// The efforts this model takes, deepest first. A coordinator takes none that delegate.
pub fn efforts_for(catalog: &ModelCatalog, engine: AgentEngine, model: &str, role: Option<CoordinationRole>) -> Vec<EffortSpec> {
    match catalog.entry(engine, model) {
        None => Vec::new(),
        Some(entry) if role == Some(CoordinationRole::Coordinator) => entry.coordinator_efforts.clone(),
        Some(entry) => entry.efforts.clone(),
    }
}

pub fn model_has_effort(catalog: &ModelCatalog, engine: AgentEngine, model: &str, effort: AgentEffort, role: Option<CoordinationRole>) -> bool {
    efforts_for(catalog, engine, model, role).iter().any(|spec| spec.id == effort)
}

pub fn model_takes_effort(catalog: &ModelCatalog, engine: AgentEngine, model: &str) -> bool {
    !efforts_for(catalog, engine, model, None).is_empty()
}

/// The effort a run on this model actually gets: the one asked for where the model takes it, else the nearest one below.
pub fn effort_for_model(catalog: &ModelCatalog, engine: AgentEngine, model: &str, effort: AgentEffort, role: Option<CoordinationRole>) -> AgentEffort {
    if !effort.is_known() {
        return effort;
    }
    let offered = efforts_for(catalog, engine, model, role);
    if offered.is_empty() || offered.iter().any(|spec| spec.id == effort) {
        return effort;
    }
    let asked = AGENT_EFFORTS.iter().position(|id| *id == effort).unwrap_or(0);
    offered
        .iter()
        .find(|spec| AGENT_EFFORTS.iter().position(|id| *id == spec.id).unwrap_or(0) >= asked)
        .or(offered.last())
        .map(|spec| spec.id)
        .unwrap_or(effort)
}

/// The engine's default where the catalogue offers it, else the first model the engine lists.
pub fn default_model_in(catalog: &ModelCatalog, engine: AgentEngine) -> Option<AgentModel> {
    let fixed = default_model_for(engine);
    if let Some(fixed) = fixed {
        if engine_has_model(catalog, engine, fixed) {
            return Some(fixed.to_string());
        }
    }
    models_for(catalog, engine).first().map(|spec| spec.id.clone()).or(fixed.map(String::from))
}

/// The model a thread runs on: its own, else its engine's default in the catalogue.
pub fn thread_model(catalog: &ModelCatalog, engine: AgentEngine, model: Option<&str>) -> Option<AgentModel> {
    model.map(String::from).or_else(|| default_model_in(catalog, engine))
}

/// The mode a run on this engine gets: the one asked for where the engine offers it, else the nearest that asks more.
pub fn policy_for(engine: AgentEngine, policy: ExecutionPolicy) -> ExecutionPolicy {
    let offered = capabilities_for(engine).policies;
    if offered.contains(&policy) {
        return policy;
    }
    match POLICY_ORDER.iter().position(|candidate| *candidate == policy) {
        Some(asked) => POLICY_ORDER[asked..].iter().copied().find(|candidate| offered.contains(candidate)).unwrap_or(ExecutionPolicy::Confirm),
        None => ExecutionPolicy::Confirm,
    }
}

/// The modes a picker offers for an engine, in the one order every picker lists them.
pub fn offered_policies(engine: AgentEngine) -> Vec<ExecutionPolicy> {
    let offered = capabilities_for(engine).policies;
    POLICY_CHOICES.iter().copied().filter(|policy| offered.contains(policy)).collect()
}

/// A model by the name the composer gives it, or by its id for one the catalogue does not hold.
pub fn model_label(catalog: &ModelCatalog, engine: Option<AgentEngine>, model: &str) -> String {
    let entry = match engine {
        None => catalog.by_id(model).first().map(|entry| entry.spec.label.clone()),
        Some(engine) => catalog.entry(engine, model).map(|entry| entry.spec.label.clone()),
    };
    entry.unwrap_or_else(|| model.to_string())
}

/// A model the engine does not offer is measured against the engine's default model.
pub fn context_window_limit(catalog: &ModelCatalog, engine: AgentEngine, model: &str) -> i64 {
    if let Some(entry) = catalog.entry(engine, model) {
        return entry.spec.context_window;
    }
    default_model_in(catalog, engine).and_then(|model| catalog.entry(engine, &model).map(|entry| entry.spec.context_window)).unwrap_or(0)
}

/// An effort Claude does not offer lands on its default.
pub fn claude_effort(effort: AgentEffort) -> AgentEffort {
    if matches!(effort, AgentEffort::Ultra | AgentEffort::Other(_)) {
        AgentEffort::High
    } else {
        effort
    }
}

/// What stands in for the composer of a thread on an engine only a newer build knows, which this one shows read-only.
pub fn unopenable_engine(engine: &str) -> String {
    format!("Runs on {engine}, which this version can't open. Update to work in it here.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_clamps_to_nearest_offered_below() {
        let catalog = built_in_models();
        assert_eq!(effort_for_model(catalog, AgentEngine::Claude, "opus", AgentEffort::Ultra, None), AgentEffort::Max);
        assert_eq!(effort_for_model(catalog, AgentEngine::Codex, "gpt-6.1-sol", AgentEffort::Ultra, Some(CoordinationRole::Coordinator)), AgentEffort::Max);
        assert_eq!(effort_for_model(catalog, AgentEngine::Claude, "haiku", AgentEffort::Low, None), AgentEffort::Low);
    }

    #[test]
    fn policy_falls_to_nearest_that_asks_more() {
        assert_eq!(policy_for(AgentEngine::Opencode, ExecutionPolicy::Autonomous), ExecutionPolicy::AllowEdits);
        assert_eq!(policy_for(AgentEngine::Claude, ExecutionPolicy::Autonomous), ExecutionPolicy::Autonomous);
    }

    fn named_specs() -> Vec<ModelSpec> {
        let effort = |id, label: &str| EffortSpec { id, label: label.into(), description: None, delegates: false };
        vec![
            ModelSpec { id: "openai/gpt-9".into(), label: "GPT-9".into(), description: "Named by the engine".into(), context_window: 400_000, efforts: vec![effort(AgentEffort::High, "High"), effort(AgentEffort::Low, "Low")], manual_compaction: Some(true), provider: None },
            ModelSpec { id: "local/tiny".into(), label: "Tiny".into(), description: "No effort".into(), context_window: 8_000, efforts: vec![], manual_compaction: None, provider: None },
        ]
    }

    fn codex_naming(catalog: Vec<ModelSpec>) -> EngineStatus {
        EngineStatus { codex: Some(EngineReadiness { catalog: Some(catalog), ..EngineReadiness::ready() }), ..EngineStatus::default() }
    }

    #[test]
    fn engines_default_to_what_they_offer_and_models_belong_to_their_engine() {
        let catalog = built_in_models();
        for engine in AGENT_ENGINES.into_iter().filter(|engine| default_model_for(*engine).is_some()) {
            assert!(engine_has_model(catalog, engine, default_model_for(engine).unwrap()));
            assert!(engine_has_effort(catalog, engine, default_effort_for(engine)));
            let default_window = context_window_limit(catalog, engine, default_model_for(engine).unwrap());
            let foreign = AGENT_ENGINES.iter().flat_map(|other| models_for(catalog, *other)).find(|model| !engine_has_model(catalog, engine, &model.id)).unwrap();
            assert_eq!(context_window_limit(catalog, engine, &foreign.id), default_window);
            for model in models_for(catalog, engine) {
                assert_eq!(model_supports_manual_compaction(catalog, engine, &model.id), model.manual_compaction == Some(true));
            }
        }
        assert!(!engine_has_model(catalog, AgentEngine::Codex, "opus"));
        assert!(!engine_has_model(catalog, AgentEngine::Claude, "gpt-6.1-sol"));
        assert!(!engine_has_effort(catalog, AgentEngine::Claude, AgentEffort::Ultra));
        assert!(engine_has_effort(catalog, AgentEngine::Codex, AgentEffort::Ultra));
        assert!(engine_has_effort(catalog, AgentEngine::Claude, AgentEffort::Max));
        assert!(!catalog_has_model(catalog, "claude-opus-4") && !catalog_has_model(catalog, "constructor") && catalog_has_model(catalog, "opus"));
        assert!(model_supports_manual_compaction(catalog, AgentEngine::Codex, "gpt-6-astra"));
        assert!(model_supports_manual_compaction(catalog, AgentEngine::Claude, "opus"));
        assert!(!model_supports_manual_compaction(catalog, AgentEngine::Claude, "gpt-6.1-sol"));
    }

    #[test]
    fn model_ids_are_well_formed_known_or_not() {
        for id in ["opus", "gpt-6.1-sol", "anthropic/claude-opus-5-5", "openrouter/meta/llama-4:free", &"x".repeat(256)] {
            assert!(is_model_id(id), "{id}");
        }
        for id in ["", " ", "has space", "tab\tid", "line\nid", "nul\u{0}id", &"x".repeat(257), "constructor", "toString", "__proto__", "hasOwnProperty", "valueOf"] {
            assert!(!is_model_id(id), "{id}");
        }
    }

    #[test]
    fn an_engine_naming_its_own_models_replaces_only_its_own_list() {
        assert!(matches!(engine_catalog(None), std::borrow::Cow::Borrowed(_)));
        let status = codex_naming(named_specs());
        let own = engine_catalog(Some(&status));
        assert_eq!(models_for(&own, AgentEngine::Codex).iter().map(|spec| spec.id.as_str()).collect::<Vec<_>>(), ["openai/gpt-9", "local/tiny"]);
        assert_eq!(models_for(&own, AgentEngine::Claude), models_for(built_in_models(), AgentEngine::Claude));
        assert_eq!(model_engine(&own, "openai/gpt-9", None), Some(AgentEngine::Codex));
        assert_eq!(model_engine(&own, "gpt-6.1-sol", None), None);
        assert_eq!(model_label(&own, Some(AgentEngine::Codex), "openai/gpt-9"), "GPT-9");
        assert_eq!(model_label(&own, Some(AgentEngine::Codex), "gpt-6.1-sol"), "gpt-6.1-sol");
        assert_eq!(context_window_limit(&own, AgentEngine::Codex, "openai/gpt-9"), 400_000);
        assert!(!model_supports_manual_compaction(&own, AgentEngine::Codex, "local/tiny"));
        assert!(!model_takes_effort(&own, AgentEngine::Codex, "local/tiny"));
        assert_eq!(effort_for_model(&own, AgentEngine::Codex, "openai/gpt-9", AgentEffort::Max, None), AgentEffort::High);
        assert_eq!(effort_for_model(&own, AgentEngine::Codex, "openai/gpt-9", AgentEffort::Medium, None), AgentEffort::Low);
        assert!(!engine_has_effort(&own, AgentEngine::Codex, AgentEffort::Ultra));
    }

    #[test]
    fn a_model_two_engines_name_is_each_ones_own() {
        let mut gateway = named_specs().remove(0);
        gateway.id = "opus".into();
        gateway.label = "Gateway Opus".into();
        gateway.context_window = 64_000;
        gateway.efforts.remove(0);
        let mut again = named_specs().remove(0);
        again.label = "Again".into();
        let status = codex_naming(vec![gateway, named_specs().remove(0), again]);
        let own = engine_catalog(Some(&status));
        assert_eq!(models_for(&own, AgentEngine::Codex).iter().map(|spec| spec.label.as_str()).collect::<Vec<_>>(), ["Gateway Opus", "GPT-9"]);
        assert_eq!((model_label(&own, Some(AgentEngine::Claude), "opus"), model_label(&own, Some(AgentEngine::Codex), "opus")), ("Opus".into(), "Gateway Opus".into()));
        assert_eq!((context_window_limit(&own, AgentEngine::Claude, "opus"), context_window_limit(&own, AgentEngine::Codex, "opus")), (1_000_000, 64_000));
        assert_eq!(effort_for_model(&own, AgentEngine::Codex, "opus", AgentEffort::Max, None), AgentEffort::Low);
        assert_eq!(model_engine(&own, "opus", None), Some(AgentEngine::Claude));
        assert_eq!(model_engine(&own, "opus", Some(AgentEngine::Codex)), Some(AgentEngine::Codex));
        let codex_opus = EngineModel { engine: AgentEngine::Codex, model: "opus".into() };
        assert_eq!(resolve_model(&own, "opus", &[], Some(AgentEngine::Codex)), Ok(codex_opus.clone()));
        assert_eq!(resolve_model(&own, "opus", &[], None), Err("opus could mean opus on Claude, opus on Codex. Name the engine as well, or pin the one you mean in the model picker.".into()));
        assert_eq!(resolve_model(&own, "opus", std::slice::from_ref(&codex_opus), None), Ok(codex_opus));
        assert_eq!(resolve_model(&own, "gpt-6.1-sol", &[], Some(AgentEngine::Codex)), Err("Codex here does not offer the gpt-6.1-sol model. Pick another model.".into()));
    }

    #[test]
    fn resolve_model_reads_labels_and_refuses_ambiguity() {
        let catalog = built_in_models();
        assert_eq!(resolve_model(catalog, "Sol", &[], None).unwrap().model, "gpt-6.1-sol");
        assert!(resolve_model(catalog, "nothing", &[], None).is_err());
    }
}
