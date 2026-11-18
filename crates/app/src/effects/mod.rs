//! Performs the reducer's effects with the host services, the engines, the store and the terminals,
//! and answers with workspace inputs. Each module owns the effects of one capability, as
//! `src/host/*-effects.ts` do.

mod attachments;
mod automations;
pub mod computers;
mod desktop;
pub mod dictation;
mod engines;
mod projects;
mod published;
mod phones;
pub mod requests;
pub mod runs;
mod store;
pub mod system;
mod terminal;
mod toolkit;
pub mod tracking;
pub mod window_desktop;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use act_core::effects::EffectWork;
use act_core::events::{AgentEvent, WorkspaceEvent, WorkspaceInput};
use act_core::AppCommand;
use act_host::instance_lock::DesktopLock;
use act_host::services::HostServices;
use act_store::json_storage::JsonStorage;
use act_store::profile::Profile;
use act_store::service::TaskDatabaseService;
use act_terminal::TerminalHost;
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use serde::de::DeserializeOwned;
use serde::Serialize;

pub use desktop::announce;
pub use projects::opened_folder;
pub use store::StoreReply;

/// What the window's inbox carries back from the host.
pub enum Inbound {
    Input(WorkspaceInput),
    /// A run's report, folded with the others that arrive in the same frame.
    Agent(AgentEvent),
    Store(StoreReply),
    /// A later launch's arguments, handed over the instance lock.
    Arguments(Vec<String>),
    /// A reply to an input someone is waiting on, counted toward it.
    Tracked(WorkspaceInput, tracking::Tracker),
    /// An input from outside the window whose answer and completion are awaited.
    Execute { input: WorkspaceInput, accepted: futures::channel::oneshot::Sender<act_core::CommandResult>, tracker: tracking::Tracker },
    /// Work that reads or waits on the window's own state.
    Window(requests::WindowWork),
}

/// The door back into the reducer, usable from any thread.
#[derive(Clone)]
pub struct Sink(UnboundedSender<Inbound>);

impl Sink {
    pub fn channel() -> (Sink, UnboundedReceiver<Inbound>) {
        let (sender, receiver) = unbounded();
        (Sink(sender), receiver)
    }

    /// An input sent from an effect working for an awaited input is counted toward it.
    pub fn send(&self, inbound: Inbound) {
        let inbound = match (inbound, tracking::current()) {
            (Inbound::Input(input), Some(tracker)) => Inbound::Tracked(input, tracker),
            (inbound, _) => inbound,
        };
        let _ = self.0.unbounded_send(inbound);
    }

    pub fn event(&self, event: WorkspaceEvent) {
        self.send(Inbound::Input(WorkspaceInput::Event(event)));
    }

    pub fn command(&self, command: AppCommand) {
        self.send(Inbound::Input(WorkspaceInput::Command(command)));
    }

    /// An event written as the TS app's JSON, so host types convert by their shared wire shape.
    pub fn json(&self, value: serde_json::Value) {
        match serde_json::from_value::<WorkspaceEvent>(value.clone()) {
            Ok(event) => self.event(event),
            Err(error) => eprintln!("dropped an unreadable event {}: {error}", value.get("type").and_then(|kind| kind.as_str()).unwrap_or("?")),
        }
    }

    pub fn failed(&self, message: impl Into<String>) {
        self.event(WorkspaceEvent::ActionFailed { message: message.into(), page: None });
    }

    pub fn open_arguments(&self, arguments: Vec<String>) {
        self.send(Inbound::Arguments(arguments));
    }
}

/// Converts between two types that write the same JSON.
pub fn convert<From: Serialize, To: DeserializeOwned>(value: &From) -> Result<To, String> {
    serde_json::to_value(value).and_then(serde_json::from_value).map_err(|error| error.to_string())
}

/// The long-lived services, shared by every effect.
pub struct Host {
    pub profile: Profile,
    pub services: Arc<HostServices>,
    pub requests: Arc<requests::WindowRequests>,
    pub computers: computers::Computers,
    pub system: system::System,
    pub sink: Sink,
    pub inbox: Mutex<Option<UnboundedReceiver<Inbound>>>,
    pub database: OnceLock<TaskDatabaseService>,
    pub storage: Mutex<JsonStorage>,
    pub terminals: TerminalHost,
    pub runs: runs::Runs,
    pub engines: engines::Engines,
    pub automations: automations::Automations,
    refreshes: Mutex<HashMap<String, Option<EffectWork>>>,
    directory_request: Mutex<u64>,
    lock: Mutex<Option<DesktopLock>>,
    headless: std::sync::atomic::AtomicBool,
}

impl Host {
    pub fn new(profile: Profile, services: HostServices, _cx: &mut gpui::App) -> Arc<Host> {
        let (sink, inbox) = Sink::channel();
        let terminal_sink = sink.clone();
        let terminals = TerminalHost::new(move |update| match convert(&update) {
            Ok(update) => terminal_sink.event(WorkspaceEvent::TerminalUpdated { update }),
            Err(error) => eprintln!("terminal update: {error}"),
        });
        let storage = JsonStorage::open(profile.window_storage());
        let runs = runs::Runs::new(&profile, sink.clone());
        Arc::new_cyclic(|me| {
            let requests = Arc::new(requests::WindowRequests::new(sink.clone(), me.clone()));
            Host {
            computers: computers::Computers::new(requests.clone()),
            requests,
            system: system::System::new(),
            engines: engines::Engines::new(),
            automations: automations::Automations::default(),
            runs,
            terminals,
            storage: Mutex::new(storage),
            database: OnceLock::new(),
            inbox: Mutex::new(Some(inbox)),
            refreshes: Mutex::new(HashMap::new()),
            directory_request: Mutex::new(0),
            lock: Mutex::new(None),
            headless: std::sync::atomic::AtomicBool::new(false),
            profile,
            services: Arc::new(services),
            sink,
        }
        })
    }

    /// Whether this process runs with no window, as `aic serve` does.
    pub fn headless(&self) -> bool {
        self.headless.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn set_headless(&self) {
        self.headless.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn hold_lock(&self, lock: Option<DesktopLock>) {
        *self.lock.lock().unwrap() = lock;
    }

    /// Runs `work` on the host runtime.
    pub fn spawn<F>(&self, work: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        act_host::runtime().spawn(work);
    }

    /// Carries out one effect off the window's thread. Effects that need the window are the
    /// workspace's own and never reach here.
    pub fn perform(self: &Arc<Self>, work: EffectWork) {
        self.perform_tracked(work, None);
    }

    /// As `perform`, with what the effect sends back counted toward the input `tracker` follows.
    pub fn perform_tracked(self: &Arc<Self>, work: EffectWork, tracker: Option<tracking::Tracker>) {
        let host = self.clone();
        match work {
            EffectWork::StartRun { command } => self.runs.start(self.clone(), *command),
            EffectWork::SendRunCommand { command } => self.runs.command(command),
            EffectWork::EngineReloadSettings => self.runs.reload_settings(),
            EffectWork::TerminalStart { terminal_id, cwd } => terminal::start(&self.terminals, &terminal_id, &cwd),
            EffectWork::TerminalWrite { terminal_id, data } => terminal::write(&self.terminals, &self.sink, &terminal_id, &data),
            EffectWork::TerminalResize { terminal_id, cols, rows } => terminal::resize(&self.terminals, &terminal_id, cols, rows),
            EffectWork::TerminalClose { terminal_id } => self.terminals.close(&terminal_id),
            EffectWork::RefreshEnvironment { .. } => self.refresh_environment(work),
            EffectWork::DictationStart { .. } | EffectWork::DictationStop { .. } | EffectWork::DictationCancel { .. } | EffectWork::DictationDownload | EffectWork::DictationCancelDownload => {
                dictation::perform(&self.sink, &work);
            }
            work => self.spawn(tracking::CURRENT.scope(tracker, async move { host.perform_async(work).await })),
        }
    }

    /// A content read (`diff-patch`, `branches`, `message-image`, `commands`, ...) answered by the
    /// computer holding what it names: this one, or the paired one holding the thread or checkout.
    pub async fn read_content(&self, query: serde_json::Value, owner: act_host::computers::queries::ReadOwner) -> Result<serde_json::Value, String> {
        match self.computers.reads() {
            Some(reads) => reads.read(query, owner).await,
            None => Err("This computer is still starting.".into()),
        }
    }

    /// A checkout's branches, read on whichever computer holds it.
    pub async fn branches(&self, workspace_id: String) -> Result<act_host::git::Branches, String> {
        let query = serde_json::json!({ "kind": "branches", "workspaceId": workspace_id });
        let value = self.read_content(query, act_host::computers::queries::ReadOwner::Workspace(workspace_id)).await?;
        if value.get("status").and_then(serde_json::Value::as_str) != Some("available") {
            return Err(value.get("message").and_then(serde_json::Value::as_str).unwrap_or("Branches unavailable").to_string());
        }
        let strings = |key: &str| value.get(key).and_then(serde_json::Value::as_array).map(|items| items.iter().filter_map(|item| item.as_str().map(String::from)).collect()).unwrap_or_default();
        Ok(act_host::git::Branches { branches: strings("branches"), remotes: strings("remotes"), current: value.get("current").and_then(serde_json::Value::as_str).map(String::from) })
    }

    /// One file's patch, read on whichever computer holds the checkout.
    pub async fn diff_patch(&self, workspace_id: String, range: &act_host::git::diff::DiffRange, path: &str, previous: Option<&str>, ignore_whitespace: bool) -> act_host::git::diff::DiffPatchResult {
        let mut query = serde_json::json!({ "kind": "diff-patch", "workspaceId": workspace_id, "range": range, "path": path, "ignoreWhitespace": ignore_whitespace });
        if let Some(previous) = previous {
            query["previousPath"] = serde_json::json!(previous);
        }
        match self.read_content(query, act_host::computers::queries::ReadOwner::Workspace(workspace_id)).await {
            Ok(value) => serde_json::from_value(value).unwrap_or_else(|error| act_host::git::diff::DiffPatchResult::Error { message: error.to_string() }),
            Err(message) => act_host::git::diff::DiffPatchResult::Error { message },
        }
    }

    /// The window's own browser bridge for a thread's tools, once the window has one.
    pub fn browser_bridge(&self, task_id: &str) -> Option<Arc<dyn act_host::tools::browser::BrowserBridge>> {
        crate::workspace::devices::BROWSER_TOOLS.get().map(|tools| tools.bridge(task_id))
    }

    /// The simulator bridge for a thread's tools, once the window has one.
    pub fn simulator_bridge(&self, task_id: &str) -> Option<Arc<dyn act_host::tools::simulator::SimulatorBridge>> {
        crate::workspace::devices::SIMULATOR.get()?;
        Some(act_host::thread_requests::RunBridges::new(self.requests.clone(), task_id).simulator())
    }

    async fn perform_async(self: Arc<Self>, work: EffectWork) {
        let sink = self.sink.clone();
        match work {
            EffectWork::ResolveRunWorkspace { .. } => runs::resolve_run_workspace(&self, work).await,
            EffectWork::SuggestTitle { task_id, engine, model, text, attachments } => runs::suggest_title(&sink, task_id, engine, model, text, attachments).await,
            EffectWork::LoadSubagentMetadata { task_id, subagent_id, engine, session_id } => runs::subagent_metadata(&sink, task_id, subagent_id, engine, session_id).await,
            EffectWork::LoadSubagentActivity { task_id, subagent_id } => store::subagent_activity(&self, task_id, subagent_id).await,
            EffectWork::PreserveMessageImages { text, root, message_id } => desktop::preserve_message_images(&self, &text, &root, &message_id).await,
            EffectWork::AddProject { root, request, start } => projects::add_project(&self, root, request, start).await,
            EffectWork::RegisterProject { project_id, root } => projects::register_project(&self, project_id, root).await,
            EffectWork::ProjectAddDirectories { prefix, computer_id: _, request } => {
                let mine = {
                    let mut current = self.directory_request.lock().unwrap();
                    *current += 1;
                    *current
                };
                tokio::time::sleep(std::time::Duration::from_millis(180)).await;
                if *self.directory_request.lock().unwrap() != mine {
                    return;
                }
                projects::list_directories(&self, prefix, request).await;
            }
            EffectWork::CreateWorktree { task_id, project_root, r#move, name, project_id, then } => projects::create_worktree(&self, task_id, project_root, r#move, name, project_id, then).await,
            EffectWork::ReleaseWorktree { task_id, worktree_id, root, title } => projects::release_worktree(&self, task_id, worktree_id, root, title).await,
            EffectWork::DeleteWorktree { worktree_id, root, title, missing_only } => projects::delete_worktree(&self, worktree_id, root, title, missing_only).await,
            EffectWork::ListWorktrees => projects::list_worktrees(&self).await,
            EffectWork::RevealWorktree { root } => projects::reveal_worktree(&self, root).await,
            EffectWork::CheckoutBranch { workspace_id, branch, create } => projects::checkout_branch(&self, workspace_id, branch, create).await,
            EffectWork::ReadDiff { owner, workspace_id, range, ignore_whitespace } => projects::read_diff(&self, owner, workspace_id, range, ignore_whitespace).await,
            EffectWork::ReadPullRequest { workspace_id, branch, read, members } => projects::read_pull_request(&self, workspace_id, branch, read, members).await,
            EffectWork::EngineRead { refresh } => self.engines.read(&sink, refresh.unwrap_or(false)).await,
            EffectWork::EngineUpdate { engine, computer_id: _ } => self.engines.update(&sink, engine).await,
            EffectWork::EngineSignIn { engine } => self.engines.sign_in(&sink, engine).await,
            EffectWork::ReadPlanUsage { engine, read } => engines::plan_usage(&sink, engine, read).await,
            EffectWork::AutomationSave { draft } => self.automations.save(&sink, draft.task_id, draft.draft).await,
            EffectWork::AutomationUpdate { task_id, patch } => self.automations.update(&sink, task_id, patch).await,
            EffectWork::AutomationDelete { task_id } => self.automations.delete(&sink, task_id).await,
            EffectWork::AutomationRunNow { task_id } => self.automations.run_now(&sink, task_id).await,
            EffectWork::AutomationAck { ack } => self.automations.acknowledge(ack),
            EffectWork::SendAttachments { task_id, steer, text, attachments } => desktop::send_attachments(&self, task_id, steer, text, attachments).await,
            EffectWork::FileOpen { roots, path, line } => desktop::open_file(&sink, roots, path, line).await,
            EffectWork::AppList => desktop::list_apps(&sink).await,
            EffectWork::AppOpenFolder { root, app_id } => desktop::open_folder_in_app(&sink, root, app_id).await,
            EffectWork::AnnounceThread { notice } => desktop::announce(&sink, notice),
            EffectWork::ImageDownload { source } => desktop::download_image(&self, source).await,
            EffectWork::DropAttachments { task_id, paths, held } => attachments::drop_files(&self, task_id, paths, held).await,
            EffectWork::PasteImages { task_id, images } => attachments::paste_images(&self, task_id, images).await,
            EffectWork::OpenLink { url } => desktop::open_link(&url),
            EffectWork::FindInTerminal { terminal_id: _, query: _, forward: _ } | EffectWork::StopFindInTerminal { terminal_id: _ } => {}
            EffectWork::SimulatorList => sink.json(serde_json::json!({ "type": "simulator.listed", "availability": { "status": "unavailable", "message": "The simulator panel is not available in this build." }, "devices": [] })),
            EffectWork::ComputerUseRead => self.system.computer_use_permissions(&sink, None).await,
            EffectWork::ComputerUseEnable { permission } => self.system.computer_use_permissions(&sink, Some(permission)).await,
            EffectWork::ComputerUseRestart => sink.send(Inbound::Window(requests::WindowWork::Relaunch { update: false })),
            EffectWork::CliRead => self.system.cli(&sink, None).await,
            EffectWork::CliInstall => self.system.cli(&sink, Some(true)).await,
            EffectWork::CliUninstall => self.system.cli(&sink, Some(false)).await,
            EffectWork::AppCheckForUpdates { quiet } => self.system.check_for_updates(quiet).await,
            EffectWork::AppDownloadUpdate { unattended } => self.system.download_update(unattended).await,
            EffectWork::AppInstallUpdate { unattended } => self.system.install_update(&sink, unattended).await,
            EffectWork::AppOpenSourceLicenses => desktop::open_licenses(&sink),
            work @ (EffectWork::ComputerDiscover
            | EffectWork::ComputerPair { .. }
            | EffectWork::ComputerForget { .. }
            | EffectWork::ComputerReconnect { .. }
            | EffectWork::ComputerRename { .. }
            | EffectWork::ComputerLabel { .. }
            | EffectWork::ComputerForward { .. }
            | EffectWork::RemoteSetEnabled { .. }
            | EffectWork::RemoteCreatePairingCode
            | EffectWork::RemoteRevokeDevice { .. }
            | EffectWork::RemoteRefresh) => self.computers.perform(&sink, &self.services.attachments, work).await,
            other => unsupported(&other),
        }
    }

    /// One Git scan per checkout. A tick during a slow scan replaces the one follow-up still needed.
    fn refresh_environment(self: &Arc<Self>, work: EffectWork) {
        let EffectWork::RefreshEnvironment { workspace_id, .. } = &work else { return };
        let workspace_id = workspace_id.clone();
        {
            let mut refreshes = self.refreshes.lock().unwrap();
            if let Some(slot) = refreshes.get_mut(&workspace_id) {
                *slot = Some(work);
                return;
            }
            refreshes.insert(workspace_id.clone(), None);
        }
        let host = self.clone();
        self.spawn(async move {
            let mut next = Some(work);
            while let Some(EffectWork::RefreshEnvironment { workspace_id, task_id, run_id }) = next {
                let result = act_host::git::changes::changed_files(&workspace_id, &host.services.workspaces).await;
                host.sink.json(serde_json::json!({ "type": "environment.updated", "workspaceId": workspace_id, "taskId": task_id, "runId": run_id, "result": result }));
                let mut refreshes = host.refreshes.lock().unwrap();
                next = refreshes.get_mut(&workspace_id).and_then(Option::take);
                if next.is_none() {
                    refreshes.remove(&workspace_id);
                }
            }
        });
    }

    /// Stops everything before the process leaves, which waits on nothing it started.
    pub fn shutdown_now(self: Arc<Self>) {
        let finished = act_host::runtime().block_on(async { tokio::time::timeout(std::time::Duration::from_secs(8), self.shutdown()).await });
        if finished.is_err() {
            eprintln!("Gave up waiting for the services to stop.");
        }
    }

    pub async fn shutdown(self: Arc<Self>) {
        dictation::stop();
        self.terminals.stop_all();
        self.runs.close();
        act_engines::engines::close_engine_services().await;
        self.computers.stop().await;
        self.system.stop().await;
        if let Some(database) = self.database.get() {
            let _ = database.flush().await;
            let _ = database.close().await;
        }
        self.lock.lock().unwrap().take();
    }
}

/// Effects whose capability this build does not have yet: browser, simulator input, dictation,
/// paired computers and the phone bridge.
fn unsupported(work: &EffectWork) {
    let name = serde_json::to_value(work).ok().and_then(|value| value.get("type").and_then(|kind| kind.as_str()).map(String::from)).unwrap_or_default();
    eprintln!("effect not available in this build: {name}");
}
