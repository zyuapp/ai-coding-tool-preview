//! Computer use: what the platform grants the app, and the bundled Cua Driver a run reaches the
//! desktop through. The driver runs as this app's child, so macOS answers its permission checks
//! with the app's own grants.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Child;
use tokio::sync::Mutex;

pub const HOST_BUNDLE_ID: &str = act_core::identity::BUNDLE_ID;

/// The identifier of the bundle this process runs from, which macOS grants permissions to; a build
/// run outside a bundle answers for the installed app's.
pub fn host_bundle_id() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let plist = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.parent()?.join("Info.plist")));
        plist
            .and_then(|plist| std::fs::read_to_string(plist).ok())
            .and_then(|plist| plist_string(&plist, "CFBundleIdentifier"))
            .unwrap_or_else(|| HOST_BUNDLE_ID.to_string())
    })
    .clone()
}

fn plist_string(plist: &str, key: &str) -> Option<String> {
    let tag = format!("<key>{key}</key>");
    let after = plist[plist.find(&tag)? + tag.len()..].trim_start().strip_prefix("<string>")?;
    let value = after[..after.find("</string>")?].trim();
    (!value.is_empty()).then(|| value.to_string())
}
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComputerUsePermission {
    Accessibility,
    ScreenRecording,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxRuntime {
    /// "available", "limited" or "unavailable".
    pub status: String,
    /// "x11", "xwayland", "wayland" or "none".
    pub display: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUsePermissions {
    pub accessibility: bool,
    pub screen_recording: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_runtime: Option<LinuxRuntime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerUseMcp {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ComputerUseRunConfig {
    Available { mcp: ComputerUseMcp },
    SetupRequired,
    Unavailable { message: String },
}

#[cfg(target_os = "macos")]
#[path = "mac.rs"]
mod os;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod os;

/// The bundled driver, which lives beside a packaged app's resources and under `vendor/` in a source checkout.
pub fn driver_binary(resources: Option<&Path>, source_root: Option<&Path>) -> Option<PathBuf> {
    let candidates = resources.map(|resources| resources.join("cua-driver")).into_iter().chain(source_root.map(|root| root.join("vendor/cua-driver/cua-driver")));
    candidates.into_iter().find(|candidate| candidate.is_file())
}

struct Daemon {
    child: Child,
    socket: PathBuf,
    /// Held open for the daemon's life: it exits when this app does and the pipe closes.
    _liveness: Option<tokio::process::ChildStdin>,
}

/// The app's computer-use side: permission checks and requests, and one private driver daemon that
/// every run's MCP proxy talks to.
pub struct ComputerUseHost {
    binary: Option<PathBuf>,
    socket_dir: PathBuf,
    daemon: Mutex<Option<Daemon>>,
    accepting: std::sync::atomic::AtomicBool,
}

impl ComputerUseHost {
    /// `binary` is the bundled `cua-driver`; `socket_dir` a private folder for the daemon's socket.
    pub fn new(binary: Option<PathBuf>, socket_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self { binary, socket_dir, daemon: Mutex::new(None), accepting: std::sync::atomic::AtomicBool::new(true) })
    }

    pub async fn permissions(&self) -> ComputerUsePermissions {
        os::permissions(self.binary.as_deref())
    }

    /// Asks the platform for one permission.
    pub async fn request(&self, permission: ComputerUsePermission) -> ComputerUsePermissions {
        os::request(permission);
        self.permissions().await
    }

    /// What a run is given: the driver's MCP proxy once the daemon is up, setup when macOS has yet to
    /// grant both permissions, or why it cannot have computer use.
    pub async fn for_run(&self) -> ComputerUseRunConfig {
        use std::sync::atomic::Ordering;
        if !self.accepting.load(Ordering::SeqCst) {
            return ComputerUseRunConfig::Unavailable { message: concat!(act_core::app_name!(), " is quitting.").into() };
        }
        let display = match os::capability() {
            Ok(display) => display,
            Err(message) => return ComputerUseRunConfig::Unavailable { message },
        };
        if os::NEEDS_GRANTS {
            let permissions = self.permissions().await;
            if !permissions.accessibility || !permissions.screen_recording {
                return ComputerUseRunConfig::SetupRequired;
            }
        }
        let Some(binary) = self.binary.clone().filter(|binary| binary.is_file()) else {
            return ComputerUseRunConfig::Unavailable { message: "The bundled CUA Driver executable is missing.".into() };
        };
        match self.ensure_daemon(&binary).await {
            Ok(socket) => ComputerUseRunConfig::Available { mcp: mcp_config(&binary, &socket) },
            Err(error) => ComputerUseRunConfig::Unavailable { message: os::start_failure(display, error) },
        }
    }

    async fn ensure_daemon(&self, binary: &Path) -> Result<PathBuf, String> {
        let mut daemon = self.daemon.lock().await;
        if let Some(running) = daemon.as_mut() {
            if running.child.try_wait().ok().flatten().is_none() {
                return Ok(running.socket.clone());
            }
            *daemon = None;
        }
        if !self.accepting.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(concat!(act_core::app_name!(), " is quitting.").into());
        }
        tokio::fs::create_dir_all(&self.socket_dir).await.map_err(|error| error.to_string())?;
        let _ = act_platform::fs::make_private_dir(&self.socket_dir);
        let name = format!("cua-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let socket = self.socket_dir.join(format!("{name}.sock"));
        let _ = tokio::fs::remove_file(&socket).await;
        // A file rather than a pipe: a daemon whose stderr reader died with the app keeps running.
        let log = self.socket_dir.join(format!("{name}.log"));
        let stderr = std::fs::File::create(&log).map_err(|error| format!("CUA Driver could not start: {error}"))?;
        let mut command = act_platform::process::command(binary);
        command
            .args(["serve", "--embedded", "--parent-liveness-stdio", "--no-permissions-gate", "--socket"])
            .arg(&socket)
            .args(["--host-bundle-id", &host_bundle_id(), "--permission-mode", "standard"])
            .env("CUA_DRIVER_EMBEDDED", "1")
            .env("CUA_DRIVER_HOST_BUNDLE_ID", host_bundle_id())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(stderr))
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| format!("CUA Driver could not start: {error}"))?;
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        loop {
            if act_platform::ipc::connect_async(&socket).await.is_ok() {
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                let stderr = tokio::fs::read_to_string(&log).await.unwrap_or_default();
                let _ = tokio::fs::remove_file(&log).await;
                let tail = stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("").trim().to_string();
                return Err(if tail.is_empty() { format!("CUA Driver exited with {status}.") } else { tail });
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill().await;
                let _ = tokio::fs::remove_file(&log).await;
                return Err("CUA Driver did not start in time.".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Only a failed start reads the daemon's diagnostics.
        let _ = tokio::fs::remove_file(&log).await;
        let liveness = child.stdin.take();
        *daemon = Some(Daemon { child, socket: socket.clone(), _liveness: liveness });
        Ok(socket)
    }

    /// Stops accepting runs and ends the daemon; awaited on quit.
    pub async fn stop(&self) {
        self.accepting.store(false, std::sync::atomic::Ordering::SeqCst);
        let taken = self.daemon.lock().await.take();
        if let Some(mut daemon) = taken {
            if let Some(pid) = daemon.child.id() {
                act_platform::process::signal(pid, act_platform::process::Signal::Terminate);
            }
            if tokio::time::timeout(SHUTDOWN_TIMEOUT, daemon.child.wait()).await.is_err() {
                let _ = daemon.child.kill().await;
            }
            let _ = tokio::fs::remove_file(&daemon.socket).await;
        }
    }

    /// A cancelled quit lets the next run start a fresh daemon.
    pub fn resume(&self) {
        self.accepting.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// How a run's engine starts the stdio MCP proxy onto the daemon.
pub fn mcp_config(binary: &Path, socket: &Path) -> ComputerUseMcp {
    ComputerUseMcp {
        command: binary.to_string_lossy().into_owned(),
        args: vec!["mcp".into(), "--embedded".into(), "--socket".into(), socket.to_string_lossy().into_owned(), "--host-bundle-id".into(), host_bundle_id()],
        env: BTreeMap::from([("CUA_DRIVER_EMBEDDED".to_string(), "1".to_string())]),
    }
}
