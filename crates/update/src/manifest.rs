//! `latest.json`: what the newest release offers, per platform.

use crate::{UpdateError, Version};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

/// One platform's artifact.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub url: String,
    /// The full content of the `.minisig` file.
    pub signature: String,
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: Option<String>,
    pub platforms: BTreeMap<String, Asset>,
}

impl Manifest {
    pub fn parse(json: &str) -> Result<Self, UpdateError> {
        serde_json::from_str(json).map_err(|e| UpdateError::Manifest(e.to_string()))
    }
}

/// A newer version this platform can install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub version: Version,
    pub notes: String,
    pub pub_date: Option<String>,
    pub asset: Asset,
}

/// The manifest key of a system, `None` where updates are not offered.
pub fn platform_key_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", _) => Some("macos-universal"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        _ => None,
    }
}

/// This system's manifest key.
pub fn platform_key() -> Option<&'static str> {
    platform_key_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Downloads go over https; plain http only to this machine (tests, a local server).
pub fn check_url(url: &str) -> Result<(), UpdateError> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
        let name = if let Some(v6) = host.strip_prefix('[') { v6.split(']').next().unwrap_or("") } else { host.split(':').next().unwrap_or("") };
        if matches!(name, "localhost" | "127.0.0.1" | "::1") {
            return Ok(());
        }
    }
    Err(UpdateError::InsecureUrl(url.to_owned()))
}

/// Whether `manifest` offers something newer than `current` for `platform`.
pub fn evaluate(manifest: &Manifest, current: &str, platform: &str) -> Result<Option<Update>, UpdateError> {
    let offered = Version::parse(&manifest.version)?;
    if offered <= Version::parse(current)? {
        return Ok(None);
    }
    let asset = manifest.platforms.get(platform).ok_or_else(|| UpdateError::NoPlatform(platform.to_owned()))?;
    check_url(&asset.url)?;
    Ok(Some(Update { version: offered, notes: manifest.notes.clone(), pub_date: manifest.pub_date.clone(), asset: asset.clone() }))
}

pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .user_agent(concat!("AI-Track/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The manifest's text (at most 1 MiB).
pub fn fetch_manifest(url: &str) -> Result<String, UpdateError> {
    check_url(url)?;
    let resp = agent().get(url).call().map_err(|e| UpdateError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(UpdateError::Http(status));
    }
    let mut text = String::new();
    resp.into_body().into_reader().take(1 << 20).read_to_string(&mut text).map_err(|e| UpdateError::Network(e.to_string()))?;
    Ok(text)
}

/// Fetch the manifest at `url` and say what it offers over `current` for this system.
pub fn check(url: &str, current: &str) -> Result<Option<Update>, UpdateError> {
    let platform = platform_key().ok_or(UpdateError::Unsupported)?;
    evaluate(&Manifest::parse(&fetch_manifest(url)?)?, current, platform)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{
        "version": "0.0.2",
        "notes": "Fixes.",
        "pub_date": "2026-11-20T12:00:00Z",
        "platforms": {
            "macos-universal": {"url": "https://example.com/a.app.tar.gz", "signature": "sig-mac", "sha256": "ab", "size": 10},
            "windows-x86_64": {"url": "https://example.com/setup.exe", "signature": "sig-win", "sha256": "cd", "size": 20}
        }
    }"#;

    #[test]
    fn parses_the_documented_format() {
        let m = Manifest::parse(JSON).unwrap();
        assert_eq!(m.version, "0.0.2");
        assert_eq!(m.platforms["windows-x86_64"].size, 20);
        assert_eq!(m.pub_date.as_deref(), Some("2026-11-20T12:00:00Z"));
        // Notes and date are optional; the rest is not.
        assert!(Manifest::parse(r#"{"version":"1.0.0","platforms":{}}"#).is_ok());
        assert!(matches!(Manifest::parse(r#"{"platforms":{}}"#), Err(UpdateError::Manifest(_))));
        assert!(matches!(Manifest::parse("not json"), Err(UpdateError::Manifest(_))));
    }

    #[test]
    fn only_a_greater_version_is_an_update() {
        let m = Manifest::parse(JSON).unwrap();
        assert!(evaluate(&m, "0.0.2", "macos-universal").unwrap().is_none());
        assert!(evaluate(&m, "0.1.0", "macos-universal").unwrap().is_none());
        let u = evaluate(&m, "0.0.1", "macos-universal").unwrap().unwrap();
        assert_eq!(u.version.to_string(), "0.0.2");
        assert_eq!(u.asset.signature, "sig-mac");
        assert_eq!(u.notes, "Fixes.");
        // 0.0.10 is newer than 0.0.9, not older.
        let mut ten = m.clone();
        ten.version = "0.0.10".into();
        assert!(evaluate(&ten, "0.0.9", "macos-universal").unwrap().is_some());
    }

    #[test]
    fn picks_the_platform_and_reports_a_missing_one() {
        let m = Manifest::parse(JSON).unwrap();
        assert_eq!(evaluate(&m, "0.0.1", "windows-x86_64").unwrap().unwrap().asset.sha256, "cd");
        assert_eq!(evaluate(&m, "0.0.1", "linux-x86_64"), Err(UpdateError::NoPlatform("linux-x86_64".into())));
        // Nothing newer: a missing platform does not matter.
        assert_eq!(evaluate(&m, "0.0.2", "linux-x86_64"), Ok(None));
    }

    #[test]
    fn platform_keys_follow_the_contract() {
        assert_eq!(platform_key_for("macos", "aarch64"), Some("macos-universal"));
        assert_eq!(platform_key_for("macos", "x86_64"), Some("macos-universal"));
        assert_eq!(platform_key_for("windows", "x86_64"), Some("windows-x86_64"));
        assert_eq!(platform_key_for("windows", "aarch64"), None);
        assert_eq!(platform_key_for("linux", "x86_64"), None);
    }

    #[test]
    fn a_bad_version_is_an_error() {
        let mut m = Manifest::parse(JSON).unwrap();
        m.version = "soon".into();
        assert!(matches!(evaluate(&m, "0.0.1", "macos-universal"), Err(UpdateError::BadVersion(_))));
        m.version = "0.0.2".into();
        assert!(matches!(evaluate(&m, "dev", "macos-universal"), Err(UpdateError::BadVersion(_))));
    }

    #[test]
    fn plain_http_is_only_for_this_machine() {
        assert!(check_url("https://github.com/x").is_ok());
        assert!(check_url("http://127.0.0.1:8080/a").is_ok());
        assert!(check_url("http://localhost/a").is_ok());
        assert!(check_url("http://[::1]:9/a").is_ok());
        assert!(check_url("http://example.com/a").is_err());
        assert!(check_url("http://127.0.0.1@evil.com/a").is_err());
        assert!(check_url("ftp://example.com/a").is_err());
        let mut m = Manifest::parse(JSON).unwrap();
        m.platforms.get_mut("macos-universal").unwrap().url = "http://example.com/a".into();
        assert!(matches!(evaluate(&m, "0.0.1", "macos-universal"), Err(UpdateError::InsecureUrl(_))));
    }
}
