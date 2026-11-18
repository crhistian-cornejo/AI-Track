//! [`AiDetector`]: key and tempo from audio through `aitrack-ai` (feature
//! `ai`). Pure DSP; no model download and no ONNX Runtime involved.

use std::path::Path;

use crate::error::{LibraryError, Result};
use crate::parse::KeyHint;
use crate::reanalyse::{Detection, KeyBpmDetector};
use crate::similar::Embedder;

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
}
