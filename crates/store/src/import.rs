//! A first launch on a machine that ran the Electron app copies that app's profile, so threads,
//! projects, settings and pairings carry over. The source is only ever read.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use crate::error::{StoreError, StoreResult};

/// Remembers where an imported profile came from, so its checkouts stay the app's own.
pub const IMPORT_MARKER: &str = "imported-from.v1.json";

/// What a folder can hold and still count as empty: Chromium's lock, and the Finder's leavings.
const DISPOSABLE: [&str; 4] = ["SingletonLock", "SingletonCookie", "SingletonSocket", ".DS_Store"];
const DATABASE: &str = "tasks.v3.sqlite";
/// Belong to the build that wrote them: its version, threads it meant to resume after its own update, and
/// the engine processes it was watching for memory, which are not this app's to signal.
const SKIPPED_FILES: [&str; 3] = ["launched-version", "update-relaunch.json", "session-processes.json"];
const LAUNCHED_VERSION: &str = "launched-version";
/// Chromium's own preferences, which hold the page zoom the window was left at.
const CHROMIUM_PREFERENCES: &str = "Preferences";
const FOLDERS: [&str; 4] = ["attachments", "message-images", "generated-images", "projectless"];
const CODEX_HOME: &str = "codex-private";
/// Codex runtime leftovers a new home rebuilds, and the credentials link the app makes again.
const CODEX_SKIPPED: [&str; 9] = [".credentials.json", ".aicodingtool-mcp-credentials.json", ".tmp", "tmp", "cache", "log", "logs_2.sqlite", "shell_snapshots", "thread-writer-locks"];

/// An Electron profile this build can import, with the root its checkouts were made under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSource {
    pub user_data: PathBuf,
    pub worktrees_root: PathBuf,
}

impl ImportSource {
    /// Every root the source's checkouts may be under.
    pub fn worktrees_roots(&self) -> Vec<PathBuf> {
        vec![self.worktrees_root.clone(), self.user_data.join("worktrees")]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub threads: usize,
    pub projects: usize,
}

/// The Electron profiles an installed build (`AI Coding Tool`, once `Threadline`) or a source build
/// (`AI Coding Tool Dev`) reads from, in order of preference.
pub fn electron_sources(app_data: &Path, home: &Path, packaged: bool) -> Vec<ImportSource> {
    let worktrees_root = home.join(if packaged { ".aicodingtool" } else { ".aicodingtool-dev" }).join("worktrees");
    let names: &[&str] = if packaged { &["AI Coding Tool", "Threadline"] } else { &["AI Coding Tool Dev"] };
    names.iter().map(|name| ImportSource { user_data: app_data.join(name), worktrees_root: worktrees_root.clone() }).collect()
}

/// The first source that holds a thread store.
pub fn find_source(sources: &[ImportSource]) -> Option<&ImportSource> {
    sources.iter().find(|source| source.user_data.join(DATABASE).is_file())
}

/// Whether a profile folder holds nothing of its own yet.
pub fn is_empty_profile(folder: &Path) -> bool {
    match fs::read_dir(folder) {
        Ok(entries) => entries.filter_map(Result::ok).all(|entry| DISPOSABLE.contains(&entry.file_name().to_string_lossy().as_ref())),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Roots recorded by an import, read from the profile's marker; empty when it was never imported.
pub fn imported_worktrees_roots(user_data: &Path) -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string(user_data.join(IMPORT_MARKER)) else { return Vec::new() };
    let Ok(marker) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    marker["worktreesRoots"].as_array().into_iter().flatten().filter_map(Value::as_str).map(PathBuf::from).collect()
}

/// Copies `source` into the empty profile at `target`. The copy is built beside the target and moved
/// into place whole, so a failed import leaves the target as it was. The copy records `version` as
/// launched: moving to this app is not an update of it, so its first launch owes no engine upgrades.
pub fn import_profile(source: &ImportSource, target: &Path, version: &str) -> StoreResult<ImportReport> {
    if !is_empty_profile(target) {
        return Err(StoreError::Invalid(format!("{} already holds data.", target.display())));
    }
    let staging = sibling(target, "importing");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;
    let built = build(source, target, &staging, version);
    if let Err(error) = built.as_ref() {
        let _ = fs::remove_dir_all(&staging);
        return Err(error.clone());
    }
    if target.exists() {
        for entry in fs::read_dir(target)?.filter_map(Result::ok) {
            let _ = fs::remove_file(entry.path());
        }
        fs::remove_dir(target)?;
    }
    if let Err(error) = fs::rename(&staging, target) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error.into());
    }
    built
}

fn sibling(target: &Path, suffix: &str) -> PathBuf {
    let name = target.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!("{name}.{suffix}"))
}

fn build(source: &ImportSource, target: &Path, staging: &Path, version: &str) -> StoreResult<ImportReport> {
    let from = &source.user_data;
    backup(&from.join(DATABASE), &staging.join(DATABASE))?;
    let report = repoint_database(&staging.join(DATABASE), from, target)?;
    for entry in fs::read_dir(from)?.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") && !SKIPPED_FILES.contains(&name.as_str()) && entry.file_type()?.is_file() {
            fs::copy(entry.path(), staging.join(&name))?;
        }
    }
    if from.join(CHROMIUM_PREFERENCES).is_file() {
        fs::copy(from.join(CHROMIUM_PREFERENCES), staging.join(CHROMIUM_PREFERENCES))?;
    }
    fs::write(staging.join(LAUNCHED_VERSION), version)?;
    repoint_registry(&staging.join("workspaces.v1.json"), from, target)?;
    for folder in FOLDERS {
        copy_tree(&from.join(folder), &staging.join(folder), &[])?;
    }
    copy_tree(&from.join(CODEX_HOME), &staging.join(CODEX_HOME), &CODEX_SKIPPED)?;
    let roots: Vec<String> = source.worktrees_roots().iter().map(|root| root.to_string_lossy().into_owned()).collect();
    let marker = json!({ "version": 1, "userData": from.to_string_lossy(), "worktreesRoots": roots, "importedAt": act_core::util::now() });
    fs::write(staging.join(IMPORT_MARKER), serde_json::to_vec_pretty(&marker)?)?;
    Ok(report)
}

/// A consistent copy of a live store through SQLite's backup, without writing to the source.
fn backup(from: &Path, to: &Path) -> StoreResult<()> {
    let source = Connection::open_with_flags(from, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    source.busy_timeout(std::time::Duration::from_secs(5))?;
    let mut destination = Connection::open(to)?;
    rusqlite::backup::Backup::new(&source, &mut destination)?.run_to_completion(256, std::time::Duration::from_millis(5), None)?;
    Ok(())
}

/// Attachments are stored by absolute path, and the app only reads its own: they follow the copy.
fn repoint_database(file: &Path, from: &Path, to: &Path) -> StoreResult<ImportReport> {
    let connection = Connection::open(file)?;
    let old = format!("{}/attachments/", from.to_string_lossy());
    let new = format!("{}/attachments/", to.to_string_lossy());
    for table in ["messages", "tasks", "subagent_activity"] {
        connection.execute(&format!("UPDATE {table} SET data = replace(data, ?1, ?2) WHERE instr(data, ?1) > 0"), [&old, &new])?;
    }
    let count = |table: &str| connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get::<_, i64>(0)).map(|count| count as usize);
    Ok(ImportReport { threads: count("tasks")?, projects: count("projects")? })
}

/// Workspace roots are stored absolute, so the ones inside the source folder move with the copy.
fn repoint_registry(registry: &Path, from: &Path, to: &Path) -> StoreResult<()> {
    let Ok(before) = fs::read_to_string(registry) else { return Ok(()) };
    let escape = |path: &Path| {
        let quoted = serde_json::to_string(&format!("{}/", path.to_string_lossy())).unwrap_or_default();
        quoted[1..quoted.len() - 1].to_string()
    };
    let after = before.replace(&escape(from), &escape(to));
    if after != before {
        fs::write(registry, after)?;
    }
    Ok(())
}

/// Copies a folder, keeping links as links. SQLite files go through the backup so a live one is
/// consistent; their journals are left behind.
fn copy_tree(from: &Path, to: &Path, skipped: &[&str]) -> StoreResult<()> {
    let entries = match fs::read_dir(from) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    fs::create_dir_all(to)?;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if skipped.contains(&name.as_str()) || name.ends_with(".lock") || name.ends_with("-wal") || name.ends_with("-shm") || name.ends_with("-journal") {
            continue;
        }
        let path = entry.path();
        let destination = to.join(&name);
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            act_platform::fs::symlink(fs::read_link(&path)?, &destination)?;
        } else if kind.is_dir() {
            copy_tree(&path, &destination, &[])?;
        } else if name.ends_with(".sqlite") {
            backup(&path, &destination)?;
        } else if kind.is_file() {
            fs::copy(&path, &destination)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn electron_profile(root: &Path) -> ImportSource {
        let source = root.join("AI Coding Tool Dev");
        fs::create_dir_all(source.join("attachments")).unwrap();
        fs::create_dir_all(source.join("codex-private/sessions")).unwrap();
        fs::write(source.join("attachments/a.png"), b"png").unwrap();
        fs::write(source.join("codex-private/sessions/s.jsonl"), b"{}").unwrap();
        fs::write(source.join("codex-private/.credentials.json"), b"{}").unwrap();
        act_platform::fs::symlink("/elsewhere/config.toml", source.join("codex-private/config.toml")).unwrap();
        fs::write(source.join("launched-version"), b"0.10.0").unwrap();
        let projectless = format!("{}/projectless", source.to_string_lossy());
        fs::write(source.join("workspaces.v1.json"), serde_json::to_vec(&json!({ "version": 1, "workspaces": [{ "id": "p", "kind": "projectless", "root": projectless }] })).unwrap()).unwrap();
        let database = crate::TaskDatabase::open(&source.join(DATABASE), crate::OpenOptions::default()).unwrap();
        drop(database);
        let connection = Connection::open(source.join(DATABASE)).unwrap();
        connection.execute("INSERT INTO tasks (id, data) VALUES ('t', '{}')", []).unwrap();
        let message = json!({ "id": "m", "kind": "user", "text": "hi", "attachments": [format!("{}/attachments/a.png", source.to_string_lossy())] });
        connection.execute("INSERT INTO messages (task_id, id, position, data) VALUES ('t', 'm', 0, ?1)", [message.to_string()]).unwrap();
        ImportSource { user_data: source, worktrees_root: root.join(".aicodingtool-dev/worktrees") }
    }

    #[test]
    fn an_import_copies_the_profile_and_points_it_at_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let source = electron_profile(dir.path());
        let target = dir.path().join("AI Coding Tool Preview Dev");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join(".DS_Store"), b"").unwrap();
        let report = import_profile(&source, &target, "1.2.3").unwrap();
        assert_eq!(report.threads, 1);

        let connection = Connection::open(target.join(DATABASE)).unwrap();
        let data: String = connection.query_row("SELECT data FROM messages", [], |row| row.get(0)).unwrap();
        assert!(data.contains(&format!("{}/attachments/a.png", target.to_string_lossy())), "{data}");
        let registry = fs::read_to_string(target.join("workspaces.v1.json")).unwrap();
        assert!(registry.contains(&format!("{}/projectless", target.to_string_lossy())), "{registry}");
        assert!(target.join("attachments/a.png").is_file());
        assert!(target.join("codex-private/sessions/s.jsonl").is_file());
        assert!(!target.join("codex-private/.credentials.json").exists());
        assert_eq!(fs::read_link(target.join("codex-private/config.toml")).unwrap(), PathBuf::from("/elsewhere/config.toml"));
        assert_eq!(fs::read_to_string(target.join("launched-version")).unwrap(), "1.2.3");
        assert_eq!(imported_worktrees_roots(&target), source.worktrees_roots());
        assert!(source.user_data.join("launched-version").exists());
        let original: String = Connection::open(source.user_data.join(DATABASE)).unwrap().query_row("SELECT data FROM messages", [], |row| row.get(0)).unwrap();
        assert!(original.contains(&format!("{}/attachments/a.png", source.user_data.to_string_lossy())));
    }

    #[test]
    fn a_profile_with_data_is_never_imported_over() {
        let dir = tempfile::tempdir().unwrap();
        let source = electron_profile(dir.path());
        let target = dir.path().join("rust");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("window.v1.json"), b"{}").unwrap();
        assert!(import_profile(&source, &target, "1.2.3").is_err());
        assert_eq!(fs::read(target.join("window.v1.json")).unwrap(), b"{}");
        assert!(!target.join(DATABASE).exists());
    }
}
