//! Memoryless waveshapers with first-order antiderivative antialiasing.
//!
//! ADAA1 (Parker, Zavalishin & Le Bivic, DAFx-16; Bilbao et al., IEEE SPL
//! 2017): instead of `f(x[n])`, output the mean of `f` over the segment
//! between the last two inputs, `(F(x[n]) − F(x[n−1])) / (x[n] − x[n−1])`,
//! where `F` is the antiderivative. Aliasing drops by roughly the amount
//! 2–4× oversampling would buy; combined with 2× IIR oversampling it rivals
//! naive 8×. The ill-conditioned case `x[n] ≈ x[n−1]` falls back to `f` at
//! the midpoint. Adds half a sample of delay.

/// Shaping curves with closed-form antiderivatives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaperKind {
    /// Smooth saturation, `tanh(x)`.
    Tanh,
    /// Hard clip at ±1.
    HardClip,
    /// Sine wavefolder `sin(π x / 2)`: folds back past ±1.
    Fold,
    /// Asymmetric `tanh(x + b) − tanh(b)`: even harmonics (tube-like).
    Asym,
    /// Cubic soft clip `x − x³/3` (clamped at ±1, 2/3).
    Cubic,
}

#[inline]
fn ln_cosh(x: f64) -> f64 {
    let ax = x.abs();
    if ax > 18.0 { ax - std::f64::consts::LN_2 } else { ax.cosh().ln() }
}

impl ShaperKind {
    /// The curve.
    #[inline]
    pub fn f(self, x: f64, bias: f64) -> f64 {
        match self {
            ShaperKind::Tanh => x.tanh(),
            ShaperKind::HardClip => x.clamp(-1.0, 1.0),
            ShaperKind::Fold => (std::f64::consts::FRAC_PI_2 * x).sin(),
            ShaperKind::Asym => (x + bias).tanh() - bias.tanh(),
            ShaperKind::Cubic => {
                let c = x.clamp(-1.0, 1.0);
                1.5 * (c - c * c * c / 3.0)
            }
        }
    }

    /// Its antiderivative.
    #[inline]
    pub fn antiderivative(self, x: f64, bias: f64) -> f64 {
        match self {
            ShaperKind::Tanh => ln_cosh(x),
            ShaperKind::HardClip => {
                if x.abs() <= 1.0 {
                    0.5 * x * x
                } else {
                    x.abs() - 0.5
                }
            }
            ShaperKind::Fold => -(std::f64::consts::FRAC_PI_2 * x).cos() / std::f64::consts::FRAC_PI_2,
            ShaperKind::Asym => ln_cosh(x + bias) - x * bias.tanh(),
            ShaperKind::Cubic => {
                if x.abs() <= 1.0 {
                    1.5 * (0.5 * x * x - x.powi(4) / 12.0)
                } else {
                    // ∫ of the clamped curve: value at 1 plus ±1 · (|x| − 1).
                    1.5 * (0.5 - 1.0 / 12.0) + (x.abs() - 1.0)
                }
            }
        }
    }
}

/// ADAA1 waveshaper with its one-sample memory.
#[derive(Clone, Copy, Debug)]
pub struct Adaa1 {
    pub kind: ShaperKind,
    pub bias: f64,
    x1: f64,
    f1: f64,
}

impl Adaa1 {
    pub fn new(kind: ShaperKind) -> Self {
        let mut s = Self { kind, bias: 0.0, x1: 0.0, f1: 0.0 };
        s.reset();
        s
    }

    pub fn set_kind(&mut self, kind: ShaperKind) {
        if kind != self.kind {
            self.kind = kind;
            self.f1 = kind.antiderivative(self.x1, self.bias);
        }
    }

    pub fn set_bias(&mut self, bias: f64) {
        if bias != self.bias {
            self.bias = bias;
            self.f1 = self.kind.antiderivative(self.x1, bias);
        }
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.f1 = self.kind.antiderivative(0.0, self.bias);
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let fx = self.kind.antiderivative(x, self.bias);
        let dx = x - self.x1;
        let y = if dx.abs() > 1e-5 { (fx - self.f1) / dx } else { self.kind.f(0.5 * (x + self.x1), self.bias) };
        self.x1 = x;
        self.f1 = fx;
        y as f32
    }
}

/// Bit depth / sample-rate reducer (aliasing is the point; no ADAA).
#[derive(Clone, Copy, Debug, Default)]
pub struct Bitcrusher {
    hold: f32,
    phase: f32,
}

impl Bitcrusher {
    /// `bits` 1..24, `rate_ratio` 0..1 of the host rate.
    #[inline]
    pub fn process(&mut self, x: f32, bits: f32, rate_ratio: f32) -> f32 {
        self.phase += rate_ratio.clamp(0.001, 1.0);
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            let levels = 2f32.powf(bits.clamp(1.0, 24.0) - 1.0);
            self.hold = (x * levels).round() / levels;
        }
        self.hold
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use realfft::RealFftPlanner;

    #[test]
    fn antiderivatives_are_consistent() {
        for kind in [ShaperKind::Tanh, ShaperKind::HardClip, ShaperKind::Fold, ShaperKind::Asym, ShaperKind::Cubic] {
            let b = 0.3;
            let mut x = -3.0;
            while x < 3.0 {
                let h = 1e-5;
                let d = (kind.antiderivative(x + h, b) - kind.antiderivative(x - h, b)) / (2.0 * h);
                assert!((d - kind.f(x, b)).abs() < 1e-4, "{kind:?} at {x}: {d} vs {}", kind.f(x, b));
                x += 0.0137;
            }
        }
    }

    fn alias_energy(mut f: impl FnMut(f32) -> f32) -> f32 {
        let sr = 48_000.0f64;
        let n = 1 << 14;
        let f0 = 5_003.0;
        let mut x: Vec<f32> = (0..n)
            .map(|i| {
                let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
                (f((4.0 * (std::f64::consts::TAU * f0 * i as f64 / sr).sin()) as f32) as f64 * w) as f32
            })
            .collect();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let mut out = fft.make_output_vec();
        fft.process(&mut x, &mut out).unwrap();
        let bin_hz = sr / n as f64;
        let mut alias = 0.0;
        for (i, c) in out.iter().enumerate() {
            let fq = i as f64 * bin_hz;
            let h = (fq / f0).round();
            if (fq - h * f0).abs() > 30.0 {
                alias += c.norm_sqr();
            }
        }
        alias
    }

    #[test]
    fn adaa_reduces_aliasing() {
        for kind in [ShaperKind::Tanh, ShaperKind::HardClip] {
            let naive = alias_energy(|x| kind.f(x as f64, 0.0) as f32);
            let mut s = Adaa1::new(kind);
            let adaa = alias_energy(|x| s.process(x));
            let gain_db = 10.0 * (naive / adaa).log10();
            // ADAA1 alone at a 5 kHz fundamental: ~6 dB (tanh) to ~10 dB (clip);
            // pair it with 2× oversampling for more.
            assert!(gain_db > 4.0, "{kind:?}: only {gain_db} dB less aliasing");
        }
    }

    #[test]
    fn finite_on_extremes() {
        let mut s = Adaa1::new(ShaperKind::Tanh);
        for x in [0.0, 1e6, -1e6, 1e-30, 0.0, 100.0, 100.0] {
            assert!(s.process(x).is_finite());
        }
    }
}
