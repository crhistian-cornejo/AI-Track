//! The SQLite index: schema, rows and basic CRUD.

use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aitrack_model::{Key, Scale, ScaleKind};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::analyze::Analysis;
use crate::error::{LibraryError, Result};
use crate::parse::KeyHint;
use crate::taxonomy::{Category, SampleKind};

pub const SCHEMA_VERSION: i32 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS roots(
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL DEFAULT 'user',
  watch INTEGER NOT NULL DEFAULT 1,
  added_at INTEGER NOT NULL,
  last_scan INTEGER);
CREATE TABLE IF NOT EXISTS packs(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  author TEXT,
  homepage TEXT,
  license TEXT,
  license_url TEXT,
  attribution TEXT,
  genres TEXT NOT NULL DEFAULT '',
  default_category TEXT,
  default_subcategory TEXT,
  factory INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS samples(
  id INTEGER PRIMARY KEY,
  root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
  rel_path TEXT NOT NULL,
  name TEXT NOT NULL,
  ext TEXT NOT NULL DEFAULT '',
  hash BLOB,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'ok',
  error TEXT,
  duration REAL,
  sample_rate INTEGER,
  channels INTEGER,
  peak_db REAL,
  loudness REAL,
  loudness_kind TEXT,
  bpm REAL,
  bpm_source TEXT,
  key_root INTEGER,
  scale_kind TEXT,
  camelot TEXT,
  key_source TEXT,
  root_note INTEGER,
  kind TEXT NOT NULL DEFAULT 'one_shot',
  is_loop INTEGER NOT NULL DEFAULT 0,
  loop_family TEXT,
  category TEXT,
  subcategory TEXT,
  label_source TEXT,
  genres TEXT NOT NULL DEFAULT '',
  pack TEXT,
  license TEXT,
  favorite INTEGER NOT NULL DEFAULT 0,
  rating INTEGER NOT NULL DEFAULT 0,
  user_tags TEXT NOT NULL DEFAULT '',
  analyzer TEXT,
  analysis_version INTEGER NOT NULL DEFAULT 0,
  embedding BLOB,
  embedding_model TEXT,
  added_at INTEGER NOT NULL,
  last_used INTEGER,
  use_count INTEGER NOT NULL DEFAULT 0,
  UNIQUE(root_id, rel_path));
CREATE INDEX IF NOT EXISTS samples_cat ON samples(category, subcategory);
CREATE INDEX IF NOT EXISTS samples_key ON samples(key_root, scale_kind);
CREATE INDEX IF NOT EXISTS samples_camelot ON samples(camelot);
CREATE INDEX IF NOT EXISTS samples_bpm ON samples(bpm);
CREATE INDEX IF NOT EXISTS samples_pack ON samples(pack);
CREATE INDEX IF NOT EXISTS samples_hash ON samples(hash);
CREATE INDEX IF NOT EXISTS samples_used ON samples(last_used);
CREATE VIRTUAL TABLE IF NOT EXISTS samples_fts USING fts5(
  name, path, tags, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE IF NOT EXISTS presets(
  id INTEGER PRIMARY KEY,
  root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
  rel_path TEXT NOT NULL,
  name TEXT NOT NULL,
  format TEXT NOT NULL,
  pack TEXT,
  category TEXT,
  subcategory TEXT,
  mtime INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'ok',
  UNIQUE(root_id, rel_path));
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SampleId(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RootId(pub i64);

/// Where a root came from. Factory roots ship with the app (read-mostly).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RootKind {
    User,
    Factory,
}

impl RootKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Factory => "factory",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Root {
    pub id: RootId,
    /// Stored path (relative paths are relative to the database's folder).
    pub stored_path: String,
    /// Absolute path on this machine.
    pub path: PathBuf,
    pub kind: RootKind,
    pub watch: bool,
    pub last_scan: Option<i64>,
}

/// Pack metadata (from the sound bank manifest or derived from a folder name).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PackInfo {
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub license_url: Option<String>,
    pub attribution: Option<String>,
    pub genres: Vec<String>,
    pub default_category: Option<Category>,
    pub default_subcategory: Option<String>,
    pub factory: bool,
}

/// One indexed sample.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub id: SampleId,
    pub root_id: RootId,
    /// Absolute path on this machine.
    pub path: PathBuf,
    /// Path relative to the root, `/`-separated.
    pub rel_path: String,
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub hash: Option<String>,
    pub duration: Option<f64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub peak_db: Option<f32>,
    pub loudness: Option<f32>,
    pub bpm: Option<f32>,
    pub key_root: Option<Key>,
    pub scale_kind: Option<ScaleKind>,
    pub camelot: Option<String>,
    pub root_note: Option<u8>,
    pub kind: SampleKind,
    pub loop_family: Option<String>,
    pub category: Option<Category>,
    pub subcategory: Option<String>,
    pub genres: Vec<String>,
    pub pack: Option<String>,
    pub license: Option<String>,
    pub favorite: bool,
    pub rating: u8,
    pub user_tags: Vec<String>,
    pub analysis_version: u32,
    pub last_used: Option<i64>,
}

impl Sample {
    pub fn is_loop(&self) -> bool {
        self.kind == SampleKind::Loop
    }

    pub fn scale(&self) -> Option<Scale> {
        Some(Scale::new(self.key_root?, self.scale_kind?))
    }

    /// Human label like "A Minor", "F#" (root only) or `None` (atonal).
    pub fn key_label(&self) -> Option<String> {
        match (self.key_root, self.scale_kind) {
            (Some(r), Some(k)) => Some(Scale::new(r, k).display()),
            (Some(r), None) => Some(r.name().to_string()),
            _ => None,
        }
    }
}

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub(crate) fn scale_kind_str(k: ScaleKind) -> &'static str {
    match k {
        ScaleKind::Major => "major",
        ScaleKind::Minor => "minor",
        ScaleKind::HarmonicMinor => "harmonic_minor",
        ScaleKind::MelodicMinor => "melodic_minor",
        ScaleKind::Dorian => "dorian",
        ScaleKind::Phrygian => "phrygian",
        ScaleKind::Lydian => "lydian",
        ScaleKind::Mixolydian => "mixolydian",
        ScaleKind::Locrian => "locrian",
        ScaleKind::MajorPentatonic => "major_pentatonic",
        ScaleKind::MinorPentatonic => "minor_pentatonic",
        ScaleKind::Blues => "blues",
        ScaleKind::Chromatic => "chromatic",
    }
}

pub(crate) fn parse_scale_kind(s: &str) -> Option<ScaleKind> {
    ScaleKind::ALL.into_iter().find(|k| scale_kind_str(*k) == s)
}

/// `|a|b|` list encoding: exact-match filtering with `LIKE '%|a|%'`.
pub(crate) fn encode_list<S: AsRef<str>>(items: &[S]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut s = String::from("|");
    for i in items {
        s.push_str(&i.as_ref().replace('|', "/"));
        s.push('|');
    }
    s
}

pub(crate) fn decode_list(s: &str) -> Vec<String> {
    s.split('|').filter(|x| !x.is_empty()).map(str::to_string).collect()
}

/// Columns selected for a [`Sample`], in [`Library::row_to_sample`] order.
pub(crate) const SAMPLE_COLS: &str = "s.id, s.root_id, s.rel_path, s.name, s.size, s.mtime, s.hash, \
    s.duration, s.sample_rate, s.channels, s.peak_db, s.loudness, s.bpm, s.key_root, s.scale_kind, \
    s.camelot, s.root_note, s.kind, s.loop_family, s.category, s.subcategory, s.genres, s.pack, \
    s.license, s.favorite, s.rating, s.user_tags, s.analysis_version, s.last_used, r.path";

/// Normalizes a relative path to `/` separators.
pub(crate) fn rel_string(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The smart library index.
pub struct Library {
    pub(crate) conn: Connection,
    db_path: Option<PathBuf>,
}

impl Library {
    /// Opens (or creates) a library database file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let lib = Self { conn, db_path: Some(std::path::absolute(path)?) };
        lib.init()?;
        Ok(lib)
    }

    /// In-memory database (tests, previews). Background scans need a file.
    pub fn open_in_memory() -> Result<Self> {
        let lib = Self { conn: Connection::open_in_memory()?, db_path: None };
        lib.init()?;
        Ok(lib)
    }

    fn init(&self) -> Result<()> {
        self.conn.busy_timeout(std::time::Duration::from_secs(10))?;
        if self.db_path.is_some() {
            self.conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        self.conn.pragma_update(None, "synchronous", "NORMAL")?;
        self.conn.execute_batch(SCHEMA)?;
        let v: i32 = self.conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if v < SCHEMA_VERSION {
            self.conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(())
    }

    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    /// Raw connection, for the app's own queries.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    fn base_dir(&self) -> PathBuf {
        self.db_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub(crate) fn resolve_root(&self, stored: &str) -> PathBuf {
        let p = PathBuf::from(stored);
        if p.is_absolute() { p } else { self.base_dir().join(p) }
    }

    // -- roots --------------------------------------------------------------

    /// Adds a folder to index. A relative path is stored relative to the
    /// database folder (used by the portable factory library).
    pub fn add_root(&self, path: impl AsRef<Path>, kind: RootKind) -> Result<RootId> {
        let stored = path.as_ref().to_string_lossy().replace('\\', "/");
        self.conn.execute(
            "INSERT INTO roots(path, kind, added_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET kind = excluded.kind",
            params![stored, kind.as_str(), now()],
        )?;
        let id = self
            .conn
            .query_row("SELECT id FROM roots WHERE path = ?1", [&stored], |r| r.get(0))?;
        Ok(RootId(id))
    }

    pub fn remove_root(&self, id: RootId) -> Result<()> {
        self.conn
            .execute("DELETE FROM samples_fts WHERE rowid IN (SELECT id FROM samples WHERE root_id = ?1)", [id.0])?;
        self.conn.execute("DELETE FROM roots WHERE id = ?1", [id.0])?;
        Ok(())
    }

    pub fn set_root_watch(&self, id: RootId, watch: bool) -> Result<()> {
        self.conn.execute("UPDATE roots SET watch = ?2 WHERE id = ?1", params![id.0, watch])?;
        Ok(())
    }

    pub fn roots(&self) -> Result<Vec<Root>> {
        let mut st = self
            .conn
            .prepare("SELECT id, path, kind, watch, last_scan FROM roots ORDER BY id")?;
        let rows = st.query_map([], |r| {
            let stored: String = r.get(1)?;
            let kind: String = r.get(2)?;
            Ok((r.get::<_, i64>(0)?, stored, kind, r.get::<_, bool>(3)?, r.get::<_, Option<i64>>(4)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, stored, kind, watch, last_scan) = row?;
            out.push(Root {
                id: RootId(id),
                path: self.resolve_root(&stored),
                stored_path: stored,
                kind: if kind == "factory" { RootKind::Factory } else { RootKind::User },
                watch,
                last_scan,
            });
        }
        Ok(out)
    }

    pub fn root(&self, id: RootId) -> Result<Root> {
        self.roots()?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| LibraryError::NotFound(format!("root {}", id.0)))
    }

    /// The root containing `path`, if any (longest match wins).
    pub fn root_for_path(&self, path: &Path) -> Result<Option<Root>> {
        Ok(self
            .roots()?
            .into_iter()
            .filter(|r| path.starts_with(&r.path))
            .max_by_key(|r| r.path.as_os_str().len()))
    }

    // -- packs --------------------------------------------------------------

    pub fn upsert_pack(&self, p: &PackInfo) -> Result<()> {
        self.conn.execute(
            "INSERT INTO packs(id, name, author, homepage, license, license_url, attribution, genres,
                               default_category, default_subcategory, factory)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, author = excluded.author,
               homepage = excluded.homepage, license = excluded.license,
               license_url = excluded.license_url, attribution = excluded.attribution,
               genres = excluded.genres, default_category = excluded.default_category,
               default_subcategory = excluded.default_subcategory, factory = excluded.factory",
            params![
                p.id,
                p.name,
                p.author,
                p.homepage,
                p.license,
                p.license_url,
                p.attribution,
                encode_list(&p.genres),
                p.default_category.map(Category::as_str),
                p.default_subcategory,
                p.factory
            ],
        )?;
        Ok(())
    }

    pub fn pack(&self, id: &str) -> Result<Option<PackInfo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, author, homepage, license, license_url, attribution, genres,
                        default_category, default_subcategory, factory FROM packs WHERE id = ?1",
                [id],
                row_to_pack,
            )
            .optional()?)
    }

    /// All packs with their sample counts.
    pub fn packs(&self) -> Result<Vec<(PackInfo, u64)>> {
        let mut st = self.conn.prepare(
            "SELECT p.id, p.name, p.author, p.homepage, p.license, p.license_url, p.attribution, p.genres,
                    p.default_category, p.default_subcategory, p.factory,
                    (SELECT COUNT(*) FROM samples s WHERE s.pack = p.id AND s.status = 'ok')
             FROM packs p ORDER BY p.name COLLATE NOCASE",
        )?;
        let rows = st.query_map([], |r| Ok((row_to_pack(r)?, r.get::<_, i64>(11)? as u64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -- samples ------------------------------------------------------------

    pub(crate) fn row_to_sample(&self, r: &Row<'_>) -> rusqlite::Result<Sample> {
        let rel: String = r.get(2)?;
        let root_path: String = r.get(29)?;
        let hash: Option<Vec<u8>> = r.get(6)?;
        let key_root: Option<i64> = r.get(13)?;
        let scale: Option<String> = r.get(14)?;
        let kind: String = r.get(17)?;
        let cat: Option<String> = r.get(19)?;
        let genres: String = r.get(21)?;
        let tags: String = r.get(26)?;
        Ok(Sample {
            id: SampleId(r.get(0)?),
            root_id: RootId(r.get(1)?),
            path: self.resolve_root(&root_path).join(&rel),
            rel_path: rel,
            name: r.get(3)?,
            size: r.get::<_, i64>(4)? as u64,
            mtime: r.get(5)?,
            hash: hash.map(|h| h.iter().map(|b| format!("{b:02x}")).collect()),
            duration: r.get(7)?,
            sample_rate: r.get::<_, Option<i64>>(8)?.map(|v| v as u32),
            channels: r.get::<_, Option<i64>>(9)?.map(|v| v as u16),
            peak_db: r.get::<_, Option<f64>>(10)?.map(|v| v as f32),
            loudness: r.get::<_, Option<f64>>(11)?.map(|v| v as f32),
            bpm: r.get::<_, Option<f64>>(12)?.map(|v| v as f32),
            key_root: key_root.map(|k| Key(k as u8)),
            scale_kind: scale.as_deref().and_then(parse_scale_kind),
            camelot: r.get(15)?,
            root_note: r.get::<_, Option<i64>>(16)?.map(|v| v as u8),
            kind: SampleKind::parse(&kind).unwrap_or_default(),
            loop_family: r.get(18)?,
            category: cat.as_deref().and_then(Category::parse),
            subcategory: r.get(20)?,
            genres: decode_list(&genres),
            pack: r.get(22)?,
            license: r.get(23)?,
            favorite: r.get(24)?,
            rating: r.get::<_, i64>(25)? as u8,
            user_tags: decode_list(&tags),
            analysis_version: r.get::<_, i64>(27)? as u32,
            last_used: r.get(28)?,
        })
    }

    pub fn get(&self, id: SampleId) -> Result<Sample> {
        let sql = format!("SELECT {SAMPLE_COLS} FROM samples s JOIN roots r ON r.id = s.root_id WHERE s.id = ?1");
        self.conn
            .query_row(&sql, [id.0], |r| self.row_to_sample(r))
            .optional()?
            .ok_or_else(|| LibraryError::NotFound(format!("sample {}", id.0)))
    }

    /// Looks a sample up by absolute path.
    pub fn find_by_path(&self, path: &Path) -> Result<Option<Sample>> {
        let Some(root) = self.root_for_path(path)? else { return Ok(None) };
        let rel = rel_string(path.strip_prefix(&root.path).unwrap_or(path));
        let sql = format!(
            "SELECT {SAMPLE_COLS} FROM samples s JOIN roots r ON r.id = s.root_id
             WHERE s.root_id = ?1 AND s.rel_path = ?2"
        );
        Ok(self.conn.query_row(&sql, params![root.id.0, rel], |r| self.row_to_sample(r)).optional()?)
    }

    pub fn count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM samples WHERE status = 'ok'", [], |r| r.get::<_, i64>(0))?
            as u64)
    }

    /// Inserts or updates a sample from a fresh analysis. Fields the user set
    /// by hand (`label_source = 'user'`, `key_source = 'user'`) survive.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upsert_analysis(
        &self,
        root_id: RootId,
        rel: &str,
        size: u64,
        mtime: i64,
        hash: Option<&[u8; 32]>,
        a: &Analysis,
        pack: Option<&str>,
        license: Option<&str>,
        analyzer: &str,
        version: u32,
    ) -> Result<SampleId> {
        let name = Path::new(rel)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(rel)
            .to_string();
        let ext = Path::new(rel)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let scale = a.key.and_then(KeyHint::scale);
        let camelot = scale.and_then(Scale::camelot);
        let existing: Option<(i64, Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT id, label_source, key_source FROM samples WHERE root_id = ?1 AND rel_path = ?2",
                params![root_id.0, rel],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let keep_label = existing.as_ref().is_some_and(|e| e.1.as_deref() == Some("user"));
        let keep_key = existing.as_ref().is_some_and(|e| e.2.as_deref() == Some("user"));

        let (emb_model, emb_blob) = match &a.embedding {
            Some((m, v)) => (Some(m.as_str()), Some(v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>())),
            None => (None, None),
        };
        let id = if let Some((id, ..)) = existing {
            self.conn.execute(
                "UPDATE samples SET name=?2, ext=?3, hash=?4, size=?5, mtime=?6, status='ok', error=NULL,
                   duration=?7, sample_rate=?8, channels=?9, peak_db=?10, loudness=?11, loudness_kind=?12,
                   bpm=?13, bpm_source=?14, root_note=?15, kind=?16, is_loop=?17, loop_family=?18,
                   genres=?19, pack=?20, license=?21, analyzer=?22, analysis_version=?23,
                   embedding=COALESCE(?24, embedding), embedding_model=COALESCE(?25, embedding_model)
                 WHERE id=?1",
                params![
                    id,
                    name,
                    ext,
                    hash.map(|h| h.as_slice()),
                    size as i64,
                    mtime,
                    a.audio.duration_s,
                    a.audio.sample_rate,
                    a.audio.channels,
                    a.audio.peak_db,
                    a.loudness,
                    a.loudness_kind,
                    a.bpm,
                    a.bpm.map(|_| a.bpm_source.as_str()),
                    a.root_note,
                    a.kind.as_str(),
                    a.kind == SampleKind::Loop,
                    a.loop_family,
                    encode_list(&a.genres),
                    pack,
                    license,
                    analyzer,
                    version,
                    emb_blob,
                    emb_model,
                ],
            )?;
            if !keep_label {
                self.conn.execute(
                    "UPDATE samples SET category=?2, subcategory=?3, label_source=?4 WHERE id=?1",
                    params![id, a.category.map(Category::as_str), a.subcategory, a.category_source.as_str()],
                )?;
            }
            if !keep_key {
                self.conn.execute(
                    "UPDATE samples SET key_root=?2, scale_kind=?3, camelot=?4, key_source=?5 WHERE id=?1",
                    params![
                        id,
                        a.key.map(|k| k.root.0),
                        a.key.and_then(|k| k.kind).map(scale_kind_str),
                        camelot,
                        a.key.map(|_| a.key_source.as_str())
                    ],
                )?;
            }
            id
        } else {
            self.conn.execute(
                "INSERT INTO samples(root_id, rel_path, name, ext, hash, size, mtime, duration, sample_rate,
                   channels, peak_db, loudness, loudness_kind, bpm, bpm_source, key_root, scale_kind, camelot,
                   key_source, root_note, kind, is_loop, loop_family, category, subcategory, label_source,
                   genres, pack, license, analyzer, analysis_version, embedding, embedding_model, added_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,
                        ?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34)",
                params![
                    root_id.0,
                    rel,
                    name,
                    ext,
                    hash.map(|h| h.as_slice()),
                    size as i64,
                    mtime,
                    a.audio.duration_s,
                    a.audio.sample_rate,
                    a.audio.channels,
                    a.audio.peak_db,
                    a.loudness,
                    a.loudness_kind,
                    a.bpm,
                    a.bpm.map(|_| a.bpm_source.as_str()),
                    a.key.map(|k| k.root.0),
                    a.key.and_then(|k| k.kind).map(scale_kind_str),
                    camelot,
                    a.key.map(|_| a.key_source.as_str()),
                    a.root_note,
                    a.kind.as_str(),
                    a.kind == SampleKind::Loop,
                    a.loop_family,
                    a.category.map(Category::as_str),
                    a.subcategory,
                    a.category_source.as_str(),
                    encode_list(&a.genres),
                    pack,
                    license,
                    analyzer,
                    version,
                    emb_blob,
                    emb_model,
                    now(),
                ],
            )?;
            self.conn.last_insert_rowid()
        };
        self.refresh_fts(SampleId(id))?;
        Ok(SampleId(id))
    }

    /// Records a file that could not be decoded, so it is not retried until it changes.
    pub(crate) fn upsert_error(&self, root_id: RootId, rel: &str, size: u64, mtime: i64, err: &str) -> Result<()> {
        let name = Path::new(rel).file_stem().and_then(|s| s.to_str()).unwrap_or(rel);
        self.conn.execute(
            "INSERT INTO samples(root_id, rel_path, name, size, mtime, status, error, added_at)
             VALUES(?1, ?2, ?3, ?4, ?5, 'error', ?6, ?7)
             ON CONFLICT(root_id, rel_path) DO UPDATE SET size=excluded.size, mtime=excluded.mtime,
               status='error', error=excluded.error",
            params![root_id.0, rel, name, size as i64, mtime, err, now()],
        )?;
        self.conn.execute(
            "DELETE FROM samples_fts WHERE rowid = (SELECT id FROM samples WHERE root_id=?1 AND rel_path=?2)",
            params![root_id.0, rel],
        )?;
        Ok(())
    }

    /// Rebuilds the full-text row of one sample.
    pub(crate) fn refresh_fts(&self, id: SampleId) -> Result<()> {
        self.conn.execute("DELETE FROM samples_fts WHERE rowid = ?1", [id.0])?;
        let s = self.get(id)?;
        let name = s.name.replace(['_', '-', '.'], " ");
        let folders = Path::new(&s.rel_path)
            .parent()
            .map(|p| rel_string(p).replace(['/', '_', '-', '.'], " "))
            .unwrap_or_default();
        let mut tags: Vec<String> = Vec::new();
        if let Some(c) = s.category {
            tags.push(c.as_str().into());
        }
        tags.extend(s.subcategory.clone());
        tags.push(match s.kind {
            SampleKind::OneShot => "one shot oneshot".into(),
            SampleKind::Loop => "loop".into(),
            SampleKind::Chop => "chop".into(),
        });
        if let Some(l) = s.key_label() {
            tags.push(l);
        }
        if let (Some(r), Some(k)) = (s.key_root, s.scale_kind) {
            let short = match k {
                ScaleKind::Minor => format!("{}m", r.name()),
                ScaleKind::Major => format!("{}maj", r.name()),
                _ => String::new(),
            };
            tags.push(short);
        }
        tags.extend(s.camelot.clone());
        if let Some(b) = s.bpm {
            tags.push(format!("{}bpm {}", b.round(), b.round()));
        }
        tags.extend(s.genres.iter().cloned());
        tags.extend(s.pack.clone());
        tags.extend(s.user_tags.iter().cloned());
        self.conn.execute(
            "INSERT INTO samples_fts(rowid, name, path, tags) VALUES(?1, ?2, ?3, ?4)",
            params![id.0, name, folders, tags.join(" ")],
        )?;
        Ok(())
    }

    // -- user edits ---------------------------------------------------------

    pub fn set_favorite(&self, id: SampleId, fav: bool) -> Result<()> {
        self.conn.execute("UPDATE samples SET favorite=?2 WHERE id=?1", params![id.0, fav])?;
        Ok(())
    }

    pub fn set_rating(&self, id: SampleId, rating: u8) -> Result<()> {
        self.conn.execute("UPDATE samples SET rating=?2 WHERE id=?1", params![id.0, rating.min(5)])?;
        Ok(())
    }

    pub fn set_user_tags(&self, id: SampleId, tags: &[String]) -> Result<()> {
        self.conn.execute("UPDATE samples SET user_tags=?2 WHERE id=?1", params![id.0, encode_list(tags)])?;
        self.refresh_fts(id)
    }

    /// A manual correction; kept across rescans.
    pub fn set_category(&self, id: SampleId, cat: Option<Category>, sub: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE samples SET category=?2, subcategory=?3, label_source='user' WHERE id=?1",
            params![id.0, cat.map(Category::as_str), sub],
        )?;
        self.refresh_fts(id)
    }

    /// A manual key correction (`None` = atonal); kept across rescans.
    pub fn set_key(&self, id: SampleId, key: Option<KeyHint>) -> Result<()> {
        let camelot = key.and_then(KeyHint::scale).and_then(Scale::camelot);
        self.conn.execute(
            "UPDATE samples SET key_root=?2, scale_kind=?3, camelot=?4, key_source='user' WHERE id=?1",
            params![id.0, key.map(|k| k.root.0), key.and_then(|k| k.kind).map(scale_kind_str), camelot],
        )?;
        self.refresh_fts(id)
    }

    pub fn set_bpm(&self, id: SampleId, bpm: Option<f32>) -> Result<()> {
        self.conn
            .execute("UPDATE samples SET bpm=?2, bpm_source='user' WHERE id=?1", params![id.0, bpm])?;
        self.refresh_fts(id)
    }

    /// Records that the sample was used (drives the "Recent" folder).
    pub fn mark_used(&self, id: SampleId) -> Result<()> {
        self.conn.execute(
            "UPDATE samples SET last_used=?2, use_count=use_count+1 WHERE id=?1",
            params![id.0, now()],
        )?;
        Ok(())
    }

    /// Stores an embedding (e.g. CLAP) as little-endian f32.
    pub fn set_embedding(&self, id: SampleId, model: &str, v: &[f32]) -> Result<()> {
        let blob: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.conn.execute(
            "UPDATE samples SET embedding=?2, embedding_model=?3 WHERE id=?1",
            params![id.0, blob, model],
        )?;
        Ok(())
    }

    pub fn embedding(&self, id: SampleId) -> Result<Option<(String, Vec<f32>)>> {
        let row: Option<(Option<String>, Option<Vec<u8>>)> = self
            .conn
            .query_row("SELECT embedding_model, embedding FROM samples WHERE id=?1", [id.0], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        Ok(match row {
            Some((Some(m), Some(b))) => Some((
                m,
                b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect(),
            )),
            _ => None,
        })
    }

    /// Updates the stored location of a sample (after a move).
    pub(crate) fn relocate(&self, id: SampleId, root_id: RootId, rel: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE samples SET root_id=?2, rel_path=?3 WHERE id=?1",
            params![id.0, root_id.0, rel],
        )?;
        self.refresh_fts(id)
    }

    /// Instrument presets (`.sfz`, `.dspreset`) found while scanning.
    pub fn presets(&self) -> Result<Vec<Preset>> {
        let mut st = self.conn.prepare(
            "SELECT p.id, p.rel_path, p.name, p.format, p.pack, p.category, p.subcategory, r.path
             FROM presets p JOIN roots r ON r.id = p.root_id WHERE p.status = 'ok' ORDER BY p.pack, p.name",
        )?;
        let rows = st.query_map([], |r| {
            let rel: String = r.get(1)?;
            let root: String = r.get(7)?;
            let cat: Option<String> = r.get(5)?;
            Ok(Preset {
                id: r.get(0)?,
                path: self.resolve_root(&root).join(&rel),
                rel_path: rel,
                name: r.get(2)?,
                format: r.get(3)?,
                pack: r.get(4)?,
                category: cat.as_deref().and_then(Category::parse),
                subcategory: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// An instrument preset file (SFZ / Decent Sampler).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: i64,
    pub path: PathBuf,
    pub rel_path: String,
    pub name: String,
    pub format: String,
    pub pack: Option<String>,
    pub category: Option<Category>,
    pub subcategory: Option<String>,
}

fn row_to_pack(r: &Row<'_>) -> rusqlite::Result<PackInfo> {
    let genres: String = r.get(7)?;
    let cat: Option<String> = r.get(8)?;
    Ok(PackInfo {
        id: r.get(0)?,
        name: r.get(1)?,
        author: r.get(2)?,
        homepage: r.get(3)?,
        license: r.get(4)?,
        license_url: r.get(5)?,
        attribution: r.get(6)?,
        genres: decode_list(&genres),
        default_category: cat.as_deref().and_then(Category::parse),
        default_subcategory: r.get(9)?,
        factory: r.get(10)?,
    })
}

