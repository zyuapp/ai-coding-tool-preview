//! Everything the window holds, as one flat record. Each group of fields belongs to one slice of the
//! reducer; only `reduce` writes any of it.

mod dock;
mod runs;

pub use dock::*;
pub use runs::*;

use serde::{Deserialize, Serialize};

use crate::domain::appearance::{CaptureOptions, ThemeMode, DEFAULT_MONO_FONT, DEFAULT_THEME, DEFAULT_THEME_MODE, DEFAULT_UI_FONT, READING_SIZE, TERMINAL_SIZE};
use crate::domain::automation::{AutomationEdit, AutomationView};
use crate::domain::conversation::{Annotation, AttachedFile, PastedText, StagedImage};
use crate::domain::coordination::{effort_role_of, OverviewFolds};
use crate::domain::engine::{engine_catalog, engine_has_model, engine_models_known, AgentEngine, AgentModel, EngineModel, EngineReadiness, EngineStatus, ModelCatalog, DEFAULT_ENGINE, DEFAULT_MODEL};
use crate::domain::find::{FindResults, FindTarget};
use crate::domain::mobile::MobileServerState;
use crate::domain::platform::{
    AppBuild, AppUpdate, AppUpdateRequest, BrowserApproval, CliState, ComputerFilter, ComputerLink, ComputerPairing, ComputerStatus, ComputerUseAccessState, DictationState, DiscoveredComputer, InstalledApp,
    PlanUsageState, SimulatorState,
};
use crate::domain::project::Project;
use crate::domain::pull_request::PullRequestRead;
use crate::domain::run::{AgentEffort, ExecutionPolicy, SubagentGroups, DEFAULT_EFFORT};
use crate::domain::shortcuts::ShortcutOverrides;
use crate::domain::sidebar::{SettingsSection, SidebarSections};
use crate::domain::terminal::{BrowserImportMemory, BrowserImportSite, BrowserImportSource};
use crate::domain::thread::Thread;
use crate::domain::thread_role::ThreadRole;
use crate::domain::toast::Toast;
use crate::domain::worktree::{ManagedWorktree, MoveAfterRun, Worktree};
use crate::events::{ChangedFilesResult, DesktopShortcutUnavailable};
use crate::util::Map;

/// Where a thread was left reading: the message held at the top and how far into it, or None for its foot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadingPoint {
    pub anchor: String,
    pub depth: f64,
}

/// The new-thread composer's one draft, kept whichever project or computer it is pointed at.
pub const NEW_THREAD_DRAFT: &str = "draft:";

/// The new-thread composer's project picker, as an `open_menu` value.
pub const THREAD_START_PROJECT_MENU: &str = "thread-start:project";

/// The add dialog belongs to this window even when it is asking another host to open a folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAddState {
    pub request: i64,
    pub computer_id: String,
    pub root: String,
    pub suggestions: Vec<String>,
    pub selected: i64,
    pub saving: bool,
    pub error: Option<String>,
    pub start: bool,
}

/// The folder editor, open on one project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEdit {
    pub project_id: String,
    /// A name typed beside a folder still being opened; `Some(None)` takes the name off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Option<String>>,
    pub saving: bool,
    pub error: Option<String>,
}

/// The branch a thread is to start from. `create` names one the repository does not have yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DraftBranch {
    pub name: String,
    pub create: bool,
}

/// A fork waiting on its name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkName {
    pub task_id: String,
    pub worktree: bool,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewStep {
    Targets,
    Base,
    Commit,
    Custom,
}

/// The native review picker follows the thread it was opened from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPicker {
    pub task_id: String,
    pub step: ReviewStep,
}

/// What the find bar is looking for, and where.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FindState {
    pub target: FindTarget,
    pub query: String,
    pub index: i64,
    /// Bumped whenever find is asked for again.
    pub focus: i64,
}

/// The jump panel: the name being searched for, and the row the user moved to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JumpState {
    pub query: String,
    pub index: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserImportPicker {
    pub source_id: String,
    pub sites: Option<Vec<BrowserImportSite>>,
    pub selected: Vec<String>,
    pub filter: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BrowserImportState {
    pub sources: Option<Vec<BrowserImportSource>>,
    pub picker: Option<BrowserImportPicker>,
    pub importing: bool,
    pub notice: Option<String>,
    pub error: Option<String>,
    pub last: Option<BrowserImportMemory>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BulkDelete {
    pub pending: Vec<String>,
    pub deleted: i64,
    pub recover: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSettingsState {
    pub project: Option<String>,
    pub confirming: Option<String>,
    pub missing_open: Option<bool>,
    pub expanded_threads: Vec<String>,
    pub confirming_safe: bool,
    pub bulk: Option<BulkDelete>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeMenuList {
    Threads,
    Destinations,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorktreeMenuSearch {
    pub threads: String,
    pub destinations: String,
}

impl WorktreeMenuSearch {
    pub fn get(&self, list: WorktreeMenuList) -> &str {
        match list {
            WorktreeMenuList::Threads => &self.threads,
            WorktreeMenuList::Destinations => &self.destinations,
        }
    }

    pub fn set(&mut self, list: WorktreeMenuList, query: String) {
        match list {
            WorktreeMenuList::Threads => self.threads = query,
            WorktreeMenuList::Destinations => self.destinations = query,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeMove {
    pub task_id: String,
    pub worktree: bool,
}

/// Where one composer's images stand between the send and the run that carries them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentSendState {
    pub busy: bool,
    pub error: Option<String>,
    pub sent: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_key: Option<String>,
}

/// The main composer sends for whichever thread is in front, so it has no thread of its own to key by.
pub const MAIN_COMPOSER: &str = "";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationReveal {
    pub task_id: String,
    pub annotation_id: String,
    pub message_id: String,
    pub count: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentSettingsReload {
    #[default]
    Idle,
    Reloading,
    Pending,
    Reloaded,
    Failed,
}

/// A settings control to scroll to and mark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingMark {
    pub id: String,
    pub visit: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DockFocus {
    pub owner: String,
    pub tab: String,
    pub count: i64,
}

/// One forked conversation. Its thread is an ordinary thread that is never persisted or listed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideChat {
    pub id: String,
    pub source_thread_id: String,
    pub error: Option<String>,
}

/// A paired computer as this one holds it: the link, and the last state it published.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairedComputer {
    #[serde(flatten)]
    pub link: ComputerLink,
    pub state: Option<Box<WorkspaceState>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputersState {
    pub name: String,
    pub found: Vec<DiscoveredComputer>,
    pub searching: bool,
    pub search_error: Option<String>,
    pub paired: Vec<PairedComputer>,
    pub pairing: Option<ComputerPairing>,
    pub active: Option<String>,
    pub dropped: Option<String>,
    pub filter: ComputerFilter,
    pub discarded_leads: Vec<String>,
}

impl Default for ComputersState {
    fn default() -> Self {
        ComputersState { name: String::new(), found: Vec::new(), searching: false, search_error: None, paired: Vec::new(), pairing: None, active: None, dropped: None, filter: "all".into(), discarded_leads: Vec::new() }
    }
}

pub const MAX_DISCARDED_LEADS: usize = 50;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceState {
    // Project add.
    pub project_add: Option<ProjectAddState>,
    pub project_add_sequence: i64,
    // Threads.
    pub threads: Vec<crate::shared::Shared<Thread>>,
    // Projects.
    pub projects: Vec<Project>,
    pub project_edit: Option<ProjectEdit>,
    // Worktrees.
    pub worktrees: Vec<Worktree>,
    pub managed_worktrees: Option<Vec<ManagedWorktree>>,
    pub worktree_settings: WorktreeSettingsState,
    pub worktree_menu_search: WorktreeMenuSearch,
    pub worktree_management_loading: bool,
    pub worktree_management_error: Option<String>,
    pub worktree_management_notice: Option<String>,
    pub creating_worktrees: Vec<String>,
    pub releasing_worktrees: Vec<String>,
    pub deleting_worktrees: Vec<String>,
    pub moves_after_run: Map<MoveAfterRun>,
    pub worktree_move: Option<WorktreeMove>,
    // Navigation.
    pub last_folder: Option<String>,
    pub current_id: Option<String>,
    pub history: Vec<String>,
    pub history_index: i64,
    // New-thread draft.
    pub draft_project_id: Option<String>,
    pub draft_policy: ExecutionPolicy,
    pub draft_branch: Option<DraftBranch>,
    pub draft_worktree: bool,
    pub draft_worktree_id: Option<String>,
    pub draft_role: Option<ThreadRole>,
    pub draft_engine: AgentEngine,
    pub draft_model: AgentModel,
    pub draft_effort: AgentEffort,
    pub draft_fast_mode: bool,
    // Engines.
    pub engine_status: Option<EngineStatus>,
    pub engine_checking: bool,
    pub engine_updating: Option<AgentEngine>,
    pub engine_update_queue: Vec<AgentEngine>,
    pub engine_updates_offered: bool,
    pub agent_settings_reload: AgentSettingsReload,
    // Composer.
    pub prompts: Map<String>,
    pub annotations: Map<Vec<Annotation>>,
    pub pastes: Map<Vec<PastedText>>,
    pub images: Map<Vec<StagedImage>>,
    pub files: Map<Vec<AttachedFile>>,
    pub composer_focus: i64,
    #[serde(default)]
    pub reading_points: Map<Option<ReadingPoint>>,
    pub annotation_reveal: Option<AnnotationReveal>,
    pub attachment_sends: Map<AttachmentSendState>,
    // Dialogs.
    pub fork_name: Option<ForkName>,
    pub archive_prompt: Option<String>,
    pub open_menu: Option<String>,
    pub review_picker: Option<ReviewPicker>,
    // Settings.
    pub sections: SidebarSections,
    pub subagent_groups: SubagentGroups,
    pub theme: String,
    pub theme_mode: ThemeMode,
    pub ui_font: String,
    pub mono_font: String,
    pub reading_size: f64,
    pub terminal_size: f64,
    pub activity_project: Option<String>,
    pub sidebar_open: bool,
    pub session_panel_open: bool,
    pub capture_sound: bool,
    pub capture_focus: bool,
    pub chrome_browser: bool,
    pub computer_use: bool,
    pub browser_tools: bool,
    /// Whether excessive engine memory stops its owning thread.
    pub memory_protection: bool,
    pub notifications: bool,
    pub settings_open: bool,
    pub settings_section: Option<SettingsSection>,
    pub settings_focus: Option<SettingMark>,
    pub usage_computer: Option<String>,
    pub favorite_models: Vec<EngineModel>,
    pub recent_projects: Vec<String>,
    pub shortcuts: ShortcutOverrides,
    pub capturing_shortcut: Option<String>,
    pub desktop_shortcut_unavailable: Option<DesktopShortcutUnavailable>,
    // Docks.
    pub overview_folds: Map<OverviewFolds>,
    pub dock_focus: Option<DockFocus>,
    pub docks: Map<ThreadDock>,
    pub diffs: Map<DiffState>,
    pub keyboard_tab: Option<String>,
    // Find bar, jump panel, zen.
    pub find: Option<FindState>,
    pub find_results: Option<FindResults>,
    pub jump: Option<JumpState>,
    pub zen: bool,
    // Browser.
    pub browser_origins: Vec<String>,
    pub browser_import: BrowserImportState,
    pub browser_approval: Option<BrowserApproval>,
    // Simulator.
    pub simulator_device: Option<String>,
    pub simulator: SimulatorState,
    // Dictation.
    pub dictation: DictationState,
    // Git.
    pub environments: Map<ChangedFilesResult>,
    pub pull_request: Option<PullRequestRead>,
    pub member_pull_requests: Map<PullRequestRead>,
    // Desktop.
    pub installed_apps: Option<Vec<InstalledApp>>,
    pub focused: bool,
    pub viewing_image: Option<String>,
    pub viewing_gallery: Vec<String>,
    // Machine access.
    pub cli: CliState,
    pub plan_usage: PlanUsageState,
    pub computer_use_permissions: ComputerUseAccessState,
    pub computer_use_setup: bool,
    // Automations.
    pub automations: Vec<AutomationView>,
    pub automation_edits: Map<AutomationEdit>,
    // Runs.
    pub active_runs: Map<ActiveRun>,
    pub run_statuses: Map<ThreadRunStatus>,
    pub approvals: Map<ApprovalView>,
    pub streaming_tails: Map<StreamingTail>,
    pub background_processes: Map<Vec<crate::domain::run::BackgroundProcess>>,
    pub workflows: Map<Vec<crate::domain::workflow::Workflow>>,
    pub subagents: Map<Vec<crate::domain::run::Subagent>>,
    pub goals: Map<crate::domain::run::ActiveGoal>,
    pub pending_runs: Map<PendingRun>,
    pub starting_proposals: Vec<String>,
    pub quiet_stops: Vec<String>,
    pub quiet_sessions: Vec<QuietSession>,
    pub queued_messages: Map<Vec<QueuedMessage>>,
    pub last_run_ids: Map<String>,
    // Side chats.
    pub side_chats: Vec<SideChat>,
    pub side_chat_sequence: i64,
    // Phone bridge.
    pub remote: MobileServerState,
    pub remote_checking: bool,
    // App updates.
    pub app_update: AppUpdate,
    pub app_build: Option<AppBuild>,
    pub app_update_request: Option<AppUpdateRequest>,
    pub engine_upgrades_owed: bool,
    // Toasts.
    pub toasts: Vec<Toast>,
    pub toast_sequence: i64,
    pub toasts_held: bool,
    pub dismissed_toasts: Vec<String>,
    // Paired computers.
    pub computers: ComputersState,
    // Store.
    pub storage_error: Option<String>,
    pub hidden_threads: i64,
    pub writable: bool,
    pub restored: bool,
    // Errors.
    pub action_error: Option<String>,
    pub action_error_page: Option<SettingsSection>,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        WorkspaceState::empty(None)
    }
}

impl WorkspaceState {
    /// Whether settings cover the window: opened by the user, or held open while a run waits for
    /// computer use to be set up (workspace-view.ts settingsOpen).
    pub fn settings_shown(&self) -> bool {
        self.settings_open || self.computer_use_setup
    }

    /// The settings page on screen; the computer-use setup holds its own page.
    pub fn settings_page(&self) -> Option<SettingsSection> {
        if self.computer_use_setup {
            Some(SettingsSection::ComputerUse)
        } else {
            self.settings_section
        }
    }

    pub fn empty(storage_error: Option<String>) -> WorkspaceState {
        WorkspaceState {
            project_add: None,
            project_add_sequence: 0,
            threads: Vec::new(),
            projects: Vec::new(),
            project_edit: None,
            worktrees: Vec::new(),
            managed_worktrees: None,
            worktree_settings: WorktreeSettingsState::default(),
            worktree_menu_search: WorktreeMenuSearch::default(),
            worktree_management_loading: false,
            worktree_management_error: None,
            worktree_management_notice: None,
            creating_worktrees: Vec::new(),
            releasing_worktrees: Vec::new(),
            deleting_worktrees: Vec::new(),
            moves_after_run: Map::new(),
            worktree_move: None,
            last_folder: None,
            current_id: None,
            history: Vec::new(),
            history_index: -1,
            draft_project_id: None,
            draft_policy: ExecutionPolicy::Confirm,
            draft_branch: None,
            draft_worktree: false,
            draft_worktree_id: None,
            draft_role: None,
            draft_engine: DEFAULT_ENGINE,
            draft_model: DEFAULT_MODEL.into(),
            draft_effort: DEFAULT_EFFORT,
            draft_fast_mode: false,
            engine_status: None,
            engine_checking: false,
            engine_updating: None,
            engine_update_queue: Vec::new(),
            engine_updates_offered: false,
            agent_settings_reload: AgentSettingsReload::Idle,
            prompts: Map::new(),
            annotations: Map::new(),
            pastes: Map::new(),
            images: Map::new(),
            files: Map::new(),
            composer_focus: 0,
            reading_points: Map::new(),
            annotation_reveal: None,
            attachment_sends: Map::new(),
            fork_name: None,
            archive_prompt: None,
            open_menu: None,
            review_picker: None,
            sections: SidebarSections::default(),
            subagent_groups: SubagentGroups::default(),
            theme: DEFAULT_THEME.into(),
            theme_mode: DEFAULT_THEME_MODE,
            ui_font: DEFAULT_UI_FONT.into(),
            mono_font: DEFAULT_MONO_FONT.into(),
            reading_size: READING_SIZE.default,
            terminal_size: TERMINAL_SIZE.default,
            activity_project: None,
            sidebar_open: true,
            session_panel_open: false,
            capture_sound: true,
            capture_focus: true,
            chrome_browser: false,
            computer_use: true,
            browser_tools: true,
            memory_protection: true,
            notifications: true,
            settings_open: false,
            settings_section: None,
            settings_focus: None,
            usage_computer: None,
            favorite_models: Vec::new(),
            recent_projects: Vec::new(),
            shortcuts: ShortcutOverrides::new(),
            capturing_shortcut: None,
            desktop_shortcut_unavailable: None,
            overview_folds: Map::new(),
            dock_focus: None,
            docks: Map::new(),
            diffs: Map::new(),
            keyboard_tab: None,
            find: None,
            find_results: None,
            jump: None,
            zen: false,
            browser_origins: Vec::new(),
            browser_import: BrowserImportState::default(),
            browser_approval: None,
            simulator_device: None,
            simulator: SimulatorState::default(),
            dictation: DictationState::default(),
            environments: Map::new(),
            pull_request: None,
            member_pull_requests: Map::new(),
            installed_apps: None,
            focused: true,
            viewing_image: None,
            viewing_gallery: Vec::new(),
            cli: CliState::default(),
            plan_usage: PlanUsageState::default(),
            computer_use_permissions: ComputerUseAccessState::default(),
            computer_use_setup: false,
            automations: Vec::new(),
            automation_edits: Map::new(),
            active_runs: Map::new(),
            run_statuses: Map::new(),
            approvals: Map::new(),
            streaming_tails: Map::new(),
            background_processes: Map::new(),
            workflows: Map::new(),
            subagents: Map::new(),
            goals: Map::new(),
            pending_runs: Map::new(),
            starting_proposals: Vec::new(),
            quiet_stops: Vec::new(),
            quiet_sessions: Vec::new(),
            queued_messages: Map::new(),
            last_run_ids: Map::new(),
            side_chats: Vec::new(),
            side_chat_sequence: 0,
            remote: MobileServerState::default(),
            remote_checking: false,
            app_update: AppUpdate::Idle,
            app_build: None,
            app_update_request: None,
            engine_upgrades_owed: false,
            toasts: Vec::new(),
            toast_sequence: 0,
            toasts_held: false,
            dismissed_toasts: Vec::new(),
            computers: ComputersState::default(),
            writable: storage_error.is_none(),
            storage_error,
            hidden_threads: 0,
            restored: false,
            action_error: None,
            action_error_page: None,
        }
    }

    pub fn thread(&self, id: &str) -> Option<&Thread> {
        self.threads.iter().find(|thread| thread.id == id).map(|thread| &**thread)
    }

    pub fn thread_mut(&mut self, id: &str) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|thread| thread.id == id).map(|thread| &mut **thread)
    }

    pub fn current_thread(&self) -> Option<&Thread> {
        self.current_id.as_deref().and_then(|id| self.thread(id))
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|project| project.id == id)
    }

    pub fn worktree(&self, id: &str) -> Option<&Worktree> {
        self.worktrees.iter().find(|worktree| worktree.id == id)
    }

    /// The models this machine's engines offer, as the host last named them.
    pub fn catalog(&self) -> std::borrow::Cow<'static, ModelCatalog> {
        engine_catalog(self.engine_status.as_ref())
    }

    /// An engine the host has said nothing about is taken as ready.
    pub fn engine_readiness(&self, engine: AgentEngine) -> EngineReadiness {
        self.engine_status.as_ref().and_then(|status| status.get(engine).cloned()).unwrap_or_else(EngineReadiness::ready)
    }

    /// Whether a run may go to `model`: one its engine offers, or any while the engine has yet to name its models.
    pub fn engine_offers_model(&self, engine: AgentEngine, model: &str) -> bool {
        engine_has_model(&self.catalog(), engine, model) || !engine_models_known(self.engine_status.as_ref(), engine)
    }

    /// The agent choices the new-thread composer holds.
    pub fn draft(&self) -> AgentDraft {
        AgentDraft { engine: self.draft_engine, model: self.draft_model.clone(), effort: self.draft_effort, policy: self.draft_policy, fast_mode: self.draft_fast_mode }
    }

    /// The draft with `change` made and held to its engine, which is how its agent choices are ever written.
    pub fn changed_draft(&self, change: impl FnOnce(&mut AgentDraftChange)) -> AgentDraft {
        let draft = self.draft();
        let mut asked = AgentDraftChange { engine: draft.engine, model: Some(draft.model), effort: draft.effort, policy: draft.policy, fast_mode: draft.fast_mode };
        change(&mut asked);
        normalize_draft(&self.catalog(), asked, effort_role_of(None, self.draft_role))
    }

    /// `changed_draft`, written to the state.
    pub fn with_draft(&mut self, change: impl FnOnce(&mut AgentDraftChange)) {
        let draft = self.changed_draft(change);
        self.draft_engine = draft.engine;
        self.draft_model = draft.model;
        self.draft_effort = draft.effort;
        self.draft_policy = draft.policy;
        self.draft_fast_mode = draft.fast_mode;
    }

    pub fn capture_options(&self) -> CaptureOptions {
        CaptureOptions { sound: self.capture_sound, focus: self.capture_focus }
    }

    /// The paired computer whose thread is on screen, while its line is up.
    pub fn active_computer(&self) -> Option<&PairedComputer> {
        let active = self.computers.active.as_deref()?;
        self.computers.paired.iter().find(|computer| computer.link.id == active).filter(|computer| computer.link.status == ComputerStatus::Connected)
    }

    /// The selected host, which may stay on screen offline while its terminal waits to reconnect.
    pub fn selected_computer(&self) -> Option<&PairedComputer> {
        let active = self.computers.active.as_deref()?;
        let computer = self.computers.paired.iter().find(|computer| computer.link.id == active)?;
        if computer.link.status == ComputerStatus::Connected {
            return Some(computer);
        }
        let state = computer.state.as_deref()?;
        let dock = front_dock(state).1;
        (dock.open && dock.terminals.iter().any(|terminal| terminal.id == dock.tab)).then_some(computer)
    }

    pub fn paired_id(&self, id: Option<&str>) -> Option<String> {
        let id = id?;
        self.computers.paired.iter().any(|computer| computer.link.id == id).then(|| id.to_string())
    }

    /// The ids of every thread that only lives for this session.
    pub fn side_chat_ids(&self) -> std::collections::HashSet<String> {
        self.side_chats.iter().map(|chat| chat.id.clone()).collect()
    }

    pub fn attachment_send_for(&self, task_id: Option<&str>) -> AttachmentSendState {
        self.attachment_sends.get(task_id.unwrap_or(MAIN_COMPOSER)).cloned().unwrap_or_default()
    }

    /// Sets a composer's prompt, dropping the entry when it is empty.
    pub fn set_prompt(&mut self, key: &str, prompt: &str) {
        if prompt.is_empty() {
            self.prompts.shift_remove(key);
        } else {
            self.prompts.insert(key.to_string(), prompt.to_string());
        }
    }
}

/// What the new-thread composer starts its thread on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDraft {
    pub engine: AgentEngine,
    pub model: AgentModel,
    pub effort: AgentEffort,
    pub policy: ExecutionPolicy,
    pub fast_mode: bool,
}

/// A draft being changed, whose model may be left for the engine to name.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentDraftChange {
    pub engine: AgentEngine,
    pub model: Option<AgentModel>,
    pub effort: AgentEffort,
    pub policy: ExecutionPolicy,
    pub fast_mode: bool,
}

/// A draft held to its engine: no model means the engine's default, the effort moves to the nearest
/// the model takes, and the mode to the nearest the engine offers.
pub fn normalize_draft(catalog: &ModelCatalog, draft: AgentDraftChange, role: Option<crate::domain::coordination::CoordinationRole>) -> AgentDraft {
    use crate::domain::engine::{default_model_in, effort_for_model, policy_for};
    let named = draft.model.or_else(|| default_model_in(catalog, draft.engine));
    let engine = if named.is_none() { DEFAULT_ENGINE } else { draft.engine };
    let model = named.or_else(|| default_model_in(catalog, DEFAULT_ENGINE)).unwrap_or_else(|| DEFAULT_MODEL.into());
    AgentDraft { engine, effort: effort_for_model(catalog, engine, &model, draft.effort, role), model, policy: policy_for(engine, draft.policy), fast_mode: draft.fast_mode }
}

/// Whether two reading points name the same place.
pub fn same_reading_point(a: &Option<ReadingPoint>, b: &Option<ReadingPoint>) -> bool {
    a == b
}
