//! Launch in the order Electron's main process uses: the login shell's PATH, the profile, the
//! single-instance lock, the host services, then the window with the store loading beside it.

use act_host::instance_lock::{claim_desktop_lock, serving_process, DesktopClaim, DesktopLock};
use act_host::services::HostServices;
use act_host::user_data::AppProfile;
use act_store::profile::Profile;
use gpui::{point, px, size, App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions};

use crate::effects::Host;
use crate::views::root::Root;
use crate::workspace::Workspace;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Everything made before the window opens.
pub struct Launch {
    pub profile: Profile,
    pub services: HostServices,
    pub lock: Option<DesktopLock>,
    pub launched_update: Option<String>,
}

/// Returns None when this launch should not open a window: another one holds the profile.
pub fn prepare() -> Option<Launch> {
    let runtime = act_host::runtime();
    // Asking the login shell takes about half a second, so the window does not wait for it.
    act_host::login_path::adopt_tool_folders();
    runtime.spawn(act_host::login_path::adopt_login_shell_path());
    let profile = Profile::resolve_for(packaged())?;
    import_electron_profile(&profile, packaged());
    if let Err(error) = profile.ensure() {
        eprintln!("Could not create the profile folder {}: {error}", profile.user_data.display());
        return None;
    }
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let lock = match claim_desktop_lock(&profile.user_data, &arguments) {
        Ok(DesktopClaim::Claimed(lock)) => Some(lock),
        Ok(DesktopClaim::Forwarded) | Ok(DesktopClaim::Held) => {
            println!("{} is already running. Bringing that window forward instead of starting a second one.", profile.name);
            return None;
        }
        Err(error) => {
            eprintln!("Could not claim the profile: {error}");
            None
        }
    };
    if let Some(process) = serving_process(&profile.user_data) {
        eprintln!("`{} serve` is running from this data folder (process {process}). Stop it before opening the app.", act_core::identity::CLI_COMMAND);
        return None;
    }
    let launched_update = act_host::user_data::read_app_launch(&profile.user_data, APP_VERSION);
    act_browser::set_session_dir(profile.user_data.join("Browser"));
    let services = open_services(&profile)?;
    Some(Launch { profile, services, lock, launched_update })
}

/// The engines' private homes and the workspace registry, shared by the window and `aic serve`.
pub fn open_services(profile: &Profile) -> Option<HostServices> {
    std::env::set_var(act_engines::codex::home::PRIVATE_CODEX_HOME_ENV, profile.codex_private());
    let app_profile = AppProfile { name: profile.name.clone(), user_data: profile.user_data.clone(), worktrees_root: profile.worktrees_root.clone() };
    match act_host::runtime().block_on(HostServices::open(app_profile)) {
        Ok(services) => Some(services),
        Err(error) => {
            eprintln!("Could not open the workspace registry: {error}");
            None
        }
    }
}

/// Names a profile folder to import from instead of the Electron app's own.
const IMPORT_FROM_ENV: &str = "ACT_IMPORT_FROM";

/// A first launch copies the Electron app's profile: the installed app's for an installed build, the
/// source checkout's for a source build. A profile named by `ACT_PROFILE_DIR` imports only from
/// `ACT_IMPORT_FROM`, so a scratch profile never picks up the user's data.
pub fn import_electron_profile(profile: &Profile, packaged: bool) {
    use act_store::import::{electron_sources, find_source, import_profile, is_empty_profile, ImportSource};
    if !is_empty_profile(&profile.user_data) {
        return;
    }
    let Some(home) = act_store::profile::home_dir() else { return };
    let Some(app_data) = act_store::profile::app_data_dir(Some(&home)) else { return };
    let mut sources = electron_sources(&app_data, &home, packaged);
    if let Some(folder) = std::env::var_os(IMPORT_FROM_ENV).filter(|folder| !folder.is_empty()) {
        let worktrees_root = sources[0].worktrees_root.clone();
        sources = vec![ImportSource { user_data: std::path::absolute(&folder).unwrap_or_else(|_| folder.into()), worktrees_root }];
    } else if std::env::var_os(act_store::profile::PROFILE_DIR_ENV).is_some_and(|folder| !folder.is_empty()) {
        return;
    }
    let Some(source) = find_source(&sources) else { return };
    match import_profile(source, &profile.user_data, APP_VERSION) {
        Ok(report) => println!("Imported {} threads and {} projects from {}.", report.threads, report.projects, source.user_data.display()),
        Err(error) => eprintln!("Could not import {}: {error}", source.user_data.display()),
    }
}

/// Whether this is an installed build, which owns the terminal command and updates itself.
pub fn packaged() -> bool {
    std::env::current_exe().ok().is_some_and(|exe| act_host::updates::update_target(&exe).is_some())
}

/// A running AppImage makes itself the handler of the app's URL scheme.
fn register_url_handler() {
    if !cfg!(target_os = "linux") || !packaged() {
        return;
    }
    let (Some(appimage), Some(home)) = (std::env::var_os("APPIMAGE"), std::env::var_os("HOME")) else { return };
    let data_home = std::env::var_os("XDG_DATA_HOME").map(std::path::PathBuf::from);
    std::thread::spawn(move || {
        let icon = include_bytes!("../../../assets/icon.png");
        if let Err(error) = act_host::linux_protocol::register_appimage(appimage.as_ref(), home.as_ref(), icon, data_home.as_deref(), None) {
            eprintln!("Could not register the AppImage URL handler: {error}");
        }
    });
}

/// Links that arrive before the host is up wait for it.
#[derive(Default)]
struct Opener {
    sink: Option<crate::effects::Sink>,
    waiting: Vec<String>,
}

pub fn run(launch: Launch) {
    let Launch { profile, services, lock, launched_update } = launch;
    let lock = std::sync::Mutex::new(lock);
    let opener: std::sync::Arc<std::sync::Mutex<Opener>> = Default::default();
    let application = Application::new().with_assets(act_ui::Assets);
    application.on_reopen(reopen);
    let url_opener = opener.clone();
    application.on_open_urls(move |urls| {
        let mut opener = url_opener.lock().unwrap();
        match &opener.sink {
            Some(sink) => sink.open_arguments(urls),
            None => opener.waiting.extend(urls),
        }
    });
    application.run(move |cx: &mut App| {
        act_ui::init(cx);
        act_browser::init(cx);
        act_markdown::bind_keys(cx);
        act_terminal::init(cx);
        crate::keys::bind(cx);
        std::thread::spawn(act_markdown::highlight::preload);

        let host = Host::new(profile.clone(), services, cx);
        let workspace = cx.new(|cx| Workspace::new(host.clone(), launched_update.clone(), cx));
        crate::workspace::install(workspace.clone(), cx);
        let state = workspace.read(cx).state.clone();
        crate::theme_bridge::apply(&state, cx);
        if let Some(lock) = lock.lock().unwrap().as_mut() {
            let sink = host.sink.clone();
            lock.listen(move |arguments| sink.open_arguments(arguments));
        }
        host.hold_lock(lock.lock().unwrap().take());
        {
            let mut opener = opener.lock().unwrap();
            opener.sink = Some(host.sink.clone());
            let waiting = std::mem::take(&mut opener.waiting);
            if !waiting.is_empty() {
                host.sink.open_arguments(waiting);
            }
        }

        register_url_handler();
        crate::zoom::init(&profile.user_data, cx);
        crate::app_menu::install(cx);
        crate::app_menu::watch_unread(cx);
        let opener = WindowOpener { workspace: workspace.clone(), title: profile.name.clone(), placement_file: crate::placement::file(&profile.user_data) };
        if let Err(error) = opener.open(cx) {
            eprintln!("Could not open the window: {error:#}");
            cx.quit();
            return;
        }
        let quits_with_last_window = !cfg!(target_os = "macos");
        cx.on_window_closed(move |cx| {
            if quits_with_last_window && cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        REOPEN.with(|reopen| *reopen.borrow_mut() = Some(opener));
        let quit_host = host.clone();
        let quit_workspace = workspace.clone();
        cx.on_app_quit(move |cx| {
            quit_workspace.update(cx, |workspace, _| workspace.flush());
            quit_host.clone().shutdown_now();
            quit_host.system.relaunch_if_asked();
            async {}
        })
        .detach();
        quit_on_signals(cx);
        cx.activate(true);
        Workspace::start(&workspace, cx);
        host.system.start(&host.sink, packaged(), true);
        host.computers.start(&host, true, crate::effects::computers::BridgeOptions::default());
        let launch_arguments: Vec<String> = std::env::args().skip(1).collect();
        if !launch_arguments.is_empty() {
            host.sink.open_arguments(launch_arguments);
        }
    });
}

/// SIGINT, SIGTERM and SIGHUP quit as the menu does, so services such as the computer-use driver
/// stop with the app; a second signal leaves at once.
fn quit_on_signals(cx: &mut App) {
    use tokio::signal::unix::{signal, SignalKind};
    let (quit_tx, mut quit_rx) = futures::channel::mpsc::unbounded::<()>();
    act_host::runtime().spawn(async move {
        let (Ok(mut interrupt), Ok(mut terminate), Ok(mut hangup)) = (signal(SignalKind::interrupt()), signal(SignalKind::terminate()), signal(SignalKind::hangup())) else { return };
        let mut asked = false;
        loop {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
                _ = hangup.recv() => {}
            }
            if asked {
                std::process::exit(1);
            }
            asked = true;
            let _ = quit_tx.unbounded_send(());
        }
    });
    cx.spawn(async move |cx| {
        if futures::StreamExt::next(&mut quit_rx).await.is_some() {
            let _ = cx.update(|cx| cx.quit());
        }
    })
    .detach();
}

thread_local! {
    static REOPEN: std::cell::RefCell<Option<WindowOpener>> = const { std::cell::RefCell::new(None) };
}

/// Clicking the Dock icon with no window open opens the window again; otherwise brings it forward.
fn reopen(cx: &mut App) {
    if let Some(window) = cx.windows().into_iter().find(|window| window.downcast::<Root>().is_some()) {
        let _ = window.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let opener = REOPEN.with(|reopen| reopen.borrow().clone());
    if let Some(opener) = opener {
        if let Err(error) = opener.open(cx) {
            eprintln!("Could not open the window: {error:#}");
        }
    }
}

#[derive(Clone)]
struct WindowOpener {
    workspace: gpui::Entity<Workspace>,
    title: String,
    placement_file: std::path::PathBuf,
}

impl WindowOpener {
    fn open(&self, cx: &mut App) -> anyhow::Result<()> {
        let fixed = window_size();
        let mut placement = None;
        let window_bounds = match fixed {
            Some((width, height)) => {
                // A fractional origin makes AppKit round the frame outward, a point taller than asked.
                let mut bounds = Bounds::centered(None, size(px(width), px(height)), cx);
                bounds.origin = point(bounds.origin.x.round(), bounds.origin.y.round());
                WindowBounds::Windowed(bounds)
            }
            None => {
                let screens: Vec<_> = cx.displays().iter().map(|display| display.bounds()).collect();
                let fitted = crate::placement::fit(crate::placement::load(&self.placement_file), &screens);
                let bounds = crate::placement::window_bounds(&fitted, cx);
                placement = Some(fitted);
                bounds
            }
        };
        let options = WindowOptions {
            window_bounds: Some(window_bounds),
            // macOS draws the content under an inset title bar; elsewhere the window keeps its native frame.
            titlebar: Some(TitlebarOptions { title: Some(self.title.clone().into()), appears_transparent: cfg!(target_os = "macos"), traffic_light_position: cfg!(target_os = "macos").then(|| point(px(12.0), px(11.0))) }),
            window_decorations: (!cfg!(target_os = "macos")).then_some(gpui::WindowDecorations::Server),
            window_min_size: Some(size(px(crate::placement::MIN_WIDTH), px(crate::placement::MIN_HEIGHT))),
            app_id: Some(act_host::linux_protocol::LINUX_APP_ID.into()),
            window_background: act_devices::glass::WindowMaterial::current().background(),
            ..Default::default()
        };
        let workspace = self.workspace.clone();
        let window = cx.open_window(options, |window, cx| cx.new(|cx| Root::new(workspace, window, cx)))?;
        if act_devices::glass::WindowMaterial::current() == act_devices::glass::WindowMaterial::Glass {
            let _ = window.update(cx, |_, window, _| act_devices::glass::attach(window));
        }
        if let Some(placement) = placement {
            crate::placement::watch(window, self.placement_file.clone(), &placement, cx);
        }
        Ok(())
    }
}

/// `ACT_WINDOW_SIZE=WxH` opens the window at a given size instead of where it was left, for comparing screens.
fn window_size() -> Option<(f32, f32)> {
    let value = std::env::var("ACT_WINDOW_SIZE").ok()?;
    let (width, height) = value.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}
