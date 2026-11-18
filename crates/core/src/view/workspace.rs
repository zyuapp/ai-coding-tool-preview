//! Everything the window reads, derived in one place so views never reach into raw state for
//! anything that takes a rule to work out. Direct settings and chrome are read from `window`.

use std::collections::{HashMap, HashSet};

use crate::domain::automation::AutomationView;
use crate::domain::coordination::{effort_role_of, is_coordinator, OverviewFolds};
use crate::domain::engine::{capabilities_for, default_effort_for, effort_for_model, engine_label, thread_model, AgentEngine, EngineCapabilities, EngineReadiness, ModelCatalog, AGENT_ENGINES};
use crate::domain::platform::{AppBuild, ComputerLink, ComputerStatus, DictationPhase, SimulatorAvailability, SimulatorState};
use crate::domain::project::{project_name, Project};
use crate::domain::pull_request::PullRequestAnswer;
use crate::domain::question::PendingQuestion;
use crate::domain::run::{ActiveGoal, BackgroundProcess, RetryNotice, Subagent};
use crate::domain::thread::Thread;
use crate::domain::usage_limit::{line_positions, LimitPause};
use crate::domain::workflow::Workflow;
use crate::domain::worktree::{worktree_name, Worktree};
use crate::events::ChangedFilesResult;
use crate::reducer::sending::prompt_key;
use crate::state::{dock_for, dock_owner, dock_side_chats, dock_subject, queued_view, workflow_by_id, ApprovalView, DiffState, PairedComputer, QueuedMessageView, ReadingPoint, RunPhase, StreamingTail, ThreadDock, ThreadRunStatus, WorkspaceState, AUTOMATION_PANEL, DIFF_PANEL};
use crate::view::computers::{shown_computers, thread_host_of};
use crate::view::coordination::{coordination_view, CoordinationView};
use crate::view::dock::{side_chat_view, thread_tab_view, worktree_choices, worktree_menu_view, SideChatView, ThreadTabView, WorktreeChoice, WorktreeMenuView};
use crate::view::pull_request::{member_pull_requests_view, pull_request_for, MemberPullRequestsView};
use crate::view::settings::{computer_upkeep, computer_usage, member_worktrees_view, project_settings_rows, ComputerUpkeep, ComputerUsage, MemberWorktree, ProjectSettingsRow};
use crate::view::sidebar::{order_projects, sidebar_lists, SidebarLists, ThreadHost};
use crate::view::worktrees::{worktree_settings_page, worktree_settings_views, WorktreeSettingsPage, WorktreeSettingsView};
use crate::workspace::activity::{doing_work, run_status, thread_activity, thread_lists, waiting_on, ThreadActivity, ThreadWait};
use crate::workspace::coordination::stoppable_coordinators;
use crate::workspace::location::{location_of, project_for, thread_workspace_id, worktree_by_id, ThreadLocation};

/// Everything the sidebar needs from the paired computers the filter shows.
#[derive(Clone, Debug, Default)]
pub struct RemoteCollections<'a> {
    pub threads: Vec<&'a Thread>,
    pub projects: Vec<&'a Project>,
    pub worktrees: Vec<&'a Worktree>,
    pub worktree_thread_ids: HashSet<String>,
    pub busy: HashSet<String>,
    pub monitoring: HashSet<String>,
    pub ranked: HashSet<String>,
    pub blocked: HashSet<String>,
    /// The coordinators with work going that Stop all would stop.
    pub stoppable: HashSet<String>,
    pub thread_hosts: HashMap<String, ThreadHost>,
    pub project_hosts: HashMap<String, ThreadHost>,
    pub side_chat_attention: HashSet<String>,
    pub unread_count: usize,
}

/// Which of a paired computer's threads are working, as it says; one whose line is down claims none.
pub fn remote_activity(computer: &PairedComputer, holder: &WorkspaceState) -> ThreadActivity {
    let mut activity = thread_activity(holder);
    if computer.link.status != ComputerStatus::Connected {
        activity.working.clear();
        activity.monitoring.clear();
    }
    activity
}

pub fn remote_collections(state: &WorkspaceState) -> RemoteCollections<'_> {
    let mut gathered = RemoteCollections::default();
    for computer in shown_computers(state) {
        let Some(remote) = computer.state.as_deref() else { continue };
        let live = computer.link.status == ComputerStatus::Connected;
        let host = thread_host_of(computer);
        let lists = thread_lists(remote);
        for thread in &lists.visible_threads {
            gathered.threads.push(thread);
            gathered.thread_hosts.insert(thread.id.clone(), host.clone());
        }
        for project in &remote.projects {
            gathered.projects.push(project);
            gathered.project_hosts.insert(project.id.clone(), host.clone());
        }
        gathered.worktrees.extend(remote.worktrees.iter());
        let activity = remote_activity(computer, remote);
        gathered.worktree_thread_ids.extend(lists.worktree_thread_ids.iter().cloned());
        gathered.busy.extend(activity.working.iter().cloned());
        gathered.monitoring.extend(activity.monitoring.iter().cloned());
        if live {
            gathered.stoppable.extend(stoppable_coordinators(remote));
            gathered.unread_count += lists.unread_count;
        }
        gathered.ranked.extend(activity.ranked.iter().cloned());
        gathered.blocked.extend(activity.blocked.iter().cloned());
        gathered.side_chat_attention.extend(lists.side_chat_attention.iter().cloned());
    }
    gathered
}

/// How many threads on the paired computers the sidebar shows carry an unseen mark.
pub fn remote_unread_count(state: &WorkspaceState) -> usize {
    shown_computers(state).iter().filter(|computer| computer.link.status == ComputerStatus::Connected).filter_map(|computer| computer.state.as_deref()).map(|remote| thread_lists(remote).unread_count).sum()
}

/// A checkout with the threads working in it.
#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeGroup<'a> {
    pub worktree: &'a Worktree,
    pub threads: Vec<&'a Thread>,
}

/// The lists that span the workspace, which only change when threads, projects or runs do.
pub struct WorkspaceCollections<'a> {
    pub listed_threads: Vec<&'a Thread>,
    pub visible_threads: Vec<&'a Thread>,
    pub archived_threads: Vec<&'a Thread>,
    pub worktree_thread_ids: HashSet<String>,
    pub side_chat_attention: HashSet<String>,
    pub unread_count: usize,
    pub lists: SidebarLists<'a>,
    /// Every computer's projects, this one's first, for a draft to start in.
    pub start_projects: Vec<Project>,
    pub project_settings: Vec<ProjectSettingsRow<'a>>,
    pub busy: HashSet<String>,
    pub blocked: HashSet<String>,
    pub every_busy: HashSet<String>,
    pub every_ranked: HashSet<String>,
    pub every_monitoring: HashSet<String>,
    pub every_blocked: HashSet<String>,
    pub every_stoppable: HashSet<String>,
    pub remote: RemoteCollections<'a>,
    pub computer_links: Vec<ComputerLink>,
    pub computer_usage: Vec<ComputerUsage<'a>>,
    pub computer_upkeep: Vec<ComputerUpkeep>,
    pub managed_worktrees: Option<Vec<WorktreeSettingsView>>,
    pub worktree_settings: WorktreeSettingsPage,
    pub worktree_groups: Vec<WorktreeGroup<'a>>,
    pub worktrees_by_id: HashMap<String, &'a Worktree>,
    pub schedules: HashMap<String, &'a AutomationView>,
    /// The coordinators a thread can be moved under, by the computer that holds them: this one's under None.
    pub coordinators_by_computer: Vec<(Option<String>, Vec<&'a Thread>)>,
}

fn union(own: &HashSet<String>, others: &HashSet<String>) -> HashSet<String> {
    own.union(others).cloned().collect()
}

pub fn workspace_collections(state: &WorkspaceState) -> WorkspaceCollections<'_> {
    let activity = thread_activity(state);
    let lists = thread_lists(state);
    let remote = remote_collections(state);
    let own = state.computers.filter == "all" || state.computers.filter == "this";
    let mut projects: Vec<Project> = if own { state.projects.clone() } else { Vec::new() };
    projects.extend(remote.projects.iter().map(|project| (*project).clone()));
    let mut threads: Vec<&Thread> = if own { lists.visible_threads.clone() } else { Vec::new() };
    threads.extend(remote.threads.iter().copied());
    let every_ranked = union(&activity.ranked, &remote.ranked);
    let every_blocked = union(&activity.blocked, &remote.blocked);
    let sidebar = sidebar_lists(state, &projects, &threads, &every_ranked, &every_blocked, Some(&remote.project_hosts));
    let mut start_projects = order_projects(&state.projects);
    for computer in &state.computers.paired {
        if let (Some(shown), ComputerStatus::Connected) = (computer.state.as_deref(), computer.link.status) {
            let listed: Vec<Project> = shown.projects.iter().filter(|project| !project.unlisted || Some(&project.id) == shown.draft_project_id.as_ref()).cloned().collect();
            start_projects.extend(order_projects(&listed));
        }
    }
    let managed = worktree_settings_views(state, &activity.working);
    let settings = worktree_settings_page(state, managed.as_deref());
    let mut by_worktree: HashMap<&str, Vec<&Thread>> = HashMap::new();
    for thread in &sidebar.ordered_threads {
        if let Some(worktree_id) = thread.worktree_id.as_deref() {
            by_worktree.entry(worktree_id).or_default().push(thread);
        }
    }
    let worktree_groups = state.worktrees.iter().chain(remote.worktrees.iter().copied()).map(|worktree| WorktreeGroup { worktree, threads: by_worktree.get(worktree.id.as_str()).cloned().unwrap_or_default() }).collect();
    let worktrees_by_id = state.worktrees.iter().chain(remote.worktrees.iter().copied()).map(|worktree| (worktree.id.clone(), worktree)).collect();
    let mut coordinators_by_computer: Vec<(Option<String>, Vec<&Thread>)> = vec![(None, lists.listed_threads.iter().copied().filter(|thread| is_coordinator(Some(thread))).collect())];
    for computer in &state.computers.paired {
        if let Some(held) = computer.state.as_deref() {
            coordinators_by_computer.push((Some(computer.link.id.clone()), thread_lists(held).listed_threads.into_iter().filter(|thread| is_coordinator(Some(thread))).collect()));
        }
    }
    let stoppable = stoppable_coordinators(state);
    WorkspaceCollections {
        worktree_thread_ids: union(&lists.worktree_thread_ids, &remote.worktree_thread_ids),
        side_chat_attention: union(&lists.side_chat_attention, &remote.side_chat_attention),
        unread_count: lists.unread_count + remote.unread_count,
        lists: sidebar,
        start_projects,
        project_settings: project_settings_rows(state),
        every_busy: union(&activity.working, &remote.busy),
        every_ranked,
        every_monitoring: union(&activity.monitoring, &remote.monitoring),
        every_blocked,
        every_stoppable: union(&stoppable, &remote.stoppable),
        computer_links: state.computers.paired.iter().map(|computer| ComputerLink { capabilities: None, ..computer.link.clone() }).collect(),
        computer_usage: computer_usage(&state.computers.paired),
        computer_upkeep: computer_upkeep(&state.computers.paired, state.app_build.as_ref()),
        managed_worktrees: managed.clone(),
        worktree_settings: settings,
        worktree_groups,
        worktrees_by_id,
        schedules: state.automations.iter().map(|automation| (automation.automation.task_id.clone(), automation)).collect(),
        coordinators_by_computer,
        busy: activity.working.clone(),
        blocked: activity.blocked.clone(),
        listed_threads: lists.listed_threads,
        visible_threads: lists.visible_threads,
        archived_threads: lists.archived_threads,
        remote,
    }
}

/// The main composer's dictation: whether it can, and what the recording on this draft is doing.
#[derive(Clone, Debug, PartialEq)]
pub struct DictationView {
    pub phase: Option<DictationPhase>,
    pub hands_free: bool,
    pub sends: i64,
}

pub fn dictation_view(state: &WorkspaceState) -> DictationView {
    let key = prompt_key(state);
    let here = state.dictation.session.as_ref().filter(|session| session.key == key);
    DictationView { phase: here.map(|session| session.phase), hands_free: here.is_some_and(|session| session.hands_free && session.phase != DictationPhase::Transcribing), sends: state.dictation.sends }
}

/// The usage limit the thread is waiting out, and its place in line.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitPauseView<'a> {
    pub pause: &'a LimitPause,
    pub position: Option<usize>,
}

/// What the thread on screen shows: everything the window draws that belongs to the computer holding
/// it. For a paired computer's thread it is read from that computer's state.
pub struct ThreadView<'a> {
    pub current_thread: Option<&'a Thread>,
    pub current_project: Option<&'a Project>,
    pub folder: String,
    pub folder_label: String,
    pub engine: AgentEngine,
    pub engine_label: &'static str,
    /// A thread exists from its first message on, and keeps the engine that message went to.
    pub engine_locked: bool,
    pub engine_access: Vec<(AgentEngine, EngineReadiness)>,
    pub capabilities: EngineCapabilities,
    pub models: std::borrow::Cow<'static, ModelCatalog>,
    pub subagents: &'a [Subagent],
    pub workflows: &'a [Workflow],
    pub model: Option<String>,
    pub coordinating: bool,
    pub effort: crate::domain::run::AgentEffort,
    pub policy: crate::domain::run::ExecutionPolicy,
    pub fast_mode: bool,
    pub goal: Option<&'a ActiveGoal>,
    pub status: ThreadRunStatus,
    pub compacting: bool,
    pub retrying: Option<&'a RetryNotice>,
    pub limit_pause: Option<LimitPauseView<'a>>,
    pub limit_positions: HashMap<String, usize>,
    pub run_active: bool,
    pub question: Option<&'a PendingQuestion>,
    pub queued_messages: Vec<QueuedMessageView>,
    pub approval: Option<&'a ApprovalView>,
    pub background_processes: &'a [BackgroundProcess],
    pub inspected_workflow: Option<&'a Workflow>,
    pub streaming_tail: Option<&'a StreamingTail>,
    pub automation: Option<&'a AutomationView>,
    pub last_found_at: Option<i64>,
    pub last_checked: Option<&'a crate::domain::finding::LastChecked>,
    pub location: ThreadLocation,
    pub worktree_menu: Option<WorktreeMenuView>,
    pub waiting_on: Option<ThreadWait>,
    pub can_compact: bool,
    pub can_review: bool,
    pub workspace_id: Option<String>,
    pub draft_worktree_name: Option<String>,
    pub draft_worktree_choices: Vec<WorktreeChoice>,
    pub environment: Option<&'a ChangedFilesResult>,
    pub pull_request: PullRequestAnswer,
    pub dock_owner: String,
    pub dock: &'a ThreadDock,
    pub review_subject: Option<String>,
    pub automation_subject: Option<String>,
    pub diff: &'a DiffState,
    pub current_folder: Option<String>,
    pub side_chats: Vec<SideChatView<'a>>,
    pub thread_tabs: Vec<ThreadTabView<'a>>,
    pub coordination: CoordinationView<'a>,
    pub member_pull_requests: MemberPullRequestsView<'a>,
    pub member_worktrees: Vec<MemberWorktree<'a>>,
    pub zen_available: bool,
    pub review_picker: Option<&'a crate::state::ReviewPicker>,
}

/// What the thread on screen in `shown` reads. `window` holds the menus and searches drawn over it.
pub fn thread_view<'a>(shown: &'a WorkspaceState, window: &WorkspaceState) -> ThreadView<'a> {
    let current = shown.current_thread();
    let activity = thread_activity(shown);
    let catalog = shown.catalog();
    let draft_worktree = worktree_by_id(shown, shown.draft_worktree_id.as_deref());
    let current_project = match current {
        Some(thread) => project_for(shown, Some(thread)),
        None => shown.draft_project_id.as_deref().and_then(|id| shown.project(id)),
    };
    let engine = current.map(|thread| thread.engine).unwrap_or(shown.draft_engine);
    let capabilities = capabilities_for(engine);
    let model = match current {
        Some(thread) => thread_model(&catalog, thread.engine, thread.model.as_deref()),
        None => Some(shown.draft_model.clone()),
    };
    let role = effort_role_of(current, shown.draft_role);
    let effort = current.map(|thread| thread.effort.unwrap_or(default_effort_for(engine))).unwrap_or(shown.draft_effort);
    let effort = match &model {
        Some(model) => effort_for_model(&catalog, engine, model, effort, role),
        None => effort,
    };
    let current_run = shown.current_id.as_deref().and_then(|id| shown.active_runs.get(id));
    let workspace_id = match current {
        Some(thread) => thread_workspace_id(shown, Some(thread)),
        None => shown.draft_project_id.as_deref().and_then(|id| shown.project(id)).and_then(|project| project.workspace_id.clone()),
    };
    let environment = workspace_id.as_deref().and_then(|id| shown.environments.get(id));
    let owner = dock_owner(shown, None);
    let dock = dock_for(shown, &owner);
    let positions = line_positions(&shown.threads);
    let settled = current.filter(|_| current_run.is_none()).filter(|thread| waiting_on(shown, Some(&thread.id), &prompt_key(shown)).is_none());
    let lists = thread_lists(shown);
    let lead = current.filter(|thread| is_coordinator(Some(thread)));
    ThreadView {
        current_thread: current,
        current_project,
        folder: current_project.map(|project| project.root.clone()).unwrap_or_default(),
        folder_label: current_project.map(project_name).unwrap_or_default(),
        engine,
        engine_label: engine_label(engine),
        engine_locked: current.is_some(),
        engine_access: AGENT_ENGINES.iter().map(|engine| (*engine, shown.engine_readiness(*engine))).collect(),
        capabilities,
        subagents: if capabilities.subagents { current.and_then(|thread| shown.subagents.get(&thread.id)).map(Vec::as_slice).unwrap_or(&[]) } else { &[] },
        workflows: if capabilities.workflows { shown.current_id.as_deref().and_then(|id| shown.workflows.get(id)).map(Vec::as_slice).unwrap_or(&[]) } else { &[] },
        coordinating: role.is_some(),
        model,
        effort,
        models: catalog.clone(),
        policy: current.map(|thread| thread.execution_policy).unwrap_or(shown.draft_policy),
        fast_mode: capabilities.fast_mode && current.map(|thread| thread.fast_mode.unwrap_or(false)).unwrap_or(shown.draft_fast_mode),
        goal: shown.current_id.as_deref().and_then(|id| shown.goals.get(id)),
        status: run_status(shown, shown.current_id.as_deref()),
        compacting: current_run.is_some_and(|run| run.phase == RunPhase::Compacting),
        retrying: current_run.and_then(|run| match &run.phase {
            RunPhase::Retrying { retry } => Some(retry),
            _ => None,
        }),
        limit_pause: current.and_then(|thread| thread.limit_pause.as_ref().map(|pause| LimitPauseView { pause, position: positions.get(&thread.id).copied() })),
        limit_positions: positions,
        run_active: current_run.is_some(),
        question: current_run.and_then(|run| run.questions.as_ref()).and_then(|questions| questions.first()),
        queued_messages: shown.current_id.as_deref().and_then(|id| queued_view(shown, id)).unwrap_or_default(),
        approval: current_run.filter(|run| run.phase == RunPhase::AwaitingApproval).and_then(|run| shown.approvals.get(&run.run_id)),
        background_processes: shown.current_id.as_deref().and_then(|id| shown.background_processes.get(id)).map(Vec::as_slice).unwrap_or(&[]),
        inspected_workflow: workflow_by_id(shown, dock.workflow_id.as_deref()),
        streaming_tail: shown.current_id.as_deref().and_then(|id| shown.streaming_tails.get(id)),
        automation: shown.automations.iter().find(|automation| Some(&automation.automation.task_id) == shown.current_id.as_ref()),
        last_found_at: current.and_then(|thread| thread.last_finding_at),
        last_checked: current.and_then(|thread| thread.last_checked.as_ref()),
        location: location_of(shown, current),
        worktree_menu: worktree_menu_view(shown, current, &lists.visible_threads, window.open_menu.as_deref(), &window.worktree_menu_search),
        waiting_on: waiting_on(shown, current.map(|thread| thread.id.as_str()), &prompt_key(shown)),
        can_compact: settled.is_some_and(|thread| crate::reducer::runs::compactable_session(shown, thread)),
        can_review: settled.is_some_and(|thread| crate::reducer::runs::reviewable_session(shown, thread)),
        draft_worktree_name: draft_worktree.map(worktree_name),
        draft_worktree_choices: match (current, current_project) {
            (None, Some(project)) => worktree_choices(shown, &project.id, shown.draft_worktree_id.as_deref()),
            _ => Vec::new(),
        },
        pull_request: pull_request_for(shown.pull_request.as_ref(), workspace_id.as_deref(), environment),
        workspace_id,
        environment,
        review_subject: dock_subject(shown, &owner, DIFF_PANEL),
        automation_subject: dock_subject(shown, &owner, AUTOMATION_PANEL),
        diff: crate::state::diff_for(shown, &owner),
        current_folder: crate::workspace::location::current_folder(shown),
        side_chats: dock_side_chats(shown, &owner).into_iter().filter_map(|chat| side_chat_view(shown, chat)).collect(),
        thread_tabs: dock.thread_tabs.iter().filter_map(|task_id| thread_tab_view(shown, task_id, &activity.working, &activity.blocked)).collect(),
        coordination: coordination_view(shown, current, &activity.working, &activity.blocked),
        member_pull_requests: member_pull_requests_view(shown, lead),
        member_worktrees: member_worktrees_view(shown, lead, &activity.working),
        zen_available: current.is_some_and(|thread| !thread.messages.is_empty()),
        review_picker: shown.review_picker.as_ref().filter(|picker| current.is_some_and(|thread| thread.id == picker.task_id && !activity.working.contains(&thread.id))),
        dock_owner: owner.clone(),
        dock,
    }
}

/// Everything the window draws: its own collections and chrome, and the thread on screen, read from
/// the paired computer holding it when one does.
pub struct WorkspaceView<'a> {
    /// This window's own state: settings, chrome, menus, and every draft typed here.
    pub window: &'a WorkspaceState,
    /// The state the thread on screen belongs to: this window's, or a paired computer's.
    pub shown: &'a WorkspaceState,
    /// The paired computer whose thread is on screen, if any.
    pub shown_computer: Option<&'a PairedComputer>,
    pub collections: WorkspaceCollections<'a>,
    pub thread: ThreadView<'a>,
    /// The draft the main composer types into, which is always this window's.
    pub prompt_key: String,
    pub dictation: DictationView,
    pub overview_folds: Option<&'a OverviewFolds>,
    pub reading_point: Option<&'a ReadingPoint>,
    pub simulator: SimulatorState,
    pub simulator_id: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub active_computer: Option<ComputerLink>,
    pub worktree_delete_confirmation: Option<WorktreeSettingsView>,
    pub action_error: Option<String>,
    pub action_error_page: Option<crate::domain::sidebar::SettingsSection>,
    pub settings_open: bool,
    pub settings_section: Option<crate::domain::sidebar::SettingsSection>,
    pub app_build: Option<&'a AppBuild>,
    /// Set while the thread on screen runs on an engine only a newer build knows, which is read here and takes nothing.
    pub unopenable: Option<String>,
}

pub fn derive_view(window: &WorkspaceState) -> WorkspaceView<'_> {
    let shown_computer = window.selected_computer();
    let shown = shown_computer.and_then(|computer| computer.state.as_deref()).unwrap_or(window);
    let collections = workspace_collections(window);
    let thread = thread_view(shown, window);
    let remote = !std::ptr::eq(shown, window);
    let key = prompt_key(window);
    let current_id = thread.current_thread.map(|thread| thread.id.clone());
    let own_confirmation = collections.managed_worktrees.as_ref().and_then(|managed| managed.iter().find(|item| Some(&item.root) == window.worktree_settings.confirming.as_ref() && !item.deleting).cloned());
    let shown_confirmation = || {
        let working = thread_activity(shown).working;
        worktree_settings_views(shown, &working).and_then(|views| views.into_iter().find(|item| Some(&item.root) == window.worktree_settings.confirming.as_ref() && !item.deleting))
    };
    let simulator = if remote {
        SimulatorState { availability: SimulatorAvailability::Unsupported { reason: "The simulator only runs on the computer you are using.".into() }, ..SimulatorState::default() }
    } else {
        window.simulator.clone()
    };
    let simulator_id = if remote { None } else { thread.dock.simulator_id.clone() };
    WorkspaceView {
        dictation: dictation_view(window),
        overview_folds: current_id.as_deref().and_then(|id| window.overview_folds.get(id)),
        reading_point: current_id.as_deref().and_then(|id| window.reading_points.get(id)).and_then(Option::as_ref),
        simulator,
        simulator_id,
        can_go_back: crate::reducer::reachable_visit(window, -1).is_some(),
        can_go_forward: crate::reducer::reachable_visit(window, 1).is_some(),
        active_computer: collections.computer_links.iter().find(|link| Some(&link.id) == window.computers.active.as_ref()).cloned(),
        worktree_delete_confirmation: if remote { own_confirmation.or_else(shown_confirmation) } else { own_confirmation },
        action_error: window.action_error.clone().or_else(|| if remote { shown.action_error.clone() } else { None }),
        action_error_page: if window.action_error.is_some() || !remote { window.action_error_page } else { shown.action_error_page },
        settings_open: window.settings_open || window.computer_use_setup,
        settings_section: if window.computer_use_setup { Some(crate::domain::sidebar::SettingsSection::ComputerUse) } else { window.settings_section },
        app_build: window.app_build.as_ref(),
        unopenable: thread.current_thread.filter(|thread| !thread.engine.is_known()).map(|thread| crate::domain::engine::unopenable_engine(thread.engine.as_str())),
        prompt_key: key,
        shown_computer,
        collections,
        thread,
        window,
        shown,
    }
}

/// Whether a thread is doing work as the computer holding it said, which one waiting on approval is not.
pub fn thread_working(state: &WorkspaceState, task_id: &str) -> bool {
    doing_work(&thread_activity(state), task_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::engine::AgentEngine;
    use crate::domain::platform::ComputerLink;
    use crate::domain::run::ExecutionPolicy;

    fn task(id: &str) -> Thread {
        Thread::new(id, id, AgentEngine::Claude, ExecutionPolicy::Confirm, 1)
    }

    #[test]
    fn a_paired_computers_thread_is_drawn_from_its_state_under_this_windows_drafts() {
        let mut remote = WorkspaceState::default();
        remote.threads.push(task("far").into());
        remote.current_id = Some("far".into());
        remote.prompts.insert("far".into(), "typed there".into());
        let mut window = WorkspaceState::default();
        window.threads.push(task("near").into());
        window.prompts.insert("far".into(), "typed here".into());
        window.computers.paired.push(PairedComputer { link: ComputerLink { id: "studio".into(), name: "Studio".into(), host: "studio.local".into(), status: ComputerStatus::Connected, error: None, paired_at: 1, capabilities: None }, state: Some(Box::new(remote)) });
        window.computers.active = Some("studio".into());
        let view = derive_view(&window);
        assert_eq!(view.thread.current_thread.map(|thread| thread.id.as_str()), Some("far"));
        assert_eq!(view.prompt_key, "far");
        assert_eq!(view.window.prompts.get(&view.prompt_key).map(String::as_str), Some("typed here"), "drafts stay where they are typed");
        let listed: Vec<&str> = view.collections.lists.ordered_threads.iter().map(|thread| thread.id.as_str()).collect();
        assert!(listed.contains(&"near") && listed.contains(&"far"), "the sidebar merges every computer's threads");
        assert_eq!(view.collections.remote.thread_hosts.get("far").map(|host| host.name.as_str()), Some("Studio"));
        assert!(matches!(view.simulator.availability, SimulatorAvailability::Unsupported { .. }), "a device the other computer runs cannot be driven here");
    }
}
