//! What can go wrong, in words the app can show.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    #[error("could not reach the update server: {0}")]
    Network(String),
    #[error("the update server answered {0}")]
    Http(u16),
    #[error("the update manifest is not valid: {0}")]
    Manifest(String),
    #[error("\"{0}\" is not a version number")]
    BadVersion(String),
    #[error("this release has no download for {0}")]
    NoPlatform(String),
    #[error("refusing to download over an insecure address: {0}")]
    InsecureUrl(String),
    #[error("{context}: {message}")]
    Io { context: String, message: String },
    #[error("the download is {got} bytes but the manifest says {expected}")]
    SizeMismatch { expected: u64, got: u64 },
    #[error("the download's checksum does not match the manifest")]
    HashMismatch,
    #[error("the update's signature is not valid: {0}")]
    BadSignature(String),
    #[error("the update is not signed by AI-Track's key")]
    SignatureInvalid,
    #[error("this build has no update key, so it cannot install updates")]
    Unconfigured,
    #[error("the embedded update key cannot be read: {0}")]
    BadPublicKey(String),
    #[error("AI-Track is not running from an installed app ({0}); update it by hand")]
    NotBundled(PathBuf),
    #[error("{0} cannot be written to; download the new version from the releases page")]
    NotWritable(PathBuf),
    #[error("the update archive is not valid: {0}")]
    BadArchive(String),
    #[error("the update was installed but AI-Track could not restart: {0}")]
    Relaunch(String),
    #[error("cancelled")]
    Cancelled,
    #[error("updates are not supported on this system")]
    Unsupported,
}

impl UpdateError {
    pub fn io(context: impl Into<String>, e: impl std::fmt::Display) -> Self {
        Self::Io { context: context.into(), message: e.to_string() }
    }

    /// Whether this failure is the user's connection rather than the update itself.
    pub fn is_network(&self) -> bool {
        matches!(self, Self::Network(_) | Self::Http(_))
    }
}
