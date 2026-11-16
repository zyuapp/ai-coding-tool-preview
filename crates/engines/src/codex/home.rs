//! Codex children keep their state in a private home of the app's, linked to the user's Codex home
//! for the inputs they share (config, sign-in, skills, plugins) but never its runtime databases.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Set at launch to where Codex children keep their state.
pub const PRIVATE_CODEX_HOME_ENV: &str = "AICODINGTOOL_CODEX_HOME";

/// Inputs shared with the user's Codex install. Never link runtime directories or databases.
const SHARED_INPUTS: [&str; 15] = [
    "config.toml",
    "auth.json",
    ".env",
    "AGENTS.md",
    "AGENTS.override.md",
    "rules",
    "skills",
    "agents",
    "hooks",
    "hooks.json",
    "requirements.toml",
    "managed_config.toml",
    "plugins/cache",
    ".tmp/marketplaces",
    "mcp-oauth-locks",
];
const WRITABLE_DIRECTORIES: [&str; 3] = ["plugins/cache", ".tmp/marketplaces", "mcp-oauth-locks"];

fn missing(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::NotFound
}

fn mkdir_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

fn share(source: &Path, destination: &Path, adopt_directory: bool) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() {
        mkdir_private(parent)?;
    }
    match std::os::unix::fs::symlink(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let target = std::fs::read_link(destination).ok();
            if target.is_none()
                && adopt_directory
                && std::fs::symlink_metadata(destination)?.is_dir()
            {
                let kept = format!(
                    "{}.before-sharing-{}",
                    destination.display(),
                    uuid::Uuid::new_v4()
                );
                std::fs::rename(destination, kept)?;
                std::os::unix::fs::symlink(source, destination)?;
                return Ok(());
            }
            let resolved =
                target.map(|target| destination.parent().unwrap_or(Path::new("/")).join(target));
            if resolved.as_deref().map(normalize) != Some(normalize(source)) {
                anyhow::bail!(
                    "Cannot share Codex settings: {} already exists and is not a link to {}.",
                    destination.display(),
                    source.display()
                );
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// A path with `.` and `..` resolved lexically, as Node's `path.resolve` does.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// MCP's file backend refuses symlinks, so credentials are hard-linked; a marker remembers the link
/// this app made so an independent file is never replaced.
fn share_mcp_credentials(source: &Path, destination: &Path) -> std::io::Result<()> {
    let original = source.join(".credentials.json");
    let target = destination.join(".credentials.json");
    let marker = destination.join(".aicodingtool-mcp-credentials.json");
    match std::fs::symlink_metadata(&original) {
        Ok(_) => {}
        Err(error) if missing(&error) => {
            let empty = source.join(format!(".credentials-{}.tmp", uuid::Uuid::new_v4()));
            let written = (|| {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&empty)?;
                file.write_all(b"{}")?;
                match std::fs::hard_link(&empty, &original) {
                    Err(error) if error.kind() != ErrorKind::AlreadyExists => Err(error),
                    _ => Ok(()),
                }
            })();
            let _ = std::fs::remove_file(&empty);
            written?;
        }
        Err(error) => return Err(error),
    }
    let canonical = std::fs::canonicalize(&original)?;
    let source_stat = std::fs::metadata(&canonical)?;
    let remember = || {
        write_private(
            &marker,
            &json!({ "dev": source_stat.dev(), "ino": source_stat.ino() }).to_string(),
        )
    };
    let target_stat = match std::fs::symlink_metadata(&target) {
        Ok(stat) => Some(stat),
        Err(error) if missing(&error) => None,
        Err(error) => return Err(error),
    };
    if let Some(stat) = &target_stat {
        if stat.is_file() && stat.dev() == source_stat.dev() && stat.ino() == source_stat.ino() {
            return remember();
        }
        if stat.file_type().is_symlink() && std::fs::read_link(&target)? != original {
            return Err(std::io::Error::other(format!(
                "Unexpected Codex credentials link: {}",
                target.display()
            )));
        }
        if !stat.file_type().is_symlink() {
            let previous: Option<Value> = std::fs::read_to_string(&marker)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok());
            let same = previous.is_some_and(|previous| {
                previous["dev"].as_u64() == Some(stat.dev())
                    && previous["ino"].as_u64() == Some(stat.ino())
            });
            if !same {
                return Err(std::io::Error::other(format!(
                    "Cannot share Codex MCP credentials: {} contains independent credentials.",
                    target.display()
                )));
            }
        }
    }
    let temporary = PathBuf::from(format!("{}.{}.tmp", target.display(), uuid::Uuid::new_v4()));
    let linked = std::fs::hard_link(&canonical, &temporary)
        .and_then(|_| std::fs::rename(&temporary, &target))
        .and_then(|_| remember());
    let _ = std::fs::remove_file(&temporary);
    linked
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(text.as_bytes())
}

/// A link left by an earlier relocation is followed, and recreated if its target was removed.
fn private_directory(private_home: &Path) -> std::io::Result<PathBuf> {
    let directory = match std::fs::read_link(private_home) {
        Ok(target) => private_home.parent().unwrap_or(Path::new("/")).join(target),
        Err(_) => private_home.to_path_buf(),
    };
    mkdir_private(&directory)?;
    std::fs::canonicalize(private_home)
}

fn ephemeral_storage(name: &str) -> bool {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| regex::Regex::new(r"^[a-z]+_\d+\.sqlite(-wal|-shm)?$").unwrap())
        .is_match(name)
}

/// A home that never started Codex holds only links and what title runs left, so nothing is lost moving it.
fn disposable(directory: &Path, top: bool) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if kind.is_symlink() || (top && (name == "log" || ephemeral_storage(&name))) {
            continue;
        }
        if !kind.is_dir() || !disposable(&entry.path(), false)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Hard links cannot cross filesystems, so the private home moves into the shared home's filesystem.
fn relocate(private_home: &Path, home: &Path, source: &Path) -> anyhow::Result<PathBuf> {
    let credentials = std::fs::canonicalize(source.join(".credentials.json"))?;
    let unrelocatable = |from: &Path| {
        anyhow::anyhow!(
            "Codex MCP sign-ins can only be shared when {} is on the same filesystem as {}.",
            from.display(),
            credentials.display()
        )
    };
    if std::fs::symlink_metadata(private_home)?
        .file_type()
        .is_symlink()
        || !disposable(private_home, true)?
    {
        return Err(unrelocatable(home));
    }
    let digest = hex::encode(Sha256::digest(
        normalize(private_home).to_string_lossy().as_bytes(),
    ));
    let destination = source.join("aicodingtool-threads").join(&digest[..16]);
    mkdir_private(&destination)?;
    share_mcp_credentials(source, &destination).map_err(|error| {
        if error.raw_os_error() == Some(libc::EXDEV) {
            unrelocatable(&destination)
        } else {
            error.into()
        }
    })?;
    std::fs::remove_dir_all(private_home)?;
    std::os::unix::fs::symlink(&destination, private_home)?;
    Ok(std::fs::canonicalize(private_home)?)
}

fn prepare(private_home: &Path, source_home: &Path) -> anyhow::Result<PathBuf> {
    mkdir_private(source_home)?;
    let home = private_directory(private_home)?;
    let source = match std::fs::canonicalize(source_home) {
        Ok(source) => source,
        Err(error) if missing(&error) => normalize(source_home),
        Err(error) => return Err(error.into()),
    };
    if source == home {
        anyhow::bail!("Private Codex storage must be separate from the shared Codex home.");
    }
    let destination = match share_mcp_credentials(&source, &home) {
        Ok(()) => home.clone(),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            relocate(private_home, &home, &source)?
        }
        Err(error) => return Err(error.into()),
    };
    let mut names: Vec<String> = SHARED_INPUTS.iter().map(|name| name.to_string()).collect();
    if let Ok(entries) = std::fs::read_dir(&source) {
        names.extend(
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".config.toml")),
        );
    }
    for name in names {
        let writable = WRITABLE_DIRECTORIES.contains(&name.as_str());
        if writable {
            mkdir_private(&source.join(&name))?;
        }
        share(&source.join(&name), &destination.join(&name), writable)?;
    }
    Ok(destination)
}

/// Prepares the private home before each launch, one preparation at a time per home.
pub async fn prepare_private_codex_home(
    private_home: &str,
    source_home: &str,
) -> anyhow::Result<PathBuf> {
    static PREPARING: OnceLock<Mutex<HashMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let key = normalize(Path::new(private_home));
    let gate = PREPARING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(key)
        .or_default()
        .clone();
    let _held = gate.lock().await;
    let (private_home, source_home) = (PathBuf::from(private_home), PathBuf::from(source_home));
    tokio::task::spawn_blocking(move || prepare(&private_home, &source_home)).await?
}

/// The user's own Codex home.
pub fn shared_codex_home(environment: &HashMap<String, String>) -> String {
    environment
        .get("CODEX_HOME")
        .map(|home| home.trim())
        .filter(|home| !home.is_empty())
        .map(String::from)
        .unwrap_or_else(|| {
            crate::claude::services::home_dir()
                .join(".codex")
                .to_string_lossy()
                .into_owned()
        })
}

/// Where Codex children keep their state, prepared for launch, or None when they use the user's home.
pub async fn private_codex_home(
    environment: &HashMap<String, String>,
) -> anyhow::Result<Option<PathBuf>> {
    match environment
        .get(PRIVATE_CODEX_HOME_ENV)
        .filter(|home| !home.is_empty())
    {
        Some(private_home) => Ok(Some(
            prepare_private_codex_home(private_home, &shared_codex_home(environment)).await?,
        )),
        None => Ok(None),
    }
}

/// The environment a Codex conversation runs with: its private home where the app keeps one.
pub async fn codex_child_environment(
    environment: &HashMap<String, String>,
) -> anyhow::Result<HashMap<String, String>> {
    let mut env = environment.clone();
    if let Some(home) = private_codex_home(environment).await? {
        let home = home.to_string_lossy().into_owned();
        env.insert("CODEX_HOME".into(), home.clone());
        env.insert("CODEX_SQLITE_HOME".into(), home);
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_shared_inputs_and_keeps_runtime_state_private() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("codex");
        let private = root.path().join("private");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("config.toml"), "model = \"x\"").unwrap();
        let home = prepare(&private, &source).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            "model = \"x\""
        );
        assert!(std::fs::symlink_metadata(home.join("auth.json"))
            .unwrap()
            .file_type()
            .is_symlink());
        let shared = std::fs::metadata(source.join(".credentials.json")).unwrap();
        let linked = std::fs::metadata(home.join(".credentials.json")).unwrap();
        assert_eq!(shared.ino(), linked.ino());
        assert_eq!(prepare(&private, &source).unwrap(), home);
    }
}
