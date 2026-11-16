//! The single writer of workspace state. Commands come from the UI and any other caller; events
//! report what the outside world did. External work is described as effects for the host to perform.

pub mod automations;
pub mod composer;
pub mod computers;
pub mod coordination;
pub mod coordination_remote;
pub mod devices;
pub mod dock;
pub mod errors;
pub mod find;
pub mod misc;
pub mod projects;
pub mod proposals;
pub mod pull_requests;
pub mod resource_stops;
pub mod run_queue;
pub mod run_state;
pub mod runs;
pub mod sending;
pub mod settings;
pub mod settlement;
pub mod store;
pub mod threads;
pub mod toasts;
pub mod view;
pub mod worktrees;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod coordination_tests;
#[cfg(test)]
mod devices_tests;
#[cfg(test)]
mod fuzz_tests;

use crate::commands::AppCommand;
use crate::domain::coordination::effort_role_of;
use crate::domain::engine::{default_effort_for, effort_for_model, efforts_for, thread_model};
use crate::domain::run::ExecutionPolicy;
use crate::domain::shortcuts::{slot_shortcut_index, ShortcutSurface};
use crate::domain::thread::{clamp_title, next_snooze_expiry};
use crate::effects::{Effect, EffectWork};
use crate::events::{agent_event_input, CommandResult, WorkspaceEvent, WorkspaceInput};
use crate::state::{dock_for, dock_owner, front_dock, keyboard_terminal_id, keyboard_thread_id, WorkspaceState, THREAD_START_PROJECT_MENU};
use crate::view::sidebar::thread_slots;
use crate::workspace::preferences::view_preferences;

/// What one input came to: the work it asks for, and its answer for whoever sent it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transition {
    pub effects: Vec<Effect>,
    pub result: Option<CommandResult>,
}

/// What a feature's handler made of an input, beyond the state it already changed in place.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Step {
    pub effects: Vec<Effect>,
    pub result: Option<CommandResult>,
    /// Turns the input down, saying why where the window shows errors.
    pub refusal: Option<String>,
    /// Saves the window's preferences as they stand once the input is in.
    pub persists: bool,
}

impl Step {
    pub fn none() -> Step {
        Step::default()
    }

    pub fn effects(effects: Vec<Effect>) -> Step {
        Step { effects, ..Step::default() }
    }

    pub fn effect(work: EffectWork) -> Step {
        Step { effects: vec![work.into()], ..Step::default() }
    }

    pub fn refuse(message: impl Into<String>) -> Step {
        Step { refusal: Some(message.into()), ..Step::default() }
    }

    pub fn result(result: CommandResult) -> Step {
        Step { result: Some(result), ..Step::default() }
    }

    pub fn persisting() -> Step {
        Step { persists: true, ..Step::default() }
    }

    pub fn with(mut self, work: EffectWork) -> Step {
        self.effects.push(work.into());
        self
    }

    /// Whether this step turned its input down, refusing it or answering that it failed.
    pub fn failed(&self) -> bool {
        self.refusal.is_some() || self.result.as_ref().is_some_and(|result| !result.is_ok())
    }

    /// This step followed by another, effects in order, the later result standing.
    pub fn then(mut self, next: Step) -> Step {
        self.effects.extend(next.effects);
        if next.result.is_some() {
            self.result = next.result;
        }
        if next.refusal.is_some() {
            self.refusal = next.refusal;
        }
        self.persists |= next.persists;
        self
    }

    /// Runs `next` unless this step was refused, joining what it makes.
    pub fn and_then(self, state: &mut WorkspaceState, next: impl FnOnce(&mut WorkspaceState) -> Step) -> Step {
        if self.refusal.is_some() {
            return self;
        }
        let following = next(state);
        self.then(following)
    }
}

/// A step's answer joined into the state it was made on.
pub fn finish(state: &mut WorkspaceState, step: Step) -> Transition {
    let Step { mut effects, result, refusal, persists } = step;
    let result = match refusal {
        Some(message) => {
            state.action_error = Some(message.clone());
            Some(CommandResult::failed(message))
        }
        None => result,
    };
    if persists {
        effects.push(EffectWork::PersistPreferences { preferences: Box::new(view_preferences(state)) }.into());
    }
    Transition { effects, result }
}

/// Refuses an input where the window shows errors, which also answers whoever sent it.
pub fn rejected(state: &mut WorkspaceState, message: impl Into<String>) -> Transition {
    let message = message.into();
    state.action_error = Some(message.clone());
    Transition { effects: Vec::new(), result: Some(CommandResult::failed(message)) }
}

/// The keys go back to the window, which a page in the panel otherwise holds.
pub fn take_keys() -> Vec<Effect> {
    vec![EffectWork::FocusWindow.into()]
}

/// Marks effects as follow-ups: work that keeps a view current, which the input does not wait for.
pub fn follow_ups(effects: Vec<Effect>) -> Vec<Effect> {
    effects.into_iter().map(|effect| Effect { follow_up: true, ..effect }).collect()
}

/// A named thread has to exist; an unnamed command falls back to the one on screen.
pub fn target_id(state: &WorkspaceState, task_id: Option<&str>) -> Option<String> {
    let target = match task_id {
        None => state.current_id.clone(),
        Some(id) => state.threads.iter().any(|thread| thread.id == id).then(|| id.to_string()),
    };
    // Every caller reads an empty id as none, as the TypeScript's truthiness checks do.
    target.filter(|id| !id.is_empty())
}

/// Reduces one input and returns the work it asks for.
pub fn reduce(state: &mut WorkspaceState, input: impl Into<WorkspaceInput>) -> Vec<Effect> {
    reduce_transition(state, input).effects
}

/// Reduces one input and returns both its work and its answer for whoever sent it.
pub fn reduce_transition(state: &mut WorkspaceState, input: impl Into<WorkspaceInput>) -> Transition {
    let input = input.into();
    let before = Before::of(state);
    let transition = reduce_input(state, input);
    settled_zen(&before, state);
    settled_archive_prompt(state);
    transition
}

/// What `reduce` needs of the state it was given once the input has changed it.
struct Before {
    current_id: Option<String>,
    browser_origins: Vec<String>,
    dock_focus: Option<crate::state::DockFocus>,
    zen: bool,
    shown_thread: String,
    sidebar_open: bool,
    session_panel_open: bool,
    dock_open: bool,
    snooze_expiry: Option<i64>,
    threads_len: usize,
    projects: Vec<(String, bool)>,
    draft_project_id: Option<String>,
}

impl Before {
    fn of(state: &WorkspaceState) -> Before {
        Before {
            current_id: state.current_id.clone(),
            browser_origins: state.browser_origins.clone(),
            dock_focus: state.dock_focus.clone(),
            zen: state.zen,
            shown_thread: shown_thread(state),
            sidebar_open: state.sidebar_open,
            session_panel_open: state.session_panel_open,
            dock_open: zen_dock_open(state),
            snooze_expiry: next_snooze_expiry(&state.threads),
            threads_len: state.threads.len(),
            projects: state.projects.iter().map(|project| (project.id.clone(), project.unlisted)).collect(),
            draft_project_id: state.draft_project_id.clone(),
        }
    }
}

fn reduce_input(state: &mut WorkspaceState, input: WorkspaceInput) -> Transition {
    match &input {
        WorkspaceInput::Command(AppCommand::Shortcut { action, surface }) => {
            let commands = shortcut_commands(state, action, *surface);
            return run_commands(state, commands);
        }
        WorkspaceInput::Command(AppCommand::Escape) => {
            let commands = escape_commands(state);
            return run_commands(state, commands);
        }
        WorkspaceInput::Command(AppCommand::ForkConfirm { title }) => return confirm_fork(state, title),
        WorkspaceInput::Command(AppCommand::ArchiveConfirm) => {
            let Some(task_id) = state.archive_prompt.take() else { return Transition::default() };
            return reduce_transition(state, AppCommand::TaskArchive { task_id });
        }
        WorkspaceInput::Event(WorkspaceEvent::AgentEvents { events }) => {
            let mut combined = Transition::default();
            for event in events.clone() {
                let next = reduce_transition(state, agent_event_input(event));
                combined = combine(combined, next);
            }
            return combined;
        }
        _ => {}
    }
    let route = computers::route_input(state, &input);
    let carried = match route {
        computers::InputRoute::Local => Vec::new(),
        computers::InputRoute::Refuse { message, .. } => return rejected(state, message),
        computers::InputRoute::Computer { computer, inputs, select, also, draft } => {
            if !also {
                return computers::forwarded(state, &computer, inputs, select, draft);
            }
            vec![computers::forward_effect(&computer, inputs, draft)]
        }
    };
    let before_active = state.active_computer().map(|computer| computer.link.id.clone());
    if (state.computers.active.is_some() || state.computers.dropped.is_some()) && computers::leaves_computer(state, &input) {
        state.computers.active = None;
        state.computers.dropped = None;
    }
    let left_behind = computers::left_behind(before_active.as_deref(), state);
    let before = Before::of(state);
    let shown_devices = devices::shown_devices(state);
    let was_downloading = matches!(state.app_update, crate::domain::platform::AppUpdate::Downloading { .. });
    let (input, paired, swapped) = paired_before(state, input);
    let coordinating = coordination::CoordinationBefore::of(state, &input, paired);
    let policy_set = match &input {
        WorkspaceInput::Command(AppCommand::TaskSetPolicy { task_id, policy }) => threads::policy_allowed(state, task_id.as_deref(), *policy),
        _ => false,
    };
    let limits = runs::LimitsBefore::of(state, &input);
    let memory_protection = state.memory_protection;
    let mut transition = if swapped { Transition::default() } else { apply(state, &input) };
    reconcile_snoozes(&before, state, &input, &mut transition);
    runs::reconcile_limit_pauses(&limits, state, &input, &mut transition);
    transition.effects.extend(coordination::reconcile_coordination(&coordinating, state));
    transition.effects.extend(devices::reconcile_app_update(was_downloading, state));
    transition.effects.extend(devices::reconcile_simulators(&shown_devices, state));
    if matches!(input, WorkspaceInput::Event(WorkspaceEvent::PreferencesLoaded { .. })) && state.memory_protection != memory_protection {
        transition.effects.push(EffectWork::SendRunCommand { command: crate::effects::RunCommand::MemoryProtection { enabled: state.memory_protection } }.into());
    }
    if !left_behind.is_empty() || !carried.is_empty() {
        let mut effects = left_behind;
        effects.extend(carried);
        effects.append(&mut transition.effects);
        transition.effects = effects;
    }
    settled_unlisted(&before, state);
    dock::with_overview_panels(state);
    dock::pruned_workflow_panels(state);
    find::pruned_find(state);
    let browsing = transition.effects.iter().any(|effect| {
        matches!(effect.work, EffectWork::BrowserOpen { .. } | EffectWork::BrowserNavigate { .. } | EffectWork::BrowserAct { .. } | EffectWork::BrowserHistory { .. } | EffectWork::BrowserReload { .. })
    });
    // A policy the engine takes is written even when it is the one already set.
    let policy_changed = policy_set;
    let loaded = matches!(input, WorkspaceInput::Event(WorkspaceEvent::StoreLoaded { .. } | WorkspaceEvent::PreferencesLoaded { .. }));
    // Clearing site data always hands the panel a fresh, empty list of origins.
    let cleared = matches!(input, WorkspaceInput::Command(AppCommand::BrowserClearData));
    if cleared || state.browser_origins != before.browser_origins || policy_changed || loaded || browsing {
        transition.effects.insert(0, EffectWork::BrowserPermissions { permissions: browser_permissions(state) }.into());
    }
    if state.current_id == before.current_id {
        return transition;
    }
    if matches!(state.open_menu.as_deref(), Some("session:location") | Some(THREAD_START_PROJECT_MENU)) {
        state.open_menu = None;
    }
    if state.dock_focus.is_some() && state.dock_focus == before.dock_focus {
        state.dock_focus = None;
    }
    let navigating = matches!(input, WorkspaceInput::Command(AppCommand::GoBack | AppCommand::GoForward));
    if let Some(current) = state.current_id.clone() {
        if !navigating {
            record_visit(state, &current);
        }
    }
    transition.effects.extend(dock::shown_page_effects(state));
    if state.focused {
        transition.effects.extend(take_keys());
    }
    transition
}

/// What the paired computers held before an input, taking a computer's new state in, and the
/// computers a new list of links drops, by moving them rather than copying what they hold.
fn paired_before(state: &mut WorkspaceState, mut input: WorkspaceInput) -> (WorkspaceInput, coordination_remote::PairedBefore, bool) {
    use coordination_remote::PairedBefore;
    let changes = matches!(input, WorkspaceInput::Event(WorkspaceEvent::ComputerState { .. } | WorkspaceEvent::ComputersChanged { .. }) | WorkspaceInput::Command(AppCommand::ComputersLabel { .. }));
    let mut paired = PairedBefore::of(state, changes);
    let mut swapped = false;
    match &mut input {
        WorkspaceInput::Event(WorkspaceEvent::ComputerState { id, state: incoming }) => {
            swapped = true;
            if let Some(computer) = state.computers.paired.iter_mut().find(|computer| &computer.link.id == id) {
                let held = std::mem::take(incoming);
                paired.replaced = Some((id.clone(), computer.state.replace(held)));
            }
        }
        WorkspaceInput::Event(WorkspaceEvent::ComputersChanged { links, .. }) => {
            let kept: std::collections::HashSet<&str> = links.iter().map(|link| link.id.as_str()).collect();
            let (stay, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut state.computers.paired).into_iter().partition(|computer| kept.contains(computer.link.id.as_str()));
            state.computers.paired = stay;
            paired.gone = gone;
        }
        _ => {}
    }
    (input, paired, swapped)
}

/// Every input, sent to the feature that owns it.
fn apply(state: &mut WorkspaceState, input: &WorkspaceInput) -> Transition {
    let step = match input {
        WorkspaceInput::Command(command) => apply_command(state, command),
        WorkspaceInput::Event(event) => apply_event(state, event),
    };
    match step {
        Applied::Step(step) => finish(state, step),
        Applied::Transition(transition) => transition,
    }
}

/// A feature's answer: a step still to be joined into the state, or a finished transition.
pub enum Applied {
    Step(Step),
    Transition(Transition),
}

impl From<Step> for Applied {
    fn from(step: Step) -> Self {
        Applied::Step(step)
    }
}

impl From<Transition> for Applied {
    fn from(transition: Transition) -> Self {
        Applied::Transition(transition)
    }
}

fn apply_command(state: &mut WorkspaceState, command: &AppCommand) -> Applied {
    use AppCommand as C;
    match command {
        C::TaskNew { .. }
        | C::TaskSelect { .. }
        | C::TaskDismiss { .. }
        | C::TaskDismissAll { .. }
        | C::TaskReadAll { .. }
        | C::TaskArchive { .. }
        | C::TaskSnooze { .. }
        | C::TaskUnsnooze { .. }
        | C::TaskRestore { .. }
        | C::TaskClearArchive
        | C::TaskRename { .. }
        | C::TaskSetRole { .. }
        | C::TaskFork { .. }
        | C::ForkName { .. }
        | C::ArchivePrompt { .. }
        | C::TaskMove { .. }
        | C::TaskSetPolicy { .. }
        | C::TaskSetModel { .. }
        | C::TaskSetEffort { .. }
        | C::TaskSetFastMode { .. } => threads::reduce_thread_command(state, command).into(),

        C::TaskSend(_) | C::QuestionAnswer { .. } | C::QuestionSetAnswer { .. } | C::TaskSteerQueued { .. } | C::TaskDropQueued { .. } | C::TaskEditQueued { .. } => sending::reduce_sending(state, command, None).into(),

        C::LimitResume { task_id } => runs::resumed(state, task_id).into(),
        C::LimitCancel { task_id } => runs::pause_cancelled(state, task_id).into(),
        C::ResourceResume { .. } | C::RunCompact { .. } | C::RunCancel { .. } | C::RunStopProcess { .. } | C::RunDecide { .. } | C::ReviewOpen { .. } | C::ReviewClose | C::ReviewSetStep { .. } | C::ReviewStart { .. } => runs::reduce_run_control(state, command).into(),

        C::SetPrompt { .. }
        | C::ReadingPoint { .. }
        | C::AnnotationAdd { .. }
        | C::AnnotationNote { .. }
        | C::AnnotationRemove { .. }
        | C::AnnotationRecall { .. }
        | C::AnnotationReveal { .. }
        | C::PasteAdd { .. }
        | C::PasteRemove { .. }
        | C::PasteRecall { .. }
        | C::ImageAdd { .. }
        | C::ImageRemove { .. }
        | C::ImageRecall { .. }
        | C::FileAttach { .. }
        | C::FileDetach { .. }
        | C::FileRecall { .. }
        | C::AttachmentsDrop { .. }
        | C::AttachmentsPasteImages { .. }
        | C::AttachmentsNotice { .. } => composer::reduce_composer(state, command).into(),

        C::DismissActionError => {
            state.action_error = None;
            state.action_error_page = None;
            Step::none().into()
        }
        C::DismissHiddenTasks => {
            state.hidden_threads = 0;
            Step::none().into()
        }
        C::SetModelFavorite { engine, model, favorite } => settings::model_favorite(state, *engine, model, *favorite).into(),
        C::RememberProject { project_id } => settings::remember_project(state, project_id).into(),
        C::DismissToast { .. } | C::HoldToasts { .. } => toasts::reduce_toasts(state, command).into(),

        C::SetTheme { .. }
        | C::SetThemeFamily { .. }
        | C::SetThemeMode { .. }
        | C::SystemScheme { .. }
        | C::SetUiFont { .. }
        | C::SetMonoFont { .. }
        | C::SetReadingSize { .. }
        | C::SetTerminalSize { .. }
        | C::SetActivityProject { .. }
        | C::SetSidebarOpen { .. }
        | C::FocusComposer { .. }
        | C::SetShortcut { .. }
        | C::ResetShortcuts
        | C::CaptureShortcut { .. }
        | C::InspectSubagent { .. }
        | C::SetCaptureOptions { .. }
        | C::SetChromeBrowser { .. }
        | C::SetComputerUse { .. }
        | C::SetBrowserTools { .. }
        | C::SetMemoryProtection { .. }
        | C::SetNotifications { .. }
        | C::SetSessionPanelOpen { .. }
        | C::SetSettingsOpen { .. }
        | C::SetUsageComputer { .. }
        | C::SetSubagentGroup { .. }
        | C::SetSectionOpen { .. } => settings::reduce_settings(state, command).into(),

        C::SetMenu { .. }
        | C::GoBack
        | C::GoForward
        | C::SetFocused { .. }
        | C::DockKeys { .. }
        | C::FindOpen { .. }
        | C::FindQuery { .. }
        | C::FindStep { .. }
        | C::FindClose
        | C::JumpOpen
        | C::JumpQuery { .. }
        | C::JumpStep { .. }
        | C::JumpChoose { .. }
        | C::JumpChooseSetting { .. }
        | C::JumpClose
        | C::SetZen { .. }
        | C::DismissComputerUseSetup => view::reduce_view(state, command).into(),

        C::CloseTab
        | C::NewTab
        | C::SetDockOpen { .. }
        | C::SetDockExpanded { .. }
        | C::OpenDockPanel { .. }
        | C::CloseDockPanel { .. }
        | C::CloseThreadTab { .. }
        | C::OpenWorkflow { .. }
        | C::SelectDockTab { .. }
        | C::SelectDockIndex { .. }
        | C::SetOverviewGroup { .. } => dock::reduce_dock(state, command).into(),

        C::EditProject { .. } | C::ProjectEdit { .. } | C::ProjectRemove { .. } | C::ProjectOpen { .. } | C::ProjectAdd { .. } | C::AddProjectClose | C::AddProjectDevice { .. } | C::AddProjectPath { .. } | C::AddProjectPick | C::AddProjectSubmit | C::AddProjectKey { .. } | C::AddProjectAccept { .. } => {
            projects::reduce_project_command(state, command).into()
        }

        C::BrowserOpen { .. } | C::BrowserNewTab | C::BrowserDecide { .. } | C::BrowserSelectTab { .. } | C::BrowserCloseTab { .. } | C::BrowserGo { .. } | C::BrowserReload { .. } | C::BrowserAct { .. } | C::BrowserClearData => {
            devices::reduce_browser(state, command).into()
        }
        C::BrowserImportRead | C::BrowserImportChoose { .. } | C::BrowserImportToggle { .. } | C::BrowserImportFilter { .. } | C::BrowserImportRun | C::BrowserImportAgain => devices::reduce_browser_import(state, &command.clone().into()).into(),
        C::SimulatorOpen { .. } | C::SimulatorRefresh | C::SimulatorSelect { .. } | C::SimulatorInput { .. } => devices::reduce_simulator(state, &command.clone().into()).into(),
        C::DictationStart | C::DictationHandsFree | C::DictationStop { .. } | C::DictationCancel | C::DictationDownload | C::DictationCancelDownload => devices::reduce_dictation(state, &command.clone().into()).into(),
        C::ComputerUseRead | C::ComputerUseEnable { .. } | C::ComputerUseRestart => devices::reduce_computer_use(state, &command.clone().into()).into(),
        C::UsageRead { .. } | C::UsageDismissWarning { .. } => devices::reduce_plan_usage(state, &command.clone().into()).into(),
        C::AppCheckForUpdates { .. } | C::AppDownloadUpdate | C::AppInstallUpdate | C::AppUpdate { .. } | C::AppCancelUpdate { .. } => devices::reduce_app_update(state, &command.clone().into()).into(),
        C::PullRequestRead { .. } | C::PullRequestReadMembers { .. } | C::PullRequestReadMember { .. } => pull_requests::reduce_pull_requests(state, &command.clone().into()).into(),
        C::TaskMoveWorktree { .. }
        | C::WorktreeOpenThread { .. }
        | C::MoveWorktree { .. }
        | C::TaskSetWorktree { .. }
        | C::TaskSetBranch { .. }
        | C::TaskCheckoutBranch { .. }
        | C::WorktreeFilterProject { .. }
        | C::WorktreeConfirmDelete { .. }
        | C::WorktreeConfirmDeleteSafe { .. }
        | C::WorktreeSetMissingOpen { .. }
        | C::WorktreeSetThreadsOpen { .. }
        | C::WorktreeRefresh
        | C::WorktreeReveal { .. }
        | C::WorktreeDelete { .. }
        | C::WorktreeDeleteSafe { .. }
        | C::WorktreeMenuOpen { .. }
        | C::WorktreeMenuSearch { .. } => worktrees::reduce_worktrees(state, &command.clone().into()).into(),
        C::TaskSetCoordinator { .. }
        | C::TaskSetLeadAway { .. }
        | C::TaskResolve { .. }
        | C::TaskReopen { .. }
        | C::CoordinationSetThreadModel { .. }
        | C::CoordinationSetMuted { .. }
        | C::MemorySetPurpose { .. }
        | C::MemoryAdd { .. }
        | C::MemoryEdit { .. }
        | C::MemoryForget { .. }
        | C::DecisionAnswer { .. }
        | C::CoordinationNextStep { .. }
        | C::CoordinationStopAll { .. } => coordination::reduce_coordination(state, &command.clone().into()).into(),
        C::ProposalStart { .. } | C::ProposalDismiss { .. } => proposals::reduce_proposals(state, &command.clone().into()).into(),
        C::AutomationNotify { task_id, headline, detail, key } => automations::raised_finding(state, task_id, headline, detail.as_deref(), key.as_deref()).into(),
        C::AutomationNothingToReport { task_id, checked } => automations::nothing_to_report(state, task_id, checked, crate::util::now()).into(),
        C::ComputersDiscover | C::ComputersPair { .. } | C::ComputersCancelPairing | C::ComputersForget { .. } | C::ComputersReconnect { .. } | C::ComputersRename { .. } | C::ComputersLabel { .. } | C::ComputersFilter { .. } => {
            computers::reduce_computer_command(state, command).into()
        }

        _ => misc::reduce_command(state, command).into(),
    }
}

fn apply_event(state: &mut WorkspaceState, event: &WorkspaceEvent) -> Applied {
    use WorkspaceEvent as E;
    match event {
        E::SnoozesElapsed { .. } | E::LimitsElapsed { .. } | E::StorePersisted => Step::none().into(),
        E::TaskResultCollected { .. } | E::TitleSuggested { .. } => threads::reduce_thread_event(state, event).into(),
        E::RunResolved { .. } | E::RunEvent { .. } => runs::reduce_runs(state, event),
        E::RunUnresolved { pending_id, message } => runs::reduce_run_unresolved(state, pending_id, message).into(),
        E::ThreadEvent { event } => runs::thread_event(state, event).into(),
        E::StoreLoaded { .. } | E::StoreThreadLoaded { .. } | E::StoreAbsent | E::StoreFailed { .. } | E::PreferencesLoaded { .. } | E::ViewClosed => store::reduce_store(state, event).into(),
        E::FindResults { .. } => view::find_results(state, event).into(),
        E::ShortcutCaptured { binding } => settings::shortcut_captured(state, binding.clone()).into(),
        E::ShortcutUnavailable { refusal } => settings::shortcut_unavailable(state, refusal).into(),
        E::SubagentActivityLoaded { task_id, subagent_id, activity } => settings::subagent_activity_loaded(state, task_id, subagent_id, activity).into(),
        E::ProjectOpened { .. } | E::ProjectRegistered { .. } | E::ProjectRegisterFailed { .. } | E::ProjectAdded { .. } | E::ProjectAddFinished { .. } | E::ProjectDirectories { .. } | E::ProjectPathPicked { .. } => {
            projects::reduce_project_event(state, event).into()
        }
        E::CoordinationReported { .. } | E::CoordinationDecisionRaised { .. } | E::CoordinationRemembered { .. } => coordination::reduce_coordination(state, &event.clone().into()).into(),
        E::CoordinationProposed { .. } | E::ProposalSettled { .. } => proposals::reduce_proposals(state, &event.clone().into()).into(),
        E::BrowserUpdated { page } => devices::browser_updated(state, page).into(),
        E::BrowserImportSources { .. } | E::BrowserImportSites { .. } | E::BrowserImportDone { .. } | E::BrowserImportFailed { .. } => devices::reduce_browser_import(state, &event.clone().into()).into(),
        E::SimulatorListed { .. } | E::SimulatorBooted { .. } | E::SimulatorFailed { .. } | E::SimulatorRefused { .. } | E::SimulatorTurned { .. } | E::SimulatorIdle { .. } => devices::reduce_simulator(state, &event.clone().into()).into(),
        E::DictationModel { .. } | E::DictationStarted { .. } | E::DictationTranscribed { .. } | E::DictationFailed { .. } => devices::reduce_dictation(state, &event.clone().into()).into(),
        E::ComputerUsePermissions { .. } | E::ComputerUseFailed { .. } => devices::reduce_computer_use(state, &event.clone().into()).into(),
        E::UsageReported { .. } => devices::reduce_plan_usage(state, &event.clone().into()).into(),
        E::AppUpdateChanged { .. } | E::AppBuild { .. } | E::AppLaunched { .. } => devices::reduce_app_update(state, &event.clone().into()).into(),
        E::PullRequestAnswered { .. } => pull_requests::reduce_pull_requests(state, &event.clone().into()).into(),
        E::CheckoutFinished { .. } | E::WorktreeCreated { .. } | E::WorktreeReleased { .. } | E::WorktreeDeleted { .. } | E::WorktreeFailed { .. } | E::WorktreeReleaseFailed { .. } | E::WorktreesLoaded { .. } | E::WorktreesFailed { .. } => {
            worktrees::reduce_worktrees(state, &event.clone().into()).into()
        }
        E::AutomationFired { fire } => automations::automation_fired(state, fire).into(),
        E::AutomationsChanged { automations } => automations::automations_changed(state, automations).into(),
        E::ComputersChanged { .. } | E::ComputersFound { .. } | E::ComputersSearchFailed { .. } | E::ComputersPairFailed { .. } | E::ComputerState { .. } | E::ComputerNotice { .. } | E::ComputersForwarded { .. } | E::ComputersAnswered { .. } => {
            computers::reduce_computer_event(state, event).into()
        }
        _ => misc::reduce_event(state, event).into(),
    }
}

fn combine(previous: Transition, next: Transition) -> Transition {
    let mut effects = previous.effects;
    effects.extend(next.effects);
    let result = match (&previous.result, &next.result) {
        (Some(result), _) if !result.is_ok() => previous.result,
        (Some(_), None) => previous.result,
        _ => next.result,
    };
    Transition { effects, result }
}

fn run_commands(state: &mut WorkspaceState, commands: Vec<AppCommand>) -> Transition {
    let mut combined = Transition::default();
    for command in commands {
        let next = reduce_transition(state, command);
        combined = combine(combined, next);
    }
    combined
}

/// Forks under the typed name.
fn confirm_fork(state: &mut WorkspaceState, typed: &str) -> Transition {
    let Some(ask) = state.fork_name.clone() else { return Transition::default() };
    let title = clamp_title(typed);
    if title.is_empty() {
        return Transition::default();
    }
    state.fork_name = None;
    let fork = AppCommand::TaskFork { task_id: Some(ask.task_id.clone()), worktree: ask.worktree.then_some(true), title: Some(title) };
    let holder = crate::view::computers::computer_of_thread(state, Some(&ask.task_id));
    let named = match holder {
        Some(holder) if !crate::validation::supports_computer_command(holder.link.capabilities.as_deref(), &fork) => AppCommand::TaskFork { task_id: Some(ask.task_id), worktree: ask.worktree.then_some(true), title: None },
        _ => fork,
    };
    reduce_transition(state, named)
}

/// What Esc means: the nearest layer claims it first, and only when none is open does it reach a run.
pub fn escape_commands(state: &WorkspaceState) -> Vec<AppCommand> {
    if state.project_add.is_some() {
        return vec![AppCommand::AddProjectKey { key: crate::commands::ProjectAddKey::Escape }];
    }
    if state.viewing_image.is_some() {
        return vec![AppCommand::ImageClose];
    }
    if state.jump.is_some() {
        return vec![AppCommand::JumpClose];
    }
    if state.open_menu.is_some() {
        return vec![AppCommand::SetMenu { menu: None }];
    }
    if state.settings_open || state.computer_use_setup {
        return vec![AppCommand::SetSettingsOpen { open: false, section: None, setting_id: None }];
    }
    if state.find.is_some() {
        return vec![AppCommand::FindClose];
    }
    vec![AppCommand::RunCancel { task_id: keyboard_thread_id(state) }]
}

/// The project a new thread starts in: the one the thread on screen is in, else the one being drafted.
fn current_project_id(state: &WorkspaceState) -> Option<String> {
    let shown = state.selected_computer().and_then(|computer| computer.state.as_deref()).unwrap_or(state);
    match &shown.current_id {
        Some(current) => shown.thread(current).and_then(|thread| thread.project_id.clone()),
        None => shown.draft_project_id.clone(),
    }
}

/// What a bound keystroke means.
pub fn shortcut_commands(state: &WorkspaceState, action: &str, surface: ShortcutSurface) -> Vec<AppCommand> {
    let project_id = current_project_id(state);
    let new_thread = AppCommand::TaskNew { project_id, worktree_id: None };
    let leaving: Vec<AppCommand> = if state.settings_open || state.computer_use_setup { vec![AppCommand::SetSettingsOpen { open: false, section: None, setting_id: None }] } else { Vec::new() };
    let leaving_zen: Vec<AppCommand> = if state.zen { vec![AppCommand::SetZen { zen: false }] } else { Vec::new() };
    let jump_close: Vec<AppCommand> = if state.jump.is_some() { vec![AppCommand::JumpClose] } else { Vec::new() };
    let join = |parts: Vec<Vec<AppCommand>>| parts.into_iter().flatten().collect::<Vec<_>>();
    if let Some(slot) = slot_shortcut_index(action) {
        if state.keyboard_tab.is_some() {
            return vec![AppCommand::SelectDockIndex { index: if slot == 8 { -1 } else { slot as i64 } }];
        }
        let Some(thread_id) = thread_slots(state).get(slot).cloned() else { return Vec::new() };
        return join(vec![leaving, jump_close, vec![AppCommand::TaskSelect { task_id: thread_id }]]);
    }
    match action {
        "app.check-for-updates" => vec![AppCommand::AppCheckForUpdates { quiet: false }],
        "app.open-source-licenses" => vec![AppCommand::AppOpenSourceLicenses],
        "thread.new" => join(vec![leaving, vec![new_thread]]),
        "thread.new-pick-project" => join(vec![leaving, vec![new_thread, AppCommand::FocusComposer { recover: None }, AppCommand::SetMenu { menu: Some(THREAD_START_PROJECT_MENU.into()) }]]),
        "run.cancel" => vec![AppCommand::RunCancel { task_id: None }],
        "run.allow" | "run.deny" => {
            let task_id = keyboard_thread_id(state).or_else(|| state.current_id.clone());
            let approval = task_id.as_deref().and_then(|id| state.active_runs.get(id)).and_then(|active| state.approvals.get(&active.run_id));
            match approval {
                Some(approval) => vec![AppCommand::RunDecide { task_id: approval.task_id.clone(), run_id: approval.run_id.clone(), approval_id: approval.approval_id.clone(), allow: action == "run.allow" }],
                None => Vec::new(),
            }
        }
        "composer.focus" => {
            let mut commands = Vec::new();
            if state.open_menu.is_some() {
                commands.push(AppCommand::SetMenu { menu: None });
            }
            commands.push(AppCommand::FocusComposer { recover: None });
            commands
        }
        "effort.increase" | "effort.decrease" => step_effort(state, action == "effort.increase"),
        "thread.jump" => {
            if state.jump.is_some() {
                vec![AppCommand::JumpClose]
            } else {
                join(vec![leaving, vec![AppCommand::JumpOpen]])
            }
        }
        "project.start" => join(vec![leaving, jump_close, vec![AppCommand::ProjectOpen { start: true }]]),
        "find.open" => vec![if state.find.is_some() { AppCommand::FindOpen { target: None } } else { AppCommand::FindOpen { target: Some(crate::view::find::find_target_for(state, surface)) } }],
        "find.next" | "find.previous" => {
            let delta = if action == "find.next" { 1 } else { -1 };
            vec![if state.find.is_some() { AppCommand::FindStep { delta } } else { AppCommand::FindOpen { target: Some(crate::view::find::find_target_for(state, surface)) } }]
        }
        "nav.back" => {
            if surface == ShortcutSurface::Browser {
                vec![AppCommand::BrowserGo { task_id: None, tab_id: None, delta: -1 }]
            } else {
                join(vec![leaving, vec![AppCommand::GoBack]])
            }
        }
        "nav.forward" => {
            if surface == ShortcutSurface::Browser {
                vec![AppCommand::BrowserGo { task_id: None, tab_id: None, delta: 1 }]
            } else {
                join(vec![leaving, vec![AppCommand::GoForward]])
            }
        }
        "page.reload" => vec![AppCommand::BrowserReload { task_id: None, tab_id: None }],
        "tab.new" => join(vec![leaving_zen, vec![AppCommand::NewTab]]),
        "terminal.focus" => {
            let shown = state.selected_computer().and_then(|computer| computer.state.as_deref()).unwrap_or(state);
            if keyboard_terminal_id(shown).is_some() {
                return vec![AppCommand::SetDockOpen { open: false }, AppCommand::FocusComposer { recover: None }];
            }
            let owner = dock_owner(shown, None);
            let latest = dock_for(shown, &owner).terminals.last().map(|terminal| terminal.id.clone());
            let open = match latest {
                Some(terminal_id) => AppCommand::TerminalSelect { terminal_id },
                None => AppCommand::TerminalOpen { cwd: None },
            };
            join(vec![leaving, leaving_zen, vec![open]])
        }
        "tab.close" => vec![AppCommand::CloseTab],
        "dock.toggle" => {
            let open = state.zen || !front_dock(state).1.open;
            join(vec![leaving_zen, vec![AppCommand::SetDockOpen { open }]])
        }
        "dock.expand" => {
            let expanded = !front_dock(state).1.expanded;
            join(vec![leaving_zen, vec![AppCommand::SetDockExpanded { expanded }]])
        }
        "sidebar.toggle" => {
            if state.zen {
                join(vec![leaving_zen, vec![AppCommand::SetSidebarOpen { open: true }]])
            } else {
                vec![AppCommand::SetSidebarOpen { open: !state.sidebar_open }]
            }
        }
        "zen.toggle" => {
            if state.zen {
                leaving_zen
            } else if zen_allowed(state) {
                join(vec![leaving, jump_close, vec![AppCommand::SetZen { zen: true }]])
            } else {
                Vec::new()
            }
        }
        "settings.toggle" => vec![AppCommand::SetSettingsOpen { open: !state.settings_open, section: None, setting_id: None }],
        _ => Vec::new(),
    }
}

/// The effort one step up or down from what the thread with the keys, or the draft, runs on.
fn step_effort(state: &WorkspaceState, increase: bool) -> Vec<AppCommand> {
    let shown = state.selected_computer().and_then(|computer| computer.state.as_deref());
    let source = shown.unwrap_or(state);
    let tabbed = match shown {
        Some(shown) => {
            let mut probe = shown.clone();
            probe.keyboard_tab = state.keyboard_tab.clone();
            keyboard_thread_id(&probe)
        }
        None => keyboard_thread_id(state),
    };
    let thread = source.thread(tabbed.as_deref().or(source.current_id.as_deref()).unwrap_or(""));
    let engine = thread.map(|thread| thread.engine).unwrap_or(source.draft_engine);
    let catalog = source.catalog();
    let model = match thread {
        Some(thread) => thread_model(&catalog, thread.engine, thread.model.as_deref()),
        None => Some(source.draft_model.clone()),
    };
    let effort = thread.map(|thread| thread.effort.unwrap_or(default_effort_for(engine))).unwrap_or(source.draft_effort);
    let role = effort_role_of(thread, source.draft_role);
    let Some(model) = model else { return Vec::new() };
    let choices = efforts_for(&catalog, engine, &model, role);
    let current = effort_for_model(&catalog, engine, &model, effort, role);
    let Some(index) = choices.iter().position(|choice| choice.id == current) else { return Vec::new() };
    let next = if increase { index.checked_sub(1) } else { Some(index + 1) };
    let Some(next) = next.and_then(|next| choices.get(next)) else { return Vec::new() };
    vec![AppCommand::TaskSetEffort { task_id: tabbed, engine, effort: next.id }]
}

pub fn browser_permissions(state: &WorkspaceState) -> crate::domain::platform::BrowserPermissions {
    crate::domain::platform::BrowserPermissions {
        origins: state.browser_origins.clone(),
        autonomous_task_ids: state.threads.iter().filter(|thread| thread.execution_policy == ExecutionPolicy::Autonomous).map(|thread| thread.id.clone()).collect(),
    }
}

/// Remembers where the app took the user, dropping the forward entries the way a browser does.
pub fn record_visit(state: &mut WorkspaceState, thread_id: &str) {
    let keep = (state.history_index + 1).max(0) as usize;
    state.history.truncate(keep.min(state.history.len()));
    if state.history.last().map(String::as_str) != Some(thread_id) {
        state.history.push(thread_id.to_string());
    }
    state.history_index = state.history.len() as i64 - 1;
}

/// Where the cursor lands moving `step` through history, stepping over threads that are gone or archived.
pub fn reachable_visit(state: &WorkspaceState, step: i64) -> Option<usize> {
    let mut index = state.history_index + step;
    while index >= 0 && (index as usize) < state.history.len() {
        let id = &state.history[index as usize];
        if state.threads.iter().any(|thread| &thread.id == id && thread.archived_at.is_none()) {
            return Some(index as usize);
        }
        index += step;
    }
    None
}

/// Deadlines are durable; expiry and the one timer watching them belong to the reducer.
fn reconcile_snoozes(before: &Before, state: &mut WorkspaceState, input: &WorkspaceInput, transition: &mut Transition) {
    let check = matches!(input, WorkspaceInput::Event(WorkspaceEvent::SnoozesElapsed { .. } | WorkspaceEvent::StoreLoaded { .. } | WorkspaceEvent::StoreAbsent))
        || matches!(input, WorkspaceInput::Command(AppCommand::SetFocused { focused: true }));
    let mut next = next_snooze_expiry(&state.threads);
    if check {
        if let Some(due) = next {
            let at = match input {
                WorkspaceInput::Event(WorkspaceEvent::SnoozesElapsed { at }) => *at,
                _ => crate::util::now(),
            };
            if due <= at {
                for thread in state.threads.iter_mut() {
                    if thread.snoozed_until.is_some_and(|until| until <= at) {
                        thread.snoozed_until = None;
                    }
                }
                next = next_snooze_expiry(&state.threads);
            }
        }
    }
    if next == before.snooze_expiry && !(check && next.is_some()) {
        return;
    }
    transition.effects.push(EffectWork::ScheduleSnoozeExpiry { at: next }.into());
}

/// Projects opened only to start a thread join the sidebar once a thread is there, and go once nothing holds them.
fn settled_unlisted(before: &Before, state: &mut WorkspaceState) {
    let projects_same = state.projects.len() == before.projects.len() && state.projects.iter().zip(&before.projects).all(|(project, (id, unlisted))| &project.id == id && project.unlisted == *unlisted);
    if projects_same && state.threads.len() == before.threads_len && state.draft_project_id == before.draft_project_id {
        let unchanged_threads = true;
        if unchanged_threads && !state.projects.iter().any(|project| project.unlisted) {
            return;
        }
    }
    let unlisted: Vec<String> = state.projects.iter().filter(|project| project.unlisted).map(|project| project.id.clone()).collect();
    if unlisted.is_empty() {
        return;
    }
    let listed: Vec<String> = unlisted.iter().filter(|id| state.threads.iter().any(|thread| thread.project_id.as_deref() == Some(id.as_str()))).cloned().collect();
    let dropped: Vec<String> = unlisted.iter().filter(|id| !listed.contains(id) && state.draft_project_id.as_deref() != Some(id.as_str())).cloned().collect();
    if listed.is_empty() && dropped.is_empty() {
        return;
    }
    let drops_last = state.projects.iter().any(|project| dropped.contains(&project.id) && state.last_folder.as_deref().is_some_and(|folder| crate::domain::project::same_root(&project.root, folder)));
    state.projects.retain(|project| !dropped.contains(&project.id));
    for project in state.projects.iter_mut() {
        if listed.contains(&project.id) {
            project.unlisted = false;
        }
    }
    if drops_last {
        state.last_folder = None;
    }
}

/// A draft or an empty thread has no conversation to focus on, so only a thread with messages may enter zen mode.
pub fn zen_allowed(state: &WorkspaceState) -> bool {
    let shown = state.selected_computer().and_then(|computer| computer.state.as_deref()).unwrap_or(state);
    shown.current_thread().is_some_and(|thread| !thread.messages.is_empty())
}

fn shown_thread(state: &WorkspaceState) -> String {
    let remote = state.selected_computer().and_then(|computer| computer.state.as_deref()).and_then(|shown| shown.current_id.clone()).unwrap_or_default();
    format!("{}\u{0}{}\u{0}{remote}", state.computers.active.clone().unwrap_or_default(), state.current_id.clone().unwrap_or_default())
}

fn zen_dock_open(state: &WorkspaceState) -> bool {
    match state.selected_computer().and_then(|computer| computer.state.as_deref()) {
        Some(shown) => front_dock(shown).1.open,
        None => front_dock(state).1.open,
    }
}

/// Zen mode belongs to the thread it was entered on, and opening anything it hides ends it.
fn settled_zen(before: &Before, state: &mut WorkspaceState) {
    if !state.zen || !before.zen {
        return;
    }
    let left = before.shown_thread != shown_thread(state);
    let revealed = (!before.sidebar_open && state.sidebar_open) || (!before.session_panel_open && state.session_panel_open) || (!before.dock_open && zen_dock_open(state));
    if left || revealed || !zen_allowed(state) {
        state.zen = false;
    }
}

/// The archive question never outlives the thread it asks about.
fn settled_archive_prompt(state: &mut WorkspaceState) {
    if let Some(task_id) = state.archive_prompt.clone() {
        if crate::view::dialogs::archive_prompt_view(state, Some(&task_id)).is_none() {
            state.archive_prompt = None;
        }
    }
}
