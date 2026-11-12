//! The Dropbox, OneDrive, Google Drive and iCloud Drive folders found on
//! this machine ([`aitrack_library::cloud::detect_folders`]), which the
//! "Add library…" picker ([`super::libraries`]) offers as one-click
//! choices. Detection and the root list are read on a
//! background thread (a provider folder that is offline may be slow to
//! answer). Online-only files in them are indexed by name and downloaded
//! when they are played or dragged ([`crate::audition::CloudFetches`]).

use aitrack_library::Library;
use aitrack_library::cloud::{CloudFolder, Provider, detect_folders};
use crossbeam_channel::{Receiver, unbounded};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct CloudRow {
    pub folder: CloudFolder,
    /// Already indexed (it, or a folder holding it, is a root).
    pub added: bool,
}

#[derive(Default)]
pub struct CloudPanel {
    pub rows: Vec<CloudRow>,
    rx: Option<Receiver<Vec<CloudRow>>>,
    loaded: bool,
}

/// Whether `folder` is already indexed by one of `roots`.
/// Compares paths only (no disk access: it runs while drawing).
pub fn covered(folder: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| folder.starts_with(r))
}

/// Made-up folders for `AITRACK_DEV_VIEW=cloud` screenshots.
pub(super) fn dev_rows() -> Vec<CloudRow> {
    let row = |provider, label: &str, path: &str, added| CloudRow { folder: CloudFolder { provider, label: label.into(), path: PathBuf::from(path) }, added };
    vec![
        row(Provider::ICloudDrive, "iCloud Drive", "~/Library/Mobile Documents/com~apple~CloudDocs", false),
        row(Provider::Dropbox, "Dropbox", "~/Library/CloudStorage/Dropbox", true),
        row(Provider::GoogleDrive, "Google Drive (producer@example.com)", "~/Library/CloudStorage/GoogleDrive-producer@example.com/My Drive", false),
    ]
}

pub fn dev_view() -> bool {
    std::env::var("AITRACK_DEV_VIEW").is_ok_and(|v| v.split([',', ' ']).any(|w| w == "cloud"))
}

impl CloudPanel {
    /// Detect once (first time the palette opens), off the UI thread.
    pub fn load(&mut self, user_db: Option<PathBuf>) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if dev_view() {
            self.rows = dev_rows();
            return;
        }
        self.reload(user_db);
    }

    /// Detect again (after a folder was added, the roots changed).
    pub fn reload(&mut self, user_db: Option<PathBuf>) {
        let (tx, rx) = unbounded();
        let run = move || {
            let roots: Vec<PathBuf> = user_db
                .filter(|p| p.is_file())
                .and_then(|p| Library::open_read_only(&p).and_then(|l| l.roots()).ok())
                .map(|r| r.into_iter().map(|r| r.path).collect())
                .unwrap_or_default();
            let rows = detect_folders().into_iter().map(|f| CloudRow { added: covered(&f.path, &roots), folder: f }).collect();
            let _ = tx.send(rows);
        };
        if std::thread::Builder::new().name("palette-cloud-detect".into()).spawn(run).is_ok() {
            self.rx = Some(rx);
        }
    }

    pub fn poll(&mut self) -> bool {
        let Some(rows) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) else { return false };
        self.rx = None;
        self.rows = rows;
        true
    }

    pub fn mark_added(&mut self, path: &Path) {
        for r in self.rows.iter_mut().filter(|r| r.folder.path == path) {
            r.added = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covered_by_a_root_or_its_parent() {
        let roots = vec![PathBuf::from("/Users/a/Library/CloudStorage/Dropbox")];
        assert!(covered(Path::new("/Users/a/Library/CloudStorage/Dropbox"), &roots));
        assert!(covered(Path::new("/Users/a/Library/CloudStorage/Dropbox/Samples"), &roots));
        assert!(!covered(Path::new("/Users/a/Library/CloudStorage/OneDrive-Personal"), &roots));
        assert!(!covered(Path::new("/Users/a/Library/CloudStorage/Drop"), &[]));
    }

    #[test]
    fn dev_rows_leave_a_provider_missing() {
        // The screenshot shows a provider that is not installed, greyed.
        let rows = dev_rows();
        assert!(Provider::ALL.iter().any(|p| !rows.iter().any(|r| r.folder.provider == *p)));
    }
}
