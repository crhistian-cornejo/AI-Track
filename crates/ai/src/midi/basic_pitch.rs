//! Spotify basic-pitch (ICASSP 2022, Apache-2.0) pre- and post-processing.
//!
//! The model (`nmp.onnx`) takes 2 s windows of 22.05 kHz mono audio and
//! returns, per window, 172 frames (≈86 fps) of note / onset activations for
//! the 88 piano keys and a 264-bin (⅓ semitone) pitch contour. This module
//! reproduces the reference pipeline: windowing with 30 overlapping frames,
//! unwrapping, then the note-creation algorithm (onset peaks, energy-tracked
//! note ends, the "melodia trick" and optional pitch bends).

use aitrack_model::Note;

use super::{MidiOptions, NoteEvent};
use crate::audio::{AudioData, resample_channel};
use crate::dsp::{hz_to_midi, midi_to_hz};
use crate::models::{ModelManager, ids};
use crate::progress::{CancelToken, Progress, Stage};
use crate::runtime::{self, Accel};
use crate::{Error, Result};

pub const SAMPLE_RATE: u32 = 22_050;
pub const FFT_HOP: usize = 256;
/// `22050 // 256` (integer division, as in the reference).
pub const ANNOTATIONS_FPS: usize = SAMPLE_RATE as usize / FFT_HOP;
pub const AUDIO_WINDOW_SECONDS: usize = 2;
pub const ANNOT_N_FRAMES: usize = ANNOTATIONS_FPS * AUDIO_WINDOW_SECONDS; // 172
pub const AUDIO_N_SAMPLES: usize = SAMPLE_RATE as usize * AUDIO_WINDOW_SECONDS - FFT_HOP; // 43844
pub const N_OVERLAPPING_FRAMES: usize = 30;
pub const OVERLAP_LEN: usize = N_OVERLAPPING_FRAMES * FFT_HOP;
pub const HOP_SIZE: usize = AUDIO_N_SAMPLES - OVERLAP_LEN;
pub const N_NOTES: usize = 88;
pub const CONTOUR_BINS_PER_SEMITONE: usize = 3;
pub const N_CONTOUR_BINS: usize = N_NOTES * CONTOUR_BINS_PER_SEMITONE;
pub const MIDI_OFFSET: usize = 21;
const MAX_FREQ_IDX: usize = 87;
const ENERGY_TOL: usize = 11;

/// Tensor names in the official `nmp.onnx` export.
pub const INPUT_NAME: &str = "serving_default_input_2:0";
pub const OUTPUT_CONTOUR: &str = "StatefulPartitionedCall:0";
pub const OUTPUT_NOTE: &str = "StatefulPartitionedCall:1";
pub const OUTPUT_ONSET: &str = "StatefulPartitionedCall:2";

/// Row-major `rows × cols` matrix (time × frequency).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mat {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f32>,
}

impl Mat {
    pub fn zeros(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            data: vec![0.0; rows * cols],
        }
    }
    #[inline]
    pub fn at(&self, r: usize, c: usize) -> f32 {
        self.data[r * self.cols + c]
    }
    #[inline]
    pub fn set(&mut self, r: usize, c: usize, v: f32) {
        self.data[r * self.cols + c] = v;
    }
    pub fn max(&self) -> f32 {
        self.data.iter().copied().fold(f32::MIN, f32::max)
    }
}

/// Model outputs after unwrapping: `note`/`onset` are frames × 88, `contour` frames × 264.
#[derive(Clone, Debug, Default)]
pub struct Activations {
    pub note: Mat,
    pub onset: Mat,
    pub contour: Mat,
}

/// Split 22.05 kHz mono audio into model windows: pad `OVERLAP_LEN / 2` zeros
/// in front, then `AUDIO_N_SAMPLES`-long windows every `HOP_SIZE` samples, the
/// last one zero-padded.
pub fn windows(audio: &[f32]) -> Vec<Vec<f32>> {
    let mut padded = vec![0.0f32; OVERLAP_LEN / 2];
    padded.extend_from_slice(audio);
    let mut out = Vec::new();
    let mut i = 0;
    while i < padded.len() {
        let mut w = padded[i..(i + AUDIO_N_SAMPLES).min(padded.len())].to_vec();
        w.resize(AUDIO_N_SAMPLES, 0.0);
        out.push(w);
        i += HOP_SIZE;
    }
    out
}

/// Concatenate per-window outputs (each `ANNOT_N_FRAMES × cols`), dropping half
/// the overlap from both ends of every window, trimmed to the audio length.
pub fn unwrap(per_window: &[Vec<f32>], cols: usize, original_len: usize) -> Mat {
    let n_olap = N_OVERLAPPING_FRAMES / 2;
    let keep = ANNOT_N_FRAMES - 2 * n_olap;
    let n_frames = original_len * ANNOTATIONS_FPS / SAMPLE_RATE as usize;
    let mut data = Vec::with_capacity(per_window.len() * keep * cols);
    for w in per_window {
        let frames = w.len() / cols;
        let end = frames.saturating_sub(n_olap);
        for t in n_olap.min(end)..end {
            data.extend_from_slice(&w[t * cols..(t + 1) * cols]);
        }
    }
    let rows = (data.len() / cols).min(n_frames);
    data.truncate(rows * cols);
    Mat { rows, cols, data }
}

/// Frame index → seconds, including the reference implementation's per-window
/// offset correction.
pub fn frame_times(n_frames: usize) -> Vec<f64> {
    let hop_s = FFT_HOP as f64 / SAMPLE_RATE as f64;
    let window_offset =
        hop_s * (ANNOT_N_FRAMES as f64 - AUDIO_N_SAMPLES as f64 / FFT_HOP as f64) + 0.0018;
    (0..n_frames)
        .map(|i| i as f64 * hop_s - window_offset * (i / ANNOT_N_FRAMES) as f64)
        .collect()
}

/// Parameters of the note-creation step.
#[derive(Clone, Debug)]
pub struct NoteParams {
    pub onset_thresh: f32,
    pub frame_thresh: f32,
    /// Minimum note length in frames.
    pub min_note_len: usize,
    pub infer_onsets: bool,
    pub min_freq: Option<f32>,
    pub max_freq: Option<f32>,
    pub melodia_trick: bool,
    pub pitch_bends: bool,
}

impl NoteParams {
    pub fn from_options(o: &MidiOptions) -> Self {
        let fps = SAMPLE_RATE as f32 / FFT_HOP as f32;
        Self {
            onset_thresh: o.onset_threshold,
            frame_thresh: o.frame_threshold,
            min_note_len: (o.min_note_ms / 1000.0 * fps).round() as usize,
            infer_onsets: o.infer_onsets,
            min_freq: (o.min_note > 0).then(|| midi_to_hz(o.min_note as f32)),
            max_freq: (o.max_note < 127).then(|| midi_to_hz(o.max_note as f32 + 1.0)),
            melodia_trick: o.melodia_trick,
            pitch_bends: o.pitch_bends,
        }
    }
}

/// Zero activations outside `[min_freq, max_freq)`.
fn constrain_frequency(
    onsets: &mut Mat,
    frames: &mut Mat,
    max_freq: Option<f32>,
    min_freq: Option<f32>,
) {
    let zero_cols = |m: &mut Mat, range: std::ops::Range<usize>| {
        for r in 0..m.rows {
            for c in range.clone() {
                m.set(r, c, 0.0);
            }
        }
    };
    if let Some(f) = max_freq {
        let idx = ((hz_to_midi(f) - MIDI_OFFSET as f32).round().max(0.0) as usize).min(N_NOTES);
        zero_cols(onsets, idx..N_NOTES);
        zero_cols(frames, idx..N_NOTES);
    }
    if let Some(f) = min_freq {
        let idx = ((hz_to_midi(f) - MIDI_OFFSET as f32).round().max(0.0) as usize).min(N_NOTES);
        zero_cols(onsets, 0..idx);
        zero_cols(frames, 0..idx);
    }
}

/// Add onsets where frame activations rise sharply (min over 1- and 2-frame
/// differences), rescaled to the onset maximum.
fn infer_onsets(onsets: &Mat, frames: &Mat) -> Mat {
    let n_diff = 2;
    let mut diff = Mat::zeros(frames.rows, frames.cols);
    for t in 0..frames.rows {
        for c in 0..frames.cols {
            let mut m = f32::MAX;
            for n in 1..=n_diff {
                let prev = if t >= n { frames.at(t - n, c) } else { 0.0 };
                m = m.min(frames.at(t, c) - prev);
            }
            diff.set(t, c, if t < n_diff { 0.0 } else { m.max(0.0) });
        }
    }
    let dmax = diff.max();
    let omax = onsets.max();
    let mut out = onsets.clone();
    if dmax > 0.0 {
        for (o, d) in out.data.iter_mut().zip(&diff.data) {
            *o = o.max(omax * d / dmax);
        }
    }
    out
}

/// Note events in frames: `(start, end, midi, amplitude)`.
pub fn output_to_notes_polyphonic(
    frames: &Mat,
    onsets: &Mat,
    p: &NoteParams,
) -> Vec<(usize, usize, u8, f32)> {
    let n_frames = frames.rows;
    let mut frames = frames.clone();
    let mut onsets = onsets.clone();
    constrain_frequency(&mut onsets, &mut frames, p.max_freq, p.min_freq);
    if p.infer_onsets {
        onsets = infer_onsets(&onsets, &frames);
    }
    // Local maxima along time (strict, edges excluded) above the onset threshold.
    let mut peaks: Vec<(usize, usize)> = Vec::new();
    for t in 1..n_frames.saturating_sub(1) {
        for c in 0..onsets.cols {
            let v = onsets.at(t, c);
            if v > onsets.at(t - 1, c) && v > onsets.at(t + 1, c) && v >= p.onset_thresh {
                peaks.push((t, c));
            }
        }
    }
    // Walk onsets backwards in time.
    peaks.reverse();
    let mut remaining = frames.clone();
    let mut notes = Vec::new();
    let clear = |m: &mut Mat, t0: usize, t1: usize, c: usize| {
        for t in t0..t1 {
            m.set(t, c, 0.0);
            if c < MAX_FREQ_IDX {
                m.set(t, c + 1, 0.0);
            }
            if c > 0 {
                m.set(t, c - 1, 0.0);
            }
        }
    };
    for (start, f) in peaks {
        if start >= n_frames - 1 {
            continue;
        }
        let mut i = start + 1;
        let mut k = 0;
        while i < n_frames - 1 && k < ENERGY_TOL {
            if remaining.at(i, f) < p.frame_thresh {
                k += 1;
            } else {
                k = 0;
            }
            i += 1;
        }
        i -= k;
        if i - start <= p.min_note_len {
            continue;
        }
        clear(&mut remaining, start, i, f);
        let amp = (start..i).map(|t| frames.at(t, f)).sum::<f32>() / (i - start) as f32;
        notes.push((start, i, (f + MIDI_OFFSET) as u8, amp));
    }

    if p.melodia_trick {
        loop {
            let (mut best, mut bi) = (f32::MIN, 0usize);
            for (j, v) in remaining.data.iter().enumerate() {
                if *v > best {
                    best = *v;
                    bi = j;
                }
            }
            if best <= p.frame_thresh {
                break;
            }
            let (i_mid, f) = (bi / remaining.cols, bi % remaining.cols);
            remaining.set(i_mid, f, 0.0);
            // Forward.
            let mut i = i_mid + 1;
            let mut k = 0;
            while i < n_frames - 1 && k < ENERGY_TOL {
                if remaining.at(i, f) < p.frame_thresh {
                    k += 1;
                } else {
                    k = 0;
                }
                clear(&mut remaining, i, i + 1, f);
                i += 1;
            }
            let i_end = i - 1 - k;
            // Backward.
            let mut i = i_mid as isize - 1;
            let mut k = 0usize;
            while i > 0 && k < ENERGY_TOL {
                if remaining.at(i as usize, f) < p.frame_thresh {
                    k += 1;
                } else {
                    k = 0;
                }
                clear(&mut remaining, i as usize, i as usize + 1, f);
                i -= 1;
            }
            let i_start = (i + 1 + k as isize).max(0) as usize;
            if i_end <= i_start || i_end - i_start <= p.min_note_len {
                continue;
            }
            let amp =
                (i_start..i_end).map(|t| frames.at(t, f)).sum::<f32>() / (i_end - i_start) as f32;
            notes.push((i_start, i_end, (f + MIDI_OFFSET) as u8, amp));
        }
    }
    notes
}

/// Median pitch bend of a note in contour bins (⅓ semitone), using the
/// reference's Gaussian-weighted ±25-bin search.
fn pitch_bend(contour: &Mat, start: usize, end: usize, midi: u8) -> Option<f32> {
    let tol: isize = 25;
    let win = (2 * tol + 1) as usize;
    let gauss: Vec<f32> = (0..win)
        .map(|i| (-0.5 * ((i as f32 - tol as f32) / 5.0).powi(2)).exp())
        .collect();
    let freq_idx = (CONTOUR_BINS_PER_SEMITONE as isize) * (midi as isize - MIDI_OFFSET as isize);
    let f_start = (freq_idx - tol).max(0);
    let f_end = (freq_idx + tol + 1).min(N_CONTOUR_BINS as isize);
    let g_start = (tol - freq_idx).max(0);
    let shift = tol - (tol - freq_idx).max(0);
    let mut bends: Vec<f32> = Vec::new();
    for t in start..end.min(contour.rows) {
        let mut best = (f32::MIN, 0isize);
        for (j, f) in (f_start..f_end).enumerate() {
            let v = contour.at(t, f as usize) * gauss[(g_start as usize + j).min(win - 1)];
            if v > best.0 {
                best = (v, j as isize);
            }
        }
        bends.push((best.1 - shift) as f32);
    }
    if bends.is_empty() {
        return None;
    }
    Some(crate::dsp::median(&mut bends))
}

/// Full note creation from activations, in seconds.
pub fn activations_to_events(act: &Activations, p: &NoteParams) -> Vec<NoteEvent> {
    let notes = output_to_notes_polyphonic(&act.note, &act.onset, p);
    let times = frame_times(act.contour.rows.max(act.note.rows));
    let t = |i: usize| {
        times
            .get(i)
            .copied()
            .unwrap_or_else(|| *times.last().unwrap_or(&0.0))
    };
    let mut events: Vec<NoteEvent> = notes
        .into_iter()
        .map(|(s, e, pitch, amp)| NoteEvent {
            start: t(s).max(0.0),
            end: t(e),
            pitch,
            amplitude: amp,
            bend_cents: if p.pitch_bends && act.contour.rows > 0 {
                pitch_bend(&act.contour, s, e, pitch)
                    .map(|b| b * 100.0 / CONTOUR_BINS_PER_SEMITONE as f32)
            } else {
                None
            },
        })
        .collect();
    events.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    events
}

/// Run the model on 22.05 kHz mono audio.
pub fn infer(
    session: &mut ort::session::Session,
    audio: &[f32],
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<Activations> {
    let wins = windows(audio);
    let input_name = session
        .inputs()
        .iter()
        .find(|i| i.name() == INPUT_NAME)
        .or_else(|| session.inputs().first())
        .map(|i| i.name().to_string())
        .ok_or_else(|| Error::ModelOutput("model has no inputs".into()))?;
    let out_names: Vec<String> = session
        .outputs()
        .iter()
        .map(|o| o.name().to_string())
        .collect();
    let pick = |want: &str, idx: usize| -> String {
        if out_names.iter().any(|n| n == want) {
            want.to_string()
        } else {
            out_names.get(idx).cloned().unwrap_or_default()
        }
    };
    let (n_note, n_onset, n_contour) = (
        pick(OUTPUT_NOTE, 1),
        pick(OUTPUT_ONSET, 2),
        pick(OUTPUT_CONTOUR, 0),
    );
    let (mut notes, mut onsets, mut contours) = (Vec::new(), Vec::new(), Vec::new());
    let total = wins.len();
    for (i, w) in wins.into_iter().enumerate() {
        cancel.check()?;
        progress(Progress::new(
            Stage::Inference,
            0.1 + 0.8 * i as f32 / total as f32,
            format!("Transcribing {}/{}", i + 1, total),
        ));
        let tensor = ort::value::Tensor::from_array(([1usize, AUDIO_N_SAMPLES, 1], w))?;
        let outputs = session.run(ort::inputs![input_name.as_str() => tensor])?;
        let grab = |name: &str| -> Result<Vec<f32>> {
            let v = outputs
                .get(name)
                .ok_or_else(|| Error::ModelOutput(format!("missing output {name}")))?;
            let (_, data) = v.try_extract_tensor::<f32>()?;
            Ok(data.to_vec())
        };
        notes.push(grab(&n_note)?);
        onsets.push(grab(&n_onset)?);
        contours.push(grab(&n_contour)?);
    }
    for (name, v, cols) in [
        ("note", &notes, N_NOTES),
        ("onset", &onsets, N_NOTES),
        ("contour", &contours, N_CONTOUR_BINS),
    ] {
        if v.iter().any(|w| w.len() != ANNOT_N_FRAMES * cols) {
            return Err(Error::ModelOutput(format!(
                "{name} output is not {ANNOT_N_FRAMES}×{cols}"
            )));
        }
    }
    let len = audio.len();
    Ok(Activations {
        note: unwrap(&notes, N_NOTES, len),
        onset: unwrap(&onsets, N_NOTES, len),
        contour: unwrap(&contours, N_CONTOUR_BINS, len),
    })
}

/// Transcribe decoded audio with basic-pitch, downloading model and runtime if needed.
pub fn transcribe(
    manager: &ModelManager,
    audio: &AudioData,
    opts: &MidiOptions,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<Vec<NoteEvent>> {
    let mono = resample_channel(&audio.to_mono(), audio.sample_rate, SAMPLE_RATE)?;
    progress(Progress::new(
        Stage::LoadingModel,
        0.02,
        "Loading basic-pitch",
    ));
    let model = manager.ensure(ids::BASIC_PITCH, &mut |p| {
        progress(Progress {
            fraction: p.fraction * 0.05,
            ..p
        })
    })?;
    let mut session = runtime::session(manager, &model.paths[0], Accel::Cpu, &mut |_| {})?;
    let act = infer(&mut session, &mono, progress, cancel)?;
    progress(Progress::new(Stage::PostProcessing, 0.92, "Creating notes"));
    Ok(activations_to_events(&act, &NoteParams::from_options(opts)))
}

/// Convenience used by tests and the worker: transcribe straight to notes.
pub fn transcribe_to_notes(
    manager: &ModelManager,
    audio: &AudioData,
    opts: &MidiOptions,
) -> Result<Vec<Note>> {
    let ev = transcribe(manager, audio, opts, &mut |_| {}, &CancelToken::new())?;
    Ok(super::events_to_notes(&ev, opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_reference() {
        assert_eq!(ANNOTATIONS_FPS, 86);
        assert_eq!(ANNOT_N_FRAMES, 172);
        assert_eq!(AUDIO_N_SAMPLES, 43_844);
        assert_eq!(HOP_SIZE, 36_164);
        let p = NoteParams::from_options(&MidiOptions::default());
        assert_eq!(p.min_note_len, 11);
    }

    #[test]
    fn windowing_and_unwrap_preserve_length() {
        let len = 5 * SAMPLE_RATE as usize;
        let w = windows(&vec![0.1; len]);
        assert_eq!(w.len(), (len + OVERLAP_LEN / 2).div_ceil(HOP_SIZE));
        let fake: Vec<Vec<f32>> = (0..w.len())
            .map(|i| vec![i as f32; ANNOT_N_FRAMES * 2])
            .collect();
        let m = unwrap(&fake, 2, len);
        assert_eq!(m.rows, len * 86 / 22_050);
        // Each window contributes 142 frames.
        assert_eq!(m.at(141, 0), 0.0);
        assert_eq!(m.at(142, 0), 1.0);
    }

    #[test]
    fn frame_times_follow_reference_offsets() {
        let t = frame_times(400);
        assert!((t[86] - 86.0 * 256.0 / 22_050.0).abs() < 1e-9);
        let off = 256.0 / 22_050.0 * (172.0 - 43_844.0 / 256.0) + 0.0018;
        assert!((t[172] - (172.0 * 256.0 / 22_050.0 - off)).abs() < 1e-9);
    }

    fn synthetic(n: usize, spans: &[(usize, usize, usize)]) -> Activations {
        let mut note = Mat::zeros(n, N_NOTES);
        let mut onset = Mat::zeros(n, N_NOTES);
        for &(s, e, key) in spans {
            let c = key - MIDI_OFFSET;
            for t in s..e {
                note.set(t, c, 0.8);
            }
            onset.set(s, c, 0.9);
        }
        Activations {
            note,
            onset,
            contour: Mat::zeros(n, N_CONTOUR_BINS),
        }
    }

    #[test]
    fn note_creation_from_activations() {
        let act = synthetic(300, &[(10, 60, 60), (70, 120, 64), (70, 120, 67)]);
        let p = NoteParams::from_options(&MidiOptions::default());
        let ev = activations_to_events(&act, &p);
        let pitches: Vec<u8> = ev.iter().map(|e| e.pitch).collect();
        assert_eq!(pitches, vec![60, 64, 67]);
        let fps = 22_050.0 / 256.0;
        assert!((ev[0].start - 10.0 / fps).abs() < 1e-6);
        assert!((ev[0].end - 60.0 / fps).abs() < 1e-6);
        assert!((ev[0].amplitude - 0.8).abs() < 1e-6);
    }

    #[test]
    fn short_notes_dropped_and_melodia_recovers_onsetless() {
        let mut act = synthetic(300, &[(10, 18, 60)]);
        // A sustained note with no onset activation at all.
        for t in 100..200 {
            act.note.set(t, 72 - MIDI_OFFSET, 0.7);
        }
        let mut p = NoteParams::from_options(&MidiOptions::default());
        p.infer_onsets = false;
        let ev = activations_to_events(&act, &p);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0].pitch, 72);
        p.melodia_trick = false;
        assert!(activations_to_events(&act, &p).is_empty());
    }

    #[test]
    fn range_limits_apply() {
        let act = synthetic(200, &[(10, 60, 40), (10, 60, 80)]);
        let opts = MidiOptions {
            min_note: 48,
            max_note: 90,
            ..Default::default()
        };
        let ev = activations_to_events(&act, &NoteParams::from_options(&opts));
        assert_eq!(ev.iter().map(|e| e.pitch).collect::<Vec<_>>(), vec![80]);
    }

    /// Downloads basic-pitch (230 KB) and ONNX Runtime, then transcribes a
    /// synthesized C major arpeggio and chord.
    #[test]
    #[ignore = "network: downloads the model and ONNX Runtime"]
    fn basic_pitch_end_to_end() {
        use crate::dsp::synth;
        let sr = 44_100;
        let mut x = Vec::new();
        // C4 E4 G4 C5, 0.5 s each, piano-like decay, then a C major chord for 1 s.
        for m in [60.0f32, 64.0, 67.0, 72.0] {
            let t: Vec<f32> = synth::tone(midi_to_hz(m), 0.5, sr, &[0.5, 0.25, 0.12])
                .into_iter()
                .enumerate()
                .map(|(i, s)| s * (-(i as f32) / sr as f32 * 3.0).exp())
                .collect();
            x.extend(t);
        }
        let mut chord = vec![0.0f32; sr as usize];
        for m in [60.0f32, 64.0, 67.0] {
            for (d, s) in
                chord
                    .iter_mut()
                    .zip(synth::tone(midi_to_hz(m), 1.0, sr, &[0.3, 0.15, 0.07]))
            {
                *d += s;
            }
        }
        x.extend(chord);
        let mm = ModelManager::with_dir(crate::models::test_models_dir());
        let audio = AudioData::mono_signal(x, sr);
        let opts = MidiOptions {
            quantize: Some(super::super::SIXTEENTH),
            ..Default::default()
        };
        let notes = transcribe_to_notes(&mm, &audio, &opts).unwrap();
        eprintln!(
            "{:?}",
            notes
                .iter()
                .map(|n| (n.key, n.start, n.length))
                .collect::<Vec<_>>()
        );
        let melody: Vec<u8> = notes
            .iter()
            .filter(|n| n.start < 4 * aitrack_model::PPQ)
            .map(|n| n.key)
            .collect();
        assert_eq!(melody, vec![60, 64, 67, 72]);
        let mut chord_keys: Vec<u8> = notes
            .iter()
            .filter(|n| n.start >= 4 * aitrack_model::PPQ)
            .map(|n| n.key)
            .collect();
        chord_keys.sort();
        chord_keys.dedup();
        // basic-pitch may add faint harmonic ghosts (e.g. G4's 3rd harmonic as D6); the triad must be there.
        assert!(
            [60, 64, 67].iter().all(|k| chord_keys.contains(k)),
            "{chord_keys:?}"
        );
    }
}
