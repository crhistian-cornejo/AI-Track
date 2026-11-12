//! [`LibraryBridge`] over `aitrack-library`.
//!
//! Two indexes: the factory sound bank (`factory.db`, roots relative to its
//! folder, found through [`find_factory_db`]) and the user's library in the
//! app data folder, whose folders are scanned by a background
//! [`ScanJob`] with [`AiAnalyzer`] (`aitrack-ai` key/BPM/category on top of
//! the filename hints). The palette searches both.

use crate::bridge::*;
use aitrack_library::{
    AiDetector,
    Analysis, Analyzer, BasicAnalyzer, Category, KeyFilter, KeyHint, KeyMatch, Library, NameHints, OrganizeLayout,
    OrganizeMode, OrganizePlan, ReanalyseEvent, ReanalyseJob, ReanalyseOptions, RecommendQuery, RootKind, Sample, SampleId, ScanEvent, ScanJob, ScanOptions, SearchQuery, Source,
};
use aitrack_model::Scale;
use crossbeam_channel::{Receiver, unbounded};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FACTORY: usize = 0;
const USER: usize = 1;

/// The factory sound bank: the setting, then `AITRACK_CONTENT_DIR`, then
/// `<exe dir>/content`, then the repository's `content/` folder. Each may
/// point at the content folder or straight at the sound bank folder.
pub fn find_factory_db(setting: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(setting.map(Path::to_path_buf));
    candidates.extend(std::env::var_os("AITRACK_CONTENT_DIR").map(PathBuf::from));
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join("content"));
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content"));
    candidates.into_iter().flat_map(|c| [c.join("factory.db"), c.join("soundbank").join("factory.db")]).find(|p| p.is_file())
}

/// Key, BPM and category from `aitrack-ai` where the file name says nothing.
pub struct AiAnalyzer;

fn category_of(c: aitrack_ai::SampleCategory) -> Option<Category> {
    use aitrack_ai::SampleCategory as S;
    Some(match c {
        S::Kick | S::Snare | S::Clap | S::HiHat | S::Perc | S::Cymbal | S::Tom | S::DrumLoop | S::PercLoop => Category::Drums,
        S::Bass808 | S::Bass | S::BassLoop => Category::Bass,
        S::Synth | S::Pad | S::Lead | S::Pluck | S::MelodicLoop => Category::Synths,
        S::Keys => Category::Instruments,
        S::Vocal | S::VocalLoop => Category::Vocals,
        S::Fx | S::Riser | S::Impact | S::FxLoop => Category::Fx,
        S::Unknown => return None,
    })
}

impl Analyzer for AiAnalyzer {
    fn id(&self) -> &str {
        "aitrack-ai"
    }

    fn version(&self) -> u32 {
        1
    }

    fn analyze(&self, path: &Path, hints: &NameHints) -> aitrack_library::Result<Analysis> {
        let mut a = BasicAnalyzer.analyze(path, hints)?;
        // A failed decode keeps the filename analysis.
        if let Ok(s) = aitrack_ai::analyze(path) {
            if a.key.is_none()
                && let Some((scale, conf)) = s.key
                && conf >= 0.5
            {
                a.key = Some(KeyHint { root: scale.root, kind: Some(scale.kind) });
                a.key_source = Source::Audio;
            }
            if a.bpm.is_none()
                && s.is_loop
                && let Some((bpm, conf)) = s.bpm
                && conf >= 0.4
            {
                a.bpm = Some(bpm);
                a.bpm_source = Source::Audio;
            }
            if a.category.is_none()
                && let Some(c) = category_of(s.category)
            {
                a.category = Some(c);
                a.category_source = Source::Audio;
            }
        }
        Ok(a)
    }
}

fn item(lib: usize, s: &Sample) -> LibraryItem {
    let category = match (&s.category, &s.subcategory) {
        (Some(c), Some(sub)) => format!("{} · {sub}", c.as_str()),
        (Some(c), None) => c.as_str().to_owned(),
        _ => if s.is_loop() { "Loop" } else { "One-shot" }.to_owned(),
    };
    LibraryItem {
        id: ((lib as u64) << 48) | s.id.0 as u64,
        name: s.name.clone(),
        path: s.path.clone(),
        category,
        key: s.scale(),
        bpm: s.bpm,
        duration: s.duration.unwrap_or(0.) as f32,
        is_loop: s.is_loop(),
        favorite: s.favorite,
    }
}

fn split_id(id: u64) -> (usize, SampleId) {
    ((id >> 48) as usize, SampleId((id & ((1 << 48) - 1)) as i64))
}

enum Scan {
    /// Indexing the user's folders.
    Folders(ScanJob),
    /// Listening to samples without a key (factory bank read-only, results
    /// cached in the user library).
    Keys(ReanalyseJob),
}

impl Scan {
    fn cancel(&self) {
        match self {
            Scan::Folders(j) => j.cancel(),
            Scan::Keys(j) => j.cancel(),
        }
    }
}

pub struct RealLibrary {
    /// `[factory, user]`.
    libs: [Option<Library>; 2],
    user_path: PathBuf,
    factory_path: Option<PathBuf>,
    search_cache: RefCell<Option<(String, Vec<LibraryItem>)>>,
    scan: Option<Scan>,
    status: ScanStatus,
    analyzer: Arc<dyn Analyzer>,
    plan: Option<OrganizePlan>,
    organize_rx: Option<Receiver<Result<(PathBuf, String), String>>>,
    organize_done: Option<Result<String, String>>,
    journal: Option<PathBuf>,
    errors: Vec<String>,
}

impl RealLibrary {
    pub fn new(content_setting: Option<&Path>) -> Self {
        Self::open(find_factory_db(content_setting), crate::appearance::config_dir().join("library.db"))
    }

    pub fn open(factory: Option<PathBuf>, user: PathBuf) -> Self {
        let mut errors = Vec::new();
        let mut open = |p: &Path, what: &str| match Library::open(p) {
            Ok(l) => Some(l),
            Err(e) => {
                errors.push(format!("Could not open the {what} library: {e}"));
                None
            }
        };
        let f = factory.as_deref().and_then(|p| open(p, "factory"));
        let u = open(&user, "user");
        let mut this = Self {
            libs: [f, u],
            user_path: user,
            factory_path: factory,
            search_cache: RefCell::default(),
            scan: None,
            status: ScanStatus::default(),
            analyzer: Arc::new(AiAnalyzer),
            plan: None,
            organize_rx: None,
            organize_done: None,
            journal: None,
            errors,
        };
        this.refresh();
        this
    }

    /// The index changed: drop the cached results.
    fn refresh(&mut self) {
        *self.search_cache.borrow_mut() = None;
    }

    fn query(&self, f: impl Fn(&Library) -> aitrack_library::Result<Vec<Sample>>) -> Vec<LibraryItem> {
        let mut out = Vec::new();
        for (i, lib) in self.libs.iter().enumerate() {
            if let Some(lib) = lib
                && let Ok(mut samples) = f(lib)
            {
                self.fill_keys(&mut samples);
                out.extend(samples.iter().map(|s| item(i, s)));
            }
        }
        out
    }

    /// Keys and tempos found by listening (cached in the user library) for
    /// rows that have none, e.g. in the read-only factory bank.
    fn fill_keys(&self, samples: &mut [Sample]) {
        if let Some(user) = &self.libs[USER] {
            let _ = user.fill_from_cache(samples);
        }
    }

    fn lib(&self, id: u64) -> Option<(&Library, SampleId)> {
        let (i, s) = split_id(id);
        Some((self.libs.get(i)?.as_ref()?, s))
    }

    fn count(&self, i: usize) -> u64 {
        self.libs[i].as_ref().and_then(|l| l.count().ok()).unwrap_or(0)
    }

    fn start_scan(&mut self, opts: ScanOptions) {
        if self.scan.is_some() {
            return;
        }
        let job = ScanJob::spawn(self.user_path.clone(), self.analyzer.clone(), opts);
        self.scan = Some(Scan::Folders(job));
        self.status = ScanStatus { running: true, fraction: 0., message: "Scanning your folders…".into() };
    }
}

impl LibraryBridge for RealLibrary {
    fn search(&self, query: &str, key: Option<Scale>) -> Vec<LibraryItem> {
        let cache_key = format!("{query}\u{1}{key:?}");
        if let Some((k, v)) = &*self.search_cache.borrow()
            && *k == cache_key
        {
            return v.clone();
        }
        let mut q = SearchQuery { limit: 500, ..Default::default() };
        if !query.trim().is_empty() {
            q.text = Some(query.trim().to_owned());
        }
        q.filters.key = key.map(|scale| KeyFilter { scale, mode: KeyMatch::Compatible, include_atonal: true });
        let v = self.query(|l| l.search(&q));
        *self.search_cache.borrow_mut() = Some((cache_key, v.clone()));
        v
    }

    fn recommend(&self, scale: Option<Scale>, bpm: f64) -> Vec<LibraryItem> {
        let cache_key = format!("\u{2}{scale:?}{bpm:.2}");
        if let Some((k, v)) = &*self.search_cache.borrow()
            && *k == cache_key
        {
            return v.clone();
        }
        let q = RecommendQuery { scale, bpm: Some(bpm as f32), limit: 200, ..Default::default() };
        let mut ranked: Vec<(f32, LibraryItem)> = Vec::new();
        for (i, lib) in self.libs.iter().enumerate() {
            if let Some(lib) = lib
                && let Ok(recs) = lib.recommend(&q)
            {
                let mut samples: Vec<Sample> = recs.iter().map(|r| r.sample.clone()).collect();
                self.fill_keys(&mut samples);
                ranked.extend(recs.iter().zip(&samples).map(|(r, s)| (r.score, item(i, s))));
            }
        }
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
        let v: Vec<LibraryItem> = ranked.into_iter().map(|(_, it)| it).collect();
        *self.search_cache.borrow_mut() = Some((cache_key, v.clone()));
        v
    }

    fn status(&self) -> String {
        if let Some(e) = self.errors.first() {
            return e.clone();
        }
        let user = self.count(USER);
        match &self.libs[FACTORY] {
            Some(_) => format!("Factory bank: {} samples · yours: {user}", self.count(FACTORY)),
            None => format!("Factory sound bank not found (set AITRACK_CONTENT_DIR) · yours: {user}"),
        }
    }

    fn detect_keys(&mut self) -> Result<(), String> {
        if self.scan.is_some() {
            return Err("A scan is already running".into());
        }
        if self.libs[USER].is_none() {
            return Err("The user library is not available".into());
        }
        // The factory bank stays untouched: what is found goes to the user
        // library's cache, which listings and searches read back.
        let job = ReanalyseJob::spawn_library(self.factory_path.clone(), self.user_path.clone(), Arc::new(AiDetector), ReanalyseOptions::default());
        self.scan = Some(Scan::Keys(job));
        self.status = ScanStatus { running: true, fraction: 0., message: "Detecting keys…".into() };
        Ok(())
    }

    fn add_folder(&mut self, path: &Path) -> Result<(), String> {
        let lib = self.libs[USER].as_ref().ok_or("The user library is not available")?;
        let root = lib.add_root(path, RootKind::User).map_err(|e| e.to_string())?;
        if self.scan.is_some() {
            return Ok(()); // picked up by the next rescan
        }
        self.start_scan(ScanOptions { roots: Some(vec![root]), ..Default::default() });
        Ok(())
    }

    fn rescan(&mut self) {
        self.start_scan(ScanOptions::default());
    }

    fn cancel_scan(&mut self) {
        if let Some(s) = &self.scan {
            s.cancel();
            self.status.message = "Cancelling…".into();
        }
    }

    fn scan_status(&self) -> ScanStatus {
        self.status.clone()
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.organize_rx
            && let Ok(r) = rx.try_recv()
        {
            self.organize_rx = None;
            self.organize_done = Some(match r {
                Ok((journal, summary)) => {
                    self.journal = Some(journal);
                    Ok(summary)
                }
                Err(e) => Err(e),
            });
            self.refresh();
            changed = true;
        }
        let Some(scan) = &self.scan else { return changed };
        let mut finished = false;
        match scan {
            Scan::Folders(job) => {
                while let Ok(ev) = job.events.try_recv() {
                    changed = true;
                    match ev {
                        ScanEvent::Progress(p) => {
                            let name = p.current.as_ref().and_then(|c| c.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            self.status = ScanStatus { running: true, fraction: p.done as f32 / p.total.max(1) as f32, message: format!("Scanning {}/{} {name}", p.done, p.total) };
                        }
                        ScanEvent::Finished(s) => {
                            let what = if s.cancelled { "Scan cancelled" } else { "Scan finished" };
                            self.status = ScanStatus { running: false, fraction: 1., message: format!("{what}: {} added, {} updated, {} errors", s.added, s.updated, s.errors) };
                            finished = true;
                        }
                        ScanEvent::Failed(e) => {
                            self.status = ScanStatus { running: false, fraction: 0., message: format!("Scan failed: {e}") };
                            finished = true;
                        }
                    }
                }
            }
            Scan::Keys(job) => {
                while let Ok(ev) = job.events.try_recv() {
                    changed = true;
                    match ev {
                        ReanalyseEvent::Progress(p) => {
                            let what = if p.source == 0 && self.factory_path.is_some() { "factory sounds" } else { "your samples" };
                            self.status = ScanStatus { running: true, fraction: p.done as f32 / p.total.max(1) as f32, message: format!("Detecting keys of {what} {}/{}", p.done, p.total) };
                        }
                        ReanalyseEvent::Finished(s) => {
                            let what = if s.cancelled { "Key detection cancelled" } else { "Key detection finished" };
                            self.status = ScanStatus { running: false, fraction: 1., message: format!("{what}: {} keys and {} tempos found, {} errors", s.keys_found, s.bpms_found, s.errors) };
                            finished = true;
                        }
                        ReanalyseEvent::Failed(e) => {
                            self.status = ScanStatus { running: false, fraction: 0., message: format!("Key detection failed: {e}") };
                            finished = true;
                        }
                    }
                }
            }
        }
        if finished {
            self.scan = None;
            self.refresh();
        }
        changed
    }

    fn set_favorite(&mut self, id: u64, on: bool) {
        if let Some((lib, s)) = self.lib(id)
            && lib.set_favorite(s, on).is_ok()
        {
            self.refresh();
        }
    }

    fn mark_used(&mut self, id: u64) {
        if let Some((lib, s)) = self.lib(id) {
            let _ = lib.mark_used(s);
        }
    }

    fn plan_organize(&mut self, items: &[u64], dest: &Path, by_key: bool, move_files: bool) -> Result<OrganizePreview, String> {
        // Only the user's own samples are reorganised; the factory bank stays put.
        let ids: Vec<SampleId> = items.iter().map(|i| split_id(*i)).filter(|(l, _)| *l == USER).map(|(_, s)| s).collect();
        if ids.is_empty() {
            return Err("Organize works on your own samples; pick a folder that holds some (factory samples stay where they are)".into());
        }
        let lib = self.libs[USER].as_ref().ok_or("The user library is not available")?;
        let layout = if by_key { OrganizeLayout::ByKey } else { OrganizeLayout::ByCategory };
        let mode = if move_files { OrganizeMode::Move } else { OrganizeMode::Copy };
        let plan = lib.plan_organize(&ids, dest, layout, mode).map_err(|e| e.to_string())?;
        let mut folders: Vec<(String, usize)> = plan.preview().into_iter().collect();
        folders.sort_by_key(|f| std::cmp::Reverse(f.1));
        folders.truncate(8);
        let preview = OrganizePreview { dest: dest.to_path_buf(), files: plan.ops.len(), bytes: plan.total_bytes, duplicates: plan.duplicates(), moves: move_files, folders };
        self.plan = Some(plan);
        Ok(preview)
    }

    fn execute_organize(&mut self) -> Result<(), String> {
        let plan = self.plan.take().ok_or("Nothing planned")?;
        let db = self.user_path.clone();
        let (tx, rx) = unbounded();
        std::thread::Builder::new()
            .name("aitrack-organize".into())
            .spawn(move || {
                let r = Library::open(&db).and_then(|lib| plan.execute(&lib)).map(|j| (j.path, format!("Organized {} files", j.operations))).map_err(|e| e.to_string());
                let _ = tx.send(r);
            })
            .map_err(|e| e.to_string())?;
        self.organize_rx = Some(rx);
        Ok(())
    }

    fn organize_result(&mut self) -> Option<Result<String, String>> {
        self.organize_done.take()
    }

    fn can_undo_organize(&self) -> bool {
        self.journal.is_some()
    }

    fn undo_organize(&mut self) -> Result<String, String> {
        let journal = self.journal.take().ok_or("Nothing to undo")?;
        let lib = self.libs[USER].as_ref().ok_or("The user library is not available")?;
        let s = aitrack_library::organize::undo_organize(lib, &journal).map_err(|e| e.to_string())?;
        self.refresh();
        Ok(format!("Undone: {} copies removed, {} files moved back", s.removed_copies, s.moved_back))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bank() -> Option<PathBuf> {
        find_factory_db(Some(Path::new(r"D:\dev\AI-Track\content\soundbank")))
    }

    #[test]
    fn ids_round_trip() {
        let id = (1u64 << 48) | 1234;
        assert_eq!(split_id(id), (1, SampleId(1234)));
    }

    /// Needs the downloaded sound bank (main checkout); skipped elsewhere.
    #[test]
    fn factory_bank_has_search_and_recommendations() {
        let Some(db) = bank() else {
            eprintln!("factory sound bank not present; skipped");
            return;
        };
        let user = std::env::temp_dir().join(format!("aitrack-test-user-{}.db", std::process::id()));
        let lib = RealLibrary::open(Some(db), user.clone());
        assert!(lib.libs[FACTORY].is_some());
        let kicks = lib.search("kick", None);
        assert!(!kicks.is_empty(), "no kicks");
        assert!(kicks[0].path.is_file(), "factory roots resolve next to the db: {}", kicks[0].path.display());
        let am = Scale::new(aitrack_model::Key(9), aitrack_model::ScaleKind::Minor);
        assert!(!lib.recommend(Some(am), 128.).is_empty());
        drop(lib);
        let _ = std::fs::remove_file(user);
    }

    #[test]
    fn user_folder_scan_indexes_and_organize_plans() {
        let dir = std::env::temp_dir().join(format!("aitrack-test-lib-{}", std::process::id()));
        let samples = dir.join("Kicks");
        std::fs::create_dir_all(&samples).unwrap();
        let spec = hound::WavSpec { channels: 1, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(samples.join("Kick Punchy 01.wav"), spec).unwrap();
        for i in 0..8_000 {
            let env = (-(i as f32) / 1500.).exp();
            w.write_sample(((i as f32 * 0.01).sin() * env * 20_000.) as i16).unwrap();
        }
        w.finalize().unwrap();
        let mut lib = RealLibrary::open(None, dir.join("user.db"));
        lib.add_folder(&dir).unwrap();
        let start = std::time::Instant::now();
        while lib.scan.is_some() && start.elapsed() < std::time::Duration::from_secs(60) {
            lib.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let found = lib.search("kick", None);
        assert_eq!(found.len(), 1, "{:?}", lib.scan_status());
        let ids: Vec<u64> = found.iter().map(|i| i.id).collect();
        let preview = lib.plan_organize(&ids, &dir.join("Organized"), false, false).unwrap();
        assert_eq!(preview.files, 1);
        drop(lib);
        let _ = std::fs::remove_dir_all(dir);
    }
}
