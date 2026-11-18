//! [`LibraryBridge`] over `aitrack-library`.
//!
//! Two indexes: the factory sound bank (`factory.db`, roots relative to its
//! folder, found through [`find_factory_db`]) and the user's library in the
//! app data folder, whose folders are scanned by a background
//! [`ScanJob`] with [`AiAnalyzer`] (`aitrack-ai` key/BPM/category on top of
//! the filename hints). The palette searches both.
//!
//! A folder may be a cloud folder (Dropbox, OneDrive, Google Drive, iCloud
//! Drive): the scan indexes its online-only files by name without reading
//! them (see [`aitrack_library::cloud`]); the palette downloads one when it
//! is played and analyses it then.

use crate::bridge::*;
use aitrack_library::{
    AiDetector, DspEmbedder, EmbedEvent, EmbedJob, EmbedOptions, SimilarFinder,
    Analysis, Analyzer, BasicAnalyzer, Category, KeyFilter, KeyHint, KeyMatch, Library, NameHints, OrganizeLayout,
    OrganizeMode, OrganizePlan, ReanalyseEvent, ReanalyseJob, ReanalyseOptions, RecommendQuery, RootKind, Sample, SampleId, ScanEvent, ScanJob, ScanOptions, SearchQuery, Source,
};
use aitrack_library::watch::{LibraryWatcher, WatchEvent};
use aitrack_model::Scale;
use std::collections::HashMap;
use crossbeam_channel::{Receiver, unbounded};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FACTORY: usize = 0;
const USER: usize = 1;

/// The factory sound bank: the setting, then `AITRACK_CONTENT_DIR`, then
/// `<exe dir>/content` and, inside a macOS bundle, `Contents/Resources/content`,
/// then (debug builds only) the repository's `content/` folder. Each may point at
/// the content folder or straight at the sound bank folder.
pub fn find_factory_db(setting: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(setting.map(Path::to_path_buf));
    candidates.extend(std::env::var_os("AITRACK_CONTENT_DIR").map(PathBuf::from));
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join("content"));
        // `AI-Track.app/Contents/MacOS/aitrack` → `AI-Track.app/Contents/Resources/content`.
        candidates.push(dir.join("../Resources/content"));
    }
    // The source tree only exists on the developer's machine.
    if cfg!(debug_assertions) {
        candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content"));
    }
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
    /// Listening to every sound to learn which ones sound alike (quiet, and
    /// stopped as soon as the user asks for anything else).
    Embed(EmbedJob),
}

impl Scan {
    fn cancel(&self) {
        match self {
            Scan::Folders(j) => j.cancel(),
            Scan::Keys(j) => j.cancel(),
            Scan::Embed(j) => j.cancel(),
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
    /// The Splice download folder to index as a collection, and the watcher
    /// that picks up sounds added to the folders later.
    splice_folder: Option<PathBuf>,
    watcher: Option<LibraryWatcher>,
    /// Kind of every root, by (library, id), to tell where a sound comes from.
    root_kinds: RefCell<HashMap<(usize, i64), RootKind>>,
    folders_cache: RefCell<Option<(u64, Vec<SoundFolder>)>>,
    /// The free-plugin installer (`None` where there is no standard plugin folder) and the cards last built from it.
    plugins: Option<aitrack_library::plugins::PluginInstaller>,
    plugin_cards: Vec<aitrack_library::plugins::PluginCard>,
    plugin_cards_at: Option<std::time::Instant>,
    /// The index of "sounds like this one", kept between questions.
    similar: RefCell<SimilarFinder>,
    /// The sounds should be listened to again (at start-up, after a scan or a key detection).
    embed_wanted: bool,
}

/// Where the plugin installer keeps its receipts and work folders: next to the user library.
fn this_data_dir(user_db: &Path) -> PathBuf {
    user_db.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf)
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
        let user_for_data = user.clone();
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
            splice_folder: None,
            watcher: None,
            root_kinds: RefCell::default(),
            folders_cache: RefCell::default(),
            plugins: aitrack_library::plugins::UserDirs::standard().map(|dirs| {
                let data = this_data_dir(&user_for_data);
                aitrack_library::plugins::PluginInstaller::new(data, dirs)
            }),
            plugin_cards: Vec::new(),
            plugin_cards_at: None,
            similar: RefCell::default(),
            embed_wanted: true,
        };
        this.restart_watcher();
        this.refresh();
        this
    }

    /// Watch the user's folders (the Splice one among them) so sounds that
    /// land in them later are indexed on their own. Started again whenever a
    /// folder is added, as the watcher reads the roots when it starts.
    fn restart_watcher(&mut self) {
        self.watcher = None;
        if self.libs[USER].is_some() {
            self.watcher = LibraryWatcher::start(self.user_path.clone(), self.analyzer.clone(), std::time::Duration::from_millis(800)).ok();
        }
    }

    /// The index changed: drop the cached results.
    fn refresh(&mut self) {
        *self.search_cache.borrow_mut() = None;
        self.root_kinds.borrow_mut().clear();
    }

    /// Make the Splice folder a collection of the user library: a root of kind
    /// Splice, scanned when it is new. Any other root that was the Splice one
    /// goes back to being an ordinary folder.
    fn sync_splice_root(&mut self) {
        let Some(lib) = self.libs[USER].as_ref() else { return };
        let folder = self.splice_folder.clone().filter(|f| f.is_dir());
        let roots = lib.roots().unwrap_or_default();
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        for r in roots.iter().filter(|r| r.kind == RootKind::Splice) {
            if folder.as_deref().is_none_or(|f| canon(f) != canon(&r.path)) {
                let _ = lib.add_root(&r.path, RootKind::User);
            }
        }
        let Some(folder) = folder else { return };
        let known = roots.iter().any(|r| r.kind == RootKind::Splice && canon(&r.path) == canon(&folder));
        if known {
            return;
        }
        if let Ok(root) = lib.add_root(&folder, RootKind::Splice) {
            self.refresh();
            self.restart_watcher();
            if !self.scan_busy() {
                self.start_scan(ScanOptions { roots: Some(vec![root]), ..Default::default() });
            }
        }
    }

    /// The Splice folder exists but is not a collection of the library yet.
    fn splice_root_pending(&self) -> bool {
        let (Some(folder), Some(lib)) = (self.splice_folder.as_deref().filter(|f| f.is_dir()), self.libs[USER].as_ref()) else { return false };
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        !lib.roots().unwrap_or_default().iter().any(|r| r.kind == RootKind::Splice && canon(&r.path) == canon(folder))
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

    /// A scan, key detection or organize that must not be interrupted is running. The
    /// background listening for similar sounds gives way to everything else.
    fn scan_busy(&mut self) -> bool {
        if matches!(self.scan, Some(Scan::Embed(_))) {
            if let Some(s) = self.scan.take() {
                s.cancel();
            }
            self.embed_wanted = true;
        }
        self.scan.is_some()
    }

    /// Learn which sounds sound alike, in the background, when nothing else is running.
    fn start_embed(&mut self) {
        self.embed_wanted = false;
        if self.scan.is_some() || self.libs[USER].is_none() {
            return;
        }
        let opts = EmbedOptions { threads: std::thread::available_parallelism().map_or(2, |n| (n.get() / 4).clamp(1, 3)), limit: None };
        self.scan = Some(Scan::Embed(EmbedJob::spawn_library(self.factory_path.clone(), self.user_path.clone(), Arc::new(DspEmbedder), opts)));
    }

    fn start_scan(&mut self, opts: ScanOptions) {
        if self.scan_busy() {
            return;
        }
        let job = ScanJob::spawn(self.user_path.clone(), self.analyzer.clone(), opts);
        self.scan = Some(Scan::Folders(job));
        self.status = ScanStatus { running: true, fraction: 0., message: "Scanning your folders…".into() };
    }
}

/// What a sound search reads: both indexes and where each sound comes from.
/// Borrowed from the bridge for a search on its own thread, or owned by the
/// search thread (`sound_searcher`), which has connections of its own.
struct SoundView<'a> {
    libs: &'a [Option<Library>; 2],
    /// The folders offered as filters, with the sounds counted when they were made.
    folders: &'a RefCell<Option<(u64, Vec<SoundFolder>)>>,
    root_kinds: &'a RefCell<HashMap<(usize, i64), RootKind>>,
    splice_folder: Option<&'a Path>,
    /// A scan is running.
    scanning: bool,
}

impl SoundView<'_> {
    /// Where a sample of library `lib` in root `root` comes from.
    fn origin_of(&self, lib: usize, root: i64) -> SoundOrigin {
        if lib == FACTORY {
            return SoundOrigin::Factory;
        }
        let mut kinds = self.root_kinds.borrow_mut();
        if kinds.is_empty()
            && let Some(l) = &self.libs[lib]
            && let Ok(roots) = l.roots()
        {
            kinds.extend(roots.into_iter().map(|r| ((lib, r.id.0), r.kind)));
        }
        match kinds.get(&(lib, root)) {
            Some(RootKind::Splice) => SoundOrigin::Splice,
            Some(RootKind::Factory) => SoundOrigin::Factory,
            _ => SoundOrigin::Mine,
        }
    }

    /// Where the Splice folder stands (for the panel's status line).
    fn splice_state(&self) -> SpliceState {
        let Some(folder) = self.splice_folder.map(Path::to_path_buf) else { return SpliceState::Unknown };
        if !folder.is_dir() {
            return SpliceState::Missing(folder);
        }
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let sounds = self.libs[USER]
            .as_ref()
            .and_then(|l| l.root_stats().ok())
            .and_then(|stats| stats.into_iter().find(|s| s.root.kind == RootKind::Splice && canon(&s.root.path) == canon(&folder)))
            .map(|s| s.samples);
        match sounds {
            Some(_) if self.scanning => SpliceState::Scanning(folder),
            Some(n) => SpliceState::Indexed { folder, sounds: n },
            None if self.scanning => SpliceState::Scanning(folder),
            None => SpliceState::Unknown,
        }
    }

    /// The folders worth filtering by, across both libraries. Reading them is a
    /// pass over every path, so they are kept until the number of sounds changes.
    fn folder_list(&self) -> Vec<SoundFolder> {
        let stamp: u64 = self.libs.iter().flatten().filter_map(|l| l.count().ok()).sum();
        if let Some((s, list)) = &*self.folders.borrow()
            && *s == stamp
        {
            return list.clone();
        }
        let mut out = Vec::new();
        for (i, lib) in self.libs.iter().enumerate() {
            let Some(lib) = lib else { continue };
            for root in lib.roots().unwrap_or_default() {
                let all = lib.folders(root.id).unwrap_or_default();
                if all.is_empty() {
                    // A flat folder is a filter by itself.
                    let n = lib.count_where(&aitrack_library::Filters { root: Some(root.id), ..Default::default() }).unwrap_or(0);
                    if n > 0 {
                        let label = root.path.file_name().map_or_else(|| "Folder".to_owned(), |n| n.to_string_lossy().into_owned());
                        out.push(SoundFolder { library: i, root: root.id.0, under: None, label, count: n });
                    }
                    continue;
                }
                for (path, n) in aitrack_library::browse_folders(&all, 16) {
                    let label = path.rsplit('/').next().unwrap_or(&path).to_owned();
                    out.push(SoundFolder { library: i, root: root.id.0, under: Some(path), label, count: n });
                }
            }
        }
        out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
        out.truncate(24);
        *self.folders.borrow_mut() = Some((stamp, out.clone()));
        out
    }

    fn bpm_trust(&self, id: u64) -> BpmTrust {
        let source = self.lib(id).and_then(|(lib, sample)| {
            lib.connection().query_row("SELECT bpm_source FROM samples WHERE id = ?1", [sample.0], |r| r.get::<_, Option<String>>(0)).ok().flatten()
        });
        BpmTrust::from_source(source.as_deref())
    }

    fn fill_keys(&self, samples: &mut [Sample]) {
        if let Some(user) = &self.libs[USER] {
            let _ = user.fill_from_cache(samples);
        }
    }

    fn lib(&self, id: u64) -> Option<(&Library, SampleId)> {
        let (i, s) = split_id(id);
        Some((self.libs.get(i)?.as_ref()?, s))
    }

    fn search(&self, q: &SoundQuery) -> SoundPage {
        use aitrack_library::{fit_tempo, key_compat, query::tempo_compat};
        let mut sq = SearchQuery { limit: 1500, ..Default::default() };
        if !q.text.trim().is_empty() {
            sq.text = Some(q.text.trim().to_owned());
        }
        sq.filters.category = q.group.map(|g| match g {
            SoundGroup::Drums => Category::Drums,
            SoundGroup::Bass => Category::Bass,
            SoundGroup::Synths => Category::Synths,
            SoundGroup::Instruments => Category::Instruments,
            SoundGroup::Vocals => Category::Vocals,
            SoundGroup::Fx => Category::Fx,
        });
        sq.filters.is_loop = match q.kind {
            SoundKind::All => None,
            SoundKind::Loops => Some(true),
            SoundKind::OneShots => Some(false),
        };
        // Drums and effects have no key and fit any song.
        sq.filters.key = q.key.map(|(scale, exact)| KeyFilter { scale, mode: if exact { KeyMatch::Exact } else { KeyMatch::Compatible }, include_atonal: true });
        sq.filters.genre = q.tag.clone();
        sq.filters.favorites_only = q.favorites;
        // The tempo is asked of the database too, a little wider than the exact test below, so a
        // big library is cut down before the 1500-row cap and not after it.
        sq.filters.loop_tempo = q.bpm.map(|(bpm, tol)| aitrack_library::BpmFilter { bpm, tolerance_pct: tol * 1.2 + 0.5, half_double: true });
        let words = aitrack_library::text::words(&q.text);
        let mut hits: Vec<(f32, SoundHit)> = Vec::new();
        for (i, lib) in self.libs.iter().enumerate() {
            let Some(lib) = lib else { continue };
            let mut sq = sq.clone();
            if let Some(f) = &q.folder {
                if f.library != i {
                    continue;
                }
                sq.filters.root = Some(aitrack_library::RootId(f.root));
                sq.filters.under = f.under.clone();
            }
            let Ok(mut samples) = lib.search(&sq) else { continue };
            self.fill_keys(&mut samples);
            for smp in &samples {
                let origin = self.origin_of(i, smp.root_id.0);
                if q.origin.is_some_and(|o| o != origin) {
                    continue;
                }
                // The tempo filter is for loops only: a one-shot has none and always passes.
                let tempo = match (q.bpm, smp.bpm) {
                    (Some((bpm, tol)), Some(b)) if smp.is_loop() => {
                        let Some(f) = fit_tempo(f64::from(bpm), b) else { continue };
                        if (f.speed - 1.).abs() * 100. > f64::from(tol) {
                            continue;
                        }
                        tempo_compat(bpm, Some(b), true)
                    }
                    (Some(_), None) if smp.is_loop() => 0.3,
                    _ => 0.8,
                };
                let key_score = match q.key {
                    Some((scale, _)) => key_compat(scale, smp.key_root, smp.scale_kind),
                    None => 1.,
                };
                let tonal = !matches!(smp.category, Some(Category::Drums | Category::Fx));
                let hit = SoundHit {
                    item: item(i, smp),
                    origin,
                    tags: smp.genres.iter().chain(smp.user_tags.iter()).cloned().collect(),
                    bpm_trust: BpmTrust::from_source(None),
                    unanalysed: (tonal && smp.key_root.is_none()) || (smp.is_loop() && smp.bpm.is_none()),
                };
                // What was typed leads (a name that starts with it, then one that has it as a word,
                // then one that only contains it), the song's fit orders the rest; both libraries
                // are ranked together, not one after the other.
                let fit = if q.key.is_some() || q.bpm.is_some() { key_score * 0.6 + tempo * 0.4 } else { 0. };
                let text = if words.is_empty() { 0. } else { aitrack_library::relevance(&words, smp) };
                hits.push((text + fit, hit));
            }
        }
        if !words.is_empty() || q.key.is_some() || q.bpm.is_some() {
            hits.sort_by(|a, b| b.0.total_cmp(&a.0));
        }
        let limit = if q.limit == 0 { 400 } else { q.limit };
        let more = hits.len() > limit;
        hits.truncate(limit);
        let mut tags: HashMap<String, usize> = HashMap::new();
        for (_, h) in &hits {
            for t in &h.tags {
                *tags.entry(t.clone()).or_default() += 1;
            }
        }
        let mut tags: Vec<(String, usize)> = tags.into_iter().collect();
        tags.sort_by_key(|(t, n)| (std::cmp::Reverse(*n), t.clone()));
        tags.truncate(12);
        // A reliable tempo for every hit that is shown (one query each: a few hundred at most).
        let hits: Vec<SoundHit> = hits.into_iter().map(|(_, mut h)| {
            if h.item.is_loop && h.item.bpm.is_some() {
                h.bpm_trust = self.bpm_trust(h.item.id);
            }
            h
        }).collect();
        SoundPage { hits, more, tags, folders: self.folder_list(), splice: self.splice_state() }
    }
}

impl RealLibrary {
    fn view(&self) -> SoundView<'_> {
        SoundView { libs: &self.libs, folders: &self.folders_cache, root_kinds: &self.root_kinds, splice_folder: self.splice_folder.as_deref(), scanning: self.status.running }
    }
}

impl LibraryBridge for RealLibrary {
    fn bpm_trust(&self, id: u64) -> BpmTrust {
        self.view().bpm_trust(id)
    }

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

    fn search_sounds(&self, q: &SoundQuery) -> SoundPage {
        self.view().search(q)
    }

    fn similar_sounds(&self, id: u64, limit: usize) -> Vec<SimilarSound> {
        let (lib, sample) = split_id(id);
        let libs = [self.libs[FACTORY].as_ref(), self.libs[USER].as_ref()];
        match self.similar.borrow_mut().find(libs, lib as u8, sample, &DspEmbedder, limit) {
            Ok(hits) => hits.iter().map(|h| SimilarSound { item: item(h.lib as usize, &h.sample), similarity: h.similarity }).collect(),
            Err(e) => {
                log::warn!("similar sounds: {e}");
                Vec::new()
            }
        }
    }

    fn sound_searcher(&self) -> Option<SoundSearchFn> {
        let (factory, user) = (self.factory_path.clone(), self.user_path.clone());
        let mut libs: Option<[Option<Library>; 2]> = None;
        let folders = RefCell::default();
        Some(Box::new(move |q, env| {
            // Opened on first use, on the search thread: connections are not shared with the UI's.
            let libs = libs.get_or_insert_with(|| [factory.as_deref().and_then(|p| Library::open(p).ok()), None]);
            if libs[USER].is_none() {
                libs[USER] = Library::open(&user).ok();
            }
            let kinds = RefCell::default();
            SoundView { libs, folders: &folders, root_kinds: &kinds, splice_folder: env.splice_folder.as_deref(), scanning: env.scanning }.search(q)
        }))
    }

    fn plugin_cards(&mut self) -> Vec<aitrack_library::plugins::PluginCard> {
        let Some(inst) = &self.plugins else { return Vec::new() };
        let every = if inst.busy() { std::time::Duration::from_millis(200) } else { std::time::Duration::from_secs(2) };
        if self.plugin_cards_at.is_none_or(|t| t.elapsed() > every) {
            self.plugin_cards = inst.cards();
            self.plugin_cards_at = Some(std::time::Instant::now());
        }
        self.plugin_cards.clone()
    }

    fn install_plugin(&mut self, id: &str) -> Result<(), String> {
        let inst = self.plugins.as_mut().ok_or("Plugins are installed on macOS and Windows")?;
        inst.start(id).map_err(|e| e.to_string().trim_start_matches("manifest: ").to_owned())?;
        self.plugin_cards_at = None;
        Ok(())
    }

    fn cancel_plugin_install(&mut self, id: &str) {
        if let Some(inst) = &mut self.plugins {
            inst.cancel(id);
        }
    }

    fn uninstall_plugin(&mut self, id: &str) -> Result<String, String> {
        let inst = self.plugins.as_mut().ok_or("Plugins are installed on macOS and Windows")?;
        let n = inst.uninstall(id).map_err(|e| e.to_string().trim_start_matches("manifest: ").to_owned())?;
        self.plugin_cards_at = None;
        Ok(format!("Removed {n} plugin file{}", if n == 1 { "" } else { "s" }))
    }

    fn take_installed_plugins(&mut self) -> Vec<PathBuf> {
        self.plugins.as_mut().map(|p| p.take_installed()).unwrap_or_default()
    }

    fn set_splice_folder(&mut self, folder: Option<PathBuf>) {
        // The same folder is looked at again when it was not there before and is now (Splice
        // was installed while the app ran) and has not been made a collection yet.
        if folder == self.splice_folder && !self.splice_root_pending() {
            return;
        }
        self.splice_folder = folder;
        self.sync_splice_root();
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
        let online = self.libs[USER].as_ref().and_then(|l| l.count_cloud().ok()).unwrap_or(0);
        let online = if online > 0 { format!(" ({online} online only)") } else { String::new() };
        match &self.libs[FACTORY] {
            Some(_) => format!("Factory bank: {} samples · yours: {user}{online}", self.count(FACTORY)),
            None => format!("Factory sound bank not found (set AITRACK_CONTENT_DIR) · yours: {user}{online}"),
        }
    }

    fn detect_keys(&mut self) -> Result<(), String> {
        if self.scan_busy() {
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
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let kind = if self.splice_folder.as_deref().is_some_and(|f| canon(f) == canon(path)) { RootKind::Splice } else { RootKind::User };
        let root = lib.add_root(path, kind).map_err(|e| e.to_string())?;
        self.refresh();
        self.restart_watcher();
        if self.scan_busy() {
            return Ok(()); // picked up by the next rescan
        }
        self.start_scan(ScanOptions { roots: Some(vec![root]), ..Default::default() });
        Ok(())
    }

    fn rescan(&mut self) {
        self.start_scan(ScanOptions::default());
    }

    fn cancel_scan(&mut self) {
        if let Some(s) = &self.scan
            && !matches!(s, Scan::Embed(_))
        {
            s.cancel();
            self.status.message = "Cancelling…".into();
        }
    }

    fn scan_status(&self) -> ScanStatus {
        self.status.clone()
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(p) = &mut self.plugins
            && p.poll()
        {
            changed = true;
            // The last install just ended: show its new state now, not in two seconds.
            if !p.busy() {
                self.plugin_cards_at = None;
            }
        }
        // Sounds that landed in a watched folder (the Splice one, say) are indexed now.
        if let Some(w) = &self.watcher {
            while let Ok(ev) = w.events.try_recv() {
                if matches!(ev, WatchEvent::Indexed(_) | WatchEvent::Removed(_)) {
                    changed = true;
                }
            }
        }
        if changed {
            self.refresh();
        }
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
        if self.scan.is_none() && self.embed_wanted && self.organize_rx.is_none() {
            self.start_embed();
        }
        let Some(scan) = &self.scan else { return changed };
        let mut finished = false;
        let after_listening = matches!(scan, Scan::Embed(_));
        match scan {
            Scan::Embed(job) => {
                while let Ok(ev) = job.events.try_recv() {
                    match ev {
                        EmbedEvent::Progress(_) => {}
                        EmbedEvent::Finished(s) => {
                            log::info!("library: listened to {} sounds for similarity ({} already known, {} errors)", s.embedded, s.cached, s.errors);
                            finished = true;
                        }
                        EmbedEvent::Failed(e) => {
                            log::warn!("library: similar-sound analysis failed: {e}");
                            finished = true;
                        }
                    }
                }
            }
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
                            let online = if s.cloud > 0 { format!(", {} online only (they download when played)", s.cloud) } else { String::new() };
                            self.status = ScanStatus { running: false, fraction: 1., message: format!("{what}: {} added, {} updated, {} errors{online}", s.added, s.updated, s.errors) };
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
            // New sounds in the library, or new keys: they may need listening to.
            self.embed_wanted = !after_listening;
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

    fn write_wav(path: &Path, seconds: f32) {
        let spec = hound::WavSpec { channels: 1, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..(44_100. * seconds) as usize {
            let env = (-(i as f32) / 6000.).exp();
            w.write_sample(((i as f32 * 0.02).sin() * env * 16_000.) as i16).unwrap();
        }
        w.finalize().unwrap();
    }

    fn wait(lib: &mut RealLibrary, what: &str, mut done: impl FnMut(&mut RealLibrary) -> bool) {
        let start = std::time::Instant::now();
        while !done(lib) {
            assert!(start.elapsed() < std::time::Duration::from_secs(40), "timed out waiting for {what}");
            lib.poll();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn similar_sounds_come_from_the_background_listening_and_stay_in_the_kind_of_sound() {
        let dir = std::env::temp_dir().join(format!("aitrack-test-similar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_wav(&dir.join("Kick Alpha.wav"), 0.3);
        write_wav(&dir.join("Kick Beta.wav"), 0.35);
        write_wav(&dir.join("Pad Gamma.wav"), 0.4);
        let mut lib = RealLibrary::open(None, dir.join("user.db"));
        lib.add_folder(&dir).unwrap();
        // The scan, then the listening that follows it.
        let start = std::time::Instant::now();
        // `embed_wanted` is raised when the scan ends and lowered when the listening starts.
        while start.elapsed() < std::time::Duration::from_secs(60) {
            lib.poll();
            if lib.scan.is_none() && !lib.embed_wanted {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let alpha = lib.search("alpha", None).remove(0);
        let similar = lib.similar_sounds(alpha.id, 10);
        assert_eq!(similar.iter().map(|s| s.item.name.as_str()).collect::<Vec<_>>(), vec!["Kick Beta"], "only kicks, and not itself");
        assert!(similar[0].similarity > 0.5, "{}", similar[0].similarity);
        // Asking for a sound that is not there answers nothing.
        assert!(lib.similar_sounds(u64::MAX >> 20, 5).is_empty());
        drop(lib);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_splice_folder_that_appears_later_is_picked_up_and_folders_filter_the_results() {
        let dir = std::env::temp_dir().join(format!("aitrack-test-splice-late-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lib = RealLibrary::open(None, dir.join("user.db"));
        let splice = dir.join("Splice");
        let query = SoundQuery { text: "pad".into(), ..Default::default() };
        // Splice is not installed yet.
        lib.set_splice_folder(Some(splice.clone()));
        assert_eq!(lib.search_sounds(&query).splice, SpliceState::Missing(splice.clone()));
        // It is installed and a sound licensed while the app runs; the same folder is looked at again.
        let (a, b) = (splice.join("packs").join("Deep House"), splice.join("packs").join("Trap"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        write_wav(&a.join("Dark Pad Eléctrico.wav"), 0.3);
        write_wav(&b.join("Pad Soft.wav"), 0.3);
        lib.set_splice_folder(Some(splice.clone()));
        wait(&mut lib, "the new folder to be indexed", |l| !l.scan_status().running && l.search_sounds(&query).hits.len() == 2);
        let page = lib.search_sounds(&query);
        // Ranked by what was typed: the name that starts with it before the one that contains it.
        assert_eq!(page.hits[0].item.name, "Pad Soft");
        // Parts of words and accents.
        assert_eq!(lib.search_sounds(&SoundQuery { text: "ELECTRICO".into(), ..Default::default() }).hits.len(), 1);
        // The folders offered skip `packs/`, which splits nothing, and filter.
        let labels: Vec<&str> = page.folders.iter().map(|f| f.label.as_str()).collect();
        assert!(labels.contains(&"Deep House") && labels.contains(&"Trap") && !labels.contains(&"packs"), "{labels:?}");
        let deep = page.folders.iter().find(|f| f.label == "Deep House").unwrap().clone();
        let inside = lib.search_sounds(&SoundQuery { folder: Some(deep), ..Default::default() });
        assert_eq!(inside.hits.iter().map(|h| h.item.name.as_str()).collect::<Vec<_>>(), vec!["Dark Pad Eléctrico"]);
        drop(lib);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_splice_folder_is_a_collection_and_sounds_added_later_show_up_on_their_own() {
        let dir = std::env::temp_dir().join(format!("aitrack-test-splice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let folder = dir.join("Splice").join("sounds").join("Some Pack");
        std::fs::create_dir_all(&folder).unwrap();
        write_wav(&folder.join("Kick Heavy 01.wav"), 0.3);
        let mut lib = RealLibrary::open(None, dir.join("user.db"));
        let query = SoundQuery { text: "kick".into(), ..Default::default() };
        assert!(lib.search_sounds(&query).hits.is_empty());
        // A folder that is not there says so instead of failing.
        lib.set_splice_folder(Some(dir.join("nowhere")));
        assert_eq!(lib.search_sounds(&query).splice, SpliceState::Missing(dir.join("nowhere")));
        // The real one becomes a collection of its own, scanned at once.
        let splice = dir.join("Splice").join("sounds");
        lib.set_splice_folder(Some(splice.clone()));
        wait(&mut lib, "the first scan", |l| !l.scan_status().running && !l.search_sounds(&query).hits.is_empty());
        let page = lib.search_sounds(&query);
        assert_eq!(page.hits.len(), 1, "{:?}", lib.scan_status());
        assert_eq!(page.hits[0].origin, SoundOrigin::Splice, "badged with where it comes from");
        assert!(matches!(page.splice, SpliceState::Indexed { sounds: 1, .. }), "{:?}", page.splice);
        assert_eq!(lib.search_sounds(&SoundQuery { origin: Some(SoundOrigin::Factory), ..query.clone() }).hits.len(), 0, "a collection filter");
        assert_eq!(lib.search_sounds(&SoundQuery { origin: Some(SoundOrigin::Splice), ..query.clone() }).hits.len(), 1);
        // The search thread's connections of its own answer the same, from another thread.
        let mut search = lib.sound_searcher().expect("the real library searches off the UI thread");
        let (q, env) = (query.clone(), SoundEnv { splice_folder: Some(splice.clone()), scanning: false });
        let threaded = std::thread::spawn(move || search(&q, &env)).join().unwrap();
        assert_eq!(threaded, page);
        // A download lands in the folder later: it appears without asking.
        write_wav(&folder.join("Kick Sub 02.wav"), 0.3);
        wait(&mut lib, "the watcher", |l| l.search_sounds(&query).hits.len() == 2);
        assert!(lib.search_sounds(&query).hits.iter().all(|h| h.origin == SoundOrigin::Splice));
        // Choosing another folder gives the first one back as an ordinary one.
        let other = dir.join("Other");
        std::fs::create_dir_all(&other).unwrap();
        lib.set_splice_folder(Some(other));
        wait(&mut lib, "the folders to settle", |l| !l.scan_status().running);
        assert!(lib.search_sounds(&query).hits.iter().all(|h| h.origin == SoundOrigin::Mine), "{:?}", lib.search_sounds(&query).hits.iter().map(|h| h.origin).collect::<Vec<_>>());
        drop(lib);
        let _ = std::fs::remove_dir_all(dir);
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
        // A tempo guessed from the length is told apart from a measured one.
        let (l, sample) = lib.lib(ids[0]).unwrap();
        assert_eq!(lib.bpm_trust(ids[0]), BpmTrust::Reliable);
        l.connection().execute("UPDATE samples SET bpm = 97, bpm_source = 'estimate' WHERE id = ?1", [sample.0]).unwrap();
        assert_eq!(lib.bpm_trust(ids[0]), BpmTrust::Estimate);
        l.connection().execute("UPDATE samples SET bpm_source = 'filename' WHERE id = ?1", [sample.0]).unwrap();
        assert_eq!(lib.bpm_trust(ids[0]), BpmTrust::Reliable);
        assert_eq!(lib.bpm_trust(u64::MAX), BpmTrust::Reliable, "unknown ids are not blocked");
        let preview = lib.plan_organize(&ids, &dir.join("Organized"), false, false).unwrap();
        assert_eq!(preview.files, 1);
        drop(lib);
        let _ = std::fs::remove_dir_all(dir);
    }
}
