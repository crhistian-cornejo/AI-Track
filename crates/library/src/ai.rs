//! [`AiDetector`]: key and tempo from audio through `aitrack-ai` (feature
//! `ai`). Pure DSP; no model download and no ONNX Runtime involved.

use std::path::Path;
use std::sync::OnceLock;

use crate::error::{LibraryError, Result};
use crate::parse::KeyHint;
use crate::reanalyse::{Detection, KeyBpmDetector};
use crate::similar::{Embedder, TextEmbedder};

/// `aitrack_ai::analyze` as a [`KeyBpmDetector`].
#[derive(Clone, Copy, Debug, Default)]
pub struct AiDetector;

impl AiDetector {
    pub const ID: &'static str = "aitrack-ai";
    pub const VERSION: u32 = 1;
}

impl KeyBpmDetector for AiDetector {
    fn id(&self) -> &str {
        Self::ID
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn detect(&self, path: &Path) -> Result<Detection> {
        let s = aitrack_ai::analyze(path).map_err(|e| LibraryError::Audio(e.to_string()))?;
        Ok(Detection {
            key: s.key.map(|(scale, conf)| (KeyHint { root: scale.root, kind: Some(scale.kind) }, conf)),
            bpm: s.bpm,
            is_loop: s.is_loop,
        })
    }
}

/// `aitrack_ai::embed_file` (MFCC, spectral shape and envelope features) as an [`Embedder`].
#[derive(Clone, Copy, Debug, Default)]
pub struct DspEmbedder;

impl Embedder for DspEmbedder {
    fn id(&self) -> &str {
        aitrack_ai::DSP_EMBEDDING_ID
    }

    fn version(&self) -> u32 {
        aitrack_ai::DSP_EMBEDDING_VERSION
    }

    fn dim(&self) -> usize {
        aitrack_ai::DSP_EMBEDDING_DIM
    }

    fn embed(&self, path: &Path) -> Result<Vec<f32>> {
        aitrack_ai::embed_file(path).map_err(|e| LibraryError::Audio(e.to_string()))
    }
}

/// CLAP (`aitrack_ai::clap`) as an [`Embedder`] and a [`TextEmbedder`]: 512 values per sound that
/// live beside the DSP vectors under their own id, and the same space for a typed phrase.
///
/// Opening the ONNX sessions takes a moment, so it happens on first use, on whichever thread
/// embeds; build one only when [`ClapEmbedder::is_ready`] says the model is installed.
pub struct ClapEmbedder {
    clap: OnceLock<std::result::Result<aitrack_ai::clap::Clap, String>>,
}

impl ClapEmbedder {
    /// For the installed model; nothing is opened yet.
    pub fn new() -> Self {
        Self { clap: OnceLock::new() }
    }

    /// Around an existing pipeline.
    pub fn from_clap(clap: aitrack_ai::clap::Clap) -> Self {
        let cell = OnceLock::new();
        let _ = cell.set(Ok(clap));
        Self { clap: cell }
    }

    /// Whether the model and ONNX Runtime are installed (reads two small files; not for the UI thread).
    pub fn is_ready() -> bool {
        aitrack_ai::ModelManager::new().is_ok_and(|m| aitrack_ai::clap::is_ready(&m))
    }

    fn clap(&self) -> Result<&aitrack_ai::clap::Clap> {
        let opened = self.clap.get_or_init(|| {
            aitrack_ai::ModelManager::new()
                .and_then(|m| aitrack_ai::clap::Clap::open(&m))
                .map_err(|e| e.to_string())
        });
        opened.as_ref().map_err(|e| LibraryError::Audio(format!("CLAP: {e}")))
    }
}

impl Default for ClapEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl Embedder for ClapEmbedder {
    fn id(&self) -> &str {
        aitrack_ai::CLAP_EMBEDDING_ID
    }

    fn version(&self) -> u32 {
        aitrack_ai::CLAP_EMBEDDING_VERSION
    }

    fn dim(&self) -> usize {
        aitrack_ai::CLAP_EMBEDDING_DIM
    }

    /// The vectors are unit length already, and cosines to a phrase only mean something unshifted.
    fn standardise(&self) -> bool {
        false
    }

    fn embed(&self, path: &Path) -> Result<Vec<f32>> {
        self.clap()?.embed_file(path).map_err(|e| LibraryError::Audio(e.to_string()))
    }
}

impl TextEmbedder for ClapEmbedder {
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        self.clap()?.embed_text(text).map_err(|e| LibraryError::Audio(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{write_clicks, write_wav};

    #[test]
    fn detects_a_click_loop_tempo() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Click Loop.wav");
        write_clicks(&p, 44_100, 120.0, 16);
        let d = AiDetector.detect(&p).unwrap();
        assert!(d.is_loop, "{d:?}");
        let bpm = d.confident_bpm().expect("a confident tempo");
        assert!((bpm - 120.0).abs() < 1.0, "{bpm}");
    }

    #[test]
    fn sine_key_root_is_a_and_bad_files_fail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Pad Sine.wav");
        write_wav(&p, 44_100, 1, 3.0, 0.5);
        let d = AiDetector.detect(&p).unwrap();
        // A lone 440 Hz tone: whatever scale is picked, it must contain A.
        if let Some((k, _)) = d.key {
            let scale = k.scale().unwrap();
            assert!(scale.contains(69), "{scale:?}");
        }
        let bad = dir.path().join("bad.wav");
        std::fs::write(&bad, b"RIFF nope").unwrap();
        assert!(AiDetector.detect(&bad).is_err());
    }

    #[test]
    fn the_dsp_embedder_ranks_a_quieter_copy_above_a_different_sound() {
        use crate::similar::{SimilarIndex, SimilarEntry, SimilarOptions};
        use crate::db::SampleId;
        let dir = tempfile::tempdir().unwrap();
        let files = [("Tone loud.wav", 0.8f32), ("Tone quiet.wav", 0.1)];
        for (name, amp) in files {
            write_wav(&dir.path().join(name), 44_100, 1, 1.0, amp);
        }
        write_clicks(&dir.path().join("Clicks.wav"), 44_100, 120.0, 4);
        let items: Vec<_> = ["Tone loud.wav", "Tone quiet.wav", "Clicks.wav"]
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let entry = SimilarEntry { lib: 1, id: SampleId(i as i64), category: None, subcategory: None, key: None, bpm: None, is_loop: false };
                (entry, DspEmbedder.embed(&dir.path().join(n)).unwrap())
            })
            .collect();
        assert_eq!(items[0].1.len(), DspEmbedder.dim());
        let index = SimilarIndex::from_vectors(DspEmbedder.dim(), true, items);
        let r = index.nearest_to(1, SampleId(0), &SimilarOptions::default());
        assert_eq!(r[0].id, SampleId(1), "{r:?}");
        assert!(r[0].similarity > r[1].similarity);
    }

    // ---- CLAP: the pipeline end to end with a fake model ----

    use crate::db::{Library, RootKind, SampleId};
    use crate::query::Filters;
    use crate::scan::ScanOptions;
    use crate::semantic::SemanticSearcher;
    use crate::similar::{EmbedJob, EmbedOptions, SimilarFinder};
    use aitrack_ai::clap::{Clap, ClapBackend, MEL_BANDS, MEL_FRAMES, Tokenizer};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    /// Audio: how much energy each eighth of the mel bands holds. Text: "dark" is the lowest
    /// eighth, "bright" the seventh. Padded with zeros to the real model's 512 values.
    struct FakeTowers;

    impl ClapBackend for FakeTowers {
        fn dim(&self) -> usize {
            aitrack_ai::CLAP_EMBEDDING_DIM
        }
        fn embed_features(&self, f: &[f32]) -> aitrack_ai::Result<Vec<f32>> {
            assert_eq!(f.len(), MEL_FRAMES * MEL_BANDS);
            let mut v = vec![0f32; self.dim()];
            for frame in f.chunks_exact(MEL_BANDS) {
                for (b, x) in frame.iter().enumerate() {
                    v[b / 8] += (x + 100.0) / MEL_FRAMES as f32;
                }
            }
            Ok(v)
        }
        fn embed_ids(&self, ids: &[i64]) -> aitrack_ai::Result<Vec<f32>> {
            let mut v = vec![0f32; self.dim()];
            if ids.contains(&4) {
                v[0] = 1.0;
            }
            if ids.contains(&5) {
                v[6] = 1.0;
            }
            Ok(v)
        }
    }

    /// A vocabulary where "dark" (4) and "bright" (5) are single tokens, built from letter merges.
    fn fake_clap_embedder() -> ClapEmbedder {
        let mut merges = String::from("#version: 0.2\n");
        let mut vocab = serde_json::Map::new();
        for (i, t) in ["<s>", "<pad>", "</s>", "<unk>", "dark", "bright"].iter().enumerate() {
            vocab.insert((*t).into(), serde_json::json!(i));
        }
        let mut next = 6;
        for word in ["dark", "bright"] {
            let mut cur = word[..1].to_owned();
            vocab.entry(cur.clone()).or_insert_with(|| {
                next += 1;
                serde_json::json!(next)
            });
            for c in word[1..].chars() {
                merges.push_str(&format!("{cur} {c}\n"));
                vocab.entry(c.to_string()).or_insert_with(|| {
                    next += 1;
                    serde_json::json!(next)
                });
                cur.push(c);
                vocab.entry(cur.clone()).or_insert_with(|| {
                    next += 1;
                    serde_json::json!(next)
                });
            }
        }
        let tok = Tokenizer::from_text(&serde_json::Value::Object(vocab).to_string(), &merges).unwrap();
        ClapEmbedder::from_clap(Clap::new(Box::new(FakeTowers), tok))
    }

    fn write_tone(path: &std::path::Path, hz: f32) {
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..24_000 {
            let v = (i as f32 * hz * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
            w.write_sample((v * i16::MAX as f32) as i16).unwrap();
        }
        w.finalize().unwrap();
    }

    fn scanned(files: &[(&str, f32)]) -> (tempfile::TempDir, std::path::PathBuf, Library) {
        let dir = tempfile::tempdir().unwrap();
        let samples = dir.path().join("samples");
        std::fs::create_dir_all(&samples).unwrap();
        for (n, hz) in files {
            write_tone(&samples.join(n), *hz);
        }
        let db = dir.path().join("lib.db");
        let lib = Library::open(&db).unwrap();
        let root = lib.add_root(&samples, RootKind::User).unwrap();
        lib.scan(&crate::analyze::BasicAnalyzer, &ScanOptions { roots: Some(vec![root]), ..Default::default() }, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        (dir, db, lib)
    }

    fn run_job(db: &std::path::Path, e: Arc<dyn Embedder>) -> crate::similar::EmbedStats {
        EmbedJob::spawn_library(None, db.to_path_buf(), e, EmbedOptions { threads: 1, limit: None }).join().unwrap()
    }

    #[test]
    fn clap_and_dsp_vectors_share_the_table_and_the_job_fills_both() {
        let (_d, db, lib) = scanned(&[("S1.wav", 80.0), ("S2.wav", 120.0), ("S3.wav", 9000.0)]);
        let clap = Arc::new(fake_clap_embedder());
        let a = run_job(&db, Arc::new(DspEmbedder));
        assert_eq!((a.embedded, a.errors), (3, 0));
        // An old database holding only DSP vectors has nothing from CLAP yet.
        assert_eq!(lib.embedding_count(aitrack_ai::CLAP_EMBEDDING_ID, 1).unwrap(), 0);
        let b = run_job(&db, clap.clone());
        assert_eq!((b.embedded, b.errors), (3, 0), "{b:?}");
        assert_eq!(lib.embedding_count(DspEmbedder.id(), 1).unwrap(), 3, "the DSP vectors stay");
        assert_eq!(lib.embedding_count(aitrack_ai::CLAP_EMBEDDING_ID, 1).unwrap(), 3);
        let n: i64 = lib.connection().query_row("SELECT COUNT(*) FROM sample_embeddings", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 6);
        let c = run_job(&db, clap.clone());
        assert_eq!((c.embedded, c.cached), (0, 3), "a second run listens to nothing again");
    }

    #[test]
    fn text_finds_the_sound_that_matches_and_similar_blends_both_embedders() {
        let (_d, db, lib) = scanned(&[("S1.wav", 80.0), ("S2.wav", 120.0), ("S3.wav", 9000.0)]);
        let clap = Arc::new(fake_clap_embedder());
        run_job(&db, Arc::new(DspEmbedder));
        run_job(&db, clap.clone());

        let mut searcher = SemanticSearcher::new();
        let libs = [None, Some(&lib)];
        let dark = searcher.search(libs, clap.as_ref(), "dark", 3, &Filters::default()).unwrap();
        assert_eq!(dark.len(), 3);
        assert_eq!(dark[2].sample.name, "S3", "the bright tone is last for \"dark\": {:?}", dark.iter().map(|h| &h.sample.name).collect::<Vec<_>>());
        let bright = searcher.search(libs, clap.as_ref(), "bright", 3, &Filters::default()).unwrap();
        assert_eq!(bright[0].sample.name, "S3");
        assert!(bright[0].cosine.unwrap() > bright[2].cosine.unwrap());

        // "Similar to S1": S2 (a near pitch) before S3, with CLAP in the mix as well.
        let mut finder = SimilarFinder::new();
        let s1 = dark.iter().find(|h| h.sample.name == "S1").unwrap().sample.id;
        let hits = finder.find_blended(libs, 1, s1, &DspEmbedder, Some(clap.as_ref()), 5).unwrap();
        assert_eq!(hits.iter().map(|h| h.sample.name.as_str()).collect::<Vec<_>>(), vec!["S2", "S3"], "{hits:?}");
        assert!(hits[0].similarity > hits[1].similarity);
        // Without CLAP the DSP list stands.
        let plain = finder.find_blended(libs, 1, s1, &DspEmbedder, None, 5).unwrap();
        assert_eq!(plain.len(), 2);
        assert!(finder.find_blended(libs, 1, SampleId(9_999), &DspEmbedder, Some(clap.as_ref()), 5).is_err());
    }
}
