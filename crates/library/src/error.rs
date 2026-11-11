use thiserror::Error;

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("audio: {0}")]
    Audio(String),
    #[error("file watcher: {0}")]
    Watch(#[from] notify::Error),
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = LibraryError> = std::result::Result<T, E>;
