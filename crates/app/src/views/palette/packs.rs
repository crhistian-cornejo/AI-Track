//! Content Hub packs in the palette: the bundled ones, the optional ones
//! the app installs itself (download → unpack → index into the user
//! library) and the link-only ones the user gets from the official page.

use aitrack_library::hub::{HubKind, Manifest};
use aitrack_library::install::{self, InstallEvent, InstallJob, Stage};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum PackKind {
    /// Part of the factory bank.
    Included { samples: u64 },
    /// Installed by the app on request.
    Optional { size_mb: f64 },
    /// Downloaded by the user from the official page.
    Link { kind: &'static str, url: String, requires_account: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackEntry {
    pub id: String,
    pub name: String,
    pub author: String,
    pub license: String,
    pub redistributable: bool,
    pub homepage: String,
    pub description: String,
    pub tags: Vec<String>,
    pub kind: PackKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InstallState {
    NotInstalled,
    Running { stage: Stage, fraction: Option<f32> },
    Installed,
    Failed(String),
}

/// Where the app installs optional packs.
pub fn install_root() -> PathBuf {
    crate::appearance::config_dir().join("packs")
}

pub fn entries(m: &Manifest) -> Vec<PackEntry> {
    let mut out = Vec::new();
    for p in &m.packs {
        out.push(PackEntry {
            id: p.id.clone(),
            name: p.name.clone(),
            author: p.author.clone(),
            license: p.license.clone(),
            redistributable: true,
            homepage: p.homepage.clone(),
            description: p.notes.clone(),
            tags: p.genres.iter().chain(&p.kinds).map(|t| t.to_lowercase()).collect(),
            kind: if p.optional && install::can_install(p) { PackKind::Optional { size_mb: p.size_mb } } else { PackKind::Included { samples: p.sample_count } },
        });
    }
    for h in &m.hub {
        let kind = match h.kind {
            HubKind::SamplePack => "Sample pack",
            HubKind::Instrument => "Instrument",
            HubKind::Plugin => "Plugin",
        };
        out.push(PackEntry {
            id: h.id.clone(),
            name: h.name.clone(),
            author: h.author.clone(),
            license: h.license.clone(),
            redistributable: h.redistributable,
            homepage: h.homepage.clone(),
            description: if h.description.is_empty() { h.license_summary.clone() } else { h.description.clone() },
            tags: h.genres.iter().chain(&h.kinds).map(|t| t.to_lowercase()).collect(),
            kind: PackKind::Link { kind, url: if h.download_page.is_empty() { h.homepage.clone() } else { h.download_page.clone() }, requires_account: h.requires_account },
        });
    }
    // What the app can fetch first, then the official-page links, then what ships.
    out.sort_by_key(|e| match e.kind {
        PackKind::Optional { .. } => 0,
        PackKind::Link { .. } => 1,
        PackKind::Included { .. } => 2,
    });
    out
}

pub struct Packs {
    manifest: Manifest,
    pub entries: Vec<PackEntry>,
    installed: HashSet<String>,
    jobs: Vec<InstallJob>,
    progress: HashMap<String, (Stage, u64, u64)>,
    errors: HashMap<String, String>,
}

impl Default for Packs {
    fn default() -> Self {
        let manifest = Manifest::bundled();
        let root = install_root();
        let installed = manifest.packs.iter().filter(|p| install::is_installed(&root, p)).map(|p| p.id.clone()).collect();
        Self { entries: entries(&manifest), manifest, installed, jobs: Vec::new(), progress: HashMap::new(), errors: HashMap::new() }
    }
}

impl Packs {
    pub fn state(&self, id: &str) -> InstallState {
        if let Some((stage, done, total)) = self.progress.get(id) {
            let fraction = (*total > 0).then(|| (*done as f32 / *total as f32).clamp(0., 1.));
            return InstallState::Running { stage: *stage, fraction };
        }
        if self.jobs.iter().any(|j| j.pack_id == id) {
            return InstallState::Running { stage: Stage::Downloading, fraction: None };
        }
        if self.installed.contains(id) {
            return InstallState::Installed;
        }
        self.errors.get(id).map_or(InstallState::NotInstalled, |e| InstallState::Failed(e.clone()))
    }

    pub fn install(&mut self, id: &str) {
        if self.jobs.iter().any(|j| j.pack_id == id) {
            return;
        }
        if let Some(p) = self.manifest.pack(id) {
            self.errors.remove(id);
            self.jobs.push(InstallJob::spawn(p.clone(), install_root()));
        }
    }

    pub fn cancel(&mut self, id: &str) {
        if let Some(j) = self.jobs.iter().find(|j| j.pack_id == id) {
            j.cancel();
        }
    }

    pub fn running(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Take install progress. Returns `(changed, folders to index, messages)`.
    pub fn poll(&mut self) -> (bool, Vec<PathBuf>, Vec<String>) {
        let mut changed = false;
        let mut index = Vec::new();
        let mut messages = Vec::new();
        let mut finished = Vec::new();
        for job in &self.jobs {
            for ev in job.poll() {
                changed = true;
                let id = job.pack_id.clone();
                match ev {
                    InstallEvent::Progress { stage, done, total } => {
                        self.progress.insert(id, (stage, done, total));
                    }
                    InstallEvent::Done(dir) => {
                        self.progress.remove(&id);
                        self.installed.insert(id.clone());
                        messages.push(format!("Installed {}; adding it to the library", self.name(&id)));
                        index.push(dir);
                        finished.push(id);
                    }
                    InstallEvent::Failed(e) => {
                        self.progress.remove(&id);
                        messages.push(format!("{}: {e}", self.name(&id)));
                        self.errors.insert(id.clone(), e);
                        finished.push(id);
                    }
                    InstallEvent::Cancelled => {
                        self.progress.remove(&id);
                        finished.push(id);
                    }
                }
            }
        }
        self.jobs.retain(|j| !finished.contains(&j.pack_id));
        (changed, index, messages)
    }

    fn name(&self, id: &str) -> String {
        self.entries.iter().find(|e| e.id == id).map_or(id.to_owned(), |e| e.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_manifest_gives_every_kind() {
        let e = entries(&Manifest::bundled());
        assert!(e.iter().any(|p| matches!(p.kind, PackKind::Included { .. })));
        assert!(e.iter().any(|p| matches!(p.kind, PackKind::Optional { .. })));
        assert!(e.iter().any(|p| matches!(&p.kind, PackKind::Link { url, .. } if url.starts_with("http"))));
    }
}
