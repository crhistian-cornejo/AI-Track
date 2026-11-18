//! The palette's sample search, on its own thread with its own read
//! connections to the factory and user indexes, so typing never waits on
//! SQLite. Only the newest query is answered; each is capped.

use crate::bridge::LibraryItem;
use aitrack_library::{BpmFilter, DspEmbedder, Filters, KeyFilter, KeyMatch, Library, Sample, SampleId, SearchQuery, SimilarFinder, SortBy};
use aitrack_model::Scale;
use crossbeam_channel::{Receiver, Sender, unbounded};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// Rows fetched per index for one query.
const FETCH: usize = 400;
/// Rows the palette lists.
pub const SHOWN: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KindFilter {
    #[default]
    All,
    OneShots,
    Loops,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SampleQuery {
    pub generation: u64,
    pub text: String,
    pub kind: KindFilter,
    pub bpm: Option<(u32, u32)>,
    /// Only samples in (or compatible with) this key.
    pub key: Option<Scale>,
    /// Every one of these tags (lower case).
    pub tags: Vec<String>,
    /// Instead of a text search: the sounds that sound like this sample (bridge id), most alike first.
    pub similar: Option<u64>,
}

/// One local sample as the palette lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct SampleRow {
    /// Bridge id: `(index << 48) | sample id` (0 factory, 1 user).
    pub id: u64,
    pub name: String,
    pub path: PathBuf,
    pub pack: Option<String>,
    /// Category, sub-category, genres and the user's own tags.
    pub tags: Vec<String>,
    pub category: String,
    pub duration: f32,
    pub key: Option<Scale>,
    pub bpm: Option<f32>,
    pub is_loop: bool,
    pub favorite: bool,
    /// Online-only in a cloud folder: listed by name, downloaded when played.
    pub cloud: bool,
    /// From the folder the Splice desktop app downloads licensed sounds to.
    pub splice: bool,
    /// Text relevance, higher first.
    pub score: f32,
    /// In a "Similar to…" list: how alike this sound is to the reference, -1..1.
    pub similarity: Option<f32>,
}

impl SampleRow {
    pub fn from_sample(index: usize, s: &Sample) -> Self {
        let mut tags = Vec::new();
        let mut push = |t: &str| {
            let t = t.trim().to_lowercase();
            if !t.is_empty() && !tags.contains(&t) {
                tags.push(t);
            }
        };
        if let Some(c) = s.category {
            push(c.as_str());
        }
        if let Some(sub) = &s.subcategory {
            push(sub);
        }
        s.genres.iter().for_each(|g| push(g));
        s.user_tags.iter().for_each(|g| push(g));
        let category = match (&s.category, &s.subcategory) {
            (Some(c), Some(sub)) => format!("{} · {sub}", c.as_str()),
            (Some(c), None) => c.as_str().to_owned(),
            _ => if s.is_loop() { "Loop" } else { "One-shot" }.to_owned(),
        };
        Self {
            id: ((index as u64) << 48) | s.id.0 as u64,
            name: s.name.clone(),
            path: s.path.clone(),
            pack: s.pack.clone(),
            tags,
            category,
            duration: s.duration.unwrap_or(0.) as f32,
            key: s.scale(),
            bpm: s.bpm,
            is_loop: s.is_loop(),
            favorite: s.favorite,
            cloud: s.cloud,
            splice: false,
            score: 0.,
            similarity: None,
        }
    }

    pub fn from_item(it: &LibraryItem) -> Self {
        let tags = it.category.split('·').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect();
        Self {
            id: it.id,
            name: it.name.clone(),
            path: it.path.clone(),
            pack: None,
            tags,
            category: it.category.clone(),
            duration: it.duration,
            key: it.key,
            bpm: it.bpm,
            is_loop: it.is_loop,
            favorite: it.favorite,
            cloud: false,
            splice: false,
            score: 0.,
            similarity: None,
        }
    }

    pub fn item(&self) -> LibraryItem {
        LibraryItem {
            id: self.id,
            name: self.name.clone(),
            path: self.path.clone(),
            category: self.category.clone(),
            key: self.key,
            bpm: self.bpm,
            duration: self.duration,
            is_loop: self.is_loop,
            favorite: self.favorite,
        }
    }
}

/// How well `row` matches the typed words: name prefix > name word > part of
/// a name > tag/pack, accents and case ignored (the library's own rule).
pub fn relevance(row: &SampleRow, text: &str) -> f32 {
    let words = aitrack_library::text::words(text);
    let mut extra: Vec<&str> = row.tags.iter().map(String::as_str).collect();
    extra.extend(row.pack.as_deref());
    aitrack_library::text::score(&words, &row.name, &extra).unwrap_or(0.) + if row.favorite { 0.5 } else { 0. }
}

/// Keep the rows carrying every tag, then rank and cap them.
pub fn finish(mut rows: Vec<SampleRow>, q: &SampleQuery) -> (Vec<SampleRow>, Vec<(String, usize)>) {
    rows.retain(|r| q.tags.iter().all(|t| r.tags.contains(t)));
    let mut seen = HashSet::new();
    rows.retain(|r| seen.insert(r.path.clone()));
    for r in &mut rows {
        r.score = relevance(r, &q.text);
    }
    // Stable: equal scores keep the index's own order.
    rows.sort_by(|a, b| b.score.total_cmp(&a.score));
    rows.truncate(SHOWN);
    let tags = tag_counts(&rows);
    (rows, tags)
}

/// The tags of the rows with how many carry each, most common first.
pub fn tag_counts(rows: &[SampleRow]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in rows {
        for t in &r.tags {
            *counts.entry(t.clone()).or_default() += 1;
        }
    }
    let mut tags: Vec<(String, usize)> = counts.into_iter().collect();
    tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    tags
}

/// The rows of a "Similar to…" list, most alike first and one per file.
fn similar_rows(libs: &[Option<Library>; 2], finder: &mut SimilarFinder, id: u64) -> Vec<SampleRow> {
    let (lib, sample) = ((id >> 48) as u8, SampleId((id & ((1 << 48) - 1)) as i64));
    let refs = [libs[FACTORY].as_ref(), libs[USER].as_ref()];
    let hits = match finder.find(refs, lib, sample, &DspEmbedder, SHOWN) {
        Ok(h) => h,
        Err(e) => {
            log::warn!("palette: similar sounds: {e}");
            return Vec::new();
        }
    };
    let splice_roots = splice_roots(libs[USER].as_ref());
    let mut seen = HashSet::new();
    hits.iter()
        .filter(|h| seen.insert(h.sample.path.clone()))
        .map(|h| SampleRow { splice: h.lib as usize == USER && splice_roots.contains(&h.sample.root_id.0), similarity: Some(h.similarity), ..SampleRow::from_sample(h.lib as usize, &h.sample) })
        .collect()
}

/// Which of the user's folders is Splice's.
fn splice_roots(user: Option<&Library>) -> HashSet<i64> {
    user.and_then(|l| l.roots().ok())
        .map(|r| r.into_iter().filter(|r| r.kind == aitrack_library::RootKind::Splice).map(|r| r.id.0).collect())
        .unwrap_or_default()
}

fn search_query(q: &SampleQuery) -> SearchQuery {
    let text = q.text.trim();
    SearchQuery {
        text: (!text.is_empty()).then(|| text.to_owned()),
        filters: Filters {
            is_loop: match q.kind {
                KindFilter::All => None,
                KindFilter::Loops => Some(true),
                KindFilter::OneShots => Some(false),
            },
            bpm_range: q.bpm.map(|(a, b)| (a.min(b) as f32, a.max(b) as f32)),
            key: q.key.map(|scale| KeyFilter { scale, mode: KeyMatch::Compatible, include_atonal: false }),
            bpm: None::<BpmFilter>,
            ..Default::default()
        },
        sort: SortBy::Relevance,
        limit: FETCH,
        offset: 0,
    }
}

const FACTORY: usize = 0;
const USER: usize = 1;

/// The same query without its key filter.
fn unkeyed(q: &SearchQuery) -> SearchQuery {
    let mut q = q.clone();
    q.filters.key = None;
    q
}

/// Keys and tempos detected by listening, cached in the user library.
fn fill_keys(user: Option<&Library>, samples: &mut [Sample]) {
    if let Some(user) = user
        && let Err(e) = user.fill_from_cache(samples)
    {
        log::warn!("palette: detected keys: {e}");
    }
}

pub enum Reply {
    /// The worker opened no index (stub bridges, or no library yet).
    NoIndex,
    Samples { generation: u64, rows: Vec<SampleRow>, tags: Vec<(String, usize)> },
}

pub struct Worker {
    tx: Sender<SampleQuery>,
    rx: Receiver<Reply>,
}

impl Worker {
    pub fn spawn(factory: Option<PathBuf>, user: PathBuf) -> Self {
        let (tx, jobs) = unbounded::<SampleQuery>();
        let (out, rx) = unbounded();
        let run = move || {
            let open = |p: &PathBuf| Library::open(p).map_err(|e| log::warn!("palette: {}: {e}", p.display())).ok();
            let mut libs = [factory.as_ref().and_then(open), user.is_file().then(|| open(&user)).flatten()];
            if libs.iter().all(Option::is_none) {
                // The UI searches the bridge itself from now on; an empty
                // answer from here would overwrite its results.
                let _ = out.send(Reply::NoIndex);
                return;
            }
            let mut finder = SimilarFinder::new();
            while let Ok(mut q) = jobs.recv() {
                while let Ok(newer) = jobs.try_recv() {
                    q = newer;
                }
                // The first folder added creates the user library.
                if libs[USER].is_none() && user.is_file() {
                    libs[USER] = open(&user);
                }
                if let Some(id) = q.similar {
                    let rows = similar_rows(&libs, &mut finder, id);
                    let tags = tag_counts(&rows);
                    if out.send(Reply::Samples { generation: q.generation, rows, tags }).is_err() {
                        break;
                    }
                    continue;
                }
                let sq = search_query(&q);
                let mut rows = Vec::new();
                for (i, lib) in libs.iter().enumerate() {
                    if let Some(lib) = lib {
                        // Which of the user's folders is Splice's.
                        let splice_roots: HashSet<i64> = if i == USER { splice_roots(Some(lib)) } else { HashSet::new() };
                        match lib.search(&sq) {
                            Ok(mut found) => {
                                fill_keys(libs[USER].as_ref(), &mut found);
                                rows.extend(found.iter().map(|s| SampleRow { splice: splice_roots.contains(&s.root_id.0), ..SampleRow::from_sample(i, s) }));
                            }
                            Err(e) => log::warn!("palette search: {e}"),
                        }
                        // The factory bank is read-only: keys found by
                        // listening live in the user library's cache, so a
                        // key filter also looks at the rows without a key.
                        if i == FACTORY
                            && let Some(key) = q.key
                            && let Ok(mut more) = lib.search(&SearchQuery { limit: FETCH * 4, ..unkeyed(&sq) })
                        {
                            more.retain(|s| s.key_root.is_none());
                            fill_keys(libs[USER].as_ref(), &mut more);
                            rows.extend(more.iter().filter(|s| s.scale().is_some_and(|k| k.mask() == key.mask())).map(|s| SampleRow::from_sample(i, s)));
                        }
                    }
                }
                let (rows, tags) = finish(rows, &q);
                if out.send(Reply::Samples { generation: q.generation, rows, tags }).is_err() {
                    break;
                }
            }
        };
        if let Err(e) = std::thread::Builder::new().name("palette-search".into()).spawn(run) {
            log::warn!("palette: cannot start the search thread: {e}");
        }
        Self { tx, rx }
    }

    pub fn query(&self, q: SampleQuery) {
        let _ = self.tx.send(q);
    }

    pub fn poll(&self) -> Vec<Reply> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, tags: &[&str]) -> SampleRow {
        SampleRow {
            id: 0,
            name: name.into(),
            path: PathBuf::from(name),
            pack: None,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            category: String::new(),
            duration: 1.,
            key: None,
            bpm: None,
            is_loop: false,
            favorite: false,
            cloud: false,
            splice: false,
            score: 0.,
            similarity: None,
        }
    }

    #[test]
    fn name_prefix_beats_tag_match() {
        let q = SampleQuery { text: "kick".into(), ..Default::default() };
        let (rows, _) = finish(vec![row("Snare_kickish", &[]), row("Perc", &["kick"]), row("Kick_01", &[])], &q);
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["Kick_01", "Snare_kickish", "Perc"]);
    }

    #[test]
    fn accents_do_not_change_the_order() {
        let q = SampleQuery { text: "electrico".into(), ..Default::default() };
        let (rows, _) = finish(vec![row("Pad Eléctrico", &[]), row("Eléctrico Bajo", &[]), row("Piano", &["electrico"])], &q);
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["Eléctrico Bajo", "Pad Eléctrico", "Piano"]);
    }

    #[test]
    fn tags_filter_and_count() {
        let q = SampleQuery { tags: vec!["dark".into()], ..Default::default() };
        let (rows, tags) = finish(vec![row("a", &["dark", "bass"]), row("b", &["bass"]), row("c", &["dark"]), row("c", &["dark"])], &q);
        assert_eq!(rows.len(), 2, "filtered and de-duplicated by path");
        assert_eq!(tags[0], ("dark".into(), 2));
    }

    #[test]
    fn tag_counts_sort_by_count_then_name() {
        let tags = tag_counts(&[row("a", &["kick", "dark"]), row("b", &["dark"]), row("c", &["bass", "dark"])]);
        assert_eq!(tags, vec![("dark".to_owned(), 3), ("bass".to_owned(), 1), ("kick".to_owned(), 1)]);
    }

    #[test]
    fn query_maps_filters() {
        let q = SampleQuery { text: " x ".into(), kind: KindFilter::Loops, bpm: Some((130, 120)), ..Default::default() };
        let s = search_query(&q);
        assert_eq!(s.text.as_deref(), Some("x"));
        assert_eq!(s.filters.is_loop, Some(true));
        assert_eq!(s.filters.bpm_range, Some((120., 130.)));
        assert!(s.filters.key.is_none());
    }

    #[test]
    fn worker_without_index_says_so() {
        let w = Worker::spawn(None, PathBuf::from("/nonexistent/aitrack/library.db"));
        let t0 = std::time::Instant::now();
        let mut got = Vec::new();
        while got.is_empty() && t0.elapsed() < std::time::Duration::from_secs(5) {
            got = w.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(matches!(got.first(), Some(Reply::NoIndex)));
        // It answers nothing after that: an empty reply would replace the
        // results the UI got from the bridge.
        w.query(SampleQuery { generation: 1, ..Default::default() });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(w.poll().is_empty());
    }
}
