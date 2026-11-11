//! Key and scale detection: a peak-picked pitch-class profile (HPCP-style
//! chroma) correlated against Krumhansl–Kessler and Temperley key profiles.

use aitrack_model::{Key, Scale, ScaleKind};
use serde::{Deserialize, Serialize};

use crate::audio::resample_channel;
use crate::dsp::{ANALYSIS_RATE, Spectrogram, hz_to_midi};

/// Krumhansl & Kessler (1982) probe-tone ratings.
const KK_MAJOR: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const KK_MINOR: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];
/// Temperley (2001, Kostka–Payne corpus) profiles.
const T_MAJOR: [f32; 12] = [
    0.748, 0.060, 0.488, 0.082, 0.670, 0.460, 0.096, 0.715, 0.104, 0.366, 0.057, 0.400,
];
const T_MINOR: [f32; 12] = [
    0.712, 0.084, 0.474, 0.618, 0.049, 0.460, 0.105, 0.747, 0.404, 0.067, 0.133, 0.330,
];

/// Result of key detection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyEstimate {
    pub scale: Scale,
    /// 0..1: how clearly the best key beats the runner-up and how tonal the material is.
    pub confidence: f32,
    /// Best three keys with their correlation score (-1..1).
    pub candidates: Vec<(Scale, f32)>,
    /// Normalised pitch-class profile (C = index 0, max = 1).
    pub chroma: [f32; 12],
}

/// Chroma per STFT frame (each frame normalised so its max is 1, or all-zero
/// for silent frames), plus the frame energies.
pub(crate) struct ChromaFrames {
    pub frames: Vec<[f32; 12]>,
    pub energy: Vec<f32>,
    pub hop: usize,
    pub sample_rate: u32,
}

/// Peak-picked chroma at [`ANALYSIS_RATE`]: spectral peaks between 55 Hz and
/// 5 kHz are folded onto pitch classes with a cosine weight that falls to zero
/// half a semitone away, and weighted by sqrt-magnitude.
pub(crate) fn chroma_frames(x: &[f32], sample_rate: u32) -> ChromaFrames {
    let n_fft = 8192;
    let hop = 2048;
    let spec = Spectrogram::compute(x, sample_rate, n_fft, hop);
    let lo = spec.hz_bin(55.0).max(1);
    let hi = spec.hz_bin(5000.0).min(n_fft / 2 - 1);
    let mut frames = Vec::with_capacity(spec.frames.len());
    let mut energy = Vec::with_capacity(spec.frames.len());
    for f in &spec.frames {
        let fmax = f[lo..hi].iter().copied().fold(0.0f32, f32::max);
        let mut c = [0.0f32; 12];
        let e: f32 = f[lo..hi].iter().map(|m| m * m).sum();
        energy.push(e);
        if fmax > 1e-6 {
            let floor = fmax * 0.01; // -40 dB below the frame peak
            for b in lo..hi {
                let m = f[b];
                if m < floor || m < f[b - 1] || m < f[b + 1] {
                    continue;
                }
                // Quadratic interpolation of the peak frequency.
                let (a, bb, cc) = (
                    f[b - 1].max(1e-12).ln(),
                    m.max(1e-12).ln(),
                    f[b + 1].max(1e-12).ln(),
                );
                let den = a - 2.0 * bb + cc;
                let off = if den.abs() > 1e-9 {
                    0.5 * (a - cc) / den
                } else {
                    0.0
                };
                let hz = (b as f32 + off) * sample_rate as f32 / n_fft as f32;
                let midi = hz_to_midi(hz);
                let nearest = midi.round();
                let dev = (midi - nearest).abs();
                let w = (std::f32::consts::PI * dev).cos().powi(2);
                let pc = (nearest as i32).rem_euclid(12) as usize;
                c[pc] += w * m.sqrt();
            }
            let cmax = c.iter().copied().fold(0.0f32, f32::max);
            if cmax > 0.0 {
                for v in &mut c {
                    *v /= cmax;
                }
            }
        }
        frames.push(c);
    }
    ChromaFrames {
        frames,
        energy,
        hop,
        sample_rate,
    }
}

fn pearson(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let ma = a.iter().sum::<f32>() / 12.0;
    let mb = b.iter().sum::<f32>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let x = a[i] - ma;
        let y = b[i] - mb;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 0.0 || db <= 0.0 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

fn rotate(p: &[f32; 12], root: usize) -> [f32; 12] {
    let mut r = [0.0; 12];
    for i in 0..12 {
        r[(i + root) % 12] = p[i];
    }
    r
}

/// Score all 24 major/minor keys for a pitch-class profile.
pub fn score_keys(chroma: &[f32; 12]) -> Vec<(Scale, f32)> {
    let mut out = Vec::with_capacity(24);
    for root in 0..12 {
        for (kind, kk, t) in [
            (ScaleKind::Major, &KK_MAJOR, &T_MAJOR),
            (ScaleKind::Minor, &KK_MINOR, &T_MINOR),
        ] {
            let s = 0.5 * (pearson(chroma, &rotate(kk, root)) + pearson(chroma, &rotate(t, root)));
            out.push((Scale::new(Key(root as u8), kind), s));
        }
    }
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

/// Key from a pitch-class profile.
pub fn key_from_chroma(chroma: &[f32; 12]) -> Option<KeyEstimate> {
    let max = chroma.iter().copied().fold(0.0f32, f32::max);
    if max <= 0.0 {
        return None;
    }
    let mut norm = *chroma;
    for v in &mut norm {
        *v /= max;
    }
    let scores = score_keys(&norm);
    let (best, s1) = scores[0];
    let s2 = scores[1].1;
    // Flatness of the chroma: a flat profile (noise, drums) is not tonal.
    let mean = norm.iter().sum::<f32>() / 12.0;
    let tonal = (1.0 - mean).clamp(0.0, 1.0);
    let margin = ((s1 - s2) / 0.15).clamp(0.0, 1.0);
    let confidence = (0.5 * margin + 0.5 * s1.max(0.0)) * tonal.sqrt();
    Some(KeyEstimate {
        scale: best,
        confidence,
        candidates: scores.into_iter().take(3).collect(),
        chroma: norm,
    })
}

/// Detect the key of a mono signal at any sample rate.
pub fn detect_key(samples: &[f32], sample_rate: u32) -> Option<KeyEstimate> {
    let x = if sample_rate == ANALYSIS_RATE {
        samples.to_vec()
    } else {
        resample_channel(samples, sample_rate, ANALYSIS_RATE).ok()?
    };
    let cf = chroma_frames(&x, ANALYSIS_RATE);
    let emax = cf.energy.iter().copied().fold(0.0f32, f32::max);
    if emax <= 1e-9 {
        return None;
    }
    let mut total = [0.0f32; 12];
    for (c, e) in cf.frames.iter().zip(&cf.energy) {
        // Ignore near-silent frames; weight the rest by loudness (sqrt keeps quiet passages relevant).
        if *e < emax * 1e-4 {
            continue;
        }
        let w = (e / emax).sqrt();
        for i in 0..12 {
            total[i] += c[i] * w;
        }
    }
    key_from_chroma(&total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{midi_to_hz, synth::tone};

    const SR: u32 = 22_050;
    const H: [f32; 3] = [1.0, 0.4, 0.15];

    fn chord(notes: &[u8], secs: f32) -> Vec<f32> {
        let mut out = vec![0.0f32; (secs * SR as f32) as usize];
        for &n in notes {
            for (o, s) in out.iter_mut().zip(tone(midi_to_hz(n as f32), secs, SR, &H)) {
                *o += s * 0.25;
            }
        }
        out
    }

    fn seq(chords: &[&[u8]], secs: f32) -> Vec<f32> {
        chords.iter().flat_map(|c| chord(c, secs)).collect()
    }

    #[test]
    fn c_major_progression_and_scale() {
        // I - IV - V - I, then the scale.
        let mut x = seq(
            &[
                &[48, 60, 64, 67],
                &[53, 65, 69, 72],
                &[55, 67, 71, 74],
                &[48, 60, 64, 67],
            ],
            1.0,
        );
        x.extend(seq(
            &[&[60], &[62], &[64], &[65], &[67], &[69], &[71], &[72]],
            0.3,
        ));
        let k = detect_key(&x, SR).unwrap();
        assert_eq!(k.scale, Scale::new(Key(0), ScaleKind::Major), "{k:?}");
        assert!(k.confidence > 0.3, "{k:?}");
        assert_eq!(k.candidates.len(), 3);
    }

    #[test]
    fn a_minor_progression() {
        // i - iv - V - i  (Am Dm E Am)
        let x = seq(
            &[
                &[45, 57, 60, 64],
                &[50, 62, 65, 69],
                &[52, 64, 68, 71],
                &[45, 57, 60, 64],
            ],
            1.0,
        );
        let k = detect_key(&x, SR).unwrap();
        assert_eq!(k.scale, Scale::new(Key(9), ScaleKind::Minor), "{k:?}");
    }

    #[test]
    fn a_minor_scale_melody() {
        let x = seq(
            &[
                &[57],
                &[59],
                &[60],
                &[62],
                &[64],
                &[65],
                &[67],
                &[69],
                &[64],
                &[60],
                &[57],
                &[57],
            ],
            0.35,
        );
        let k = detect_key(&x, 44_100.min(SR)).unwrap();
        assert_eq!(k.scale, Scale::new(Key(9), ScaleKind::Minor), "{k:?}");
    }

    #[test]
    fn c_major_triad_alone() {
        let x = chord(&[60, 64, 67], 2.0);
        let k = detect_key(&x, SR).unwrap();
        assert_eq!(k.scale, Scale::new(Key(0), ScaleKind::Major), "{k:?}");
    }

    #[test]
    fn silence_has_no_key() {
        assert!(detect_key(&vec![0.0; 44_100], SR).is_none());
    }
}
