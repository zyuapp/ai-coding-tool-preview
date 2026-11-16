//! The registry of folders the app works in, kept in `workspaces.v1.json` beside the store.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    Project,
    Projectless,
    Worktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub id: String,
    pub kind: WorkspaceKind,
    pub root: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnavailableReason {
    Missing,
    NotDirectory,
    Inaccessible,
    Changed,
}

impl UnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UnavailableReason::Missing => "missing",
            UnavailableReason::NotDirectory => "not-directory",
            UnavailableReason::Inaccessible => "inaccessible",
            UnavailableReason::Changed => "changed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum WorkspaceResolution {
    Available { workspace: WorkspaceRecord },
    Unavailable { workspace: WorkspaceRecord, reason: UnavailableReason },
}

impl WorkspaceResolution {
    pub fn workspace(&self) -> &WorkspaceRecord {
        match self {
            WorkspaceResolution::Available { workspace } | WorkspaceResolution::Unavailable { workspace, .. } => workspace,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("Unknown workspace: {0}")]
    Unknown(String),
    #[error("{message}")]
    Registration { root: String, message: String },
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Serialize, Deserialize)]
struct RegistryFile {
    version: u32,
    workspaces: Vec<WorkspaceRecord>,
}

pub struct WorkspaceService {
    registry_path: PathBuf,
    projectless_root: PathBuf,
    /// Held across a registration and its write, so registrations and writes land one at a time.
    records: Mutex<Vec<WorkspaceRecord>>,
}

impl WorkspaceService {
    /// Reads the registry. A corrupt one is preserved beside itself and the app starts empty.
    pub async fn open(registry_path: impl Into<PathBuf>, projectless_root: impl Into<PathBuf>) -> Result<Self, WorkspaceError> {
        let registry_path = registry_path.into();
        let records = load(&registry_path).await?;
        Ok(Self { registry_path, projectless_root: projectless_root.into(), records: Mutex::new(records) })
    }

    pub async fn register_project(&self, root: &Path) -> Result<WorkspaceRecord, WorkspaceError> {
        self.register(WorkspaceKind::Project, root).await
    }

    pub async fn register_worktree(&self, root: &Path) -> Result<WorkspaceRecord, WorkspaceError> {
        self.register(WorkspaceKind::Worktree, root).await
    }

    pub async fn projectless(&self) -> Result<WorkspaceRecord, WorkspaceError> {
        tokio::fs::create_dir_all(&self.projectless_root).await?;
        let root = self.projectless_root.clone();
        self.register(WorkspaceKind::Projectless, &root).await
    }

    /// Drops a worktree's registration once its directory is gone, so the registry never outgrows the disk.
    pub async fn forget_worktree(&self, root: &Path) -> Result<(), WorkspaceError> {
        let canonical = crate::paths::resolve(root).to_string_lossy().into_owned();
        let mut records = self.records.lock().await;
        let before = records.len();
        records.retain(|record| !(record.kind == WorkspaceKind::Worktree && record.root == canonical));
        if records.len() == before {
            return Ok(());
        }
        self.write(&records).await
    }

    pub async fn resolve(&self, id: &str) -> Result<WorkspaceResolution, WorkspaceError> {
        let record = self.records.lock().await.iter().find(|record| record.id == id).cloned().ok_or_else(|| WorkspaceError::Unknown(id.to_string()))?;
        Ok(match tokio::fs::canonicalize(&record.root).await {
            Ok(actual) if actual.to_string_lossy() != record.root => WorkspaceResolution::Unavailable { workspace: record, reason: UnavailableReason::Changed },
            Ok(actual) => match tokio::fs::metadata(&actual).await {
                Ok(metadata) if metadata.is_dir() => WorkspaceResolution::Available { workspace: record },
                Ok(_) => WorkspaceResolution::Unavailable { workspace: record, reason: UnavailableReason::NotDirectory },
                Err(error) => WorkspaceResolution::Unavailable { workspace: record, reason: unavailable_reason(&error) },
            },
            Err(error) => WorkspaceResolution::Unavailable { workspace: record, reason: unavailable_reason(&error) },
        })
    }

    /// The root of a workspace that is there to work in, or why it is not.
    pub async fn available_root(&self, id: &str) -> Result<PathBuf, WorkspaceError> {
        match self.resolve(id).await? {
            WorkspaceResolution::Available { workspace } => Ok(PathBuf::from(workspace.root)),
            WorkspaceResolution::Unavailable { reason, workspace } => {
                Err(WorkspaceError::Registration { root: workspace.root, message: format!("Workspace is unavailable ({}).", reason.as_str()) })
            }
        }
    }

    pub async fn records(&self) -> Vec<WorkspaceRecord> {
        self.records.lock().await.clone()
    }

    async fn register(&self, kind: WorkspaceKind, root: &Path) -> Result<WorkspaceRecord, WorkspaceError> {
        let canonical = canonical_directory(root).await?;
        let mut records = self.records.lock().await;
        if let Some(existing) = records.iter().find(|record| record.kind == kind && record.root == canonical) {
            return Ok(existing.clone());
        }
        let record = WorkspaceRecord { id: uuid::Uuid::new_v4().to_string(), kind, root: canonical };
        records.push(record.clone());
        if let Err(error) = self.write(&records).await {
            records.pop();
            return Err(error);
        }
        Ok(record)
    }

    async fn write(&self, records: &[WorkspaceRecord]) -> Result<(), WorkspaceError> {
        if let Some(parent) = self.registry_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let registry = RegistryFile { version: 1, workspaces: records.to_vec() };
        let mut text = serde_json::to_string_pretty(&registry).map_err(std::io::Error::other)?;
        text.push('\n');
        let temporary = PathBuf::from(format!("{}.{}.tmp", self.registry_path.to_string_lossy(), uuid::Uuid::new_v4()));
        let written = async {
            tokio::fs::write(&temporary, text).await?;
            tokio::fs::rename(&temporary, &self.registry_path).await
        }
        .await;
        if let Err(error) = written {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.into());
        }
        Ok(())
    }
}

async fn load(registry_path: &Path) -> Result<Vec<WorkspaceRecord>, WorkspaceError> {
    let content = match tokio::fs::read_to_string(registry_path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    match serde_json::from_str::<serde_json::Value>(&content).ok().and_then(registry_records) {
        Some(records) => Ok(records),
        None => {
            let preserved = format!("{}.corrupt.{}.json", registry_path.to_string_lossy(), uuid::Uuid::new_v4());
            let mut file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&preserved).await?;
            tokio::io::AsyncWriteExt::write_all(&mut file, content.as_bytes()).await?;
            Ok(Vec::new())
        }
    }
}

/// Every record has to be whole, or the registry as a whole is treated as corrupt.
fn registry_records(value: serde_json::Value) -> Option<Vec<WorkspaceRecord>> {
    if value.get("version")?.as_u64()? != 1 {
        return None;
    }
    let records: Vec<WorkspaceRecord> = serde_json::from_value(value.get("workspaces")?.clone()).ok()?;
    records.iter().all(|record| !record.id.is_empty() && !record.root.is_empty()).then_some(records)
}

async fn canonical_directory(root: &Path) -> Result<String, WorkspaceError> {
    let shown = root.to_string_lossy().into_owned();
    let refused = |reason: UnavailableReason| WorkspaceError::Registration { root: shown.clone(), message: format!("Cannot register workspace ({}): {shown}", reason.as_str()) };
    let canonical = tokio::fs::canonicalize(root).await.map_err(|error| refused(unavailable_reason(&error)))?;
    match tokio::fs::metadata(&canonical).await {
        Ok(metadata) if metadata.is_dir() => Ok(canonical.to_string_lossy().into_owned()),
        Ok(_) => Err(refused(UnavailableReason::NotDirectory)),
        Err(error) => Err(refused(unavailable_reason(&error))),
    }
}

fn unavailable_reason(error: &std::io::Error) -> UnavailableReason {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => UnavailableReason::Missing,
        _ if error.raw_os_error() == Some(libc::ENOTDIR) => UnavailableReason::Missing,
        _ => UnavailableReason::Inaccessible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registering_twice_answers_with_the_same_record_and_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("data/workspaces.v1.json");
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let service = WorkspaceService::open(&registry, dir.path().join("projectless")).await.unwrap();
        let first = service.register_project(&project).await.unwrap();
        let again = service.register_project(&project).await.unwrap();
        assert_eq!(first, again);
        let reopened = WorkspaceService::open(&registry, dir.path().join("projectless")).await.unwrap();
        assert!(matches!(reopened.resolve(&first.id).await.unwrap(), WorkspaceResolution::Available { .. }));
    }

    #[tokio::test]
    async fn a_missing_folder_resolves_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let service = WorkspaceService::open(dir.path().join("w.json"), dir.path().join("p")).await.unwrap();
        let record = service.register_project(&project).await.unwrap();
        std::fs::remove_dir(&project).unwrap();
        assert!(matches!(service.resolve(&record.id).await.unwrap(), WorkspaceResolution::Unavailable { reason: UnavailableReason::Missing, .. }));
        assert!(matches!(service.resolve("nope").await, Err(WorkspaceError::Unknown(_))));
    }

    #[tokio::test]
    async fn a_corrupt_registry_is_preserved_and_the_app_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("w.json");
        std::fs::write(&registry, "{\"version\":1,\"workspaces\":[{\"id\":\"\",\"kind\":\"project\",\"root\":\"/x\"}]}").unwrap();
        let service = WorkspaceService::open(&registry, dir.path().join("p")).await.unwrap();
        assert!(service.records().await.is_empty());
        let preserved = std::fs::read_dir(dir.path()).unwrap().filter_map(Result::ok).filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt.")).count();
        assert_eq!(preserved, 1);
    }

    #[tokio::test]
    async fn refuses_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        let service = WorkspaceService::open(dir.path().join("w.json"), dir.path().join("p")).await.unwrap();
        let error = service.register_project(&file).await.unwrap_err();
        assert!(error.to_string().starts_with("Cannot register workspace (not-directory)"));
    }
}
