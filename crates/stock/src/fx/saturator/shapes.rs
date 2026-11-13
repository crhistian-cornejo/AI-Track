//! Memoryless saturation curves with closed-form antiderivatives, and the
//! first-order antiderivative antialiasing (ADAA1) that runs them.
//!
//! ADAA1 (Parker, Zavalishin & Le Bivic, DAFx-16; Bilbao, Esqueda, Parker &
//! Välimäki, IEEE SPL 2017): instead of `f(x[n])` output the mean of `f`
//! over the segment between the last two inputs,
//! `(F(x[n]) − F(x[n−1])) / (x[n] − x[n−1])` with `F' = f`. It acts as a
//! continuous-time box filter on the shaped signal before sampling, so the
//! harmonics that fold back are attenuated by a sinc. The ill-conditioned
//! case `x[n] ≈ x[n−1]` falls back to `f` at the midpoint. Half a sample of
//! delay. Everything runs in `f64`: the difference of antiderivatives
//! cancels badly in `f32` at high drive.
//!
//! Every curve has unity slope at the origin (after the per-curve gain
//! normalisation the caller applies with [`Curve::slope0`]), so a drive of
//! 0 dB is nearly transparent at low levels.

use std::f64::consts::{FRAC_PI_2, LN_2};

/// The curves. `k` is a per-curve shape parameter (see each variant).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// `tanh(x)`: smooth, odd harmonics (tape-like when wrapped in emphasis).
    Tanh,
    /// Biased `tanh(x + k) − tanh(k)`: triode-like even harmonics.
    Tube,
    /// Exponential diode knee, `1 − e^{−x}` above zero and a harder knee
    /// at `−k` below (`k` in 0.3..1 sets the asymmetry).
    Diode,
    /// Biased algebraic sigmoid `g(x + k) − g(k)`, `g(u) = u / √(1 + u²)`:
    /// a softer knee than tanh with slowly decaying harmonics (BJT stage).
    Transistor,
    /// Hard clip at ±1.
    Clip,
    /// Clip with the negative half scaled by `k` (1 = clean, −1 = full-wave
    /// rectifier, octave up).
    Rectify,
    /// Sine wavefolder `sin(x)`.
    SineFold,
    /// Triangle wavefolder (period 4, unity slope through the origin).
    TriFold,
    /// `sin(x)` clamped at `±π/2`: the console channel encode curve.
    Sine,
    /// `asin(x)` clamped at `±1`: the console buss decode curve.
    Asin,
}

#[inline]
fn ln_cosh(x: f64) -> f64 {
    let ax = x.abs();
    if ax > 18.0 { ax - LN_2 } else { ax.cosh().ln() }
}

/// Antiderivative of the hard clip at ±1 (even, `C(0) = 0`).
#[inline]
fn clip_f(x: f64) -> f64 {
    let ax = x.abs();
    if ax <= 1.0 { 0.5 * x * x } else { ax - 0.5 }
}

/// Algebraic sigmoid and its antiderivative.
#[inline]
fn alg(u: f64) -> f64 {
    u / (1.0 + u * u).sqrt()
}

impl Curve {
    /// The curve at `x`.
    #[inline]
    pub fn f(self, x: f64, k: f64) -> f64 {
        match self {
            Curve::Tanh => x.tanh(),
            Curve::Tube => (x + k).tanh() - k.tanh(),
            Curve::Diode => {
                if x >= 0.0 {
                    1.0 - (-x).exp()
                } else {
                    -k * (1.0 - (x / k).exp())
                }
            }
            Curve::Transistor => alg(x + k) - alg(k),
            Curve::Clip => x.clamp(-1.0, 1.0),
            Curve::Rectify => {
                let c = x.clamp(-1.0, 1.0);
                if x >= 0.0 { c } else { k * c }
            }
            Curve::SineFold => x.sin(),
            Curve::TriFold => {
                let t = (x + 1.0).rem_euclid(4.0) - 2.0;
                1.0 - t.abs()
            }
            Curve::Sine => x.clamp(-FRAC_PI_2, FRAC_PI_2).sin(),
            Curve::Asin => x.clamp(-1.0, 1.0).asin(),
        }
    }

    /// Its antiderivative, `F(0) = 0`.
    #[inline]
    pub fn antiderivative(self, x: f64, k: f64) -> f64 {
        match self {
            Curve::Tanh => ln_cosh(x),
            Curve::Tube => ln_cosh(x + k) - x * k.tanh() - ln_cosh(k),
            Curve::Diode => {
                if x >= 0.0 {
                    x + (-x).exp() - 1.0
                } else {
                    -k * x + k * k * ((x / k).exp() - 1.0)
                }
            }
            Curve::Transistor => (1.0 + (x + k) * (x + k)).sqrt() - x * alg(k) - (1.0 + k * k).sqrt(),
            Curve::Clip => clip_f(x),
            Curve::Rectify => {
                if x >= 0.0 {
                    clip_f(x)
                } else {
                    k * clip_f(x)
                }
            }
            Curve::SineFold => 1.0 - x.cos(),
            Curve::TriFold => {
                // Over one period starting at x = −1 (u = 0): u²/2 − u up to
                // u = 2, then 3u − u²/2 − 4; zero mean, so F is periodic.
                let u = (x + 1.0).rem_euclid(4.0);
                let g = if u <= 2.0 { 0.5 * u * u - u } else { 3.0 * u - 0.5 * u * u - 4.0 };
                g + 0.5
            }
            Curve::Sine => {
                let ax = x.abs();
                if ax <= FRAC_PI_2 { 1.0 - x.cos() } else { 1.0 + (ax - FRAC_PI_2) }
            }
            Curve::Asin => {
                let ax = x.abs();
                if ax <= 1.0 {
                    x * x.asin() + (1.0 - x * x).sqrt() - 1.0
                } else {
                    FRAC_PI_2 - 1.0 + FRAC_PI_2 * (ax - 1.0)
                }
            }
        }
    }

    /// Slope at the origin (divide by it for unity small-signal gain).
    pub fn slope0(self, k: f64) -> f64 {
        match self {
            Curve::Tube => 1.0 - k.tanh().powi(2),
            Curve::Transistor => (1.0 + k * k).powf(-1.5),
            _ => 1.0,
        }
    }
}

/// ADAA1 waveshaper with its one-sample memory.
#[derive(Clone, Copy, Debug)]
pub struct Adaa {
    curve: Curve,
    k: f64,
    x1: f64,
    f1: f64,
}

impl Adaa {
    pub fn new(curve: Curve, k: f64) -> Self {
        Self { curve, k, x1: 0.0, f1: 0.0 }
    }

    /// Change the curve or its parameter without a click: the stored
    /// antiderivative is recomputed for the new curve.
    pub fn set(&mut self, curve: Curve, k: f64) {
        if curve != self.curve || k != self.k {
            self.curve = curve;
            self.k = k;
            self.f1 = curve.antiderivative(self.x1, k);
        }
    }

    pub fn curve(&self) -> Curve {
        self.curve
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.f1 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let fx = self.curve.antiderivative(x, self.k);
        let dx = x - self.x1;
        let y = if dx.abs() > 1e-6 { (fx - self.f1) / dx } else { self.curve.f(0.5 * (x + self.x1), self.k) };
        self.x1 = x;
        self.f1 = fx;
        y as f32
    }
}

/// Power of `x` at `freq` (Goertzel), for the family's tests.
#[cfg(test)]
pub(crate) fn power(x: &[f32], freq: f64, sr: f64) -> f64 {
    let w = std::f64::consts::TAU * freq / sr;
    let c = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s = v as f64 + c * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - c * s1 * s2) / (x.len() as f64).powi(2)
}

/// Energy (dB) of the components of `x` that are not harmonics of `f0`,
/// relative to the harmonics: Blackman–Harris windowed FFT (−92 dB side
/// lobes); bins within 6 of a harmonic count as harmonic, bins farther
/// than 10 from every harmonic
/// (and above 20 Hz) as aliases.
#[cfg(test)]
pub(crate) fn alias_db(x: &[f32], f0: f64, sr: f64) -> f64 {
    use realfft::RealFftPlanner;
    let n = x.len() & !1;
    let bh = |i: usize| {
        let t = std::f64::consts::TAU * i as f64 / n as f64;
        0.358_75 - 0.488_29 * t.cos() + 0.141_28 * (2.0 * t).cos() - 0.011_68 * (3.0 * t).cos()
    };
    let mut w: Vec<f32> = x[..n].iter().enumerate().map(|(i, v)| v * bh(i) as f32).collect();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let mut out = fft.make_output_vec();
    fft.process(&mut w, &mut out).unwrap();
    let bin = sr / n as f64;
    let (mut harm, mut alias) = (0.0f64, 0.0f64);
    for (i, c) in out.iter().enumerate() {
        let f = i as f64 * bin;
        let h = (f / f0).round().max(1.0);
        let dist = (f - h * f0).abs() / bin;
        let p = c.norm_sqr() as f64;
        if dist <= 6.0 {
            harm += p;
        } else if dist > 10.0 && f > 20.0 {
            alias += p;
        }
    }
    10.0 * (alias / harm.max(1e-30)).max(1e-30).log10()
}

/// CPU budget check for one stereo instance: 2 s of noise-ish input at
/// 48 kHz in 128-frame blocks must take under `max_pct` % of the audio's
/// duration. Dev builds (opt-level 1) are allowed 25× more, which also
/// absorbs a loaded machine (several builds in parallel); the release figure is what the budget means.
#[cfg(test)]
pub(crate) fn assert_cpu(p: &mut dyn aitrack_dsp::Processor, max_pct: f64) {
    use crate::test_util::{SR, process_stereo};
    let n = (2.0 * SR) as usize;
    let mut seed = 1u32;
    let input = move |i: usize| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5;
        let v = 0.4 * (std::f64::consts::TAU * 110.0 * i as f64 / SR).sin() as f32 + 0.2 * noise;
        (v, -v)
    };
    let t = std::time::Instant::now();
    let (l, _) = process_stereo(p, input, n);
    let pct = 100.0 * t.elapsed().as_secs_f64() / 2.0;
    assert!(l.iter().all(|v| v.is_finite()));
    let budget = if cfg!(debug_assertions) { max_pct * 25.0 } else { max_pct };
    assert!(pct < budget, "{}: {pct:.2} % of a core (budget {budget:.1} %)", p.info().id);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Curve; 10] = [
        Curve::Tanh,
        Curve::Tube,
        Curve::Diode,
        Curve::Transistor,
        Curve::Clip,
        Curve::Rectify,
        Curve::SineFold,
        Curve::TriFold,
        Curve::Sine,
        Curve::Asin,
    ];

    fn k_of(c: Curve) -> f64 {
        match c {
            Curve::Tube => 0.3,
            Curve::Diode => 0.6,
            Curve::Transistor => 0.4,
            Curve::Rectify => -0.5,
            _ => 0.0,
        }
    }

    #[test]
    fn antiderivatives_match_the_curves() {
        for c in ALL {
            let k = k_of(c);
            assert!(c.antiderivative(0.0, k).abs() < 1e-12, "{c:?}: F(0) = {}", c.antiderivative(0.0, k));
            let mut x = -7.0;
            while x < 7.0 {
                let h = 1e-5;
                let d = (c.antiderivative(x + h, k) - c.antiderivative(x - h, k)) / (2.0 * h);
                // Kinks (clip edges, fold corners) only match one side.
                let near_kink = [1.0f64, FRAC_PI_2].iter().any(|e| ((x.abs() - e).abs()) < 2e-5) || (c == Curve::TriFold && ((x + 1.0).rem_euclid(2.0) < 2e-5 || (x + 1.0).rem_euclid(2.0) > 2.0 - 2e-5));
                if !near_kink {
                    assert!((d - c.f(x, k)).abs() < 1e-4, "{c:?} at {x}: F' {d} vs f {}", c.f(x, k));
                }
                x += 0.0173;
            }
        }
    }

    #[test]
    fn unity_slope_at_the_origin() {
        for c in ALL {
            let k = k_of(c);
            let s = (c.f(1e-4, k) - c.f(-1e-4, k)) / 2e-4 / c.slope0(k);
            // The rectifier's negative half has slope k by design.
            let want = if c == Curve::Rectify { (1.0 + k) / 2.0 } else { 1.0 };
            assert!((s - want).abs() < 1e-3, "{c:?}: slope {s}");
        }
    }

    #[test]
    fn adaa_lowers_aliasing_for_every_curve() {
        let sr = 48_000.0;
        let f0 = 4_987.0;
        let n = 1 << 14;
        for c in ALL {
            let k = k_of(c);
            let input = |i: usize| (3.0 * (std::f64::consts::TAU * f0 * i as f64 / sr).sin()) as f32;
            let naive: Vec<f32> = (0..n).map(|i| c.f(input(i) as f64, k) as f32).collect();
            let mut s = Adaa::new(c, k);
            let adaa: Vec<f32> = (0..n).map(|i| s.process(input(i))).collect();
            let (a, b) = (alias_db(&naive, f0, sr), alias_db(&adaa, f0, sr));
            assert!(b < a - 3.0, "{c:?}: naive {a:.1} dB, ADAA {b:.1} dB");
        }
    }

    #[test]
    fn finite_on_extremes() {
        for c in ALL {
            let mut s = Adaa::new(c, k_of(c));
            for x in [0.0, 1e6, -1e6, 1e-30, 0.0, 100.0, 100.0, -3.0, f32::MIN_POSITIVE] {
                assert!(s.process(x).is_finite(), "{c:?} at {x}");
            }
        }
    }
}
