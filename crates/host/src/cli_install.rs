//! The terminal command (`act_core::identity::CLI_COMMAND`): a small shell script that opens a folder
//! as a project through the app's URL scheme, or runs this app headless with its `serve` subcommand.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

use act_core::identity::APP_NAME;

use crate::process::Exec;

pub use act_core::identity::CLI_COMMAND;
pub const CLI_INSTALL_PATH: &str = concat!("/usr/local/bin/", act_core::cli_command!());
pub use act_core::identity::URL_SCHEME as CLI_URL_SCHEME;
/// Stamped into the script so an install can tell its own file from someone else's `aic`. Older stamps still count.
pub const CLI_SCRIPT_MARKER: &str = "# aic-cli v3";
const CLI_SCRIPT_STAMP: &str = "# aic-cli v";
/// `aic serve` exits with this to be started again by the script, which then runs whatever build is installed.
pub const SERVE_RESTART_EXIT: i32 = 75;
/// Set by a script that starts `aic serve` again on [`SERVE_RESTART_EXIT`]; a server without it cannot update itself.
pub const SERVE_RESTARTS_ENV: &str = "AIC_SERVE_RESTARTS";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliInstallState {
    Installed,
    Missing,
    /// A different `aic` already on the path, which an install would overwrite.
    Conflict,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    pub state: CliInstallState,
    pub path: String,
    /// Linux shells run the installed file by name only when its directory is on the PATH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_path: Option<bool>,
    /// Whether an installed script is the one this build writes. An older one still opens folders, but cannot serve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliConfiguration {
    pub install_path: String,
    pub script: String,
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn refusal() -> Vec<String> {
    vec![format!("    printf '{CLI_COMMAND}: this install cannot serve. Install the command again from {APP_NAME}.\\n' >&2"), "    exit 1".into()]
}

/// `aic serve` runs as a child so the script can start it again after it installs an update, re-reading
/// the script in case the update moved the app. Termination is passed on; Ctrl+C reaches the child itself.
fn serve_lines(executable: Option<&str>) -> Vec<String> {
    let Some(executable) = executable else { return refusal() };
    vec![
        format!("    env {SERVE_RESTARTS_ENV}=1 {} \"$@\" <&0 &", shell_quote(executable)),
        "    child=$!".into(),
        "    trap : INT".into(),
        "    trap 'kill -TERM \"$child\" 2>/dev/null' TERM HUP".into(),
        "    wait \"$child\"; status=$?".into(),
        "    while kill -0 \"$child\" 2>/dev/null; do wait \"$child\"; status=$?; done".into(),
        format!("    [ \"$status\" -eq {SERVE_RESTART_EXIT} ] && exec \"$0\" \"$@\""),
        "    exit \"$status\"".into(),
    ]
}

fn pair_lines(executable: Option<&str>) -> Vec<String> {
    let Some(executable) = executable else { return refusal() };
    vec![format!("    exec {} \"$@\"", shell_quote(executable))]
}

fn cli_script(opener: &[String], executable: Option<&str>) -> String {
    let mut lines: Vec<String> = vec![
        "#!/bin/sh".into(),
        CLI_SCRIPT_MARKER.into(),
        format!("# Opens a folder as an {APP_NAME} project, or serves the app headless. Installed from {APP_NAME} settings."),
        "case \"$1\" in".into(),
        "  serve)".into(),
    ];
    lines.extend(serve_lines(executable));
    lines.extend(["    ;;".into(), "  pair)".into()]);
    lines.extend(pair_lines(executable));
    lines.extend([
        "    ;;".into(),
        "esac".into(),
        "target=$1".into(),
        "[ -n \"$target\" ] || target=.".into(),
        "if [ ! -d \"$target\" ]; then".into(),
        format!("  printf '{CLI_COMMAND}: not a directory: %s\\n' \"$target\" >&2"),
        "  exit 1".into(),
        "fi".into(),
        "dir=$(cd \"$target\" && pwd)".into(),
        "encoded=$(printf %s \"$dir\" | base64 | tr -d '\\n' | tr '+/' '-_')".into(),
    ]);
    lines.extend(opener.iter().cloned());
    lines.push(String::new());
    lines.join("\n")
}

pub fn mac_cli_script(executable: Option<&str>) -> String {
    cli_script(&[format!("exec open \"{CLI_URL_SCHEME}://open?path=$encoded\"")], executable)
}

pub fn linux_cli_script(executable: Option<&str>) -> String {
    cli_script(
        &[
            format!("url=\"{CLI_URL_SCHEME}://open?path=$encoded\""),
            "if command -v gio >/dev/null 2>&1; then exec gio open \"$url\"; fi".into(),
            "if command -v xdg-open >/dev/null 2>&1; then exec xdg-open \"$url\"; fi".into(),
            format!("printf '{CLI_COMMAND}: could not find xdg-open or gio to open {APP_NAME}.\\n' >&2"),
            "exit 1".into(),
        ],
        executable,
    )
}

/// The per-platform pieces of the terminal integration. `executable` is the app as installed, which
/// `aic serve` runs; an install that knows none, such as a source build, refuses to serve.
pub fn cli_configuration(platform: &str, home: &str, executable: Option<&str>) -> Option<CliConfiguration> {
    if platform == "macos" || platform == "darwin" {
        return Some(CliConfiguration { install_path: CLI_INSTALL_PATH.into(), script: mac_cli_script(executable) });
    }
    if platform != "linux" || !home.starts_with('/') {
        return None;
    }
    let trimmed = home.trim_end_matches('/');
    Some(CliConfiguration { install_path: format!("{trimmed}/.local/bin/{CLI_COMMAND}"), script: linux_cli_script(executable) })
}

pub fn is_cli_script(contents: &str) -> bool {
    contents.contains(CLI_SCRIPT_STAMP)
}

/// The folder an `<scheme>://open?path=` URL names, or None when the URL is not one the script wrote.
pub fn project_path_from_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != CLI_URL_SCHEME || url.host_str() != Some("open") {
        return None;
    }
    let encoded = url.query_pairs().find(|(key, _)| key == "path").map(|(_, value)| value.into_owned())?;
    if encoded.is_empty() || encoded.len() > 8_192 || !encoded.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'=')) {
        return None;
    }
    let standard = encoded.replace('-', "+").replace('_', "/");
    let bytes = base64::engine::general_purpose::STANDARD.decode(standard.trim_end_matches('=')).or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(standard.trim_end_matches('='))).ok()?;
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    (decoded.starts_with('/') && !decoded.contains('\0')).then_some(decoded)
}

/// Linux delivers the URL as a launch argument rather than as an event.
pub fn project_path_from_argv(argv: &[String]) -> Option<String> {
    argv.iter().filter(|argument| argument.starts_with(&format!("{CLI_URL_SCHEME}://"))).find_map(|argument| project_path_from_url(argument))
}

/// Only known bytes cross elevation. The elevated process owns its private staging directory.
pub fn cli_install_command(target: &CliConfiguration) -> String {
    let parent = Path::new(&target.install_path).parent().map(|parent| parent.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
    [
        "set -eu".to_string(),
        "umask 077".into(),
        "stage=$(/usr/bin/mktemp -d /private/tmp/aic-cli-install.XXXXXXXX)".into(),
        "trap '/bin/rm -rf \"$stage\"' EXIT".into(),
        format!("/usr/bin/printf '%s' {} | /usr/bin/base64 -D > \"$stage/aic\"", shell_quote(&base64::engine::general_purpose::STANDARD.encode(target.script.as_bytes()))),
        "/bin/chmod 755 \"$stage/aic\"".into(),
        format!("/bin/mkdir -p {}", shell_quote(&parent)),
        format!("/bin/mv -fh \"$stage/aic\" {}", shell_quote(&target.install_path)),
    ]
    .join("\n")
}

fn is_permission_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::EROFS)) || error.kind() == std::io::ErrorKind::PermissionDenied
}

/// `/usr/local/bin` belongs to root on a stock Mac, so writing there asks for the password once.
async fn elevate(command: &str) -> Result<(), String> {
    let script = format!("do shell script {} with administrator privileges", serde_json::to_string(command).unwrap_or_default());
    match Exec::new("/usr/bin/osascript").args(["-e", script.as_str()]).run().await {
        Ok(_) => Ok(()),
        Err(error) => {
            let message = format!("{} {}", String::from_utf8_lossy(error.stderr()), error);
            if message.contains("User canceled") || message.contains("-128") {
                return Err("Cancelled.".into());
            }
            let trimmed = String::from_utf8_lossy(error.stderr()).trim().to_string();
            Err(if trimmed.is_empty() { "The command could not be installed.".into() } else { trimmed })
        }
    }
}

/// Installs, removes and repairs the script at one place for one platform.
#[derive(Debug, Clone)]
pub struct CliInstaller {
    configuration: Option<CliConfiguration>,
    platform: String,
}

impl CliInstaller {
    pub fn new(configuration: Option<CliConfiguration>, platform: impl Into<String>) -> Self {
        Self { configuration, platform: platform.into() }
    }

    /// The installer for this machine. `executable` is the binary `aic serve` runs, or None for a source build.
    pub fn for_this_machine(executable: Option<&str>) -> Self {
        let platform = std::env::consts::OS;
        let installed = std::env::var("APPIMAGE").ok().filter(|_| platform == "linux");
        let executable = installed.as_deref().or(executable);
        Self::new(cli_configuration(platform, &crate::paths::home().to_string_lossy(), executable), platform)
    }

    fn is_mac(&self) -> bool {
        self.platform == "macos" || self.platform == "darwin"
    }

    fn result(&self, state: CliInstallState) -> CliStatus {
        let target = self.configuration.as_ref().map(|configuration| configuration.install_path.clone()).unwrap_or_else(|| CLI_INSTALL_PATH.into());
        let on_path = (self.platform == "linux").then(|| {
            let directory = Path::new(&target).parent().map(Path::to_path_buf).unwrap_or_default();
            std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|entry| crate::paths::resolve(&entry) == directory))
        });
        CliStatus { state, path: target, on_path, current: None }
    }

    fn supported(&self) -> Result<&CliConfiguration, String> {
        self.configuration.as_ref().ok_or_else(|| format!("The {CLI_COMMAND} command can only be installed on macOS or Linux."))
    }

    pub async fn status(&self) -> Result<CliStatus, String> {
        let Some(configuration) = &self.configuration else { return Ok(self.result(CliInstallState::Unsupported)) };
        match tokio::fs::symlink_metadata(&configuration.install_path).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(self.result(CliInstallState::Missing)),
            Err(error) => Err(error.to_string()),
            Ok(metadata) if !metadata.is_file() => Ok(self.result(CliInstallState::Conflict)),
            Ok(_) => {
                let contents = tokio::fs::read(&configuration.install_path).await.map_err(|error| error.to_string())?;
                let contents = String::from_utf8_lossy(&contents);
                if !is_cli_script(&contents) {
                    return Ok(self.result(CliInstallState::Conflict));
                }
                Ok(CliStatus { current: Some(contents == configuration.script), ..self.result(CliInstallState::Installed) })
            }
        }
    }

    async fn write_install(&self, allow_elevation: bool) -> Result<CliStatus, String> {
        let target = self.supported()?;
        if let Err(error) = write_script(target).await {
            if !allow_elevation || !is_permission_error(&error) || !self.is_mac() {
                return Err(error.to_string());
            }
            elevate(&cli_install_command(target)).await?;
        }
        self.status().await
    }

    pub async fn install(&self) -> Result<CliStatus, String> {
        self.write_install(true).await
    }

    pub async fn uninstall(&self) -> Result<CliStatus, String> {
        let target = self.supported()?;
        match tokio::fs::remove_file(&target.install_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if is_permission_error(&error) && self.is_mac() => elevate(&format!("/bin/rm -f {}", shell_quote(&target.install_path))).await?,
            Err(error) => return Err(error.to_string()),
        }
        self.status().await
    }

    /// Repairs only our own launcher; startup respects an uninstall or another command, and never asks for a password.
    pub async fn refresh(&self) -> Result<CliStatus, String> {
        let current = self.status().await?;
        if current.state != CliInstallState::Installed || current.current == Some(true) {
            return Ok(current);
        }
        let target = self.supported()?;
        match write_script(target).await {
            Ok(()) => self.status().await,
            Err(error) if is_permission_error(&error) => Ok(current),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// Written beside and renamed over, so the command is never half a script.
async fn write_script(target: &CliConfiguration) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = PathBuf::from(&target.install_path);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let staged = PathBuf::from(format!("{}.{}.{}.tmp", target.install_path, std::process::id(), uuid::Uuid::new_v4()));
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o755);
        let mut file = options.open(&staged).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, target.script.as_bytes()).await?;
        drop(file);
        tokio::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).await?;
        tokio::fs::rename(&staged, &path).await
    }
    .await;
    let _ = tokio::fs::remove_file(&staged).await;
    result
}

/// Makes the bundle `bundle_id` the handler for the app's URL scheme, so the command opens this app, as
/// Electron's `setAsDefaultProtocolClient` does for an installed build.
#[cfg(target_os = "macos")]
pub fn claim_url_scheme(bundle_id: &str) -> bool {
    use objc2_foundation::NSString;
    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn LSSetDefaultHandlerForURLScheme(scheme: *const std::ffi::c_void, handler: *const std::ffi::c_void) -> i32;
    }
    let scheme = NSString::from_str(CLI_URL_SCHEME);
    let handler = NSString::from_str(bundle_id);
    // NSString is toll-free bridged to CFString.
    unsafe { LSSetDefaultHandlerForURLScheme(&*scheme as *const NSString as *const _, &*handler as *const NSString as *const _) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_round_trips_through_the_url_the_script_writes() {
        let encoded = base64::engine::general_purpose::STANDARD.encode("/Users/me/my repo?").replace('+', "-").replace('/', "_");
        assert_eq!(project_path_from_url(&format!("{CLI_URL_SCHEME}://open?path={encoded}")).as_deref(), Some("/Users/me/my repo?"));
        assert_eq!(project_path_from_url(&format!("{CLI_URL_SCHEME}://other?path=L3g=")), None);
        assert_eq!(project_path_from_url(&format!("{CLI_URL_SCHEME}://open?path=eA==")), None);
    }

    #[tokio::test]
    async fn refresh_rewrites_only_an_outdated_script_of_our_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin/aic").to_string_lossy().into_owned();
        let installer = CliInstaller::new(Some(CliConfiguration { install_path: path.clone(), script: mac_cli_script(Some("/Apps/AI.app/Contents/MacOS/ai")) }), "macos");
        assert_eq!(installer.refresh().await.unwrap().state, CliInstallState::Missing);
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(&path, "#!/bin/sh\necho someone else's\n").unwrap();
        assert_eq!(installer.refresh().await.unwrap().state, CliInstallState::Conflict);
        std::fs::write(&path, format!("#!/bin/sh\n{CLI_SCRIPT_MARKER}\nold\n")).unwrap();
        let refreshed = installer.refresh().await.unwrap();
        assert_eq!((refreshed.state, refreshed.current), (CliInstallState::Installed, Some(true)));
    }
}
