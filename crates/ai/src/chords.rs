//! Chord recognition: beat-synchronous chroma matched against chord templates,
//! smoothed with a Viterbi pass, summarised per bar for the Chord Track.

use aitrack_model::Key;
use serde::{Deserialize, Serialize};

use crate::audio::resample_channel;
use crate::dsp::{ANALYSIS_RATE, OnsetEnvelope};
use crate::key::chroma_frames;
use crate::tempo::detect_bpm;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChordQuality {
    Major,
    Minor,
    Diminished,
    Augmented,
    Sus2,
    Sus4,
    Dominant7,
    Major7,
    Minor7,
}

impl ChordQuality {
    pub const ALL: [ChordQuality; 9] = [
        Self::Major,
        Self::Minor,
        Self::Diminished,
        Self::Augmented,
        Self::Sus2,
        Self::Sus4,
        Self::Dominant7,
        Self::Major7,
        Self::Minor7,
    ];

    pub fn intervals(self) -> &'static [u8] {
        match self {
            Self::Major => &[0, 4, 7],
            Self::Minor => &[0, 3, 7],
            Self::Diminished => &[0, 3, 6],
            Self::Augmented => &[0, 4, 8],
            Self::Sus2 => &[0, 2, 7],
            Self::Sus4 => &[0, 5, 7],
            Self::Dominant7 => &[0, 4, 7, 10],
            Self::Major7 => &[0, 4, 7, 11],
            Self::Minor7 => &[0, 3, 7, 10],
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            Self::Major => "",
            Self::Minor => "m",
            Self::Diminished => "dim",
            Self::Augmented => "aug",
            Self::Sus2 => "sus2",
            Self::Sus4 => "sus4",
            Self::Dominant7 => "7",
            Self::Major7 => "maj7",
            Self::Minor7 => "m7",
        }
    }

    /// Prior: plain triads are far more common; extended chords must earn it.
    fn prior(self) -> f32 {
        match self {
            Self::Major | Self::Minor => 1.0,
            Self::Dominant7 | Self::Minor7 | Self::Major7 => 0.93,
            _ => 0.88,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Chord {
    pub root: Key,
    pub quality: ChordQuality,
}

impl Chord {
    pub fn name(&self) -> String {
        format!("{}{}", self.root.name(), self.quality.suffix())
    }

    /// Pitch classes of the chord.
    pub fn pitch_classes(&self) -> Vec<u8> {
        self.quality
            .intervals()
            .iter()
            .map(|i| (self.root.0 + i) % 12)
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChordSegment {
    /// 0-based bar and beat of the segment start.
    pub bar: u32,
    pub beat: u32,
    pub start: f64,
    pub end: f64,
    /// `None` = no chord (silence or unpitched).
    pub chord: Option<Chord>,
    /// Chord symbol, "N" for no chord.
    pub name: String,
    pub confidence: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChordOptions {
    /// Tempo; detected when `None`.
    pub bpm: Option<f32>,
    pub beats_per_bar: u32,
    /// Seconds of the first downbeat; estimated from onsets when `None`.
    pub first_downbeat: Option<f64>,
}

impl Default for ChordOptions {
    fn default() -> Self {
        Self {
            bpm: None,
            beats_per_bar: 4,
            first_downbeat: None,
        }
    }
}

fn templates() -> Vec<(Chord, [f32; 12], f32)> {
    let mut out = Vec::new();
    for root in 0..12u8 {
        for q in ChordQuality::ALL {
            let mut t = [0.0f32; 12];
            for (k, i) in q.intervals().iter().enumerate() {
                // Root and fifth weigh a little more than colour tones.
                t[((root + i) % 12) as usize] = match k {
                    0 => 1.0,
                    2 => 0.9,
                    _ => 0.8,
                };
            }
            out.push((
                Chord {
                    root: Key(root),
                    quality: q,
                },
                t,
                q.prior(),
            ));
        }
    }
    out
}

fn cosine(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let (mut d, mut na, mut nb) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        d += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na <= 0.0 || nb <= 0.0 {
        0.0
    } else {
        d / (na * nb).sqrt()
    }
}

/// Detect chords per beat (merged into segments). Use [`chords_per_bar`] for
/// one symbol per bar.
pub fn detect_chords(samples: &[f32], sample_rate: u32, opts: &ChordOptions) -> Vec<ChordSegment> {
    let Ok(x) = resample_channel(samples, sample_rate, ANALYSIS_RATE) else {
        return Vec::new();
    };
    let sr = ANALYSIS_RATE;
    let duration = x.len() as f64 / sr as f64;
    let bpm = opts
        .bpm
        .or_else(|| detect_bpm(&x, sr).map(|b| b.bpm))
        .unwrap_or(120.0) as f64;
    let beat = 60.0 / bpm;
    let offset = opts
        .first_downbeat
        .unwrap_or_else(|| first_onset(&x, sr).unwrap_or(0.0) % beat);

    // Chroma on a finer hop than key detection, for beat resolution.
    let cf = chroma_frames(&x, sr);
    let frame_dt = cf.hop as f64 / cf.sample_rate as f64;
    let n_fft_center = 4096.0 / cf.sample_rate as f64;
    let emax = cf.energy.iter().copied().fold(0.0f32, f32::max).max(1e-12);
    let n_beats = ((duration - offset) / beat).ceil().max(0.0) as usize;
    let mut beat_chroma: Vec<([f32; 12], f32)> = Vec::with_capacity(n_beats);
    for b in 0..n_beats {
        let t0 = offset + b as f64 * beat;
        let t1 = t0 + beat;
        let mut c = [0.0f32; 12];
        let mut e = 0.0f32;
        let mut count = 0;
        for (i, fr) in cf.frames.iter().enumerate() {
            let t = i as f64 * frame_dt + n_fft_center;
            if t >= t0 && t < t1 {
                for k in 0..12 {
                    c[k] += fr[k];
                }
                e = e.max(cf.energy[i]);
                count += 1;
            }
        }
        if count == 0 {
            // Beat shorter than a chroma hop: borrow the nearest frame.
            let i = (((t0 + t1) * 0.5 - n_fft_center) / frame_dt)
                .round()
                .max(0.0) as usize;
            if let Some(fr) = cf.frames.get(i) {
                c = *fr;
                e = cf.energy[i];
            }
        }
        beat_chroma.push((c, e / emax));
    }
    if beat_chroma.is_empty() {
        return Vec::new();
    }

    // States: every template plus "no chord".
    let tpl = templates();
    let n_states = tpl.len() + 1;
    let no_chord = tpl.len();
    let emission = |c: &[f32; 12], e: f32| -> Vec<f32> {
        let mut v: Vec<f32> = tpl
            .iter()
            .map(|(_, t, prior)| cosine(c, t) * prior)
            .collect();
        let max = c.iter().copied().fold(0.0f32, f32::max);
        let mean = c.iter().sum::<f32>() / 12.0;
        let flat = if max > 0.0 { mean / max } else { 1.0 };
        v.push(if e < 1e-3 || max <= 0.0 {
            1.0
        } else {
            0.35 + 0.4 * flat
        });
        v
    };
    let stay = 0.15f32; // bonus (in log-ish score units) for keeping the same chord
    let mut score: Vec<f32> = emission(&beat_chroma[0].0, beat_chroma[0].1);
    let mut back: Vec<Vec<usize>> = Vec::with_capacity(beat_chroma.len());
    back.push((0..n_states).collect());
    for (c, e) in beat_chroma.iter().skip(1) {
        let em = emission(c, *e);
        let (best_prev, best_val) =
            score.iter().enumerate().fold(
                (0, f32::MIN),
                |acc, (i, v)| if *v > acc.1 { (i, *v) } else { acc },
            );
        let mut next = vec![0.0f32; n_states];
        let mut bp = vec![0usize; n_states];
        for s in 0..n_states {
            let keep = score[s] + stay;
            if keep >= best_val {
                next[s] = keep + em[s];
                bp[s] = s;
            } else {
                next[s] = best_val + em[s];
                bp[s] = best_prev;
            }
        }
        score = next;
        back.push(bp);
    }
    let mut state = score
        .iter()
        .enumerate()
        .fold(
            (0, f32::MIN),
            |acc, (i, v)| if *v > acc.1 { (i, *v) } else { acc },
        )
        .0;
    let mut path = vec![0usize; beat_chroma.len()];
    for t in (0..beat_chroma.len()).rev() {
        path[t] = state;
        state = back[t][state];
    }

    let bpb = opts.beats_per_bar.max(1) as usize;
    let mut segs: Vec<ChordSegment> = Vec::new();
    for (b, &s) in path.iter().enumerate() {
        let chord = (s != no_chord).then(|| tpl[s].0);
        let em = emission(&beat_chroma[b].0, beat_chroma[b].1);
        let conf = em[s].clamp(0.0, 1.0);
        let start = offset + b as f64 * beat;
        let end = (start + beat).min(duration);
        match segs.last_mut() {
            Some(last) if last.chord == chord => {
                last.end = end;
                last.confidence = last.confidence.max(conf);
            }
            _ => segs.push(ChordSegment {
                bar: (b / bpb) as u32,
                beat: (b % bpb) as u32,
                start,
                end,
                chord,
                name: chord.map_or_else(|| "N".to_string(), |c| c.name()),
                confidence: conf,
            }),
        }
    }
    segs
}

/// One chord symbol per bar (the chord covering most of the bar).
pub fn chords_per_bar(
    segments: &[ChordSegment],
    bpm: f32,
    beats_per_bar: u32,
    first_downbeat: f64,
) -> Vec<String> {
    let bar_len = 60.0 / bpm as f64 * beats_per_bar as f64;
    let end = segments.last().map_or(0.0, |s| s.end);
    let n_bars = ((end - first_downbeat) / bar_len).ceil().max(0.0) as usize;
    (0..n_bars)
        .map(|b| {
            let t0 = first_downbeat + b as f64 * bar_len;
            let t1 = t0 + bar_len;
            segments
                .iter()
                .map(|s| (s, (s.end.min(t1) - s.start.max(t0)).max(0.0)))
                .filter(|(_, o)| *o > 0.0)
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map_or_else(|| "N".to_string(), |(s, _)| s.name.clone())
        })
        .collect()
}

fn first_onset(x: &[f32], sr: u32) -> Option<f64> {
    let env = OnsetEnvelope::compute(x, sr, 1024, 256);
    env.peaks(0.2, 0.05)
        .first()
        .map(|&p| (p * 256 + 768) as f64 / sr as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{midi_to_hz, synth::tone};

    fn chord_audio(notes: &[u8], secs: f32, sr: u32) -> Vec<f32> {
        let mut out = vec![0.0f32; (secs * sr as f32) as usize];
        for &n in notes {
            for (o, s) in out
                .iter_mut()
                .zip(tone(midi_to_hz(n as f32), secs, sr, &[1.0, 0.3]))
            {
                *o += s * 0.2;
            }
        }
        out
    }

    #[test]
    fn am_f_c_g_per_bar() {
        // i - VI - III - VII in A minor, one bar each at 120 BPM (2 s per bar).
        let sr = 22_050;
        let prog: [&[u8]; 4] = [&[57, 60, 64], &[53, 57, 60], &[48, 52, 55], &[55, 59, 62]];
        let x: Vec<f32> = prog.iter().flat_map(|c| chord_audio(c, 2.0, sr)).collect();
        let opts = ChordOptions {
            bpm: Some(120.0),
            beats_per_bar: 4,
            first_downbeat: Some(0.0),
        };
        let segs = detect_chords(&x, sr, &opts);
        let bars = chords_per_bar(&segs, 120.0, 4, 0.0);
        assert_eq!(bars, vec!["Am", "F", "C", "G"], "{segs:#?}");
    }

    #[test]
    fn names() {
        let c = Chord {
            root: Key(1),
            quality: ChordQuality::Minor7,
        };
        assert_eq!(c.name(), "C#m7");
        assert_eq!(c.pitch_classes(), vec![1, 4, 8, 11]);
    }
}
