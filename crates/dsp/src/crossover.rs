//! Linkwitz–Riley 4th-order crossovers on TPT state-variable filters.
//!
//! An LR4 split is two cascaded Butterworth low-passes and two cascaded
//! Butterworth high-passes at the same frequency. The two outputs sum to a
//! 2nd-order all-pass, so a multi-band splitter runs each lower band through
//! the all-pass of every split above it: all bands then sum back to the
//! same all-pass response — flat magnitude, no comb.

use crate::svf::{SvfCoeffs, SvfMode, SvfState};
use std::f32::consts::FRAC_1_SQRT_2;

/// Coefficients of one LR4 split.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lr4Coeffs {
    lp: SvfCoeffs,
    hp: SvfCoeffs,
    ap: SvfCoeffs,
}

impl Lr4Coeffs {
    pub fn new(freq: f32, sample_rate: f64) -> Self {
        Self {
            lp: SvfCoeffs::new(SvfMode::Lowpass, freq, FRAC_1_SQRT_2, 0.0, sample_rate),
            hp: SvfCoeffs::new(SvfMode::Highpass, freq, FRAC_1_SQRT_2, 0.0, sample_rate),
            ap: SvfCoeffs::new(SvfMode::Allpass, freq, FRAC_1_SQRT_2, 0.0, sample_rate),
        }
    }
}

/// State of one LR4 split for one channel.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lr4 {
    lp: [SvfState; 2],
    hp: [SvfState; 2],
}

impl Lr4 {
    /// Returns `(low, high)`.
    #[inline]
    pub fn split(&mut self, c: &Lr4Coeffs, x: f32) -> (f32, f32) {
        let l1 = self.lp[0].process(&c.lp, x);
        let l = self.lp[1].process(&c.lp, l1);
        let h1 = self.hp[0].process(&c.hp, x);
        let h = self.hp[1].process(&c.hp, h1);
        (l, h)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Up to 3 bands (2 splits) for one channel. With `bands == 1` the input
/// passes straight through.
#[derive(Clone, Copy, Debug, Default)]
pub struct Crossover3 {
    coeffs: [Lr4Coeffs; 2],
    splits: [Lr4; 2],
    /// Phase compensation of the low band for the upper split.
    ap_low: SvfState,
    bands: usize,
}

impl Crossover3 {
    pub fn new(bands: usize, f1: f32, f2: f32, sample_rate: f64) -> Self {
        let mut c = Self::default();
        c.set(bands, f1, f2, sample_rate);
        c
    }

    /// Retune without clearing the state.
    pub fn set(&mut self, bands: usize, f1: f32, f2: f32, sample_rate: f64) {
        let (lo, hi) = if f1 <= f2 { (f1, f2) } else { (f2, f1) };
        self.bands = bands.clamp(1, 3);
        if self.bands == 2 {
            self.coeffs[0] = Lr4Coeffs::new(lo, sample_rate);
        } else {
            self.coeffs[0] = Lr4Coeffs::new(lo, sample_rate);
            self.coeffs[1] = Lr4Coeffs::new(hi.max(lo * 1.05), sample_rate);
        }
    }

    pub fn bands(&self) -> usize {
        self.bands
    }

    /// Split `x` into `out[..bands]`, low to high.
    #[inline]
    pub fn split(&mut self, x: f32, out: &mut [f32; 3]) {
        match self.bands {
            1 => {
                *out = [x, 0.0, 0.0];
            }
            2 => {
                let (l, h) = self.splits[0].split(&self.coeffs[0], x);
                *out = [l, h, 0.0];
            }
            _ => {
                let (l, rest) = self.splits[0].split(&self.coeffs[0], x);
                let (m, h) = self.splits[1].split(&self.coeffs[1], rest);
                let l = self.ap_low.process(&self.coeffs[1].ap, l);
                *out = [l, m, h];
            }
        }
    }

    pub fn reset(&mut self) {
        for s in &mut self.splits {
            s.reset();
        }
        self.ap_low.reset();
    }
}

/// Linear-phase 3-band crossover: two complementary Kaiser-windowed sinc
/// low-passes, `low = LP1`, `mid = LP2 − LP1`, `high = x − LP2`, all delayed
/// by the same `(taps − 1) / 2`. The three outputs therefore sum to the input
/// delayed by that many samples, exactly — no all-pass and no magnitude
/// ripple at the sum, at the price of latency.
///
/// The FIR length scales with the sample rate (~5.3 ms at any rate). The
/// coefficients are rewritten in place by [`set`](Self::set), so changing the
/// crossovers from the audio thread does not allocate.
#[derive(Clone, Debug)]
pub struct LinearPhaseCrossover3 {
    h1: Vec<f32>,
    h2: Vec<f32>,
    /// Doubled history so the convolution reads a contiguous slice.
    hist: Vec<f32>,
    pos: usize,
    n: usize,
    center: usize,
    bands: usize,
}

impl Default for LinearPhaseCrossover3 {
    fn default() -> Self {
        Self { h1: Vec::new(), h2: Vec::new(), hist: Vec::new(), pos: 0, n: 0, center: 0, bands: 1 }
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..50 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

/// Taps of the linear-phase split for a sample rate (odd, ~5.3 ms).
pub fn linear_phase_taps(sample_rate: f64) -> usize {
    let n = (511.0 * sample_rate / 48_000.0).round() as usize;
    n.max(63) | 1
}

/// Kaiser-windowed sinc low-pass into `h` (DC gain 1, symmetric).
fn design_lowpass(h: &mut [f32], freq: f32, sample_rate: f64) {
    let n = h.len();
    if n == 0 {
        return;
    }
    let m = (n - 1) as f64 / 2.0;
    let fc = (freq as f64).clamp(1.0, sample_rate * 0.49);
    let wc = 2.0 * std::f64::consts::PI * fc / sample_rate;
    let beta = 8.6; // ≈ −90 dB stop band.
    let i0 = bessel_i0(beta);
    let mut sum = 0.0;
    for (i, v) in h.iter_mut().enumerate() {
        let t = i as f64 - m;
        let sinc = if t.abs() < 1e-12 { wc / std::f64::consts::PI } else { (wc * t).sin() / (std::f64::consts::PI * t) };
        let r = t / m;
        let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0;
        *v = (sinc * w) as f32;
        sum += sinc * w;
    }
    if sum.abs() > 1e-12 {
        let inv = (1.0 / sum) as f32;
        for v in h.iter_mut() {
            *v *= inv;
        }
    }
    // Exact symmetry after the f32 rounding.
    let half = n / 2;
    for i in 0..half {
        h[n - 1 - i] = h[i];
    }
}

impl LinearPhaseCrossover3 {
    pub fn new(bands: usize, f1: f32, f2: f32, sample_rate: f64) -> Self {
        let n = linear_phase_taps(sample_rate);
        let mut c = Self { h1: vec![0.0; n], h2: vec![0.0; n], hist: vec![0.0; n * 2], pos: 0, n, center: (n - 1) / 2, bands: 1 };
        c.set(bands, f1, f2, sample_rate);
        c
    }

    /// Latency in samples of a split built for `sample_rate`.
    pub fn latency_for(sample_rate: f64) -> usize {
        (linear_phase_taps(sample_rate) - 1) / 2
    }

    /// Latency in samples.
    pub fn latency(&self) -> usize {
        self.center
    }

    pub fn taps(&self) -> usize {
        self.n
    }

    pub fn bands(&self) -> usize {
        self.bands
    }

    /// Retune without allocating. `sample_rate` must be the one the split was
    /// built with (`new`/`set` are called from `prepare` in the plugins).
    pub fn set(&mut self, bands: usize, f1: f32, f2: f32, sample_rate: f64) {
        debug_assert_eq!(self.n, linear_phase_taps(sample_rate));
        let (lo, hi) = if f1 <= f2 { (f1, f2) } else { (f2, f1) };
        self.bands = bands.clamp(1, 3);
        design_lowpass(&mut self.h1, lo, sample_rate);
        design_lowpass(&mut self.h2, hi.max(lo * 1.05), sample_rate);
    }

    pub fn reset(&mut self) {
        self.hist.fill(0.0);
        self.pos = 0;
    }

    #[inline]
    fn dot(&self, h: &[f32]) -> f32 {
        let n = self.n;
        let base = self.pos + 1;
        let center = self.center;
        let hist = &self.hist;
        let mut acc = h[center] * hist[base + center];
        for k in 0..center {
            acc += h[k] * (hist[base + k] + hist[base + n - 1 - k]);
        }
        acc
    }

    /// Split `x` into `out[..bands]`, low to high.
    #[inline]
    pub fn split(&mut self, x: f32, out: &mut [f32; 3]) {
        if self.n == 0 || self.bands == 1 {
            *out = [x, 0.0, 0.0];
            return;
        }
        self.pos += 1;
        if self.pos >= self.n {
            self.pos = 0;
        }
        self.hist[self.pos] = x;
        self.hist[self.pos + self.n] = x;
        let lp1 = self.dot(&self.h1);
        let delayed = self.hist[self.pos + 1 + self.center];
        if self.bands == 2 {
            *out = [lp1, delayed - lp1, 0.0];
        } else {
            let lp2 = self.dot(&self.h2);
            *out = [lp1, lp2 - lp1, delayed - lp2];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svf::tests::measure_gain_db;

    #[test]
    fn bands_sum_flat() {
        let sr = 48_000.0;
        for bands in [2usize, 3] {
            for f in [40.0, 200.0, 800.0, 3000.0, 12_000.0] {
                let mut x = Crossover3::new(bands, 250.0, 2500.0, sr);
                let db = measure_gain_db(
                    |s| {
                        let mut o = [0.0; 3];
                        x.split(s, &mut o);
                        o.iter().sum()
                    },
                    f,
                    sr,
                );
                assert!(db.abs() < 0.05, "{bands} bands at {f}: {db}");
            }
        }
    }

    #[test]
    fn crossover_point_is_minus_6() {
        let sr = 48_000.0;
        let mut x = Crossover3::new(2, 1000.0, 1000.0, sr);
        let db = measure_gain_db(
            |s| {
                let mut o = [0.0; 3];
                x.split(s, &mut o);
                o[0]
            },
            1000.0,
            sr,
        );
        assert!((db + 6.02).abs() < 0.05, "{db}");
    }

    #[test]
    fn linear_phase_sum_is_the_delayed_input_exactly() {
        let sr = 48_000.0;
        for bands in [2usize, 3] {
            let mut x = LinearPhaseCrossover3::new(bands, 200.0, 2500.0, sr);
            assert_eq!(x.latency(), LinearPhaseCrossover3::latency_for(sr));
            let lat = x.latency();
            let mut err = 0.0f32;
            for i in 0..8000 {
                let input = (i as f32 * 0.017).sin() * 0.7 + (i as f32 * 0.31).cos() * 0.2;
                let mut o = [0.0; 3];
                x.split(input, &mut o);
                if i >= lat + 2 {
                    let delayed = ((i - lat) as f32 * 0.017).sin() * 0.7 + ((i - lat) as f32 * 0.31).cos() * 0.2;
                    err = err.max((o[0] + o[1] + o[2] - delayed).abs());
                }
            }
            assert!(err < 2e-5, "{bands} bands: sum error {err}");
        }
    }

    #[test]
    fn linear_phase_lowpass_is_flat_in_the_passband() {
        let sr = 48_000.0;
        let mut x = LinearPhaseCrossover3::new(3, 500.0, 4000.0, sr);
        // 511 Kaiser taps (β 8.6) give a ~540 Hz transition band, so at a
        // 500 Hz split the low band is flat up to ~230 Hz and -6 dB at 500.
        for f in [20.0, 50.0, 100.0, 200.0] {
            let db = measure_gain_db(
                |s| {
                    let mut o = [0.0; 3];
                    x.split(s, &mut o);
                    o[0]
                },
                f,
                sr,
            );
            assert!(db.abs() < 0.1, "low band at {f} Hz: {db} dB");
        }
        let db = measure_gain_db(
            |s| {
                let mut o = [0.0; 3];
                x.split(s, &mut o);
                o[0]
            },
            500.0,
            sr,
        );
        assert!((db + 6.02).abs() < 0.3, "low band at the split: {db} dB");
        let db = measure_gain_db(
            |s| {
                let mut o = [0.0; 3];
                x.split(s, &mut o);
                o[2]
            },
            15_000.0,
            sr,
        );
        assert!(db.abs() < 0.1, "high band at 15 kHz: {db} dB");
    }
}
