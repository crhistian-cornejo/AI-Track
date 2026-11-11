//! Audio analysis and local machine learning for AI-Track.
//!
//! * Pure DSP, no model needed: [`analyze`] (key, BPM, loop/one-shot, loudness,
//!   features, category), [`detect_key`], [`detect_bpm`], [`detect_chords`],
//!   monophonic and drum audio-to-MIDI, and the rule-based [`theory`] helpers.
//! * Models downloaded on demand by the [`ModelManager`] and run with ONNX
//!   Runtime (loaded dynamically, see [`runtime`]): polyphonic audio-to-MIDI
//!   with Spotify's basic-pitch ([`extract_midi`]) and Demucs stem separation
//!   ([`split_stems`]).
//!
//! Nothing here may run on the audio thread. Long jobs take a progress
//! callback and a [`CancelToken`]; [`worker`] runs them on background threads
//! or in the separate `aitrack-ml-worker` process.

pub mod analysis;
pub mod audio;
pub mod chords;
pub mod classify;
pub mod dsp;
mod error;
pub mod features;
pub mod key;
pub mod midi;
pub mod models;
pub mod progress;
pub mod runtime;
pub mod stems;
pub mod tempo;
pub mod theory;
pub mod worker;

pub use analysis::{RootNote, SampleAnalysis, analyze, analyze_audio};
pub use chords::{ChordSegment, detect_chords};
pub use classify::SampleCategory;
pub use error::{Error, Result};
pub use key::{KeyEstimate, detect_key};
pub use midi::{MidiEngine, MidiOptions, extract_midi};
pub use models::{ModelEntry, ModelManager};
pub use progress::{CancelToken, Progress, Stage};
pub use stems::{StemOptions, split_stems};
pub use tempo::{BpmEstimate, detect_bpm};
