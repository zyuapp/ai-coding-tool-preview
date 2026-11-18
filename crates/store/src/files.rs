//! The small JSON files a profile keeps beside the database: paired computers, the window's theme
//! and placement, the workspace registry, the phone bridge settings, and the launch records.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use act_core::persisted::js;

fn read_json(file: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
}

/// Writes through a staging file and renames it over the target, so a reader never sees half.
fn replace_file(file: &Path, staging: &Path, contents: &str, private: bool) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    if private {
        act_platform::fs::private(&mut options);
    }
    std::io::Write::write_all(&mut options.open(staging)?, contents.as_bytes())?;
    std::fs::rename(staging, file)
}

fn with_suffix(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A computer this one has paired with (`computers.v1.json`). `name` is what it calls itself,
/// `label` what the user here calls it, and `node` Tailscale's stable id for the machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredComputer {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub token: String,
    #[serde(serialize_with = "js::num")]
    pub paired_at: f64,
}

/// The paired-computers file: this computer's chosen name, when one was chosen, and its links.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComputersFile {
    pub name: Option<String>,
    pub computers: Vec<StoredComputer>,
}

fn is_stored_computer(value: &Value) -> bool {
    let Some(record) = value.as_object() else { return false };
    let string = |key: &str| record.get(key).is_some_and(Value::is_string);
    let optional = |key: &str| record.get(key).is_none_or(Value::is_string);
    string("id") && string("name") && optional("label") && optional("node") && string("host") && string("token")
        && record.get("pairedAt").is_some_and(Value::is_number)
}

/// Unreadable files and records read as none.
pub fn read_computers(file: &Path) -> ComputersFile {
    let Some(stored) = read_json(file) else { return ComputersFile::default() };
    let name = stored.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).map(str::to_owned);
    let computers = stored
        .get("computers")
        .and_then(Value::as_array)
        .map(|computers| {
            computers
                .iter()
                .filter(|computer| is_stored_computer(computer))
                .filter_map(|computer| serde_json::from_value(computer.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    ComputersFile { name, computers }
}

/// Written readable by the user alone, since each link's token lets this computer back in.
pub fn write_computers(file: &Path, stored: &ComputersFile) -> std::io::Result<()> {
    let mut record = serde_json::Map::new();
    record.insert("version".into(), Value::from(1));
    if let Some(name) = &stored.name {
        record.insert("name".into(), Value::from(name.as_str()));
    }
    record.insert("computers".into(), serde_json::to_value(&stored.computers).map_err(std::io::Error::other)?);
    let text = serde_json::to_string(&record).map_err(std::io::Error::other)?;
    replace_file(file, &with_suffix(file, ".tmp"), &text, true)
}

/// What this computer calls itself: the chosen name, else `fallback`.
pub fn stored_computer_name(file: &Path, fallback: &str) -> String {
    read_computers(file).name.unwrap_or_else(|| fallback.to_owned())
}

/// What the window needs of a theme: the ground its frame is drawn on, and the colour it paints bare.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowTheme {
    /// `"dark"` or `"light"`.
    pub variant: String,
    /// `#rrggbb`.
    pub canvas: String,
    /// Whether the renderer took this ground from the system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<bool>,
}

impl Default for WindowTheme {
    fn default() -> Self {
        WindowTheme { variant: "dark".to_owned(), canvas: "#0e1117".to_owned(), follow: None }
    }
}

pub fn is_window_theme(value: &Value) -> bool {
    let Some(theme) = value.as_object() else { return false };
    matches!(theme.get("variant").and_then(Value::as_str), Some("dark" | "light"))
        && theme.get("canvas").and_then(Value::as_str).is_some_and(|canvas| {
            canvas.len() == 7 && canvas.starts_with('#') && canvas[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        && theme.get("follow").is_none_or(Value::is_boolean)
}

/// An unreadable file means the default, which is what a first launch reads anyway.
pub fn load_window_theme(file: &Path) -> WindowTheme {
    read_json(file)
        .filter(is_window_theme)
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

pub fn save_window_theme(file: &Path, theme: &WindowTheme) -> std::io::Result<()> {
    std::fs::write(file, serde_json::to_string(theme).map_err(std::io::Error::other)?)
}

/// Where the window sat, and whether the platform was holding it open at its largest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowPlacement {
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "js::opt_num")]
    pub y: Option<f64>,
    #[serde(serialize_with = "js::num")]
    pub width: f64,
    #[serde(serialize_with = "js::num")]
    pub height: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximized: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_screen: Option<bool>,
}

pub const DEFAULT_PLACEMENT: WindowPlacement =
    WindowPlacement { x: None, y: None, width: 1240.0, height: 820.0, maximized: None, full_screen: None };
pub const MIN_WINDOW_WIDTH: f64 = 820.0;
pub const MIN_WINDOW_HEIGHT: f64 = 620.0;
/// How much of the window's top edge has to stay on a screen for the user to drag it back.
const VISIBLE_EDGE: f64 = 80.0;

/// A screen's work area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenArea {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn is_window_placement(value: &Value) -> bool {
    let Some(placement) = value.as_object() else { return false };
    let number = |key: &str| placement.get(key).and_then(Value::as_f64);
    let (Some(width), Some(height)) = (number("width"), number("height")) else { return false };
    width >= MIN_WINDOW_WIDTH
        && height >= MIN_WINDOW_HEIGHT
        && ["x", "y"].iter().all(|key| placement.get(*key).is_none_or(Value::is_number))
        && ["maximized", "fullScreen"].iter().all(|key| placement.get(*key).is_none_or(Value::is_boolean))
}

pub fn load_window_placement(file: &Path) -> WindowPlacement {
    read_json(file)
        .filter(is_window_placement)
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or(DEFAULT_PLACEMENT)
}

pub fn save_window_placement(file: &Path, placement: &WindowPlacement) -> std::io::Result<()> {
    std::fs::write(file, serde_json::to_string(placement).map_err(std::io::Error::other)?)
}

/// The screen that holds the window's title bar.
fn screen_under(placement: &WindowPlacement, screens: &[ScreenArea]) -> Option<ScreenArea> {
    let (x, y) = (placement.x?, placement.y?);
    screens.iter().copied().find(|screen| {
        x + placement.width > screen.x
            && x < screen.x + screen.width
            && y + VISIBLE_EDGE > screen.y
            && y < screen.y + screen.height
    })
}

/// Fits a remembered placement to the screens there are now. A window wider than its screen is cut
/// down to it, and one on a screen that is gone loses its position so the platform places it again.
pub fn fit_placement(placement: &WindowPlacement, screens: &[ScreenArea]) -> WindowPlacement {
    let Some(first) = screens.first().copied() else { return placement.clone() };
    let under = screen_under(placement, screens);
    let screen = under.unwrap_or(first);
    let width = MIN_WINDOW_WIDTH.max(placement.width.min(screen.width));
    let height = MIN_WINDOW_HEIGHT.max(placement.height.min(screen.height));
    let mut fitted = WindowPlacement { width, height, ..placement.clone() };
    match (placement.x, placement.y, under) {
        (Some(x), Some(y), Some(_)) => {
            fitted.x = Some((x.max(screen.x - width + VISIBLE_EDGE)).min(screen.x + screen.width - VISIBLE_EDGE));
            fitted.y = Some((y.max(screen.y)).min(screen.y + screen.height - VISIBLE_EDGE));
        }
        _ => {
            fitted.x = None;
            fitted.y = None;
        }
    }
    fitted
}

/// What the workspace registry calls a root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    Project,
    Projectless,
    Worktree,
}

/// A directory the app has registered, by the id threads and projects name it with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub id: String,
    pub kind: WorkspaceKind,
    pub root: String,
}

fn is_workspace_record(value: &Value) -> bool {
    let Some(record) = value.as_object() else { return false };
    let filled = |key: &str| record.get(key).and_then(Value::as_str).is_some_and(|text| !text.is_empty());
    filled("id") && filled("root") && matches!(record.get("kind").and_then(Value::as_str), Some("project" | "projectless" | "worktree"))
}

/// The registered workspaces. A missing file is none. One that will not read is set aside beside
/// itself under a `.corrupt.<id>.json` name, so it is never lost, and the registry starts empty.
pub fn read_workspaces(file: &Path) -> std::io::Result<Vec<WorkspaceRecord>> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let registry = serde_json::from_str::<Value>(&text).ok().filter(|registry| {
        registry.get("version").and_then(Value::as_f64) == Some(1.0)
            && registry.get("workspaces").and_then(Value::as_array).is_some_and(|items| items.iter().all(is_workspace_record))
    });
    let Some(registry) = registry else {
        let preserved = with_suffix(file, &format!(".corrupt.{}.json", uuid::Uuid::new_v4()));
        std::fs::OpenOptions::new().write(true).create_new(true).open(&preserved).and_then(|mut target| {
            std::io::Write::write_all(&mut target, text.as_bytes())
        })?;
        return Ok(Vec::new());
    };
    serde_json::from_value(registry["workspaces"].clone()).map_err(std::io::Error::other)
}

/// Replaces the registry whole, pretty-printed as the TypeScript build writes it.
pub fn write_workspaces(file: &Path, workspaces: &[WorkspaceRecord]) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(&serde_json::json!({ "version": 1, "workspaces": workspaces }))
        .map_err(std::io::Error::other)?;
    let staging = with_suffix(file, &format!(".{}.tmp", uuid::Uuid::new_v4()));
    let written = replace_file(file, &staging, &format!("{text}\n"), false);
    if written.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    written
}

/// The phone bridge's settings (`mobile.v1.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MobileSettings {
    pub enabled: bool,
    pub magic_dns_name: Option<String>,
}

pub fn read_mobile_settings(file: &Path) -> MobileSettings {
    let Some(stored) = read_json(file).filter(|value| !value.is_null()) else { return MobileSettings::default() };
    MobileSettings {
        enabled: stored.get("enabled") == Some(&Value::Bool(true)),
        magic_dns_name: stored.get("magicDnsName").and_then(Value::as_str).filter(|name| !name.is_empty()).map(str::to_owned),
    }
}

pub fn write_mobile_settings(file: &Path, settings: &MobileSettings) -> std::io::Result<()> {
    let value = serde_json::json!({ "version": 1, "enabled": settings.enabled, "magicDnsName": settings.magic_dns_name });
    std::fs::write(file, value.to_string())
}

/// Negative when `left` is older, zero when they match, positive when `left` is newer.
pub fn compare_versions(left: &str, right: &str) -> f64 {
    let parts = |version: &str| -> Vec<f64> {
        version.split('.').map(|part| if part.trim().is_empty() { 0.0 } else { part.trim().parse::<f64>().unwrap_or(f64::NAN) }).collect()
    };
    let (left, right) = (parts(left), parts(right));
    for index in 0..left.len().max(right.len()) {
        let difference = left.get(index).copied().unwrap_or(0.0) - right.get(index).copied().unwrap_or(0.0);
        if difference != 0.0 {
            return difference;
        }
    }
    0.0
}

/// The version this launch updated the app to, or none when it is not the first launch since an
/// update. A profile from before the record was kept counts as updated when its window storage
/// exists. Read once at startup, before the window writes there.
pub fn read_app_launch(user_data: &Path, version: &str) -> Option<String> {
    let record = user_data.join("launched-version");
    let previous = std::fs::read_to_string(&record).ok().map(|text| text.trim().to_owned()).filter(|text| !text.is_empty());
    if previous.as_deref() == Some(version) {
        return None;
    }
    let updated = match &previous {
        None => user_data.join(crate::json_storage::WINDOW_STORAGE_FILE).exists(),
        Some(previous) => compare_versions(version, previous) > 0.0,
    };
    let _ = std::fs::write(&record, version);
    updated.then(|| version.to_owned())
}

/// Writes down the threads to carry on once `version` is the build running.
pub fn record_update_relaunch(user_data: &Path, version: &str, task_ids: &[String]) {
    let record = user_data.join("update-relaunch.json");
    let result = if task_ids.is_empty() {
        match std::fs::remove_file(&record) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    } else {
        std::fs::write(&record, serde_json::json!({ "version": version, "taskIds": task_ids }).to_string())
    };
    if let Err(error) = result {
        eprintln!("Could not record the threads to resume after updating: {error}");
    }
}

/// The threads the last update cut off, read once. A record for another version is dropped.
pub fn take_update_relaunch(user_data: &Path, version: &str) -> Vec<String> {
    let record = user_data.join("update-relaunch.json");
    let Ok(raw) = std::fs::read_to_string(&record) else { return Vec::new() };
    let _ = std::fs::remove_file(&record);
    let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(&raw) else { return Vec::new() };
    if parsed.get("version").and_then(Value::as_str) != Some(version) {
        return Vec::new();
    }
    parsed
        .get("taskIds")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_off_every_screen_loses_its_position() {
        let screens = [ScreenArea { x: 0.0, y: 0.0, width: 1440.0, height: 900.0 }];
        let placement = WindowPlacement { x: Some(5000.0), y: Some(10.0), width: 2000.0, height: 700.0, ..DEFAULT_PLACEMENT };
        let fitted = fit_placement(&placement, &screens);
        assert_eq!((fitted.x, fitted.width), (None, 1440.0));
        let onscreen = WindowPlacement { x: Some(1400.0), y: Some(-50.0), ..DEFAULT_PLACEMENT };
        let fitted = fit_placement(&onscreen, &screens);
        assert_eq!((fitted.x, fitted.y), (Some(1360.0), Some(0.0)));
    }

    #[test]
    fn update_relaunch_is_read_once_and_only_for_its_version() {
        let dir = tempfile::tempdir().unwrap();
        record_update_relaunch(dir.path(), "1.2.0", &["a".to_owned()]);
        assert!(take_update_relaunch(dir.path(), "1.1.0").is_empty());
        record_update_relaunch(dir.path(), "1.2.0", &["a".to_owned()]);
        assert_eq!(take_update_relaunch(dir.path(), "1.2.0"), vec!["a".to_owned()]);
        assert!(take_update_relaunch(dir.path(), "1.2.0").is_empty());
    }

    #[test]
    fn launch_after_an_update_is_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("launched-version"), "0.9.10").unwrap();
        assert_eq!(read_app_launch(dir.path(), "0.10.1").as_deref(), Some("0.10.1"));
        assert_eq!(read_app_launch(dir.path(), "0.10.1"), None);
        assert_eq!(read_app_launch(dir.path(), "0.9.0"), None);
    }
}
