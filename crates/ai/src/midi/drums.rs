//! Model-free drum transcription: onsets from spectral flux, each hit
//! classified by the energy balance of its attack (low / mid / high bands)
//! into General MIDI kick (36), snare (38), closed hat (42) or open hat (46).

use super::{MidiOptions, NoteEvent};
use crate::audio::resample_channel;
use crate::dsp::{ANALYSIS_RATE, OnsetEnvelope, Spectrogram};

pub const GM_KICK: u8 = 36;
pub const GM_SNARE: u8 = 38;
pub const GM_CLOSED_HAT: u8 = 42;
pub const GM_OPEN_HAT: u8 = 46;

const HOP: usize = 128;
const PAD: usize = 1024;

pub fn transcribe(samples: &[f32], sample_rate: u32, _opts: &MidiOptions) -> Vec<NoteEvent> {
    let Ok(resampled) = resample_channel(samples, sample_rate, ANALYSIS_RATE) else {
        return Vec::new();
    };
    let sr = ANALYSIS_RATE as f64;
    // Leading silence so a hit at sample 0 still produces a flux peak.
    let mut x = vec![0.0f32; PAD];
    x.extend_from_slice(&resampled);
    let env = OnsetEnvelope::compute(&x, ANALYSIS_RATE, 1024, HOP);
    let peaks = env.peaks(0.08, 0.04);
    if peaks.is_empty() {
        return Vec::new();
    }
    let spec = Spectrogram::compute(&x, ANALYSIS_RATE, 1024, HOP);
    let b_low = spec.hz_bin(150.0);
    let b_mid = spec.hz_bin(5000.0);
    let max_strength = peaks.iter().map(|&p| env.values[p]).fold(0.0f32, f32::max);
    let mut events = Vec::new();
    for (k, &p) in peaks.iter().enumerate() {
        // Log-compressed flux peaks as soon as the hit enters the (uncentred)
        // 1024-sample window, i.e. near its last quarter.
        let onset_sample = (p * HOP + 768).min(x.len());
        let next = peaks
            .get(k + 1)
            .map_or(x.len(), |&n| n * HOP + 768)
            .min(x.len());
        // Energy the hit adds per band: mean of the ~60 ms after the onset minus
        // the spectrum just before it, so the tail of an earlier kick does not
        // make the next hi-hat look like a kick.
        let n_att = ((0.06 * sr) as usize / HOP).max(2);
        let before = &spec.frames[p.saturating_sub(2)];
        let (mut lo, mut mid, mut hi) = (0.0f64, 0.0f64, 0.0f64);
        let after: Vec<&Vec<f32>> = spec.frames.iter().skip(p).take(n_att).collect();
        for b in 1..before.len() {
            let mean = after.iter().map(|f| (f[b] as f64).powi(2)).sum::<f64>()
                / after.len().max(1) as f64;
            let e = (mean - (before[b] as f64).powi(2)).max(0.0);
            if b < b_low {
                lo += e;
            } else if b < b_mid {
                mid += e;
            } else {
                hi += e;
            }
        }
        let tot = (lo + mid + hi).max(1e-20);
        let (lo, mid, hi) = (lo / tot, mid / tot, hi / tot);
        let vel = (0.35 + 0.65 * (env.values[p] / max_strength).sqrt()).clamp(0.05, 1.0);
        let start = onset_sample.saturating_sub(PAD) as f64 / sr;
        let end = (start + 0.1)
            .min(next.saturating_sub(PAD) as f64 / sr)
            .max(start + 0.02);
        let mut hit = |pitch: u8| {
            events.push(NoteEvent {
                start,
                end,
                pitch,
                amplitude: vel,
                bend_cents: None,
            })
        };
        if lo > 0.35 {
            hit(GM_KICK);
            // Kick + hat together: substantial high band on top.
            if hi > 0.25 {
                hit(GM_CLOSED_HAT);
            }
        } else if hi > 0.5 && lo < 0.1 {
            // Open hat: still ringing at -20 dB after 150 ms.
            let ring = sustain_after(&x, onset_sample, (0.15 * sr) as usize, next);
            hit(if ring { GM_OPEN_HAT } else { GM_CLOSED_HAT });
        } else if mid + lo > 0.35 {
            hit(GM_SNARE);
        } else {
            hit(GM_CLOSED_HAT);
        }
    }
    events.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    events
}

fn sustain_after(x: &[f32], at: usize, delay: usize, limit: usize) -> bool {
    let peak = x[at.min(x.len())..(at + 2000).min(x.len())]
        .iter()
        .fold(0.0f32, |m, s| m.max(s.abs()));
    let t = at + delay;
    if t + 200 > limit || t + 200 > x.len() {
        return false;
    }
    let late = x[t..t + 200].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    late > peak * 0.1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth;
    use crate::midi::{SIXTEENTH, events_to_notes};
    use aitrack_model::PPQ;

    #[test]
    fn kick_snare_hat_pattern() {
        let sr = 44_100;
        let bpm = 100.0;
        let beat = 60.0 / bpm;
        let bars = 2;
        let mut x = vec![0.0f32; (beat * 4.0 * bars as f64 * sr as f64) as usize + sr as usize / 2];
        let mut want = Vec::new();
        for b in 0..4 * bars {
            let at = (b as f64 * beat * sr as f64) as usize;
            if b % 2 == 0 {
                synth::mix_at(&mut x, &synth::kick(sr, 0.35), at);
                want.push((GM_KICK, b as i64 * PPQ));
            } else {
                synth::mix_at(&mut x, &synth::snare(sr, 0.25, b as u32), at);
                want.push((GM_SNARE, b as i64 * PPQ));
            }
            let off = at + (beat * 0.5 * sr as f64) as usize;
            synth::mix_at(&mut x, &synth::hat(sr, 0.06, 100 + b as u32), off);
            want.push((GM_CLOSED_HAT, b as i64 * PPQ + PPQ / 2));
        }
        let opts = MidiOptions {
            bpm,
            quantize: Some(SIXTEENTH),
            ..Default::default()
        };
        let notes = events_to_notes(&transcribe(&x, sr, &opts), &opts);
        let got: Vec<(u8, i64)> = notes.iter().map(|n| (n.key, n.start)).collect();
        want.sort_by_key(|w| (w.1, w.0));
        assert_eq!(got, want);
    }
}
