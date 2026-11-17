//! Grain windows: six tapers tabulated once, read with linear interpolation.
//!
//! Every window is exactly zero at both ends (the Gaussian is shifted and
//! rescaled so it is, the Tukey and the Sine reach zero too), which is what
//! keeps a grain from clicking when it starts and stops. The *shape* knob
//! moves the peak of the window along the grain (a piecewise-linear time
//! warp that keeps both ends at zero): early peak = percussive grain, late
//! peak = reversed-envelope swell.
//!
//! References: Roads, *Microsound* (2001) ch. 3 on grain envelopes; the
//! Hann / Tukey / Gaussian forms are the textbook ones (Harris 1978), and
//! the "a Gaussian must be multiplied by something that reaches zero or it
//! pops" caveat is the usual one for granular engines.

pub const TABLE: usize = 1024;
pub const NAMES: &[&str] = &["Hann", "Tukey", "Gauss", "Triangle", "Blackman", "Sine"];
pub const COUNT: usize = 6;

/// Tabulated windows (`TABLE + 1` points so `idx + 1` is always valid) and
/// the mean power of each (for the overlap gain compensation).
pub struct Windows {
    tables: [[f32; TABLE + 1]; COUNT],
    power: [f32; COUNT],
}

fn shape(kind: usize, x: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    match kind {
        0 => 0.5 - 0.5 * (TAU * x).cos(),
        1 => {
            // Tukey, alpha = 0.5: cosine tapers over the outer quarters.
            let a = 0.5;
            if x < a * 0.5 {
                0.5 - 0.5 * (TAU * x / a).cos()
            } else if x > 1.0 - a * 0.5 {
                0.5 - 0.5 * (TAU * (1.0 - x) / a).cos()
            } else {
                1.0
            }
        }
        2 => {
            let g = |x: f64| (-0.5 * ((x - 0.5) / 0.17).powi(2)).exp();
            let e = g(0.0);
            (g(x) - e) / (1.0 - e)
        }
        3 => 1.0 - (2.0 * x - 1.0).abs(),
        4 => 0.42 - 0.5 * (TAU * x).cos() + 0.08 * (2.0 * TAU * x).cos(),
        _ => (PI * x).sin(),
    }
}

impl Windows {
    pub fn new() -> Self {
        let mut tables = [[0.0; TABLE + 1]; COUNT];
        let mut power = [0.0; COUNT];
        for k in 0..COUNT {
            let mut acc = 0.0f64;
            for i in 0..=TABLE {
                let v = shape(k, i as f64 / TABLE as f64).max(0.0);
                tables[k][i] = v as f32;
                acc += v * v;
            }
            tables[k][0] = 0.0;
            tables[k][TABLE] = 0.0;
            power[k] = (acc / (TABLE + 1) as f64) as f32;
        }
        Self { tables, power }
    }

    /// Mean of `w²` over the grain.
    pub fn power(&self, kind: usize) -> f32 {
        self.power[kind.min(COUNT - 1)]
    }

    /// Window value at `u ∈ [0, 1]` of the (already warped) grain phase.
    #[inline]
    pub fn at(&self, kind: usize, u: f32) -> f32 {
        let x = u.clamp(0.0, 1.0) * TABLE as f32;
        let i = (x as usize).min(TABLE - 1);
        let t = x - i as f32;
        let tab = &self.tables[kind];
        tab[i] + (tab[i + 1] - tab[i]) * t
    }
}

impl Default for Windows {
    fn default() -> Self {
        Self::new()
    }
}

/// Piecewise-linear warp moving the window peak to `peak ∈ (0, 1)`:
/// returns the two slopes `(before, after)` of the map `u → u'`.
#[inline]
pub fn warp_slopes(peak: f32) -> (f32, f32) {
    let a = peak.clamp(0.1, 0.9);
    (0.5 / a, 0.5 / (1.0 - a))
}

/// Apply the warp.
#[inline]
pub fn warp(u: f32, peak: f32, s1: f32, s2: f32) -> f32 {
    if u < peak { u * s1 } else { 0.5 + (u - peak) * s2 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_window_is_zero_at_both_ends_and_bounded() {
        let w = Windows::new();
        for k in 0..COUNT {
            assert_eq!(w.at(k, 0.0), 0.0, "{}", NAMES[k]);
            assert_eq!(w.at(k, 1.0), 0.0, "{}", NAMES[k]);
            let mut peak = 0.0f32;
            for i in 0..=1000 {
                let v = w.at(k, i as f32 / 1000.0);
                assert!((0.0..=1.0001).contains(&v), "{} {v}", NAMES[k]);
                peak = peak.max(v);
            }
            assert!(peak > 0.99, "{} peak {peak}", NAMES[k]);
            assert!(w.power(k) > 0.1 && w.power(k) < 0.7, "{} power {}", NAMES[k], w.power(k));
        }
    }

    #[test]
    fn warp_keeps_the_ends_and_moves_the_peak() {
        let (s1, s2) = warp_slopes(0.25);
        assert_eq!(warp(0.0, 0.25, s1, s2), 0.0);
        assert!((warp(0.25, 0.25, s1, s2) - 0.5).abs() < 1e-6);
        assert!((warp(1.0, 0.25, s1, s2) - 1.0).abs() < 1e-6);
        // Monotonic.
        let mut prev = -1.0;
        for i in 0..=100 {
            let v = warp(i as f32 / 100.0, 0.25, s1, s2);
            assert!(v >= prev);
            prev = v;
        }
    }
}
