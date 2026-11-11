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
}
