//! [`AiDetector`]: key and tempo from audio through `aitrack-ai` (feature
//! `ai`). Pure DSP; no model download and no ONNX Runtime involved.

use std::path::Path;

use crate::error::{LibraryError, Result};
use crate::parse::KeyHint;
use crate::reanalyse::{Detection, KeyBpmDetector};

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
}
