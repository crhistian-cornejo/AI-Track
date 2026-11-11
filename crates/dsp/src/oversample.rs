//! Polyphase IIR half-band oversampling (2× and 4×).
//!
//! Each 2× stage is a half-band filter built from two parallel chains of
//! first-order all-pass sections in `z⁻²` (M. Valenzuela & A. Constantinides,
//! *Digital signal processing schemes for efficient interpolation and
//! decimation*, IEE Proc. 1983). Coefficients come from the closed-form
//! elliptic design popularised by L. de Soras' HIIR. Phase is non-linear
//! but smooth, the delay is a few samples and the cost is a handful of
//! multiplies per sample — the default for saturators and filters.

use std::f64::consts::PI;

/// Most all-pass coefficients one stage holds.
pub const MAX_COEFS: usize = 12;

/// Half-band all-pass coefficients for `n` coefficients and a normalised
/// transition bandwidth (relative to the high sample rate, `0 < tbw < 0.5`).
pub fn halfband_coefs(n: usize, transition: f64) -> Vec<f64> {
    let n = n.clamp(1, MAX_COEFS);
    let (k, q) = transition_params(transition.clamp(1e-4, 0.4999));
    let order = n * 2 + 1;
    (0..n).map(|i| coef(i, k, q, order)).collect()
}

fn transition_params(transition: f64) -> (f64, f64) {
    let mut k = ((1.0 - transition * 2.0) * PI / 4.0).tan();
    k *= k;
    let kksqrt = (1.0 - k * k).powf(0.25);
    let e = 0.5 * (1.0 - kksqrt) / (1.0 + kksqrt);
    let e2 = e * e;
    let e4 = e2 * e2;
    let q = e * (1.0 + e4 * (2.0 + e4 * (15.0 + 150.0 * e4)));
    (k, q)
}

fn coef(index: usize, k: f64, q: f64, order: usize) -> f64 {
    let c = (index + 1) as f64;
    let ord = order as f64;
    let mut num = 0.0;
    let mut i = 0i32;
    let mut sign = 1.0;
    loop {
        let cur = q.powi(i * (i + 1)) * (((i * 2 + 1) as f64) * c * PI / ord).sin() * sign;
        num += cur;
        sign = -sign;
        i += 1;
        if cur.abs() < 1e-100 || i > 200 {
            break;
        }
    }
    let mut den = 0.0;
    let mut i = 1i32;
    let mut sign = -1.0;
    loop {
        let cur = q.powi(i * i) * ((i * 2) as f64 * c * PI / ord).cos() * sign;
        den += cur;
        sign = -sign;
        i += 1;
        if cur.abs() < 1e-100 || i > 200 {
            break;
        }
    }
    let ww = num * q.powf(0.25) / (den + 0.5);
    let wwsq = ww * ww;
    let x = ((1.0 - wwsq * k) * (1.0 - wwsq / k)).sqrt() / (1.0 + wwsq);
    (1.0 - x) / (1.0 + x)
}

/// One all-pass chain running at the low rate.
#[derive(Clone, Copy, Debug, Default)]
struct Chain {
    a: [f32; MAX_COEFS / 2],
    x: [f32; MAX_COEFS / 2],
    y: [f32; MAX_COEFS / 2],
    n: usize,
}

impl Chain {
    #[inline]
    fn process(&mut self, mut v: f32) -> f32 {
        for i in 0..self.n {
            let out = self.a[i] * (v - self.y[i]) + self.x[i];
            self.x[i] = v;
            self.y[i] = if out.abs() < 1e-25 { 0.0 } else { out };
            v = out;
        }
        v
    }

    fn reset(&mut self) {
        self.x = [0.0; MAX_COEFS / 2];
        self.y = [0.0; MAX_COEFS / 2];
    }
}

/// One half-band 2× stage (used for both directions).
#[derive(Clone, Copy, Debug, Default)]
pub struct Halfband {
    even: Chain,
    odd: Chain,
    coefs: [f64; MAX_COEFS],
    n: usize,
}

impl Halfband {
    pub fn new(n: usize, transition: f64) -> Self {
        let c = halfband_coefs(n, transition);
        let mut hb = Self { n: c.len(), ..Default::default() };
        for (i, &a) in c.iter().enumerate() {
            hb.coefs[i] = a;
            if i % 2 == 0 {
                hb.even.a[hb.even.n] = a as f32;
                hb.even.n += 1;
            } else {
                hb.odd.a[hb.odd.n] = a as f32;
                hb.odd.n += 1;
            }
        }
        hb
    }

    /// Interpolate one input sample into two output samples.
    #[inline]
    pub fn upsample(&mut self, x: f32) -> (f32, f32) {
        (self.even.process(x), self.odd.process(x))
    }

    /// Decimate two input samples into one.
    #[inline]
    pub fn downsample(&mut self, x0: f32, x1: f32) -> f32 {
        0.5 * (self.even.process(x1) + self.odd.process(x0))
    }

    pub fn reset(&mut self) {
        self.even.reset();
        self.odd.reset();
    }

    /// Low-frequency group delay of the half-band filter, in samples at the
    /// high rate.
    pub fn group_delay(&self) -> f64 {
        // Phase of H(e^{jw}) = ½ (A0(e^{2jw}) + e^{-jw} A1(e^{2jw})) at two
        // small frequencies.
        let phase = |w: f64| {
            let chain = |parity: usize| {
                let mut ph = 0.0;
                for i in (parity..self.n).step_by(2) {
                    let a = self.coefs[i];
                    // (a + z^-2) / (1 + a z^-2)
                    let (s, c) = (2.0 * w).sin_cos();
                    let num = (a + c, -s);
                    let den = (1.0 + a * c, -a * s);
                    ph += num.1.atan2(num.0) - den.1.atan2(den.0);
                }
                ph
            };
            let p0 = chain(0);
            let p1 = chain(1) - w;
            let re = p0.cos() + p1.cos();
            let im = p0.sin() + p1.sin();
            im.atan2(re)
        };
        let w = 1e-3;
        -(phase(2.0 * w) - phase(w)) / w
    }
}

/// Mono 1×/2×/4× oversampler. Factor changes are applied in `set_factor`
/// (control thread); processing never allocates.
#[derive(Clone, Copy, Debug)]
pub struct Oversampler {
    factor: usize,
    up: [Halfband; 2],
    down: [Halfband; 2],
}

impl Default for Oversampler {
    fn default() -> Self {
        Self::new(1)
    }
}

impl Oversampler {
    /// First stage: 10 coefficients, 2 % transition (≈ −100 dB stop band);
    /// second stage only has to reject what is above 24 kHz at 4×.
    pub fn new(factor: usize) -> Self {
        let s1 = Halfband::new(10, 0.02);
        let s2 = Halfband::new(4, 0.24);
        let mut o = Self { factor: 1, up: [s1, s2], down: [s1, s2] };
        o.set_factor(factor);
        o
    }

    pub fn set_factor(&mut self, factor: usize) {
        self.factor = match factor {
            0..=1 => 1,
            2..=3 => 2,
            _ => 4,
        };
        self.reset();
    }

    pub fn factor(&self) -> usize {
        self.factor
    }

    pub fn reset(&mut self) {
        for h in self.up.iter_mut().chain(self.down.iter_mut()) {
            h.reset();
        }
    }

    /// Fill `out[..factor]` with the interpolated samples of `x`.
    #[inline]
    pub fn upsample(&mut self, x: f32, out: &mut [f32; 4]) {
        match self.factor {
            1 => out[0] = x,
            2 => {
                let (a, b) = self.up[0].upsample(x);
                out[0] = a;
                out[1] = b;
            }
            _ => {
                let (a, b) = self.up[0].upsample(x);
                let (a0, a1) = self.up[1].upsample(a);
                let (b0, b1) = self.up[1].upsample(b);
                *out = [a0, a1, b0, b1];
            }
        }
    }

    /// Decimate `inp[..factor]` back to one sample.
    #[inline]
    pub fn downsample(&mut self, inp: &[f32; 4]) -> f32 {
        match self.factor {
            1 => inp[0],
            2 => self.down[0].downsample(inp[0], inp[1]),
            _ => {
                let a = self.down[1].downsample(inp[0], inp[1]);
                let b = self.down[1].downsample(inp[2], inp[3]);
                self.down[0].downsample(a, b)
            }
        }
    }

    /// Round-trip delay (up + down) in base-rate samples.
    pub fn latency(&self) -> f32 {
        match self.factor {
            1 => 0.0,
            // Two passes of the stage at 2×: 2 · gd / 2 base samples.
            2 => self.up[0].group_delay() as f32,
            _ => {
                let s1 = self.up[0].group_delay(); // at 2× rate, per direction
                let s2 = self.up[1].group_delay(); // at 4× rate, per direction
                ((2.0 * s1 / 2.0) + (2.0 * s2 / 4.0)) as f32
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use realfft::RealFftPlanner;

    fn spectrum_db(x: &[f32]) -> Vec<f32> {
        let n = x.len();
        let mut buf: Vec<f32> = x.iter().enumerate().map(|(i, v)| v * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())).collect();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let mut out = fft.make_output_vec();
        fft.process(&mut buf, &mut out).unwrap();
        out.iter().map(|c| 20.0 * (c.norm() + 1e-12).log10()).collect()
    }

    #[test]
    fn coefficients_are_valid_allpasses() {
        for c in halfband_coefs(10, 0.02) {
            assert!(c > 0.0 && c < 1.0, "{c}");
        }
    }

    #[test]
    fn upsampler_rejects_images() {
        let sr = 48_000.0f32;
        let n = 1 << 14;
        for factor in [2usize, 4] {
            let mut os = Oversampler::new(factor);
            let f = 15_000.0;
            let mut hi = Vec::with_capacity(n * factor);
            let mut tmp = [0.0; 4];
            for i in 0..n {
                os.upsample((std::f32::consts::TAU * f * i as f32 / sr).sin() * 0.5, &mut tmp);
                hi.extend_from_slice(&tmp[..factor]);
            }
            let hi = &hi[hi.len() - n..];
            let spec = spectrum_db(hi);
            let hr = sr * factor as f32;
            let bin = |hz: f32| (hz / hr * n as f32).round() as usize;
            let signal = spec[bin(f)];
            let image = spec[bin(sr - f) - 3..bin(sr - f) + 4].iter().cloned().fold(-300.0, f32::max);
            assert!(signal - image > 90.0, "{factor}x image rejection {}", signal - image);
        }
    }

    #[test]
    fn downsampler_rejects_aliases_and_round_trip_is_flat() {
        let sr = 48_000.0f32;
        for factor in [2usize, 4] {
            // A tone above the base Nyquist at the high rate must not fold.
            let hr = sr * factor as f32;
            let mut os = Oversampler::new(factor);
            let n = 1 << 13;
            let mut out = Vec::with_capacity(n);
            let f = 30_000.0; // folds to 18 kHz if not filtered
            for i in 0..n {
                let mut inp = [0.0; 4];
                for (j, v) in inp.iter_mut().enumerate().take(factor) {
                    // f64 phase: f32 phase noise alone sits near −55 dB.
                    let t = (i * factor + j) as f64 / hr as f64;
                    *v = (std::f64::consts::TAU * f * t).sin() as f32;
                }
                out.push(os.downsample(&inp));
            }
            let peak = out[n / 2..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(20.0 * peak.log10() < -85.0, "{factor}x alias {}", 20.0 * peak.log10());

            // Round trip of a 1 kHz tone keeps its level.
            let mut os = Oversampler::new(factor);
            let mut e_in = 0.0;
            let mut e_out = 0.0;
            let mut tmp = [0.0; 4];
            for i in 0..n {
                let x = (std::f32::consts::TAU * 1000.0 * i as f32 / sr).sin();
                os.upsample(x, &mut tmp);
                let y = os.downsample(&tmp);
                if i > 1000 {
                    e_in += x * x;
                    e_out += y * y;
                }
            }
            assert!((10.0 * (e_out / e_in).log10()).abs() < 0.01);
        }
    }

    #[test]
    fn latency_is_small() {
        let l2 = Oversampler::new(2).latency();
        let l4 = Oversampler::new(4).latency();
        assert!(l2 > 0.5 && l2 < 8.0, "{l2}");
        assert!(l4 > l2 && l4 < 10.0, "{l4}");
    }
}
