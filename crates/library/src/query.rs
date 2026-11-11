//! Search, key compatibility and project-aware recommendations.

use aitrack_model::{Key, Scale, ScaleKind};
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::db::{Library, SAMPLE_COLS, Sample, scale_kind_str};
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

impl Library {
    /// Full-text + filtered search.
    pub fn search(&self, q: &SearchQuery) -> Result<Vec<Sample>> {
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
