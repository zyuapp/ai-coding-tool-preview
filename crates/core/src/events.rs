//! Things that happened: what the agent process reports, replies to effects, and pushes from the host.
//! Each comes back to the reducer as a `WorkspaceEvent`.

use serde::{Deserialize, Serialize};

use crate::commands::AppCommand;
use crate::contracts::preferences::ViewPreferences;
use crate::domain::automation::AutomationView;
use crate::domain::conversation::{Annotation, ConversationMessage, PastedText, RunAttachment};
use crate::domain::coordination::{CoordinationState, DecisionRequest, MemoryChange, ThreadProposal};
use crate::domain::diff::{DiffFileSummary, DiffRange};
use crate::domain::engine::{AgentEngine, EngineStatus};
use crate::domain::find::{FindResults, FindTarget};
use crate::domain::mobile::MobileServerState;
use crate::domain::platform::{
    AppBuild, AppUpdate, CliStatus, ComputerLink, ComputerUsePermissions, DictationModel, DiscoveredComputer, InstalledApp, PlanUsage, SimulatorAvailability, SimulatorDevice, SimulatorOrientation, WorkspaceRecord,
};
use crate::domain::pull_request::PullRequestAnswer;
use crate::domain::question::QuestionRequest;
use crate::domain::run::{ActiveGoal, BackgroundProcess, Continuation, ExecutionPolicy, RetryNotice, RunStatus, SubagentActivity, SubagentReport, ToolIntent};
use crate::domain::sidebar::SettingsSection;
use crate::domain::terminal::{BrowserImportResult, BrowserImportSite, BrowserImportSource, TerminalUpdate};
use crate::domain::usage_limit::UsageLimit;
use crate::domain::workflow::{WorkflowAgent, WorkflowPhase, WorkflowStatus};
use crate::domain::worktree::{CreatedWorktree, ManagedWorktree};
use crate::persisted::ThreadStoreData;
use crate::state::WorkspaceState;

/// A run's report, numbered so a stale or superseded one can be dropped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    pub task_id: String,
    pub run_id: String,
    pub sequence: i64,
    #[serde(flatten)]
    pub report: RunReport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionTrigger {
    Manual,
    Auto,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum RunReport {
    #[serde(rename = "question.requested")]
    QuestionRequested { request_id: String, request: QuestionRequest },
    #[serde(rename = "question.answered")]
    QuestionAnswered { request_id: String, question_id: String, text: String },
    #[serde(rename = "question.closed")]
    QuestionClosed { request_id: String },
    #[serde(rename = "run.started")]
    RunStarted {
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        agent_initiated: bool,
    },
    #[serde(rename = "run.status")]
    RunStatus {
        status: RunStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<UsageLimit>,
    },
    #[serde(rename = "assistant.delta")]
    AssistantDelta {
        message_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        append: Option<bool>,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        artifact: bool,
    },
    #[serde(rename = "assistant.tail")]
    AssistantTail { message_id: String, text: String },
    #[serde(rename = "context.usage")]
    ContextUsage { tokens: i64, limit: i64, model: String },
    #[serde(rename = "context.spent")]
    ContextSpent { tokens: i64 },
    #[serde(rename = "context.compaction-status")]
    CompactionStatus {
        compacting: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "run.retrying")]
    Retrying {
        #[serde(flatten)]
        notice: RetryNotice,
    },
    #[serde(rename = "context.compacted")]
    Compacted {
        trigger: CompactionTrigger,
        pre_tokens: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        post_tokens: Option<i64>,
    },
    #[serde(rename = "tool.intent")]
    ToolIntent { intent: ToolIntent },
    #[serde(rename = "computer-use.setup-required")]
    ComputerUseSetupRequired,
    #[serde(rename = "approval.requested")]
    ApprovalRequested { approval_id: String, intent: ToolIntent, title: String, description: String },
    #[serde(rename = "continuation.updated")]
    ContinuationUpdated { continuation: Continuation },
    #[serde(rename = "continuation.lost")]
    ContinuationLost,
    #[serde(rename = "queued.delivered")]
    QueuedDelivered { message_id: String },
    #[serde(rename = "queued.steer-failed")]
    QueuedSteerFailed { message_id: String, message: String },
}

/// What the agent process reports about work that answers to the thread rather than a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ThreadReport {
    #[serde(rename = "workflow.started")]
    WorkflowStarted { id: String, name: String, description: String },
    #[serde(rename = "workflow.progress")]
    WorkflowProgress { id: String, phases: Vec<WorkflowPhase>, agents: Vec<WorkflowAgent>, total_tokens: i64, total_tool_calls: i64 },
    #[serde(rename = "workflow.finished")]
    WorkflowFinished { id: String, status: WorkflowStatus, summary: String },
    #[serde(rename = "background.changed")]
    BackgroundChanged { processes: Vec<BackgroundProcess> },
    #[serde(rename = "goal.changed")]
    GoalChanged { goal: Option<ActiveGoal> },
    #[serde(rename = "resource.stopped")]
    ResourceStopped { stop: crate::domain::session_resources::ResourceStop },
    #[serde(untagged)]
    Subagent(SubagentReport),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadEvent {
    pub task_id: String,
    #[serde(flatten)]
    pub report: ThreadReport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsReloadStatus {
    Pending,
    Reloaded,
    Failed,
}

/// Everything the agent process pushes back, on one channel so a workflow cannot overtake its own run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentEvent {
    Run(RunEvent),
    SettingsReload(SettingsReloadEvent),
    Thread(ThreadEvent),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "engine.settings-reload-status")]
pub struct SettingsReloadEvent {
    pub status: SettingsReloadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnavailableReason {
    Missing,
    NotDirectory,
    Inaccessible,
    Changed,
}

/// A comparison's file list. Patches are fetched one at a time and never travel with the list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum DiffSummaryResult {
    Available { range: DiffRange, ignore_whitespace: bool, files: Vec<DiffFileSummary>, additions: i64, deletions: i64 },
    Unavailable { reason: UnavailableReason },
    Unknown { workspace_id: String },
    Error { message: String },
}

/// One file's patch, as Git wrote it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum DiffPatchResult {
    Available { patch: String },
    TooLarge { limit: i64 },
    Error { message: String },
}

/// What Git last said about a checkout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum ChangedFilesResult {
    Available { files: Vec<String>, branch: Option<String>, baseline: Option<String>, additions: i64, deletions: i64 },
    Unavailable { reason: UnavailableReason },
    Unknown { workspace_id: String },
    Error { message: String },
}

/// A desktop-wide binding whose action this session cannot perform on the active platform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DesktopShortcutUnavailable {
    pub binding: String,
    pub reason: String,
    pub message: String,
}

/// What a thread wants in front of the user.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadNotice {
    pub task_id: String,
    pub title: String,
    pub headline: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationRequest {
    pub task_id: String,
    pub url: String,
}

/// What a page did, pushed from the host so the reducer stays the only writer of the tab record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserPageEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation_request: Option<NavigationRequest>,
    pub tab_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loading: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_go_back: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_go_forward: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The scheduler owns the run ID so it can correlate the run back to the tick that asked for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationFire {
    pub automation_id: String,
    pub task_id: String,
    pub run_id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ExecutionPolicy>,
    pub run_number: i64,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub quiet: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub unattended: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_when: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationAck {
    pub automation_id: String,
    pub run_id: String,
    pub started: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSnapshotResult {
    pub commit: Option<String>,
    pub short_commit: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachmentStrip {
    pub key: String,
    pub ids: Vec<String>,
}

/// A send's draft, carried to a paired computer, which this window lets go of once taken.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SentDraft {
    pub key: String,
    pub prompt: String,
    pub pastes: Vec<PastedText>,
    pub annotations: Vec<Annotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<AttachmentStrip>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum WorkspaceEvent {
    #[serde(rename = "snoozes.elapsed")]
    SnoozesElapsed { at: i64 },
    #[serde(rename = "limits.elapsed")]
    LimitsElapsed { at: i64 },
    #[serde(rename = "view.closed")]
    ViewClosed,
    #[serde(rename = "task.result-collected")]
    TaskResultCollected { task_id: String },
    #[serde(rename = "store.loaded")]
    StoreLoaded {
        data: Box<ThreadStoreData>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hidden_tasks: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resuming: Option<Vec<String>>,
    },
    #[serde(rename = "store.thread-loaded")]
    StoreThreadLoaded { task_id: String, messages: Vec<crate::shared::Shared<ConversationMessage>> },
    #[serde(rename = "store.absent")]
    StoreAbsent,
    #[serde(rename = "store.persisted")]
    StorePersisted,
    #[serde(rename = "preferences.loaded")]
    PreferencesLoaded { preferences: Box<ViewPreferences> },
    #[serde(rename = "store.failed")]
    StoreFailed { message: String },
    #[serde(rename = "action.failed")]
    ActionFailed {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page: Option<SettingsSection>,
    },

    // Adding projects.
    #[serde(rename = "project.added")]
    ProjectAdded {
        workspace: WorkspaceRecord,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<i64>,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        start: bool,
    },
    #[serde(rename = "project.add-finished")]
    ProjectAddFinished {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "project.directories")]
    ProjectDirectories {
        request: i64,
        directories: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "project.path-picked")]
    ProjectPathPicked {
        request: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "project.opened")]
    ProjectOpened {
        workspace: WorkspaceRecord,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        unlisted: bool,
    },
    #[serde(rename = "project.registered")]
    ProjectRegistered { project_id: String, workspace: WorkspaceRecord },
    #[serde(rename = "project.register-failed")]
    ProjectRegisterFailed { project_id: String, message: String },

    // Agent reports.
    #[serde(rename = "run.event")]
    RunEvent { event: RunEvent },
    #[serde(rename = "thread.event")]
    ThreadEvent { event: ThreadEvent },
    #[serde(rename = "agent.events")]
    AgentEvents { events: Vec<AgentEvent> },
    #[serde(rename = "engine.settings-reload-status")]
    SettingsReloadStatus {
        status: SettingsReloadStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    #[serde(rename = "run.resolved")]
    RunResolved {
        pending_id: String,
        workspace: WorkspaceRecord,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree: Option<CreatedWorktree>,
    },
    #[serde(rename = "run.unresolved")]
    RunUnresolved { pending_id: String, message: String },
    #[serde(rename = "title.suggested")]
    TitleSuggested { task_id: String, title: String },
    #[serde(rename = "coordination.reported")]
    CoordinationReported { task_id: String, state: CoordinationState, summary: String },
    #[serde(rename = "coordination.decision-raised")]
    CoordinationDecisionRaised {
        task_id: String,
        request: DecisionRequest,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision_id: Option<String>,
    },
    #[serde(rename = "coordination.remembered")]
    CoordinationRemembered { task_id: String, change: MemoryChange },
    #[serde(rename = "coordination.proposed")]
    CoordinationProposed { task_id: String, proposals: Vec<ThreadProposal> },
    #[serde(rename = "proposal.settled")]
    ProposalSettled {
        task_id: String,
        proposal_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        refused: bool,
    },
    #[serde(rename = "subagent.activity.loaded")]
    SubagentActivityLoaded { task_id: String, subagent_id: String, activity: Vec<SubagentActivity> },

    // Phone bridge and paired computers.
    #[serde(rename = "remote.changed")]
    RemoteChanged { remote: Box<MobileServerState> },
    #[serde(rename = "computers.changed")]
    ComputersChanged { name: String, links: Vec<ComputerLink> },
    #[serde(rename = "computers.found")]
    ComputersFound { found: Vec<DiscoveredComputer> },
    #[serde(rename = "computers.search-failed")]
    ComputersSearchFailed { message: String },
    #[serde(rename = "computers.pair-failed")]
    ComputersPairFailed { message: String },
    #[serde(rename = "computer.state")]
    ComputerState { id: String, state: Box<WorkspaceState> },
    #[serde(rename = "computer.notice")]
    ComputerNotice { id: String, notice: ThreadNotice },
    #[serde(rename = "computers.forwarded")]
    ComputersForwarded { draft: SentDraft },
    #[serde(rename = "computers.answered")]
    ComputersAnswered { task_id: String },

    // Engines.
    #[serde(rename = "engine.status")]
    EngineStatus { status: EngineStatus },
    #[serde(rename = "engine.failed")]
    EngineFailed { message: String },
    #[serde(rename = "engine.updated")]
    EngineUpdated { engine: AgentEngine, status: EngineStatus },
    #[serde(rename = "engine.update-failed")]
    EngineUpdateFailed { engine: AgentEngine, message: String },

    // Automations.
    #[serde(rename = "automation.fired")]
    AutomationFired { fire: AutomationFire },
    #[serde(rename = "automations.changed")]
    AutomationsChanged { automations: Vec<AutomationView> },

    // Worktrees.
    #[serde(rename = "worktree.created")]
    WorktreeCreated {
        task_id: String,
        worktree: CreatedWorktree,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        r#move: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<String>,
    },
    #[serde(rename = "worktree.failed")]
    WorktreeFailed { task_id: String, message: String },
    #[serde(rename = "worktrees.loaded")]
    WorktreesLoaded { worktrees: Vec<ManagedWorktree> },
    #[serde(rename = "worktrees.failed")]
    WorktreesFailed {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<String>,
    },
    #[serde(rename = "worktree.released")]
    WorktreeReleased { task_id: String, snapshot: WorktreeSnapshotResult },
    #[serde(rename = "worktree.release-failed")]
    WorktreeReleaseFailed { task_id: String, message: String },
    #[serde(rename = "checkout.finished")]
    CheckoutFinished {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    #[serde(rename = "worktree.deleted")]
    WorktreeDeleted {
        worktree_id: String,
        root: String,
        snapshot: WorktreeSnapshotResult,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        missing_only: Option<bool>,
    },

    // Diffs and pull requests.
    #[serde(rename = "environment.updated")]
    EnvironmentUpdated {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        result: ChangedFilesResult,
    },
    #[serde(rename = "diff.loaded")]
    DiffLoaded { owner: String, workspace_id: String, range: DiffRange, result: DiffSummaryResult },
    #[serde(rename = "pull-request.answered")]
    PullRequestAnswered {
        workspace_id: String,
        branch: Option<String>,
        read: i64,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        members: bool,
        answer: PullRequestAnswer,
    },

    // Browser.
    #[serde(rename = "browser.updated")]
    BrowserUpdated { page: BrowserPageEvent },
    #[serde(rename = "browser-import.sources")]
    BrowserImportSources { sources: Vec<BrowserImportSource> },
    #[serde(rename = "browser-import.sites")]
    BrowserImportSites { source_id: String, sites: Vec<BrowserImportSite> },
    #[serde(rename = "browser-import.done")]
    BrowserImportDone { source_id: String, sites: Vec<String>, result: BrowserImportResult },
    #[serde(rename = "browser-import.failed")]
    BrowserImportFailed { message: String },
    #[serde(rename = "find.results")]
    FindResults { target: FindTarget, results: FindResults },

    // Simulator.
    #[serde(rename = "simulator.listed")]
    SimulatorListed { availability: SimulatorAvailability, devices: Vec<SimulatorDevice> },
    #[serde(rename = "simulator.booted")]
    SimulatorBooted { udid: String },
    #[serde(rename = "simulator.failed")]
    SimulatorFailed { udid: String, message: String },
    #[serde(rename = "simulator.refused")]
    SimulatorRefused { message: String },
    #[serde(rename = "simulator.turned")]
    SimulatorTurned { udid: String, orientation: SimulatorOrientation },
    #[serde(rename = "simulator.idle")]
    SimulatorIdle { seq: i64 },

    // Dictation.
    #[serde(rename = "dictation.model")]
    DictationModel {
        model: DictationModel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<f64>,
    },
    #[serde(rename = "dictation.started")]
    DictationStarted { id: String },
    #[serde(rename = "dictation.transcribed")]
    DictationTranscribed { id: String, text: String },
    #[serde(rename = "dictation.failed")]
    DictationFailed { id: String, message: String },

    // Terminal.
    #[serde(rename = "terminal.updated")]
    TerminalUpdated { update: TerminalUpdate },

    // Composer images.
    #[serde(rename = "attachments.saved")]
    AttachmentsSaved {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        steer: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        ids: Vec<String>,
        attachments: Vec<RunAttachment>,
    },
    #[serde(rename = "attachments.failed")]
    AttachmentsFailed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        message: String,
    },

    // Desktop.
    #[serde(rename = "apps.listed")]
    AppsListed { apps: Vec<InstalledApp> },

    // App updates.
    #[serde(rename = "app.update-changed")]
    AppUpdateChanged { update: AppUpdate },
    #[serde(rename = "app.build")]
    AppBuild { build: Option<AppBuild> },
    #[serde(rename = "app.launched")]
    AppLaunched { version: String },

    // Computer use, plan usage, the terminal command.
    #[serde(rename = "computer-use.permissions")]
    ComputerUsePermissions {
        permissions: ComputerUsePermissions,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        enabling: bool,
    },
    #[serde(rename = "computer-use.failed")]
    ComputerUseFailed {
        message: String,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        enabling: bool,
    },
    #[serde(rename = "usage.reported")]
    UsageReported { engine: AgentEngine, read: i64, usage: PlanUsage },
    #[serde(rename = "cli.read-status")]
    CliReadStatus { status: CliStatus },
    #[serde(rename = "cli.failed")]
    CliFailed { message: String },

    // Shortcuts.
    #[serde(rename = "shortcut.captured")]
    ShortcutCaptured { binding: Option<String> },
    #[serde(rename = "shortcut.unavailable")]
    ShortcutUnavailable { refusal: DesktopShortcutUnavailable },
}

/// Whatever the reducer takes: a command from a person or an agent, or a report of what happened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkspaceInput {
    Command(AppCommand),
    Event(WorkspaceEvent),
}

impl From<AppCommand> for WorkspaceInput {
    fn from(command: AppCommand) -> Self {
        WorkspaceInput::Command(command)
    }
}

impl From<WorkspaceEvent> for WorkspaceInput {
    fn from(event: WorkspaceEvent) -> Self {
        WorkspaceInput::Event(event)
    }
}

/// What a window reports besides its commands: the only events a view sends.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewEvent {
    /// What a page or a shell found, counted by whoever holds the text.
    FindResults { target: FindTarget, results: FindResults },
    /// The keystroke settings were waiting for, or None when Escape gave up.
    ShortcutCaptured { binding: Option<String> },
}

impl From<ViewEvent> for WorkspaceInput {
    fn from(event: ViewEvent) -> Self {
        WorkspaceInput::Event(match event {
            ViewEvent::FindResults { target, results } => WorkspaceEvent::FindResults { target, results },
            ViewEvent::ShortcutCaptured { binding } => WorkspaceEvent::ShortcutCaptured { binding },
        })
    }
}

impl WorkspaceInput {
    /// The wire name of the input, as its `type` field carries it.
    pub fn type_name(&self) -> String {
        serde_json::to_value(self).ok().and_then(|value| value.get("type").and_then(|kind| kind.as_str()).map(String::from)).unwrap_or_default()
    }
}

/// What a thread's own report, decision or memory change came to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CoordinationAnswer {
    Recorded {
        recorded: bool,
        #[serde(rename = "noteId", default, skip_serializing_if = "Option::is_none")]
        note_id: Option<String>,
    },
    Refused { recorded: bool, reason: String },
}

impl CoordinationAnswer {
    pub fn recorded(note_id: Option<String>) -> Self {
        CoordinationAnswer::Recorded { recorded: true, note_id }
    }

    pub fn refused(reason: impl Into<String>) -> Self {
        CoordinationAnswer::Refused { recorded: false, reason: reason.into() }
    }
}

/// What an input came to for whoever sent it: done, naming the thread it acted on, or why not.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandResult {
    Ok {
        ok: bool,
        #[serde(rename = "taskId", default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<CoordinationAnswer>,
    },
    Failed { ok: bool, message: String },
}

impl CommandResult {
    pub fn ok() -> Self {
        CommandResult::Ok { ok: true, task_id: None, answer: None }
    }

    pub fn ok_with_task(task_id: impl Into<String>) -> Self {
        CommandResult::Ok { ok: true, task_id: Some(task_id.into()), answer: None }
    }

    pub fn answered(answer: CoordinationAnswer) -> Self {
        CommandResult::Ok { ok: true, task_id: None, answer: Some(answer) }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        CommandResult::Failed { ok: false, message: message.into() }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, CommandResult::Ok { .. })
    }

    pub fn message(&self) -> Option<&str> {
        match self {
            CommandResult::Failed { message, .. } => Some(message),
            _ => None,
        }
    }
}

/// An agent event as the reducer takes it: a run's own channel, or the thread's.
pub fn agent_event_input(event: AgentEvent) -> WorkspaceInput {
    match event {
        AgentEvent::Run(event) => WorkspaceEvent::RunEvent { event }.into(),
        AgentEvent::Thread(event) => WorkspaceEvent::ThreadEvent { event }.into(),
        AgentEvent::SettingsReload(event) => WorkspaceEvent::SettingsReloadStatus { status: event.status, message: event.message }.into(),
    }
}

/// An input that is the user's own command as opposed to a report.
pub fn command_of(input: &WorkspaceInput) -> Option<&AppCommand> {
    match input {
        WorkspaceInput::Command(command) => Some(command),
        WorkspaceInput::Event(_) => None,
    }
}
