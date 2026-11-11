//! The sound bank manifest: packs bundled with AI-Track (`[[pack]]`, only
//! redistributable licenses) and the Content Hub (`[[hub]]`, link-only packs
//! and plugins the user downloads from the official page).
//!
//! The canonical file is `content/soundbank/manifest.toml`; it is compiled in
//! so the app can show the hub without network access.

use serde::{Deserialize, Serialize};

use crate::error::{LibraryError, Result};
use crate::taxonomy::Category;

/// Licenses that allow bundling inside another free product.
pub const REDISTRIBUTABLE_LICENSES: &[&str] = &["CC0-1.0", "CC-BY-3.0", "CC-BY-4.0"];

static BUNDLED: &str = include_str!("../../../content/soundbank/manifest.toml");

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default, rename = "pack")]
    pub packs: Vec<BundledPack>,
    #[serde(default)]
    pub hub: Vec<HubEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    #[default]
    Zip,
    TarGz,
    TarXz,
    #[serde(rename = "7z")]
    SevenZ,
    /// Individual files from a GitHub repository at a pinned revision.
    GithubTree,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GithubSource {
    /// `owner/name`.
    pub repo: String,
    /// Commit SHA (immutable).
    pub rev: String,
    /// Path prefixes to fetch.
    pub paths: Vec<String>,
}

/// A pack shipped inside the factory library.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BundledPack {
    pub id: String,
    pub name: String,
    pub author: String,
    pub homepage: String,
    #[serde(default)]
    pub download_url: String,
    /// sha256 of the archive (for `github_tree`: of the sorted `path\tblob-sha\n` list).
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size_mb: f64,
    /// SPDX identifier.
    pub license: String,
    pub license_url: String,
    pub redistributable: bool,
    /// Redistributable but left out of the default factory bank (size):
    /// fetched only on request, e.g. from the app's Content Hub.
    #[serde(default)]
    pub optional: bool,
    /// Where we read the license (file in the archive/repo or official page) and what it says.
    #[serde(default)]
    pub license_evidence: String,
    #[serde(default)]
    pub attribution: String,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub install_subdir: String,
    #[serde(default)]
    pub format: ArchiveFormat,
    #[serde(default)]
    pub github: Option<GithubSource>,
    /// Keep only archive entries under these prefixes (after `strip_prefix`).
    #[serde(default)]
    pub include: Vec<String>,
    /// Drop entries whose path contains any of these substrings.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Leading folder inside the archive to remove.
    #[serde(default)]
    pub strip_prefix: String,
    /// License file inside the archive/repo, copied next to the pack as `LICENSE`.
    #[serde(default)]
    pub license_file: String,
    #[serde(default)]
    pub default_category: Option<String>,
    #[serde(default)]
    pub default_subcategory: Option<String>,
    /// Filled after fetching.
    #[serde(default)]
    pub sample_count: u64,
    #[serde(default)]
    pub notes: String,
}

impl BundledPack {
    pub fn dir_name(&self) -> &str {
        if self.install_subdir.is_empty() { &self.id } else { &self.install_subdir }
    }

    pub fn category(&self) -> Option<Category> {
        self.default_category.as_deref().and_then(Category::parse)
    }

    /// Text for `ATTRIBUTION.md` and the about screen.
    pub fn credit(&self) -> String {
        if self.attribution.is_empty() {
            format!("{} by {} ({})", self.name, self.author, self.license)
        } else {
            self.attribution.clone()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubKind {
    SamplePack,
    Instrument,
    Plugin,
}

/// A link-only Content Hub entry: the user downloads it from the official page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HubEntry {
    pub id: String,
    pub name: String,
    pub author: String,
    pub kind: HubKind,
    pub homepage: String,
    #[serde(default)]
    pub download_page: String,
    /// SPDX id, or "Proprietary-Freeware" / "Royalty-Free-No-Redistribution".
    pub license: String,
    #[serde(default)]
    pub license_summary: String,
    #[serde(default)]
    pub redistributable: bool,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Plugin formats (VST3, CLAP, AU) or sample formats (WAV).
    #[serde(default)]
    pub formats: Vec<String>,
    #[serde(default)]
    pub os: Vec<String>,
    #[serde(default)]
    pub requires_account: bool,
    #[serde(default)]
    pub description: String,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self> {
        let m: Self = toml::from_str(text).map_err(|e| LibraryError::Manifest(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    /// The manifest compiled into this build.
    pub fn bundled() -> Self {
        Self::parse(BUNDLED).expect("content/soundbank/manifest.toml is valid")
    }

    /// Enforces the legal rule: only redistributable licenses in `[[pack]]`.
    pub fn validate(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        for p in &self.packs {
            if !ids.insert(p.id.as_str()) {
                return Err(LibraryError::Manifest(format!("duplicate id {}", p.id)));
            }
            if !p.redistributable {
                return Err(LibraryError::Manifest(format!("{}: [[pack]] must be redistributable", p.id)));
            }
            if !REDISTRIBUTABLE_LICENSES.contains(&p.license.as_str()) {
                return Err(LibraryError::Manifest(format!("{}: license {} is not allowed for bundling", p.id, p.license)));
            }
            if p.license.starts_with("CC-BY") && p.attribution.is_empty() {
                return Err(LibraryError::Manifest(format!("{}: CC-BY needs an attribution", p.id)));
            }
            if p.format == ArchiveFormat::GithubTree && p.github.is_none() {
                return Err(LibraryError::Manifest(format!("{}: github_tree needs [pack.github]", p.id)));
            }
            if p.format != ArchiveFormat::GithubTree && p.download_url.is_empty() {
                return Err(LibraryError::Manifest(format!("{}: missing download_url", p.id)));
            }
            if let Some(c) = &p.default_category
                && Category::parse(c).is_none()
            {
                return Err(LibraryError::Manifest(format!("{}: unknown category {c}", p.id)));
            }
        }
        for h in &self.hub {
            // Hub entries are link-only whatever their license (some are CC0
            // but too large to bundle); `redistributable` just reports the license.
            if !ids.insert(h.id.as_str()) {
                return Err(LibraryError::Manifest(format!("duplicate id {}", h.id)));
            }
            if h.homepage.is_empty() {
                return Err(LibraryError::Manifest(format!("{}: hub entry needs an official page", h.id)));
            }
        }
        Ok(())
    }

    pub fn pack(&self, id: &str) -> Option<&BundledPack> {
        self.packs.iter().find(|p| p.id == id)
    }

    pub fn hub_by_kind(&self, kind: HubKind) -> impl Iterator<Item = &HubEntry> {
        self.hub.iter().filter(move |h| h.kind == kind)
    }

    /// Markdown credits for every bundled pack.
    pub fn attribution_markdown(&self) -> String {
        let mut s = String::from(
            "# Sound bank attribution\n\n\
             <!-- Generated by `cargo xtask soundbank attribution` from manifest.toml. Do not edit by hand. -->\n\n\
             AI-Track bundles the following free sound packs. Each one keeps its own license; \
             a copy of it sits next to the pack as `packs/<id>/LICENSE`.\n\n\
             | Pack | Author | License | Source |\n|---|---|---|---|\n",
        );
        for p in &self.packs {
            let optional = if p.optional { " (optional download)" } else { "" };
            s.push_str(&format!(
                "| {}{optional} | {} | [{}]({}) | {} |\n",
                p.name, p.author, p.license, p.license_url, p.homepage
            ));
        }
        s.push_str("\n## Credits\n\n");
        for p in &self.packs {
            s.push_str(&format!("- **{}**: {}\n", p.name, p.credit()));
        }
        s
    }
}

impl From<&BundledPack> for crate::db::PackInfo {
    fn from(p: &BundledPack) -> Self {
        Self {
            id: p.dir_name().to_string(),
            name: p.name.clone(),
            author: Some(p.author.clone()),
            homepage: Some(p.homepage.clone()),
            license: Some(p.license.clone()),
            license_url: Some(p.license_url.clone()),
            attribution: Some(p.credit()),
            genres: p.genres.clone(),
            default_category: p.category(),
            default_subcategory: p.default_subcategory.clone(),
            factory: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_manifest_is_valid() {
        let m = Manifest::bundled();
        assert!(m.packs.iter().all(|p| p.redistributable));
        assert!(m.hub.iter().all(|h| !h.homepage.is_empty()));
        assert!(m.hub_by_kind(HubKind::Plugin).count() > 0);
        assert!(m.attribution_markdown().contains("| Pack |"));
    }

    #[test]
    fn rejects_non_redistributable_pack() {
        let bad = r#"
[[pack]]
id = "x"
name = "X"
author = "Y"
homepage = "https://example.com"
download_url = "https://example.com/x.zip"
license = "Royalty-Free"
license_url = "https://example.com"
redistributable = true
"#;
        assert!(Manifest::parse(bad).is_err());
        let bad2 = bad.replace("Royalty-Free", "CC-BY-4.0");
        assert!(Manifest::parse(&bad2).is_err(), "CC-BY without attribution");
        let ok = bad.replace("Royalty-Free", "CC0-1.0");
        assert!(Manifest::parse(&ok).is_ok());
    }
}
