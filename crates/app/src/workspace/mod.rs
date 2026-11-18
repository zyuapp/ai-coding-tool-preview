//! The one workspace: its state, the reducer that writes it, the effects that leave it, and what
//! the window keeps durable around it (src/host/workspace-runtime.ts, useTaskWorkspace.ts).

pub mod devices;
mod history;
mod persistence;
mod plan_usage_reads;
mod pull_request_reads;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use act_core::effects::{Effect, EffectWork};
use act_core::events::{AgentEvent, ViewEvent, WorkspaceEvent, WorkspaceInput};
use act_core::state::{shown_thread_tab, WorkspaceState, NEW_THREAD_DRAFT};
use act_core::workspace::preferences::read_view_preferences;
use act_core::{reduce_transition, AppCommand};
use act_store::json_storage::{DRAFT_PROMPTS_KEY, VIEW_PREFERENCES_KEY};
use futures::StreamExt;
use gpui::{App, Context, Entity, EventEmitter, Global, Task};

use crate::effects::requests::WindowWork;
use crate::effects::tracking::Tracker;
use crate::effects::{Host, Inbound, StoreReply};
use history::Reply;
use persistence::Persistence;
use plan_usage_reads::PlanUsageReads;
use pull_request_reads::PullRequestReads;

/// What the window's own views are asked to do beside the state change: effects that belong to
/// a view rather than to the host.
#[derive(Clone, Debug, PartialEq)]
pub enum Surface {
    CloseWindow,
    FocusWindow,
    FindInTerminal { terminal_id: String, query: String, forward: bool },
    StopFindInTerminal { terminal_id: String },
    /// Text streamed into these threads' messages and nothing else changed.
    Streamed { tasks: Vec<String> },
}

pub struct Workspace {
    pub state: std::rc::Rc<WorkspaceState>,
    pub host: Arc<Host>,
    persistence: Persistence,
    history: history::History,
    drafts_ready: bool,
    drafts_timer: Option<Task<()>>,
    snooze_timer: Option<Task<()>>,
    limit_timer: Option<Task<()>>,
    toast_timers: HashMap<i64, Task<()>>,
    environment_poll: Option<Task<()>>,
    environment_key: Option<String>,
    plan_usage: PlanUsageReads,
    pull_requests: PullRequestReads,
    started: bool,
    /// Coalesces what paired computers and phones are told.
    outside_timer: Option<Task<()>>,
    /// Bumped on every change, so views can tell whether what they derived is still current.
    pub revision: u64,
    /// Bumped on every change but text streaming into messages that already exist.
    pub outline_revision: u64,
    launched_update: Option<String>,
    pub browser: Option<Entity<act_browser::BrowserHost>>,
    pub simulator: Option<act_devices::simulator::SimulatorHost>,
    /// Requests that arrived before the store loaded, run in arrival order once it has; None after.
    early: Option<Vec<Early>>,
}

/// A request held until the store has loaded, so nothing acts on the empty state.
enum Early {
    Input(WorkspaceInput, Reply),
    Arguments(Vec<String>),
}

/// The window's handle on the workspace, for views that dispatch from a click.
struct WorkspaceGlobal(Entity<Workspace>);
impl Global for WorkspaceGlobal {}

pub fn install(workspace: Entity<Workspace>, cx: &mut App) {
    cx.set_global(WorkspaceGlobal(workspace));
}

pub fn workspace(cx: &App) -> Entity<Workspace> {
    cx.global::<WorkspaceGlobal>().0.clone()
}

/// Sends a command from any view.
pub fn dispatch(command: AppCommand, cx: &mut App) {
    let workspace = workspace(cx);
    workspace.update(cx, |workspace, cx| workspace.request(WorkspaceInput::Command(command), Reply::None, cx));
}

/// Sends what a view found, such as a search's count, from any view.
pub fn report(event: ViewEvent, cx: &mut App) {
    let workspace = workspace(cx);
    workspace.update(cx, |workspace, cx| workspace.dispatch(event.into(), cx));
}

impl EventEmitter<Surface> for Workspace {}

impl Workspace {
    pub fn new(host: Arc<Host>, launched_update: Option<String>, cx: &mut Context<Self>) -> Workspace {
        let mut state = WorkspaceState::default();
        let raw = host.storage.lock().unwrap().get(VIEW_PREFERENCES_KEY).map(String::from);
        let preferences = read_view_preferences(raw.as_deref());
        reduce_transition(&mut state, WorkspaceEvent::PreferencesLoaded { preferences: Box::new(preferences) });
        let _ = cx;
        Workspace {
            state: std::rc::Rc::new(state),
            host,
            persistence: Persistence::default(),
            history: history::History::default(),
            drafts_ready: false,
            drafts_timer: None,
            snooze_timer: None,
            limit_timer: None,
            toast_timers: HashMap::new(),
            environment_poll: None,
            environment_key: None,
            plan_usage: PlanUsageReads::default(),
            pull_requests: PullRequestReads::default(),
            started: false,
            outside_timer: None,
            revision: 0,
            outline_revision: 0,
            launched_update,
            browser: None,
            simulator: None,
            early: Some(Vec::new()),
        }
    }

    /// Opens the inbox, loads the store, and asks the engines where they stand.
    pub fn start(this: &Entity<Workspace>, cx: &mut App) {
        let inbox = this.read(cx).host.inbox.lock().unwrap().take();
        let Some(mut inbox) = inbox else { return };
        let weak = this.downgrade();
        cx.spawn(async move |cx| {
            while let Some(first) = inbox.next().await {
                let mut batch = vec![first];
                while let Ok(more) = inbox.try_recv() {
                    batch.push(more);
                    if batch.len() > 512 {
                        break;
                    }
                }
                let alive = weak.update(cx, |workspace, cx| workspace.receive(batch, cx)).is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
        this.update(cx, |workspace, cx| {
            workspace.started = true;
            workspace.host.load_store();
            workspace.restore_computers(cx);
            if let Some(version) = workspace.launched_update.clone() {
                workspace.dispatch(WorkspaceInput::Event(WorkspaceEvent::AppLaunched { version }), cx);
            }
            workspace.dispatch(WorkspaceInput::Command(AppCommand::EngineRead { refresh: None }), cx);
            workspace.request(WorkspaceInput::Command(AppCommand::ViewMounted), Reply::None, cx);
        });
    }

    /// One batch from the inbox: run reports fold into one input, the rest go in order.
    fn receive(&mut self, batch: Vec<Inbound>, cx: &mut Context<Self>) {
        let mut agent: Vec<AgentEvent> = Vec::new();
        for inbound in batch {
            match inbound {
                Inbound::Agent(event) => {
                    if let Some((run_id, status)) = crate::effects::runs::terminal_status(&event) {
                        self.host.automations.settle(&run_id, status);
                    }
                    agent.push(event);
                }
                other => {
                    if !agent.is_empty() {
                        self.dispatch(WorkspaceInput::Event(WorkspaceEvent::AgentEvents { events: std::mem::take(&mut agent) }), cx);
                    }
                    match other {
                        Inbound::Input(input) => self.request(input, Reply::None, cx),
                        Inbound::Tracked(input, tracker) => self.request(input, Reply::Tracked(tracker), cx),
                        Inbound::Execute { input, accepted, tracker } => self.request(input, Reply::Execute(tracker, accepted), cx),
                        Inbound::Window(work) => self.window_work(work, cx),
                        Inbound::Store(reply) => self.store_replied(reply, cx),
                        Inbound::Arguments(arguments) => match &mut self.early {
                            Some(early) => early.push(Early::Arguments(arguments)),
                            None => self.open_arguments(arguments, cx),
                        },
                        Inbound::Agent(_) => {}
                    }
                }
            }
        }
        if !agent.is_empty() {
            self.dispatch(WorkspaceInput::Event(WorkspaceEvent::AgentEvents { events: agent }), cx);
        }
    }

    /// A command from a view or from outside the window: held while the store has yet to load,
    /// else run now. Events always run, as they report on work already going.
    fn request(&mut self, input: WorkspaceInput, reply: Reply, cx: &mut Context<Self>) {
        match &mut self.early {
            Some(early) if matches!(input, WorkspaceInput::Command(_)) => early.push(Early::Input(input, reply)),
            _ => self.run(input, reply, cx),
        }
    }

    /// Runs what arrived before the store loaded, in the order it arrived.
    fn release_early(&mut self, cx: &mut Context<Self>) {
        for early in self.early.take().unwrap_or_default() {
            match early {
                Early::Input(input, reply) => self.run(input, reply, cx),
                Early::Arguments(arguments) => self.open_arguments(arguments, cx),
            }
        }
    }

    /// Runs one input through the reducer, performs what it asks for, and keeps what must last.
    pub fn dispatch(&mut self, input: WorkspaceInput, cx: &mut Context<Self>) {
        self.run(input, Reply::None, cx);
    }

    /// As `dispatch`, answering whoever waits on the input once the reducer and its effects are done.
    fn run(&mut self, input: WorkspaceInput, reply: Reply, cx: &mut Context<Self>) {
        let needed = self.history.needed(&self.state, &input);
        if !needed.is_empty() {
            for task_id in &needed {
                self.hydrate(task_id.clone());
            }
            self.history.hold(needed, input, reply);
            return;
        }
        let previous_current = self.state.current_id.clone();
        let previous_tab = shown_thread_tab(&self.state);
        let previous_prompts = (self.state.prompts.len(), self.state.prompts.values().map(String::len).sum::<usize>());
        let streamed = streamed_text(&input).map(|tasks| {
            let before = outline(&self.state, &tasks);
            (tasks, before)
        });
        let skips_persistence = matches!(&input, WorkspaceInput::Event(WorkspaceEvent::SubagentActivityLoaded { .. }) | WorkspaceInput::Event(WorkspaceEvent::StoreThreadLoaded { .. }));
        let transition = reduce_transition(std::rc::Rc::make_mut(&mut self.state), input);
        let answer = transition.result.clone().unwrap_or_else(act_core::CommandResult::ok);
        let tracker = match reply {
            Reply::None => None,
            Reply::Tracked(tracker) => {
                tracker.record(answer);
                Some(tracker)
            }
            Reply::Execute(tracker, accepted) => {
                let _ = accepted.send(answer.clone());
                tracker.accept(answer);
                Some(tracker)
            }
        };
        for effect in transition.effects {
            let tracked = if effect.follow_up { None } else { tracker.clone() };
            self.perform(effect, tracked, cx);
        }
        drop(tracker);
        self.revision += 1;
        let streamed = streamed.filter(|(tasks, before)| outline(&self.state, tasks) == *before).map(|(tasks, _)| tasks);
        if streamed.is_none() {
            self.outline_revision += 1;
        }
        self.host.requests.release_waiters(&self.state);
        self.host.system.work_changed(&self.state);
        self.tell_outside(cx);
        if self.state.current_id != previous_current {
            if let Some(id) = self.state.current_id.clone() {
                self.hydrate(id);
            }
        }
        let tab = shown_thread_tab(&self.state);
        if tab != previous_tab {
            if let Some(id) = tab {
                self.hydrate(id);
            }
        }
        let prompts = (self.state.prompts.len(), self.state.prompts.values().map(String::len).sum::<usize>());
        if prompts != previous_prompts {
            self.drafts_changed(cx);
        }
        if !skips_persistence {
            self.schedule_persistence(cx);
        }
        self.poll_environment(cx);
        match streamed {
            Some(tasks) => cx.emit(Surface::Streamed { tasks }),
            None => {
                self.ask_reads(cx);
                cx.notify();
            }
        }
    }

    /// Asks for what the views on screen show and only a read keeps current: plan limits and pull
    /// requests.
    fn ask_reads(&mut self, cx: &mut Context<Self>) {
        let mut commands = self.pull_requests.due_reads(&self.state);
        if self.plan_usage.due(&self.state) {
            commands.push(AppCommand::UsageRead { everywhere: None });
        }
        self.poll_pull_requests(cx);
        if !commands.is_empty() {
            cx.defer(move |cx| {
                for command in commands {
                    dispatch(command, cx);
                }
            });
        }
    }

    fn poll_pull_requests(&mut self, cx: &mut Context<Self>) {
        if !self.pull_requests.needs_poll() {
            return;
        }
        self.pull_requests.poll = Some(cx.spawn(async |this, cx| {
            cx.background_executor().timer(Duration::from_millis(act_core::domain::pull_request::PULL_REQUEST_POLL_MS as u64)).await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.pull_requests.poll = None;
                let commands = workspace.pull_requests.due_reads(&workspace.state);
                workspace.poll_pull_requests(cx);
                for command in commands {
                    workspace.command(command, cx);
                }
            });
        }));
    }

    pub fn command(&mut self, command: AppCommand, cx: &mut Context<Self>) {
        self.dispatch(WorkspaceInput::Command(command), cx);
    }

    fn perform(&mut self, effect: Effect, tracker: Option<Tracker>, cx: &mut Context<Self>) {
        if self.perform_device(&effect.work, tracker.clone(), cx) {
            return;
        }
        match effect.work {
            EffectWork::PersistPreferences { preferences } => {
                if let Ok(text) = serde_json::to_string(&preferences) {
                    if let Err(error) = self.host.storage.lock().unwrap().set(VIEW_PREFERENCES_KEY, text) {
                        eprintln!("Could not save preferences: {error}");
                    }
                }
                crate::theme_bridge::apply(&self.state, cx);
                crate::theme_bridge::remember(&self.state, &self.host.profile.window_theme(), cx);
            }
            EffectWork::ScheduleSnoozeExpiry { at } => self.snooze_timer = at.map(|at| self.timer_at(at, cx, |at| WorkspaceEvent::SnoozesElapsed { at })),
            EffectWork::ScheduleLimitReset { at } => self.limit_timer = at.map(|at| self.timer_at(at, cx, |at| WorkspaceEvent::LimitsElapsed { at })),
            EffectWork::ScheduleToastDismissal { id, at } => match at {
                Some(at) => {
                    let delay = Duration::from_millis((at - act_core::util::now()).max(0) as u64);
                    let task = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(delay).await;
                        let _ = this.update(cx, |workspace, cx| workspace.command(AppCommand::DismissToast { id }, cx));
                    });
                    self.toast_timers.insert(id, task);
                }
                None => {
                    self.toast_timers.remove(&id);
                }
            },
            EffectWork::PickProject => self.pick_folder(cx, |host, root| {
                let runner = host.clone();
                runner.spawn(async move { crate::effects::opened_folder(&host, root).await })
            }),
            EffectWork::ProjectAddPick { request } => {
                let sink = self.host.sink.clone();
                self.pick_folder(cx, move |_, root| sink.event(WorkspaceEvent::ProjectPathPicked { request, root: Some(root), error: None }));
            }
            EffectWork::AppOpenSourceLicenses => crate::licenses::open(cx),
            EffectWork::CloseWindow => cx.emit(Surface::CloseWindow),
            EffectWork::FocusWindow => cx.emit(Surface::FocusWindow),
            EffectWork::FindInTerminal { terminal_id, query, forward } => cx.emit(Surface::FindInTerminal { terminal_id, query, forward }),
            EffectWork::StopFindInTerminal { terminal_id } => cx.emit(Surface::StopFindInTerminal { terminal_id }),
            EffectWork::AnnounceThread { notice } => self.announce(notice, cx),
            EffectWork::ApplyShortcuts { .. } | EffectWork::ApplyCaptureOptions { .. } | EffectWork::CaptureShortcut { .. } => {}
            EffectWork::BrowserShow { .. } | EffectWork::FocusBrowser { .. } | EffectWork::BrowserPermissions { .. } => {}
            work => match tracker {
                Some(tracker) => self.host.perform_tracked(work, Some(tracker)),
                None => self.host.perform(work),
            },
        }
    }

    /// What the host asked of the window's own thread.
    fn window_work(&mut self, work: WindowWork, cx: &mut Context<Self>) {
        match work {
            WindowWork::Read(read) => read(&self.state),
            WindowWork::Hydrate(task_id, done) => {
                if self.state.thread(&task_id).is_some_and(|thread| thread.history_summary.is_some()) {
                    self.host.requests.await_history(task_id.clone(), done);
                    self.hydrate(task_id);
                } else {
                    let _ = done.send(());
                }
            }
            WindowWork::UpdateNotice(notice) => crate::effects::system::show_update_notice(&self.host, notice, cx),
            WindowWork::Relaunch { update } => {
                if update {
                    if let Some(version) = self.host.system.updated_version() {
                        let resumable = crate::effects::system::resumable_after_update(&self.state);
                        act_host::updates::record_update_relaunch(&self.host.profile.user_data, &version, &resumable);
                    }
                }
                self.host.system.request_restart(Vec::new());
                cx.quit();
            }
            WindowWork::UpdateChecking(checking) => crate::app_menu::set_update_checking(checking, cx),
            WindowWork::SaveImage(download) => {
                let directory = download.default_path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
                let name = download.default_path.file_name().map(|name| name.to_string_lossy().into_owned());
                let chosen = cx.prompt_for_new_path(&directory, name.as_deref());
                let sink = self.host.sink.clone();
                cx.spawn(async move |_, _| {
                    let Ok(Ok(Some(destination))) = chosen.await else { return };
                    let saved = act_host::runtime().spawn(async move { act_host::image_download::save_image_download(&download, &destination).await }).await;
                    if let Ok(Err(error)) = saved {
                        sink.failed(error);
                    }
                })
                .detach();
            }
            WindowWork::Reveal => {
                cx.activate(true);
                if let Some(window) = cx.windows().into_iter().next() {
                    let _ = window.update(cx, |_, window, _| window.activate_window());
                }
            }
        }
    }

    /// A thread's notice goes where the user is: the desktop while this window is not in front, and
    /// the phones while the user is away from this computer.
    fn announce(&mut self, notice: act_core::events::ThreadNotice, cx: &mut Context<Self>) {
        let _ = cx;
        let headless = self.host.headless();
        if let Ok(shared) = crate::effects::convert::<_, act_host::notify::ThreadNotice>(&notice) {
            if headless {
                self.host.computers.notice(&shared);
            } else if act_host::notify::desktop_away() {
                self.host.computers.notice_phones(&shared);
            }
        }
        if !headless && !self.state.focused {
            crate::effects::announce(&self.host.sink, notice);
        }
    }

    /// Paired computers and phones hear of a change once per short while, so a run streaming
    /// tokens costs them one update rather than one per report.
    fn tell_outside(&mut self, cx: &mut Context<Self>) {
        if self.outside_timer.is_some() {
            return;
        }
        self.outside_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(80)).await;
            let _ = this.update(cx, |workspace, _| {
                workspace.outside_timer = None;
                let state = workspace.state.clone();
                if workspace.host.computers.watched(&state) {
                    workspace.host.computers.publish(&state);
                }
            });
        }));
    }

    fn timer_at(&self, at: i64, cx: &mut Context<Self>, event: impl FnOnce(i64) -> WorkspaceEvent + 'static) -> Task<()> {
        let delay = Duration::from_millis((at - act_core::util::now()).max(0) as u64);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |workspace, cx| workspace.dispatch(WorkspaceInput::Event(event(at)), cx));
        })
    }

    /// The system's folder panel. What it hands back is the host's to register.
    fn pick_folder(&mut self, cx: &mut Context<Self>, chosen: impl FnOnce(Arc<Host>, String) + 'static) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Open".into()) });
        let host = self.host.clone();
        cx.spawn(async move |_, _| {
            if let Ok(Ok(Some(paths))) = paths.await {
                if let Some(path) = paths.into_iter().next() {
                    chosen(host, path.to_string_lossy().into_owned());
                }
            }
        })
        .detach();
    }

    fn store_replied(&mut self, reply: StoreReply, cx: &mut Context<Self>) {
        match reply {
            StoreReply::Loaded(Ok(Some(loaded))) => {
                self.persistence.adopt_loaded(&loaded.data);
                let resuming = act_store::files::take_update_relaunch(&self.host.profile.user_data, crate::startup::APP_VERSION);
                let hidden = i64::try_from(loaded.hidden_tasks).ok();
                self.dispatch(
                    WorkspaceInput::Event(WorkspaceEvent::StoreLoaded { data: Box::new(loaded.data), hidden_tasks: hidden, resuming: (!resuming.is_empty()).then_some(resuming) }),
                    cx,
                );
                self.store_settled(cx);
            }
            StoreReply::Loaded(Ok(None)) => {
                self.dispatch(WorkspaceInput::Event(WorkspaceEvent::StoreAbsent), cx);
                self.store_settled(cx);
            }
            StoreReply::Loaded(Err(message)) => {
                self.dispatch(WorkspaceInput::Event(WorkspaceEvent::StoreFailed { message }), cx);
                self.release_early(cx);
            }
            StoreReply::Messages { task_id, result } => {
                self.history.loaded(&task_id);
                match result {
                    Ok(messages) => {
                        let messages: Vec<_> = messages.into_iter().map(act_core::shared::Shared::new).collect();
                        self.persistence.adopt_messages(&task_id, &messages);
                        if self.state.thread(&task_id).is_some_and(|thread| thread.history_summary.is_some()) {
                            self.dispatch(WorkspaceInput::Event(WorkspaceEvent::StoreThreadLoaded { task_id: task_id.clone(), messages }), cx);
                        }
                    }
                    Err(message) => self.dispatch(WorkspaceInput::Event(WorkspaceEvent::ActionFailed { message, page: None }), cx),
                }
                self.host.requests.history_loaded(&task_id);
                for (input, reply) in self.history.release(&task_id, &self.state) {
                    self.run(input, reply, cx);
                }
            }
            StoreReply::Persisted(result) => {
                self.persistence.in_flight = false;
                if let Err(message) = result {
                    self.persistence.ready = false;
                    self.dispatch(WorkspaceInput::Event(WorkspaceEvent::StoreFailed { message }), cx);
                } else {
                    self.schedule_persistence(cx);
                }
            }
        }
    }

    /// A thread put on screen loads its transcript behind the loading state it shows meanwhile.
    fn hydrate(&mut self, task_id: String) {
        if !self.state.thread(&task_id).is_some_and(|thread| thread.history_summary.is_some()) {
            return;
        }
        if self.history.begin(&task_id) {
            self.host.load_thread_messages(task_id);
        }
    }

    fn schedule_persistence(&mut self, cx: &mut Context<Self>) {
        if !self.persistence.ready || !self.state.writable || self.state.storage_error.is_some() || self.persistence.timer.is_some() || self.persistence.in_flight {
            return;
        }
        self.persistence.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let _ = this.update(cx, |workspace, _| {
                workspace.persistence.timer = None;
                if let Some(delta) = workspace.persistence.delta(&workspace.state) {
                    workspace.persistence.in_flight = true;
                    workspace.host.persist(delta);
                }
            });
        }));
    }

    /// Drafts come back once the store is in; saved drafts that cannot be read stop persistence as an unreadable store does.
    fn store_settled(&mut self, cx: &mut Context<Self>) {
        match self.restore_drafts(cx) {
            Ok(()) => {
                self.persistence.ready = true;
                self.schedule_persistence(cx);
            }
            Err(message) => self.dispatch(WorkspaceInput::Event(WorkspaceEvent::StoreFailed { message }), cx),
        }
        self.release_early(cx);
    }

    fn restore_drafts(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let raw = self.host.storage.lock().unwrap().get(DRAFT_PROMPTS_KEY).map(String::from);
        let prompts = match raw {
            None => serde_json::Map::new(),
            Some(raw) => read_draft_prompts(&raw)?,
        };
        // Builds that kept a new-thread draft per project saved several; the first with text wins.
        let mut kept: Vec<(String, String)> = Vec::new();
        for (owner, text) in prompts {
            let Some(text) = text.as_str() else { continue };
            let key = if owner.starts_with(NEW_THREAD_DRAFT) { NEW_THREAD_DRAFT.to_string() } else { owner };
            match kept.iter_mut().find(|(known, _)| *known == key) {
                Some((_, held)) if held.is_empty() => *held = text.to_string(),
                Some(_) => {}
                None => kept.push((key, text.to_string())),
            }
        }
        for (key, text) in kept {
            if !self.state.prompts.contains_key(&key) {
                self.command(AppCommand::SetPrompt { task_id: Some(key), prompt: text }, cx);
            }
        }
        self.drafts_ready = true;
        Ok(())
    }

    /// Drafts are saved off the typing path.
    fn drafts_changed(&mut self, cx: &mut Context<Self>) {
        if !self.drafts_ready || self.drafts_timer.is_some() {
            return;
        }
        self.drafts_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let _ = this.update(cx, |workspace, _| {
                workspace.drafts_timer = None;
                workspace.save_drafts();
            });
        }));
    }

    fn save_drafts(&self) {
        let temporary = self.state.side_chat_ids();
        let remote_known = self.state.computers.paired.iter().all(|computer| computer.state.is_some());
        let mut owners: HashSet<String> = HashSet::from([NEW_THREAD_DRAFT.to_string()]);
        for thread in &self.state.threads {
            owners.insert(thread.id.clone());
        }
        for computer in &self.state.computers.paired {
            for thread in computer.state.iter().flat_map(|state| state.threads.iter()) {
                owners.insert(thread.id.clone());
            }
        }
        let kept: serde_json::Map<String, serde_json::Value> = self
            .state
            .prompts
            .iter()
            .filter(|(owner, _)| !temporary.contains(*owner) && (!remote_known || owners.contains(*owner)))
            .map(|(owner, text)| (owner.clone(), serde_json::Value::String(text.clone())))
            .collect();
        if let Ok(text) = serde_json::to_string(&kept) {
            let _ = self.host.storage.lock().unwrap().set(DRAFT_PROMPTS_KEY, text);
        }
    }

    /// Asks Git about the checkout in front at once, then on a timer: often during a run someone is
    /// watching, rarely otherwise.
    fn poll_environment(&mut self, cx: &mut Context<Self>) {
        if !self.started {
            return;
        }
        let thread = self.state.current_thread();
        let run = self.state.current_id.as_ref().and_then(|id| self.state.active_runs.get(id)).map(|run| run.run_id.clone());
        let key = format!(
            "{:?}|{:?}|{:?}|{}|{:?}",
            self.state.current_id,
            self.state.draft_project_id,
            thread.and_then(|thread| thread.worktree_id.clone()),
            self.state.projects.iter().map(|project| format!("{}:{:?}", project.id, project.workspace_id)).collect::<Vec<_>>().join(","),
            run
        );
        if self.environment_key.as_deref() == Some(key.as_str()) {
            return;
        }
        self.environment_key = Some(key);
        let interval = Duration::from_millis(if run.is_some() { if self.state.focused { 2_000 } else { 10_000 } } else { 15_000 });
        self.environment_poll = Some(cx.spawn(async move |this, cx| loop {
            if this.update(cx, |workspace, cx| workspace.command(AppCommand::RefreshEnvironment, cx)).is_err() {
                break;
            }
            cx.background_executor().timer(interval).await;
        }));
    }

    /// Paired computers this build cannot reach, listed as offline so their names still read.
    fn restore_computers(&mut self, cx: &mut Context<Self>) {
        let stored = act_store::files::read_computers(&self.host.profile.computers());
        let name = stored.name.unwrap_or_else(|| act_platform::ipc::hostname().trim_end_matches(".local").to_string());
        let links: Vec<serde_json::Value> = stored
            .computers
            .iter()
            .map(|computer| {
                serde_json::json!({
                    "id": computer.id,
                    "name": computer.label.clone().unwrap_or_else(|| computer.name.clone()),
                    "host": computer.host,
                    "status": "offline",
                    "error": "Paired computers are not available in this build.",
                    "pairedAt": computer.paired_at as i64,
                })
            })
            .collect();
        if let Ok(event) = serde_json::from_value::<WorkspaceEvent>(serde_json::json!({ "type": "computers.changed", "name": name, "links": links })) {
            self.dispatch(WorkspaceInput::Event(event), cx);
        }
    }

    /// A later launch, a link or the `aic` command named a folder to open; anything else brings the
    /// window forward.
    fn open_arguments(&mut self, arguments: Vec<String>, cx: &mut Context<Self>) {
        let link = arguments.iter().find(|argument| argument.starts_with(&format!("{}://", act_core::identity::URL_SCHEME)));
        let root = match link {
            Some(link) => act_host::cli_install::project_path_from_url(link),
            None => act_host::cli_install::project_path_from_argv(&arguments).or_else(|| arguments.iter().find(|argument| std::path::Path::new(argument).is_absolute() && std::path::Path::new(argument).is_dir()).cloned()),
        };
        if let Some(root) = root {
            let host = self.host.clone();
            self.host.spawn(async move { crate::effects::opened_folder(&host, root).await });
        }
        if !self.host.headless() {
            self.window_work(WindowWork::Reveal, cx);
        }
    }

    /// Flushes what is still to be written, before quitting.
    pub fn flush(&mut self) {
        if self.drafts_ready {
            self.save_drafts();
        }
        if self.persistence.ready && !self.persistence.in_flight {
            if let Some(delta) = self.persistence.delta(&self.state) {
                if let Some(database) = self.host.database.get() {
                    let _ = database.persist(delta).wait();
                }
            }
        }
    }
}

/// The saved drafts: an object of text by owner, or an error when they are anything else.
fn read_draft_prompts(raw: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    const UNREADABLE: &str = "Saved draft text could not be read.";
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(prompts)) if prompts.values().all(serde_json::Value::is_string) => Ok(prompts),
        _ => Err(UNREADABLE.into()),
    }
}

/// The threads an input only streams text into, when that is all it does.
fn streamed_text(input: &WorkspaceInput) -> Option<Vec<String>> {
    use act_core::events::RunReport;
    let WorkspaceInput::Event(WorkspaceEvent::AgentEvents { events }) = input else { return None };
    let mut tasks: Vec<String> = Vec::new();
    for event in events {
        let AgentEvent::Run(run) = event else { return None };
        if !matches!(run.report, RunReport::AssistantDelta { .. } | RunReport::AssistantTail { .. }) {
            return None;
        }
        if !tasks.contains(&run.task_id) {
            tasks.push(run.task_id.clone());
        }
    }
    Some(tasks)
}

/// What text streaming into existing messages leaves alone: how many threads and runs there are,
/// and how many messages each streamed thread has.
fn outline(state: &WorkspaceState, tasks: &[String]) -> (usize, usize, Vec<Option<usize>>) {
    let counts = tasks.iter().map(|id| state.thread(id).map(|thread| thread.messages.len())).collect();
    (state.threads.len(), state.active_runs.len(), counts)
}
