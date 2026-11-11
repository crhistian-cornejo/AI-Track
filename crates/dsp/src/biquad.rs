//! Biquad filters for static EQ bands, designed in `f64` and run in `f32`.
//!
//! Two design families:
//! * **RBJ** (Audio EQ Cookbook): bilinear transform, exact at the centre
//!   frequency but "cramped" near Nyquist.
//! * **Matched** (after M. Vicanek, *Matched Second Order Digital Filters*,
//!   2016): poles by impulse invariance of the analogue prototype, zeros
//!   solved so the digital magnitude equals the analogue one at DC, at
//!   Nyquist and at the centre frequency. Bells and shelves keep their
//!   analogue shape up to 20 kHz at 44.1/48 kHz.
//!
//! Run with transposed direct form II ([`BiquadState`]).

use std::f64::consts::PI;

/// Normalised coefficients (`a0 = 1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Default for BiquadCoeffs {
    fn default() -> Self {
        Self::identity()
    }
}

/// Analogue prototypes the designers know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiquadShape {
    Lowpass,
    Highpass,
    Bandpass,
    Notch,
    Bell,
    LowShelf,
    HighShelf,
    Allpass,
}

impl BiquadCoeffs {
    pub const fn identity() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 }
    }

    /// Audio EQ Cookbook design.
    pub fn rbj(shape: BiquadShape, freq: f64, q: f64, gain_db: f64, sample_rate: f64) -> Self {
        let f = freq.clamp(1.0, sample_rate * 0.4999);
        let q = q.clamp(0.025, 200.0);
        let w0 = 2.0 * PI * f / sample_rate;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q);
        let a = 10f64.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match shape {
            BiquadShape::Lowpass => ((1.0 - cs) / 2.0, 1.0 - cs, (1.0 - cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            BiquadShape::Highpass => ((1.0 + cs) / 2.0, -(1.0 + cs), (1.0 + cs) / 2.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            BiquadShape::Bandpass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            BiquadShape::Notch => (1.0, -2.0 * cs, 1.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            BiquadShape::Allpass => (1.0 - alpha, -2.0 * cs, 1.0 + alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            BiquadShape::Bell => (1.0 + alpha * a, -2.0 * cs, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cs, 1.0 - alpha / a),
            BiquadShape::LowShelf => {
                let sa = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cs + sa),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                    a * ((a + 1.0) - (a - 1.0) * cs - sa),
                    (a + 1.0) + (a - 1.0) * cs + sa,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                    (a + 1.0) + (a - 1.0) * cs - sa,
                )
            }
            BiquadShape::HighShelf => {
                let sa = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cs + sa),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                    a * ((a + 1.0) + (a - 1.0) * cs - sa),
                    (a + 1.0) - (a - 1.0) * cs + sa,
                    2.0 * ((a - 1.0) - (a + 1.0) * cs),
                    (a + 1.0) - (a - 1.0) * cs - sa,
                )
            }
        };
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    /// Magnitude-matched design (no cramping near Nyquist). Notch and
    /// all-pass have no meaningful magnitude fit and fall back to RBJ.
    pub fn matched(shape: BiquadShape, freq: f64, q: f64, gain_db: f64, sample_rate: f64) -> Self {
        let f = freq.clamp(1.0, sample_rate * 0.4999);
        let q = q.clamp(0.025, 200.0);
        let w0 = 2.0 * PI * f / sample_rate;
        let g = 10f64.powf(gain_db / 20.0);
        let a = g.sqrt();
        match shape {
            BiquadShape::Notch | BiquadShape::Allpass => Self::rbj(shape, freq, q, gain_db, sample_rate),
            BiquadShape::Lowpass => {
                let (a1, a2) = matched_poles(w0, q);
                let (aa0, aa1, aa2) = pole_terms(a1, a2);
                let (p0, p1, p2) = phis(w0);
                let r1 = (aa0 * p0 + aa1 * p1 + aa2 * p2) * q * q;
                let bb0 = aa0;
                let bb1 = ((r1 - bb0 * p0) / p1).max(0.0);
                let b0 = 0.5 * (bb0.sqrt() + bb1.sqrt());
                let b1 = bb0.sqrt() - b0;
                Self { b0, b1, b2: 0.0, a1, a2 }
            }
            BiquadShape::Highpass => {
                let (a1, a2) = matched_poles(w0, q);
                let (aa0, aa1, aa2) = pole_terms(a1, a2);
                let (p0, p1, p2) = phis(w0);
                let b0 = (aa0 * p0 + aa1 * p1 + aa2 * p2).max(0.0).sqrt() * q / (4.0 * p1);
                Self { b0, b1: -2.0 * b0, b2: b0, a1, a2 }
            }
            BiquadShape::Bandpass => {
                // Unity gain at the centre (RBJ "constant 0 dB peak").
                let (a1, a2) = matched_poles(w0, q);
                Self::fit(a1, a2, w0, |w| analog_mag2(BiquadShape::Bandpass, w / w0, q, 1.0))
            }
            BiquadShape::Bell => {
                // RBJ prototype: (s² + s A/Q + 1) / (s² + s / (A Q) + 1).
                // Cuts are the exact inverse of the mirrored boost, which keeps
                // the fit well conditioned (the boost's zeros are minimum phase).
                let a = if gain_db < 0.0 { 1.0 / a } else { a };
                let (a1, a2) = matched_poles(w0, q * a);
                let boost = Self::fit(a1, a2, w0, |w| analog_mag2(BiquadShape::Bell, w / w0, q, a));
                if gain_db < 0.0 { boost.inverse() } else { boost }
            }
            BiquadShape::LowShelf => {
                // Poles at w0 / sqrt(A) with the same Q.
                let wp = w0 / a.sqrt();
                let (a1, a2) = matched_poles(wp, q);
                Self::fit(a1, a2, w0, |w| analog_mag2(BiquadShape::LowShelf, w / w0, q, a))
            }
            BiquadShape::HighShelf => {
                let wp = (w0 * a.sqrt()).min(PI * 0.98);
                let (a1, a2) = matched_poles(wp, q);
                Self::fit(a1, a2, w0, |w| analog_mag2(BiquadShape::HighShelf, w / w0, q, a))
            }
        }
    }

    /// Solve the numerator so `|H|²` matches `target(w)` at DC, Nyquist and
    /// `w_match`, given the denominator.
    fn fit(a1: f64, a2: f64, w0: f64, target: impl Fn(f64) -> f64) -> Self {
        let (aa0, aa1, aa2) = pole_terms(a1, a2);
        let bb0 = target(0.0) * aa0;
        let bb1 = target(PI) * aa1;
        let wm = w0.clamp(1e-4, PI * 0.85);
        let (p0, p1, p2) = phis(wm);
        let den = aa0 * p0 + aa1 * p1 + aa2 * p2;
        let mut bb2 = (target(wm) * den - bb0 * p0 - bb1 * p1) / p2;
        let w_0 = bb0.max(0.0).sqrt();
        let w_1 = bb1.max(0.0).sqrt();
        let b1 = 0.5 * (w_0 - w_1);
        let s = 0.5 * (w_0 + w_1);
        if s * s + bb2 < 0.0 {
            bb2 = -s * s;
        }
        let r = (s * s + bb2).sqrt();
        let b0 = 0.5 * (s + r);
        let b2 = 0.5 * (s - r);
        Self { b0, b1, b2, a1, a2 }
    }

    /// `1 / H(z)`; valid when the numerator is minimum phase.
    pub fn inverse(&self) -> Self {
        let b0 = self.b0.max(1e-12);
        Self { b0: 1.0 / b0, b1: self.a1 / b0, b2: self.a2 / b0, a1: self.b1 / b0, a2: self.b2 / b0 }
    }

    /// Complex response at `freq`, `(re, im)`.
    pub fn response(&self, freq: f64, sample_rate: f64) -> (f64, f64) {
        let w = 2.0 * PI * freq / sample_rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        // e^{-jw} = c1 - j s1
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -self.b1 * s1 - self.b2 * s2;
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -self.a1 * s1 - self.a2 * s2;
        let den = dr * dr + di * di;
        ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den)
    }

    pub fn magnitude_db(&self, freq: f64, sample_rate: f64) -> f64 {
        let (r, i) = self.response(freq, sample_rate);
        10.0 * (r * r + i * i).max(1e-30).log10()
    }
}

/// Matched-z (impulse invariant) poles of `s² + (w/Q) s + w²`.
fn matched_poles(w0: f64, q: f64) -> (f64, f64) {
    let zeta = 1.0 / (2.0 * q);
    let e = (-zeta * w0).exp();
    let a1 = if zeta <= 1.0 { -2.0 * e * ((1.0 - zeta * zeta).sqrt() * w0).cos() } else { -2.0 * e * ((zeta * zeta - 1.0).sqrt() * w0).cosh() };
    (a1, e * e)
}

/// Vicanek's `A0, A1, A2` (squared denominator at DC, Nyquist, cross term).
fn pole_terms(a1: f64, a2: f64) -> (f64, f64, f64) {
    ((1.0 + a1 + a2).powi(2), (1.0 - a1 + a2).powi(2), -4.0 * a2)
}

fn phis(w: f64) -> (f64, f64, f64) {
    let s = (w / 2.0).sin().powi(2);
    let p0 = 1.0 - s;
    (p0, s, 4.0 * p0 * s)
}

/// `|H(jΩ)|²` of the analogue prototypes; `x = Ω / Ω0`, `a = 10^(dB/40)`.
pub fn analog_mag2(shape: BiquadShape, x: f64, q: f64, a: f64) -> f64 {
    let x2 = x * x;
    let quad = |c: f64, b: f64, aa: f64| {
        // |c s² + b s + aa|² at s = j x
        let re = aa - c * x2;
        let im = b * x;
        re * re + im * im
    };
    match shape {
        BiquadShape::Lowpass => 1.0 / quad(1.0, 1.0 / q, 1.0),
        BiquadShape::Highpass => x2 * x2 / quad(1.0, 1.0 / q, 1.0),
        BiquadShape::Bandpass => (x2 / (q * q)) / quad(1.0, 1.0 / q, 1.0),
        BiquadShape::Notch => quad(1.0, 0.0, 1.0) / quad(1.0, 1.0 / q, 1.0),
        BiquadShape::Allpass => 1.0,
        BiquadShape::Bell => quad(1.0, a / q, 1.0) / quad(1.0, 1.0 / (a * q), 1.0),
        BiquadShape::LowShelf => a * a * quad(1.0, a.sqrt() / q, a) / quad(a, a.sqrt() / q, 1.0),
        BiquadShape::HighShelf => a * a * quad(a, a.sqrt() / q, 1.0) / quad(1.0, a.sqrt() / q, a),
    }
}

/// Analogue prototype magnitude in dB at `freq` (for EQ curve drawing).
pub fn analog_magnitude_db(shape: BiquadShape, freq: f64, f0: f64, q: f64, gain_db: f64) -> f64 {
    let a = 10f64.powf(gain_db / 40.0);
    10.0 * analog_mag2(shape, freq / f0, q, a).max(1e-30).log10()
}

/// `f32` coefficients ready for the audio thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Biquad32 {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Default for Biquad32 {
    fn default() -> Self {
        BiquadCoeffs::identity().into()
    }
}

impl From<BiquadCoeffs> for Biquad32 {
    fn from(c: BiquadCoeffs) -> Self {
        Self { b0: c.b0 as f32, b1: c.b1 as f32, b2: c.b2 as f32, a1: c.a1 as f32, a2: c.a2 as f32 }
    }
}

/// Transposed direct form II state of one channel.
#[derive(Clone, Copy, Debug, Default)]
pub struct BiquadState {
    s1: f32,
    s2: f32,
}

impl BiquadState {
    #[inline]
    pub fn process(&mut self, c: &Biquad32, x: f32) -> f32 {
        let y = c.b0 * x + self.s1;
        self.s1 = c.b1 * x - c.a1 * y + self.s2;
        self.s2 = c.b2 * x - c.a2 * y;
        if self.s1.abs() < 1e-20 {
            self.s1 = 0.0;
        }
        if self.s2.abs() < 1e-20 {
            self.s2 = 0.0;
        }
        y
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// `f64` direct form I section (K-weighting and other low-frequency work
/// where `f32` state loses precision).
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad64 {
    pub c: BiquadCoeffs,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad64 {
    pub fn new(c: BiquadCoeffs) -> Self {
        Self { c, ..Default::default() }
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let c = &self.c;
        let mut y = c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        if y.abs() < 1e-30 {
            y = 0.0;
        }
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svf::tests::measure_gain_db;

    fn worst_dev(shape: BiquadShape, f0: f64, q: f64, g: f64, sr: f64, fmax: f64) -> f64 {
        let c = BiquadCoeffs::matched(shape, f0, q, g, sr);
        let mut worst = 0.0f64;
        let mut f = 20.0;
        while f < fmax {
            let d = c.magnitude_db(f, sr) - analog_magnitude_db(shape, f, f0, q, g);
            worst = worst.max(d.abs());
            f *= 1.05;
        }
        worst
    }

    #[test]
    fn matched_bells_track_analog_to_nyquist() {
        let sr = 48_000.0;
        for &(f0, q, g) in &[(1000.0, 1.0, 6.0), (10_000.0, 0.7, 9.0), (16_000.0, 1.5, -12.0), (18_000.0, 2.0, 6.0), (100.0, 4.0, 12.0)] {
            let dev = worst_dev(BiquadShape::Bell, f0, q, g, sr, 22_000.0);
            assert!(dev < 1.0, "bell {f0} Hz Q{q} {g} dB deviates {dev} dB");
            let c = BiquadCoeffs::matched(BiquadShape::Bell, f0, q, g, sr);
            assert!((c.magnitude_db(f0, sr) - g).abs() < 0.05, "centre gain");
        }
        // RBJ at 16 kHz is clearly cramped: the matched design must beat it.
        let rbj = BiquadCoeffs::rbj(BiquadShape::Bell, 16_000.0, 1.5, -12.0, sr);
        let rbj_dev = (rbj.magnitude_db(20_000.0, sr) - analog_magnitude_db(BiquadShape::Bell, 20_000.0, 16_000.0, 1.5, -12.0)).abs();
        assert!(rbj_dev > 2.0 * worst_dev(BiquadShape::Bell, 16_000.0, 1.5, -12.0, sr, 22_000.0));
    }

    #[test]
    fn matched_shelves_track_analog() {
        let sr = 48_000.0;
        for &(shape, f0, g) in &[
            (BiquadShape::LowShelf, 100.0, 6.0),
            (BiquadShape::LowShelf, 1000.0, -9.0),
            (BiquadShape::HighShelf, 8000.0, 6.0),
            (BiquadShape::HighShelf, 12_000.0, -6.0),
            (BiquadShape::HighShelf, 3000.0, 12.0),
        ] {
            let dev = worst_dev(shape, f0, 0.707, g, sr, 22_000.0);
            assert!(dev < 1.0, "{shape:?} {f0} {g}: {dev} dB");
        }
    }

    #[test]
    fn matched_cuts_and_bandpass() {
        let sr = 48_000.0;
        for f0 in [100.0, 1000.0, 10_000.0] {
            let lp = BiquadCoeffs::matched(BiquadShape::Lowpass, f0, 0.707, 0.0, sr);
            assert!(lp.magnitude_db(10.0, sr).abs() < 0.05);
            assert!((lp.magnitude_db(f0, sr) + 3.01).abs() < 0.1, "lp {f0}: {}", lp.magnitude_db(f0, sr));
            let hp = BiquadCoeffs::matched(BiquadShape::Highpass, f0, 0.707, 0.0, sr);
            assert!((hp.magnitude_db(f0, sr) + 3.01).abs() < 0.1, "hp {f0}: {}", hp.magnitude_db(f0, sr));
            assert!(hp.magnitude_db(23_000.0, sr).abs() < 0.3);
            let bp = BiquadCoeffs::matched(BiquadShape::Bandpass, f0, 2.0, 0.0, sr);
            assert!(bp.magnitude_db(f0, sr).abs() < 0.1, "bp {f0}: {}", bp.magnitude_db(f0, sr));
        }
    }

    #[test]
    fn runtime_matches_design() {
        let sr = 48_000.0;
        let c = BiquadCoeffs::matched(BiquadShape::Bell, 3000.0, 1.0, 6.0, sr);
        let c32: Biquad32 = c.into();
        for f in [100.0, 3000.0, 12_000.0] {
            let mut st = BiquadState::default();
            let m = measure_gain_db(|x| st.process(&c32, x), f, sr);
            assert!((m - c.magnitude_db(f, sr)).abs() < 0.05);
        }
    }

    #[test]
    fn rbj_is_exact_at_centre() {
        let sr = 44_100.0;
        let c = BiquadCoeffs::rbj(BiquadShape::Bell, 1000.0, 1.0, -6.0, sr);
        assert!((c.magnitude_db(1000.0, sr) + 6.0).abs() < 1e-6);
        let n = BiquadCoeffs::rbj(BiquadShape::Notch, 1000.0, 2.0, 0.0, sr);
        assert!(n.magnitude_db(1000.0, sr) < -100.0);
    }
}
