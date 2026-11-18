//! A fixed-length timbre embedding made of plain DSP features, no model.
//!
//! The vector describes how a sound *sounds* (spectral shape, brightness,
//! noisiness, how it starts and how it dies away) and not how loud it is or
//! what pitch it plays, so two renders of one kick at different gains are
//! near each other and a kick is far from a hi-hat. It feeds the library's
//! "Similar sounds"; a learned embedding (CLAP through ONNX) can replace or
//! complement it behind the library's `Embedder` trait, so everything here is
//! versioned: change the features and bump [`DSP_EMBEDDING_VERSION`].
//!
//! Layout of the [`DSP_EMBEDDING_DIM`] values, each scaled to roughly -1..1
//! (the library standardises them over the whole collection anyway):
//!
//! | values | what |
//! |---|---|
//! | 13 | mean of MFCC 1..13 over the sounding frames |
//! | 13 | standard deviation of the same |
//! | 13 | mean absolute delta of the same (how much the timbre moves) |
//! | 8 | centroid, bandwidth, roll-off and flatness: mean and deviation |
//! | 5 | share of the energy below 100 Hz, 100–400, 400–2k, 2k–6k, above |
//! | 2 | zero-crossing rate: mean and deviation |
//! | 10 | attack, decay to -20 dB, decay to -40 dB, temporal centroid, crest factor, length, energy in each quarter of the sounding part |

use std::path::Path;
use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex32;

use crate::audio::{AudioData, load, resample_channel};
use crate::dsp::{ANALYSIS_RATE, hann};
use crate::{Error, Result};

/// Stable name stored with each vector.
pub const DSP_EMBEDDING_ID: &str = "dsp-timbre";
/// Increase when the features change, so stored vectors are made again.
pub const DSP_EMBEDDING_VERSION: u32 = 1;
/// Length of the vector.
pub const DSP_EMBEDDING_DIM: usize = 64;

const N_FFT: usize = 1024;
const HOP: usize = 256;
const N_MEL: usize = 40;
const N_COEF: usize = 13;
/// Only the first seconds are listened to: the timbre of a loop is in its first bars.
const MAX_SECONDS: f32 = 8.0;
/// Frames quieter than this under the loudest one (power ratio) are the silence after the sound.
const ACTIVE_FLOOR: f32 = 1e-4;

/// Decodes `path` and describes it.
pub fn embed_file(path: impl AsRef<Path>) -> Result<Vec<f32>> {
    let path = path.as_ref();
    let audio = load(path)?;
    dsp_embedding(&audio).ok_or_else(|| Error::InvalidInput(format!("{}: nothing to describe (silent or too short)", path.display())))
}

/// Describes decoded audio; `None` for digital silence or fewer than a handful of samples.
pub fn dsp_embedding(audio: &AudioData) -> Option<Vec<f32>> {
    let duration = audio.duration() as f32;
    let mono = audio.to_mono();
    if mono.len() < 64 {
        return None;
    }
    let mut x = resample_channel(&mono, audio.sample_rate, ANALYSIS_RATE).ok()?;
    x.truncate((MAX_SECONDS * ANALYSIS_RATE as f32) as usize);
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak < 1e-6 || !peak.is_finite() {
        return None;
    }
    // Level does not describe a timbre.
    x.iter_mut().for_each(|v| *v /= peak);
    Some(describe(&x, ANALYSIS_RATE, duration))
}

struct Frames {
    /// Power per bin, `N_FFT / 2 + 1` bins.
    power: Vec<Vec<f32>>,
    /// Total power per frame.
    energy: Vec<f32>,
}

fn stft(x: &[f32]) -> Frames {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft: Arc<dyn RealToComplex<f32>> = planner.plan_fft_forward(N_FFT);
    let win = hann(N_FFT);
    let mut input = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let n = if x.len() <= N_FFT { 1 } else { 1 + (x.len() - N_FFT).div_ceil(HOP) };
    let mut power = Vec::with_capacity(n);
    let mut energy = Vec::with_capacity(n);
    for t in 0..n {
        let start = t * HOP;
        for (i, v) in input.iter_mut().enumerate() {
            *v = x.get(start + i).copied().unwrap_or(0.0) * win[i];
        }
        fft.process(&mut input, &mut spec).expect("fft sizes match");
        let p: Vec<f32> = spec.iter().map(|c: &Complex32| c.norm_sqr()).collect();
        energy.push(p.iter().sum());
        power.push(p);
    }
    Frames { power, energy }
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10f32.powf(mel / 2595.0) - 1.0)
}

/// Triangular mel filters over `N_FFT / 2 + 1` bins, as sparse `(first bin, weights)`.
fn mel_bank(sr: u32) -> Vec<(usize, Vec<f32>)> {
    let bins = N_FFT / 2 + 1;
    let hz_bin = |hz: f32| hz * N_FFT as f32 / sr as f32;
    let (lo, hi) = (hz_to_mel(30.0), hz_to_mel(sr as f32 / 2.0));
    let edges: Vec<f32> = (0..N_MEL + 2).map(|i| hz_bin(mel_to_hz(lo + (hi - lo) * i as f32 / (N_MEL + 1) as f32))).collect();
    (0..N_MEL)
        .map(|m| {
            let (a, b, c) = (edges[m], edges[m + 1], edges[m + 2]);
            let first = (a.floor() as usize).min(bins - 1);
            let last = (c.ceil() as usize).min(bins - 1);
            let w = (first..=last)
                .map(|k| {
                    let k = k as f32;
                    if k <= a || k >= c {
                        0.0
                    } else if k <= b {
                        (k - a) / (b - a).max(1e-6)
                    } else {
                        (c - k) / (c - b).max(1e-6)
                    }
                })
                .collect();
            (first, w)
        })
        .collect()
}

/// DCT-II (orthonormal) basis rows 1..=N_COEF over `N_MEL` inputs.
fn dct_basis() -> Vec<Vec<f32>> {
    (1..=N_COEF)
        .map(|k| {
            let scale = (2.0 / N_MEL as f32).sqrt();
            (0..N_MEL).map(|n| scale * (std::f32::consts::PI * k as f32 * (n as f32 + 0.5) / N_MEL as f32).cos()).collect()
        })
        .collect()
}

fn mean_std(v: &[f32]) -> (f32, f32) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let m = v.iter().sum::<f32>() / v.len() as f32;
    let var = v.iter().map(|a| (a - m) * (a - m)).sum::<f32>() / v.len() as f32;
    (m, var.sqrt())
}

/// Hz on a log axis, 50 Hz → -1, 10 kHz → +1.
fn log_hz(hz: f32) -> f32 {
    ((hz.max(1.0) / 700.0).ln() / (10_000.0f32 / 700.0).ln()).clamp(-2.0, 1.0)
}

/// Seconds on a log axis, 1 ms → -1, 1 s → 0, 10 s → +0.33.
fn log_secs(s: f32) -> f32 {
    ((s.max(1e-4)).log10() / 3.0).clamp(-1.3, 1.0)
}

fn describe(x: &[f32], sr: u32, file_seconds: f32) -> Vec<f32> {
    let frames = stft(x);
    let n = frames.power.len();
    let max_e = frames.energy.iter().copied().fold(0.0f32, f32::max).max(1e-20);
    let active: Vec<usize> = (0..n).filter(|&t| frames.energy[t] >= max_e * ACTIVE_FLOOR).collect();
    let active = if active.is_empty() { vec![0] } else { active };
    let bank = mel_bank(sr);
    let dct = dct_basis();
    let bins = N_FFT / 2 + 1;
    let hz = |k: usize| k as f32 * sr as f32 / N_FFT as f32;
    // Anything 80 dB under the loudest frame is rounding noise, not timbre.
    let floor = max_e * 1e-8;

    // Per sounding frame: cepstrum and spectral shape.
    let mut mfcc: Vec<[f32; N_COEF]> = Vec::with_capacity(active.len());
    let (mut centroid, mut bandwidth, mut rolloff, mut flatness) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut bands = [0f32; 5];
    for &t in &active {
        let p = &frames.power[t];
        let mel: Vec<f32> = bank.iter().map(|(first, w)| w.iter().enumerate().map(|(i, g)| g * p[first + i]).sum::<f32>()).collect();
        let logmel: Vec<f32> = mel.iter().map(|m| 10.0 * (m + floor).log10()).collect();
        let mut c = [0f32; N_COEF];
        for (k, row) in dct.iter().enumerate() {
            c[k] = row.iter().zip(&logmel).map(|(a, b)| a * b).sum::<f32>() / 20.0;
        }
        mfcc.push(c);

        let total: f32 = p.iter().sum::<f32>().max(1e-20);
        let cen = (0..bins).map(|k| hz(k) * p[k]).sum::<f32>() / total;
        let bw = ((0..bins).map(|k| (hz(k) - cen).powi(2) * p[k]).sum::<f32>() / total).sqrt();
        let mut acc = 0.0;
        let mut roll = hz(bins - 1);
        for k in 0..bins {
            acc += p[k];
            if acc >= 0.85 * total {
                roll = hz(k);
                break;
            }
        }
        // Flatness of the magnitude spectrum: geometric over arithmetic mean.
        let log_mean = p[1..].iter().map(|v| (v.sqrt() + 1e-9).ln()).sum::<f32>() / (bins - 1) as f32;
        let arith = p[1..].iter().map(|v| v.sqrt()).sum::<f32>() / (bins - 1) as f32;
        centroid.push(log_hz(cen));
        bandwidth.push(bw / (sr as f32 / 4.0));
        rolloff.push(log_hz(roll));
        flatness.push((log_mean.exp() / (arith + 1e-9)).clamp(0.0, 1.0) * 2.0 - 1.0);
        for k in 0..bins {
            let f = hz(k);
            let b = if f < 100.0 {
                0
            } else if f < 400.0 {
                1
            } else if f < 2_000.0 {
                2
            } else if f < 6_000.0 {
                3
            } else {
                4
            };
            bands[b] += p[k];
        }
    }

    let mut out = Vec::with_capacity(DSP_EMBEDDING_DIM);
    // MFCC mean, deviation, and how much each moves between frames.
    let col = |k: usize| -> Vec<f32> { mfcc.iter().map(|c| c[k]).collect() };
    let stats: Vec<(f32, f32)> = (0..N_COEF).map(|k| mean_std(&col(k))).collect();
    out.extend(stats.iter().map(|s| s.0));
    out.extend(stats.iter().map(|s| s.1));
    for k in 0..N_COEF {
        let c = col(k);
        let d = if c.len() < 3 { 0.0 } else { c.windows(3).map(|w| ((w[2] - w[0]) / 2.0).abs()).sum::<f32>() / (c.len() - 2) as f32 };
        out.push(d);
    }
    for v in [&centroid, &bandwidth, &rolloff, &flatness] {
        let (m, s) = mean_std(v);
        out.push(m);
        out.push(s);
    }
    let band_total: f32 = bands.iter().sum::<f32>().max(1e-20);
    out.extend(bands.iter().map(|b| (b / band_total).sqrt() * 2.0 - 1.0));

    // Zero-crossing rate per hop.
    let zcr: Vec<f32> = (0..n)
        .filter(|t| active.binary_search(t).is_ok())
        .map(|t| {
            let seg = &x[(t * HOP).min(x.len())..(t * HOP + N_FFT).min(x.len())];
            if seg.len() < 2 { 0.0 } else { seg.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count() as f32 / seg.len() as f32 * 4.0 - 1.0 }
        })
        .collect();
    let (zm, zs) = mean_std(&zcr);
    out.push(zm);
    out.push(zs * 4.0);

    // Envelope: attack, decay, centre of mass, crest, length, shape.
    let secs = |frames: f32| frames * HOP as f32 / sr as f32;
    let env_db: Vec<f32> = frames.energy.iter().map(|e| 10.0 * (e / max_e + 1e-12).log10()).collect();
    let peak_t = (0..n).max_by(|&a, &b| frames.energy[a].total_cmp(&frames.energy[b])).unwrap_or(0);
    let first = active.first().copied().unwrap_or(0);
    let last = active.last().copied().unwrap_or(0);
    let start = (first..=peak_t).find(|&t| env_db[t] >= -30.0).unwrap_or(peak_t);
    out.push(log_secs(secs((peak_t - start) as f32 + 1.0)));
    let fall = |db: f32| (peak_t..n).find(|&t| env_db[t] <= -db).unwrap_or(last.max(peak_t)) - peak_t;
    out.push(log_secs(secs(fall(20.0) as f32 + 1.0)));
    out.push(log_secs(secs(fall(40.0) as f32 + 1.0)));
    let span = (last + 1 - first).max(1);
    let e_sum: f32 = (first..=last).map(|t| frames.energy[t]).sum::<f32>().max(1e-20);
    let t_centroid = (first..=last).map(|t| (t - first) as f32 * frames.energy[t]).sum::<f32>() / e_sum;
    out.push(log_secs(secs(t_centroid + 1.0)));
    let rms = (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt().max(1e-9);
    out.push((20.0 * (1.0 / rms).log10() / 20.0 - 0.5).clamp(-1.0, 1.5));
    out.push(log_secs(file_seconds.max(0.001)));
    for q in 0..4 {
        let (a, b) = (first + span * q / 4, first + span * (q + 1) / 4);
        let e: f32 = (a..b.max(a + 1).min(last + 1)).map(|t| frames.energy[t]).sum();
        out.push((e / e_sum).sqrt() * 2.0 - 1.0);
    }
    debug_assert_eq!(out.len(), DSP_EMBEDDING_DIM);
    out.resize(DSP_EMBEDDING_DIM, 0.0);
    out.iter_mut().for_each(|v| {
        if !v.is_finite() {
            *v = 0.0;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth;

    const SR: u32 = 44_100;

    fn audio(x: Vec<f32>) -> AudioData {
        AudioData::mono_signal(x, SR)
    }

    fn dist(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
    }

    fn embed(x: Vec<f32>) -> Vec<f32> {
        dsp_embedding(&audio(x)).expect("an embedding")
    }

    #[test]
    fn the_vector_has_the_documented_length_and_is_finite() {
        let v = embed(synth::kick(SR, 0.5));
        assert_eq!(v.len(), DSP_EMBEDDING_DIM);
        assert!(v.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn silence_and_scraps_have_no_embedding() {
        assert!(dsp_embedding(&audio(vec![0.0; 10_000])).is_none());
        assert!(dsp_embedding(&audio(vec![0.5; 10])).is_none());
    }

    #[test]
    fn gain_does_not_move_the_vector() {
        let kick = synth::kick(SR, 0.5);
        let quiet: Vec<f32> = kick.iter().map(|v| v * 0.1).collect();
        let a = embed(kick.clone());
        let b = embed(quiet);
        assert!(dist(&a, &b) < 1e-3, "{}", dist(&a, &b));
    }

    #[test]
    fn a_kick_is_nearer_another_kick_than_a_hat_or_a_snare() {
        let kick = embed(synth::kick(SR, 0.6));
        // The same kick through a different decay and a touch of tilt: another render of the family.
        let kick2: Vec<f32> = synth::kick(SR, 0.5).iter().enumerate().map(|(i, v)| v * (-(i as f32) / SR as f32 * 2.0).exp() * 0.5).collect();
        let kick2 = embed(kick2);
        let hat = embed(synth::hat(SR, 0.3, 3));
        let snare = embed(synth::snare(SR, 0.4, 5));
        let (near, far_hat, far_snare) = (dist(&kick, &kick2), dist(&kick, &hat), dist(&kick, &snare));
        assert!(near < far_hat, "{near} < {far_hat}");
        assert!(near < far_snare, "{near} < {far_snare}");
    }

    #[test]
    fn a_hat_is_nearer_another_hat_than_a_tone() {
        let hat = embed(synth::hat(SR, 0.3, 1));
        let hat2 = embed(synth::hat(SR, 0.25, 9));
        let tone = embed(synth::tone(220.0, 1.0, SR, &[1.0, 0.4, 0.2]));
        assert!(dist(&hat, &hat2) < dist(&hat, &tone));
    }

    #[test]
    fn the_sample_rate_of_the_file_does_not_matter() {
        let a = embed(synth::kick(SR, 0.5));
        let b = dsp_embedding(&AudioData::mono_signal(synth::kick(48_000, 0.5), 48_000)).unwrap();
        let other = embed(synth::hat(SR, 0.3, 2));
        assert!(dist(&a, &b) < dist(&a, &other) * 0.5, "{} vs {}", dist(&a, &b), dist(&a, &other));
    }
}
