//! Opt-in Splice integration for the Cmd+K palette.
//!
//! This talks to Splice's *unofficial* web API: the GraphQL endpoint its
//! own web app uses for search ([`query`], [`model`]) and the obfuscated
//! preview MP3s it plays ([`descramble`]). Previews are only auditioned,
//! never added to the library: getting a sample into the library goes
//! through the user's Splice account (Splice's site or desktop app), whose
//! download folder the app indexes. Sign-in ([`auth`]) happens on Splice's
//! own page in the system browser; the app never sees a password and keeps
//! the token in the OS credential store.
//!
//! Network calls run on the [`Service`] threads only.

pub mod auth;
pub mod client;
pub mod descramble;
pub mod model;
pub mod query;
pub mod service;

pub use model::{PackRef, SearchPage, SpliceSample, TagCount};
pub use query::{AssetKind, SearchParams, Sort};
pub use service::{Event, Service, WAVE_BARS};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network: {0}")]
    Network(String),
    #[error("unexpected answer from Splice: {0}")]
    Parse(String),
    #[error("Splice: {0}")]
    Api(String),
    #[error("{0}")]
    Auth(String),
    #[error("disk: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Where the Splice desktop app saves the sounds you license (its default).
pub fn default_sounds_folder() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(std::path::PathBuf::from(home).join("Splice").join("sounds"))
}

/// A sample's or pack's page on splice.com, where the user licenses it.
pub fn page_url(sample: &SpliceSample) -> String {
    sample.pack.as_ref().and_then(|p| p.page_url.clone()).unwrap_or_else(|| format!("https://splice.com/sounds/search?q={}", auth::encode(&sample.name)))
}
