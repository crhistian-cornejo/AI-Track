//! Keeps the index in sync with the disk using `notify`.
//!
//! Events are debounced (files being copied fire many events) and applied on
//! a worker thread with its own connection: new/changed audio is analysed,
//! removed files or folders are marked missing.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::analyze::Analyzer;
use crate::db::{Library, SampleId};
use crate::error::Result;
use crate::scan::is_audio;

/// What the watcher did, for the UI to refresh.
#[derive(Clone, Debug, PartialEq)]
pub enum WatchEvent {
    Indexed(Vec<SampleId>),
    Removed(u64),
    Error(String),
}

pub struct LibraryWatcher {
    pub events: Receiver<WatchEvent>,
    _watcher: RecommendedWatcher,
    stop: Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl LibraryWatcher {
    /// Watches every root of the library at `db_path` that has `watch` set.
    pub fn start(db_path: PathBuf, analyzer: Arc<dyn Analyzer>, debounce: Duration) -> Result<Self> {
        let roots = Library::open(&db_path)?.roots()?;
        let (raw_tx, raw_rx) = unbounded::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(move |ev| {
            let _ = raw_tx.send(ev);
        })?;
        for r in roots.iter().filter(|r| r.watch && r.path.is_dir()) {
            watcher.watch(&r.path, RecursiveMode::Recursive)?;
        }
        let (out_tx, out_rx) = unbounded();
        let (stop_tx, stop_rx) = unbounded::<()>();
        let handle = std::thread::Builder::new()
            .name("aitrack-library-watch".into())
            .spawn(move || {
                let lib = match Library::open(&db_path) {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = out_tx.send(WatchEvent::Error(e.to_string()));
                        return;
                    }
                };
                let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
                let mut last = Instant::now();
                loop {
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }
                    match raw_rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(Ok(ev)) => {
                            if !matches!(ev.kind, EventKind::Access(_)) {
                                pending.extend(ev.paths);
                                last = Instant::now();
                            }
                        }
                        Ok(Err(e)) => {
                            let _ = out_tx.send(WatchEvent::Error(e.to_string()));
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    if pending.is_empty() || last.elapsed() < debounce {
                        continue;
                    }
                    let mut indexed = Vec::new();
                    let mut removed = 0;
                    for p in std::mem::take(&mut pending) {
                        if p.is_file() {
                            if is_audio(&p) {
                                match lib.index_file(analyzer.as_ref(), &p) {
                                    Ok(Some(id)) => indexed.push(id),
                                    Ok(None) => {}
                                    Err(e) => {
                                        let _ = out_tx.send(WatchEvent::Error(e.to_string()));
                                    }
                                }
                            }
                        } else if !p.exists() {
                            removed += lib.forget_path(&p).unwrap_or(0);
                        }
                    }
                    if !indexed.is_empty() {
                        let _ = out_tx.send(WatchEvent::Indexed(indexed));
                    }
                    if removed > 0 {
                        let _ = out_tx.send(WatchEvent::Removed(removed));
                    }
                }
            })
            .expect("spawn watch thread");
        Ok(Self { events: out_rx, _watcher: watcher, stop: stop_tx, handle: Some(handle) })
    }

    pub fn stop(mut self) {
        let _ = self.stop.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for LibraryWatcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::BasicAnalyzer;
    use crate::db::RootKind;
    use crate::test_util::write_wav;

    #[test]
    fn picks_up_new_and_removed_files() {
        // macOS: the temp dir lives under /var, a symlink to /private/var. The
        // watcher reports canonical paths, so the stored root must be canonical too.
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().canonicalize().unwrap();
        let root = dir_path.join("watched");
        std::fs::create_dir_all(&root).unwrap();
        let db = dir_path.join("lib.db");
        Library::open(&db).unwrap().add_root(&root, RootKind::User).unwrap();
        let w = LibraryWatcher::start(db.clone(), Arc::new(BasicAnalyzer), Duration::from_millis(200)).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let f = root.join("Clap 7.wav");
        write_wav(&f, 22_050, 1, 0.2, 0.5);

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut got = false;
        while Instant::now() < deadline && !got {
            if let Ok(WatchEvent::Indexed(ids)) = w.events.recv_timeout(Duration::from_millis(200)) {
                got = !ids.is_empty();
            }
        }
        assert!(got, "watcher indexed the new file");
        let lib = Library::open(&db).unwrap();
        assert_eq!(lib.count().unwrap(), 1);

        std::fs::remove_file(&f).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && lib.count().unwrap() != 0 {
            let _ = w.events.recv_timeout(Duration::from_millis(200));
        }
        assert_eq!(lib.count().unwrap(), 0);
        w.stop();
    }
}
