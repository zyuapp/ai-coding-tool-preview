//! The engine commands the user installed: where they are, what version they report, the newest
//! release, and how to bring an install up to date the way the user installed it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::future::{BoxFuture, FutureExt, Shared};
use regex::Regex;
use act_platform::process::command;

use crate::types::EngineReadiness;
use crate::version::{is_older_than, read_version};

/// Long enough for a cold binary to answer, short enough that the model menu is not held open on one.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// Long enough for a package manager to download and replace a release.
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Releases come out daily at most, so an hour old answer about the newest one is fresh enough.
const LATEST_TTL: Duration = Duration::from_secs(60 * 60);
const LATEST_TIMEOUT: Duration = Duration::from_secs(4);
const HOMEBREW_INFO_TIMEOUT: Duration = Duration::from_secs(10);
/// A launcher is a few lines; anything bigger is the engine itself and is not read.
const MAX_LAUNCHER_BYTES: u64 = 16 * 1024;

/// The command an engine is installed as, the npm package it is published as, and how to install it.
#[derive(Debug, Clone, Copy)]
pub struct EngineCommand {
    pub command: &'static str,
    pub package_name: &'static str,
    pub install: &'static str,
    pub is_native: fn(&str) -> bool,
}

pub const CLAUDE_BINARY: EngineCommand = EngineCommand {
    command: "claude",
    package_name: "@anthropic-ai/claude-code",
    install: "curl -fsSL https://claude.ai/install.sh | bash",
    is_native: |path| {
        path.ends_with("/.local/bin/claude") || path.contains("/.local/share/claude/")
    },
};

pub const CODEX_BINARY: EngineCommand = EngineCommand {
    command: "codex",
    package_name: "@openai/codex",
    install: "brew install --cask codex",
    is_native: |path| path.contains("/packages/standalone/"),
};

pub const OPENCODE_BINARY: EngineCommand = EngineCommand {
    command: "opencode",
    package_name: "@opencode/cli",
    install: "curl -fsSL https://opencode.ai/v2/install | bash",
    is_native: |path| path.contains("/.opencode/bin/"),
};

/// Where an engine's command sits on the search path, or nothing when the user has not installed it.
pub fn engine_binary_path(binary: &EngineCommand) -> Option<PathBuf> {
    act_platform::search_path::find(binary.command)
}

/// How an install is brought up to date: what the user would type, and the program and arguments the app runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineUpdater {
    pub command: String,
    pub file: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomebrewKeg {
    pub cask: bool,
    pub name: String,
    pub prefix: String,
}

fn homebrew_keg(real_path: &str) -> Option<HomebrewKeg> {
    static KEG: OnceLock<Regex> = OnceLock::new();
    let keg =
        KEG.get_or_init(|| Regex::new(r"(?i)^(.*)/(cellar|caskroom)/([^/]+)/[^/]+/").unwrap());
    let captures = keg.captures(real_path)?;
    Some(HomebrewKeg {
        cask: !captures[2].eq_ignore_ascii_case("cellar"),
        name: captures[3].to_string(),
        prefix: captures[1].to_string(),
    })
}

/// The npm prefix a global install of the package sits under. A package nested in another one's
/// `node_modules` belongs to a project, and one inside a mise tool other than Node is mise's.
fn npm_prefix(real_path: &str, package_name: &str) -> Option<String> {
    let index = real_path.rfind(&format!("/lib/node_modules/{package_name}/"))?;
    let before = &real_path[..index];
    if before.contains("/node_modules/") {
        return None;
    }
    static MISE: OnceLock<Regex> = OnceLock::new();
    let mise = MISE.get_or_init(|| Regex::new(r"/mise/installs/([^/]+)/[^/]+$").unwrap());
    if let Some(tool) = mise.captures(before) {
        if &tool[1] != "node" {
            return None;
        }
    }
    Some(if index == 0 {
        "/".to_string()
    } else {
        before.to_string()
    })
}

fn is_mise(path: &str) -> bool {
    path.contains("/mise/installs/") || path.contains("/mise/shims/")
}

/// A launcher script that runs the engine through mise (`exec mise x codex -- codex`), which mise keeps.
fn is_mise_launcher(path: &str) -> bool {
    static LAUNCH: OnceLock<Regex> = OnceLock::new();
    let launch = LAUNCH.get_or_init(|| Regex::new(r"\bmise\s+(?:x|exec)\b").unwrap());
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() > MAX_LAUNCHER_BYTES {
        return false;
    }
    std::fs::read_to_string(path)
        .is_ok_and(|script| script.starts_with("#!") && launch.is_match(&script))
}

/// The tool beside an install, which belongs to it, or the one on the search path when there is none.
fn beside_or_on_path(prefix: &str, tool: &str) -> String {
    let beside = Path::new(prefix).join("bin").join(tool);
    if act_platform::fs::is_executable(&beside) {
        beside.to_string_lossy().into_owned()
    } else {
        tool.to_string()
    }
}

/// How this install is brought up to date, or None when the app cannot tell and leaves it to the user.
pub fn updater_for(
    binary: &EngineCommand,
    binary_path: &str,
    real_path: &str,
) -> Option<EngineUpdater> {
    let native = EngineUpdater {
        command: format!("{} update", binary.command),
        file: binary_path.to_string(),
        args: vec!["update".into()],
    };
    if is_mise_launcher(real_path) {
        return None;
    }
    if let Some(prefix) = npm_prefix(real_path, binary.package_name) {
        let package = binary.package_name;
        return Some(EngineUpdater {
            command: format!("npm install -g {package}@latest"),
            file: beside_or_on_path(&prefix, "npm"),
            args: vec![
                "install".into(),
                "-g".into(),
                "--prefix".into(),
                prefix.clone(),
                format!("--allow-scripts={package}"),
                format!("{package}@latest"),
            ],
        });
    }
    if is_mise(real_path) || is_mise(binary_path) {
        return None;
    }
    if (binary.is_native)(real_path) || (binary.is_native)(binary_path) {
        return Some(native);
    }
    if let Some(keg) = homebrew_keg(real_path) {
        let args: Vec<String> = if keg.cask {
            vec!["upgrade".into(), "--cask".into(), keg.name.clone()]
        } else {
            vec!["upgrade".into(), keg.name.clone()]
        };
        return Some(EngineUpdater {
            command: format!("brew {}", args.join(" ")),
            file: beside_or_on_path(&keg.prefix, "brew"),
            args,
        });
    }
    if real_path.contains("/node_modules/") {
        None
    } else {
        Some(native)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledEngine {
    pub path: String,
    /// What `--version` printed, or nothing when the command would not answer.
    pub version: Option<String>,
    pub update: Option<EngineUpdater>,
    pub keg: Option<HomebrewKeg>,
}

/// What the app would run for an engine, with the version it reports.
pub async fn installed_engine(binary: &EngineCommand) -> Option<InstalledEngine> {
    let path = engine_binary_path(binary)?;
    let binary_path = path.to_string_lossy().into_owned();
    let real_path = std::fs::canonicalize(&path)
        .map(|real| real.to_string_lossy().into_owned())
        .unwrap_or_else(|_| binary_path.clone());
    let version = match tokio::time::timeout(
        VERSION_TIMEOUT,
        command(&path)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(output)) if output.status.success() => {
            read_version(&String::from_utf8_lossy(&output.stdout))
        }
        _ => None,
    };
    Some(InstalledEngine {
        update: updater_for(binary, &binary_path, &real_path),
        keg: homebrew_keg(&real_path),
        path: binary_path,
        version,
    })
}

fn stable_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

type LatestRead = Shared<BoxFuture<'static, Option<String>>>;

/// Asked once an hour per source, failures included, so a machine offline at launch does not ask on every read.
fn cached_latest(key: String, read: BoxFuture<'static, Option<String>>) -> LatestRead {
    static READS: OnceLock<Mutex<HashMap<String, (Instant, LatestRead)>>> = OnceLock::new();
    let mut reads = READS.get_or_init(Default::default).lock().unwrap();
    if let Some((expires, version)) = reads.get(&key) {
        if *expires > Instant::now() {
            return version.clone();
        }
    }
    let version = read.shared();
    reads.insert(key, (Instant::now() + LATEST_TTL, version.clone()));
    version
}

async fn npm_latest(package_name: String) -> Option<String> {
    let encoded: String = package_name
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    let client = reqwest::Client::builder()
        .timeout(LATEST_TIMEOUT)
        .build()
        .ok()?;
    let response = client
        .get(format!("https://registry.npmjs.org/{encoded}/latest"))
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: serde_json::Value = response.json().await.ok()?;
    body["version"]
        .as_str()
        .filter(|version| stable_version(version))
        .map(String::from)
}

/// A cask's version can carry a build after a comma (`1.2.3,456`).
async fn homebrew_latest(keg: HomebrewKeg) -> Option<String> {
    let brew = beside_or_on_path(&keg.prefix, "brew");
    let output = tokio::time::timeout(
        HOMEBREW_INFO_TIMEOUT,
        command(brew)
            .args(["info", "--json=v2", &keg.name])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let version = if keg.cask {
        info["casks"][0]["version"]
            .as_str()
            .map(|version| version.split(',').next().unwrap_or_default().to_string())
    } else {
        info["formulae"][0]["versions"]["stable"]
            .as_str()
            .map(String::from)
    };
    version.filter(|version| stable_version(version))
}

/// The newest release this install could move to, or None when it cannot be read. A Homebrew install
/// is compared with what `brew upgrade` can deliver.
pub async fn latest_engine_version(
    binary: &EngineCommand,
    installed: &InstalledEngine,
) -> Option<String> {
    match &installed.keg {
        Some(keg) => {
            cached_latest(
                format!("brew:{}", keg.name),
                homebrew_latest(keg.clone()).boxed(),
            )
            .await
        }
        None => {
            cached_latest(
                format!("npm:{}", binary.package_name),
                npm_latest(binary.package_name.to_string()).boxed(),
            )
            .await
        }
    }
}

/// Where an install stands against what the app was built on and the newest release, and the command
/// that moves it on when it is behind either and the app can run that command.
pub async fn version_readiness(
    binary: &EngineCommand,
    installed: &InstalledEngine,
    baseline: &str,
    access: crate::types::EngineAccess,
) -> EngineReadiness {
    let latest = if installed.version.is_some() {
        latest_engine_version(binary, installed).await
    } else {
        None
    };
    let required =
        is_older_than(installed.version.as_deref(), baseline).then(|| baseline.to_string());
    let newer = match (&latest, &installed.version) {
        (Some(latest), Some(version)) if is_older_than(Some(version), latest) => {
            Some(latest.clone())
        }
        _ => None,
    };
    let fix = installed
        .update
        .as_ref()
        .filter(|_| required.is_some() || newer.is_some())
        .map(|update| update.command.clone());
    EngineReadiness {
        version: installed.version.clone(),
        required,
        latest: newer,
        fix,
        ..EngineReadiness::of(access)
    }
}

/// Brings an installed engine up to date the way the user installed it. Fails with what the command
/// said and the command to run by hand.
pub async fn upgrade_engine(binary: &EngineCommand) -> anyhow::Result<()> {
    let Some(installed) = installed_engine(binary).await else {
        anyhow::bail!("{} is not installed.", binary.command)
    };
    let Some(update) = installed.update else {
        anyhow::bail!(
            "The app cannot tell how it was installed. Update it the way you installed it."
        )
    };
    let output = tokio::time::timeout(
        UPGRADE_TIMEOUT,
        command(&update.file)
            .args(&update.args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let stderr = match output {
        Ok(Ok(output)) if output.status.success() => return Ok(()),
        Ok(Ok(output)) => String::from_utf8_lossy(&output.stderr).into_owned(),
        Ok(Err(error)) => error.to_string(),
        Err(_) => String::new(),
    };
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let said = lines
        .iter()
        .find(|line| line.to_lowercase().contains("error"))
        .or(lines.last())
        .copied();
    let prefix = said
        .map(|line| format!("{}. ", line.trim_end_matches(['.', ' ', '\t'])))
        .unwrap_or_default();
    anyhow::bail!("{prefix}Run `{}` in your terminal.", update.command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_only_installs_the_path_proves() {
        let npm = updater_for(
            &CLAUDE_BINARY,
            "/usr/local/bin/claude",
            "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
        )
        .unwrap();
        assert_eq!(
            npm.command,
            "npm install -g @anthropic-ai/claude-code@latest"
        );
        assert!(npm.args.contains(&"/usr/local".to_string()));
        let native = updater_for(
            &CLAUDE_BINARY,
            "/Users/x/.local/bin/claude",
            "/Users/x/.local/share/claude/versions/2.1.0",
        )
        .unwrap();
        assert_eq!(native.command, "claude update");
        let cask = updater_for(
            &CODEX_BINARY,
            "/opt/homebrew/bin/codex",
            "/opt/homebrew/Caskroom/codex/0.1.0/codex",
        )
        .unwrap();
        assert_eq!(cask.command, "brew upgrade --cask codex");
        assert!(updater_for(&CODEX_BINARY, "/x/bin/codex", "/x/mise/installs/node/22/lib/node_modules/other/node_modules/@openai/codex/bin/codex.js").is_none());
        assert!(updater_for(
            &CODEX_BINARY,
            "/x/mise/shims/codex",
            "/x/mise/installs/codex/1/codex"
        )
        .is_none());
    }
}
