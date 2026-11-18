//! What the app does for itself rather than for a thread: computer use, the terminal command, its own
//! updates, keeping a locked Mac awake for work, and restarting (src/host/system-effects.ts,
//! update-effects.ts, src/main/computer-use-host.ts, cli-install.ts, lock-awake.ts, updates.ts).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use act_core::domain::platform::ComputerUsePermission;
use act_core::WorkspaceState;
use act_engines::types::ComputerUseRunConfig;
use act_host::cli_install::CliInstaller;
use act_host::computer_use_host::{driver_binary, ComputerUseHost};
use act_host::lock_awake::LockAwake;
use act_host::updates::{AppUpdater, UpdateNotice, UpdaterOptions};
use serde_json::json;

use super::{convert, Inbound, Sink};

/// What the app's own host keeps for its whole life.
pub struct System {
    pub computer_use: Arc<ComputerUseHost>,
    cli: CliInstaller,
    updater: OnceLock<Arc<AppUpdater>>,
    lock_awake: Mutex<Option<LockAwake>>,
    /// Whether anything is going a locked Mac should stay awake for.
    work_ahead: Arc<AtomicBool>,
    /// Arguments the next launch is started with once this one has quit, when a restart is asked for.
    restart: Mutex<Option<Vec<String>>>,
}

fn executable() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// Where the bundled driver is: a packaged app's resources, or the source checkout's vendor folder.
pub(crate) fn computer_use_driver() -> Option<PathBuf> {
    let exe = executable()?;
    let resources = exe.parent().and_then(|macos| macos.parent()).map(|contents| contents.join("Resources"));
    let source = std::env::var_os("ACT_SOURCE_ROOT").map(PathBuf::from).or_else(|| exe.ancestors().find(|dir| dir.join("vendor/cua-driver/cua-driver").is_file()).map(PathBuf::from));
    let found = driver_binary(resources.as_deref(), source.as_deref());
    found.or_else(|| std::env::var_os("ACT_CUA_DRIVER").map(PathBuf::from).filter(|path| path.is_file())).or_else(|| act_host::open::on_path("cua-driver"))
}

/// A short private folder for the driver's socket; a Unix socket path holds about 104 bytes.
fn socket_dir() -> PathBuf {
    let uid = unsafe { libc_getuid() };
    PathBuf::from(format!("/tmp/aic-{uid}"))
}

extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

impl System {
    pub fn new() -> System {
        let exe = executable().map(|path| path.to_string_lossy().into_owned());
        System {
            computer_use: ComputerUseHost::new(computer_use_driver(), socket_dir()),
            cli: CliInstaller::for_this_machine(exe.as_deref()),
            updater: OnceLock::new(),
            lock_awake: Mutex::new(None),
            work_ahead: Arc::new(AtomicBool::new(false)),
            restart: Mutex::new(None),
        }
    }

    /// Starts what runs from launch to quit: the terminal command's upkeep, the update checks, and
    /// the watch on the screen lock.
    /// `desktop` is the window's launch; `aic serve` neither claims links nor holds a locked Mac awake.
    pub fn start(&self, sink: &Sink, packaged: bool, desktop: bool) {
        let _guard = act_host::runtime().enter();
        act_host::notify::set_notification_application(&notification_sender());
        #[cfg(target_os = "macos")]
        if packaged && desktop {
            if let Some(bundle_id) = bundle_identifier() {
                act_host::cli_install::claim_url_scheme(&bundle_id);
            }
        }
        if packaged {
            let cli = self.cli.clone();
            act_host::runtime().spawn(async move {
                if let Err(error) = cli.refresh().await {
                    eprintln!("Could not refresh the {} command: {error}", act_core::identity::CLI_COMMAND);
                }
            });
        }
        if cfg!(target_os = "macos") && desktop {
            let work_ahead = self.work_ahead.clone();
            *self.lock_awake.lock().unwrap() = Some(LockAwake::start(move || work_ahead.load(Ordering::SeqCst)));
        }
        let state_sink = sink.clone();
        let notice_sink = sink.clone();
        let checking_sink = sink.clone();
        let updater = AppUpdater::new(UpdaterOptions {
            version: crate::startup::APP_VERSION.to_string(),
            target: executable().as_deref().and_then(act_host::updates::update_target),
            packaged,
            releases: std::env::var("ACT_RELEASES").ok(),
            on_state: Box::new(move |update| state_sink.json(json!({ "type": "app.update-changed", "update": update }))),
            on_checking: Box::new(move |checking| checking_sink.send(Inbound::Window(super::requests::WindowWork::UpdateChecking(checking)))),
            on_notice: Box::new(move |notice| notice_sink.send(Inbound::Window(super::requests::WindowWork::UpdateNotice(notice)))),
            say: Box::new(|line| eprintln!("{line}")),
        });
        sink.json(json!({ "type": "app.build", "build": updater.build() }));
        sink.json(json!({ "type": "app.update-changed", "update": updater.update() }));
        if packaged {
            updater.start();
        }
        let _ = self.updater.set(updater);
    }

    /// Looks at the work again after a change; a locked Mac stays awake while any is ahead.
    pub fn work_changed(&self, state: &WorkspaceState) {
        let ahead = !state.active_runs.is_empty() || state.automations.iter().any(|view| view.next_run_at.is_some()) || state.threads.iter().any(|thread| thread.limit_pause.is_some());
        if self.work_ahead.swap(ahead, Ordering::SeqCst) != ahead {
            if let Some(lock) = self.lock_awake.lock().unwrap().as_ref() {
                lock.update();
            }
        }
    }

    pub async fn computer_use_for_run(&self) -> ComputerUseRunConfig {
        let config = self.computer_use.for_run().await;
        convert(&config).unwrap_or(ComputerUseRunConfig::Unavailable { message: "Computer use is unavailable.".into() })
    }

    pub async fn computer_use_permissions(&self, sink: &Sink, asked: Option<ComputerUsePermission>) {
        let permissions = match asked.map(|permission| convert(&permission)) {
            Some(Ok(permission)) => self.computer_use.request(permission).await,
            Some(Err(error)) => return sink.json(json!({ "type": "computer-use.failed", "message": error, "enabling": true })),
            None => self.computer_use.permissions().await,
        };
        let mut event = json!({ "type": "computer-use.permissions", "permissions": permissions });
        if asked.is_some() {
            event["enabling"] = json!(true);
        }
        sink.json(event);
    }

    pub async fn cli(&self, sink: &Sink, change: Option<bool>) {
        let status = match change {
            None => self.cli.status().await,
            Some(true) => self.cli.install().await,
            Some(false) => self.cli.uninstall().await,
        };
        match status {
            Ok(status) => sink.json(json!({ "type": "cli.read-status", "status": status })),
            Err(message) => sink.json(json!({ "type": "cli.failed", "message": message })),
        }
    }

    pub async fn check_for_updates(&self, quiet: bool) {
        let Some(updater) = self.updater.get().cloned() else { return };
        let stage = serde_json::to_value(updater.update()).ok().and_then(|update| update.get("status").and_then(|status| status.as_str()).map(String::from));
        if quiet && matches!(stage.as_deref(), Some("downloading" | "ready")) {
            return;
        }
        updater.check(!quiet).await;
    }

    pub async fn download_update(&self, unattended: bool) {
        if let Some(updater) = self.updater.get().cloned() {
            updater.download(unattended).await;
        }
    }

    /// Installs the downloaded build and starts it once this process quits. An attended install
    /// asks first, from the window.
    pub async fn install_update(&self, sink: &Sink, unattended: bool) {
        let Some(updater) = self.updater.get().cloned() else { return };
        if !unattended {
            let version = serde_json::to_value(updater.update()).ok().and_then(|update| update.get("version").and_then(|version| version.as_str()).map(String::from));
            if let Some(version) = version {
                sink.send(Inbound::Window(super::requests::WindowWork::UpdateNotice(UpdateNotice::ReadyToInstall { version })));
            }
            return;
        }
        self.install_now(sink).await;
    }

    /// Swaps the build in, notes which threads to carry on, and quits into the new one.
    pub async fn install_now(&self, sink: &Sink) {
        let Some(updater) = self.updater.get().cloned() else { return };
        match updater.install().await {
            Ok(_) => sink.send(Inbound::Window(super::requests::WindowWork::Relaunch { update: true })),
            Err(message) => eprintln!("Could not install the update: {message}"),
        }
    }

    /// Asks for this app to start again once it has quit.
    pub fn request_restart(&self, arguments: Vec<String>) {
        *self.restart.lock().unwrap() = Some(arguments);
    }

    /// Starts the next launch after this process exits, when one was asked for.
    pub fn relaunch_if_asked(&self) {
        let Some(arguments) = self.restart.lock().unwrap().take() else { return };
        let Some(exe) = executable() else { return };
        let target = act_host::updates::update_target(&exe);
        let result = match target {
            Some(target) => act_host::updates::relaunch(&target, &arguments),
            None => act_platform::process::start_after_exit(&exe.to_string_lossy(), &arguments),
        };
        if let Err(error) = result {
            eprintln!("Could not restart: {error}");
        }
    }

    pub fn updated_version(&self) -> Option<String> {
        let updater = self.updater.get()?;
        let update = serde_json::to_value(updater.update()).ok()?;
        update.get("version").and_then(|version| version.as_str()).map(String::from)
    }

    pub async fn stop(&self) {
        if let Some(lock) = self.lock_awake.lock().unwrap().take() {
            lock.stop();
        }
        if let Some(updater) = self.updater.get() {
            updater.dispose().await;
        }
        self.computer_use.stop().await;
    }
}

/// The bundle notifications are sent as: this app's own, or the Terminal's for a bare binary,
/// which has no bundle for the system to name.
fn notification_sender() -> String {
    bundle_identifier().unwrap_or_else(|| "com.apple.Terminal".into())
}

/// The identifier of the bundle this binary runs from, if it runs from one.
fn bundle_identifier() -> Option<String> {
    let bundle = executable().and_then(|exe| exe.parent()?.parent().map(|contents| contents.join("Info.plist")));
    bundle.filter(|plist| plist.is_file()).and_then(|plist| act_platform::tools::bundle_identifier(&plist))
}

/// Threads whose run an update would cut off, to carry on in the new build (application/update-relaunch.ts).
pub fn resumable_after_update(state: &WorkspaceState) -> Vec<String> {
    let open: std::collections::HashSet<&str> = state.threads.iter().filter(|thread| thread.run_open).map(|thread| thread.id.as_str()).collect();
    state
        .active_runs
        .values()
        .filter(|run| open.contains(run.task_id.as_str()) && run.phase != act_core::state::RunPhase::AwaitingApproval && run.questions.as_ref().is_none_or(|questions| questions.is_empty()) && !run.cancelling)
        .map(|run| run.task_id.clone())
        .collect()
}

/// What the user hears about an update they asked for, as a dialog over the window.
pub fn show_update_notice(host: &Arc<super::Host>, notice: UpdateNotice, cx: &mut gpui::App) {
    use act_core::identity::APP_NAME;
    use gpui::PromptLevel;
    let version = crate::startup::APP_VERSION;
    let (level, message, detail, buttons): (PromptLevel, String, Option<String>, Vec<&'static str>) = match &notice {
        UpdateNotice::UpToDate { version } => (PromptLevel::Info, format!("{APP_NAME} {version} is the latest version."), None, vec!["OK"]),
        UpdateNotice::CheckFailed => (PromptLevel::Warning, concat!(act_core::app_name!(), " could not check for updates.").into(), Some("Please try again in a moment.".into()), vec!["OK"]),
        UpdateNotice::InstallFailed { recovery } => (PromptLevel::Warning, concat!(act_core::app_name!(), " could not install the update.").into(), Some(recovery.to_string()), vec!["Open downloads", "Later"]),
        UpdateNotice::SourceCopy => (PromptLevel::Info, concat!("This copy of ", act_core::app_name!(), " runs from source.").into(), Some("Only the installed app updates itself.".into()), vec!["OK"]),
        UpdateNotice::ManualLinux => (PromptLevel::Info, "This Linux package is updated manually.".into(), Some("Download the latest package and install it the same way you installed this one.".into()), vec!["Open downloads", "Later"]),
        UpdateNotice::ReadyToInstall { version } => (PromptLevel::Info, format!("{APP_NAME} {version} is ready to install."), Some(concat!("Restart ", act_core::app_name!(), " to finish the update.").into()), vec!["Restart and install", "Later"]),
    };
    let _ = version;
    let Some(window) = cx.windows().into_iter().next() else { return };
    let answer = window.update(cx, |_, window, cx| window.prompt(level, &message, detail.as_deref(), &buttons, cx));
    let Ok(answer) = answer else { return };
    let host = host.clone();
    cx.spawn(async move |_| {
        if answer.await != Ok(0) {
            return;
        }
        match notice {
            UpdateNotice::InstallFailed { .. } | UpdateNotice::ManualLinux => {
                act_host::open::open_url(&act_host::updates::latest_release_url());
            }
            UpdateNotice::ReadyToInstall { .. } => {
                let sink = host.sink.clone();
                act_host::runtime().spawn(async move { host.system.install_now(&sink).await });
            }
            _ => {}
        }
    })
    .detach();
}
