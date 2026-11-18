//! "Find similar sounds": an embedding per sample and nearest-neighbour search.
//!
//! An [`Embedder`] turns a file into a fixed-length vector. The one that
//! ships is [`crate::ai::DspEmbedder`] (feature `ai`: MFCC, spectral shape
//! and envelope features, no model). A learned embedder (CLAP through ONNX)
//! implements the same trait with its own id and version and sits beside the
//! DSP one in the same table; a new version of an embedder makes
//! [`Library::embed_missing`] listen to the files again.
//!
//! Vectors live in `sample_embeddings`, keyed by the file's [`quick_hash`] and
//! the embedder, like the key/BPM detections of [`crate::reanalyse`]: the
//! read-only factory bank is embedded into the user library's database, two
//! copies of one file are decoded once, and a file that moves or is renamed
//! keeps its vector. The table is created on first use in the database that
//! holds the cache (never in a read-only bank), so old databases need no
//! migration step. Online-only cloud files are never read for this.
//!
//! [`SimilarIndex`] is the query side: all vectors in one contiguous matrix,
//! standardised over the collection and normalised, so a query is one dot
//! product per sample (10 000 samples in about a millisecond).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aitrack_model::{Key, Scale, ScaleKind};
use crossbeam_channel::{Receiver, unbounded};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::db::{Library, SampleId, now, parse_scale_kind};
use crate::error::{LibraryError, Result};
use crate::query::{key_compat, tempo_compat};
use crate::reanalyse::ReanalyseSource;
use crate::scan::quick_hash;
use crate::taxonomy::Category;

/// Files longer than this are not sounds to be compared (full songs).
pub const MAX_EMBED_SECONDS: f64 = 90.0;

const TABLE: &str = "
CREATE TABLE IF NOT EXISTS sample_embeddings(
  hash BLOB NOT NULL,
  embedder TEXT NOT NULL,
  version INTEGER NOT NULL,
  dim INTEGER NOT NULL DEFAULT 0,
  vec BLOB,
  error TEXT,
  embedded_at INTEGER NOT NULL,
  PRIMARY KEY(hash, embedder));";

/// Something that turns a file into a vector. Must be thread-safe: the job
/// runs it on a few worker threads.
pub trait Embedder: Send + Sync {
    /// Stable identifier stored with each vector (e.g. `"dsp-timbre"`, `"clap-htsat"`).
    fn id(&self) -> &str;
    /// Increase to make vectors of older versions stale.
    fn version(&self) -> u32;
    /// Length of every vector.
    fn dim(&self) -> usize;
    /// Whether the features differ in scale and should be standardised over the
    /// collection before comparing (hand-made features: yes; a learned
    /// embedding that is already a point on a sphere: no).
    fn standardise(&self) -> bool {
        true
    }
    fn embed(&self, path: &Path) -> Result<Vec<f32>>;
}

fn encode(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn decode(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

fn hash_from_blob(b: Option<Vec<u8>>) -> Option<[u8; 32]> {
    b.and_then(|v| v.try_into().ok())
}

// ───────────────────────────── Storing ─────────────────────────────

impl Library {
    /// Creates the vector table in this database when it is missing. Only
    /// the database that holds the cache needs it.
    pub fn ensure_embedding_table(&self) -> Result<()> {
        Ok(self.conn.execute_batch(TABLE)?)
    }

    fn has_embedding_table(&self) -> Result<bool> {
        Ok(self.conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name='sample_embeddings'", [], |_| Ok(())).optional()?.is_some())
    }

    fn has_embedding(&self, hash: &[u8; 32], embedder: &str, min_version: u32) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM sample_embeddings WHERE hash = ?1 AND embedder = ?2 AND version >= ?3",
                params![hash.as_slice(), embedder, min_version],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    fn store_embedding(&self, hash: &[u8; 32], embedder: &str, version: u32, v: Result<Vec<f32>, &str>) -> Result<()> {
        let (dim, blob, err) = match &v {
            Ok(v) => (v.len(), Some(encode(v)), None),
            Err(e) => (0, None, Some(*e)),
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO sample_embeddings(hash, embedder, version, dim, vec, error, embedded_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![hash.as_slice(), embedder, version, dim as i64, blob, err, now()],
        )?;
        Ok(())
    }

    /// The vector stored for a file hash by `embedder` at `min_version` or newer.
    pub fn embedding_for_hash(&self, hash: &[u8; 32], embedder: &str, min_version: u32) -> Result<Option<Vec<f32>>> {
        if !self.has_embedding_table()? {
            return Ok(None);
        }
        let blob: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT vec FROM sample_embeddings WHERE hash = ?1 AND embedder = ?2 AND version >= ?3",
                params![hash.as_slice(), embedder, min_version],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok(blob.map(|b| decode(&b)))
    }

    /// How many distinct files have a vector from `embedder` at `min_version` or newer.
    pub fn embedding_count(&self, embedder: &str, min_version: u32) -> Result<u64> {
        if !self.has_embedding_table()? {
            return Ok(0);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sample_embeddings WHERE embedder = ?1 AND version >= ?2 AND vec IS NOT NULL",
                params![embedder, min_version],
                |r| r.get::<_, i64>(0),
            )? as u64)
    }

    /// Rows worth embedding: readable now (online-only cloud files are never
    /// read) and no longer than [`MAX_EMBED_SECONDS`].
    fn embed_candidates(&self, limit: Option<usize>) -> Result<Vec<EmbedCandidate>> {
        let mut st = self.conn.prepare(
            "SELECT s.id, s.rel_path, r.path, s.hash FROM samples s JOIN roots r ON r.id = s.root_id
             WHERE s.status = 'ok' AND s.cloud = 0 AND COALESCE(s.duration, 1) BETWEEN 0.005 AND ?1
             ORDER BY s.id LIMIT ?2",
        )?;
        let limit = limit.map_or(-1, |l| l as i64);
        let rows = st.query_map(params![MAX_EMBED_SECONDS, limit], |r| {
            let rel: String = r.get(1)?;
            let root: String = r.get(2)?;
            Ok(EmbedCandidate { path: self.resolve_root(&root).join(rel), hash: hash_from_blob(r.get(3)?) })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Embeds this library's samples that `cache` has no current vector for.
    ///
    /// Vectors are stored in `cache` (which may be `self`) by file hash, so a
    /// read-only bank only feeds the cache. Decoding runs on
    /// [`EmbedOptions::threads`] workers, the database writes on the calling
    /// thread. `progress` is called after every file; setting `cancel` stops
    /// soon after (finished work is kept).
    pub fn embed_missing(
        &self,
        cache: &Library,
        embedder: &dyn Embedder,
        opts: &EmbedOptions,
        progress: &mut dyn FnMut(&EmbedProgress),
        cancel: &AtomicBool,
    ) -> Result<EmbedStats> {
        cache.ensure_embedding_table()?;
        let candidates = self.embed_candidates(opts.limit)?;
        let (id, version) = (embedder.id().to_string(), embedder.version());
        let mut stats = EmbedStats { candidates: candidates.len() as u64, ..Default::default() };
        let mut pr = EmbedProgress { total: candidates.len() as u64, ..Default::default() };

        // What the cache already has is skipped without opening the file.
        let mut todo: Vec<(usize, Option<[u8; 32]>)> = Vec::new();
        for (i, c) in candidates.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let hash = c.hash.or_else(|| quick_hash(&c.path).ok());
            match hash {
                Some(h) if cache.has_embedding(&h, &id, version)? => {
                    stats.cached += 1;
                    pr.cached += 1;
                    pr.done += 1;
                }
                // A file that cannot be read (moved away since the scan) is not an error to remember.
                None => {
                    pr.done += 1;
                }
                h => todo.push((i, h)),
            }
        }
        progress(&pr);

        let threads = match opts.threads {
            0 => std::thread::available_parallelism().map_or(4, |n| n.get().min(8)),
            n => n,
        }
        .clamp(1, todo.len().max(1));
        let next = AtomicUsize::new(0);
        let (tx, rx) = unbounded::<(usize, Result<Vec<f32>>)>();
        let (candidates, todo_ref) = (&candidates, &todo);
        let result = std::thread::scope(|s| -> Result<()> {
            for _ in 0..threads {
                let tx = tx.clone();
                let next = &next;
                s.spawn(move || {
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let n = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(i, _)) = todo_ref.get(n) else { break };
                        let r = embedder.embed(&candidates[i].path);
                        if tx.send((n, r)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(tx);
            // Results are written in short transactions that are opened and closed
            // between two waits on the channel: waiting on a slow decode never
            // holds the database's write lock (the UI writes to it too).
            while let Ok(first) = rx.recv() {
                let mut batch = vec![first];
                while batch.len() < 50 {
                    match rx.try_recv() {
                        Ok(x) => batch.push(x),
                        Err(_) => break,
                    }
                }
                cache.conn.execute_batch("BEGIN")?;
                let written = (|| -> Result<()> {
                    for (n, r) in batch {
                        let (i, hash) = todo_ref[n];
                        let Some(hash) = hash else { continue };
                        let r = r.and_then(|v| {
                            if v.len() == embedder.dim() { Ok(v) } else { Err(LibraryError::Audio(format!("embedding of {} values, expected {}", v.len(), embedder.dim()))) }
                        });
                        match &r {
                            Ok(v) => {
                                cache.store_embedding(&hash, &id, version, Ok(v.clone()))?;
                                stats.embedded += 1;
                                pr.embedded += 1;
                            }
                            Err(e) => {
                                log::debug!("embedding failed for {}: {e}", candidates[i].path.display());
                                cache.store_embedding(&hash, &id, version, Err(&e.to_string()))?;
                                stats.errors += 1;
                            }
                        }
                        pr.done += 1;
                        pr.current = Some(candidates[i].path.clone());
                    }
                    Ok(())
                })();
                match written {
                    Ok(()) => cache.conn.execute_batch("COMMIT")?,
                    Err(e) => {
                        let _ = cache.conn.execute_batch("ROLLBACK");
                        return Err(e);
                    }
                }
                progress(&pr);
            }
            Ok(())
        });
        result?;
        stats.cancelled = cancel.load(Ordering::Relaxed) && pr.done < pr.total;
        pr.current = None;
        progress(&pr);
        Ok(stats)
    }

    /// The vector of one sample: from `cache` when it has one, else computed
    /// now (and remembered there when the cache is writable). For the
    /// reference of a "similar to" query whose file the job has not reached.
    pub fn embedding_of(&self, id: SampleId, cache: &Library, embedder: &dyn Embedder) -> Result<Option<Vec<f32>>> {
        let s = self.get(id)?;
        if s.cloud || s.duration.is_some_and(|d| d > MAX_EMBED_SECONDS) {
            return Ok(None);
        }
        let hash = match s.hash.as_deref().and_then(hex_hash) {
            Some(h) => Some(h),
            None => quick_hash(&s.path).ok(),
        };
        if let Some(h) = &hash
            && let Some(v) = cache.embedding_for_hash(h, embedder.id(), embedder.version())?
        {
            return Ok(Some(v));
        }
        let v = embedder.embed(&s.path)?;
        if let Some(h) = &hash
            && cache.ensure_embedding_table().is_ok()
        {
            let _ = cache.store_embedding(h, embedder.id(), embedder.version(), Ok(v.clone()));
        }
        Ok(Some(v))
    }
}

fn hex_hash(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

struct EmbedCandidate {
    path: PathBuf,
    hash: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EmbedOptions {
    /// Worker threads; 0 = one per core (at most 8).
    pub threads: usize,
    /// Stop after this many candidates (tests, trial runs).
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EmbedProgress {
    /// Index of the source in an [`EmbedJob`] (always 0 for [`Library::embed_missing`]).
    pub source: usize,
    pub done: u64,
    pub total: u64,
    /// Decoded so far / already in the cache.
    pub embedded: u64,
    pub cached: u64,
    pub current: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedStats {
    pub candidates: u64,
    pub embedded: u64,
    pub cached: u64,
    pub errors: u64,
    pub cancelled: bool,
}

impl EmbedStats {
    fn add(&mut self, o: &Self) {
        self.candidates += o.candidates;
        self.embedded += o.embedded;
        self.cached += o.cached;
        self.errors += o.errors;
        self.cancelled |= o.cancelled;
    }
}

/// Messages from a background embedding run.
#[derive(Clone, Debug)]
pub enum EmbedEvent {
    Progress(EmbedProgress),
    Finished(EmbedStats),
    Failed(String),
}

/// Embedding on its own thread with its own connections.
pub struct EmbedJob {
    pub events: Receiver<EmbedEvent>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<EmbedStats>>>,
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

impl EmbedJob {
    /// The app's job: the factory bank (read-only, if any) and the user
    /// library, vectors cached in the user library.
    pub fn spawn_library(factory: Option<PathBuf>, user: PathBuf, embedder: Arc<dyn Embedder>, opts: EmbedOptions) -> Self {
        let mut sources: Vec<ReanalyseSource> = factory.into_iter().map(|db| ReanalyseSource { db, writable: false }).collect();
        sources.push(ReanalyseSource { db: user.clone(), writable: true });
        Self::spawn(sources, user, embedder, opts)
    }

    /// Embeds `sources` in order, caching in `cache_db` (which may be one of them).
    pub fn spawn(sources: Vec<ReanalyseSource>, cache_db: PathBuf, embedder: Arc<dyn Embedder>, opts: EmbedOptions) -> Self {
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let handle = std::thread::Builder::new()
            .name("aitrack-library-embed".into())
            .spawn(move || {
                let run = || -> Result<EmbedStats> {
                    let cache = Library::open(&cache_db)?;
                    let mut total = EmbedStats::default();
                    for (idx, src) in sources.iter().enumerate() {
                        if c.load(Ordering::Relaxed) {
                            total.cancelled = true;
                            break;
                        }
                        let mut last = Instant::now() - Duration::from_secs(1);
                        let mut on_progress = |p: &EmbedProgress| {
                            if p.current.is_none() || last.elapsed() >= Duration::from_millis(50) {
                                last = Instant::now();
                                let _ = tx.send(EmbedEvent::Progress(EmbedProgress { source: idx, ..p.clone() }));
                            }
                        };
                        let stats = if same_file(&src.db, &cache_db) {
                            cache.embed_missing(&cache, embedder.as_ref(), &opts, &mut on_progress, &c)?
                        } else {
                            if !src.db.is_file() {
                                return Err(LibraryError::NotFound(src.db.display().to_string()));
                            }
                            let lib = Library::open_read_only(&src.db)?;
                            lib.embed_missing(&cache, embedder.as_ref(), &opts, &mut on_progress, &c)?
                        };
                        total.add(&stats);
                    }
                    Ok(total)
                };
                let res = run();
                let _ = tx.send(match &res {
                    Ok(s) => EmbedEvent::Finished(s.clone()),
                    Err(e) => EmbedEvent::Failed(e.to_string()),
                });
                res
            })
            .expect("spawn embedding thread");
        Self { events: rx, cancel, handle: Some(handle) }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    pub fn join(mut self) -> Result<EmbedStats> {
        match self.handle.take() {
            Some(h) => h.join().map_err(|_| LibraryError::Other("embedding thread panicked".into()))?,
            None => Err(LibraryError::Other("already joined".into())),
        }
    }
}

// ───────────────────────────── Searching ─────────────────────────────

/// What the index knows about a sample besides its vector.
#[derive(Clone, Debug, PartialEq)]
pub struct SimilarEntry {
    /// Which library it comes from (the app's index: 0 factory, 1 user).
    pub lib: u8,
    pub id: SampleId,
    pub category: Option<Category>,
    pub subcategory: Option<String>,
    pub key: Option<(Key, Option<ScaleKind>)>,
    pub bpm: Option<f32>,
    pub is_loop: bool,
}

impl SimilarEntry {
    pub fn scale(&self) -> Option<Scale> {
        let (root, kind) = self.key?;
        Some(Scale::new(root, kind?))
    }
}

/// How to rank the neighbours.
#[derive(Clone, Debug)]
pub struct SimilarOptions {
    /// At most this many results.
    pub limit: usize,
    /// Only samples of the same category (and sub-category, when the reference has one): kicks for a kick.
    pub same_category: Option<(Category, Option<String>)>,
    /// Samples in this key (or one that fits it) come ahead of equals.
    pub key: Option<Scale>,
    /// Loops at this tempo (or half or double) come ahead of equals.
    pub bpm: Option<f32>,
    /// Drop results less similar than this cosine.
    pub min_similarity: f32,
}

impl Default for SimilarOptions {
    fn default() -> Self {
        Self { limit: 50, same_category: None, key: None, bpm: None, min_similarity: -1.0 }
    }
}

/// Largest change the key or the tempo can make to a score (cosine runs -1..1).
pub const KEY_WEIGHT: f32 = 0.1;
pub const BPM_WEIGHT: f32 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Neighbour {
    pub lib: u8,
    pub id: SampleId,
    /// Cosine similarity of the (standardised) vectors, -1..1.
    pub similarity: f32,
    /// What the list is ordered by: similarity plus the key and tempo boosts.
    pub score: f32,
}

/// All vectors of a collection in one matrix, ready for nearest-neighbour queries.
pub struct SimilarIndex {
    dim: usize,
    entries: Vec<SimilarEntry>,
    /// `entries.len() * dim` values, row `i` standardised and of length 1.
    rows: Vec<f32>,
    mean: Vec<f32>,
    inv_std: Vec<f32>,
    by_id: HashMap<(u8, i64), usize>,
    /// The embedder this was built for.
    embedder: String,
    version: u32,
}

impl SimilarIndex {
    /// Builds the index from raw vectors. Entries whose vector has the wrong
    /// length are dropped.
    pub fn from_vectors(dim: usize, standardise: bool, items: Vec<(SimilarEntry, Vec<f32>)>) -> Self {
        let items: Vec<_> = items.into_iter().filter(|(_, v)| v.len() == dim && v.iter().all(|x| x.is_finite())).collect();
        let n = items.len();
        let mut mean = vec![0f32; dim];
        let mut inv_std = vec![1f32; dim];
        // Too few sounds to know what is usual: compare the features as they are.
        if standardise && n >= 8 {
            for (_, v) in &items {
                for (m, x) in mean.iter_mut().zip(v) {
                    *m += x;
                }
            }
            mean.iter_mut().for_each(|m| *m /= n as f32);
            let mut var = vec![0f32; dim];
            for (_, v) in &items {
                for ((s, x), m) in var.iter_mut().zip(v).zip(&mean) {
                    *s += (x - m) * (x - m);
                }
            }
            for (i, s) in var.iter().enumerate() {
                let sd = (s / n as f32).sqrt();
                // A feature that never varies says nothing.
                inv_std[i] = if sd > 1e-6 { 1.0 / sd } else { 0.0 };
            }
        }
        let mut entries = Vec::with_capacity(n);
        let mut rows = Vec::with_capacity(n * dim);
        let mut by_id = HashMap::with_capacity(n);
        for (e, v) in items {
            by_id.insert((e.lib, e.id.0), entries.len());
            rows.extend(prepare(&v, &mean, &inv_std));
            entries.push(e);
        }
        Self { dim, entries, rows, mean, inv_std, by_id, embedder: String::new(), version: 0 }
    }

    /// Loads the vectors `embedder` stored in `cache` for the samples of `libs`
    /// (each tagged with the number the app calls it by).
    pub fn load(libs: &[(u8, &Library)], cache: &Library, embedder: &dyn Embedder) -> Result<Self> {
        let (id, version) = (embedder.id().to_string(), embedder.version());
        let mut vectors: HashMap<[u8; 32], Vec<f32>> = HashMap::new();
        if cache.has_embedding_table()? {
            let mut st = cache.conn.prepare("SELECT hash, vec FROM sample_embeddings WHERE embedder = ?1 AND version >= ?2 AND vec IS NOT NULL")?;
            let rows = st.query_map(params![id, version], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
            for r in rows {
                let (h, v) = r?;
                if let Ok(h) = <[u8; 32]>::try_from(h) {
                    vectors.insert(h, decode(&v));
                }
            }
        }
        let mut items = Vec::new();
        for &(tag, lib) in libs {
            let mut st = lib.conn.prepare(
                "SELECT id, hash, category, subcategory, key_root, scale_kind, bpm, is_loop FROM samples
                 WHERE status = 'ok' AND hash IS NOT NULL",
            )?;
            let rows = st.query_map([], |r| {
                let cat: Option<String> = r.get(2)?;
                let key: Option<i64> = r.get(4)?;
                let kind: Option<String> = r.get(5)?;
                Ok((
                    hash_from_blob(r.get(1)?),
                    SimilarEntry {
                        lib: tag,
                        id: SampleId(r.get(0)?),
                        category: cat.as_deref().and_then(Category::parse),
                        subcategory: r.get(3)?,
                        key: key.map(|k| (Key(k as u8), kind.as_deref().and_then(parse_scale_kind))),
                        bpm: r.get::<_, Option<f64>>(6)?.map(|b| b as f32),
                        is_loop: r.get(7)?,
                    },
                ))
            })?;
            for r in rows {
                let (hash, entry) = r?;
                if let Some(v) = hash.and_then(|h| vectors.get(&h)) {
                    items.push((entry, v.clone()));
                }
            }
        }
        let mut index = Self::from_vectors(embedder.dim(), embedder.standardise(), items);
        index.embedder = id;
        index.version = version;
        Ok(index)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// The embedder (id, version) the vectors come from; empty for an index made by [`SimilarIndex::from_vectors`].
    pub fn embedder(&self) -> (&str, u32) {
        (&self.embedder, self.version)
    }

    pub fn entry(&self, lib: u8, id: SampleId) -> Option<&SimilarEntry> {
        self.by_id.get(&(lib, id.0)).map(|&i| &self.entries[i])
    }

    pub fn contains(&self, lib: u8, id: SampleId) -> bool {
        self.by_id.contains_key(&(lib, id.0))
    }

    /// The options that answer "more like this one": same category, ranked
    /// towards its key and tempo. `None` when the sample is not in the index.
    pub fn options_like(&self, lib: u8, id: SampleId, limit: usize) -> Option<SimilarOptions> {
        let e = self.entry(lib, id)?;
        Some(SimilarOptions {
            limit,
            same_category: e.category.map(|c| (c, e.subcategory.clone())),
            key: e.scale(),
            bpm: e.bpm.filter(|_| e.is_loop),
            ..Default::default()
        })
    }

    /// The samples that sound most like the one at (`lib`, `id`), without itself.
    pub fn nearest_to(&self, lib: u8, id: SampleId, opts: &SimilarOptions) -> Vec<Neighbour> {
        let Some(&i) = self.by_id.get(&(lib, id.0)) else { return Vec::new() };
        let q = self.rows[i * self.dim..(i + 1) * self.dim].to_vec();
        self.search(&q, Some(i), opts)
    }

    /// The samples that sound most like a raw vector (one the embedder made for a file that is
    /// not indexed yet), optionally leaving out `exclude`.
    pub fn nearest(&self, raw: &[f32], exclude: Option<(u8, SampleId)>, opts: &SimilarOptions) -> Vec<Neighbour> {
        if raw.len() != self.dim {
            return Vec::new();
        }
        let skip = exclude.and_then(|(l, id)| self.by_id.get(&(l, id.0)).copied());
        self.search(&prepare(raw, &self.mean, &self.inv_std), skip, opts)
    }

    fn search(&self, q: &[f32], skip: Option<usize>, opts: &SimilarOptions) -> Vec<Neighbour> {
        if opts.limit == 0 || self.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(f32, f32, usize)> = Vec::new();
        for (i, row) in self.rows.chunks_exact(self.dim).enumerate() {
            if Some(i) == skip {
                continue;
            }
            let e = &self.entries[i];
            if let Some((cat, sub)) = &opts.same_category
                && (e.category != Some(*cat) || sub.as_ref().is_some_and(|s| e.subcategory.as_ref() != Some(s)))
            {
                continue;
            }
            let sim: f32 = row.iter().zip(q).map(|(a, b)| a * b).sum();
            if sim < opts.min_similarity {
                continue;
            }
            let mut score = sim;
            if let (Some(scale), Some((root, kind))) = (opts.key, e.key) {
                score += KEY_WEIGHT * (key_compat(scale, Some(root), kind) - 0.5);
            }
            if let (Some(bpm), true, Some(b)) = (opts.bpm, e.is_loop, e.bpm) {
                score += BPM_WEIGHT * (tempo_compat(bpm, Some(b), true) - 0.5);
            }
            hits.push((score, sim, i));
        }
        let cmp = |a: &(f32, f32, usize), b: &(f32, f32, usize)| b.0.total_cmp(&a.0).then(a.2.cmp(&b.2));
        if hits.len() > opts.limit {
            hits.select_nth_unstable_by(opts.limit - 1, cmp);
            hits.truncate(opts.limit);
        }
        hits.sort_by(cmp);
        hits.into_iter().map(|(score, similarity, i)| Neighbour { lib: self.entries[i].lib, id: self.entries[i].id, similarity, score }).collect()
    }
}

// ───────────────────────────── Finding ─────────────────────────────

/// One answer of [`SimilarFinder::find`].
#[derive(Clone, Debug)]
pub struct SimilarHit {
    /// The library the sample is in (the index of the `libs` given to `find`).
    pub lib: u8,
    pub sample: crate::db::Sample,
    /// Cosine similarity, -1..1 (about 1 for the same sound).
    pub similarity: f32,
}

/// Answers "sounds like this one" over the app's two libraries, `[factory, user]`.
/// Keeps the index between questions and builds it again when the vectors or
/// the samples changed. The user library holds the vectors (see [`EmbedJob::spawn_library`]).
#[derive(Default)]
pub struct SimilarFinder {
    index: Option<((u64, u64, u64), SimilarIndex)>,
}

impl SimilarFinder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the index (the libraries changed in a way the cheap check cannot see).
    pub fn invalidate(&mut self) {
        self.index = None;
    }

    /// The index as it is now (built when missing or out of date).
    pub fn index(&mut self, libs: [Option<&Library>; 2], embedder: &dyn Embedder) -> Result<&SimilarIndex> {
        let Some(cache) = libs[1] else { return Err(LibraryError::NotFound("the user library".into())) };
        let stamp = (
            cache.embedding_count(embedder.id(), embedder.version())?,
            libs[0].map_or(0, |l| l.count().unwrap_or(0)),
            cache.count().unwrap_or(0),
        );
        if self.index.as_ref().is_none_or(|(s, _)| *s != stamp) {
            let tagged: Vec<(u8, &Library)> = libs.iter().enumerate().filter_map(|(i, l)| l.map(|l| (i as u8, l))).collect();
            self.index = Some((stamp, SimilarIndex::load(&tagged, cache, embedder)?));
        }
        Ok(&self.index.as_ref().expect("just built").1)
    }

    /// The `limit` samples that sound most like sample `id` of library `lib`: the same
    /// kind of sound (category and sub-category), nearer when they share its key and tempo.
    /// A reference the job has not reached yet is listened to now.
    pub fn find(&mut self, libs: [Option<&Library>; 2], lib: u8, id: SampleId, embedder: &dyn Embedder, limit: usize) -> Result<Vec<SimilarHit>> {
        let (Some(source), Some(cache)) = (libs.get(lib as usize).copied().flatten(), libs[1]) else { return Ok(Vec::new()) };
        let mut reference = vec![source.get(id)?];
        // Keys and tempos found by listening are in the user library's cache.
        let _ = cache.fill_from_cache(&mut reference);
        let reference = reference.remove(0);
        let Some(raw) = source.embedding_of(id, cache, embedder)? else { return Ok(Vec::new()) };
        let opts = SimilarOptions {
            limit,
            same_category: reference.category.map(|c| (c, reference.subcategory.clone())),
            key: reference.scale(),
            bpm: reference.bpm.filter(|_| reference.is_loop()),
            ..Default::default()
        };
        let neighbours = self.index(libs, embedder)?.nearest(&raw, Some((lib, id)), &opts);
        let mut out = Vec::with_capacity(neighbours.len());
        for n in neighbours {
            if let Some(l) = libs.get(n.lib as usize).copied().flatten()
                && let Ok(sample) = l.get(n.id)
            {
                out.push(SimilarHit { lib: n.lib, sample, similarity: n.similarity });
            }
        }
        let mut samples: Vec<_> = out.iter().map(|h| h.sample.clone()).collect();
        let _ = cache.fill_from_cache(&mut samples);
        for (h, s) in out.iter_mut().zip(samples) {
            h.sample = s;
        }
        Ok(out)
    }
}

/// Standardises `v` and scales it to length 1 (a vector with nothing in it stays zero).
fn prepare(v: &[f32], mean: &[f32], inv_std: &[f32]) -> Vec<f32> {
    let mut out: Vec<f32> = v.iter().zip(mean).zip(inv_std).map(|((x, m), s)| (x - m) * s).collect();
    let norm = out.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        out.iter_mut().for_each(|x| *x /= norm);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::BasicAnalyzer;
    use crate::db::RootKind;
    use crate::scan::ScanOptions;
    use crate::test_util::write_wav;
    use std::sync::atomic::AtomicU32;

    /// A deterministic "embedder": the pitch of the sine in the file name ("Tone 440.wav") as a one-hot-ish vector.
    struct Fake {
        calls: AtomicU32,
        version: u32,
    }

    impl Fake {
        fn new(version: u32) -> Self {
            Self { calls: AtomicU32::new(0), version }
        }
    }

    impl Embedder for Fake {
        fn id(&self) -> &str {
            "fake"
        }
        fn version(&self) -> u32 {
            self.version
        }
        fn dim(&self) -> usize {
            4
        }
        fn embed(&self, path: &Path) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.contains("broken") {
                return Err(LibraryError::Audio("bad".into()));
            }
            let n = name.chars().filter(char::is_ascii_digit).collect::<String>().parse::<f32>().unwrap_or(0.0);
            Ok(vec![n, n * 0.5, 1.0, (n / 100.0).sin()])
        }
    }

    fn rng(seed: &mut u64) -> f32 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*seed >> 33) as f32) / (1u64 << 31) as f32
    }

    fn entry(lib: u8, id: i64, cat: Option<Category>) -> SimilarEntry {
        SimilarEntry { lib, id: SampleId(id), category: cat, subcategory: None, key: None, bpm: None, is_loop: false }
    }

    #[test]
    fn nearest_neighbours_are_ordered_and_leave_the_query_out() {
        let items: Vec<_> = (0..20).map(|i| (entry(1, i, None), vec![i as f32, (i * i) as f32 % 7.0, 1.0, (i % 3) as f32])).collect();
        let idx = SimilarIndex::from_vectors(4, true, items);
        let r = idx.nearest_to(1, SampleId(5), &SimilarOptions { limit: 5, ..Default::default() });
        assert_eq!(r.len(), 5);
        assert!(r.iter().all(|n| n.id != SampleId(5)));
        assert!(r.windows(2).all(|w| w[0].score >= w[1].score));
        assert!(idx.nearest_to(1, SampleId(999), &SimilarOptions::default()).is_empty());
    }

    #[test]
    fn same_category_keeps_to_the_category_and_boosts_reorder() {
        let mut items = Vec::new();
        for i in 0..12 {
            let cat = if i % 2 == 0 { Category::Drums } else { Category::Bass };
            // Every vector nearly the same, so only the boosts decide.
            items.push((entry(1, i, Some(cat)), vec![1.0 + i as f32 * 0.001, 2.0, 3.0 - i as f32 * 0.001, rng(&mut (i as u64 + 1))]));
        }
        let idx = SimilarIndex::from_vectors(4, false, items);
        let drums = SimilarOptions { limit: 20, same_category: Some((Category::Drums, None)), ..Default::default() };
        let r = idx.nearest_to(1, SampleId(0), &drums);
        assert_eq!(r.len(), 5, "the other drums only");
        assert!(r.iter().all(|n| n.id.0 % 2 == 0));

        let mut keyed: Vec<_> = (0..12).map(|i| (entry(1, i, None), vec![1.0, 2.0, 3.0, 4.0 + i as f32 * 1e-4])).collect();
        keyed[3].0.key = Some((Key(9), Some(ScaleKind::Minor)));
        keyed[7].0.key = Some((Key(1), Some(ScaleKind::Major)));
        let idx = SimilarIndex::from_vectors(4, false, keyed);
        let am = Scale::new(Key(9), ScaleKind::Minor);
        let r = idx.nearest_to(1, SampleId(0), &SimilarOptions { key: Some(am), limit: 3, ..Default::default() });
        assert_eq!(r[0].id, SampleId(3), "the sample in A minor leads: {r:?}");
        assert!(r.iter().all(|n| n.id != SampleId(7)), "the one in a far key trails");
    }

    #[test]
    fn ten_thousand_samples_answer_in_a_blink() {
        let dim = 64;
        let mut seed = 7;
        let items: Vec<_> = (0..10_000).map(|i| (entry((i % 2) as u8, i, Some(Category::Drums)), (0..dim).map(|_| rng(&mut seed) * 4.0 - 2.0).collect::<Vec<f32>>())).collect();
        let t0 = Instant::now();
        let idx = SimilarIndex::from_vectors(dim, true, items);
        let build = t0.elapsed();
        let t1 = Instant::now();
        let runs = 20;
        let mut last = Vec::new();
        for i in 0..runs {
            last = idx.nearest_to((i % 2) as u8, SampleId(i), &SimilarOptions { limit: 50, ..Default::default() });
        }
        let per_query = t1.elapsed() / runs as u32;
        eprintln!("10 000 x {dim}: built in {build:?}, {per_query:?} per query");
        assert_eq!(last.len(), 50);
        assert!(per_query < Duration::from_millis(50), "{per_query:?}");
        // The query's own vector is the best match of itself.
        let own = idx.nearest(&idx.rows[0..dim], None, &SimilarOptions { limit: 1, ..Default::default() });
        assert!(own[0].similarity > 0.99);
    }

    fn library_with_sounds() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let samples = dir.path().join("samples");
        std::fs::create_dir_all(&samples).unwrap();
        for (name, amp) in [("Tone 100.wav", 0.5), ("Tone 110.wav", 0.4), ("Tone 900.wav", 0.6), ("broken 5.wav", 0.7)] {
            let p = samples.join(name);
            write_wav(&p, 44_100, 1, 0.3, amp);
        }
        let db = dir.path().join("lib.db");
        let lib = Library::open(&db).unwrap();
        let root = lib.add_root(&samples, RootKind::User).unwrap();
        lib.scan(&BasicAnalyzer, &ScanOptions { roots: Some(vec![root]), ..Default::default() }, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        (dir, db, samples)
    }

    /// Embeds the first two files at once, then blocks on the third until released.
    struct Gate {
        inner: Fake,
        release: Arc<AtomicBool>,
    }

    impl Embedder for Gate {
        fn id(&self) -> &str {
            "fake"
        }
        fn version(&self) -> u32 {
            1
        }
        fn dim(&self) -> usize {
            4
        }
        fn embed(&self, path: &Path) -> Result<Vec<f32>> {
            if self.inner.calls.load(Ordering::Relaxed) >= 2 {
                self.inner.calls.fetch_add(1, Ordering::Relaxed);
                while !self.release.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                return self.inner.embed(path);
            }
            self.inner.embed(path)
        }
    }

    #[test]
    fn waiting_on_a_slow_decode_does_not_hold_the_write_lock() {
        let (_dir, db, _) = library_with_sounds();
        let other = Library::open(&db).unwrap();
        let release = Arc::new(AtomicBool::new(false));
        let gate = Gate { inner: Fake::new(1), release: release.clone() };
        let cancel = AtomicBool::new(false);
        let fav = other.connection().query_row("SELECT id FROM samples WHERE name LIKE 'Tone 900%'", [], |r| r.get::<_, i64>(0)).unwrap();
        std::thread::scope(|s| {
            let job = s.spawn(|| {
                let lib = Library::open(&db).unwrap();
                lib.embed_missing(&lib, &gate, &EmbedOptions { threads: 1, limit: None }, &mut |_| {}, &cancel)
            });
            // Two files are embedded and stored; the third decode is stuck.
            let t0 = Instant::now();
            while gate.inner.calls.load(Ordering::Relaxed) < 3 && t0.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(300));
            // Another connection (the UI) writes while the job waits.
            other.connection().busy_timeout(Duration::from_millis(500)).unwrap();
            let wrote = other.set_favorite(SampleId(fav), true);
            release.store(true, Ordering::Relaxed);
            job.join().unwrap().unwrap();
            assert!(wrote.is_ok(), "the write blocked behind the embed job: {wrote:?}");
        });
        assert_eq!(other.embedding_count("fake", 1).unwrap(), 3);
    }

    #[test]
    fn embedding_is_incremental_cached_by_version_and_finds_neighbours() {
        let (_dir, db, _) = library_with_sounds();
        let lib = Library::open(&db).unwrap();
        let fake = Fake::new(1);
        let cancel = AtomicBool::new(false);
        let s1 = lib.embed_missing(&lib, &fake, &EmbedOptions { threads: 2, limit: None }, &mut |_| {}, &cancel).unwrap();
        assert_eq!((s1.candidates, s1.embedded, s1.errors), (4, 3, 1), "{s1:?}");
        assert_eq!(fake.calls.load(Ordering::Relaxed), 4);
        assert_eq!(lib.embedding_count("fake", 1).unwrap(), 3);

        // Nothing changed: nothing is decoded again (the broken file is remembered as broken).
        let s2 = lib.embed_missing(&lib, &fake, &EmbedOptions::default(), &mut |_| {}, &cancel).unwrap();
        assert_eq!((s2.embedded, s2.cached), (0, 4), "{s2:?}");
        assert_eq!(fake.calls.load(Ordering::Relaxed), 4);

        // A newer version of the embedder re-does everything.
        let v2 = Fake::new(2);
        let s3 = lib.embed_missing(&lib, &v2, &EmbedOptions::default(), &mut |_| {}, &cancel).unwrap();
        assert_eq!(s3.embedded, 3, "{s3:?}");
        assert_eq!(lib.embedding_count("fake", 2).unwrap(), 3);

        let index = SimilarIndex::load(&[(1, &lib)], &lib, &v2).unwrap();
        assert_eq!(index.len(), 3);
        let by_name = |n: &str| lib.connection().query_row("SELECT id FROM samples WHERE name = ?1", [n], |r| r.get::<_, i64>(0)).unwrap();
        let r = index.nearest_to(1, SampleId(by_name("Tone 100")), &SimilarOptions { limit: 2, ..Default::default() });
        assert_eq!(r[0].id, SampleId(by_name("Tone 110")), "{r:?}");
        // A version-1 index finds nothing once only version 2 matters.
        let old = Fake::new(3);
        assert!(SimilarIndex::load(&[(1, &lib)], &lib, &old).unwrap().is_empty());
    }

    #[test]
    fn a_read_only_bank_is_embedded_into_the_users_database() {
        let (dir, bank_db, _) = library_with_sounds();
        let user_db = dir.path().join("user.db");
        Library::open(&user_db).unwrap();
        let tables_before: i64 = Library::open_read_only(&bank_db).unwrap().connection().query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0)).unwrap();
        let fake = Arc::new(Fake::new(1));
        let job = EmbedJob::spawn_library(Some(bank_db.clone()), user_db.clone(), fake.clone(), EmbedOptions { threads: 2, limit: None });
        let stats = job.join().unwrap();
        assert_eq!(stats.embedded, 3, "{stats:?}");
        let bank = Library::open_read_only(&bank_db).unwrap();
        let tables_after: i64 = bank.connection().query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0)).unwrap();
        assert_eq!(tables_before, tables_after, "the bank got no new table");
        let user = Library::open(&user_db).unwrap();
        assert_eq!(user.embedding_count("fake", 1).unwrap(), 3);
        // The bank's samples are found through their hashes.
        let index = SimilarIndex::load(&[(0, &bank), (1, &user)], &user, fake.as_ref()).unwrap();
        assert_eq!(index.len(), 3);
        // An embedding asked for one sample comes from the cache without decoding.
        let id = SampleId(bank.connection().query_row("SELECT id FROM samples WHERE name = 'Tone 900'", [], |r| r.get(0)).unwrap());
        let calls = fake.calls.load(Ordering::Relaxed);
        assert!(bank.embedding_of(id, &user, fake.as_ref()).unwrap().is_some());
        assert_eq!(fake.calls.load(Ordering::Relaxed), calls);
    }

    #[test]
    fn a_database_from_before_the_table_gets_it_on_first_use() {
        let (_dir, db, _) = library_with_sounds();
        {
            let lib = Library::open(&db).unwrap();
            assert_eq!(lib.embedding_count("fake", 1).unwrap(), 0, "no table is not an error");
            assert!(lib.connection().query_row("SELECT 1 FROM sqlite_master WHERE name = 'sample_embeddings'", [], |_| Ok(())).optional().unwrap().is_none());
        }
        let lib = Library::open(&db).unwrap();
        lib.embed_missing(&lib, &Fake::new(1), &EmbedOptions::default(), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(lib.embedding_count("fake", 1).unwrap(), 3);
        // The rest of the old rows are untouched.
        assert_eq!(lib.count().unwrap(), 4);
    }

    #[test]
    fn online_only_cloud_files_are_never_opened() {
        let (_dir, db, samples) = library_with_sounds();
        let lib = Library::open(&db).unwrap();
        lib.connection().execute("UPDATE samples SET cloud = 1 WHERE name = 'Tone 900'", []).unwrap();
        let fake = Fake::new(1);
        let s = lib.embed_missing(&lib, &fake, &EmbedOptions::default(), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(s.candidates, 3);
        assert_eq!(fake.calls.load(Ordering::Relaxed), 3);
        let id = SampleId(lib.connection().query_row("SELECT id FROM samples WHERE name = 'Tone 900'", [], |r| r.get(0)).unwrap());
        assert!(lib.embedding_of(id, &lib, &fake).unwrap().is_none(), "{}", samples.display());
        assert_eq!(fake.calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn the_finder_answers_from_two_libraries_and_listens_to_a_reference_not_yet_embedded() {
        let (dir, bank_db, _) = library_with_sounds();
        let user_db = dir.path().join("user.db");
        Library::open(&user_db).unwrap();
        let fake = Arc::new(Fake::new(1));
        EmbedJob::spawn_library(Some(bank_db.clone()), user_db.clone(), fake.clone(), EmbedOptions { threads: 1, limit: None }).join().unwrap();
        let bank = Library::open_read_only(&bank_db).unwrap();
        let user = Library::open(&user_db).unwrap();
        let id = |n: &str| SampleId(bank.connection().query_row("SELECT id FROM samples WHERE name = ?1", [n], |r| r.get(0)).unwrap());
        let mut finder = SimilarFinder::new();
        let hits = finder.find([Some(&bank), Some(&user)], 0, id("Tone 100"), fake.as_ref(), 5).unwrap();
        assert_eq!(hits.first().map(|h| h.sample.name.as_str()), Some("Tone 110"), "{:?}", hits.iter().map(|h| &h.sample.name).collect::<Vec<_>>());
        assert!(hits.iter().all(|h| h.sample.name != "Tone 100"));
        assert!(hits[0].similarity > hits.last().unwrap().similarity);
        // Wipe the vectors: the reference is listened to on the spot, the others are simply not there yet.
        user.connection().execute("DELETE FROM sample_embeddings", []).unwrap();
        let hits = finder.find([Some(&bank), Some(&user)], 0, id("Tone 100"), fake.as_ref(), 5).unwrap();
        assert!(hits.is_empty(), "nothing to compare with yet");
        assert_eq!(user.embedding_count("fake", 1).unwrap(), 1, "the reference was remembered");
    }

    #[test]
    fn cancel_keeps_what_was_done() {
        let (_dir, db, _) = library_with_sounds();
        let lib = Library::open(&db).unwrap();
        let cancel = AtomicBool::new(true);
        let s = lib.embed_missing(&lib, &Fake::new(1), &EmbedOptions::default(), &mut |_| {}, &cancel).unwrap();
        assert!(s.cancelled);
        assert_eq!(s.embedded, 0);
    }
}
