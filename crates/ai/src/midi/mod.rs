//! Audio → MIDI. Three engines:
//!
//! * [`MidiEngine::BasicPitch`]: Spotify basic-pitch (polyphonic, any
//!   instrument), ONNX model downloaded on demand.
//! * [`MidiEngine::Monophonic`]: YIN pitch tracking + segmentation, no model.
//! * [`MidiEngine::Drums`]: onset detection + spectral band classification to
//!   General MIDI kick/snare/hat, no model.
//!
//! All engines produce [`NoteEvent`]s in seconds; [`events_to_notes`] turns
//! them into project [`Note`]s at a tempo, with quantise / scale snap / range.

pub mod basic_pitch;
pub mod drums;
pub mod mono;

use std::path::Path;

use aitrack_model::{Note, NoteId, PPQ, Scale, TempoMap, Tick};
use serde::{Deserialize, Serialize};

use crate::models::ModelManager;
use crate::progress::{CancelToken, Progress, Stage};
use crate::{Result, audio};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MidiEngine {
    #[default]
    BasicPitch,
    Monophonic,
    Drums,
}

/// Options shared by all engines. Thresholds and the minimum length default
/// to basic-pitch's own defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiOptions {
    pub engine: MidiEngine,
    /// Tempo used to convert seconds to ticks.
    pub bpm: f64,
    /// basic-pitch: minimum onset activation (0..1).
    pub onset_threshold: f32,
    /// basic-pitch: minimum frame activation for a note to stay on (0..1).
    pub frame_threshold: f32,
    /// Notes shorter than this are dropped, milliseconds.
    pub min_note_ms: f32,
    /// Quantise starts and ends to this grid (ticks; e.g. `PPQ / 4` = 1/16).
    pub quantize: Option<Tick>,
    /// Move out-of-scale notes to the nearest scale tone.
    pub snap_to_scale: Option<Scale>,
    /// Keep only notes in this MIDI range (inclusive).
    pub min_note: u8,
    pub max_note: u8,
    /// basic-pitch: store the median pitch bend of each note in `Note::fine_pitch`.
    pub pitch_bends: bool,
    /// basic-pitch: add onsets where frame activations jump.
    pub infer_onsets: bool,
    /// basic-pitch: also create notes from strong activations without onsets.
    pub melodia_trick: bool,
}

impl Default for MidiOptions {
    fn default() -> Self {
        Self {
            engine: MidiEngine::BasicPitch,
            bpm: 120.0,
            onset_threshold: 0.5,
            frame_threshold: 0.3,
            min_note_ms: 127.70,
            quantize: None,
            snap_to_scale: None,
            min_note: 0,
            max_note: 127,
            pitch_bends: false,
            infer_onsets: true,
            melodia_trick: true,
        }
    }
}

/// A transcribed note in seconds, before tempo conversion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteEvent {
    pub start: f64,
    pub end: f64,
    pub pitch: u8,
    /// 0..1
    pub amplitude: f32,
    /// Median pitch bend relative to `pitch`, cents.
    pub bend_cents: Option<f32>,
}

/// Convert events to project notes: range filter, scale snap, seconds → ticks,
/// optional quantise, overlap clean-up. Note ids are local (1..=n); the caller
/// allocates real ids when inserting through commands.
pub fn events_to_notes(events: &[NoteEvent], opts: &MidiOptions) -> Vec<Note> {
    let map = TempoMap::constant(opts.bpm);
    let mut notes: Vec<Note> = Vec::with_capacity(events.len());
    for e in events {
        if e.pitch < opts.min_note || e.pitch > opts.max_note || e.end <= e.start {
            continue;
        }
        let key = opts.snap_to_scale.map_or(e.pitch, |s| s.snap(e.pitch));
        let mut start = map.seconds_to_tick(e.start).round() as Tick;
        let mut end = map.seconds_to_tick(e.end).round() as Tick;
        if let Some(q) = opts.quantize.filter(|q| *q > 0) {
            start = (start as f64 / q as f64).round() as Tick * q;
            end = ((end as f64 / q as f64).round() as Tick * q).max(start + q);
        }
        let mut n = Note::new(
            NoteId(0),
            key,
            start.max(0),
            (end - start).max(1),
            e.amplitude.clamp(0.0, 1.0),
        );
        if opts.pitch_bends && opts.snap_to_scale.is_none() {
            n.fine_pitch = e.bend_cents.unwrap_or(0.0);
        }
        notes.push(n);
    }
    notes.sort_by(|a, b| a.start.cmp(&b.start).then(a.key.cmp(&b.key)));
    // Same key overlapping itself (common after quantise or scale snap): merge.
    let mut out: Vec<Note> = Vec::with_capacity(notes.len());
    for n in notes {
        if let Some(prev) = out
            .iter_mut()
            .rev()
            .find(|p| p.key == n.key && p.end() > n.start)
        {
            if n.start == prev.start {
                prev.length = prev.length.max(n.length);
                prev.velocity = prev.velocity.max(n.velocity);
                continue;
            }
            prev.length = n.start - prev.start;
        }
        out.push(n);
    }
    for (i, n) in out.iter_mut().enumerate() {
        n.id = NoteId(i as u64 + 1);
    }
    out
}

/// One sixteenth note in ticks, the usual quantise grid.
pub const SIXTEENTH: Tick = PPQ / 4;

/// Transcribe a file. basic-pitch downloads its model (Apache-2.0, ~230 KB)
/// and the ONNX Runtime library on first use.
pub fn extract_midi(
    path: impl AsRef<Path>,
    opts: &MidiOptions,
    progress: &mut dyn FnMut(Progress),
) -> Result<Vec<Note>> {
    let manager = ModelManager::new()?;
    extract_midi_with(&manager, path.as_ref(), opts, progress, &CancelToken::new())
}

pub fn extract_midi_with(
    manager: &ModelManager,
    path: &Path,
    opts: &MidiOptions,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<Vec<Note>> {
    progress(Progress::new(Stage::Decoding, 0.0, "Decoding audio"));
    let decoded = audio::load(path)?;
    cancel.check()?;
    let events = match opts.engine {
        MidiEngine::BasicPitch => {
            basic_pitch::transcribe(manager, &decoded, opts, progress, cancel)?
        }
        MidiEngine::Monophonic => mono::transcribe(&decoded.to_mono(), decoded.sample_rate, opts),
        MidiEngine::Drums => drums::transcribe(&decoded.to_mono(), decoded.sample_rate, opts),
    };
    progress(Progress::new(
        Stage::Done,
        1.0,
        format!("{} notes", events.len()),
    ));
    Ok(events_to_notes(&events, opts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{Key, ScaleKind};

    fn ev(start: f64, end: f64, pitch: u8) -> NoteEvent {
        NoteEvent {
            start,
            end,
            pitch,
            amplitude: 0.8,
            bend_cents: Some(12.0),
        }
    }

    #[test]
    fn converts_quantises_snaps_and_filters() {
        let opts = MidiOptions {
            bpm: 120.0,
            quantize: Some(SIXTEENTH),
            snap_to_scale: Some(Scale::new(Key(0), ScaleKind::Major)),
            min_note: 40,
            ..Default::default()
        };
        // 0.51 s at 120 BPM = 979 ticks -> quantised to 960.
        let notes = events_to_notes(
            &[ev(0.51, 0.99, 61), ev(0.0, 0.5, 30), ev(1.0, 1.5, 64)],
            &opts,
        );
        assert_eq!(notes.len(), 2);
        assert_eq!(
            (notes[0].key, notes[0].start, notes[0].length),
            (60, 960, 960)
        );
        assert_eq!((notes[1].key, notes[1].start), (64, 1920));
        assert_eq!(notes[0].id, NoteId(1));
    }

    #[test]
    fn overlapping_same_key_is_trimmed() {
        let opts = MidiOptions::default();
        let notes = events_to_notes(&[ev(0.0, 1.0, 60), ev(0.5, 1.5, 60)], &opts);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].end(), notes[1].start);
    }

    #[test]
    fn pitch_bends_go_to_fine_pitch() {
        let opts = MidiOptions {
            pitch_bends: true,
            ..Default::default()
        };
        let notes = events_to_notes(&[ev(0.0, 1.0, 60)], &opts);
        assert_eq!(notes[0].fine_pitch, 12.0);
    }
}
