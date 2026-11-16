//! Work the reducer wants done outside itself. The host performs these and answers with events.

use serde::{Deserialize, Serialize};

use crate::contracts::preferences::ViewPreferences;
use crate::domain::appearance::CaptureOptions;
use crate::domain::automation::{AutomationDraft, AutomationPatch};
use crate::domain::conversation::OutgoingAttachment;
use crate::domain::coordination::CoordinationRole;
use crate::domain::diff::DiffRange;
use crate::domain::engine::{AgentEngine, AgentModel};
use crate::domain::platform::{BrowserAction, BrowserPermissions, ComputerUsePermission, SimulatorInput, WorkspaceRecord};
use crate::domain::question::ReviewTarget;
use crate::domain::run::{AgentEffort, Continuation, ExecutionPolicy};
use crate::domain::shortcuts::ShortcutOverrides;
use crate::events::{AutomationAck, SentDraft, ThreadNotice, WorkspaceInput};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunChannel {
    Main,
    Side,
}

/// Work a run performs without sending its prompt as a model turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum RunOperation {
    Compact { pre_tokens: i64 },
    Review { target: ReviewTarget },
}

/// What the agent process needs to start a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRunCommand {
    /// Whether the host stops this run's thread when its engine holds excessive memory.
    #[serde(default = "crate::util::yes")]
    pub memory_protection: bool,
    pub channel: RunChannel,
    pub task_id: String,
    pub title: String,
    pub run_id: String,
    pub prompt: String,
    pub workspace_id: String,
    pub policy: ExecutionPolicy,
    pub engine: AgentEngine,
    pub model: AgentModel,
    pub effort: AgentEffort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<RunOperation>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub chrome_browser: bool,
    /// Set false when the user turned computer use off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_use_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulator_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Continuation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_continuation: Option<bool>,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub fresh_session: bool,
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub unattended: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination_role: Option<CoordinationRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_workspace_id: Option<String>,
}

/// Commands to a run or a thread's session already under way.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum RunCommand {
    #[serde(rename = "memory-protection")]
    MemoryProtection { enabled: bool },
    #[serde(rename = "cancel")]
    Cancel { task_id: String, run_id: String },
    #[serde(rename = "answer-question")]
    AnswerQuestion { task_id: String, run_id: String, request_id: String, question_id: String, text: String },
    #[serde(rename = "approval")]
    Approval { task_id: String, run_id: String, approval_id: String, allow: bool },
    #[serde(rename = "steer")]
    Steer { task_id: String, run_id: String, message_id: String, prompt: String },
    #[serde(rename = "stop-process")]
    StopProcess { task_id: String, process_id: String },
    #[serde(rename = "label")]
    Label { task_id: String, title: String },
    #[serde(rename = "archive")]
    Archive {
        task_id: String,
        channel: RunChannel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuation: Option<Continuation>,
        archived: bool,
    },
    #[serde(rename = "role")]
    Role {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        coordination_role: Option<CoordinationRole>,
    },
}

/// A schedule to save, with the thread it belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadAutomationDraft {
    #[serde(flatten)]
    pub draft: AutomationDraft,
    pub task_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorktreeSpec {
    pub project_root: String,
    pub carry_changes: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSpec {
    pub workspace_id: String,
    pub branch: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardProposal {
    pub task_id: String,
    pub proposal_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum EffectWork {
    #[serde(rename = "schedule-snooze-expiry")]
    ScheduleSnoozeExpiry { at: Option<i64> },
    #[serde(rename = "schedule-limit-reset")]
    ScheduleLimitReset { at: Option<i64> },
    /// Takes a toast down at `at`, unless the user closes it first. None keeps it up.
    #[serde(rename = "schedule-toast-dismissal")]
    ScheduleToastDismissal { id: i64, at: Option<i64> },
    #[serde(rename = "pick-project")]
    PickProject,
    #[serde(rename = "add-project")]
    AddProject {
        root: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<i64>,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        start: bool,
    },
    #[serde(rename = "project-add.directories")]
    ProjectAddDirectories { prefix: String, computer_id: String, request: i64 },
    #[serde(rename = "project-add.pick")]
    ProjectAddPick { request: i64 },
    #[serde(rename = "register-project")]
    RegisterProject { project_id: String, root: String },
    #[serde(rename = "persist-preferences")]
    PersistPreferences { preferences: Box<ViewPreferences> },
    #[serde(rename = "resolve-run-workspace")]
    ResolveRunWorkspace {
        pending_id: String,
        picker: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<WorkspaceRecord>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create_worktree: Option<CreateWorktreeSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create_branch: Option<BranchSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkout: Option<BranchSpec>,
    },
    #[serde(rename = "start-run")]
    StartRun { command: Box<StartRunCommand> },
    #[serde(rename = "send-run-command")]
    SendRunCommand { command: RunCommand },
    #[serde(rename = "suggest-title")]
    SuggestTitle { task_id: String, engine: AgentEngine, model: AgentModel, text: String, attachments: Vec<String> },
    #[serde(rename = "preserve-message-images")]
    PreserveMessageImages { text: String, root: String, message_id: String },
    #[serde(rename = "load-subagent-activity")]
    LoadSubagentActivity { task_id: String, subagent_id: String },
    #[serde(rename = "load-subagent-metadata")]
    LoadSubagentMetadata {
        task_id: String,
        subagent_id: String,
        engine: AgentEngine,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// Nothing was left in front of the window, so close-tab closes the window.
    #[serde(rename = "close-window")]
    CloseWindow,
    /// Takes the keyboard off a page in the panel.
    #[serde(rename = "focus-window")]
    FocusWindow,
    #[serde(rename = "announce-thread")]
    AnnounceThread { notice: ThreadNotice },

    // Phone bridge.
    #[serde(rename = "remote.set-enabled")]
    RemoteSetEnabled { enabled: bool },
    #[serde(rename = "remote.create-pairing-code")]
    RemoteCreatePairingCode,
    #[serde(rename = "remote.revoke-device")]
    RemoteRevokeDevice { device_id: String },
    #[serde(rename = "remote.refresh")]
    RemoteRefresh,

    // Paired computers.
    #[serde(rename = "computer.discover")]
    ComputerDiscover,
    #[serde(rename = "computer.pair")]
    ComputerPair { host: String, name: String, code: String },
    #[serde(rename = "computer.forget")]
    ComputerForget { id: String },
    #[serde(rename = "computer.reconnect")]
    ComputerReconnect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "computer.rename")]
    ComputerRename { name: String },
    #[serde(rename = "computer.label")]
    ComputerLabel { id: String, name: String },
    #[serde(rename = "computer.forward")]
    ComputerForward {
        id: String,
        inputs: Vec<WorkspaceInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_add_request: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        draft: Option<SentDraft>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposal: Option<ForwardProposal>,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        silent: bool,
    },

    // Engines.
    #[serde(rename = "engine.read")]
    EngineRead {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refresh: Option<bool>,
    },
    #[serde(rename = "engine.sign-in")]
    EngineSignIn { engine: AgentEngine },
    #[serde(rename = "engine.update")]
    EngineUpdate {
        engine: AgentEngine,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        computer_id: Option<String>,
    },
    #[serde(rename = "engine.reload-settings")]
    EngineReloadSettings,

    // Automations.
    #[serde(rename = "automation.save")]
    AutomationSave { draft: ThreadAutomationDraft },
    #[serde(rename = "automation.update")]
    AutomationUpdate { task_id: String, patch: AutomationPatch },
    #[serde(rename = "automation.delete")]
    AutomationDelete { task_id: String },
    #[serde(rename = "automation.run-now")]
    AutomationRunNow { task_id: String },
    #[serde(rename = "automation.ack")]
    AutomationAck { ack: AutomationAck },

    // Worktrees.
    #[serde(rename = "create-worktree")]
    CreateWorktree {
        task_id: String,
        project_root: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        r#move: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<String>,
    },
    #[serde(rename = "release-worktree")]
    ReleaseWorktree { task_id: String, worktree_id: String, root: String, title: String },
    #[serde(rename = "list-worktrees")]
    ListWorktrees,
    #[serde(rename = "reveal-worktree")]
    RevealWorktree { root: String },
    #[serde(rename = "delete-worktree")]
    DeleteWorktree {
        worktree_id: String,
        root: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        missing_only: Option<bool>,
    },
    #[serde(rename = "checkout-branch")]
    CheckoutBranch {
        workspace_id: String,
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create: Option<bool>,
    },

    // Diffs and pull requests.
    #[serde(rename = "refresh-environment")]
    RefreshEnvironment {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
    },
    #[serde(rename = "read-diff")]
    ReadDiff { owner: String, workspace_id: String, range: DiffRange, ignore_whitespace: bool },
    #[serde(rename = "read-pull-request")]
    ReadPullRequest {
        workspace_id: String,
        branch: Option<String>,
        read: i64,
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        members: bool,
    },

    // Browser.
    #[serde(rename = "browser.permissions")]
    BrowserPermissions { permissions: BrowserPermissions },
    #[serde(rename = "browser.open")]
    BrowserOpen {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        tab_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    #[serde(rename = "browser.navigate")]
    BrowserNavigate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        tab_id: String,
        url: String,
    },
    #[serde(rename = "browser.history")]
    BrowserHistory {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        tab_id: String,
        delta: i8,
    },
    #[serde(rename = "browser.reload")]
    BrowserReload {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        tab_id: String,
    },
    #[serde(rename = "browser.act")]
    BrowserAct {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        tab_id: String,
        action: BrowserAction,
    },
    #[serde(rename = "browser.close")]
    BrowserClose { tab_id: String },
    /// Which tab the panel shows.
    #[serde(rename = "browser.show")]
    BrowserShow { tab_id: Option<String> },
    #[serde(rename = "browser.clear-data")]
    BrowserClearData,
    #[serde(rename = "focus-browser")]
    FocusBrowser { tab_id: String },
    #[serde(rename = "browser-import.list-sources")]
    BrowserImportListSources,
    #[serde(rename = "browser-import.list-sites")]
    BrowserImportListSites { source_id: String },
    #[serde(rename = "browser-import.import")]
    BrowserImportImport { source_id: String, sites: Vec<String> },

    // Find.
    #[serde(rename = "find-in-page")]
    FindInPage { tab_id: String, query: String, forward: bool, find_next: bool },
    #[serde(rename = "stop-find-in-page")]
    StopFindInPage { tab_id: String },
    #[serde(rename = "find-in-terminal")]
    FindInTerminal { terminal_id: String, query: String, forward: bool },
    #[serde(rename = "stop-find-in-terminal")]
    StopFindInTerminal { terminal_id: String },

    // Simulator.
    #[serde(rename = "simulator.list")]
    SimulatorList,
    #[serde(rename = "simulator.boot")]
    SimulatorBoot { udid: String },
    #[serde(rename = "simulator.release")]
    SimulatorRelease { udid: String },
    #[serde(rename = "simulator.input")]
    SimulatorInput {
        udid: String,
        input: SimulatorInput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<i64>,
    },

    // Dictation.
    #[serde(rename = "dictation.start")]
    DictationStart { id: String },
    #[serde(rename = "dictation.stop")]
    DictationStop { id: String },
    #[serde(rename = "dictation.cancel")]
    DictationCancel { id: String },
    #[serde(rename = "dictation.download")]
    DictationDownload,
    #[serde(rename = "dictation.cancel-download")]
    DictationCancelDownload,

    // Terminal.
    #[serde(rename = "terminal.start")]
    TerminalStart { terminal_id: String, cwd: String },
    #[serde(rename = "terminal.write")]
    TerminalWrite { terminal_id: String, data: String },
    #[serde(rename = "terminal.resize")]
    TerminalResize { terminal_id: String, cols: i64, rows: i64 },
    #[serde(rename = "terminal.close")]
    TerminalClose { terminal_id: String },

    // Composer images.
    #[serde(rename = "send-attachments")]
    SendAttachments {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        steer: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        attachments: Vec<OutgoingAttachment>,
    },

    /// Keeps dropped images in the app's own images and names the other files where they sit,
    /// skipping the sources the composer already holds.
    #[serde(rename = "attachments.drop")]
    DropAttachments {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        paths: Vec<String>,
        held: Vec<String>,
    },
    #[serde(rename = "attachments.paste-images")]
    PasteImages {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        images: Vec<crate::commands::PastedImage>,
    },

    // Desktop.
    #[serde(rename = "image.download")]
    ImageDownload { source: String },
    #[serde(rename = "file.open")]
    FileOpen { roots: Vec<String>, path: String, line: Option<i64> },
    #[serde(rename = "link.open")]
    OpenLink { url: String },
    #[serde(rename = "app.list")]
    AppList,
    #[serde(rename = "app.open-folder")]
    AppOpenFolder { root: String, app_id: String },
    #[serde(rename = "app.open-source-licenses")]
    AppOpenSourceLicenses,

    // App updates.
    #[serde(rename = "app.check-for-updates")]
    AppCheckForUpdates {
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        quiet: bool,
    },
    #[serde(rename = "app.download-update")]
    AppDownloadUpdate {
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        unattended: bool,
    },
    #[serde(rename = "app.install-update")]
    AppInstallUpdate {
        #[serde(default, skip_serializing_if = "crate::util::is_false")]
        unattended: bool,
    },

    // Computer use, plan usage, the terminal command.
    #[serde(rename = "computer-use.read")]
    ComputerUseRead,
    #[serde(rename = "computer-use.enable")]
    ComputerUseEnable { permission: ComputerUsePermission },
    #[serde(rename = "computer-use.restart")]
    ComputerUseRestart,
    #[serde(rename = "read-plan-usage")]
    ReadPlanUsage { engine: AgentEngine, read: i64 },
    #[serde(rename = "cli.read")]
    CliRead,
    #[serde(rename = "cli.install")]
    CliInstall,
    #[serde(rename = "cli.uninstall")]
    CliUninstall,

    // Shortcuts.
    #[serde(rename = "apply-shortcuts")]
    ApplyShortcuts { overrides: ShortcutOverrides },
    #[serde(rename = "apply-capture-options")]
    ApplyCaptureOptions { options: CaptureOptions },
    #[serde(rename = "capture-shortcut")]
    CaptureShortcut { capturing: bool },
}

/// One piece of external work, and whether it only keeps a view current after the input's own work.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effect {
    #[serde(flatten)]
    pub work: EffectWork,
    /// The input completes without waiting for this one; what it finds still lands through events.
    #[serde(default, skip_serializing_if = "crate::util::is_false")]
    pub follow_up: bool,
}

impl Effect {
    pub fn follow_up(work: EffectWork) -> Effect {
        Effect { work, follow_up: true }
    }

    pub fn type_name(&self) -> String {
        serde_json::to_value(&self.work).ok().and_then(|value| value.get("type").and_then(|kind| kind.as_str()).map(String::from)).unwrap_or_default()
    }
}

impl From<EffectWork> for Effect {
    fn from(work: EffectWork) -> Self {
        Effect { work, follow_up: false }
    }
}
