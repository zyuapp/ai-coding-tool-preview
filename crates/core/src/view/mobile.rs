//! The phone's view, derived from workspace state alone, and the difference between two of them.
//! Nothing here reads state the desktop does not already derive, so a phone and the window in front
//! of the user never disagree about what a thread is doing.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::commands::AppCommand;
use crate::domain::appearance::{theme_for, theme_or_default, ThemeMode, ThemeVariant};
use crate::domain::conversation::{ConversationMessage, MessageKind};
use crate::domain::coordination::is_coordinator;
use crate::domain::diff::{default_branch_range, DiffRange};
use crate::domain::engine::{built_in_models, capabilities_for, efforts, engine_catalog, engine_label, model_label, offered_policies, AgentEngine, AgentModel, EngineAccess, EngineStatus, ModelSpec, AGENT_EFFORTS, AGENT_ENGINES};
use crate::domain::message_origin::is_users_own;
use crate::domain::platform::{bar_share, plan_label, session_and_week, tightest_window, usage_warning_key, window_span, windows_for, AppUpdateRequest, DictationModel, PlanUsage, UsageWindow, NEAR_LIMIT_SHARE};
use crate::domain::project::project_name;
use crate::domain::question::PendingQuestion;
use crate::domain::run::{order_subagents, process_label, subagent_line, AgentEffort, BackgroundProcess, BackgroundProcessKind, ExecutionPolicy, Subagent, SubagentStatus};
use crate::domain::thread::{has_unread_attention, newest_unread_finding, Thread, ThreadOutcome};
use crate::domain::thread_role::{ThreadRole, THREAD_ROLES};
use crate::domain::tool_call::{describe_tool_call, ToolFamily};
use crate::domain::usage_limit::LimitWindow;
use crate::domain::workflow::{workflow_label, Workflow, WorkflowStatus};
use crate::domain::worktree::worktree_name;
use crate::events::ChangedFilesResult;
use crate::state::{diff_for, draining_queued, PairedComputer, QueuedMessage, WorkspaceState};
use crate::validation::{phone_command, supports_computer_command};
use crate::view::computers::computer_of_thread;
use crate::view::coordination::{CoordinationDecisionView, ProposalView};
use crate::view::dock::{worktree_choices, WorktreeChoice};
use crate::view::settings::{own_upkeep, UpkeepTone};
use crate::view::thread_projection::{clip_text, thread_summaries, thread_transcript, ProjectScope, ThreadFilter, ThreadMessage};
use crate::view::workspace::{derive_view, WorkspaceView};
use crate::workspace::activity::{thread_activity, ThreadActivity};
use crate::workspace::coordination::{coordination_members, coordinator_roster, own_coordination_sections, row_headline, waiting_line};
use crate::workspace::location::{project_for, thread_workspace_id, ThreadLocation};
use crate::workspace::order::order_refs;

pub const MOBILE_THREAD_LIMIT: usize = 100;
pub const MOBILE_TRANSCRIPT_MESSAGES: usize = 40;
pub const MOBILE_HISTORY_PAGE: usize = 100;
pub const MOBILE_MESSAGE_TEXT_LIMIT: usize = 40_000;
pub const MOBILE_BACKGROUND_LIMIT: usize = 100;
pub const MOBILE_BACKGROUND_DETAIL_LIMIT: usize = 300;
pub const MOBILE_APPROVAL_DETAIL_LIMIT: usize = 4_000;
pub const MOBILE_DECISION_LIMIT: usize = 20;
pub const MOBILE_SUGGESTION_PROMPT_LIMIT: usize = 4_000;
pub const MOBILE_PROTOCOL_VERSION: i64 = 8;
pub const OLDEST_PHONE_PROTOCOL_VERSION: i64 = 8;

const ARCHIVE_RETENTION_MS: i64 = 5 * 24 * 60 * 60 * 1000;
const ACTIVITY_DRIFT_MS: i64 = 60_000;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MobileRunStatus {
    Idle,
    Running,
    Stopped,
    AwaitingApproval,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileUpkeepState {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileUpkeepAction {
    pub label: String,
    pub command: AppCommand,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileUpkeepItem {
    pub key: String,
    pub label: String,
    pub version: Option<String>,
    pub state: MobileUpkeepState,
    pub action: Option<MobileUpkeepAction>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub installing: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileThreadEntry {
    pub id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub status: MobileRunStatus,
    pub last_activity_at: i64,
    pub unread: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ThreadOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub waiting: bool,
    pub attention: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub snoozable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MobileActivity {
    pub priority: Vec<MobileThreadEntry>,
    pub running: Vec<MobileThreadEntry>,
    pub threads: Vec<MobileThreadEntry>,
    pub snoozed: Vec<MobileThreadEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileProjectGroup {
    pub project_id: Option<String>,
    pub name: String,
    pub threads: Vec<MobileThreadEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileToolCall {
    pub family: String,
    pub label: String,
    pub argument: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigil: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub verb: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileMessage {
    pub kind: MessageKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<MobileToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileMember {
    pub id: String,
    pub title: String,
    pub status: MobileRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileBackgroundItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub status: String,
    pub live: bool,
    pub stoppable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stopping: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileApproval {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub approval_id: String,
    pub run_id: String,
    pub title: String,
    pub description: String,
    pub tool_name: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileDecisionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub recommended: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileDecision {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub options: Vec<MobileDecisionOption>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileBrief {
    pub intent: String,
    pub done_when: String,
    pub delivers: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileSuggestion {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<MobileBrief>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub starting: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileLimitPause {
    pub resets_at: i64,
    pub window: LimitWindow,
    pub position: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileArchivedThread {
    pub id: String,
    pub title: String,
    pub project_name: Option<String>,
    pub archived_at: i64,
    pub deletes_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileQueuedMessage {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steering: Option<bool>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub editable: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MobileLocation {
    Local,
    Creating,
    Releasing,
    Worktree { name: String, threads: usize },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileChanges {
    pub branch: Option<String>,
    pub files: usize,
    pub additions: i64,
    pub deletions: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileThreadSettings {
    pub engine: AgentEngine,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentModel>,
    pub effort: AgentEffort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
    pub policy: ExecutionPolicy,
    #[serde(default, skip_serializing_if = "is_false")]
    pub coordinating: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileModel {
    pub id: AgentModel,
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub efforts: Vec<AgentEffort>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileEngine {
    pub access: EngineAccess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<AgentModel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<Vec<MobileModel>>,
}

pub type MobileEngines = crate::util::Map<MobileEngine>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileThreadView {
    #[serde(default, skip_serializing_if = "is_false")]
    pub loading: bool,
    pub id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub worktree_id: Option<String>,
    pub messages: Vec<MobileMessage>,
    pub omitted: usize,
    pub streaming_tail: Option<String>,
    pub status: MobileRunStatus,
    pub approval: Option<MobileApproval>,
    #[serde(default)]
    pub question: Option<PendingQuestion>,
    pub queued: Vec<MobileQueuedMessage>,
    pub settings: MobileThreadSettings,
    pub location: MobileLocation,
    pub worktrees: Vec<WorktreeChoice>,
    pub can_move: bool,
    pub changes: Option<MobileChanges>,
    pub reviewable: bool,
    pub members: Vec<MobileMember>,
    pub role: Option<ThreadRole>,
    pub background: Vec<MobileBackgroundItem>,
    pub branch_range: DiffRange,
    pub decisions: Vec<MobileDecision>,
    pub suggestions: Vec<MobileSuggestion>,
    pub limit: Option<MobileLimitPause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<String>,
    pub stoppable: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileDraftView {
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub settings: MobileThreadSettings,
    pub worktree: bool,
    pub worktree_name: Option<String>,
    pub worktrees: Vec<WorktreeChoice>,
    pub can_worktree: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileTheme {
    pub dark: String,
    pub light: String,
    pub mode: ThemeMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileUsageWindow {
    pub id: String,
    pub label: String,
    pub share: Option<i64>,
    pub resets_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileEngineUsage {
    pub engine: AgentEngine,
    pub label: String,
    pub plan: Option<String>,
    pub windows: Vec<MobileUsageWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileMeterRow {
    pub span: String,
    pub share: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileUsageWarning {
    pub key: String,
    pub share: i64,
    pub window: String,
    pub resets_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileComposerUsage {
    pub share: i64,
    pub meter: Vec<MobileMeterRow>,
    pub spends: Vec<String>,
    pub warning: Option<MobileUsageWarning>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MobileUsage {
    pub engines: Vec<MobileEngineUsage>,
    pub composer: Option<MobileComposerUsage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileView {
    pub groups: Vec<MobileProjectGroup>,
    pub activity: MobileActivity,
    pub theme: MobileTheme,
    pub engines: MobileEngines,
    pub upkeep: Vec<MobileUpkeepItem>,
    pub dictation: bool,
    pub usage: MobileUsage,
    pub archived: usize,
    pub thread: Option<MobileThreadView>,
    pub draft: Option<MobileDraftView>,
    pub error: Option<String>,
}

pub fn empty_mobile_view() -> MobileView {
    MobileView {
        groups: Vec::new(),
        activity: MobileActivity::default(),
        theme: MobileTheme { dark: "aicodingtool-dark".into(), light: "aicodingtool-light".into(), mode: ThemeMode::Dark },
        engines: MobileEngines::new(),
        upkeep: Vec::new(),
        dictation: false,
        usage: MobileUsage::default(),
        archived: 0,
        thread: None,
        draft: None,
        error: None,
    }
}

fn mobile_model(spec: &ModelSpec) -> MobileModel {
    MobileModel { id: spec.id.clone(), label: spec.label.clone(), description: spec.description.clone(), provider: spec.provider.clone(), efforts: spec.efforts.iter().map(|effort| effort.id).collect() }
}

/// The engines as a phone reads them.
pub fn project_mobile_engines(status: Option<&EngineStatus>) -> MobileEngines {
    let mut engines = MobileEngines::new();
    let Some(status) = status else { return engines };
    for engine in AGENT_ENGINES {
        let Some(readiness) = status.get(engine) else { continue };
        engines.insert(engine.as_str().into(), MobileEngine { access: readiness.access, models: readiness.models.clone(), catalog: readiness.catalog.as_ref().map(|catalog| catalog.iter().map(mobile_model).collect()) });
    }
    engines
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobilePolicyTerm {
    pub id: ExecutionPolicy,
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileEngineTerms {
    pub id: AgentEngine,
    pub label: String,
    pub fast_mode: bool,
    pub policies: Vec<MobilePolicyTerm>,
    pub models: Vec<MobileModel>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileRoleTerm {
    pub id: ThreadRole,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileVocabulary {
    pub engines: Vec<MobileEngineTerms>,
    pub efforts: Vec<crate::domain::engine::EffortSpec>,
    pub roles: Vec<MobileRoleTerm>,
}

/// What this build calls its engines, modes, models and efforts, as a phone app built apart from it reads them.
pub fn mobile_vocabulary() -> MobileVocabulary {
    let builtin = built_in_models();
    MobileVocabulary {
        engines: AGENT_ENGINES
            .iter()
            .map(|engine| MobileEngineTerms {
                id: *engine,
                label: engine_label(*engine).into(),
                fast_mode: capabilities_for(*engine).fast_mode,
                policies: offered_policies(*engine).into_iter().map(|policy| MobilePolicyTerm { id: policy, label: policy.label().into(), description: policy.description().into() }).collect(),
                models: builtin.models.get(engine).map(|models| models.iter().map(mobile_model).collect()).unwrap_or_default(),
            })
            .collect(),
        efforts: efforts(&AGENT_EFFORTS),
        roles: THREAD_ROLES.iter().map(|role| MobileRoleTerm { id: *role, label: role.label().into() }).collect(),
    }
}

fn untitled(title: &str) -> String {
    if title.is_empty() {
        "Untitled thread".into()
    } else {
        title.to_string()
    }
}

/// Everything a phone reads, derived from workspace state alone.
pub fn project_mobile_view(state: &WorkspaceState, at: i64) -> MobileView {
    let view = derive_view(state);
    let collections = &view.collections;
    let here = ThreadActivity { working: collections.every_busy.clone(), blocked: collections.every_blocked.clone(), ranked: collections.every_ranked.clone(), ..ThreadActivity::default() };
    let summaries = thread_summaries(state, &ThreadFilter { scope: Some(ProjectScope::All), limit: Some(MOBILE_THREAD_LIMIT), ..ThreadFilter::default() }, at);
    let mut entries: HashMap<Option<String>, Vec<MobileThreadEntry>> = HashMap::new();
    let mut entries_by_id: HashMap<String, MobileThreadEntry> = HashMap::new();
    for summary in &summaries {
        let thread = state.thread(&summary.id);
        let project = summary.project_id.as_deref().and_then(|id| state.project(id));
        let roster = thread.map(|thread| coordinator_roster(state, thread, &here, None));
        let waiting = roster.as_ref().and_then(|roster| waiting_line(&roster.waiting, "you"));
        let headline = thread.and_then(|thread| row_headline(thread, waiting.clone()));
        let blocked = collections.every_blocked.contains(&summary.id) || roster.as_ref().is_some_and(|roster| roster.blocked);
        let running = summary.status == crate::state::ThreadRunStatus::Running || roster.as_ref().is_some_and(|roster| roster.running);
        let status = if blocked {
            MobileRunStatus::AwaitingApproval
        } else if running {
            MobileRunStatus::Running
        } else {
            match summary.status {
                crate::state::ThreadRunStatus::Stopped => MobileRunStatus::Stopped,
                crate::state::ThreadRunStatus::Running => MobileRunStatus::Running,
                crate::state::ThreadRunStatus::Idle => MobileRunStatus::Idle,
            }
        };
        let entry = MobileThreadEntry {
            id: summary.id.clone(),
            title: untitled(&summary.title),
            project_id: project.map(|project| project.id.clone()),
            project_name: project.map(project_name),
            status,
            last_activity_at: summary.last_activity_at,
            unread: thread.is_some_and(|thread| thread.outcome_unread || newest_unread_finding(thread).is_some()) || roster.as_ref().is_some_and(|roster| roster.threads.iter().any(|member| has_unread_attention(member))),
            outcome: thread.and_then(|thread| thread.outcome),
            headline,
            waiting: waiting.is_some(),
            attention: roster.as_ref().is_some_and(|roster| roster.dismissable),
            snoozable: false,
            snoozed_until: thread.and_then(|thread| thread.snoozed_until),
        };
        entries.entry(summary.project_id.clone()).or_default().push(entry.clone());
        entries_by_id.insert(entry.id.clone(), entry);
    }
    let mut groups: Vec<MobileProjectGroup> = collections
        .lists
        .projects
        .iter()
        .map(|project| MobileProjectGroup { project_id: Some(project.id.clone()), name: project_name(project), threads: rank_group(entries.remove(&Some(project.id.clone())).unwrap_or_default(), state) })
        .collect();
    groups.push(MobileProjectGroup { project_id: None, name: "Recents".into(), threads: rank_group(entries.remove(&None).unwrap_or_default(), state) });
    let thread = project_mobile_thread(state, &view);
    MobileView {
        groups,
        activity: project_mobile_activity(state, &here, &entries_by_id, &view),
        theme: project_mobile_theme(state),
        engines: project_mobile_engines(state.engine_status.as_ref()),
        upkeep: project_mobile_upkeep(state),
        dictation: state.dictation.model == DictationModel::Ready,
        usage: project_mobile_usage(state, &view),
        archived: collections.archived_threads.len(),
        draft: if thread.is_some() { None } else { Some(project_mobile_draft(state, &view)) },
        thread,
        error: state.action_error.clone(),
    }
}

fn share(utilization: Option<f64>) -> Option<i64> {
    utilization.map(|value| bar_share(Some(value)).round() as i64)
}

fn project_mobile_usage(state: &WorkspaceState, view: &WorkspaceView) -> MobileUsage {
    let reports = &state.plan_usage.reports;
    let drawn = |item: &UsageWindow| MobileUsageWindow { id: item.id.clone(), label: item.label.clone(), share: share(item.utilization), resets_at: item.resets_at.clone() };
    let engines: Vec<MobileEngineUsage> = AGENT_ENGINES
        .iter()
        .filter_map(|engine| match reports.get(engine) {
            Some(PlanUsage::Available { subscription, windows }) => Some(MobileEngineUsage { engine: *engine, label: engine_label(*engine).into(), plan: plan_label(subscription.as_deref()), windows: windows.iter().map(drawn).collect(), message: None }),
            Some(PlanUsage::Unavailable { message }) => Some(MobileEngineUsage { engine: *engine, label: engine_label(*engine).into(), plan: None, windows: Vec::new(), message: Some(message.clone()) }),
            _ => None,
        })
        .collect();
    if engines.is_empty() {
        return MobileUsage::default();
    }
    let engine = view.thread.engine;
    let spent = match &view.thread.model {
        Some(model) => windows_for(reports.get(&engine), &model_label(&engine_catalog(state.engine_status.as_ref()), Some(engine), model)),
        None => Vec::new(),
    };
    let composer = tightest_window(&spent).map(|tightest| {
        let (session, week) = session_and_week(&spent);
        let rows: Vec<UsageWindow> = if session.is_some() || week.is_some() { [session, week].into_iter().flatten().collect() } else { vec![tightest.clone()] };
        let key = usage_warning_key(engine.as_str(), &tightest);
        let warned = view.thread.limit_pause.is_none() && tightest.utilization.unwrap_or(0.0) >= NEAR_LIMIT_SHARE && !state.plan_usage.dismissed.contains(&key);
        MobileComposerUsage {
            share: share(tightest.utilization).unwrap_or(0),
            meter: rows.iter().map(|row| MobileMeterRow { span: window_span(row).unwrap_or_else(|| row.label.clone()), share: share(row.utilization).unwrap_or(0) }).collect(),
            spends: spent.iter().map(|item| item.id.clone()).collect(),
            warning: warned.then(|| MobileUsageWarning { key, share: share(tightest.utilization).unwrap_or(0), window: format!("{} · {}", engine_label(engine), tightest.label), resets_at: tightest.resets_at.clone() }),
        }
    });
    MobileUsage { engines, composer }
}

fn tone_name(tone: UpkeepTone) -> &'static str {
    match tone {
        UpkeepTone::Live => "live",
        UpkeepTone::Quiet => "quiet",
        UpkeepTone::Progress => "progress",
        UpkeepTone::Offer => "offer",
        UpkeepTone::Error => "error",
    }
}

fn project_mobile_upkeep(state: &WorkspaceState) -> Vec<MobileUpkeepItem> {
    let installing = matches!(state.app_update_request, Some(AppUpdateRequest::Installing { .. }));
    own_upkeep(state)
        .into_iter()
        .map(|item| MobileUpkeepItem {
            installing: installing && item.key == "app",
            action: item.action.filter(|action| serde_json::to_value(&action.command).ok().is_some_and(|value| phone_command(&value).is_ok())).map(|action| MobileUpkeepAction { label: action.label, command: action.command }),
            state: MobileUpkeepState { text: item.state.text, tone: item.state.tone.map(|tone| tone_name(tone).into()) },
            key: item.key,
            label: item.label,
            version: item.version,
        })
        .collect()
}

fn project_mobile_activity(state: &WorkspaceState, here: &ThreadActivity, entries: &HashMap<String, MobileThreadEntry>, view: &WorkspaceView) -> MobileActivity {
    let listed: Vec<&Thread> = view.collections.visible_threads.iter().copied().filter(|thread| entries.contains_key(&thread.id)).collect();
    let paired: Vec<&PairedComputer> = state.computers.paired.iter().collect();
    let (sections, _) = own_coordination_sections(state, &listed, here, &paired);
    let rows = |threads: &[&Thread]| -> Vec<MobileThreadEntry> { threads.iter().filter_map(|thread| entries.get(&thread.id).cloned()).collect() };
    MobileActivity {
        priority: rows(&sections.priority).into_iter().map(|entry| MobileThreadEntry { snoozable: true, ..entry }).collect(),
        running: rows(&sections.running),
        threads: rows(&sections.threads),
        snoozed: rows(&sections.snoozed),
    }
}

/// The desktop's family in both faces, so a phone on "auto" can pick its own.
pub fn project_mobile_theme(state: &WorkspaceState) -> MobileTheme {
    let family = theme_or_default(&state.theme).family;
    MobileTheme { dark: theme_for(family, ThemeVariant::Dark).id.into(), light: theme_for(family, ThemeVariant::Light).id.into(), mode: state.theme_mode }
}

/// One group's rows: threads waiting on the user first, then the rest in the sidebar's own order.
pub fn rank_group(entries: Vec<MobileThreadEntry>, state: &WorkspaceState) -> Vec<MobileThreadEntry> {
    let (blocked, rest): (Vec<MobileThreadEntry>, Vec<MobileThreadEntry>) = entries.into_iter().partition(|entry| entry.status == MobileRunStatus::AwaitingApproval);
    let threads: Vec<&Thread> = rest.iter().filter_map(|entry| state.thread(&entry.id)).collect();
    let mut by_thread: HashMap<String, MobileThreadEntry> = rest.into_iter().map(|entry| (entry.id.clone(), entry)).collect();
    let mut ranked = blocked;
    ranked.extend(order_refs(&threads).into_iter().filter_map(|thread| by_thread.remove(&thread.id)));
    ranked
}

fn settings_of(view: &WorkspaceView) -> MobileThreadSettings {
    let thread = &view.thread;
    MobileThreadSettings { engine: thread.engine, model: thread.model.clone(), effort: thread.effort, fast_mode: Some(thread.fast_mode), policy: thread.policy, coordinating: thread.coordinating }
}

fn project_mobile_draft(state: &WorkspaceState, view: &WorkspaceView) -> MobileDraftView {
    let project = view.thread.current_project;
    MobileDraftView {
        project_id: project.map(|project| project.id.clone()),
        project_name: project.map(project_name),
        settings: settings_of(view),
        worktree: state.draft_worktree,
        worktree_name: view.thread.draft_worktree_name.clone(),
        worktrees: view.thread.draft_worktree_choices.clone(),
        can_worktree: project.is_some_and(|project| project.workspace_id.is_some()),
    }
}

fn location_of(location: &ThreadLocation) -> MobileLocation {
    match location {
        ThreadLocation::Local => MobileLocation::Local,
        ThreadLocation::Creating => MobileLocation::Creating,
        ThreadLocation::Releasing => MobileLocation::Releasing,
        ThreadLocation::Worktree { worktree, threads } => MobileLocation::Worktree { name: worktree_name(worktree), threads: *threads },
    }
}

fn changes_of(environment: Option<&ChangedFilesResult>) -> Option<MobileChanges> {
    match environment {
        Some(ChangedFilesResult::Available { files, branch, additions, deletions, .. }) => Some(MobileChanges { branch: branch.clone(), files: files.len(), additions: *additions, deletions: *deletions }),
        _ => None,
    }
}

/// Whether a phone may name a thread: this computer's own, and those on a paired computer working under a coordinator here.
pub fn phone_reaches_thread(state: &WorkspaceState, task_id: &str) -> bool {
    state.thread(task_id).is_some() || remote_member_holder(state, task_id).is_some()
}

/// The paired computer holding a thread that works under a coordinator here.
pub fn remote_member_holder<'a>(state: &'a WorkspaceState, task_id: &str) -> Option<&'a PairedComputer> {
    let holder = computer_of_thread(state, Some(task_id))?;
    let lead = holder.state.as_deref()?.thread(task_id)?.remote_lead.as_ref()?.thread_id.clone();
    coordination_members(state, &lead).iter().any(|member| member.computer.is_some_and(|computer| computer.link.id == holder.link.id) && member.thread.id == task_id).then_some(holder)
}

/// Whether a paired computer says when a message it was asked to drop had already gone.
pub fn says_queued_gone(computer: &PairedComputer) -> bool {
    computer.link.capabilities.is_some() && supports_computer_command(computer.link.capabilities.as_deref(), &AppCommand::TaskDropQueued { task_id: None, message_id: String::new(), if_waiting: true })
}

/// Only the user's own words travel to the phone's composer and back.
pub fn phone_edits_queued(message: &QueuedMessage) -> bool {
    let held = &message.held;
    is_users_own(held.origin.as_ref(), held.detail.as_deref(), held.answers.as_deref())
        && !message.steering
        && held.attachments.is_empty()
        && held.files.as_ref().is_none_or(|files| files.is_empty())
        && held.pastes.as_ref().is_none_or(|pastes| pastes.is_empty())
        && held.annotations.as_ref().is_none_or(|annotations| annotations.is_empty())
}

fn family_name(family: ToolFamily) -> &'static str {
    match family {
        ToolFamily::Shell => "shell",
        ToolFamily::Read => "read",
        ToolFamily::Write => "write",
        ToolFamily::Search => "search",
        ToolFamily::Web => "web",
        ToolFamily::Agent => "agent",
        ToolFamily::Thread => "thread",
        ToolFamily::Automation => "automation",
        ToolFamily::Other => "other",
    }
}

fn may_link_images(text: &str) -> bool {
    regex::Regex::new(r"(?i)\.(?:png|jpe?g|gif|webp)\b").is_ok_and(|pattern| pattern.is_match(text))
}

fn mobile_message(message: &ThreadMessage) -> MobileMessage {
    MobileMessage { kind: message.kind, text: message.text.clone(), origin: message.origin.clone(), at: message.at, call: None, images: None, id: None }
}

/// The transcript with what a phone reads beside each message's words.
fn with_details(thread: &Thread, messages: &[ThreadMessage], start: usize) -> Vec<MobileMessage> {
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let mut mobile = mobile_message(message);
            let stored: Option<&ConversationMessage> = thread.messages.get(start + index).map(|message| &**message);
            match stored {
                Some(stored) if message.kind == MessageKind::Tool && stored.kind == MessageKind::Tool => {
                    let described = describe_tool_call(Some(thread.engine), &stored.text, stored.detail.as_deref());
                    mobile.call = Some(MobileToolCall { family: family_name(described.family).into(), label: described.label, argument: described.argument, sigil: described.sigil, verb: described.verb });
                }
                Some(stored) if stored.kind == MessageKind::Assistant && may_link_images(&stored.text) => mobile.id = Some(stored.id.clone()),
                Some(stored) if stored.attachments.as_ref().is_some_and(|items| !items.is_empty()) => mobile.images = stored.attachments.as_ref().map(Vec::len),
                _ => {}
            }
            mobile
        })
        .collect()
}

fn approval_detail(input: &serde_json::Map<String, serde_json::Value>) -> String {
    clip_text(&serde_json::to_string_pretty(input).unwrap_or_default(), MOBILE_APPROVAL_DETAIL_LIMIT)
}

fn project_decisions(open: &Thread, decisions: &[CoordinationDecisionView]) -> Vec<MobileDecision> {
    decisions
        .iter()
        .take(MOBILE_DECISION_LIMIT)
        .map(|item| MobileDecision {
            task_id: item.thread.id.clone(),
            from: (item.thread.id != open.id).then(|| untitled(&item.thread.title)),
            id: item.decision.id.clone(),
            question: item.decision.question.clone(),
            context: item.decision.context.clone().filter(|context| !context.is_empty()),
            options: item.decision.options.iter().map(|option| MobileDecisionOption { label: option.label.clone(), description: option.description.clone().filter(|text| !text.is_empty()), recommended: option.recommended }).collect(),
        })
        .collect()
}

fn project_suggestions(proposals: &[ProposalView]) -> Vec<MobileSuggestion> {
    proposals
        .iter()
        .filter(|view| view.elsewhere.is_none())
        .map(|view| MobileSuggestion {
            id: view.proposal.id.clone(),
            title: view.proposal.title.clone(),
            brief: view.proposal.brief.as_ref().map(|brief| MobileBrief { intent: brief.intent.clone(), done_when: brief.done_when.clone(), delivers: brief.delivers.label().into() }),
            prompt: clip_text(&view.proposal.prompt, MOBILE_SUGGESTION_PROMPT_LIMIT),
            starting: view.starting,
            error: view.proposal.error.clone().filter(|_| !view.starting),
        })
        .collect()
}

fn subagent_status(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Working => "working",
        SubagentStatus::Idle => "idle",
        SubagentStatus::Completed => "completed",
        SubagentStatus::Failed => "failed",
        SubagentStatus::Stopped => "stopped",
    }
}

/// The desktop's Processes and Subagents lists as one, live ones first.
fn project_background(workflows: &[Workflow], processes: &[BackgroundProcess], subagents: &[Subagent]) -> Vec<MobileBackgroundItem> {
    let mut all: Vec<MobileBackgroundItem> = Vec::new();
    for workflow in workflows {
        let running = workflow.status == WorkflowStatus::Running;
        let status = match workflow.status {
            WorkflowStatus::Running => "working",
            WorkflowStatus::Completed => "completed",
            WorkflowStatus::Failed => "failed",
            WorkflowStatus::Stopped => "stopped",
        };
        all.push(MobileBackgroundItem { id: workflow.id.clone(), kind: "workflow".into(), title: workflow.name.clone(), detail: workflow_label(workflow), status: status.into(), live: running, stoppable: running, stopping: workflow.stopping });
    }
    for process in processes {
        let kind = match process.kind {
            BackgroundProcessKind::Shell => "shell",
            BackgroundProcessKind::Monitor => "monitor",
        };
        all.push(MobileBackgroundItem { id: process.id.clone(), kind: kind.into(), title: process.description.clone(), detail: process_label(process).into(), status: "working".into(), live: true, stoppable: true, stopping: process.stopping });
    }
    for subagent in order_subagents(subagents) {
        all.push(MobileBackgroundItem {
            id: subagent.id.clone(),
            kind: "agent".into(),
            title: subagent.description.clone(),
            detail: clip_text(&subagent_line(&subagent), MOBILE_BACKGROUND_DETAIL_LIMIT),
            status: subagent_status(subagent.status).into(),
            live: matches!(subagent.status, SubagentStatus::Working | SubagentStatus::Idle),
            stoppable: subagent.status == SubagentStatus::Working,
            stopping: subagent.stopping,
        });
    }
    let (mut live, rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|item| item.live);
    live.extend(rest);
    live.truncate(MOBILE_BACKGROUND_LIMIT);
    live
}

fn project_members(state: &WorkspaceState, lead_id: &str, view: &WorkspaceView) -> Vec<MobileMember> {
    coordination_members(state, lead_id)
        .into_iter()
        .map(|member| {
            let there = member.holder().map(thread_activity);
            let (working, blocked) = match &there {
                Some(activity) => (&activity.working, &activity.blocked),
                None => (&view.collections.every_busy, &view.collections.every_blocked),
            };
            let status = if blocked.contains(&member.thread.id) {
                MobileRunStatus::AwaitingApproval
            } else if working.contains(&member.thread.id) {
                MobileRunStatus::Running
            } else {
                MobileRunStatus::Idle
            };
            MobileMember {
                id: member.thread.id.clone(),
                title: untitled(&member.thread.title),
                status,
                computer: member.computer.map(|computer| computer.link.name.clone()),
                online: member.computer.map(|computer| computer.link.status == crate::domain::platform::ComputerStatus::Connected),
            }
        })
        .collect()
}

/// The thread the desktop has open, read from the computer holding it while it works under a coordinator here.
fn project_mobile_thread(state: &WorkspaceState, view: &WorkspaceView) -> Option<MobileThreadView> {
    let thread = view.thread.current_thread?;
    let elsewhere = view.shown_computer;
    let shown = view.shown;
    if let Some(elsewhere) = elsewhere {
        if remote_member_holder(state, &thread.id).map(|holder| holder.link.id.as_str()) != Some(elsewhere.link.id.as_str()) {
            return None;
        }
    }
    let transcript = thread_transcript(shown, &thread.id, Some(MOBILE_TRANSCRIPT_MESSAGES), Some(MOBILE_MESSAGE_TEXT_LIMIT), None)?;
    let blocked = match elsewhere {
        Some(_) => thread_activity(shown).blocked,
        None => view.collections.every_blocked.clone(),
    };
    let editable = elsewhere.is_none_or(says_queued_gone);
    let coordination = &view.thread.coordination;
    let member = if view.thread.approval.is_some() { None } else { coordination.approvals.first() };
    let approval = view.thread.approval.or(member.map(|member| member.approval));
    let project = project_for(shown, Some(thread));
    let diff = diff_for(shown, &thread.id);
    let location = location_of(&view.thread.location);
    let moving = !matches!(location, MobileLocation::Creating | MobileLocation::Releasing);
    let queued_held = shown.queued_messages.get(&thread.id).cloned().unwrap_or_default();
    Some(MobileThreadView {
        id: thread.id.clone(),
        loading: thread.history_summary.is_some(),
        title: untitled(&thread.title),
        project_id: project.map(|project| project.id.clone()),
        project_name: view.thread.current_project.map(project_name),
        worktree_id: thread.worktree_id.clone(),
        messages: with_details(thread, &transcript.messages, transcript.omitted),
        omitted: transcript.omitted,
        streaming_tail: view.thread.streaming_tail.map(|tail| tail.text.clone()),
        status: if blocked.contains(&thread.id) {
            MobileRunStatus::AwaitingApproval
        } else {
            match view.thread.status {
                crate::state::ThreadRunStatus::Idle => MobileRunStatus::Idle,
                crate::state::ThreadRunStatus::Running => MobileRunStatus::Running,
                crate::state::ThreadRunStatus::Stopped => MobileRunStatus::Stopped,
            }
        },
        question: view.thread.question.cloned(),
        approval: approval.map(|approval| MobileApproval {
            task_id: approval.task_id.clone(),
            from: member.filter(|member| member.thread.id != thread.id).map(|member| untitled(&member.thread.title)),
            approval_id: approval.approval_id.clone(),
            run_id: approval.run_id.clone(),
            title: approval.title.clone(),
            description: approval.description.clone(),
            tool_name: approval.tool_name.clone(),
            detail: approval_detail(&approval.input),
        }),
        queued: view
            .thread
            .queued_messages
            .iter()
            .map(|message| {
                let held = queued_held.iter().find(|item| item.held.id == message.message.held.id);
                MobileQueuedMessage {
                    id: message.message.held.id.clone(),
                    text: message.message.held.text.clone(),
                    origin: crate::domain::message_origin::message_label(message.message.held.origin.as_ref(), message.message.held.detail.as_deref()),
                    steering: message.message.steering.then_some(true),
                    editable: editable && held.is_some_and(phone_edits_queued) && !draining_queued(shown, &thread.id, &message.message.held.id),
                }
            })
            .collect(),
        settings: settings_of(view),
        location,
        worktrees: match (project, elsewhere) {
            (Some(project), None) => worktree_choices(state, &project.id, thread.worktree_id.as_deref()),
            _ => Vec::new(),
        },
        can_move: elsewhere.is_none() && project.is_some_and(|project| project.workspace_id.is_some()) && moving && !view.collections.every_busy.contains(&thread.id),
        changes: changes_of(view.thread.environment),
        reviewable: elsewhere.is_none() && thread_workspace_id(state, Some(thread)).is_some(),
        branch_range: match &diff.range {
            DiffRange::Branches { .. } => diff.range.clone(),
            _ => diff.branch_range.clone().unwrap_or_else(default_branch_range),
        },
        members: if is_coordinator(Some(thread)) { project_members(state, &thread.id, view) } else { Vec::new() },
        role: thread.role,
        background: project_background(view.thread.workflows, view.thread.background_processes, view.thread.subagents),
        decisions: project_decisions(thread, &coordination.decisions),
        suggestions: project_suggestions(&coordination.proposals),
        limit: view.thread.limit_pause.as_ref().map(|limit| MobileLimitPause { resets_at: limit.pause.resets_at, window: limit.pause.window, position: limit.position }),
        computer: elsewhere.map(|computer| computer.link.name.clone()),
        stoppable: view.collections.every_stoppable.contains(&thread.id),
    })
}

/// Every archived thread as the phone's Archived list reads it, newest first.
pub fn mobile_archived_threads(state: &WorkspaceState) -> Vec<MobileArchivedThread> {
    crate::workspace::activity::thread_lists(state)
        .archived_threads
        .into_iter()
        .map(|thread| {
            let archived_at = thread.archived_at.unwrap_or(0);
            MobileArchivedThread { id: thread.id.clone(), title: untitled(&thread.title), project_name: project_for(state, Some(thread)).map(project_name), archived_at, deletes_at: archived_at + ARCHIVE_RETENTION_MS }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileThreadRead {
    pub title: String,
    pub status: MobileRunStatus,
    pub messages: Vec<MobileMessage>,
    pub omitted: usize,
    pub streaming_tail: Option<String>,
}

/// A thread on this computer as its own screen reads it, without opening it.
pub fn mobile_thread_read(state: &WorkspaceState, task_id: &str) -> Option<MobileThreadRead> {
    let thread = state.thread(task_id)?;
    let transcript = thread_transcript(state, &thread.id, Some(MOBILE_TRANSCRIPT_MESSAGES), Some(MOBILE_MESSAGE_TEXT_LIMIT), None)?;
    let activity = thread_activity(state);
    let status = if activity.blocked.contains(&thread.id) {
        MobileRunStatus::AwaitingApproval
    } else if activity.working.contains(&thread.id) {
        MobileRunStatus::Running
    } else {
        MobileRunStatus::Idle
    };
    let omitted = match &thread.history_summary {
        Some(summary) => transcript.omitted.max((summary.message_count as usize).saturating_sub(transcript.messages.len())),
        None => transcript.omitted,
    };
    Some(MobileThreadRead { title: untitled(&thread.title), status, messages: with_details(thread, &transcript.messages, transcript.omitted), omitted, streaming_tail: state.streaming_tails.get(&thread.id).map(|tail| tail.text.clone()) })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileThreadHistory {
    pub start: usize,
    pub messages: Vec<MobileMessage>,
}

/// The page of a thread's messages before index `before`.
pub fn mobile_thread_history(state: &WorkspaceState, task_id: &str, before: usize) -> Option<MobileThreadHistory> {
    let thread = state.thread(task_id)?;
    let transcript = thread_transcript(state, &thread.id, Some(MOBILE_HISTORY_PAGE), Some(MOBILE_MESSAGE_TEXT_LIMIT), Some(before))?;
    Some(MobileThreadHistory { start: transcript.omitted, messages: with_details(thread, &transcript.messages, transcript.omitted) })
}

// Patches.

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileSplice {
    pub dropped: usize,
    pub kept: usize,
    pub messages: Vec<MobileMessage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileTail {
    pub from: usize,
    pub text: String,
}

/// What moved on the open thread: changed fields as they stand, and how its transcript and tail grew.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileThreadDelta {
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spliced: Option<MobileSplice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<MobileMessage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<MobileTail>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MobileThreadPatch {
    Closed,
    Opened { thread: Box<MobileThreadView> },
    Changed { id: String, delta: MobileThreadDelta },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MobilePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<MobileProjectGroup>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<MobileActivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<MobileTheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engines: Option<MobileEngines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upkeep: Option<Vec<MobileUpkeepItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dictation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<MobileUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<MobileThreadPatch>,
    /// Present only when it moved, and `Some(None)` when a thread took its place.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nullable")]
    pub draft: Option<Option<MobileDraftView>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nullable")]
    pub error: Option<Option<String>>,
}

mod nullable {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(value: &Option<Option<T>>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}

fn same<T: Serialize>(left: &T, right: &T) -> bool {
    serde_json::to_value(left).ok() == serde_json::to_value(right).ok()
}

fn same_rows(previous: &[MobileThreadEntry], next: &[MobileThreadEntry]) -> bool {
    previous.len() == next.len()
        && previous.iter().zip(next).all(|(before, after)| (before.last_activity_at - after.last_activity_at).abs() < ACTIVITY_DRIFT_MS && MobileThreadEntry { last_activity_at: 0, ..before.clone() } == MobileThreadEntry { last_activity_at: 0, ..after.clone() })
}

fn same_groups(previous: &[MobileProjectGroup], next: &[MobileProjectGroup]) -> bool {
    previous.len() == next.len() && previous.iter().zip(next).all(|(group, other)| group.project_id == other.project_id && group.name == other.name && same_rows(&group.threads, &other.threads))
}

fn same_activity(previous: &MobileActivity, next: &MobileActivity) -> bool {
    same_rows(&previous.priority, &next.priority) && same_rows(&previous.running, &next.running) && same_rows(&previous.threads, &next.threads) && same_rows(&previous.snoozed, &next.snoozed)
}

const MOVING_KEYS: [&str; 25] = [
    "loading", "title", "projectId", "projectName", "worktreeId", "omitted", "status", "question", "approval", "queued", "settings", "location", "worktrees", "canMove", "changes", "reviewable", "branchRange", "members", "role", "background",
    "decisions", "suggestions", "limit", "computer", "stoppable",
];

fn transcript_delta(previous: &[MobileMessage], next: &[MobileMessage], delta: &mut MobileThreadDelta) {
    let held: Vec<String> = previous.iter().map(|message| serde_json::to_string(message).unwrap_or_default()).collect();
    let wanted: Vec<String> = next.iter().map(|message| serde_json::to_string(message).unwrap_or_default()).collect();
    if held.is_empty() {
        if !wanted.is_empty() {
            delta.spliced = Some(MobileSplice { dropped: 0, kept: 0, messages: next.to_vec() });
        }
        return;
    }
    let Some(dropped) = wanted.first().and_then(|first| held.iter().position(|item| item == first)) else {
        delta.messages = Some(next.to_vec());
        return;
    };
    let mut kept = 0;
    while dropped + kept < held.len() && kept < wanted.len() && held[dropped + kept] == wanted[kept] {
        kept += 1;
    }
    let rest = held.len() - dropped;
    if kept + 1 < rest {
        delta.messages = Some(next.to_vec());
        return;
    }
    if dropped == 0 && kept == rest && kept == wanted.len() {
        return;
    }
    delta.spliced = Some(MobileSplice { dropped, kept, messages: next[kept..].to_vec() });
}

fn diff_mobile_thread(previous: Option<&MobileThreadView>, next: Option<&MobileThreadView>) -> Option<MobileThreadPatch> {
    let Some(next) = next else { return previous.map(|_| MobileThreadPatch::Closed) };
    let previous = match previous {
        Some(previous) if previous.id == next.id => previous,
        _ => return Some(MobileThreadPatch::Opened { thread: Box::new(next.clone()) }),
    };
    let before = serde_json::to_value(previous).ok();
    let after = serde_json::to_value(next).ok();
    let mut delta = MobileThreadDelta::default();
    if let (Some(serde_json::Value::Object(before)), Some(serde_json::Value::Object(after))) = (before, after) {
        for key in MOVING_KEYS {
            let was = before.get(key).cloned().unwrap_or(serde_json::Value::Null);
            let is = after.get(key).cloned().unwrap_or(serde_json::Value::Null);
            if was != is {
                delta.fields.insert(key.into(), is);
            }
        }
    }
    if previous.streaming_tail != next.streaming_tail {
        match (&previous.streaming_tail, &next.streaming_tail) {
            (Some(tail), Some(grown)) if grown.starts_with(tail.as_str()) => delta.tail = Some(MobileTail { from: crate::util::js_len(tail), text: grown[tail.len()..].to_string() }),
            _ => {
                delta.fields.insert("streamingTail".into(), serde_json::to_value(&next.streaming_tail).unwrap_or_default());
            }
        }
    }
    transcript_delta(&previous.messages, &next.messages, &mut delta);
    let empty = delta.fields.is_empty() && delta.spliced.is_none() && delta.messages.is_none() && delta.tail.is_none();
    (!empty).then(|| MobileThreadPatch::Changed { id: next.id.clone(), delta })
}

/// None when nothing moved, so a server with nothing to say sends nothing at all.
pub fn diff_mobile_view(previous: &MobileView, next: &MobileView) -> Option<MobilePatch> {
    let mut patch = MobilePatch::default();
    if !same_groups(&previous.groups, &next.groups) {
        patch.groups = Some(next.groups.clone());
    }
    if !same_activity(&previous.activity, &next.activity) {
        patch.activity = Some(next.activity.clone());
    }
    if !same(&previous.theme, &next.theme) {
        patch.theme = Some(next.theme.clone());
    }
    if !same(&previous.engines, &next.engines) {
        patch.engines = Some(next.engines.clone());
    }
    if !same(&previous.upkeep, &next.upkeep) {
        patch.upkeep = Some(next.upkeep.clone());
    }
    if previous.dictation != next.dictation {
        patch.dictation = Some(next.dictation);
    }
    if !same(&previous.usage, &next.usage) {
        patch.usage = Some(next.usage.clone());
    }
    if previous.archived != next.archived {
        patch.archived = Some(next.archived);
    }
    patch.thread = diff_mobile_thread(previous.thread.as_ref(), next.thread.as_ref());
    if !same(&previous.draft, &next.draft) {
        patch.draft = Some(next.draft.clone());
    }
    if previous.error != next.error {
        patch.error = Some(next.error.clone());
    }
    (patch != MobilePatch::default()).then_some(patch)
}

/// Puts a patch back on the view the phone holds, or None when it does not fit what is held.
pub fn apply_mobile_patch(view: &MobileView, patch: &MobilePatch) -> Option<MobileView> {
    let mut next = MobileView {
        groups: patch.groups.clone().unwrap_or_else(|| view.groups.clone()),
        activity: patch.activity.clone().unwrap_or_else(|| view.activity.clone()),
        theme: patch.theme.clone().unwrap_or_else(|| view.theme.clone()),
        engines: patch.engines.clone().unwrap_or_else(|| view.engines.clone()),
        upkeep: patch.upkeep.clone().unwrap_or_else(|| view.upkeep.clone()),
        dictation: patch.dictation.unwrap_or(view.dictation),
        usage: patch.usage.clone().unwrap_or_else(|| view.usage.clone()),
        archived: patch.archived.unwrap_or(view.archived),
        draft: patch.draft.clone().unwrap_or_else(|| view.draft.clone()),
        error: patch.error.clone().unwrap_or_else(|| view.error.clone()),
        thread: view.thread.clone(),
    };
    match &patch.thread {
        None => {}
        Some(MobileThreadPatch::Closed) => next.thread = None,
        Some(MobileThreadPatch::Opened { thread }) => next.thread = Some((**thread).clone()),
        Some(MobileThreadPatch::Changed { id, delta }) => {
            let Some(current) = view.thread.as_ref().filter(|current| &current.id == id) else { return Some(next) };
            let mut transcript = delta.messages.clone().unwrap_or_else(|| current.messages.clone());
            if let Some(spliced) = &delta.spliced {
                if current.messages.len() < spliced.dropped + spliced.kept {
                    return None;
                }
                transcript = current.messages[spliced.dropped..spliced.dropped + spliced.kept].to_vec();
                transcript.extend(spliced.messages.iter().cloned());
            }
            let mut value = serde_json::to_value(current).ok()?;
            let fields = value.as_object_mut()?;
            for (key, moved) in &delta.fields {
                fields.insert(key.clone(), moved.clone());
            }
            let mut thread: MobileThreadView = serde_json::from_value(value).ok()?;
            if let Some(tail) = &delta.tail {
                let held = current.streaming_tail.clone().unwrap_or_default();
                if current.streaming_tail.is_none() || crate::util::js_len(&held) != tail.from {
                    return None;
                }
                thread.streaming_tail = Some(format!("{held}{}", tail.text));
            }
            thread.messages = transcript;
            next.thread = Some(thread);
        }
    }
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> MobileMessage {
        MobileMessage { kind: MessageKind::Assistant, text: text.into(), origin: None, at: 1, call: None, images: None, id: None }
    }

    fn view_with(messages: Vec<MobileMessage>, tail: Option<&str>) -> MobileView {
        let mut view = empty_mobile_view();
        view.thread = Some(MobileThreadView {
            loading: false,
            id: "t".into(),
            title: "T".into(),
            project_id: None,
            project_name: None,
            worktree_id: None,
            messages,
            omitted: 0,
            streaming_tail: tail.map(String::from),
            status: MobileRunStatus::Running,
            approval: None,
            question: None,
            queued: Vec::new(),
            settings: MobileThreadSettings { engine: AgentEngine::Claude, model: None, effort: AgentEffort::High, fast_mode: None, policy: ExecutionPolicy::Confirm, coordinating: false },
            location: MobileLocation::Local,
            worktrees: Vec::new(),
            can_move: false,
            changes: None,
            reviewable: false,
            members: Vec::new(),
            role: None,
            background: Vec::new(),
            branch_range: default_branch_range(),
            decisions: Vec::new(),
            suggestions: Vec::new(),
            limit: None,
            computer: None,
            stoppable: false,
        });
        view
    }

    #[test]
    fn a_transcript_that_grows_at_its_end_travels_as_what_it_grew_by() {
        let before = view_with(vec![message("a"), message("b")], Some("Wor"));
        let after = view_with(vec![message("b"), message("c")], Some("Working"));
        let patch = diff_mobile_view(&before, &after).expect("something moved");
        let Some(MobileThreadPatch::Changed { delta, .. }) = &patch.thread else { panic!("{patch:?}") };
        assert_eq!(delta.spliced, Some(MobileSplice { dropped: 1, kept: 1, messages: vec![message("c")] }));
        assert_eq!(delta.tail, Some(MobileTail { from: 3, text: "king".into() }));
        assert_eq!(apply_mobile_patch(&before, &patch), Some(after.clone()));
        assert!(diff_mobile_view(&after, &after).is_none());
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use crate::domain::engine::AgentEngine;
    use crate::domain::project::Project;

    #[test]
    fn every_open_project_is_a_group_and_recents_comes_last() {
        let mut state = WorkspaceState::default();
        state.projects.push(Project { id: "p".into(), root: "/repo".into(), name: Some("Repo".into()), workspace_id: None, sort_index: None, unlisted: false });
        let mut inside = Thread::new("a", "Inside", AgentEngine::Claude, ExecutionPolicy::Confirm, 1);
        inside.project_id = Some("p".into());
        state.threads.push(inside.into());
        state.threads.push(Thread::new("b", "", AgentEngine::Claude, ExecutionPolicy::Confirm, 1).into());
        let view = project_mobile_view(&state, 10);
        let groups: Vec<(Option<&str>, &str, Vec<&str>)> = view.groups.iter().map(|group| (group.project_id.as_deref(), group.name.as_str(), group.threads.iter().map(|entry| entry.title.as_str()).collect())).collect();
        assert_eq!(groups, vec![(Some("p"), "Repo", vec!["Inside"]), (None, "Recents", vec!["Untitled thread"])]);
        assert!(view.thread.is_none());
        assert!(view.draft.is_some(), "with no thread open the draft stands in its place");
    }
}
