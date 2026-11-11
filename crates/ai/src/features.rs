//! Loudness (ITU-R BS.1770-4 / EBU R128 integrated) and descriptive features.

use serde::{Deserialize, Serialize};

use crate::dsp::{OnsetEnvelope, Spectrogram, db, rms};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loudness {
    /// Integrated loudness in LUFS (gated). Very short sounds use one block over the whole file.
    pub lufs_integrated: f32,
    /// Sample peak in dBFS.
    pub peak_dbfs: f32,
    pub rms_dbfs: f32,
}

/// Second-order IIR section (direct form I).
#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
}

impl Biquad {
    fn run(&self, x: &[f32]) -> Vec<f64> {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        x.iter()
            .map(|&s| {
                let s = s as f64;
                let y = self.b[0] * s + self.b[1] * x1 + self.b[2] * x2
                    - self.a[1] * y1
                    - self.a[2] * y2;
                x2 = x1;
                x1 = s;
                y2 = y1;
                y1 = y;
                y
            })
            .collect()
    }
}

/// K-weighting filters for an arbitrary sample rate (BS.1770 pre-filter shelf + RLB high-pass),
/// derived from the analogue prototypes through the bilinear transform.
fn k_weighting(sr: f64) -> (Biquad, Biquad) {
    use std::f64::consts::PI;
    let f0 = 1_681.974_450_955_533;
    let g = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (PI * f0 / sr).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (PI * f0 / sr).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    (shelf, hp)
}

pub fn loudness(channels: &[Vec<f32>], sample_rate: u32) -> Loudness {
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    let peak = channels
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |m, s| m.max(s.abs()));
    let all_rms = {
        let sum: f64 = channels
            .iter()
            .flat_map(|c| c.iter())
            .map(|s| (*s as f64).powi(2))
            .sum();
        let n = (frames * channels.len()).max(1);
        (sum / n as f64).sqrt() as f32
    };
    let (shelf, hp) = k_weighting(sample_rate as f64);
    let weighted: Vec<Vec<f64>> = channels
        .iter()
        .take(5)
        .map(|c| {
            let y: Vec<f32> = shelf.run(c).into_iter().map(|v| v as f32).collect();
            hp.run(&y)
        })
        .collect();
    let block = (0.4 * sample_rate as f64) as usize;
    let step = block / 4;
    let block_power = |start: usize, len: usize| -> f64 {
        weighted
            .iter()
            .map(|c| c[start..start + len].iter().map(|v| v * v).sum::<f64>() / len as f64)
            .sum()
    };
    let to_lufs = |p: f64| -0.691 + 10.0 * p.max(1e-20).log10();
    let mut powers = Vec::new();
    if frames >= block && block > 0 {
        let mut s = 0;
        while s + block <= frames {
            powers.push(block_power(s, block));
            s += step;
        }
    } else if frames > 0 {
        powers.push(block_power(0, frames));
    }
    let abs_gated: Vec<f64> = powers
        .iter()
        .copied()
        .filter(|p| to_lufs(*p) > -70.0)
        .collect();
    let lufs = if abs_gated.is_empty() {
        -70.0
    } else {
        let mean = abs_gated.iter().sum::<f64>() / abs_gated.len() as f64;
        let rel = to_lufs(mean) - 10.0;
        let gated: Vec<f64> = abs_gated
            .into_iter()
            .filter(|p| to_lufs(*p) > rel)
            .collect();
        to_lufs(gated.iter().sum::<f64>() / gated.len().max(1) as f64)
    };
    Loudness {
        lufs_integrated: lufs as f32,
        peak_dbfs: db(peak),
        rms_dbfs: db(all_rms),
    }
}

/// Descriptive features used by the classifier and the library UI.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Features {
    /// Spectral centroid in Hz (energy-weighted over the sound).
    pub centroid_hz: f32,
    /// Spectral flatness 0 (tonal) .. 1 (noise).
    pub flatness: f32,
    /// 85 % spectral roll-off in Hz.
    pub rolloff_hz: f32,
    /// Zero-crossing rate in crossings per second.
    pub zero_crossing_rate: f32,
    /// Fraction of energy below 150 Hz, 150 Hz–2 kHz, above 2 kHz (sums to 1).
    pub band_low: f32,
    pub band_mid: f32,
    pub band_high: f32,
    /// Time from start to the RMS peak, seconds.
    pub attack_s: f32,
    /// Time from the RMS peak until it falls 30 dB below, seconds (or to the end).
    pub decay_s: f32,
    /// Detected transients.
    pub transients: u32,
    /// Change in centroid from the first to the last third (positive = brightening, e.g. risers).
    pub centroid_slope: f32,
    /// Change in RMS (dB) from the first to the last third.
    pub level_slope_db: f32,
    /// Peak-to-RMS ratio in dB.
    pub crest_db: f32,
    /// Fraction of 10 ms frames within 12 dB of the loudest frame of the
    /// surrounding ±250 ms: lower for drums (hits decay between beats), high
    /// for sustained material (pads, held bass notes).
    #[serde(default)]
    pub sustain: f32,
}

/// Compute features of a mono signal.
pub fn features(x: &[f32], sample_rate: u32) -> Features {
    if x.is_empty() {
        return Features::default();
    }
    let sr = sample_rate as f32;
    let n_fft = 2048;
    let hop = 512;
    let spec = Spectrogram::compute(x, sample_rate, n_fft, hop);
    let b150 = spec.hz_bin(150.0);
    let b2k = spec.hz_bin(2000.0);
    let (mut lo, mut mid, mut hi) = (0.0f64, 0.0f64, 0.0f64);
    let (mut cen_num, mut cen_den) = (0.0f64, 0.0f64);
    let mut flat_acc = 0.0f64;
    let mut flat_w = 0.0f64;
    let mut roll_acc = 0.0f64;
    let mut centroids = Vec::with_capacity(spec.frames.len());
    let mut frame_energy = Vec::with_capacity(spec.frames.len());
    for f in &spec.frames {
        let mut e = 0.0f64;
        let mut fc = 0.0f64;
        for (b, m) in f.iter().enumerate().skip(1) {
            let p = (*m as f64).powi(2);
            e += p;
            fc += p * spec.bin_hz(b) as f64;
            if b < b150 {
                lo += p;
            } else if b < b2k {
                mid += p;
            } else {
                hi += p;
            }
        }
        frame_energy.push(e);
        cen_num += fc;
        cen_den += e;
        centroids.push(if e > 0.0 { (fc / e) as f32 } else { 0.0 });
        if e > 1e-10 {
            let n = (f.len() - 1) as f64;
            let log_mean = f
                .iter()
                .skip(1)
                .map(|m| ((*m as f64).powi(2) + 1e-12).ln())
                .sum::<f64>()
                / n;
            let mean = e / n;
            flat_acc += (log_mean.exp() / mean) * e;
            flat_w += e;
            let mut acc = 0.0;
            let mut roll = 0.0;
            for (b, m) in f.iter().enumerate().skip(1) {
                acc += (*m as f64).powi(2);
                if acc >= 0.85 * e {
                    roll = spec.bin_hz(b) as f64;
                    break;
                }
            }
            roll_acc += roll * e;
        }
    }
    let tot = (lo + mid + hi).max(1e-20);
    let zc = x
        .windows(2)
        .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
        .count() as f32
        / (x.len() as f32 / sr);

    // RMS envelope at 10 ms.
    let w = ((0.01 * sr) as usize).max(1);
    let env: Vec<f32> = x.chunks(w).map(rms).collect();
    let (pk_i, pk) =
        env.iter().enumerate().fold(
            (0, 0.0f32),
            |(bi, bv), (i, v)| if *v > bv { (i, *v) } else { (bi, bv) },
        );
    let attack_s = pk_i as f32 * w as f32 / sr;
    let floor = pk * 10f32.powf(-30.0 / 20.0);
    let decay_end = env[pk_i..]
        .iter()
        .position(|v| *v < floor)
        .map_or(env.len(), |p| pk_i + p);
    let decay_s = (decay_end - pk_i) as f32 * w as f32 / sr;
    let sustain = {
        let gate = pk * 10f32.powf(-50.0 / 20.0);
        let (mut held, mut active) = (0usize, 0usize);
        for i in 0..env.len() {
            if env[i] < gate {
                continue;
            }
            let lo = i.saturating_sub(25);
            let hi = (i + 26).min(env.len());
            let local = env[lo..hi].iter().copied().fold(0.0f32, f32::max);
            active += 1;
            if env[i] >= local * 0.25 {
                held += 1;
            }
        }
        if active == 0 {
            0.0
        } else {
            held as f32 / active as f32
        }
    };

    let thirds = |v: &[f32]| -> (f32, f32) {
        let n = v.len();
        if n < 3 {
            return (0.0, 0.0);
        }
        let a = v[..n / 3].iter().sum::<f32>() / (n / 3) as f32;
        let b = v[n - n / 3..].iter().sum::<f32>() / (n / 3) as f32;
        (a, b)
    };
    let (c0, c1) = thirds(&centroids);
    let (e0, e1) = thirds(&env);
    let onsets = OnsetEnvelope::from_spectrogram(&spec, 0.0, sr / 2.0);
    let transients = onsets.peaks(0.15, 0.05).len() as u32;
    let total_rms = rms(x);
    let peak = x.iter().fold(0.0f32, |m, s| m.max(s.abs()));

    Features {
        centroid_hz: (cen_num / cen_den.max(1e-20)) as f32,
        flatness: (flat_acc / flat_w.max(1e-20)) as f32,
        rolloff_hz: (roll_acc / flat_w.max(1e-20)) as f32,
        zero_crossing_rate: zc,
        band_low: (lo / tot) as f32,
        band_mid: (mid / tot) as f32,
        band_high: (hi / tot) as f32,
        attack_s,
        decay_s,
        transients,
        centroid_slope: if c0 > 0.0 { (c1 - c0) / c0 } else { 0.0 },
        level_slope_db: db(e1) - db(e0),
        crest_db: db(peak) - db(total_rms),
        sustain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth;

    #[test]
    fn full_scale_1k_sine_is_about_minus_3_lufs() {
        // A 0 dBFS 1 kHz sine reads -3.01 LUFS in one channel; BS.1770 sums channel
        // powers, so the same sine in both channels of a stereo file reads 0 LUFS.
        let sr = 48_000;
        let x = synth::tone(1000.0, 3.0, sr, &[1.0]);
        let mono = loudness(std::slice::from_ref(&x), sr);
        assert!((mono.lufs_integrated + 3.01).abs() < 0.15, "{mono:?}");
        let stereo = loudness(&[x.clone(), x], sr);
        assert!(stereo.lufs_integrated.abs() < 0.15, "{stereo:?}");
        assert!(stereo.peak_dbfs.abs() < 0.01);
    }

    #[test]
    fn noise_is_flat_and_sine_is_not() {
        let sr = 22_050;
        let n = synth::noise(sr as usize, 3);
        let s = synth::tone(440.0, 1.0, sr, &[1.0]);
        let fnz = features(&n, sr);
        let fs = features(&s, sr);
        assert!(fnz.flatness > 0.5, "{fnz:?}");
        assert!(fs.flatness < 0.05, "{fs:?}");
        assert!((fs.centroid_hz - 440.0).abs() < 60.0, "{fs:?}");
    }
}
