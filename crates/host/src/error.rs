use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("cannot load module {path}: {reason}")]
    Load { path: PathBuf, reason: String },
    #[error("plugin {0} not found in module")]
    NotFound(String),
    #[error("plugin instantiation failed: {0}")]
    Instantiate(String),
    #[error("plugin activation failed: {0}")]
    Activate(String),
    #[error("state error: {0}")]
    State(String),
    #[error("editor error: {0}")]
    Editor(String),
    #[error("plugin has no editor")]
    NoEditor,
    #[error("must be called on the thread that created the plugin (main thread)")]
    WrongThread,
    #[error("plugin crashed and was disabled")]
    Crashed,
    #[error("scan failed: {0}")]
    Scan(String),
    #[error("sandbox error: {0}")]
    Sandbox(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T, E = HostError> = std::result::Result<T, E>;
