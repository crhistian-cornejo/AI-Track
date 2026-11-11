use std::path::PathBuf;

/// Everything that can go wrong in `aitrack-ai`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not decode audio: {0}")]
    Decode(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("unknown model `{0}`")]
    UnknownModel(String),
    #[error("model `{0}` has a license notice that must be accepted before downloading")]
    LicenseNotAccepted(String),
    #[error("model `{0}` is not installed")]
    NotInstalled(String),
    #[error("model `{id}` is not available on this platform")]
    UnsupportedPlatform { id: String },
    #[error("download failed for {url}: {message}")]
    Download { url: String, message: String },
    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("ONNX Runtime error: {0}")]
    Runtime(String),
    #[error("unexpected model output: {0}")]
    ModelOutput(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

impl<T> From<ort::Error<T>> for Error {
    fn from(e: ort::Error<T>) -> Self {
        Self::Runtime(e.to_string())
    }
}
