//! Shared signal-processing building blocks: windows, STFT, onset strength,
//! peak picking and YIN pitch estimation.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex32;

/// Analysis sample rate used by the pure-DSP analysers.
pub const ANALYSIS_RATE: u32 = 22_050;

pub fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos())
        .collect()
}

/// Magnitude spectrogram: `frames[t][bin]`, `n_fft / 2 + 1` bins, Hann window,
/// frame `t` starts at sample `t * hop` (no centring).
pub struct Spectrogram {
    pub frames: Vec<Vec<f32>>,
    pub n_fft: usize,
    pub hop: usize,
    pub sample_rate: u32,
}

impl Spectrogram {
    pub fn compute(x: &[f32], sample_rate: u32, n_fft: usize, hop: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft: Arc<dyn RealToComplex<f32>> = planner.plan_fft_forward(n_fft);
        let win = hann(n_fft);
        let mut input = fft.make_input_vec();
        let mut spec = fft.make_output_vec();
        let mut frames = Vec::new();
        let n_frames = if x.len() <= n_fft {
            1
        } else {
            1 + (x.len() - n_fft).div_ceil(hop)
        };
        for t in 0..n_frames {
            let start = t * hop;
            for (i, v) in input.iter_mut().enumerate() {
                *v = x.get(start + i).copied().unwrap_or(0.0) * win[i];
            }
            fft.process(&mut input, &mut spec).expect("fft sizes match");
            frames.push(spec.iter().map(|c: &Complex32| c.norm()).collect());
        }
        Self {
            frames,
            n_fft,
            hop,
            sample_rate,
        }
    }

    pub fn bin_hz(&self, bin: usize) -> f32 {
        bin as f32 * self.sample_rate as f32 / self.n_fft as f32
    }

    pub fn hz_bin(&self, hz: f32) -> usize {
        ((hz * self.n_fft as f32 / self.sample_rate as f32).round() as usize).min(self.n_fft / 2)
    }

    pub fn frame_time(&self, t: usize) -> f64 {
        (t * self.hop) as f64 / self.sample_rate as f64
    }
}

/// Spectral-flux onset strength envelope (log-compressed, half-wave rectified,
/// local-mean removed). One value per hop; `fps = sr / hop`.
pub struct OnsetEnvelope {
    pub values: Vec<f32>,
    pub hop: usize,
    pub sample_rate: u32,
}

impl OnsetEnvelope {
    pub fn compute(x: &[f32], sample_rate: u32, n_fft: usize, hop: usize) -> Self {
        let spec = Spectrogram::compute(x, sample_rate, n_fft, hop);
        Self::from_spectrogram(&spec, 0.0, sample_rate as f32 / 2.0)
    }

    /// Flux restricted to `[lo_hz, hi_hz]`.
    pub fn from_spectrogram(spec: &Spectrogram, lo_hz: f32, hi_hz: f32) -> Self {
        let lo = spec.hz_bin(lo_hz);
        let hi = spec.hz_bin(hi_hz).max(lo + 1);
        let logged: Vec<Vec<f32>> = spec
            .frames
            .iter()
            .map(|f| f[lo..hi].iter().map(|m| (1.0 + 100.0 * m).ln()).collect())
            .collect();
        let mut values = vec![0.0f32; logged.len()];
        for t in 1..logged.len() {
            values[t] = logged[t]
                .iter()
                .zip(&logged[t - 1])
                .map(|(a, b)| (a - b).max(0.0))
                .sum();
        }
        // Remove the slowly varying part so sustained loudness changes don't dominate.
        let w = 8usize;
        let smooth: Vec<f32> = (0..values.len())
            .map(|i| {
                let a = i.saturating_sub(w);
                let b = (i + w + 1).min(values.len());
                values[a..b].iter().sum::<f32>() / (b - a) as f32
            })
            .collect();
        let values = values
            .iter()
            .zip(&smooth)
            .map(|(v, m)| (v - m).max(0.0))
            .collect();
        Self {
            values,
            hop: spec.hop,
            sample_rate: spec.sample_rate,
        }
    }

    pub fn fps(&self) -> f64 {
        self.sample_rate as f64 / self.hop as f64
    }

    /// Frames that are onsets: local maxima above an adaptive threshold,
    /// at least `min_gap_s` apart.
    pub fn peaks(&self, delta: f32, min_gap_s: f64) -> Vec<usize> {
        pick_peaks(
            &self.values,
            delta,
            (min_gap_s * self.fps()).round().max(1.0) as usize,
        )
    }
}

/// Peak picking on a non-negative envelope. A frame is a peak if it is the
/// maximum of a ±3 frame neighbourhood, exceeds the local mean (±16 frames)
/// plus `delta * max`, and is at least `min_gap` frames after the last peak
/// (a louder peak inside the gap replaces the previous one).
pub fn pick_peaks(env: &[f32], delta: f32, min_gap: usize) -> Vec<usize> {
    let max = env.iter().copied().fold(0.0f32, f32::max);
    if max <= 1e-9 {
        return Vec::new();
    }
    let mut peaks: Vec<usize> = Vec::new();
    for i in 0..env.len() {
        let a = i.saturating_sub(3);
        let b = (i + 4).min(env.len());
        if env[i] < env[a..b].iter().copied().fold(0.0, f32::max) || env[i] <= 0.0 {
            continue;
        }
        let ma = i.saturating_sub(16);
        let mb = (i + 17).min(env.len());
        let mean = env[ma..mb].iter().sum::<f32>() / (mb - ma) as f32;
        if env[i] < mean + delta * max {
            continue;
        }
        match peaks.last() {
            Some(&p) if i - p < min_gap => {
                if env[i] > env[p] {
                    *peaks.last_mut().unwrap() = i;
                }
            }
            _ => peaks.push(i),
        }
    }
    peaks
}

/// YIN fundamental-frequency estimator (de Cheveigné & Kawahara 2002) with the
/// difference function computed through FFT cross-correlation.
pub struct Yin {
    window: usize,
    max_lag: usize,
    min_lag: usize,
    sample_rate: u32,
    threshold: f32,
    fft_len: usize,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn realfft::ComplexToReal<f32>>,
}

/// One YIN estimate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PitchEstimate {
    pub hz: f32,
    /// Normalised difference at the chosen lag: 0 = perfectly periodic, 1 = noise.
    pub aperiodicity: f32,
}

impl PitchEstimate {
    pub fn midi(&self) -> f32 {
        hz_to_midi(self.hz)
    }
}

impl Yin {
    pub fn new(sample_rate: u32, fmin: f32, fmax: f32, window: usize) -> Self {
        let max_lag = (sample_rate as f32 / fmin).ceil() as usize + 2;
        let min_lag = ((sample_rate as f32 / fmax).floor() as usize).max(2);
        let fft_len = (window + max_lag + window).next_power_of_two();
        let mut planner = RealFftPlanner::<f32>::new();
        Self {
            window,
            max_lag,
            min_lag,
            sample_rate,
            threshold: 0.15,
            fft_len,
            fwd: planner.plan_fft_forward(fft_len),
            inv: planner.plan_fft_inverse(fft_len),
        }
    }

    /// Samples needed per estimate.
    pub fn frame_len(&self) -> usize {
        self.window + self.max_lag
    }

    /// Estimate f0 for the frame starting at `x[0]` (needs `frame_len()` samples; zero-padded).
    pub fn estimate(&self, x: &[f32]) -> Option<PitchEstimate> {
        let n = self.frame_len();
        let w = self.window;
        let get = |i: usize| x.get(i).copied().unwrap_or(0.0);
        // Cross-correlation r(tau) = sum_{j<w} x[j] x[j+tau] via FFT.
        let mut a = vec![0.0f32; self.fft_len];
        let mut b = vec![0.0f32; self.fft_len];
        for (j, v) in a.iter_mut().enumerate().take(w) {
            *v = get(j);
        }
        for (j, v) in b.iter_mut().enumerate().take(n) {
            *v = get(j);
        }
        let mut fa = self.fwd.make_output_vec();
        let mut fb = self.fwd.make_output_vec();
        self.fwd.process(&mut a, &mut fa).ok()?;
        self.fwd.process(&mut b, &mut fb).ok()?;
        for (p, q) in fa.iter_mut().zip(&fb) {
            *p = p.conj() * q;
        }
        let mut r = vec![0.0f32; self.fft_len];
        self.inv.process(&mut fa, &mut r).ok()?;
        let scale = 1.0 / self.fft_len as f32;
        // Prefix sums of energy.
        let mut cum = vec![0.0f64; n + 1];
        for i in 0..n {
            cum[i + 1] = cum[i] + (get(i) as f64).powi(2);
        }
        let e0 = cum[w];
        if e0 < 1e-10 {
            return None;
        }
        let max_lag = self.max_lag.min(n - w);
        let mut d = vec![0.0f32; max_lag + 1];
        for (tau, dv) in d.iter_mut().enumerate().skip(1) {
            let et = cum[tau + w] - cum[tau];
            *dv = (e0 + et - 2.0 * (r[tau] * scale) as f64).max(0.0) as f32;
        }
        // Cumulative mean normalised difference.
        let mut cmnd = vec![1.0f32; max_lag + 1];
        let mut running = 0.0f32;
        for tau in 1..=max_lag {
            running += d[tau];
            cmnd[tau] = if running > 0.0 {
                d[tau] * tau as f32 / running
            } else {
                1.0
            };
        }
        let mut tau = None;
        let mut t = self.min_lag;
        while t < max_lag {
            if cmnd[t] < self.threshold {
                while t + 1 < max_lag && cmnd[t + 1] < cmnd[t] {
                    t += 1;
                }
                tau = Some(t);
                break;
            }
            t += 1;
        }
        let tau = match tau {
            Some(t) => t,
            None => {
                // No dip under the threshold: take the global minimum (weakly periodic).
                (self.min_lag..max_lag).min_by(|&a, &b| cmnd[a].total_cmp(&cmnd[b]))?
            }
        };
        let ap = cmnd[tau];
        // Parabolic interpolation on the raw difference function.
        let refined = if tau > 1 && tau < max_lag {
            let (s0, s1, s2) = (d[tau - 1], d[tau], d[tau + 1]);
            let den = s0 + s2 - 2.0 * s1;
            if den.abs() > 1e-12 {
                tau as f32 + 0.5 * (s0 - s2) / den
            } else {
                tau as f32
            }
        } else {
            tau as f32
        };
        Some(PitchEstimate {
            hz: self.sample_rate as f32 / refined,
            aperiodicity: ap,
        })
    }
}

pub fn hz_to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

pub fn midi_to_hz(midi: f32) -> f32 {
    440.0 * 2f32.powf((midi - 69.0) / 12.0)
}

pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        0.0
    } else {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }
}

pub fn db(x: f32) -> f32 {
    20.0 * x.max(1e-10).log10()
}

pub fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    let m = v.len() / 2;
    if v.len() % 2 == 1 {
        v[m]
    } else {
        0.5 * (v[m - 1] + v[m])
    }
}

#[cfg(test)]
pub(crate) mod synth {
    //! Deterministic test-signal generators.
    use std::f32::consts::PI;

    pub fn tone(freq: f32, secs: f32, sr: u32, harmonics: &[f32]) -> Vec<f32> {
        let n = (secs * sr as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / sr as f32;
                harmonics
                    .iter()
                    .enumerate()
                    .map(|(h, a)| a * (2.0 * PI * freq * (h + 1) as f32 * t).sin())
                    .sum::<f32>()
            })
            .collect()
    }

    /// Simple LCG noise in -1..1.
    pub fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    }

    /// Synthetic kick: sine sweeping 160 → 45 Hz with exponential decay.
    pub fn kick(sr: u32, secs: f32) -> Vec<f32> {
        let n = (secs * sr as f32) as usize;
        let mut phase = 0.0f32;
        (0..n)
            .map(|i| {
                let t = i as f32 / sr as f32;
                let f = 45.0 + 115.0 * (-t * 30.0).exp();
                phase += 2.0 * PI * f / sr as f32;
                phase.sin() * (-t * 7.0).exp() * 0.9
            })
            .collect()
    }

    /// Synthetic closed hi-hat: first-difference (high-passed) noise, fast decay.
    pub fn hat(sr: u32, secs: f32, seed: u32) -> Vec<f32> {
        let n = (secs * sr as f32) as usize;
        let w = noise(n + 2, seed);
        (0..n)
            .map(|i| {
                let t = i as f32 / sr as f32;
                let hp = w[i + 2] - 2.0 * w[i + 1] + w[i];
                0.3 * hp * (-t * 60.0).exp()
            })
            .collect()
    }

    /// Synthetic snare: 190 Hz body + band-ish noise.
    pub fn snare(sr: u32, secs: f32, seed: u32) -> Vec<f32> {
        let n = (secs * sr as f32) as usize;
        let w = noise(n + 1, seed);
        (0..n)
            .map(|i| {
                let t = i as f32 / sr as f32;
                let body = (2.0 * PI * 190.0 * t).sin() * (-t * 25.0).exp() * 0.5;
                let nz = (w[i + 1] - w[i]) * 0.5 * (-t * 18.0).exp();
                body + nz
            })
            .collect()
    }

    pub fn mix_at(dst: &mut [f32], src: &[f32], at: usize) {
        for (i, s) in src.iter().enumerate() {
            if let Some(d) = dst.get_mut(at + i) {
                *d += s;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yin_finds_a440() {
        let sr = 22_050;
        let x = synth::tone(440.0, 0.2, sr, &[1.0, 0.5, 0.3]);
        let yin = Yin::new(sr, 40.0, 2000.0, 1024);
        let p = yin.estimate(&x).unwrap();
        assert!((p.hz - 440.0).abs() < 1.0, "{p:?}");
        assert!(p.aperiodicity < 0.1);
    }

    #[test]
    fn yin_low_808() {
        let sr = 22_050;
        let x = synth::tone(43.65, 0.5, sr, &[1.0]);
        let yin = Yin::new(sr, 30.0, 1000.0, 2048);
        let p = yin.estimate(&x).unwrap();
        assert!((p.midi() - 29.0).abs() < 0.2, "{p:?}");
    }
}
