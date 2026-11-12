//! Key/BPM re-analysis of samples whose key or tempo is unknown.
//!
//! The scanner's fast pass only knows what the file name says, so most
//! samples come out without a key. [`Library::reanalyse`] picks the rows that
//! are still missing a key or a tempo, runs a [`KeyBpmDetector`] over them on
//! a few worker threads and writes what it found.
//!
//! Every detection, confident or not, is remembered in an `analysis_cache`
//! table keyed by the file's [`quick_hash`] and the detector, so a file is
//! decoded at most once per detector version. The cache can live in another
//! database than the samples: the app re-analyses the read-only factory bank
//! into the user library ([`ReanalyseJob::spawn_library`]) and reads the
//! results back with [`Library::fill_from_cache`]; `cargo xtask soundbank
//! index` keeps its cache next to the downloads so a rebuilt `factory.db` does
//! not start from zero.
//!
//! Hand-set values (`key_source = 'user'`, `bpm_source = 'user'`) are never
//! touched, and neither is a row that already has the value; the one
//! exception is a tempo guessed from the loop length (`bpm_source =
//! 'estimate'`), which a confident detection replaces. Only loops get a
//! tempo: a detector that hears a beat in a two-second bass note is wrong.
//!
//! [`Library::retag`] is the other kind of re-analysis: it re-derives the
//! labels (category, genres, loop/one-shot, name tempo) of every stored row
//! from its path and stored length, without decoding anything. The database
//! runs it on open whenever [`TAGGER_VERSION`] is newer than the one that
//! made its labels.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aitrack_model::{Key, Scale};
use crossbeam_channel::{Receiver, unbounded};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::analyze::{AudioInfo, Source, analysis_from_hints};
use crate::db::{Library, SampleId, Sample, encode_list, now, parse_scale_kind, scale_kind_str};
use crate::error::{LibraryError, Result};
use crate::parse::KeyHint;
use crate::scan::{PackCache, hints_for, quick_hash};
use crate::taxonomy::{Category, SampleKind};

/// Version of the labelling rules (parser, pack defaults, loop/one-shot
/// classification). Increase it when they change so stored rows are
/// re-tagged on the next open.
///
/// 1: genres by whole word only, a pack's genre list no longer copied onto
/// every file, a manifest pack's folder name not read as a category, the
/// ordered loop/one-shot rules, no detected tempo on one-shots.
pub const TAGGER_VERSION: u32 = 1;

/// A key needs at least this confidence to be stored on a sample.
pub const KEY_MIN_CONFIDENCE: f32 = 0.5;
/// A tempo needs at least this confidence (and a loop) to be stored.
pub const BPM_MIN_CONFIDENCE: f32 = 0.4;
/// Shorter files are not worth a key estimate (the detectors skip them too).
const KEY_MIN_SECONDS: f64 = 0.75;
/// Shorter files cannot be loops, so they never get a tempo.
const BPM_MIN_SECONDS: f64 = 1.0;

const CACHE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS analysis_cache(
  hash BLOB NOT NULL,
  analyzer TEXT NOT NULL,
  version INTEGER NOT NULL,
  key_root INTEGER,
  scale_kind TEXT,
  key_conf REAL,
  bpm REAL,
  bpm_conf REAL,
  is_loop INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  analyzed_at INTEGER NOT NULL,
  PRIMARY KEY(hash, analyzer));";

/// What a detector heard in one file, with raw confidences (0..1). The
/// thresholds are applied when storing, so changing them needs no re-decode.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detection {
    pub key: Option<(KeyHint, f32)>,
    pub bpm: Option<(f32, f32)>,
    pub is_loop: bool,
}

impl Detection {
    pub fn confident_key(&self) -> Option<KeyHint> {
        self.key.filter(|(_, c)| *c >= KEY_MIN_CONFIDENCE).map(|(k, _)| k)
    }

    pub fn confident_bpm(&self) -> Option<f32> {
        self.bpm.filter(|(b, c)| self.is_loop && *c >= BPM_MIN_CONFIDENCE && *b > 0.0).map(|(b, _)| b)
    }
}

/// Key/BPM from audio. Runs on worker threads, so it must be thread-safe.
pub trait KeyBpmDetector: Send + Sync {
    /// Stable identifier stored in the cache (e.g. `"aitrack-ai"`).
    fn id(&self) -> &str;
    /// Increase to make cached detections of older versions stale.
    fn version(&self) -> u32;
    fn detect(&self, path: &Path) -> Result<Detection>;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReanalyseOptions {
    /// Worker threads; 0 = one per core.
    pub threads: usize,
    /// Write confident results into the source rows (`key_source`/`bpm_source
    /// = 'audio'`). Off for a read-only source: results only go to the cache.
    pub write_samples: bool,
    /// Stop after this many candidates (tests, trial runs).
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReanalyseProgress {
    /// Index of the source in a [`ReanalyseJob`] (always 0 for [`Library::reanalyse`]).
    pub source: usize,
    pub done: u64,
    pub total: u64,
    /// Decoded so far / answered from the cache so far.
    pub analysed: u64,
    pub cached: u64,
    pub current: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReanalyseStats {
    /// Rows missing a key or a tempo.
    pub candidates: u64,
    /// Files decoded by the detector.
    pub analysed: u64,
    /// Rows answered from the cache without decoding.
    pub cached: u64,
    /// Rows that got a key / a tempo written (only with `write_samples`).
    pub keys_written: u64,
    pub bpms_written: u64,
    /// Confident keys / tempos found, written or not.
    pub keys_found: u64,
    pub bpms_found: u64,
    pub errors: u64,
    pub cancelled: bool,
}

impl ReanalyseStats {
    fn add(&mut self, o: &Self) {
        self.candidates += o.candidates;
        self.analysed += o.analysed;
        self.cached += o.cached;
        self.keys_written += o.keys_written;
        self.bpms_written += o.bpms_written;
        self.keys_found += o.keys_found;
        self.bpms_found += o.bpms_found;
        self.errors += o.errors;
        self.cancelled |= o.cancelled;
    }
}

struct Candidate {
    id: SampleId,
    path: PathBuf,
    hash: Option<[u8; 32]>,
    need_key: bool,
    need_bpm: bool,
}

type Detected = (usize, Option<[u8; 32]>, std::result::Result<Detection, String>);

fn hash_from_blob(b: Option<Vec<u8>>) -> Option<[u8; 32]> {
    b.and_then(|v| v.try_into().ok())
}

fn hash_from_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

impl Library {
    fn ensure_cache_table(&self) -> Result<()> {
        Ok(self.conn.execute_batch(CACHE_SCHEMA)?)
    }

    fn has_cache_table(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name='analysis_cache'", [], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// A cached detection by `detector` (at `min_version` or newer).
    fn cached(&self, hash: &[u8; 32], detector: &str, min_version: u32) -> Result<Option<Detection>> {
        let row = self
            .conn
            .query_row(
                "SELECT key_root, scale_kind, key_conf, bpm, bpm_conf, is_loop FROM analysis_cache
                 WHERE hash = ?1 AND analyzer = ?2 AND version >= ?3",
                params![hash.as_slice(), detector, min_version],
                row_to_detection,
            )
            .optional()?;
        Ok(row)
    }

    fn store_cached(&self, hash: &[u8; 32], detector: &str, version: u32, d: &Detection, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO analysis_cache(hash, analyzer, version, key_root, scale_kind, key_conf,
               bpm, bpm_conf, is_loop, error, analyzed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                hash.as_slice(),
                detector,
                version,
                d.key.map(|(k, _)| k.root.0),
                d.key.and_then(|(k, _)| k.kind).map(scale_kind_str),
                d.key.map(|(_, c)| c),
                d.bpm.map(|(b, _)| b),
                d.bpm.map(|(_, c)| c),
                d.is_loop,
                error,
                now(),
            ],
        )?;
        Ok(())
    }

    /// Writes the confident parts of `d` into a row still missing them.
    /// Returns (key written, bpm written).
    fn apply_detection(&self, c: &Candidate, d: &Detection) -> Result<(bool, bool)> {
        let mut wrote = (false, false);
        if c.need_key
            && let Some(k) = d.confident_key()
        {
            let camelot = k.scale().and_then(Scale::camelot);
            wrote.0 = self.conn.execute(
                "UPDATE samples SET key_root=?2, scale_kind=?3, camelot=?4, key_source='audio'
                 WHERE id=?1 AND key_root IS NULL AND COALESCE(key_source, '') != 'user'",
                params![c.id.0, k.root.0, k.kind.map(scale_kind_str), camelot],
            )? > 0;
        }
        if c.need_bpm
            && let Some(b) = d.confident_bpm()
        {
            wrote.1 = self.conn.execute(
                "UPDATE samples SET bpm=?2, bpm_source='audio'
                 WHERE id=?1 AND kind = 'loop' AND (bpm IS NULL OR bpm_source = 'estimate')",
                params![c.id.0, b],
            )? > 0;
        }
        if wrote.0 || wrote.1 {
            self.refresh_fts(c.id)?;
        }
        Ok(wrote)
    }

    /// Rows still missing a key or a tempo (or with a length-based tempo
    /// estimate). Drum hits (808s aside) and very
    /// short files are not asked for a key, files under a second not for a tempo.
    fn reanalyse_candidates(&self, limit: Option<usize>) -> Result<Vec<Candidate>> {
        let mut st = self.conn.prepare(
            "SELECT id, rel_path, root_path, hash, need_key, need_bpm FROM (
               SELECT s.id, s.rel_path, r.path AS root_path, s.hash,
                 (s.key_root IS NULL AND COALESCE(s.key_source, '') != 'user'
                   AND NOT (COALESCE(s.category, '') = 'Drums' AND COALESCE(s.subcategory, '') != '808s')
                   AND COALESCE(s.duration, 0) >= ?1) AS need_key,
                 ((s.bpm IS NULL OR s.bpm_source = 'estimate') AND s.kind = 'loop'
                   AND COALESCE(s.duration, 0) >= ?2) AS need_bpm
               FROM samples s JOIN roots r ON r.id = s.root_id WHERE s.status = 'ok')
             WHERE need_key OR need_bpm ORDER BY id LIMIT ?3",
        )?;
        let limit = limit.map_or(-1, |l| l as i64);
        let rows = st.query_map(params![KEY_MIN_SECONDS, BPM_MIN_SECONDS, limit], |r| {
            let rel: String = r.get(1)?;
            let root: String = r.get(2)?;
            Ok(Candidate {
                id: SampleId(r.get(0)?),
                path: self.resolve_root(&root).join(rel),
                hash: hash_from_blob(r.get(3)?),
                need_key: r.get(4)?,
                need_bpm: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Re-analyses this library's samples that miss a key or a tempo.
    ///
    /// Detections are cached in `cache` (which may be `self`); a file whose
    /// hash is cached for this detector version is not decoded again. With
    /// [`ReanalyseOptions::write_samples`] confident results are written to
    /// the rows. Decoding runs on [`ReanalyseOptions::threads`] workers, the
    /// database writes on the calling thread. `progress` is called after every
    /// row; setting `cancel` stops soon after (finished work is kept).
    pub fn reanalyse(
        &self,
        cache: &Library,
        detector: &dyn KeyBpmDetector,
        opts: &ReanalyseOptions,
        progress: &mut dyn FnMut(&ReanalyseProgress),
        cancel: &AtomicBool,
    ) -> Result<ReanalyseStats> {
        cache.ensure_cache_table()?;
        let mut candidates = self.reanalyse_candidates(opts.limit)?;
        let total = candidates.len() as u64;
        let mut stats = ReanalyseStats { candidates: total, ..Default::default() };
        let (det_id, det_ver) = (detector.id().to_string(), detector.version());
        let same_db = std::ptr::eq(self, cache);
        let write = opts.write_samples;
        let mut pr = ReanalyseProgress { total, ..Default::default() };

        let mut tx = Batch::begin(self, cache, same_db, write)?;
        let record = |lib: &Library, c: &Candidate, d: &Detection, stats: &mut ReanalyseStats| -> Result<()> {
            stats.keys_found += (c.need_key && d.confident_key().is_some()) as u64;
            stats.bpms_found += (c.need_bpm && d.confident_bpm().is_some()) as u64;
            if write {
                let (k, b) = lib.apply_detection(c, d)?;
                stats.keys_written += k as u64;
                stats.bpms_written += b as u64;
            }
            Ok(())
        };

        // Answer what the cache knows; the rest goes to the workers.
        let mut todo = Vec::new();
        for (i, c) in candidates.iter_mut().enumerate() {
            if c.hash.is_none() {
                c.hash = quick_hash(&c.path).ok();
            }
            let hit = match &c.hash {
                Some(h) => cache.cached(h, &det_id, det_ver)?,
                None => None,
            };
            match hit {
                Some(d) => {
                    record(self, c, &d, &mut stats)?;
                    stats.cached += 1;
                    pr.cached += 1;
                    pr.done += 1;
                    pr.current = Some(c.path.clone());
                    progress(&pr);
                    tx.tick()?;
                }
                None => todo.push(i),
            }
        }

        let threads = match opts.threads {
            0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
            n => n,
        }
        .clamp(1, todo.len().max(1));
        let next = AtomicUsize::new(0);
        let (rtx, rrx) = unbounded::<Detected>();
        let candidates = &candidates;
        let todo = &todo;
        let result = std::thread::scope(|s| -> Result<()> {
            for _ in 0..threads {
                let rtx = rtx.clone();
                let next = &next;
                s.spawn(move || {
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let n = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&i) = todo.get(n) else { break };
                        let c = &candidates[i];
                        let r = detector.detect(&c.path).map_err(|e| e.to_string());
                        let hash = c.hash.or_else(|| quick_hash(&c.path).ok());
                        if rtx.send((i, hash, r)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(rtx);
            for (i, hash, r) in rrx {
                let c = &candidates[i];
                let (d, err) = match r {
                    Ok(d) => (d, None),
                    Err(e) => {
                        log::debug!("re-analysis failed for {}: {e}", c.path.display());
                        stats.errors += 1;
                        (Detection::default(), Some(e))
                    }
                };
                if let Some(h) = &hash {
                    cache.store_cached(h, &det_id, det_ver, &d, err.as_deref())?;
                }
                record(self, c, &d, &mut stats)?;
                stats.analysed += 1;
                pr.analysed += 1;
                pr.done += 1;
                pr.current = Some(c.path.clone());
                progress(&pr);
                tx.tick()?;
            }
            Ok(())
        });
        // Whatever happened, keep the work done so far.
        tx.commit()?;
        result?;
        stats.cancelled = cancel.load(Ordering::Relaxed) && pr.done < total;
        pr.current = None;
        progress(&pr);
        Ok(stats)
    }

    /// The newest cached detection for a file hash (any detector).
    pub fn cached_detection(&self, hash: &[u8; 32]) -> Result<Option<Detection>> {
        if !self.has_cache_table()? {
            return Ok(None);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT key_root, scale_kind, key_conf, bpm, bpm_conf, is_loop FROM analysis_cache
                 WHERE hash = ?1 ORDER BY analyzed_at DESC LIMIT 1",
                [hash.as_slice()],
                row_to_detection,
            )
            .optional()?)
    }

    /// Re-derives the labels of every indexed sample from its path, its pack
    /// and the stored length and attack count, with the current rules; no
    /// file is opened. Returns how many rows were looked at.
    ///
    /// What the user set by hand stays: favourites, ratings, tags, a manual
    /// category (`label_source = 'user'`), key or tempo. A category the audio
    /// model gave (`'audio'`) stays unless the name now gives one. A tempo
    /// detected from audio stays on loops and is dropped from one-shots.
    pub fn retag(&self) -> Result<u64> {
        struct Row {
            id: i64,
            root: String,
            rel: String,
            duration: Option<f64>,
            onsets: Option<i64>,
            pack: Option<String>,
            label_source: Option<String>,
            key_source: Option<String>,
            bpm_source: Option<String>,
        }
        let rows: Vec<Row> = {
            let mut st = self.conn.prepare(
                "SELECT s.id, r.path, s.rel_path, s.duration, s.onsets, s.pack, s.label_source, s.key_source,
                        s.bpm_source
                 FROM samples s JOIN roots r ON r.id = s.root_id WHERE s.status = 'ok'",
            )?;
            st.query_map([], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    root: r.get(1)?,
                    rel: r.get(2)?,
                    duration: r.get(3)?,
                    onsets: r.get(4)?,
                    pack: r.get(5)?,
                    label_source: r.get(6)?,
                    key_source: r.get(7)?,
                    bpm_source: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        let tx = self.conn.unchecked_transaction()?;
        let mut packs = PackCache::new(self);
        for row in &rows {
            let root = self.resolve_root(&row.root);
            let path = root.join(&row.rel);
            let pack = match &row.pack {
                Some(id) => packs.get(id)?.cloned(),
                None => None,
            };
            let hints = hints_for(&root, &row.rel, &path, pack.as_ref());
            let audio = AudioInfo {
                duration_s: row.duration.unwrap_or(0.0),
                onsets: row.onsets.map(|n| n.max(0) as u32),
                ..Default::default()
            };
            let a = analysis_from_hints(audio, &hints);
            self.conn.execute(
                "UPDATE samples SET genres=?2, kind=?3, is_loop=?4, loop_family=?5, root_note=?6 WHERE id=?1",
                params![
                    row.id,
                    encode_list(&a.genres),
                    a.kind.as_str(),
                    a.kind == SampleKind::Loop,
                    a.loop_family,
                    a.root_note
                ],
            )?;
            let label = row.label_source.as_deref();
            if label != Some("user") && !(label == Some("audio") && a.category.is_none()) {
                self.conn.execute(
                    "UPDATE samples SET category=?2, subcategory=?3, label_source=?4 WHERE id=?1",
                    params![row.id, a.category.map(Category::as_str), a.subcategory, a.category_source.as_str()],
                )?;
            }
            if matches!(row.key_source.as_deref(), None | Some("filename")) {
                let scale = a.key.and_then(KeyHint::scale);
                self.conn.execute(
                    "UPDATE samples SET key_root=?2, scale_kind=?3, camelot=?4, key_source=?5 WHERE id=?1",
                    params![
                        row.id,
                        a.key.map(|k| k.root.0),
                        a.key.and_then(|k| k.kind).map(scale_kind_str),
                        scale.and_then(Scale::camelot),
                        a.key.map(|_| Source::Filename.as_str())
                    ],
                )?;
            }
            match row.bpm_source.as_deref() {
                Some("user") => {}
                Some("audio") if a.kind == SampleKind::Loop => {}
                _ => {
                    self.conn.execute(
                        "UPDATE samples SET bpm=?2, bpm_source=?3 WHERE id=?1",
                        params![row.id, a.bpm, a.bpm.map(|_| a.bpm_source.as_str())],
                    )?;
                }
            }
            self.refresh_fts(SampleId(row.id))?;
        }
        tx.commit()?;
        Ok(rows.len() as u64)
    }

    /// Fills the missing key/BPM of samples (typically from the factory bank)
    /// with confident detections cached in this library. Hand-set values and
    /// values already present are kept. Returns how many samples changed.
    pub fn fill_from_cache(&self, samples: &mut [Sample]) -> Result<usize> {
        if !self.has_cache_table()? {
            return Ok(0);
        }
        let mut changed = 0;
        for s in samples {
            let wants_key = s.key_root.is_none();
            let wants_bpm = s.bpm.is_none() && s.is_loop();
            if !(wants_key || wants_bpm) {
                continue;
            }
            let Some(d) = s.hash.as_deref().and_then(hash_from_hex).map(|h| self.cached_detection(&h)).transpose()?.flatten()
            else {
                continue;
            };
            let mut hit = false;
            if wants_key && let Some(k) = d.confident_key() {
                s.key_root = Some(k.root);
                s.scale_kind = k.kind;
                s.camelot = k.scale().and_then(Scale::camelot);
                hit = true;
            }
            if wants_bpm && let Some(b) = d.confident_bpm() {
                s.bpm = Some(b);
                hit = true;
            }
            changed += hit as usize;
        }
        Ok(changed)
    }
}

fn row_to_detection(r: &rusqlite::Row<'_>) -> rusqlite::Result<Detection> {
    let root: Option<i64> = r.get(0)?;
    let kind: Option<String> = r.get(1)?;
    let key_conf: Option<f64> = r.get(2)?;
    let bpm: Option<f64> = r.get(3)?;
    let bpm_conf: Option<f64> = r.get(4)?;
    Ok(Detection {
        key: root.map(|k| {
            (KeyHint { root: Key(k as u8), kind: kind.as_deref().and_then(parse_scale_kind) }, key_conf.unwrap_or(0.0) as f32)
        }),
        bpm: bpm.map(|b| (b as f32, bpm_conf.unwrap_or(0.0) as f32)),
        is_loop: r.get(5)?,
    })
}

/// Groups writes into transactions of a few hundred rows on the cache and,
/// when it is written, the source.
struct Batch<'a> {
    conns: Vec<&'a rusqlite::Connection>,
    n: usize,
}

impl<'a> Batch<'a> {
    fn begin(source: &'a Library, cache: &'a Library, same: bool, write_source: bool) -> Result<Self> {
        let mut conns = vec![&cache.conn];
        if write_source && !same {
            conns.push(&source.conn);
        }
        for c in &conns {
            c.execute_batch("BEGIN")?;
        }
        Ok(Self { conns, n: 0 })
    }

    fn tick(&mut self) -> Result<()> {
        self.n += 1;
        if self.n >= 200 {
            for c in &self.conns {
                c.execute_batch("COMMIT; BEGIN")?;
            }
            self.n = 0;
        }
        Ok(())
    }

    fn commit(self) -> Result<()> {
        for c in &self.conns {
            c.execute_batch("COMMIT")?;
        }
        Ok(())
    }
}

/// A database to re-analyse in a [`ReanalyseJob`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReanalyseSource {
    pub db: PathBuf,
    /// Write results into its rows. A source that is not writable is opened
    /// read-only and only feeds the cache.
    pub writable: bool,
}

/// Messages from a background re-analysis.
#[derive(Clone, Debug)]
pub enum ReanalyseEvent {
    Progress(ReanalyseProgress),
    Finished(ReanalyseStats),
    Failed(String),
}

/// Re-analysis on its own thread with its own connections.
pub struct ReanalyseJob {
    pub events: Receiver<ReanalyseEvent>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<ReanalyseStats>>>,
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

impl ReanalyseJob {
    /// The app's job: the factory bank (read-only) and the user library,
    /// both cached in the user library, whose own rows get the results.
    pub fn spawn_library(
        factory: Option<PathBuf>,
        user: PathBuf,
        detector: Arc<dyn KeyBpmDetector>,
        opts: ReanalyseOptions,
    ) -> Self {
        let mut sources: Vec<ReanalyseSource> =
            factory.into_iter().map(|db| ReanalyseSource { db, writable: false }).collect();
        sources.push(ReanalyseSource { db: user.clone(), writable: true });
        Self::spawn(sources, user, detector, opts)
    }

    /// Re-analyses `sources` in order, caching in `cache_db` (which may be
    /// one of the sources). `opts.write_samples` is set per source from
    /// [`ReanalyseSource::writable`].
    pub fn spawn(
        sources: Vec<ReanalyseSource>,
        cache_db: PathBuf,
        detector: Arc<dyn KeyBpmDetector>,
        opts: ReanalyseOptions,
    ) -> Self {
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let handle = std::thread::Builder::new()
            .name("aitrack-library-reanalyse".into())
            .spawn(move || {
                let run = || -> Result<ReanalyseStats> {
                    let cache = Library::open(&cache_db)?;
                    let mut total = ReanalyseStats::default();
                    for (idx, src) in sources.iter().enumerate() {
                        if c.load(Ordering::Relaxed) {
                            total.cancelled = true;
                            break;
                        }
                        let opts = ReanalyseOptions { write_samples: src.writable, ..opts.clone() };
                        let mut last = Instant::now() - Duration::from_secs(1);
                        let mut on_progress = |p: &ReanalyseProgress| {
                            if p.current.is_none() || last.elapsed() >= Duration::from_millis(50) {
                                last = Instant::now();
                                let _ = tx.send(ReanalyseEvent::Progress(ReanalyseProgress { source: idx, ..p.clone() }));
                            }
                        };
                        let stats = if same_file(&src.db, &cache_db) {
                            cache.reanalyse(&cache, detector.as_ref(), &opts, &mut on_progress, &c)?
                        } else {
                            if !src.db.is_file() {
                                return Err(LibraryError::NotFound(src.db.display().to_string()));
                            }
                            let lib =
                                if src.writable { Library::open(&src.db)? } else { Library::open_read_only(&src.db)? };
                            lib.reanalyse(&cache, detector.as_ref(), &opts, &mut on_progress, &c)?
                        };
                        total.add(&stats);
                    }
                    Ok(total)
                };
                let res = run();
                let _ = tx.send(match &res {
                    Ok(s) => ReanalyseEvent::Finished(s.clone()),
                    Err(e) => ReanalyseEvent::Failed(e.to_string()),
                });
                res
            })
            .expect("spawn re-analysis thread");
        Self { events: rx, cancel, handle: Some(handle) }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    pub fn join(mut self) -> Result<ReanalyseStats> {
        match self.handle.take() {
            Some(h) => h.join().map_err(|_| LibraryError::Other("re-analysis thread panicked".into()))?,
            None => Err(LibraryError::Other("already joined".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::BasicAnalyzer;
    use crate::db::{PackInfo, RootKind};
    use crate::scan::ScanOptions;
    use crate::test_util::{write_clicks, write_wav};
    use aitrack_model::ScaleKind;
    use std::sync::atomic::AtomicU32;

    /// Says every sine is A minor and every click track a 120 BPM loop,
    /// counting how often it is asked.
    struct Fake {
        calls: AtomicU32,
    }

    impl KeyBpmDetector for Fake {
        fn id(&self) -> &str {
            "fake"
        }
        fn version(&self) -> u32 {
            1
        }
        fn detect(&self, path: &Path) -> Result<Detection> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let name = path.file_name().unwrap().to_string_lossy();
            if name.contains("broken") {
                return Err(LibraryError::Audio("bad".into()));
            }
            Ok(if name.contains("Click") {
                Detection { key: None, bpm: Some((120.0, 0.9)), is_loop: true }
            } else {
                Detection { key: Some((KeyHint { root: Key(9), kind: Some(ScaleKind::Minor) }, 0.8)), bpm: None, is_loop: false }
            })
        }
    }

    fn fixtures() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("Samples");
        std::fs::create_dir_all(root.join("Pack")).unwrap();
        write_wav(&root.join("Pack/Pad Sine.wav"), 22_050, 1, 2.0, 0.5);
        write_clicks(&root.join("Pack/Click Groove.wav"), 22_050, 120.0, 8);
        // Too short for either: never a candidate.
        write_wav(&root.join("Pack/Blip.wav"), 22_050, 1, 0.2, 0.5);
        // Already keyed and tempo'd by its name: not a candidate either.
        write_wav(&root.join("Pack/Lead Loop Am 120bpm.wav"), 22_050, 1, 4.0, 0.5);
        (dir, root)
    }

    fn scanned(dbp: &Path, root: &Path) -> Library {
        let lib = Library::open(dbp).unwrap();
        lib.add_root(root, RootKind::User).unwrap();
        lib.scan(&BasicAnalyzer, &ScanOptions::default(), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        lib
    }

    fn opts(write: bool) -> ReanalyseOptions {
        ReanalyseOptions { threads: 2, write_samples: write, limit: None }
    }

    #[test]
    fn fills_missing_values_and_caches() {
        let (dir, root) = fixtures();
        let lib = scanned(&dir.path().join("lib.db"), &root);
        let fake = Fake { calls: AtomicU32::new(0) };
        let mut last = ReanalyseProgress::default();
        let stats = lib.reanalyse(&lib, &fake, &opts(true), &mut |p| last = p.clone(), &AtomicBool::new(false)).unwrap();
        assert_eq!(stats.candidates, 2, "{stats:?}");
        assert_eq!(stats.analysed, 2);
        assert_eq!(stats.cached, 0);
        assert_eq!((stats.keys_written, stats.bpms_written), (1, 1));
        assert_eq!((last.done, last.total), (2, 2));
        assert_eq!(fake.calls.load(Ordering::Relaxed), 2);

        let pad = lib.find_by_path(&root.join("Pack/Pad Sine.wav")).unwrap().unwrap();
        assert_eq!(pad.key_label().as_deref(), Some("A Minor"));
        assert_eq!(pad.camelot.as_deref(), Some("8A"));
        let groove = lib.find_by_path(&root.join("Pack/Click Groove.wav")).unwrap().unwrap();
        assert_eq!(groove.bpm, Some(120.0));
        let src: String = lib
            .connection()
            .query_row("SELECT bpm_source FROM samples WHERE id=?1", [groove.id.0], |r| r.get(0))
            .unwrap();
        assert_eq!(src, "audio");
        // The FTS row knows the new key.
        let hits: i64 = lib
            .connection()
            .query_row("SELECT COUNT(*) FROM samples_fts WHERE samples_fts MATCH '\"A Minor\"'", [], |r| r.get(0))
            .unwrap();
        assert!(hits >= 1);

        // Second pass: the pad has a key now but still no tempo, the groove
        // (a click loop, no key) still lacks a key; both come from the cache.
        let again = lib.reanalyse(&lib, &fake, &opts(true), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(again.analysed, 0, "{again:?}");
        assert_eq!(again.cached, again.candidates);
        assert_eq!(fake.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn user_values_are_kept_and_failures_cached() {
        let (dir, root) = fixtures();
        // Decodes fine for the scanner, fails in the detector.
        write_wav(&root.join("Pack/Strings broken.wav"), 22_050, 1, 2.0, 0.5);
        let lib = scanned(&dir.path().join("lib.db"), &root);
        let pad = lib.find_by_path(&root.join("Pack/Pad Sine.wav")).unwrap().unwrap();
        // "Atonal" by hand: not a key candidate any more.
        lib.set_key(pad.id, None).unwrap();
        let fake = Fake { calls: AtomicU32::new(0) };
        let stats = lib.reanalyse(&lib, &fake, &opts(true), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        // The pad is a one-shot, so it is not asked for a tempo either; the
        // groove and the strings still are.
        assert_eq!(stats.candidates, 2, "{stats:?}");
        assert_eq!(stats.errors, 1);
        assert_eq!(stats.keys_written, 0);
        let pad = lib.get(pad.id).unwrap();
        assert_eq!(pad.key_root, None);
        // A failed file is not decoded again until it changes.
        let again = lib.reanalyse(&lib, &fake, &opts(true), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!((again.analysed, again.errors), (0, 0), "{again:?}");
        assert_eq!(fake.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn read_only_source_feeds_the_user_cache() {
        let (dir, root) = fixtures();
        let factory_db = dir.path().join("factory.db");
        drop(scanned(&factory_db, &root));
        let before = std::fs::read(&factory_db).unwrap();
        let user_db = dir.path().join("user.db");
        drop(Library::open(&user_db).unwrap());

        let fake: Arc<dyn KeyBpmDetector> = Arc::new(Fake { calls: AtomicU32::new(0) });
        let job = ReanalyseJob::spawn_library(Some(factory_db.clone()), user_db.clone(), fake.clone(), opts(false));
        let stats = job.join().unwrap();
        assert_eq!(stats.candidates, 2, "{stats:?}");
        assert_eq!(stats.keys_found, 1);
        assert_eq!(stats.keys_written, 0);
        assert_eq!(std::fs::read(&factory_db).unwrap(), before, "factory.db must not change");

        // The app overlays the cached results on factory rows.
        let factory = Library::open_read_only(&factory_db).unwrap();
        let mut rows = vec![factory.find_by_path(&root.join("Pack/Pad Sine.wav")).unwrap().unwrap()];
        assert_eq!(rows[0].key_root, None);
        let user = Library::open(&user_db).unwrap();
        assert_eq!(user.fill_from_cache(&mut rows).unwrap(), 1);
        assert_eq!(rows[0].key_label().as_deref(), Some("A Minor"));

        // A second job answers from the cache.
        let fake2 = Arc::new(Fake { calls: AtomicU32::new(0) });
        let stats = ReanalyseJob::spawn_library(Some(factory_db), user_db, fake2.clone(), opts(false)).join().unwrap();
        assert_eq!((stats.analysed, stats.cached), (0, 2));
        assert_eq!(fake2.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cancel_keeps_finished_work() {
        let (dir, root) = fixtures();
        let lib = scanned(&dir.path().join("lib.db"), &root);
        let fake = Fake { calls: AtomicU32::new(0) };
        let stats = lib.reanalyse(&lib, &fake, &opts(true), &mut |_| {}, &AtomicBool::new(true)).unwrap();
        assert!(stats.cancelled);
        assert_eq!(stats.analysed, 0);
        assert_eq!(fake.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn opening_an_old_library_retags_it_and_keeps_user_edits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("packs");
        let samples = root.join("karoryfer-string-cyborgs/Samples/ironface");
        std::fs::create_dir_all(&samples).unwrap();
        std::fs::create_dir_all(root.join("sonic-pi-samples")).unwrap();
        write_wav(&samples.join("a0_oscillator_tuned.wav"), 22_050, 1, 1.93, 0.5);
        write_wav(&samples.join("b0_oscillator_tuned.wav"), 22_050, 1, 1.93, 0.5);
        write_wav(&root.join("sonic-pi-samples/loop_mika.wav"), 22_050, 1, 8.0, 0.5);
        write_wav(&root.join("sonic-pi-samples/bd_haus.wav"), 22_050, 1, 0.3, 0.5);
        let dbp = dir.path().join("factory.db");
        let (osc, other, lp, kick) = {
            let lib = Library::open(&dbp).unwrap();
            lib.add_root(&root, RootKind::Factory).unwrap();
            lib.upsert_pack(&PackInfo {
                id: "karoryfer-string-cyborgs".into(),
                name: "String Cyborgs".into(),
                genres: vec!["Ambient".into(), "Cinematic".into(), "EDM".into()],
                default_category: Some(Category::Synths),
                default_subcategory: Some("Pads".into()),
                factory: true,
                ..Default::default()
            })
            .unwrap();
            lib.scan(&BasicAnalyzer, &ScanOptions::default(), &mut |_| {}, &AtomicBool::new(false)).unwrap();
            let get = |p: &Path| lib.find_by_path(p).unwrap().unwrap().id;
            let ids = (
                get(&samples.join("a0_oscillator_tuned.wav")),
                get(&samples.join("b0_oscillator_tuned.wav")),
                get(&root.join("sonic-pi-samples/loop_mika.wav")),
                get(&root.join("sonic-pi-samples/bd_haus.wav")),
            );
            // What the old tagger stored: the pack's genres on every file, the
            // pack folder read as "strings", a detected tempo on a one-shot,
            // no attack counts.
            let c = lib.connection();
            c.execute_batch(
                "UPDATE samples SET genres='|Ambient|Cinematic|EDM|', category='Instruments',
                   subcategory='Strings', label_source='filename', onsets=NULL
                   WHERE rel_path LIKE 'karoryfer%';
                 UPDATE samples SET genres='|Techno|House|Drum & Bass|Ambient|', onsets=NULL
                   WHERE rel_path LIKE 'sonic%';
                 UPDATE samples SET bpm=120.3, bpm_source='audio' WHERE rel_path LIKE '%a0_osc%';
                 UPDATE samples SET bpm=120.0, bpm_source='audio' WHERE rel_path LIKE '%loop_mika%';
                 DELETE FROM meta WHERE key='tagger_version';
                 PRAGMA user_version = 2;",
            )
            .unwrap();
            lib.set_favorite(ids.0, true).unwrap();
            lib.set_rating(ids.0, 4).unwrap();
            lib.set_user_tags(ids.0, &["dark".into()]).unwrap();
            lib.set_category(ids.1, Some(Category::Instruments), Some("Strings")).unwrap();
            ids
        };

        let lib = Library::open(&dbp).unwrap();
        let o = lib.get(osc).unwrap();
        assert_eq!((o.category, o.subcategory.as_deref()), (Some(Category::Synths), Some("Pads")));
        assert!(o.genres.is_empty(), "{:?}", o.genres);
        assert_eq!(o.bpm, None, "no detected tempo on a one-shot");
        assert!(o.favorite);
        assert_eq!(o.rating, 4);
        assert_eq!(o.user_tags, vec!["dark".to_string()]);
        // A category set by hand stays, the genres are still fixed.
        let other = lib.get(other).unwrap();
        assert_eq!(other.subcategory.as_deref(), Some("Strings"));
        assert!(other.genres.is_empty());
        let lp = lib.get(lp).unwrap();
        assert!(lp.is_loop());
        assert!(lp.genres.is_empty(), "{:?}", lp.genres);
        assert_eq!(lp.bpm, Some(120.0), "a detected tempo on a loop stays");
        let kick = lib.get(kick).unwrap();
        assert_eq!(kick.subcategory.as_deref(), Some("Kicks"));
        assert!(!kick.is_loop());
        // The full-text index follows: the old genres are not searchable any more.
        let hits: i64 = lib
            .connection()
            .query_row("SELECT COUNT(*) FROM samples_fts WHERE samples_fts MATCH 'tags:Cinematic'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hits, 0);
        assert_eq!(lib.meta("tagger_version").unwrap(), Some(TAGGER_VERSION.to_string()));
        let v: i32 = lib.connection().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, crate::db::SCHEMA_VERSION);
    }

    #[test]
    fn hex_round_trip() {
        let h = [0xabu8; 32];
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hash_from_hex(&hex), Some(h));
        assert_eq!(hash_from_hex("zz"), None);
    }
}
