//! The free-plugin catalog: what the Plugins tab offers, and what the installer
//! may fetch. The canonical file is `content/plugins/manifest.toml`, compiled in
//! so the tab works offline.
//!
//! Two lanes:
//! - `direct`: free software that the maker lets anyone download from their own
//!   server. The app downloads it on request, from the maker's domain only, and
//!   installs it for the user once its sha256 matches the pinned one.
//! - `link`: everything else (an account, the maker's own manager, a form, only
//!   an installer program). The tab opens the official page and recognises the
//!   plugin once it shows up in the plugin folders.
//!
//! Nothing here is ever mirrored on our servers. A `direct` artifact without a
//! pinned sha256 is treated as a link until `cargo xtask plugins pin` fills it in.

use serde::{Deserialize, Serialize};

use crate::error::{LibraryError, Result};

static BUNDLED: &str = include_str!("../../../content/plugins/manifest.toml");

/// Licences a catalog entry may carry: open source ones, or the label for
/// freeware that is free to use but not to redistribute. Anything else (trial,
/// demo, commercial) is rejected by [`Catalog::validate`].
pub const ALLOWED_LICENSES: &[&str] = &[
    "GPL-3.0",
    "GPL-3.0-or-later",
    "AGPL-3.0",
    "MIT",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "Apache-2.0",
    "MPL-2.0",
    "Proprietary-Freeware",
    "u-he-EULA",
];

pub const CATEGORIES: &[&str] = &["dynamics", "eq", "saturation", "reverb", "delay", "modulation", "glitch", "vocal", "utility", "synth", "sampler", "multi"];

/// Largest download any artifact may declare.
pub const MAX_DOWNLOAD_BYTES: u64 = 2 << 30;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    Direct,
    #[default]
    Link,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Macos,
    Windows,
}

impl Os {
    pub fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::Macos)
        } else if cfg!(windows) {
            Some(Self::Windows)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    #[default]
    Zip,
    TarGz,
    /// macOS disk image: mounted read-only and copied out of, nothing run.
    Dmg,
    /// macOS installer package: expanded with `pkgutil --expand-full`, never installed.
    Pkg,
}

impl Container {
    pub fn is_macos_only(self) -> bool {
        matches!(self, Self::Dmg | Self::Pkg)
    }
}

/// One downloadable file for one platform.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub os: Option<Os>,
    /// `universal`, `arm64` or `x86_64`.
    #[serde(default = "universal")]
    pub arch: String,
    pub url: String,
    /// Lower-case hex sha256 of the file. Empty until pinned.
    #[serde(default)]
    pub sha256: String,
    /// Exact size in bytes (the download limit). 0 until pinned.
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub container: Container,
    /// Apple Team ID the bundles must be signed with, when the maker signs.
    #[serde(default)]
    pub team_id: String,
}

fn universal() -> String {
    "universal".into()
}

impl Artifact {
    pub fn pinned(&self) -> bool {
        self.sha256.len() == 64 && self.size > 0
    }

    pub fn host(&self) -> String {
        url_host(&self.url).unwrap_or_default()
    }

    fn fits_arch(&self) -> bool {
        self.arch == "universal" || self.arch == std::env::consts::ARCH.replace("aarch64", "arm64")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginEntry {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub category: String,
    /// The version the pinned hashes belong to.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub homepage: String,
    /// Where `link` entries send the user.
    #[serde(default)]
    pub download_page: String,
    pub license: String,
    #[serde(default)]
    pub license_url: String,
    /// What the user reads before anything is downloaded.
    #[serde(default)]
    pub license_notice: String,
    #[serde(default)]
    pub lane: Lane,
    #[serde(default)]
    pub requires_account: bool,
    /// What the user must do on the page (`link` entries).
    #[serde(default)]
    pub account_hint: String,
    /// Formats AI-Track can host: `vst3`, `clap`.
    #[serde(default)]
    pub formats: Vec<String>,
    /// File names (without extension) of the bundles it installs, to recognise it.
    #[serde(default)]
    pub bundle_ids: Vec<String>,
    /// The maker's domains: every URL and redirect must stay inside them.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub artifact: Vec<Artifact>,
}

impl PluginEntry {
    /// The artifact for this machine, if the entry has one.
    pub fn artifact_for(&self, os: Os) -> Option<&Artifact> {
        self.artifact.iter().find(|a| a.os == Some(os) && a.fits_arch())
    }

    /// Whether the app can install it here: `direct`, with a pinned artifact for this OS.
    pub fn installable(&self, os: Os) -> bool {
        self.lane == Lane::Direct && self.artifact_for(os).is_some_and(|a| a.pinned() && (!a.container.is_macos_only() || os == Os::Macos))
    }

    /// The page the user is sent to when the app cannot install it.
    pub fn page(&self) -> &str {
        if self.download_page.is_empty() { &self.homepage } else { &self.download_page }
    }
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub plugin: Vec<PluginEntry>,
}

fn bad(msg: String) -> LibraryError {
    LibraryError::Manifest(msg)
}

/// The host of an `https://` URL, lower-case and without port or credentials.
/// `None` for anything else (other schemes, `user@host` tricks, no host).
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
        return None;
    }
    let host = authority.rsplit_once(':').map_or(authority, |(h, port)| if port.chars().all(|c| c.is_ascii_digit()) { h } else { authority });
    (!host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')).then(|| host.to_ascii_lowercase())
}

impl Catalog {
    pub fn parse(text: &str) -> Result<Self> {
        let c: Self = toml::from_str(text).map_err(|e| bad(e.to_string()))?;
        c.validate()?;
        Ok(c)
    }

    /// The catalog compiled into this build.
    pub fn bundled() -> Self {
        Self::parse(BUNDLED).expect("content/plugins/manifest.toml is valid")
    }

    pub fn get(&self, id: &str) -> Option<&PluginEntry> {
        self.plugin.iter().find(|p| p.id == id)
    }

    /// The rules that keep the installer safe and the catalog legal.
    pub fn validate(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        for p in &self.plugin {
            let id = &p.id;
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
                return Err(bad(format!("{id:?}: ids are lower-case letters, digits and dashes")));
            }
            if !ids.insert(id.as_str()) {
                return Err(bad(format!("duplicate id {id}")));
            }
            if p.name.is_empty() || p.vendor.is_empty() {
                return Err(bad(format!("{id}: name and vendor are required")));
            }
            if !CATEGORIES.contains(&p.category.as_str()) {
                return Err(bad(format!("{id}: unknown category {}", p.category)));
            }
            if !ALLOWED_LICENSES.contains(&p.license.as_str()) {
                return Err(bad(format!("{id}: licence {} is not allowed (free software only)", p.license)));
            }
            if p.formats.iter().any(|f| f != "vst3" && f != "clap") {
                return Err(bad(format!("{id}: formats are vst3 and clap")));
            }
            if url_host(&p.homepage).is_none() {
                return Err(bad(format!("{id}: the official page must be an https URL")));
            }
            if !p.download_page.is_empty() && url_host(&p.download_page).is_none() {
                return Err(bad(format!("{id}: download_page must be an https URL")));
            }
            if p.lane == Lane::Link {
                if !p.artifact.is_empty() {
                    return Err(bad(format!("{id}: a guided link has no artifacts")));
                }
                continue;
            }
            if p.requires_account {
                return Err(bad(format!("{id}: an entry that needs an account is a guided link")));
            }
            if p.license_notice.is_empty() {
                return Err(bad(format!("{id}: a downloadable entry needs a licence notice")));
            }
            if p.formats.is_empty() || p.artifact.is_empty() || p.allowed_hosts.is_empty() {
                return Err(bad(format!("{id}: formats, artifacts and allowed_hosts are required for lane direct")));
            }
            if p.allowed_hosts.iter().any(|h| h.is_empty() || h != &h.to_ascii_lowercase() || h.contains(['/', ':', '*', '@'])) {
                return Err(bad(format!("{id}: allowed_hosts are bare lower-case host names")));
            }
            let mut seen = std::collections::HashSet::new();
            for a in &p.artifact {
                let Some(os) = a.os else { return Err(bad(format!("{id}: artifact without os"))) };
                if !seen.insert((os, a.arch.clone())) {
                    return Err(bad(format!("{id}: two artifacts for {os:?}/{}", a.arch)));
                }
                if !["universal", "arm64", "x86_64"].contains(&a.arch.as_str()) {
                    return Err(bad(format!("{id}: unknown arch {}", a.arch)));
                }
                let Some(host) = url_host(&a.url) else { return Err(bad(format!("{id}: {} is not an https URL", a.url))) };
                if !p.allowed_hosts.contains(&host) {
                    return Err(bad(format!("{id}: {host} is not one of the maker's allowed_hosts")));
                }
                let hex = a.sha256.is_empty() || (a.sha256.len() == 64 && a.sha256.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
                if !hex {
                    return Err(bad(format!("{id}: sha256 is 64 lower-case hex digits, or empty until pinned")));
                }
                if (a.sha256.is_empty()) != (a.size == 0) {
                    return Err(bad(format!("{id}: sha256 and size are pinned together")));
                }
                if a.size > MAX_DOWNLOAD_BYTES {
                    return Err(bad(format!("{id}: {} bytes is over the download limit", a.size)));
                }
                if a.container.is_macos_only() && os != Os::Macos {
                    return Err(bad(format!("{id}: dmg and pkg are macOS containers")));
                }
            }
        }
        Ok(())
    }
}

/// Fill in the pinned `sha256` and `size` of one artifact in the manifest text,
/// keeping every comment and line. `None` when no such artifact is found.
pub fn pin_text(text: &str, id: &str, os: Os, arch: &str, sha256: &str, size: u64) -> Option<String> {
    let os_word = if os == Os::Macos { "macos" } else { "windows" };
    let mut out = Vec::new();
    let (mut in_plugin, mut in_artifact, mut matched_os, mut matched_arch, mut done) = (false, false, false, arch == "universal", false);
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("[[") {
            in_artifact = t == "[[plugin.artifact]]";
            if t == "[[plugin]]" {
                in_plugin = false;
                matched_os = false;
            }
            if in_artifact {
                matched_os = false;
                matched_arch = arch == "universal";
            }
        } else if t.starts_with("id") && !in_artifact && t.split('=').next().is_some_and(|k| k.trim() == "id") {
            in_plugin = t.split('=').nth(1).is_some_and(|v| v.trim().trim_matches('"') == id);
        } else if in_plugin && in_artifact {
            let key = t.split('=').next().unwrap_or("").trim();
            let val = t.split('=').nth(1).unwrap_or("").trim().trim_matches('"');
            match key {
                "os" => matched_os = val == os_word,
                "arch" => matched_arch = val == arch,
                _ => {}
            }
            if matched_os && matched_arch && !done {
                let indent = &line[..line.len() - line.trim_start().len()];
                if key == "sha256" {
                    out.push(format!("{indent}sha256 = \"{sha256}\""));
                    continue;
                }
                if key == "size" {
                    out.push(format!("{indent}size = {size}"));
                    done = true;
                    continue;
                }
            }
        }
        out.push(line.to_owned());
    }
    done.then(|| out.join("\n") + if text.ends_with('\n') { "\n" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[[plugin]]
id = "demo"
name = "Demo"
vendor = "Maker"
category = "dynamics"
version = "1.0"
homepage = "https://maker.example/demo"
license = "Proprietary-Freeware"
license_notice = "Free. Downloaded from the maker."
lane = "direct"
formats = ["vst3"]
allowed_hosts = ["maker.example"]
[[plugin.artifact]]
os = "macos"
url = "https://maker.example/demo.zip"
sha256 = ""
size = 0
"#;

    #[test]
    fn the_bundled_catalog_is_valid_and_has_both_lanes() {
        let c = Catalog::bundled();
        assert!(c.plugin.iter().any(|p| p.lane == Lane::Direct));
        assert!(c.plugin.iter().any(|p| p.lane == Lane::Link && p.requires_account));
        assert!(c.plugin.len() >= 40);
    }

    #[test]
    fn an_artifact_must_stay_on_the_makers_domain_and_on_https() {
        assert!(Catalog::parse(GOOD).is_ok());
        let other = GOOD.replace("https://maker.example/demo.zip", "https://cdn.other.example/demo.zip");
        assert!(Catalog::parse(&other).unwrap_err().to_string().contains("allowed_hosts"));
        let plain = GOOD.replace("https://maker.example/demo.zip", "http://maker.example/demo.zip");
        assert!(Catalog::parse(&plain).is_err());
        let sneaky = GOOD.replace("https://maker.example/demo.zip", "https://maker.example@evil.example/demo.zip");
        assert!(Catalog::parse(&sneaky).is_err());
    }

    #[test]
    fn only_free_licences_and_no_accounts_in_the_direct_lane() {
        assert!(Catalog::parse(&GOOD.replace("Proprietary-Freeware", "Commercial-Trial")).is_err());
        assert!(Catalog::parse(&GOOD.replace("lane = \"direct\"", "lane = \"direct\"\nrequires_account = true")).is_err());
        assert!(Catalog::parse(&GOOD.replace("license_notice = \"Free. Downloaded from the maker.\"\n", "")).is_err());
    }

    #[test]
    fn a_pinned_hash_needs_its_size_and_the_right_shape() {
        let hex = "a".repeat(64);
        assert!(Catalog::parse(&GOOD.replace("sha256 = \"\"", &format!("sha256 = \"{hex}\""))).is_err(), "hash without size");
        let ok = GOOD.replace("sha256 = \"\"", &format!("sha256 = \"{hex}\"")).replace("size = 0", "size = 10");
        assert!(Catalog::parse(&ok).is_ok());
        assert!(Catalog::parse(&ok.replace(&hex, "ABC")).is_err());
        assert!(Catalog::parse(&ok.replace("size = 10", &format!("size = {}", MAX_DOWNLOAD_BYTES + 1))).is_err());
    }

    #[test]
    fn a_dmg_is_a_macos_container_and_a_link_has_no_artifacts() {
        let win_dmg = GOOD.replace("os = \"macos\"", "os = \"windows\"\ncontainer = \"dmg\"");
        assert!(Catalog::parse(&win_dmg).is_err());
        let link = GOOD.replace("lane = \"direct\"", "lane = \"link\"");
        assert!(Catalog::parse(&link).is_err());
    }

    #[test]
    fn unpinned_entries_are_not_installable_and_pinned_ones_are() {
        let mut c = Catalog::parse(GOOD).unwrap();
        assert!(!c.plugin[0].installable(Os::Macos));
        c.plugin[0].artifact[0].sha256 = "b".repeat(64);
        c.plugin[0].artifact[0].size = 5;
        assert!(c.plugin[0].installable(Os::Macos));
        assert!(!c.plugin[0].installable(Os::Windows), "no artifact for Windows");
    }

    #[test]
    fn host_parsing_is_strict() {
        assert_eq!(url_host("https://WWW.Maker.example:443/x?y"), Some("www.maker.example".into()));
        assert_eq!(url_host("https://a.example/x#f"), Some("a.example".into()));
        assert_eq!(url_host("ftp://a.example"), None);
        assert_eq!(url_host("https://"), None);
        assert_eq!(url_host("https://a.example\\@b.example/"), None);
    }

    #[test]
    fn pinning_rewrites_only_the_matching_artifact_and_keeps_the_rest() {
        let text = GOOD.replace("[[plugin.artifact]]\nos = \"macos\"", "# first\n[[plugin.artifact]]\nos = \"windows\"\nurl = \"https://maker.example/w.zip\"\nsha256 = \"\"\nsize = 0\n[[plugin.artifact]]\nos = \"macos\"");
        let pinned = pin_text(&text, "demo", Os::Macos, "universal", &"c".repeat(64), 123).unwrap();
        assert!(pinned.contains(&format!("sha256 = \"{}\"", "c".repeat(64))) && pinned.contains("size = 123"));
        assert_eq!(pinned.matches("sha256 = \"\"").count(), 1, "the Windows one is untouched");
        assert!(pinned.contains("# first"));
        assert!(Catalog::parse(&pinned).is_ok());
        assert!(pin_text(&text, "nope", Os::Macos, "universal", "x", 1).is_none());
    }
}
