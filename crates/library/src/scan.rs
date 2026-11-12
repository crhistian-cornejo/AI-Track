//! Incremental scanning of library roots.
//!
//! A file is re-analysed only when its size or mtime changed, when it was
//! analysed by an older analyzer version, or when `force` is set. Files that
//! disappeared are marked `missing` (kept for relinking by hash) rather than
//! deleted.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crossbeam_channel::{Receiver, unbounded};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::analyze::Analyzer;
use crate::db::{Library, PackInfo, Root, RootId, SampleId, now, rel_string};
use crate::error::{LibraryError, Result};
use crate::parse::{NameHints, parse_path};

pub const AUDIO_EXTS: &[&str] = &["wav", "wave", "aif", "aiff", "aifc", "flac", "ogg", "oga", "mp3"];
pub const PRESET_EXTS: &[&str] = &["sfz", "dspreset"];

const MB: u64 = 1024 * 1024;

/// Fast content fingerprint: blake3 over the first and last MiB plus the size.
pub fn quick_hash(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; MB.min(size) as usize];
    f.read_exact(&mut buf)?;
    h.update(&buf);
    if size > MB {
        let tail_start = (size - MB).max(MB);
        f.seek(SeekFrom::Start(tail_start))?;
        buf.resize((size - tail_start) as usize, 0);
        f.read_exact(&mut buf)?;
        h.update(&buf);
    }
    h.update(&size.to_le_bytes());
    Ok(*h.finalize().as_bytes())
}

fn ext_of(p: &Path) -> Option<String> {
    p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
}

/// Audio file by extension (macOS `._` resource forks excluded).
pub fn is_audio(p: &Path) -> bool {
    ext_of(p).is_some_and(|e| AUDIO_EXTS.contains(&e.as_str())) && !is_fork(p)
}

pub fn is_preset(p: &Path) -> bool {
    ext_of(p).is_some_and(|e| PRESET_EXTS.contains(&e.as_str())) && !is_fork(p)
}

fn is_fork(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("._"))
}

/// Hidden folders (ours: `.aitrack-organize`) and `__MACOSX`, checked on a
/// path relative to its root so a hidden parent of the root does not matter.
pub(crate) fn is_junk(rel: &Path) -> bool {
    rel.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s.starts_with("._") || s == "__MACOSX" || (s.starts_with('.') && s.len() > 1 && s != "..")
    })
}

fn mtime_of(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScanOptions {
    /// Re-analyse everything, even unchanged files.
    pub force: bool,
    /// Limit to these roots (default: all).
    pub roots: Option<Vec<RootId>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanProgress {
    pub root: Option<RootId>,
    pub done: u64,
    pub total: u64,
    pub current: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanStats {
    pub seen: u64,
    pub added: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub missing: u64,
    pub errors: u64,
    pub presets: u64,
    pub cancelled: bool,
}

/// The pack a file belongs to: its first folder under the root, or the
/// root's own folder name for loose files.
pub(crate) fn pack_id_for(root: &Path, rel: &str) -> Option<String> {
    let mut parts = rel.split('/');
    let first = parts.next()?;
    if parts.next().is_some() {
        Some(first.to_string())
    } else {
        root.file_name().and_then(|n| n.to_str()).map(str::to_string)
    }
}

struct PackCache<'a> {
    lib: &'a Library,
    map: HashMap<String, Option<PackInfo>>,
}

impl PackCache<'_> {
    fn get(&mut self, id: &str) -> Result<Option<&PackInfo>> {
        if !self.map.contains_key(id) {
            let p = self.lib.pack(id)?;
            self.map.insert(id.to_string(), p);
        }
        Ok(self.map.get(id).and_then(Option::as_ref))
    }
}

/// Applies pack defaults (category, genres) to name hints.
fn merge_pack(mut hints: NameHints, pack: Option<&PackInfo>) -> NameHints {
    if let Some(p) = pack {
        if hints.category.is_none()
            && let Some(c) = p.default_category
        {
            hints.category = Some(c);
            hints.subcategory = p
                .default_subcategory
                .as_deref()
                .and_then(|s| c.subcategories().iter().copied().find(|x| x.eq_ignore_ascii_case(s)));
        }
        for g in &p.genres {
            if let Some(known) = crate::parse::known_genres().into_iter().find(|k| k.eq_ignore_ascii_case(g))
                && !hints.genres.contains(&known)
            {
                hints.genres.push(known);
            }
        }
    }
    hints
}

impl Library {
    /// Scans roots synchronously. `progress` is called for every file;
    /// setting `cancel` stops at the next file (work done so far is kept).
    pub fn scan(
        &self,
        analyzer: &dyn Analyzer,
        opts: &ScanOptions,
        progress: &mut dyn FnMut(&ScanProgress),
        cancel: &AtomicBool,
    ) -> Result<ScanStats> {
        let mut stats = ScanStats::default();
        let roots: Vec<Root> = self
            .roots()?
            .into_iter()
            .filter(|r| opts.roots.as_ref().is_none_or(|ids| ids.contains(&r.id)))
            .collect();
        for root in roots {
            self.scan_root(&root, analyzer, opts, progress, cancel, &mut stats)?;
            if stats.cancelled {
                break;
            }
        }
        Ok(stats)
    }

    fn scan_root(
        &self,
        root: &Root,
        analyzer: &dyn Analyzer,
        opts: &ScanOptions,
        progress: &mut dyn FnMut(&ScanProgress),
        cancel: &AtomicBool,
        stats: &mut ScanStats,
    ) -> Result<()> {
        if !root.path.is_dir() {
            log::warn!("library root missing: {}", root.path.display());
            return Ok(());
        }
        // Existing rows: rel → (size, mtime, version, status).
        let mut existing: HashMap<String, (u64, i64, u32, String)> = HashMap::new();
        {
            let mut st = self.conn.prepare(
                "SELECT rel_path, size, mtime, analysis_version, status, COALESCE(analyzer, '')
                 FROM samples WHERE root_id = ?1",
            )?;
            let rows = st.query_map([root.id.0], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)? as u32,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?;
            for row in rows {
                let (rel, size, mtime, ver, status, an) = row?;
                // A row analysed by a different analyzer counts as stale.
                let ver = if an == analyzer.id() || status == "error" { ver } else { 0 };
                existing.insert(rel, (size, mtime, ver, status));
            }
        }

        let mut audio_files = Vec::new();
        let mut preset_files = Vec::new();
        for entry in WalkDir::new(&root.path).follow_links(false).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path();
            let rel_p = p.strip_prefix(&root.path).unwrap_or(p);
            if is_junk(rel_p) {
                continue;
            }
            if is_audio(rel_p) {
                audio_files.push(entry.into_path());
            } else if is_preset(rel_p) {
                preset_files.push(entry.into_path());
            }
        }
        audio_files.sort();

        let total = audio_files.len() as u64;
        let mut seen: HashSet<String> = HashSet::with_capacity(audio_files.len());
        let mut packs = PackCache { lib: self, map: HashMap::new() };
        self.conn.execute_batch("BEGIN")?;
        let mut in_tx = 0usize;
        for (i, path) in audio_files.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                stats.cancelled = true;
                break;
            }
            progress(&ScanProgress {
                root: Some(root.id),
                done: i as u64,
                total,
                current: Some(path.clone()),
            });
            let rel = rel_string(path.strip_prefix(&root.path).unwrap_or(path));
            seen.insert(rel.clone());
            stats.seen += 1;
            let Ok(md) = std::fs::metadata(path) else { continue };
            let size = md.len();
            let mtime = mtime_of(&md);
            let prev = existing.get(&rel);
            if let Some((psize, pmtime, pver, status)) = prev
                && !opts.force
                && *psize == size
                && *pmtime == mtime
                && (status == "error" || *pver >= analyzer.version())
            {
                if status == "missing" {
                    self.conn.execute(
                        "UPDATE samples SET status='ok' WHERE root_id=?1 AND rel_path=?2",
                        params![root.id.0, rel],
                    )?;
                    let id: i64 = self.conn.query_row(
                        "SELECT id FROM samples WHERE root_id=?1 AND rel_path=?2",
                        params![root.id.0, rel],
                        |r| r.get(0),
                    )?;
                    self.refresh_fts(SampleId(id))?;
                }
                stats.unchanged += 1;
                continue;
            }
            match self.analyze_one(root, &rel, path, size, mtime, analyzer, &mut packs) {
                Ok(_) => {
                    if prev.is_some() {
                        stats.updated += 1;
                    } else {
                        stats.added += 1;
                    }
                }
                Err(e) => {
                    log::debug!("analysis failed for {}: {e}", path.display());
                    self.upsert_error(root.id, &rel, size, mtime, &e.to_string())?;
                    stats.errors += 1;
                }
            }
            in_tx += 1;
            if in_tx >= 200 {
                self.conn.execute_batch("COMMIT; BEGIN")?;
                in_tx = 0;
            }
        }

        if !stats.cancelled {
            for (rel, (.., status)) in &existing {
                if !seen.contains(rel) && status != "missing" {
                    self.conn.execute(
                        "UPDATE samples SET status='missing' WHERE root_id=?1 AND rel_path=?2",
                        params![root.id.0, rel],
                    )?;
                    self.conn.execute(
                        "DELETE FROM samples_fts WHERE rowid=(SELECT id FROM samples WHERE root_id=?1 AND rel_path=?2)",
                        params![root.id.0, rel],
                    )?;
                    stats.missing += 1;
                }
            }
            stats.presets += self.index_presets(root, &preset_files, &mut packs)?;
            self.conn
                .execute("UPDATE roots SET last_scan=?2 WHERE id=?1", params![root.id.0, now()])?;
        }
        self.conn.execute_batch("COMMIT")?;
        progress(&ScanProgress { root: Some(root.id), done: total, total, current: None });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_one(
        &self,
        root: &Root,
        rel: &str,
        path: &Path,
        size: u64,
        mtime: i64,
        analyzer: &dyn Analyzer,
        packs: &mut PackCache<'_>,
    ) -> Result<SampleId> {
        let pack_id = pack_id_for(&root.path, rel);
        let pack = match &pack_id {
            Some(id) => packs.get(id)?.cloned(),
            None => None,
        };
        let hints = merge_pack(parse_path(path, Some(&root.path)), pack.as_ref());
        let a = analyzer.analyze(path, &hints)?;
        let hash = quick_hash(path).ok();
        self.upsert_analysis(
            root.id,
            rel,
            size,
            mtime,
            hash.as_ref(),
            &a,
            pack_id.as_deref(),
            pack.as_ref().and_then(|p| p.license.as_deref()),
            analyzer.id(),
            analyzer.version(),
        )
    }

    fn index_presets(&self, root: &Root, files: &[PathBuf], packs: &mut PackCache<'_>) -> Result<u64> {
        let mut seen = HashSet::new();
        for path in files {
            let rel = rel_string(path.strip_prefix(&root.path).unwrap_or(path));
            let pack_id = pack_id_for(&root.path, &rel);
            let pack = match &pack_id {
                Some(id) => packs.get(id)?.cloned(),
                None => None,
            };
            let hints = merge_pack(parse_path(path, Some(&root.path)), pack.as_ref());
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            let mtime = std::fs::metadata(path).map(|m| mtime_of(&m)).unwrap_or(0);
            self.conn.execute(
                "INSERT INTO presets(root_id, rel_path, name, format, pack, category, subcategory, mtime)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(root_id, rel_path) DO UPDATE SET name=excluded.name, pack=excluded.pack,
                   category=excluded.category, subcategory=excluded.subcategory, mtime=excluded.mtime,
                   status='ok'",
                params![
                    root.id.0,
                    rel,
                    name,
                    ext_of(path).unwrap_or_default(),
                    pack_id,
                    hints.category.map(|c| c.as_str()),
                    hints.subcategory,
                    mtime
                ],
            )?;
            seen.insert(rel);
        }
        let mut st = self.conn.prepare("SELECT rel_path FROM presets WHERE root_id=?1")?;
        let all: Vec<String> = st.query_map([root.id.0], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for rel in all {
            if !seen.contains(&rel) {
                self.conn.execute(
                    "UPDATE presets SET status='missing' WHERE root_id=?1 AND rel_path=?2",
                    params![root.id.0, rel],
                )?;
            }
        }
        Ok(seen.len() as u64)
    }

    /// (Re)indexes a single file inside a known root. Used by the watcher.
    pub fn index_file(&self, analyzer: &dyn Analyzer, path: &Path) -> Result<Option<SampleId>> {
        if !is_audio(path) {
            return Ok(None);
        }
        let Some(root) = self.root_for_path(path)? else { return Ok(None) };
        let rel_p = path.strip_prefix(&root.path).unwrap_or(path);
        if is_junk(rel_p) {
            return Ok(None);
        }
        let md = std::fs::metadata(path)?;
        let rel = rel_string(rel_p);
        let mut packs = PackCache { lib: self, map: HashMap::new() };
        match self.analyze_one(&root, &rel, path, md.len(), mtime_of(&md), analyzer, &mut packs) {
            Ok(id) => Ok(Some(id)),
            Err(e) => {
                self.upsert_error(root.id, &rel, md.len(), mtime_of(&md), &e.to_string())?;
                Ok(None)
            }
        }
    }

    /// Marks a removed file, or everything under a removed folder, as missing.
    pub fn forget_path(&self, path: &Path) -> Result<u64> {
        let Some(root) = self.root_for_path(path)? else { return Ok(0) };
        let rel = rel_string(path.strip_prefix(&root.path).unwrap_or(path));
        let like = format!("{}/%", rel.replace('%', "\\%").replace('_', "\\_"));
        self.conn.execute(
            "DELETE FROM samples_fts WHERE rowid IN (SELECT id FROM samples WHERE root_id=?1
               AND (rel_path=?2 OR rel_path LIKE ?3 ESCAPE '\\'))",
            params![root.id.0, rel, like],
        )?;
        let n = self.conn.execute(
            "UPDATE samples SET status='missing' WHERE root_id=?1 AND status != 'missing'
               AND (rel_path=?2 OR rel_path LIKE ?3 ESCAPE '\\')",
            params![root.id.0, rel, like],
        )?;
        Ok(n as u64)
    }
}

/// Messages from a background scan.
#[derive(Clone, Debug)]
pub enum ScanEvent {
    Progress(ScanProgress),
    Finished(ScanStats),
    Failed(String),
}

/// A scan running on its own thread with its own SQLite connection.
pub struct ScanJob {
    pub events: Receiver<ScanEvent>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<ScanStats>>>,
}

impl ScanJob {
    pub fn spawn(db_path: PathBuf, analyzer: Arc<dyn Analyzer>, opts: ScanOptions) -> Self {
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let handle = std::thread::Builder::new()
            .name("aitrack-library-scan".into())
            .spawn(move || {
                let run = || -> Result<ScanStats> {
                    let lib = Library::open(&db_path)?;
                    let mut last = Instant::now() - Duration::from_secs(1);
                    let mut on_progress = |p: &ScanProgress| {
                        // Throttle UI updates; always send the final one.
                        if p.current.is_none() || last.elapsed() >= Duration::from_millis(50) {
                            last = Instant::now();
                            let _ = tx.send(ScanEvent::Progress(p.clone()));
                        }
                    };
                    lib.scan(analyzer.as_ref(), &opts, &mut on_progress, &c)
                };
                let res = run();
                match &res {
                    Ok(s) => {
                        let _ = tx.send(ScanEvent::Finished(s.clone()));
                    }
                    Err(e) => {
                        let _ = tx.send(ScanEvent::Failed(e.to_string()));
                    }
                }
                res
            })
            .expect("spawn scan thread");
        Self { events: rx, cancel, handle: Some(handle) }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    pub fn join(mut self) -> Result<ScanStats> {
        match self.handle.take() {
            Some(h) => h.join().map_err(|_| LibraryError::Other("scan thread panicked".into()))?,
            None => Err(LibraryError::Other("already joined".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::BasicAnalyzer;
    use crate::db::RootKind;
    use crate::taxonomy::Category;
    use crate::test_util::write_wav;

    fn no_progress() -> impl FnMut(&ScanProgress) {
        |_| {}
    }

    #[test]
    fn scan_and_rescan_incremental() {
        // Roots are stored canonical (macOS: /var is /private/var), so build the paths from there.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("Samples");
        let pack = root.join("Trap Pack");
        std::fs::create_dir_all(pack.join("Kicks")).unwrap();
        write_wav(&pack.join("Kicks/Hard 01.wav"), 44_100, 1, 0.2, 0.8);
        write_wav(&pack.join("808 F#.wav"), 44_100, 1, 1.0, 0.8);
        write_wav(&pack.join("Melody Loop Am 120bpm.wav"), 44_100, 2, 4.0, 0.5);
        std::fs::write(pack.join("broken.wav"), b"not a wav").unwrap();
        std::fs::write(pack.join("readme.txt"), b"hi").unwrap();
        std::fs::write(pack.join("Piano.sfz"), b"<region> sample=a.wav").unwrap();

        let lib = Library::open(dir.path().join("lib.db")).unwrap();
        lib.add_root(&root, RootKind::User).unwrap();
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let s = lib
            .scan(&BasicAnalyzer, &ScanOptions::default(), &mut |_| calls += 1, &cancel)
            .unwrap();
        assert_eq!((s.added, s.errors, s.presets), (3, 1, 1), "{s:?}");
        assert!(calls >= 4);
        assert_eq!(lib.count().unwrap(), 3);

        let kick = lib.find_by_path(&pack.join("Kicks/Hard 01.wav")).unwrap().unwrap();
        assert_eq!(kick.subcategory.as_deref(), Some("Kicks"));
        assert_eq!(kick.pack.as_deref(), Some("Trap Pack"));
        assert!(kick.genres.contains(&"Trap".to_string()));
        assert_eq!(kick.hash.as_ref().map(String::len), Some(64));
        let lp = lib.find_by_path(&pack.join("Melody Loop Am 120bpm.wav")).unwrap().unwrap();
        assert!(lp.is_loop());
        assert_eq!(lp.camelot.as_deref(), Some("8A"));
        assert_eq!(lp.bpm, Some(120.0));

        // Nothing changed: everything is skipped.
        let s = lib.scan(&BasicAnalyzer, &ScanOptions::default(), &mut no_progress(), &cancel).unwrap();
        assert_eq!((s.added, s.updated, s.unchanged), (0, 0, 4), "{s:?}");

        // A user correction survives a forced rescan.
        lib.set_category(kick.id, Some(Category::Drums), Some("Percs")).unwrap();
        lib.set_favorite(kick.id, true).unwrap();

        // Change one file (size differs), delete another, add a new one.
        write_wav(&pack.join("808 F#.wav"), 44_100, 1, 1.5, 0.8);
        std::fs::remove_file(pack.join("Melody Loop Am 120bpm.wav")).unwrap();
        write_wav(&pack.join("Clap 2.wav"), 44_100, 1, 0.2, 0.8);
        let s = lib.scan(&BasicAnalyzer, &ScanOptions::default(), &mut no_progress(), &cancel).unwrap();
        assert_eq!((s.added, s.updated, s.missing), (1, 1, 1), "{s:?}");
        assert_eq!(lib.count().unwrap(), 3);

        let s = lib
            .scan(&BasicAnalyzer, &ScanOptions { force: true, ..Default::default() }, &mut no_progress(), &cancel)
            .unwrap();
        assert_eq!((s.updated, s.errors), (3, 1), "{s:?}");
        let kick = lib.get(kick.id).unwrap();
        assert_eq!(kick.subcategory.as_deref(), Some("Percs"));
        assert!(kick.favorite);

        // The file comes back: it is ok again without re-analysis.
        write_wav(&pack.join("Melody Loop Am 120bpm.wav"), 44_100, 2, 4.0, 0.5);
        lib.scan(&BasicAnalyzer, &ScanOptions::default(), &mut no_progress(), &cancel).unwrap();
        assert_eq!(lib.count().unwrap(), 4);
    }

    #[test]
    fn background_job_and_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..30 {
            write_wav(&root.join(format!("Snare {i}.wav")), 22_050, 1, 0.1, 0.5);
        }
        let db = dir.path().join("lib.db");
        {
            let lib = Library::open(&db).unwrap();
            lib.add_root(&root, RootKind::User).unwrap();
        }
        let job = ScanJob::spawn(db.clone(), Arc::new(BasicAnalyzer), ScanOptions::default());
        let mut finished = None;
        for ev in job.events.iter() {
            if let ScanEvent::Finished(s) = ev {
                finished = Some(s);
                break;
            }
        }
        assert_eq!(finished.unwrap().added, 30);
        job.join().unwrap();

        // Cancelling before the first file stops immediately.
        let job = ScanJob::spawn(db.clone(), Arc::new(BasicAnalyzer), ScanOptions { force: true, roots: None });
        job.cancel();
        let s = job.join().unwrap();
        assert!(s.cancelled || s.updated <= 30);
    }

    #[test]
    fn quick_hash_large_and_small() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        std::fs::write(&a, vec![1u8; 3 * MB as usize]).unwrap();
        let b = dir.path().join("b.bin");
        let mut data = vec![1u8; 3 * MB as usize];
        data[3 * MB as usize - 1] = 2;
        std::fs::write(&b, data).unwrap();
        assert_ne!(quick_hash(&a).unwrap(), quick_hash(&b).unwrap());
        let c = dir.path().join("c.bin");
        std::fs::write(&c, b"tiny").unwrap();
        assert_eq!(quick_hash(&c).unwrap(), quick_hash(&c).unwrap());
    }

    #[test]
    fn pack_ids() {
        assert_eq!(pack_id_for(Path::new("/x/Samples"), "Pack A/k.wav").as_deref(), Some("Pack A"));
        assert_eq!(pack_id_for(Path::new("/x/Samples"), "k.wav").as_deref(), Some("Samples"));
    }
}
