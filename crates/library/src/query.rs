//! Search, key compatibility and project-aware recommendations.

use aitrack_model::{Key, Scale, ScaleKind};
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::db::{Library, RootId, SAMPLE_COLS, Sample, scale_kind_str};
use crate::error::Result;
use crate::parse::from_camelot;
use crate::taxonomy::{Category, SampleKind};

// ---------------------------------------------------------------------------
// Key compatibility
// ---------------------------------------------------------------------------

/// Parses a Camelot code like "8A".
pub fn parse_camelot(code: &str) -> Option<(u8, bool)> {
    let code = code.trim();
    let minor = match code.chars().last()? {
        'A' | 'a' => true,
        'B' | 'b' => false,
        _ => return None,
    };
    let n: u8 = code[..code.len() - 1].parse().ok()?;
    (1..=12).contains(&n).then_some((n, minor))
}

/// Camelot codes harmonically compatible with `code`: itself, the relative
/// (same number, other letter) and ±1 on the same ring.
pub fn camelot_neighbors(code: &str) -> Vec<String> {
    let Some((n, minor)) = parse_camelot(code) else { return Vec::new() };
    let l = if minor { 'A' } else { 'B' };
    let o = if minor { 'B' } else { 'A' };
    let up = n % 12 + 1;
    let down = (n + 10) % 12 + 1;
    vec![format!("{n}{l}"), format!("{n}{o}"), format!("{up}{l}"), format!("{down}{l}")]
}

/// How well a sample's key fits the project scale, 0..=1:
/// atonal 1, same key 1, relative 0.9, Camelot ±1 0.8, parallel 0.5,
/// root-only in scale 0.85 (out of scale 0.3, it can be transposed), else 0.1.
pub fn key_compat(project: Scale, sample_root: Option<Key>, sample_kind: Option<ScaleKind>) -> f32 {
    let Some(root) = sample_root else { return 1.0 };
    let Some(kind) = sample_kind else {
        return if project.mask() & (1 << root.0) != 0 { 0.85 } else { 0.3 };
    };
    let s = Scale::new(root, kind);
    if s == project {
        return 1.0;
    }
    match (s.camelot(), project.camelot()) {
        (Some(a), Some(b)) => {
            let (an, am) = parse_camelot(&a).unwrap_or((0, false));
            let (bn, bm) = parse_camelot(&b).unwrap_or((0, false));
            if a == b {
                1.0
            } else if an == bn {
                0.9
            } else if am == bm && (an % 12 + 1 == bn || bn % 12 + 1 == an) {
                0.8
            } else if root == project.root {
                0.5
            } else {
                0.1
            }
        }
        _ => {
            if root == project.root {
                0.5
            } else if s.mask() & !project.mask() == 0 {
                0.8
            } else {
                0.1
            }
        }
    }
}

/// Semitones (−6..=5) that move `pitch_class` onto the project tonic.
pub fn transpose_to(pitch_class: u8, target: Key) -> i8 {
    let d = (target.0 as i8 - pitch_class as i8).rem_euclid(12);
    if d > 5 { d - 12 } else { d }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum KeyMatch {
    /// Same key and scale only.
    Exact,
    /// Same, relative and Camelot ±1 (plus root-only samples in the scale).
    #[default]
    Compatible,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyFilter {
    pub scale: Scale,
    pub mode: KeyMatch,
    /// Also return samples with no key (drums, FX).
    pub include_atonal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BpmFilter {
    pub bpm: f32,
    /// Tolerance in percent (e.g. 6.0 = ±6 %).
    pub tolerance_pct: f32,
    /// Accept half and double time.
    pub half_double: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Filters {
    pub category: Option<Category>,
    pub subcategory: Option<String>,
    pub kind: Option<SampleKind>,
    pub is_loop: Option<bool>,
    pub loop_family: Option<String>,
    pub key: Option<KeyFilter>,
    /// Exact key/scale (virtual folders). `scale_kind` `None` = root-only samples.
    pub key_exact: Option<(Key, Option<ScaleKind>)>,
    pub camelot: Option<String>,
    pub bpm: Option<BpmFilter>,
    pub bpm_range: Option<(f32, f32)>,
    /// Tempo for loops only (½×, 1× or 2× within the tolerance): one-shots and
    /// loops with no tempo pass, as a one-shot has none to compare.
    pub loop_tempo: Option<BpmFilter>,
    /// Only this collection (a folder the user added, the factory bank…).
    pub root: Option<RootId>,
    /// Only inside this folder of the root, `/`-separated, subfolders included.
    pub under: Option<String>,
    pub genre: Option<String>,
    pub pack: Option<String>,
    pub favorites_only: bool,
    pub min_rating: Option<u8>,
    pub recent_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SortBy {
    /// Text relevance when there is a query, otherwise name.
    #[default]
    Relevance,
    Name,
    Bpm,
    Duration,
    RecentlyUsed,
    RecentlyAdded,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: Option<String>,
    pub filters: Filters,
    pub sort: SortBy,
    /// 0 = default (200).
    pub limit: usize,
    pub offset: usize,
}

impl SearchQuery {
    pub fn text(t: impl Into<String>) -> Self {
        Self { text: Some(t.into()), ..Default::default() }
    }
}

/// Turns user text into a safe FTS5 query: every word is a prefix term.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '#')
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"*", t.replace('"', "")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

pub(crate) struct Where {
    pub sql: Vec<String>,
    pub args: Vec<Value>,
}

impl Where {
    fn push(&mut self, clause: impl Into<String>, args: impl IntoIterator<Item = Value>) {
        self.sql.push(clause.into());
        self.args.extend(args);
    }
}

pub(crate) fn build_where(f: &Filters) -> Where {
    let mut w = Where { sql: vec!["s.status = 'ok'".into()], args: Vec::new() };
    if let Some(c) = f.category {
        w.push("s.category = ?", [Value::Text(c.as_str().into())]);
    }
    if let Some(s) = &f.subcategory {
        w.push("s.subcategory = ?", [Value::Text(s.clone())]);
    }
    if let Some(k) = f.kind {
        w.push("s.kind = ?", [Value::Text(k.as_str().into())]);
    }
    if let Some(l) = f.is_loop {
        w.push("s.is_loop = ?", [Value::Integer(l as i64)]);
    }
    if let Some(fam) = &f.loop_family {
        w.push("s.loop_family = ?", [Value::Text(fam.clone())]);
    }
    if let Some((root, kind)) = f.key_exact {
        match kind {
            Some(k) => w.push(
                "s.key_root = ? AND s.scale_kind = ?",
                [Value::Integer(root.0 as i64), Value::Text(scale_kind_str(k).into())],
            ),
            None => w.push("s.key_root = ? AND s.scale_kind IS NULL", [Value::Integer(root.0 as i64)]),
        }
    }
    if let Some(c) = &f.camelot {
        w.push("s.camelot = ?", [Value::Text(c.clone())]);
    }
    if let Some(kf) = f.key {
        let mut ors: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        let code = kf.scale.camelot();
        match (kf.mode, &code) {
            (KeyMatch::Exact, _) => {
                ors.push("(s.key_root = ? AND s.scale_kind = ?)".into());
                args.push(Value::Integer(kf.scale.root.0 as i64));
                args.push(Value::Text(scale_kind_str(kf.scale.kind).into()));
                if let Some(c) = &code {
                    ors.push("s.camelot = ?".into());
                    args.push(Value::Text(c.clone()));
                }
            }
            (KeyMatch::Compatible, Some(c)) => {
                let codes = camelot_neighbors(c);
                ors.push(format!("s.camelot IN ({})", vec!["?"; codes.len()].join(",")));
                args.extend(codes.into_iter().map(Value::Text));
                ors.push("(s.key_root = ? AND s.scale_kind = ?)".into());
                args.push(Value::Integer(kf.scale.root.0 as i64));
                args.push(Value::Text(scale_kind_str(kf.scale.kind).into()));
            }
            (KeyMatch::Compatible, None) => {
                ors.push("(s.key_root = ? AND s.scale_kind IS NOT NULL)".into());
                args.push(Value::Integer(kf.scale.root.0 as i64));
            }
        }
        if kf.mode == KeyMatch::Compatible {
            // Root-only samples whose note is in the scale.
            let pcs: Vec<i64> = (0..12).filter(|pc| kf.scale.mask() & (1 << pc) != 0).collect();
            ors.push(format!("(s.scale_kind IS NULL AND s.key_root IN ({}))", vec!["?"; pcs.len()].join(",")));
            args.extend(pcs.into_iter().map(Value::Integer));
        }
        if kf.include_atonal {
            ors.push("s.key_root IS NULL".into());
        }
        w.push(format!("({})", ors.join(" OR ")), args);
    }
    let bpm_clause = |lo: f32, hi: f32, half_double: bool| -> (String, Vec<Value>) {
        let mut parts = vec!["s.bpm BETWEEN ? AND ?".to_string()];
        let mut a = vec![Value::Real(lo as f64), Value::Real(hi as f64)];
        if half_double {
            parts.push("s.bpm BETWEEN ? AND ?".into());
            a.extend([Value::Real(lo as f64 / 2.0), Value::Real(hi as f64 / 2.0)]);
            parts.push("s.bpm BETWEEN ? AND ?".into());
            a.extend([Value::Real(lo as f64 * 2.0), Value::Real(hi as f64 * 2.0)]);
        }
        (format!("({})", parts.join(" OR ")), a)
    };
    if let Some(b) = f.bpm {
        let tol = b.bpm * b.tolerance_pct.max(0.0) / 100.0;
        let (c, a) = bpm_clause(b.bpm - tol, b.bpm + tol, b.half_double);
        w.push(c, a);
    }
    if let Some((lo, hi)) = f.bpm_range {
        let (c, a) = bpm_clause(lo, hi, false);
        w.push(c, a);
    }
    if let Some(b) = f.loop_tempo {
        let tol = b.bpm * b.tolerance_pct.max(0.0) / 100.0;
        let (c, a) = bpm_clause(b.bpm - tol, b.bpm + tol, b.half_double);
        w.push(format!("(s.is_loop = 0 OR s.bpm IS NULL OR {c})"), a);
    }
    if let Some(r) = f.root {
        w.push("s.root_id = ?", [Value::Integer(r.0)]);
    }
    if let Some(dir) = &f.under {
        let dir = dir.trim_matches('/');
        if !dir.is_empty() {
            let escaped = dir.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            w.push("s.rel_path LIKE ? ESCAPE '\\'", [Value::Text(format!("{escaped}/%"))]);
        }
    }
    if let Some(g) = &f.genre {
        w.push("s.genres LIKE ?", [Value::Text(format!("%|{g}|%"))]);
    }
    if let Some(p) = &f.pack {
        w.push("s.pack = ?", [Value::Text(p.clone())]);
    }
    if f.favorites_only {
        w.push("s.favorite = 1", []);
    }
    if let Some(r) = f.min_rating {
        w.push("s.rating >= ?", [Value::Integer(r as i64)]);
    }
    if f.recent_only {
        w.push("s.last_used IS NOT NULL", []);
    }
    w
}

/// The folders worth offering as a filter: the first level of `folders`
/// (all the folders of a root with their counts, as [`Library::folders`]
/// gives them) that splits the root into more than one, so `packs/` holding
/// only `packs/Deep House` and `packs/Trap` offers those two. Most sounds
/// first, at most `cap`.
pub fn browse_folders(folders: &[(String, u64)], cap: usize) -> Vec<(String, u64)> {
    let depth = |p: &str| p.matches('/').count() + 1;
    let max = folders.iter().map(|f| depth(&f.0)).max().unwrap_or(0);
    for d in 1..=max {
        let mut level: Vec<(String, u64)> = folders.iter().filter(|f| depth(&f.0) == d).cloned().collect();
        if level.len() > 1 || d == max {
            level.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            level.truncate(cap);
            return level;
        }
    }
    Vec::new()
}

/// A scored recommendation with a human-readable explanation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recommendation {
    pub sample: Sample,
    pub score: f32,
    pub key_score: f32,
    pub tempo_score: f32,
    /// Semitones to transpose a tonal one-shot onto the project tonic, when its key does not fit.
    pub transpose: Option<i8>,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecommendQuery {
    pub scale: Option<Scale>,
    pub bpm: Option<f32>,
    pub category: Option<Category>,
    pub subcategory: Option<String>,
    pub kind: Option<SampleKind>,
    /// 0 = default (50).
    pub limit: usize,
}

/// Tempo fit 0..=1. One-shots are neutral; loops are compared at ½×, 1×, 2×.
pub fn tempo_compat(project_bpm: f32, sample_bpm: Option<f32>, is_loop: bool) -> f32 {
    if !is_loop {
        return 0.8;
    }
    let Some(b) = sample_bpm else { return 0.4 };
    let dev = [0.5f32, 1.0, 2.0]
        .iter()
        .map(|r| ((b * r) - project_bpm).abs() / project_bpm)
        .fold(f32::MAX, f32::min);
    if dev <= 0.005 {
        1.0
    } else if dev <= 0.06 {
        1.0 - dev / 0.06 * 0.3
    } else {
        (0.5 - dev).max(0.0)
    }
}

/// How well `s` answers the typed `words`: name beats folders and tags, a
/// favourite and a sound used before come a little ahead of equals. Rows
/// the database matched through a tag the text does not show (a Camelot
/// code, "124bpm") still count a little.
pub fn relevance(words: &[String], s: &Sample) -> f32 {
    let mut extra: Vec<&str> = vec![&s.rel_path];
    extra.extend(s.pack.as_deref());
    extra.extend(s.category.map(|c| c.as_str()));
    extra.extend(s.subcategory.as_deref());
    extra.extend(s.genres.iter().map(String::as_str));
    extra.extend(s.user_tags.iter().map(String::as_str));
    let text = crate::text::score(words, &s.name, &extra).unwrap_or(0.5);
    text + if s.favorite { 0.5 } else { 0. } + if s.last_used.is_some() { 0.2 } else { 0. }
}

impl Library {
    /// Full-text + filtered search. With text, the best answers come first:
    /// the database ranks the words it finds (a word that starts with the
    /// text), and those are put in order of how much of the name they are;
    /// when there is room left, names that only *contain* the text ("DarkKick"
    /// for "kick") follow, folded the same way, so accents and case never matter.
    pub fn search(&self, q: &SearchQuery) -> Result<Vec<Sample>> {
        let words = crate::text::words(q.text.as_deref().unwrap_or(""));
        let mut out = self.search_fts(q)?;
        if words.is_empty() || q.sort != SortBy::Relevance {
            return Ok(out);
        }
        // Name beats folder beats tag; the database's own order breaks ties (stable).
        let mut scored: Vec<(f32, Sample)> = out.drain(..).map(|s| (relevance(&words, &s), s)).collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        out = scored.into_iter().map(|(_, s)| s).collect();
        let limit = if q.limit == 0 { 200 } else { q.limit };
        // Parts of words only for words of two letters or more, and not for later pages.
        if out.len() < limit && q.offset == 0 && words.iter().all(|w| w.chars().count() >= 2) {
            let have: std::collections::HashSet<i64> = out.iter().map(|s| s.id.0).collect();
            let more = self.search_inside(q, &words, limit - out.len(), &have)?;
            out.extend(more);
        }
        Ok(out)
    }

    fn search_fts(&self, q: &SearchQuery) -> Result<Vec<Sample>> {
        let mut w = build_where(&q.filters);
        let fts = q.text.as_deref().and_then(fts_query);
        let mut from = String::from("samples s JOIN roots r ON r.id = s.root_id");
        if let Some(m) = &fts {
            from.push_str(" JOIN samples_fts ON samples_fts.rowid = s.id");
            w.push("samples_fts MATCH ?", [Value::Text(m.clone())]);
        }
        let order = match q.sort {
            SortBy::Relevance if fts.is_some() => "bm25(samples_fts, 10.0, 2.0, 4.0), s.name COLLATE NOCASE",
            SortBy::Relevance | SortBy::Name => "s.name COLLATE NOCASE",
            SortBy::Bpm => "s.bpm IS NULL, s.bpm, s.name COLLATE NOCASE",
            SortBy::Duration => "s.duration, s.name COLLATE NOCASE",
            SortBy::RecentlyUsed => "s.last_used DESC, s.name COLLATE NOCASE",
            SortBy::RecentlyAdded => "s.added_at DESC, s.id DESC",
        };
        let limit = if q.limit == 0 { 200 } else { q.limit };
        let sql = format!(
            "SELECT {SAMPLE_COLS} FROM {from} WHERE {} ORDER BY {order} LIMIT {limit} OFFSET {}",
            w.sql.join(" AND "),
            q.offset
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(w.args.iter()), |r| self.row_to_sample(r))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Samples whose name or folders contain every word somewhere inside, best
    /// first, up to `room`, leaving out those in `have`. Reads the names of the
    /// rows the filters let through (a scan: tens of milliseconds for a
    /// hundred thousand), which is why it only runs when the indexed search
    /// left room.
    fn search_inside(&self, q: &SearchQuery, words: &[String], room: usize, have: &std::collections::HashSet<i64>) -> Result<Vec<Sample>> {
        let w = build_where(&q.filters);
        let sql = format!(
            "SELECT s.id, f.name, f.path FROM samples s JOIN roots r ON r.id = s.root_id JOIN samples_fts f ON f.rowid = s.id WHERE {}",
            w.sql.join(" AND ")
        );
        let mut st = self.conn.prepare(&sql)?;
        let mut rows = st.query(rusqlite::params_from_iter(w.args.iter()))?;
        let mut hits: Vec<(f32, String, i64)> = Vec::new();
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            if have.contains(&id) {
                continue;
            }
            let name: String = r.get(1)?;
            let folders: String = r.get(2)?;
            let (name_f, folders_f) = (crate::text::fold(&name), crate::text::fold(&folders));
            if words.iter().all(|w| name_f.contains(w.as_str()) || folders_f.contains(w.as_str())) {
                let score = crate::text::score(words, &name, &[&folders]).unwrap_or(0.);
                hits.push((score, name, id));
            }
        }
        hits.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        hits.truncate(room);
        self.samples_by_id(&hits.iter().map(|h| h.2).collect::<Vec<_>>())
    }

    /// The samples with these ids, in the order given (the missing ones skipped).
    pub fn samples_by_id(&self, ids: &[i64]) -> Result<Vec<Sample>> {
        let mut by_id: std::collections::HashMap<i64, Sample> = std::collections::HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(400) {
            let sql = format!(
                "SELECT {SAMPLE_COLS} FROM samples s JOIN roots r ON r.id = s.root_id WHERE s.id IN ({})",
                vec!["?"; chunk.len()].join(",")
            );
            let mut st = self.conn.prepare(&sql)?;
            let rows = st.query_map(rusqlite::params_from_iter(chunk.iter()), |r| self.row_to_sample(r))?;
            for s in rows {
                let s = s?;
                by_id.insert(s.id.0, s);
            }
        }
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// The folders of a root with how many sounds each holds (subfolders
    /// counted in their parents), by path, most sounds first. Sounds at the top
    /// of the root belong to no folder.
    pub fn folders(&self, root: RootId) -> Result<Vec<(String, u64)>> {
        let mut st = self.conn.prepare("SELECT rel_path FROM samples WHERE root_id = ?1 AND status = 'ok'")?;
        let mut rows = st.query([root.0])?;
        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        while let Some(r) = rows.next()? {
            let rel: String = r.get(0)?;
            let mut end = 0;
            while let Some(i) = rel[end..].find('/') {
                end += i;
                *counts.entry(rel[..end].to_owned()).or_default() += 1;
                end += 1;
            }
        }
        let mut out: Vec<(String, u64)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    /// Counts matches for filters (no text).
    pub fn count_where(&self, f: &Filters) -> Result<u64> {
        let w = build_where(f);
        let sql = format!("SELECT COUNT(*) FROM samples s WHERE {}", w.sql.join(" AND "));
        Ok(self
            .conn
            .query_row(&sql, rusqlite::params_from_iter(w.args.iter()), |r| r.get::<_, i64>(0))? as u64)
    }

    /// Samples that fit the project: key compatibility, tempo, favourites.
    /// `score = 0.5·key + 0.35·tempo + 0.1·favourite + 0.05·rating`.
    pub fn recommend(&self, q: &RecommendQuery) -> Result<Vec<Recommendation>> {
        let filters = Filters {
            category: q.category,
            subcategory: q.subcategory.clone(),
            kind: q.kind,
            ..Default::default()
        };
        let candidates = self.search(&SearchQuery { filters, limit: 5000, ..Default::default() })?;
        let mut out: Vec<Recommendation> = candidates
            .into_iter()
            .map(|s| {
                let mut reasons = Vec::new();
                let mut transpose = None;
                let key_score = match q.scale {
                    Some(proj) => {
                        let k = key_compat(proj, s.key_root, s.scale_kind);
                        match (s.key_label(), s.camelot.as_deref()) {
                            (Some(label), cam) => {
                                let rel = match k {
                                    x if x >= 1.0 => "same key as your project",
                                    x if x >= 0.9 => "relative of your project key",
                                    x if x >= 0.85 => "root note in your scale",
                                    x if x >= 0.8 => "neighbour on the Camelot wheel",
                                    x if x >= 0.5 => "parallel key",
                                    _ => "different key",
                                };
                                let cam = cam.map(|c| format!(" ({c})")).unwrap_or_default();
                                reasons.push(format!("{label}{cam}: {rel}"));
                            }
                            (None, _) => reasons.push("no key (fits any project)".into()),
                        }
                        let pc = s.key_root.map(|k| k.0).or(s.root_note.map(|n| n % 12));
                        if k < 0.8
                            && let Some(pc) = pc
                            && s.kind != SampleKind::Loop
                        {
                            let t = transpose_to(pc, proj.root);
                            if t != 0 {
                                transpose = Some(t);
                                reasons.push(format!("transpose {t:+} semitones"));
                            }
                        }
                        if transpose.is_some() { k.max(0.7) } else { k }
                    }
                    None => 0.8,
                };
                let tempo_score = match q.bpm {
                    Some(pb) => {
                        let t = tempo_compat(pb, s.bpm, s.is_loop());
                        if s.is_loop()
                            && let Some(b) = s.bpm
                        {
                            let pct = (b - pb) / pb * 100.0;
                            reasons.push(format!("{} BPM ({pct:+.0}%)", b.round()));
                        }
                        t
                    }
                    None => 0.8,
                };
                if s.favorite {
                    reasons.push("favourite".into());
                }
                let score = 0.5 * key_score
                    + 0.35 * tempo_score
                    + 0.1 * (s.favorite as u8 as f32)
                    + 0.05 * (s.rating as f32 / 5.0);
                Recommendation { sample: s, score, key_score, tempo_score, transpose, reasons }
            })
            .collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.sample.name.cmp(&b.sample.name)));
        out.truncate(if q.limit == 0 { 50 } else { q.limit });
        Ok(out)
    }
}

/// Convenience: the key for a Camelot code.
pub fn camelot_scale(code: &str) -> Option<Scale> {
    let (n, minor) = parse_camelot(code)?;
    from_camelot(n, minor).scale()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::BasicAnalyzer;
    use crate::db::RootKind;
    use crate::scan::ScanOptions;
    use crate::test_util::write_wav;
    use std::sync::atomic::AtomicBool;

    fn am() -> Scale {
        Scale::new(Key(9), ScaleKind::Minor)
    }

    #[test]
    fn compat_scores() {
        let a = am();
        assert_eq!(key_compat(a, Some(Key(9)), Some(ScaleKind::Minor)), 1.0);
        assert_eq!(key_compat(a, Some(Key(0)), Some(ScaleKind::Major)), 0.9); // C major = relative
        assert_eq!(key_compat(a, Some(Key(4)), Some(ScaleKind::Minor)), 0.8); // Em = 9A
        assert_eq!(key_compat(a, Some(Key(2)), Some(ScaleKind::Minor)), 0.8); // Dm = 7A
        assert_eq!(key_compat(a, Some(Key(9)), Some(ScaleKind::Major)), 0.5); // parallel
        assert_eq!(key_compat(a, Some(Key(1)), Some(ScaleKind::Major)), 0.1);
        assert_eq!(key_compat(a, None, None), 1.0);
        assert_eq!(key_compat(a, Some(Key(4)), None), 0.85);
        assert_eq!(key_compat(a, Some(Key(1)), None), 0.3);
        assert_eq!(camelot_neighbors("12A"), vec!["12A", "12B", "1A", "11A"]);
        assert_eq!(camelot_neighbors("1B"), vec!["1B", "1A", "2B", "12B"]);
        assert_eq!(transpose_to(7, Key(9)), 2);
        assert_eq!(transpose_to(3, Key(9)), -6);
        assert_eq!(camelot_scale("8A"), Some(am()));
    }

    #[test]
    fn tempo_scores() {
        assert_eq!(tempo_compat(128.0, Some(128.0), true), 1.0);
        assert_eq!(tempo_compat(128.0, Some(64.0), true), 1.0);
        assert!(tempo_compat(128.0, Some(124.0), true) > 0.8);
        assert!(tempo_compat(128.0, Some(90.0), true) < 0.3);
        assert_eq!(tempo_compat(128.0, None, false), 0.8);
    }

    fn fixture() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        let files = [
            ("House Pack/Kick 1.wav", 0.3),
            ("House Pack/Clap 2.wav", 0.3),
            ("House Pack/Bass Loop Am 124bpm.wav", 124f64.recip() * 60.0 * 8.0),
            ("House Pack/Chord Loop Cmaj 124bpm.wav", 124f64.recip() * 60.0 * 8.0),
            ("House Pack/Lead Loop Em 126bpm.wav", 126f64.recip() * 60.0 * 8.0),
            ("Trap Pack/Lead Loop C#m 140bpm.wav", 140f64.recip() * 60.0 * 8.0),
            ("Trap Pack/Pad Loop Am 70bpm.wav", 70f64.recip() * 60.0 * 8.0),
            ("Trap Pack/808 G.wav", 1.0),
            ("Trap Pack/Vocal Chop F#m.wav", 0.5),
        ];
        for (rel, dur) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            write_wav(&p, 22_050, 1, dur, 0.5);
        }
        let lib = Library::open(dir.path().join("lib.db")).unwrap();
        lib.add_root(&root, RootKind::User).unwrap();
        lib.scan(&BasicAnalyzer, &ScanOptions::default(), &mut |_| {}, &AtomicBool::new(false))
            .unwrap();
        (dir, lib)
    }

    fn names(v: &[Sample]) -> Vec<&str> {
        let mut n: Vec<&str> = v.iter().map(|s| s.name.as_str()).collect();
        n.sort_unstable();
        n
    }

    #[test]
    fn search_filters() {
        let (_d, lib) = fixture();
        assert_eq!(lib.count().unwrap(), 9);
        assert_eq!(names(&lib.search(&SearchQuery::text("kick")).unwrap()), vec!["Kick 1"]);
        assert_eq!(lib.search(&SearchQuery::text("lead loop")).unwrap().len(), 2);
        assert_eq!(lib.search(&SearchQuery::text("trap")).unwrap().len(), 4);
        assert_eq!(lib.search(&SearchQuery::text("8A")).unwrap().len(), 2);
        // Weird input does not break the FTS parser.
        assert!(lib.search(&SearchQuery::text("\"(*) AND OR")).is_ok());

        let compatible = SearchQuery {
            filters: Filters {
                key: Some(KeyFilter { scale: am(), mode: KeyMatch::Compatible, include_atonal: false }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            names(&lib.search(&compatible).unwrap()),
            vec!["808 G", "Bass Loop Am 124bpm", "Chord Loop Cmaj 124bpm", "Lead Loop Em 126bpm", "Pad Loop Am 70bpm"]
        );
        let exact = SearchQuery {
            filters: Filters {
                key: Some(KeyFilter { scale: am(), mode: KeyMatch::Exact, include_atonal: true }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            names(&lib.search(&exact).unwrap()),
            vec!["Bass Loop Am 124bpm", "Clap 2", "Kick 1", "Pad Loop Am 70bpm"]
        );

        let bpm = SearchQuery {
            filters: Filters {
                bpm: Some(BpmFilter { bpm: 125.0, tolerance_pct: 2.0, half_double: false }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(lib.search(&bpm).unwrap().len(), 3);
        let half = SearchQuery {
            filters: Filters {
                bpm: Some(BpmFilter { bpm: 140.0, tolerance_pct: 1.0, half_double: true }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(names(&lib.search(&half).unwrap()), vec!["Lead Loop C#m 140bpm", "Pad Loop Am 70bpm"]);

        let f = Filters { is_loop: Some(true), genre: Some("House".into()), ..Default::default() };
        assert_eq!(lib.count_where(&f).unwrap(), 3);
        let f = Filters { category: Some(Category::Drums), pack: Some("House Pack".into()), ..Default::default() };
        assert_eq!(lib.count_where(&f).unwrap(), 2);
        let f = Filters { kind: Some(SampleKind::Chop), ..Default::default() };
        assert_eq!(lib.count_where(&f).unwrap(), 1);
    }

    /// A library of files with awkward names, scanned for real.
    fn named(files: &[&str]) -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        for rel in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            write_wav(&p, 22_050, 1, 0.2, 0.5);
        }
        let lib = Library::open(dir.path().join("lib.db")).unwrap();
        lib.add_root(&root, RootKind::User).unwrap();
        lib.scan(&BasicAnalyzer, &ScanOptions::default(), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        (dir, lib)
    }

    fn found(lib: &Library, text: &str) -> Vec<String> {
        lib.search(&SearchQuery::text(text)).unwrap().into_iter().map(|s| s.name).collect()
    }

    #[test]
    fn a_search_finds_parts_of_words_not_only_their_beginnings() {
        let (_d, lib) = named(&["DarkKick.wav", "Kick_01.wav", "Snare.wav", "HardKickDrum.wav"]);
        let k = found(&lib, "kick");
        assert_eq!(k.len(), 3, "{k:?}");
        // The whole word first, then the ones that only contain it.
        assert_eq!(k[0], "Kick_01");
        assert_eq!(found(&lib, "ardki").len(), 1, "inside a name, across no word boundary");
        // Every word still has to be there.
        assert_eq!(found(&lib, "dark kick"), vec!["DarkKick"]);
        assert!(found(&lib, "kick snare").is_empty());
    }

    #[test]
    fn accents_and_case_do_not_matter_either_way() {
        let (_d, lib) = named(&["Bajo Eléctrico.wav", "SuperÉpico.wav", "Piano.wav", "Ñandú Loop.wav"]);
        assert_eq!(found(&lib, "electrico"), vec!["Bajo Eléctrico"]);
        assert_eq!(found(&lib, "ELÉCTRICO"), vec!["Bajo Eléctrico"]);
        assert_eq!(found(&lib, "pico"), vec!["SuperÉpico"], "inside a word, accent folded");
        assert_eq!(found(&lib, "épico"), vec!["SuperÉpico"]);
        assert_eq!(found(&lib, "nandu"), vec!["Ñandú Loop"]);
    }

    #[test]
    fn filters_by_folder_and_by_root() {
        let (_d, lib) = named(&["House/Kick 1.wav", "House/Deep/Kick 2.wav", "Trap/Kick 3.wav"]);
        let root = lib.roots().unwrap()[0].id;
        let q = |under: &str| SearchQuery {
            text: Some("kick".into()),
            filters: Filters { root: Some(root), under: Some(under.into()), ..Default::default() },
            ..Default::default()
        };
        let names = |under: &str| {
            let mut n: Vec<String> = lib.search(&q(under)).unwrap().into_iter().map(|s| s.name).collect();
            n.sort();
            n
        };
        assert_eq!(names("House"), vec!["Kick 1", "Kick 2"], "a folder includes its subfolders");
        assert_eq!(names("House/Deep"), vec!["Kick 2"]);
        assert_eq!(names("Hous"), Vec::<String>::new(), "a folder is whole names, not a text prefix");
        let none = Filters { root: Some(crate::db::RootId(root.0 + 99)), ..Default::default() };
        assert_eq!(lib.count_where(&none).unwrap(), 0);
    }

    #[test]
    fn folders_lists_the_directories_with_their_counts() {
        let (_d, lib) = named(&["House/Kick 1.wav", "House/Deep/Kick 2.wav", "Trap/Kick 3.wav", "Top.wav"]);
        let root = lib.roots().unwrap()[0].id;
        let f = lib.folders(root).unwrap();
        let get = |p: &str| f.iter().find(|x| x.0 == p).map(|x| x.1);
        assert_eq!(get("House"), Some(2), "{f:?}");
        assert_eq!(get("House/Deep"), Some(1));
        assert_eq!(get("Trap"), Some(1));
        assert_eq!(get(""), None, "files at the top are not a folder");
    }

    #[test]
    fn tempo_filter_for_loops_is_done_by_the_database() {
        let (_d, lib) = fixture();
        let q = SearchQuery {
            filters: Filters { loop_tempo: Some(BpmFilter { bpm: 124.0, tolerance_pct: 3.0, half_double: true }), ..Default::default() },
            limit: 100,
            ..Default::default()
        };
        let got = lib.search(&q).unwrap();
        let n = names(&got);
        // 124 and 126 bpm loops, the 70 bpm pad (x2 = 140 is out, 70 is not half of 124) is out; one-shots always pass.
        assert!(n.contains(&"Bass Loop Am 124bpm"), "{n:?}");
        assert!(n.contains(&"Kick 1"), "one-shots have no tempo and pass: {n:?}");
        assert!(!n.contains(&"Lead Loop C#m 140bpm"), "{n:?}");
    }

    /// Timings of typical searches against a real bank and a big synthetic one.
    /// `AITRACK_FACTORY_DB=<copy of factory.db> cargo test -p aitrack-library --lib
    /// search_timings -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn search_timings() {
        use std::time::Instant;
        let time = |lib: &Library, q: SearchQuery| {
            let t = Instant::now();
            let n = lib.search(&q).unwrap().len();
            (n, t.elapsed().as_secs_f64() * 1000.)
        };
        let queries = ["kick", "ick", "pad am", "electrico", "a", "bass loop", "128"];
        let report = |label: &str, lib: &Library| {
            for text in queries {
                let q = SearchQuery { text: Some(text.into()), limit: 400, ..Default::default() };
                let (n, ms) = time(lib, q.clone());
                let t = Instant::now();
                let fts_only = lib.search_fts(&q).unwrap().len();
                let fts_ms = t.elapsed().as_secs_f64() * 1000.;
                println!("{label:>10} {text:>10}: {n:>5} hits in {ms:6.2} ms (indexed alone: {fts_only:>5} in {fts_ms:6.2} ms)");
            }
            let f = SearchQuery { filters: Filters { is_loop: Some(true), loop_tempo: Some(BpmFilter { bpm: 124.0, tolerance_pct: 6.0, half_double: true }), ..Default::default() }, limit: 1500, ..Default::default() };
            let (n, ms) = time(lib, f);
            println!("{label:>10} {:>10}: {n:>5} hits in {ms:6.2} ms", "loops@124");
        };
        if let Some(p) = std::env::var_os("AITRACK_FACTORY_DB") {
            let lib = Library::open(p).unwrap();
            println!("factory bank: {} samples", lib.count().unwrap());
            report("factory", &lib);
        }
        // 200 000 rows written straight into the tables.
        let lib = Library::open_in_memory().unwrap();
        let root = lib.add_root(std::env::temp_dir(), RootKind::User).unwrap();
        let words = ["Kick", "Snare", "Bass", "Pad", "Lead", "Loop", "Dark", "Hard", "Deep", "Vocal", "Chop", "Eléctrico", "Hat", "Clap", "FX"];
        let c = lib.connection();
        c.execute_batch("BEGIN").unwrap();
        for i in 0..200_000i64 {
            let name = format!("{} {} {:04}", words[(i % 15) as usize], words[((i / 15) % 15) as usize], i % 997);
            let rel = format!("Pack {}/{}.wav", i % 300, name);
            let looped = i % 3 == 0;
            c.execute(
                "INSERT INTO samples(id, root_id, rel_path, name, size, mtime, added_at, is_loop, kind, bpm, category) VALUES(?1, ?2, ?3, ?4, 1, 1, 1, ?5, ?6, ?7, 'drums')",
                rusqlite::params![i + 1, root.0, rel, name, looped, if looped { "loop" } else { "one_shot" }, 80.0 + (i % 90) as f64],
            )
            .unwrap();
            c.execute("INSERT INTO samples_fts(rowid, name, path, tags) VALUES(?1, ?2, ?3, 'drums one shot')", rusqlite::params![i + 1, name, format!("Pack {}", i % 300)]).unwrap();
        }
        c.execute_batch("COMMIT").unwrap();
        println!("synthetic bank: {} samples", lib.count().unwrap());
        report("200k", &lib);
    }

    #[test]
    fn browse_folders_skips_levels_that_do_not_split_anything() {
        let f = |v: &[(&str, u64)]| v.iter().map(|(p, n)| ((*p).to_owned(), *n)).collect::<Vec<_>>();
        let splice = f(&[("packs", 30), ("packs/Deep House", 20), ("packs/Trap", 10), ("packs/Deep House/Kicks", 5)]);
        assert_eq!(browse_folders(&splice, 10), f(&[("packs/Deep House", 20), ("packs/Trap", 10)]));
        let flat = f(&[("House", 9), ("Trap", 12), ("House/Deep", 3)]);
        assert_eq!(browse_folders(&flat, 1), f(&[("Trap", 12)]), "capped, biggest first");
        assert_eq!(browse_folders(&f(&[("only", 4)]), 5), f(&[("only", 4)]), "a single folder is still the answer");
        assert!(browse_folders(&[], 5).is_empty());
    }

    #[test]
    fn recommendations_rank_by_key_and_tempo() {
        let (_d, lib) = fixture();
        let recs = lib
            .recommend(&RecommendQuery { scale: Some(am()), bpm: Some(124.0), ..Default::default() })
            .unwrap();
        assert_eq!(recs[0].sample.name, "Bass Loop Am 124bpm");
        assert!(recs[0].reasons.iter().any(|r| r.contains("same key")));
        let last = recs.last().unwrap();
        assert_eq!(last.sample.name, "Lead Loop C#m 140bpm");

        let bass = lib
            .recommend(&RecommendQuery {
                scale: Some(am()),
                bpm: Some(124.0),
                category: Some(Category::Synths),
                ..Default::default()
            })
            .unwrap();
        assert!(bass.iter().all(|r| r.sample.category == Some(Category::Synths)));
        assert_eq!(bass[0].sample.name, "Chord Loop Cmaj 124bpm");
    }
}
