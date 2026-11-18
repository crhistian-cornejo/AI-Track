//! The logic of the in-app updater, without any UI. `docs/updater.md` is the
//! contract with the release pipeline: a manifest (`latest.json`), a minisign
//! signature per artifact, and how each platform installs.
//!
//! The flow: [`check`] reads the manifest and says whether a newer version
//! exists for this platform; [`download`] fetches the artifact (resuming a cut
//! download) and verifies size, sha256 and signature before it is trusted;
//! [`install`] swaps the app bundle (macOS) or starts the installer (Windows).
//! Every step returns an [`UpdateError`]; nothing is left half-installed.

pub mod download;
pub mod error;
pub mod install;
pub mod key;
pub mod manifest;
pub mod verify;
pub mod version;

pub use download::{Progress, download};
pub use error::UpdateError;
pub use install::{Launcher, SystemLauncher, bundle_from_exe, cleanup_leftovers, install_downloaded, prune_downloads};
pub use manifest::{Asset, Manifest, Update, check, evaluate, fetch_manifest, platform_key};
pub use version::Version;

/// The version this build is.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The fixed manifest URL (`AITRACK_UPDATE_URL` replaces it).
pub const DEFAULT_MANIFEST_URL: &str = "https://github.com/crhistian-cornejo/AI-Track/releases/latest/download/latest.json";

#[cfg(test)]
mod test_util;
