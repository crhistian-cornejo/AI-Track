//! Reproducible measurements for the tests of the reverb and the delay:
//! decay times per octave band, early decay time, normalised echo density,
//! spectral roughness of a tail and the spurious content of a swept tone.
//! Test-only; nothing here is on the real-time path.

use crate::test_util::SR;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use realfft::RealFftPlanner;
use std::f64::consts::TAU;

/// Octave band around `fc`: two cascaded second-order band-passes.
pub fn octave_band(x: &[f32], fc: f32) -> Vec<f32> {
    let c = SvfCoeffs::new(SvfMode::Bandpass, fc, 1.2, 0.0, SR);
    let mut a = SvfState::default();
    let mut b = SvfState::default();
    x.iter().map(|&v| b.process(&c, a.process(&c, v))).collect()
}

/// Schroeder energy decay curve in dB (0 dB at the start).
pub fn edc_db(ir: &[f32]) -> Vec<f64> {
    let mut edc = vec![0.0f64; ir.len()];
    let mut acc = 0.0f64;
    for i in (0..ir.len()).rev() {
        acc += (ir[i] as f64).powi(2);
        edc[i] = acc;
    }
    let total = edc[0].max(1e-30);
    edc.iter().map(|e| 10.0 * (e / total).max(1e-30).log10()).collect()
}

/// Decay time scaled to 60 dB from a least-squares line through the EDC
/// between `hi` and `lo` dB (T20: −5 → −25, T30: −5 → −35, EDT: 0 → −10).
pub fn decay_time(edc: &[f64], hi: f64, lo: f64) -> f32 {
    let i0 = edc.iter().position(|&d| d <= hi).unwrap_or(0);
    let i1 = edc.iter().position(|&d| d <= lo).unwrap_or(edc.len() - 1);
    let n = (i1 - i0 + 1) as f64;
    let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
    for (k, &y) in edc[i0..=i1].iter().enumerate() {
        let x = (i0 + k) as f64 / SR;
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
    }
    let slope = (n * sxy - sx * sy) / (n * sxx - sx * sx);
    (-60.0 / slope) as f32
}

/// Early decay time (0 → −10 dB, scaled to 60 dB) of a whole IR.
pub fn edt(ir: &[f32]) -> f32 {
    decay_time(&edc_db(ir), 0.0, -10.0)
}

/// T20 (−5 → −25 dB) of a whole IR.
pub fn t20(ir: &[f32]) -> f32 {
    decay_time(&edc_db(ir), -5.0, -25.0)
}

/// Normalised echo density (Abel & Huang 2006) at `centre` samples: the
/// fraction of a 20 ms window above its own standard deviation, over the
/// 31.7 % a Gaussian has. 1.0 = noise-like; a sparse train of echoes is
/// well below.
pub fn echo_density(ir: &[f32], centre: usize) -> f32 {
    let half = (0.010 * SR) as usize;
    let a = centre.saturating_sub(half);
    let b = (centre + half).min(ir.len());
    let w = &ir[a..b];
    let var = w.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / w.len() as f64;
    let sd = var.sqrt();
    let above = w.iter().filter(|&&v| (v as f64).abs() > sd).count() as f64;
    (above / w.len() as f64 / 0.3173) as f32
}

/// Spectral roughness of a stretch of noise-like signal between `lo` and
/// `hi` Hz. Returns `(std dev in dB, highest peak above its local mean in
/// dB)` of the log power spectrum after a Hann window, with the local mean
/// taken over ±`span_hz`. A Gaussian tail (smooth, no resonances) has a
/// deviation of about 5.6 dB; ringing modes push both numbers up.
pub fn roughness(x: &[f32], lo: f32, hi: f32, span_hz: f32) -> (f32, f32) {
    let n = x.len().next_power_of_two() / if x.len().is_power_of_two() { 1 } else { 2 };
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let mut buf: Vec<f64> = (0..n).map(|i| x[i] as f64 * (0.5 - 0.5 * (TAU * i as f64 / n as f64).cos())).collect();
    let mut spec = fft.make_output_vec();
    fft.process(&mut buf, &mut spec).unwrap();
    let hz = SR / n as f64;
    let db: Vec<f64> = spec.iter().map(|c| 10.0 * (c.norm_sqr() + 1e-30).log10()).collect();
    let k0 = (lo as f64 / hz) as usize;
    let k1 = ((hi as f64 / hz) as usize).min(db.len() - 1);
    let span = ((span_hz as f64 / hz) as usize).max(4);
    let (mut sum2, mut count, mut worst) = (0.0f64, 0usize, f64::MIN);
    for k in k0..k1 {
        let a = k.saturating_sub(span).max(1);
        let b = (k + span).min(db.len() - 1);
        let mean = db[a..=b].iter().sum::<f64>() / (b - a + 1) as f64;
        let d = db[k] - mean;
        sum2 += d * d;
        count += 1;
        worst = worst.max(d);
    }
    ((sum2 / count as f64).sqrt() as f32, worst as f32)
}

/// Zero-lag correlation of two channels over `from..`.
pub fn correlation(l: &[f32], r: &[f32], from: usize) -> f32 {
    let (mut xy, mut xx, mut yy) = (0.0f64, 0.0f64, 0.0f64);
    for (a, b) in l.iter().zip(r).skip(from) {
        xy += (*a as f64) * (*b as f64);
        xx += (*a as f64).powi(2);
        yy += (*b as f64).powi(2);
    }
    (xy / (xx * yy).sqrt().max(1e-30)) as f32
}

/// Hann-windowed spectrum of `x` (power, dB) with its bin width in Hz.
pub fn spectrum_db(x: &[f32]) -> (Vec<f64>, f64) {
    let n = x.len().next_power_of_two() / if x.len().is_power_of_two() { 1 } else { 2 };
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let mut buf: Vec<f64> = (0..n).map(|i| x[i] as f64 * (0.5 - 0.5 * (TAU * i as f64 / n as f64).cos())).collect();
    let mut spec = fft.make_output_vec();
    fft.process(&mut buf, &mut spec).unwrap();
    (spec.iter().map(|c| 10.0 * (c.norm_sqr() + 1e-30).log10()).collect(), SR / n as f64)
}

/// Deterministic white noise in ±`amp`.
pub fn noise(seed: u32, amp: f32) -> impl FnMut(usize) -> (f32, f32) {
    let mut n = seed.max(1);
    move |_| {
        n ^= n << 13;
        n ^= n >> 17;
        n ^= n << 5;
        let v = (n >> 8) as f32 / 8_388_608.0 - 1.0;
        (v * amp, v * amp)
    }
}

/// Power outside `±excl_hz` of `freq` relative to the power inside, in dB
/// (Hann window): what a processed tone carries that is not the tone and
/// its own close-in modulation sidebands.
pub fn spurious_db(x: &[f32], freq: f64, excl_hz: f64) -> f32 {
    let (db, hz) = spectrum_db(x);
    let (mut inside, mut outside) = (0.0f64, 0.0f64);
    for (k, d) in db.iter().enumerate() {
        let p = 10f64.powf(d / 10.0);
        if (k as f64 * hz - freq).abs() <= excl_hz {
            inside += p;
        } else {
            outside += p;
        }
    }
    (10.0 * (outside / inside.max(1e-30)).max(1e-30).log10()) as f32
}
