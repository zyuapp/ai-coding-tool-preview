/// Why a store request failed. Cloneable so one failure can answer every request waiting on it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Sqlite(String),
    #[error("{0}")]
    Json(String),
    #[error("{0}")]
    Io(String),
    /// The store holds something this build refuses to load, such as an unreadable project.
    #[error("{0}")]
    Invalid(String),
    #[error("Thread no longer exists.")]
    ThreadGone,
    #[error("Task database is closing.")]
    Closing,
    #[error("Task database worker exited.")]
    WorkerExited,
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        StoreError::Sqlite(error.to_string())
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        StoreError::Json(error.to_string())
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        StoreError::Io(error.to_string())
    }
}

pub type StoreResult<T> = Result<T, StoreError>;
