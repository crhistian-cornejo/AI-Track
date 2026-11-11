//! Model-free monophonic transcription: YIN pitch track, voicing by
//! periodicity and level, segmentation at pitch changes, gaps and onsets.

use super::{MidiOptions, NoteEvent};
use crate::audio::resample_channel;
use crate::dsp::{ANALYSIS_RATE, OnsetEnvelope, Yin, db, median, rms};

const HOP: usize = 128;

#[derive(Clone, Copy)]
struct Frame {
    midi: Option<f32>,
    level: f32,
}

pub fn transcribe(samples: &[f32], sample_rate: u32, opts: &MidiOptions) -> Vec<NoteEvent> {
    let Ok(x) = resample_channel(samples, sample_rate, ANALYSIS_RATE) else {
        return Vec::new();
    };
    let sr = ANALYSIS_RATE as f64;
    let fmin = crate::dsp::midi_to_hz(opts.min_note.max(24) as f32).max(30.0);
    let fmax = crate::dsp::midi_to_hz(opts.max_note.min(108) as f32).min(4000.0);
    let yin = Yin::new(ANALYSIS_RATE, fmin * 0.95, fmax * 1.05, 1024);
    let flen = yin.frame_len();
    let n = if x.len() > flen {
        (x.len() - flen) / HOP + 1
    } else {
        0
    };
    let levels: Vec<f32> = (0..n).map(|i| rms(&x[i * HOP..i * HOP + flen])).collect();
    let max_level = levels.iter().copied().fold(0.0f32, f32::max);
    if max_level <= 1e-6 {
        return Vec::new();
    }
    let gate = max_level * 10f32.powf(-40.0 / 20.0);
    let frames: Vec<Frame> = (0..n)
        .map(|i| {
            let level = levels[i];
            let midi = if level > gate {
                yin.estimate(&x[i * HOP..i * HOP + flen])
                    .filter(|p| p.aperiodicity < 0.25)
                    .map(|p| p.midi())
            } else {
                None
            };
            Frame { midi, level }
        })
        .collect();
    // Frame i's analysis window is centred at i*HOP + flen/2; report that time.
    let time = |i: usize| (i * HOP + flen / 2) as f64 / sr;

    // Median-smoothed integer pitch per frame (5 frames), unvoiced = None.
    let rounded: Vec<Option<i32>> = (0..n)
        .map(|i| {
            frames[i].midi?;
            let mut v: Vec<f32> = (i.saturating_sub(2)..(i + 3).min(n))
                .filter_map(|j| frames[j].midi)
                .collect();
            Some(median(&mut v).round() as i32)
        })
        .collect();

    // Onsets split repeated notes of the same pitch.
    let env = OnsetEnvelope::compute(&x, ANALYSIS_RATE, 1024, HOP);
    let onset_frames: Vec<usize> = env
        .peaks(0.3, 0.06)
        .into_iter()
        // Envelope frame t starts at t*HOP; map to the YIN frame whose centre is nearest.
        .map(|t| (t * HOP + 512).saturating_sub(flen / 2) / HOP)
        .collect();

    let min_frames = ((opts.min_note_ms as f64 / 1000.0) * sr / HOP as f64)
        .round()
        .max(2.0) as usize;
    let hysteresis = 3;
    let mut events = Vec::new();
    let mut i = 0;
    while i < n {
        let Some(p) = rounded[i] else {
            i += 1;
            continue;
        };
        let start = i;
        let mut j = i + 1;
        while j < n {
            if onset_frames.contains(&j) && j - start >= min_frames {
                break;
            }
            match rounded[j] {
                Some(q) if q == p => j += 1,
                // Pitch change or gap: confirm it lasts `hysteresis` frames.
                _ => {
                    let lasting = (j..(j + hysteresis).min(n)).all(|k| rounded[k] != Some(p));
                    if lasting {
                        break;
                    }
                    j += 1;
                }
            }
        }
        if j - start >= min_frames {
            let mut cents: Vec<f32> = (start..j).filter_map(|k| frames[k].midi).collect();
            let pitch = median(&mut cents);
            let peak = (start..j).map(|k| frames[k].level).fold(0.0f32, f32::max);
            let vel = ((db(peak / max_level) + 40.0) / 40.0 * 0.8 + 0.2).clamp(0.05, 1.0);
            let midi = pitch.round().clamp(0.0, 127.0) as u8;
            events.push(NoteEvent {
                start: if start == 0 { 0.0 } else { time(start) },
                end: time(j),
                pitch: midi,
                amplitude: vel,
                bend_cents: Some((pitch - midi as f32) * 100.0),
            });
        }
        i = j;
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{midi_to_hz, synth};
    use crate::midi::{SIXTEENTH, events_to_notes};
    use aitrack_model::PPQ;

    #[test]
    fn sine_melody_exact_notes() {
        let sr = 44_100;
        let melody = [60u8, 62, 64, 65, 67, 72, 69];
        let mut x = Vec::new();
        for &m in &melody {
            // 0.5 s per note = one quarter at 120 BPM, short fades to avoid clicks.
            let mut t = synth::tone(midi_to_hz(m as f32), 0.5, sr, &[0.6]);
            let fade = 200;
            let len = t.len();
            for k in 0..fade {
                let g = k as f32 / fade as f32;
                t[k] *= g;
                t[len - 1 - k] *= g;
            }
            x.extend(t);
        }
        let opts = MidiOptions {
            engine: super::super::MidiEngine::Monophonic,
            bpm: 120.0,
            quantize: Some(SIXTEENTH),
            ..Default::default()
        };
        let ev = transcribe(&x, sr, &opts);
        let notes = events_to_notes(&ev, &opts);
        let got: Vec<(u8, i64, i64)> = notes.iter().map(|n| (n.key, n.start, n.length)).collect();
        let want: Vec<(u8, i64, i64)> = melody
            .iter()
            .enumerate()
            .map(|(i, &m)| (m, i as i64 * PPQ, PPQ))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn repeated_note_is_split_by_onsets() {
        let sr = 22_050;
        let mut x = Vec::new();
        for _ in 0..3 {
            let t: Vec<f32> = synth::tone(220.0, 0.4, sr, &[0.5, 0.2])
                .into_iter()
                .enumerate()
                .map(|(i, s)| s * (-(i as f32) / sr as f32 * 6.0).exp())
                .collect();
            x.extend(t);
        }
        let ev = transcribe(&x, sr, &MidiOptions::default());
        assert_eq!(ev.len(), 3, "{ev:?}");
        assert!(ev.iter().all(|e| e.pitch == 57));
    }
}
