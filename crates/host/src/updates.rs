//! The app's own updates: finds the newest release on GitHub, downloads and checks it, swaps it in
//! place of the installed app, and starts the new build once this one has quit.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use act_core::domain::platform::{AppBuild, AppUpdate};
use act_platform::Platform;
use base64::Engine;
use futures::StreamExt;
use sha2::{Digest, Sha512};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::user_data::compare_versions;

pub const RELEASES: &str = act_core::identity::RELEASES_URL;

/// The newest release's page.
pub fn latest_release_url() -> String {
    format!("{RELEASES}/latest")
}
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const RELAUNCH_RECORD: &str = "update-relaunch.json";

/// What an update replaces: the macOS bundle the binary sits in, or the AppImage that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateTarget {
    Bundle(PathBuf),
    AppImage(PathBuf),
}

/// What this process would replace, or None for a copy that cannot replace itself, such as a source build.
pub fn update_target(executable: &Path) -> Option<UpdateTarget> {
    match Platform::current() {
        Platform::Darwin => {
            let bundle = executable.parent()?.parent()?.parent()?;
            bundle.extension().is_some_and(|extension| extension == "app").then(|| UpdateTarget::Bundle(bundle.to_path_buf()))
        }
        Platform::Linux => std::env::var_os("APPIMAGE").map(|image| UpdateTarget::AppImage(PathBuf::from(image))),
        Platform::Windows => None,
    }
}

/// What a user who asked for a check is told, for the window to show as a dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateNotice {
    UpToDate { version: String },
    /// The check itself failed.
    CheckFailed,
    /// A download or install failed; the dialog offers [`latest_release_url`].
    InstallFailed { recovery: &'static str },
    /// A source build has no installer behind it.
    SourceCopy,
    /// A Linux package that is not an AppImage is updated by the user's package workflow.
    ManualLinux,
    /// A downloaded build waits on a restart; the window asks before installing it.
    ReadyToInstall { version: String },
}

/// How a user replaces this app by hand when the update could not.
pub fn manual_update_recovery() -> &'static str {
    match Platform::current() {
        Platform::Darwin => "Download the new version and replace the app in Applications.",
        Platform::Linux => "Download the new AppImage and replace the one you run.",
        Platform::Windows => "Download the new version and replace the installed app.",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFile {
    pub url: String,
    pub sha512: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseManifest {
    pub version: String,
    pub files: Vec<ReleaseFile>,
}

fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && (bytes[0] == b'\'' || bytes[0] == b'"') && bytes[bytes.len() - 1] == bytes[0] {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// Reads the `latest-*.yml` electron-builder publishes: its version and the files it lists.
pub fn parse_release_manifest(text: &str) -> Option<ReleaseManifest> {
    let version = text.lines().find_map(|line| line.strip_prefix("version:").map(str::trim).filter(|value| !value.is_empty() && !value.contains(char::is_whitespace)))?;
    let mut files: Vec<(Option<String>, Option<String>, Option<f64>)> = Vec::new();
    let mut current = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indented = trimmed.len() < line.len();
        if indented {
            if let Some(url) = trimmed.strip_prefix("- url:").map(str::trim).filter(|value| !value.is_empty() && !value.contains(char::is_whitespace)) {
                files.push((Some(unquote(url).to_string()), None, None));
                current = true;
                continue;
            }
            if current {
                if let Some(size) = trimmed.strip_prefix("size:").map(str::trim) {
                    files.last_mut().unwrap().2 = size.parse::<f64>().ok();
                } else if let Some(hash) = trimmed.strip_prefix("sha512:").map(str::trim) {
                    files.last_mut().unwrap().1 = Some(unquote(hash).to_string());
                }
            }
        } else {
            current = false;
        }
    }
    let files = files
        .into_iter()
        .filter_map(|(url, sha512, size)| match (url, sha512, size) {
            (Some(url), Some(sha512), Some(size)) if !url.is_empty() && !sha512.is_empty() && size.is_finite() && size >= 0.0 => Some(ReleaseFile { url, sha512, size: size as u64 }),
            _ => None,
        })
        .collect();
    Some(ReleaseManifest { version: unquote(version).to_string(), files })
}

fn manifest_name() -> &'static str {
    match Platform::current() {
        Platform::Darwin => "latest-mac.yml",
        Platform::Linux | Platform::Windows if cfg!(target_arch = "aarch64") => "latest-linux-arm64.yml",
        Platform::Linux | Platform::Windows => "latest-linux.yml",
    }
}

fn release_file<'a>(manifest: &'a ReleaseManifest, target: &UpdateTarget) -> Option<&'a ReleaseFile> {
    let suffix = match target {
        UpdateTarget::Bundle(_) => ".zip",
        UpdateTarget::AppImage(_) => ".AppImage",
    };
    manifest.files.iter().find(|file| file.url.ends_with(suffix))
}

/// Accepts only a bundle signed by whoever signed the one it replaces, as Squirrel does.
pub async fn verify_bundle(current: &Path, next: &Path) -> Result<(), String> {
    let shown = act_platform::tools::signature_details(current).await.map_err(|error| error.to_string())?;
    let text = format!("{}\n{}", shown.stdout_text(), String::from_utf8_lossy(&shown.stderr));
    let requirement = text.lines().find_map(|line| line.strip_prefix("designated => ")).ok_or("The installed app has no signature to check the update against.")?;
    let requirement = format!("={requirement}");
    act_platform::tools::verify_signature(next, &requirement).await.map_err(|error| format!("The update is not signed like the installed app: {error}"))?;
    Ok(())
}

struct Staged {
    version: String,
    path: PathBuf,
    cleanup: PathBuf,
}

#[derive(Clone)]
struct Source {
    releases: String,
    client: reqwest::Client,
    stopping: CancellationToken,
}

impl Source {
    async fn get(&self, url: &str) -> Result<reqwest::Response, String> {
        let request = self.client.get(url).send();
        let response = tokio::select! {
            response = request => response.map_err(|error| error.to_string())?,
            _ = self.stopping.cancelled() => return Err("Cancelled.".into()),
        };
        if !response.status().is_success() {
            return Err(format!("{url} answered {}.", response.status().as_u16()));
        }
        Ok(response)
    }

    async fn manifest(&self, target: &UpdateTarget) -> Result<ReleaseManifest, String> {
        let name = manifest_name();
        let text = self.get(&format!("{}/latest/download/{name}", self.releases)).await?.text().await.map_err(|error| error.to_string())?;
        parse_release_manifest(&text).filter(|manifest| release_file(manifest, target).is_some()).ok_or_else(|| format!("{name} names no build for this computer."))
    }

    /// Streams the file to `destination`, hashing as it goes, then checks it whole.
    async fn fetch(&self, manifest: &ReleaseManifest, file: &ReleaseFile, destination: &Path, progress: &(dyn Fn(i64) + Send + Sync)) -> Result<(), String> {
        let url = if file.url.starts_with("https://") { file.url.clone() } else { format!("{}/download/v{}/{}", self.releases, manifest.version, crate::tools::browser::encode_uri_component(&file.url)) };
        let response = self.get(&url).await?;
        let mut options = tokio::fs::OpenOptions::new();
        act_platform::fs::executable(options.write(true).create_new(true));
        let mut out = options.open(destination).await.map_err(|error| error.to_string())?;
        let mut hash = Sha512::new();
        let mut received: u64 = 0;
        let mut stream = response.bytes_stream();
        loop {
            let chunk = tokio::select! {
                chunk = stream.next() => chunk,
                _ = self.stopping.cancelled() => return Err("Cancelled.".into()),
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|error| error.to_string())?;
            hash.update(&chunk);
            received += chunk.len() as u64;
            progress(((received as f64 / file.size.max(1) as f64) * 100.0).floor().min(100.0) as i64);
            out.write_all(&chunk).await.map_err(|error| error.to_string())?;
        }
        out.flush().await.map_err(|error| error.to_string())?;
        let digest = base64::engine::general_purpose::STANDARD.encode(hash.finalize());
        if received != file.size || digest != file.sha512 {
            return Err(format!("{} did not match its checksum.", file.url));
        }
        Ok(())
    }

    /// Downloads the build beside what it replaces, so the swap is a rename on one volume.
    async fn stage(&self, target: &UpdateTarget, manifest: &ReleaseManifest, progress: &(dyn Fn(i64) + Send + Sync)) -> Result<Staged, String> {
        let file = release_file(manifest, target).ok_or("No build for this computer.")?;
        match target {
            UpdateTarget::AppImage(image) => {
                let name = image.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                let destination = image.with_file_name(format!(".{name}.{}.download", uuid::Uuid::new_v4()));
                if let Err(error) = self.fetch(manifest, file, &destination, progress).await {
                    let _ = tokio::fs::remove_file(&destination).await;
                    return Err(error);
                }
                Ok(Staged { version: manifest.version.clone(), path: destination.clone(), cleanup: destination })
            }
            UpdateTarget::Bundle(bundle) => {
                let download = std::env::temp_dir().join(format!("aic-update-{}", uuid::Uuid::new_v4()));
                tokio::fs::create_dir_all(&download).await.map_err(|error| error.to_string())?;
                let name = bundle.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                let unpacked = bundle.with_file_name(format!(".{name}.{}.update", uuid::Uuid::new_v4()));
                let result = async {
                    let archive = download.join("update.zip");
                    self.fetch(manifest, file, &archive, progress).await?;
                    act_platform::tools::extract_zip(&archive, &unpacked).await.map_err(|error| error.to_string())?;
                    let mut entries = tokio::fs::read_dir(&unpacked).await.map_err(|error| error.to_string())?;
                    let mut app = None;
                    while let Some(entry) = entries.next_entry().await.map_err(|error| error.to_string())? {
                        if entry.file_name().to_string_lossy().ends_with(".app") {
                            app = Some(entry.path());
                            break;
                        }
                    }
                    let app = app.ok_or_else(|| format!("{} holds no app.", file.url))?;
                    verify_bundle(bundle, &app).await?;
                    Ok::<_, String>(app)
                }
                .await;
                let _ = tokio::fs::remove_dir_all(&download).await;
                match result {
                    Ok(app) => Ok(Staged { version: manifest.version.clone(), path: app, cleanup: unpacked }),
                    Err(error) => {
                        let _ = tokio::fs::remove_dir_all(&unpacked).await;
                        Err(error)
                    }
                }
            }
        }
    }
}

/// Puts the new build at the path the old one held, which the `aic` script and desktop entry name.
async fn swap(target: &UpdateTarget, update: &Staged) -> Result<(), String> {
    match target {
        UpdateTarget::AppImage(image) => tokio::fs::rename(&update.path, image).await.map_err(|error| error.to_string()),
        UpdateTarget::Bundle(bundle) => {
            let previous = update.cleanup.join("previous.app");
            tokio::fs::rename(bundle, &previous).await.map_err(|error| error.to_string())?;
            if let Err(error) = tokio::fs::rename(&update.path, bundle).await {
                let _ = tokio::fs::rename(&previous, bundle).await;
                return Err(error.to_string());
            }
            let _ = tokio::fs::remove_dir_all(&update.cleanup).await;
            Ok(())
        }
    }
}

pub struct UpdaterOptions {
    pub version: String,
    /// What a download replaces. None for a copy that cannot update itself, which still answers checks.
    pub target: Option<UpdateTarget>,
    /// Whether this is an installed build rather than one run from source.
    pub packaged: bool,
    pub releases: Option<String>,
    pub on_state: Box<dyn Fn(AppUpdate) + Send + Sync>,
    /// Told when a check the user asked for starts and stops, for a spinner.
    pub on_checking: Box<dyn Fn(bool) + Send + Sync>,
    /// What the user is told about a check, download or install they asked for.
    pub on_notice: Box<dyn Fn(UpdateNotice) + Send + Sync>,
    pub say: Box<dyn Fn(&str) + Send + Sync>,
}

struct UpdaterState {
    update: AppUpdate,
    found: Option<ReleaseManifest>,
    staged: Option<Staged>,
    checking: Option<futures::future::Shared<futures::future::BoxFuture<'static, ()>>>,
    check_requested: bool,
    installing: bool,
    disposed: bool,
    /// Whether a failure is the user's to hear about: true unless the download was unattended.
    announce_failure: bool,
}

/// One updater for the app's lifetime. Checks hourly; a check the user asked for downloads what it
/// finds, and a downloaded build installs when the user agrees or another computer asks.
pub struct AppUpdater {
    options: UpdaterOptions,
    source: Source,
    state: Mutex<UpdaterState>,
    timer: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl AppUpdater {
    pub fn new(options: UpdaterOptions) -> Arc<Self> {
        let client = reqwest::Client::builder().user_agent(act_core::identity::USER_AGENT).connect_timeout(Duration::from_secs(20)).build().unwrap_or_default();
        let source = Source { releases: options.releases.clone().unwrap_or_else(|| RELEASES.into()), client, stopping: CancellationToken::new() };
        Arc::new(Self {
            options,
            source,
            state: Mutex::new(UpdaterState { update: AppUpdate::Idle, found: None, staged: None, checking: None, check_requested: false, installing: false, disposed: false, announce_failure: true }),
            timer: Mutex::new(None),
        })
    }

    pub fn update(&self) -> AppUpdate {
        self.state.lock().unwrap().update.clone()
    }

    /// The build this app runs, and whether it replaces itself.
    pub fn build(&self) -> AppBuild {
        AppBuild { version: self.options.version.clone(), self_updating: self.options.packaged && self.options.target.is_some() }
    }

    fn set(&self, next: AppUpdate) {
        self.state.lock().unwrap().update = next.clone();
        (self.options.on_state)(next);
    }

    fn holding(&self) -> bool {
        matches!(self.state.lock().unwrap().update, AppUpdate::Downloading { .. } | AppUpdate::Ready { .. })
    }

    /// Checks now and every hour after, skipping while an update is downloading or waiting on a restart.
    pub fn start(self: &Arc<Self>) {
        let updater = self.clone();
        let handle = tokio::spawn(async move {
            loop {
                if !updater.holding() {
                    updater.check(false).await;
                }
                tokio::time::sleep(CHECK_INTERVAL).await;
            }
        });
        if let Some(previous) = self.timer.lock().unwrap().replace(handle) {
            previous.abort();
        }
    }

    /// One check. A check the user asked for says what it found and downloads a newer build; a ready
    /// build is offered for install instead of checking again.
    pub async fn check(self: &Arc<Self>, user_requested: bool) {
        if !self.options.packaged {
            if user_requested {
                (self.options.on_notice)(UpdateNotice::SourceCopy);
            }
            return;
        }
        if self.options.target.is_none() {
            if user_requested {
                (self.options.on_notice)(if Platform::current() == Platform::Linux { UpdateNotice::ManualLinux } else { UpdateNotice::SourceCopy });
            }
            return;
        }
        if let AppUpdate::Ready { version } = self.update() {
            if user_requested {
                (self.options.on_notice)(UpdateNotice::ReadyToInstall { version });
            }
            return;
        }
        let shared = {
            let mut state = self.state.lock().unwrap();
            if user_requested && !state.check_requested {
                state.check_requested = true;
                if state.checking.is_some() {
                    (self.options.on_checking)(true);
                }
            }
            if let Some(checking) = &state.checking {
                checking.clone()
            } else {
                let updater = self.clone();
                let work: futures::future::BoxFuture<'static, ()> = Box::pin(async move { updater.run_check().await });
                let shared = futures::FutureExt::shared(work);
                state.checking = Some(shared.clone());
                if user_requested {
                    (self.options.on_checking)(true);
                }
                shared
            }
        };
        shared.await;
    }

    async fn run_check(self: Arc<Self>) {
        let target = self.options.target.clone().expect("checked by the caller");
        let result = if self.holding() { Ok(None) } else { self.source.manifest(&target).await.map(Some) };
        let requested = {
            let mut state = self.state.lock().unwrap();
            state.checking = None;
            std::mem::take(&mut state.check_requested)
        };
        if requested {
            (self.options.on_checking)(false);
        }
        match result {
            Ok(None) => {}
            Ok(Some(manifest)) => {
                if self.holding() {
                    return;
                }
                if compare_versions(&manifest.version, &self.options.version) != std::cmp::Ordering::Greater {
                    let changed = {
                        let mut state = self.state.lock().unwrap();
                        state.found = None;
                        state.update != AppUpdate::Idle
                    };
                    if changed {
                        self.set(AppUpdate::Idle);
                    }
                    if requested {
                        (self.options.on_notice)(UpdateNotice::UpToDate { version: self.options.version.clone() });
                    }
                    return;
                }
                let version = manifest.version.clone();
                let changed = {
                    let mut state = self.state.lock().unwrap();
                    state.found = Some(manifest);
                    !matches!(&state.update, AppUpdate::Available { version: held } if *held == version)
                };
                if changed {
                    self.set(AppUpdate::Available { version });
                }
                if requested {
                    self.download(false).await;
                }
            }
            Err(error) => {
                (self.options.say)(&format!("Update check failed: {error}"));
                if requested {
                    (self.options.on_notice)(UpdateNotice::CheckFailed);
                }
            }
        }
    }

    /// Downloads the build a check found. A failed download offers it again; an unattended one, asked
    /// from another computer, says nothing on this screen.
    pub async fn download(self: &Arc<Self>, unattended: bool) {
        self.clone().fetch_build(unattended).await;
    }

    async fn fetch_build(self: Arc<Self>, unattended: bool) {
        let (manifest, target) = {
            let mut state = self.state.lock().unwrap();
            let available = matches!(state.update, AppUpdate::Available { .. });
            let found = match (&state.found, &self.options.target) {
                (Some(manifest), Some(target)) if available && !state.disposed => (manifest.clone(), target.clone()),
                _ => return,
            };
            state.announce_failure = !unattended;
            found
        };
        let version = manifest.version.clone();
        self.set(AppUpdate::Downloading { version: version.clone(), percent: 0 });
        (self.options.say)(&format!("Downloading {} {version}…", act_core::identity::APP_NAME));
        let progress_updater = self.clone();
        let progress = move |percent: i64| {
            let next = {
                let state = progress_updater.state.lock().unwrap();
                match &state.update {
                    AppUpdate::Downloading { version, percent: held } if *held != percent => Some(AppUpdate::Downloading { version: version.clone(), percent }),
                    _ => None,
                }
            };
            if let Some(next) = next {
                progress_updater.set(next);
            }
        };
        match self.source.stage(&target, &manifest, &progress).await {
            Ok(staged) => {
                self.state.lock().unwrap().staged = Some(staged);
                self.set(AppUpdate::Ready { version });
            }
            Err(error) => {
                (self.options.say)(&format!("Could not download {version}: {error}"));
                self.set(AppUpdate::Available { version });
                if !unattended {
                    (self.options.on_notice)(UpdateNotice::InstallFailed { recovery: manual_update_recovery() });
                }
            }
        }
    }

    /// Swaps the downloaded build into place. On success the caller quits and calls [`relaunch`] with
    /// the target; on failure the build is offered again.
    pub async fn install(self: &Arc<Self>) -> Result<String, String> {
        let (update, target) = {
            let mut state = self.state.lock().unwrap();
            if !matches!(state.update, AppUpdate::Ready { .. }) || state.installing || state.disposed {
                return Err("No update is ready to install.".into());
            }
            let Some(update) = state.staged.take() else { return Err("No update is ready to install.".into()) };
            state.installing = true;
            (update, self.options.target.clone().expect("a staged build has a target"))
        };
        (self.options.say)(&format!("Installing {} {}…", act_core::identity::APP_NAME, update.version));
        let result = swap(&target, &update).await;
        let announce = {
            let mut state = self.state.lock().unwrap();
            state.installing = false;
            state.announce_failure
        };
        match result {
            Ok(()) => {
                (self.options.say)(&format!("Installed {}. Restarting…", update.version));
                Ok(update.version)
            }
            Err(error) => {
                (self.options.say)(&format!("Could not install {}: {error}", update.version));
                let _ = tokio::fs::remove_dir_all(&update.cleanup).await;
                let _ = tokio::fs::remove_file(&update.cleanup).await;
                self.set(AppUpdate::Available { version: update.version.clone() });
                if announce {
                    (self.options.on_notice)(UpdateNotice::InstallFailed { recovery: manual_update_recovery() });
                }
                Err(error)
            }
        }
    }

    /// Cancels a download and the hourly checks. A build nobody installed is left for the next check to fetch again.
    pub async fn dispose(&self) {
        self.state.lock().unwrap().disposed = true;
        self.source.stopping.cancel();
        if let Some(timer) = self.timer.lock().unwrap().take() {
            timer.abort();
        }
        let staged = self.state.lock().unwrap().staged.take();
        if let Some(staged) = staged {
            let _ = tokio::fs::remove_dir_all(&staged.cleanup).await;
            let _ = tokio::fs::remove_file(&staged.cleanup).await;
        }
    }
}

/// Starts the installed build once this process has exited, so the new one claims the instance lock.
pub fn relaunch(target: &UpdateTarget, arguments: &[String]) -> std::io::Result<()> {
    match target {
        UpdateTarget::Bundle(bundle) => {
            let mut args = vec!["-n".to_string(), bundle.to_string_lossy().into_owned()];
            if !arguments.is_empty() {
                args.push("--args".into());
                args.extend(arguments.iter().cloned());
            }
            act_platform::process::start_after_exit("/usr/bin/open", &args)
        }
        UpdateTarget::AppImage(image) => act_platform::process::start_after_exit(&image.to_string_lossy(), arguments),
    }
}

/// Writes down the threads to carry on once `version` is the build running.
pub fn record_update_relaunch(user_data: &Path, version: &str, task_ids: &[String]) {
    let record = user_data.join(RELAUNCH_RECORD);
    let result = if task_ids.is_empty() {
        match std::fs::remove_file(&record) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    } else {
        std::fs::write(&record, serde_json::json!({ "version": version, "taskIds": task_ids }).to_string())
    };
    if let Err(error) = result {
        tracing::error!("Could not record the threads to resume after updating: {error}");
    }
}

/// The threads the last update cut off, read once. A record left by an install that never took is dropped.
pub fn take_update_relaunch(user_data: &Path, version: &str) -> Vec<String> {
    let record = user_data.join(RELAUNCH_RECORD);
    let Ok(raw) = std::fs::read_to_string(&record) else { return Vec::new() };
    let _ = std::fs::remove_file(&record);
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else { return Vec::new() };
    if parsed.get("version").and_then(|value| value.as_str()) != Some(version) {
        return Vec::new();
    }
    parsed.get("taskIds").and_then(|value| value.as_array()).map(|ids| ids.iter().filter_map(|id| id.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_manifest_electron_builder_publishes() {
        let text = "version: 0.10.27\nfiles:\n  - url: AI-Coding-Tool-0.10.27-arm64-mac.zip\n    sha512: 'abc=='\n    size: 1234\n  - url: AI-Coding-Tool-0.10.27-arm64.dmg\n    sha512: def==\n    size: 99\n  - url: broken.zip\npath: AI-Coding-Tool-0.10.27-arm64-mac.zip\nsha512: abc==\n";
        let manifest = parse_release_manifest(text).unwrap();
        assert_eq!(manifest.version, "0.10.27");
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(manifest.files[0], ReleaseFile { url: "AI-Coding-Tool-0.10.27-arm64-mac.zip".into(), sha512: "abc==".into(), size: 1234 });
        assert_eq!(release_file(&manifest, &UpdateTarget::Bundle("/A.app".into())).unwrap().size, 1234);
        assert!(parse_release_manifest("files: []").is_none());
    }

    #[test]
    fn a_relaunch_record_is_read_once_and_only_by_its_version() {
        let dir = tempfile::tempdir().unwrap();
        record_update_relaunch(dir.path(), "1.2.0", &["a".into(), "b".into()]);
        assert!(take_update_relaunch(dir.path(), "1.1.0").is_empty());
        record_update_relaunch(dir.path(), "1.2.0", &["a".into()]);
        assert_eq!(take_update_relaunch(dir.path(), "1.2.0"), vec!["a".to_string()]);
        assert!(take_update_relaunch(dir.path(), "1.2.0").is_empty());
    }
}
