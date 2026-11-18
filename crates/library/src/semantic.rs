//! Search by meaning: "dark punchy kick" ranks the sounds whose learned (CLAP) vector is nearest
//! to the phrase's, mixed with what the words say about the file name and tags.
//!
//! The semantic half needs a [`TextEmbedder`] and the vectors its job stored in
//! `sample_embeddings` ([`crate::similar`]); the text half is the library's own word matching, so
//! a sound whose name says what you typed still comes first, and a library that was not listened
//! to yet (or a cloud placeholder, which is never read) is still searchable by name.
//!
//! A hit's score is [`hybrid_score`]:
//!
//! * `SEMANTIC_WEIGHT` × how close the cosine is to the best one of the query (0 when more than
//!   [`SEMANTIC_SPAN`] below it);
//! * `LEXICAL_WEIGHT` × the share of the typed words found in the name, folders, tags;
//! * [`FILENAME_BOOST`] when every word is in the file name.

use std::collections::{HashMap, HashSet};

use crate::db::{Library, Sample, SampleId};
use crate::error::{LibraryError, Result};
use crate::query::{Filters, SearchQuery, SortBy};
use crate::similar::{SimilarFinder, SimilarOptions, TextEmbedder};
use crate::text;

/// Weight of the cosine part of a hit's score.
pub const SEMANTIC_WEIGHT: f32 = 0.7;
/// Weight of the share of typed words found in the name and tags.
pub const LEXICAL_WEIGHT: f32 = 0.3;
/// Added when every typed word is in the file name.
pub const FILENAME_BOOST: f32 = 0.3;
/// Cosine distance under the best match at which a sound scores nothing for meaning.
pub const SEMANTIC_SPAN: f32 = 0.25;

/// The score of one hit, higher first. `closeness` is 0..1 from the cosine (`None`: the sound has
/// no learned vector), `words` the share 0..1 of typed words found in its name, folders and tags.
pub fn hybrid_score(closeness: Option<f32>, words: f32, in_name: bool) -> f32 {
    SEMANTIC_WEIGHT * closeness.unwrap_or(0.0).clamp(0.0, 1.0) + LEXICAL_WEIGHT * words.clamp(0.0, 1.0) + if in_name { FILENAME_BOOST } else { 0.0 }
}

/// How close a cosine is to the best of its query, 1 = the best, 0 = [`SEMANTIC_SPAN`] or more below it.
pub fn closeness(cosine: f32, best: f32) -> f32 {
    (1.0 - (best - cosine) / SEMANTIC_SPAN).clamp(0.0, 1.0)
}

/// One answer of a semantic search.
#[derive(Clone, Debug)]
pub struct SemanticHit {
    /// The library the sample is in (the index of the `libs` given to the search).
    pub lib: u8,
    pub sample: Sample,
    /// [`hybrid_score`].
    pub score: f32,
    /// Cosine between the phrase and the sound (`None`: no learned vector yet).
    pub cosine: Option<f32>,
    /// Every typed word is in the file name.
    pub in_name: bool,
}

/// Keeps the learned vectors in memory between searches.
#[derive(Default)]
pub struct SemanticSearcher {
    finder: SimilarFinder,
}

impl SemanticSearcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the vectors (the libraries changed in a way the cheap check cannot see).
    pub fn invalidate(&mut self) {
        self.finder.invalidate();
    }

    /// The `limit` samples that best answer `query` over the app's two libraries, `[factory, user]`
    /// (the user library holds the vectors). `filters` apply to both. Needs at least one word.
    pub fn search(&mut self, libs: [Option<&Library>; 2], embedder: &dyn TextEmbedder, query: &str, limit: usize, filters: &Filters) -> Result<Vec<SemanticHit>> {
        let words = text::words(query);
        if words.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let phrase = embedder.embed_text(query)?;
        if phrase.len() != embedder.dim() {
            return Err(LibraryError::Audio(format!("text embedding of {} values, expected {}", phrase.len(), embedder.dim())));
        }
        let pool = (limit * 3).max(300);
        let unfiltered = *filters == Filters::default();
        let allowed: Vec<Option<HashSet<i64>>> = libs
            .iter()
            .map(|l| match l {
                Some(l) if !unfiltered => l.filtered_ids(filters).map(Some),
                _ => Ok(None),
            })
            .collect::<Result<_>>()?;
        let allows = |lib: u8, id: SampleId| allowed.get(lib as usize).and_then(Option::as_ref).is_none_or(|set| set.contains(&id.0));

        // Meaning: the nearest vectors to the phrase.
        let index = self.finder.index(libs, embedder)?;
        let opts = SimilarOptions { limit: if unfiltered { pool } else { usize::MAX }, ..Default::default() };
        let mut near: Vec<(u8, SampleId, f32)> = index.nearest(&phrase, None, &opts).into_iter().filter(|n| allows(n.lib, n.id)).map(|n| (n.lib, n.id, n.similarity)).collect();
        near.truncate(pool);
        let best = near.first().map(|n| n.2);

        // Words: the library's own search, all typed words required.
        let mut samples: HashMap<(u8, i64), Sample> = HashMap::new();
        for (tag, lib) in libs.iter().enumerate() {
            let Some(lib) = lib else { continue };
            let q = SearchQuery { text: Some(query.to_owned()), filters: filters.clone(), sort: SortBy::Relevance, limit: pool, offset: 0 };
            for s in lib.search(&q)? {
                samples.insert((tag as u8, s.id.0), s);
            }
            let wanted: Vec<i64> = near.iter().filter(|n| n.0 as usize == tag).map(|n| n.1.0).filter(|id| !samples.contains_key(&(tag as u8, *id))).collect();
            for s in lib.samples_by_id(&wanted)? {
                samples.insert((tag as u8, s.id.0), s);
            }
        }

        let mut hits: Vec<SemanticHit> = samples
            .into_iter()
            .map(|((lib, id), sample)| {
                let cosine = index.similarity_to(&phrase, lib, SampleId(id));
                let closeness = cosine.zip(best).map(|(c, b)| closeness(c, b));
                let (found, in_name) = word_share(&words, &sample);
                SemanticHit { lib, score: hybrid_score(closeness, found, in_name), sample, cosine, in_name }
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.lib.cmp(&b.lib)).then_with(|| a.sample.id.0.cmp(&b.sample.id.0)));
        hits.truncate(limit);
        Ok(hits)
    }
}

/// The share of `words` found in the sample's name, folders, pack or tags, and whether all of them are in the name.
fn word_share(words: &[String], s: &Sample) -> (f32, bool) {
    let mut extra: Vec<&str> = vec![&s.rel_path];
    extra.extend(s.pack.as_deref());
    extra.extend(s.category.map(|c| c.as_str()));
    extra.extend(s.subcategory.as_deref());
    extra.extend(s.genres.iter().map(String::as_str));
    extra.extend(s.user_tags.iter().map(String::as_str));
    let found = words.iter().filter(|w| text::score(std::slice::from_ref(*w), &s.name, &extra).is_some()).count();
    (found as f32 / words.len() as f32, text::score(words, &s.name, &[]).is_some())
}

impl Library {
    /// Search by meaning in this library alone (it holds its own vectors): see [`SemanticSearcher`],
    /// which keeps them between calls and handles the factory bank beside the user library.
    pub fn search_text_semantic(&self, embedder: &dyn TextEmbedder, query: &str, limit: usize, filters: &Filters) -> Result<Vec<SemanticHit>> {
        SemanticSearcher::new().search([None, Some(self)], embedder, query, limit, filters)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::BasicAnalyzer;
    use crate::db::RootKind;
    use crate::scan::ScanOptions;
    use crate::similar::{EmbedOptions, Embedder};
    use crate::test_util::write_wav;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn the_score_mixes_meaning_words_and_the_name() {
        // A sound that is the best match for meaning, with no word in common.
        let semantic_only = hybrid_score(Some(1.0), 0.0, false);
        // Weak meaning but the name says it.
        let named = hybrid_score(Some(0.3), 1.0, true);
        assert!(named > semantic_only, "{named} vs {semantic_only}");
        // A name match with no learned vector still outranks a poor semantic match.
        assert!(hybrid_score(None, 1.0, true) > hybrid_score(Some(0.5), 0.0, false));
        // Equal words: meaning decides.
        assert!(hybrid_score(Some(0.9), 0.5, false) > hybrid_score(Some(0.4), 0.5, false));
        // Bounded.
        assert!(hybrid_score(Some(7.0), 9.0, true) <= SEMANTIC_WEIGHT + LEXICAL_WEIGHT + FILENAME_BOOST + 1e-6);
        assert_eq!(hybrid_score(None, 0.0, false), 0.0);
    }

    #[test]
    fn closeness_is_relative_to_the_best_cosine() {
        assert_eq!(closeness(0.4, 0.4), 1.0);
        assert!((closeness(0.275, 0.4) - 0.5).abs() < 1e-6);
        assert_eq!(closeness(0.0, 0.4), 0.0);
        assert_eq!(closeness(0.9, 0.4), 1.0);
    }

    /// A tiny "CLAP": the audio vector says how bright the file is (by its name, as the fake
    /// cannot listen: "Hat" is bright, "Kick" is dark), the text vector does the same for words.
    struct Fake;

    fn axes(s: &str) -> Vec<f32> {
        let s = s.to_lowercase();
        let mut v = vec![0.05f32; 4];
        for (i, words) in [["kick", "dark", "punchy", "boom"], ["hat", "bright", "tick", "sizzle"], ["pad", "warm", "analog", "soft"], ["vocal", "chop", "voice", "choir"]].iter().enumerate() {
            if words.iter().any(|w| s.contains(w)) {
                v[i] = 1.0;
            }
        }
        v
    }

    impl Embedder for Fake {
        fn id(&self) -> &str {
            "fake-clap"
        }
        fn version(&self) -> u32 {
            1
        }
        fn dim(&self) -> usize {
            4
        }
        fn standardise(&self) -> bool {
            false
        }
        fn embed(&self, path: &Path) -> Result<Vec<f32>> {
            // Samples named for what they are, except one that is mislabelled on purpose.
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            Ok(axes(name.split("~").last().unwrap_or(&name)))
        }
    }

    impl TextEmbedder for Fake {
        fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
            Ok(axes(text))
        }
    }

    fn library(names: &[&str]) -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let samples = dir.path().join("samples");
        std::fs::create_dir_all(&samples).unwrap();
        for (i, n) in names.iter().enumerate() {
            write_wav(&samples.join(format!("{n}.wav")), 44_100, 1, 0.2, 0.1 + i as f32 * 0.01);
        }
        let lib = Library::open(dir.path().join("lib.db")).unwrap();
        let root = lib.add_root(&samples, RootKind::User).unwrap();
        lib.scan(&BasicAnalyzer, &ScanOptions { roots: Some(vec![root]), ..Default::default() }, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        lib.embed_missing(&lib, &Fake, &EmbedOptions { threads: 1, limit: None }, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        (dir, lib)
    }

    fn names(hits: &[SemanticHit]) -> Vec<&str> {
        hits.iter().map(|h| h.sample.name.as_str()).collect()
    }

    #[test]
    fn words_find_sounds_by_meaning_not_by_file_name() {
        // File names carry no hint except the part after "~", which only the fake "listens" to.
        let (_d, lib) = library(&["A1~kick", "A2~kick", "A3~hat", "A4~hat", "A5~pad", "A6~vocal chop"]);
        let hits = lib.search_text_semantic(&Fake, "dark punchy kick", 3, &Filters::default()).unwrap();
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().take(2).all(|h| h.sample.name.contains("kick")), "{:?}", names(&hits));
        assert!(hits[0].score >= hits[1].score && hits[1].score >= hits[2].score);
        assert!(hits[0].cosine.unwrap() > hits[2].cosine.unwrap());
        let hits = lib.search_text_semantic(&Fake, "warm analog pad", 2, &Filters::default()).unwrap();
        assert_eq!(hits[0].sample.name, "A5~pad", "{:?}", names(&hits));
        let hits = lib.search_text_semantic(&Fake, "vocal chop", 1, &Filters::default()).unwrap();
        assert_eq!(hits[0].sample.name, "A6~vocal chop");
    }

    #[test]
    fn a_file_name_that_says_it_gets_a_boost_over_a_better_listening_match() {
        // "Hat Kick Layer" sounds like a hat to the fake (its vector is the last "~" part), but its name says kick.
        let (_d, lib) = library(&["kick one~kick", "Kick Layer~hat", "B3~hat"]);
        let hits = lib.search_text_semantic(&Fake, "kick", 3, &Filters::default()).unwrap();
        assert_eq!(hits[0].sample.name, "kick one~kick", "meaning and name agree: first");
        assert!(hits[0].in_name);
        assert_eq!(hits[1].sample.name, "Kick Layer~hat", "the name beats a sound that only resembles hats: {:?}", names(&hits));
        assert!(hits[1].in_name && !hits[2].in_name);
    }

    #[test]
    fn filters_narrow_the_semantic_list_and_empty_queries_answer_nothing() {
        let (_d, lib) = library(&["A1~kick", "A2~kick loop", "A3~hat"]);
        let f = Filters { is_loop: Some(false), ..Default::default() };
        let all = lib.search_text_semantic(&Fake, "kick", 10, &Filters::default()).unwrap();
        assert_eq!(all.len(), 3, "everything is a candidate");
        let some = lib.search_text_semantic(&Fake, "kick", 10, &f).unwrap();
        assert!(some.iter().all(|h| !h.sample.name.contains("loop") || !h.sample.is_loop()), "{:?}", names(&some));
        assert!(lib.search_text_semantic(&Fake, "   ", 10, &Filters::default()).unwrap().is_empty());
        assert!(lib.search_text_semantic(&Fake, "kick", 0, &Filters::default()).unwrap().is_empty());
    }

    #[test]
    fn sounds_without_a_learned_vector_are_still_found_by_name() {
        let (_d, lib) = library(&["A1~kick", "A2~hat"]);
        // A sound added after the listening has no vector.
        let dir = tempfile::tempdir().unwrap();
        write_wav(&dir.path().join("Sub Boom Kick.wav"), 44_100, 1, 0.2, 0.3);
        let root = lib.add_root(dir.path(), RootKind::User).unwrap();
        lib.scan(&BasicAnalyzer, &ScanOptions { roots: Some(vec![root]), ..Default::default() }, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        let hits = lib.search_text_semantic(&Fake, "sub boom kick", 5, &Filters::default()).unwrap();
        let pos = |n: &str| hits.iter().position(|h| h.sample.name == n).unwrap_or_else(|| panic!("{n} missing: {:?}", names(&hits)));
        assert!(hits[pos("Sub Boom Kick")].cosine.is_none() && hits[pos("Sub Boom Kick")].in_name);
        assert!(pos("Sub Boom Kick") < pos("A2~hat"), "{:?}", names(&hits));
    }
}
