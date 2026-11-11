//! Topology-preserving-transform state-variable filter (ZDF / "Simper SVF").
//!
//! After A. Simper, *Linear Trapezoidal Integrated SVF* (Cytomic, 2013) and
//! V. Zavalishin, *The Art of VA Filter Design* rev. 2.1. One structure gives
//! every second-order response by mixing its three outputs, and it stays
//! stable and click-free under fast modulation (dynamic EQ, filter sweeps).
//!
//! Coefficients ([`SvfCoeffs`]) and state ([`SvfState`]) are separate so a
//! stereo pair or a bank of voices can share one set of coefficients.

use std::f64::consts::PI;

/// Response of an SVF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvfMode {
    Lowpass,
    Highpass,
    /// Band-pass normalised to 0 dB at the centre.
    Bandpass,
    Notch,
    /// Resonant peak (LP − HP), Simper's "peak".
    Peak,
    Allpass,
    /// Peaking EQ with gain in dB.
    Bell,
    LowShelf,
    HighShelf,
}

/// Mixing coefficients and integrator gains of one SVF.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SvfCoeffs {
    pub g: f32,
    pub k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    m0: f32,
    m1: f32,
    m2: f32,
}

impl Default for SvfCoeffs {
    fn default() -> Self {
        Self::passthrough()
    }
}

impl SvfCoeffs {
    /// Unity gain (m0 = 1).
    pub fn passthrough() -> Self {
        Self::from_gk(0.1, 1.0, 1.0, 0.0, 0.0)
    }

    fn from_gk(g: f64, k: f64, m0: f64, m1: f64, m2: f64) -> Self {
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        Self { g: g as f32, k: k as f32, a1: a1 as f32, a2: a2 as f32, a3: a3 as f32, m0: m0 as f32, m1: m1 as f32, m2: m2 as f32 }
    }

    /// Design a filter. `freq` is clamped below Nyquist, `q` to a sane range;
    /// `gain_db` is used by the bell and shelves only.
    pub fn new(mode: SvfMode, freq: f32, q: f32, gain_db: f32, sample_rate: f64) -> Self {
        let f = (freq as f64).clamp(1.0, sample_rate * 0.4999);
        let q = (q as f64).clamp(0.025, 200.0);
        let w = (PI * f / sample_rate).tan();
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let k = 1.0 / q;
        match mode {
            SvfMode::Lowpass => Self::from_gk(w, k, 0.0, 0.0, 1.0),
            SvfMode::Highpass => Self::from_gk(w, k, 1.0, -k, -1.0),
            SvfMode::Bandpass => Self::from_gk(w, k, 0.0, k, 0.0),
            SvfMode::Notch => Self::from_gk(w, k, 1.0, -k, 0.0),
            SvfMode::Peak => Self::from_gk(w, k, 1.0, -k, -2.0),
            SvfMode::Allpass => Self::from_gk(w, k, 1.0, -2.0 * k, 0.0),
            SvfMode::Bell => {
                let k = 1.0 / (q * a);
                Self::from_gk(w, k, 1.0, k * (a * a - 1.0), 0.0)
            }
            SvfMode::LowShelf => Self::from_gk(w / a.sqrt(), k, 1.0, k * (a - 1.0), a * a - 1.0),
            SvfMode::HighShelf => Self::from_gk(w * a.sqrt(), k, a * a, k * (1.0 - a) * a, 1.0 - a * a),
        }
    }

    /// Complex response at `freq` as `(re, im)` — exact for this structure
    /// (it is the bilinear transform of the analogue SVF).
    pub fn response(&self, freq: f64, sample_rate: f64) -> (f64, f64) {
        let w = (PI * freq / sample_rate).min(PI * 0.499_999).tan();
        // s = j w / g
        let s_im = w / self.g as f64;
        let k = self.k as f64;
        // D = s² + k s + 1 = (1 - s_im²) + j k s_im
        let (dr, di) = (1.0 - s_im * s_im, k * s_im);
        let den = dr * dr + di * di;
        // BP = s / D, LP = 1 / D
        let (bp_r, bp_i) = ((s_im * di) / den, (s_im * dr) / den);
        let (lp_r, lp_i) = (dr / den, -di / den);
        let (m0, m1, m2) = (self.m0 as f64, self.m1 as f64, self.m2 as f64);
        (m0 + m1 * bp_r + m2 * lp_r, m1 * bp_i + m2 * lp_i)
    }

    /// Magnitude in dB at `freq`.
    pub fn magnitude_db(&self, freq: f64, sample_rate: f64) -> f64 {
        let (r, i) = self.response(freq, sample_rate);
        10.0 * (r * r + i * i).max(1e-30).log10()
    }
}

/// Integrator state of one SVF channel.
#[derive(Clone, Copy, Debug, Default)]
pub struct SvfState {
    ic1: f32,
    ic2: f32,
}

impl SvfState {
    #[inline]
    pub fn process(&mut self, c: &SvfCoeffs, x: f32) -> f32 {
        let (_, bp, lp) = self.tick(c, x);
        c.m0 * x + c.m1 * bp + c.m2 * lp
    }

    /// One step returning `(hp, bp, lp)` with bp at unity-k gain (peak 1/k).
    #[inline]
    pub fn tick(&mut self, c: &SvfCoeffs, x: f32) -> (f32, f32, f32) {
        let v3 = x - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        // Denormal guard for silence after loud input.
        if self.ic1.abs() < 1e-20 {
            self.ic1 = 0.0;
        }
        if self.ic2.abs() < 1e-20 {
            self.ic2 = 0.0;
        }
        let hp = x - c.k * v1 - v2;
        (hp, v1, v2)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn is_finite(&self) -> bool {
        self.ic1.is_finite() && self.ic2.is_finite()
    }
}

/// Coefficients + state, for the common mono case.
#[derive(Clone, Copy, Debug, Default)]
pub struct Svf {
    pub coeffs: SvfCoeffs,
    pub state: SvfState,
}

impl Svf {
    pub fn new(mode: SvfMode, freq: f32, q: f32, gain_db: f32, sample_rate: f64) -> Self {
        Self { coeffs: SvfCoeffs::new(mode, freq, q, gain_db, sample_rate), state: SvfState::default() }
    }

    pub fn set(&mut self, mode: SvfMode, freq: f32, q: f32, gain_db: f32, sample_rate: f64) {
        self.coeffs = SvfCoeffs::new(mode, freq, q, gain_db, sample_rate);
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.state.process(&self.coeffs, x)
    }

    pub fn reset(&mut self) {
        self.state.reset();
    }
}

/// First-order TPT filter (one-pole low/high-pass, first-order all-pass).
#[derive(Clone, Copy, Debug, Default)]
pub struct OnePole {
    g: f32,
    s: f32,
}

impl OnePole {
    pub fn new(freq: f32, sample_rate: f64) -> Self {
        let mut p = Self::default();
        p.set_freq(freq, sample_rate);
        p
    }

    pub fn set_freq(&mut self, freq: f32, sample_rate: f64) {
        let f = (freq as f64).clamp(0.1, sample_rate * 0.4999);
        let w = (PI * f / sample_rate).tan();
        self.g = (w / (1.0 + w)) as f32;
    }

    /// Returns `(lp, hp)`.
    #[inline]
    pub fn tick(&mut self, x: f32) -> (f32, f32) {
        let v = (x - self.s) * self.g;
        let lp = v + self.s;
        self.s = lp + v;
        if self.s.abs() < 1e-20 {
            self.s = 0.0;
        }
        (lp, x - lp)
    }

    #[inline]
    pub fn lowpass(&mut self, x: f32) -> f32 {
        self.tick(x).0
    }

    #[inline]
    pub fn highpass(&mut self, x: f32) -> f32 {
        self.tick(x).1
    }

    #[inline]
    pub fn allpass(&mut self, x: f32) -> f32 {
        let (lp, hp) = self.tick(x);
        lp - hp
    }

    /// Response of the low-pass `(re, im)` at `freq`.
    pub fn lowpass_response(&self, freq: f64, sample_rate: f64) -> (f64, f64) {
        let g = self.g as f64;
        let wc = g / (1.0 - g); // tan(π fc / fs)
        let s = (PI * freq / sample_rate).min(PI * 0.499_999).tan() / wc;
        // 1 / (1 + j s)
        let den = 1.0 + s * s;
        (1.0 / den, -s / den)
    }

    pub fn reset(&mut self) {
        self.s = 0.0;
    }
}

/// DC blocker: first-order high-pass at ~5–20 Hz.
#[derive(Clone, Copy, Debug, Default)]
pub struct DcBlocker {
    hp: OnePole,
}

impl DcBlocker {
    pub fn new(freq: f32, sample_rate: f64) -> Self {
        Self { hp: OnePole::new(freq, sample_rate) }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.hp.highpass(x)
    }

    pub fn reset(&mut self) {
        self.hp.reset();
    }
}

/// Butterworth Q of section `i` of an order-`order` cascade (even part).
pub fn butterworth_q(order: usize, section: usize) -> f32 {
    // Pole angles θ = π (2k + 1) / (2n); Q = 1 / (2 cos θ) for k < n/2.
    let n = order as f64;
    let theta = PI * (2.0 * section as f64 + 1.0) / (2.0 * n);
    (1.0 / (2.0 * theta.cos())) as f32
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Measure the gain of a mono processor at `freq` with a steady sine.
    pub fn measure_gain_db(mut f: impl FnMut(f32) -> f32, freq: f64, sr: f64) -> f64 {
        let n_settle = (sr * 0.2) as usize;
        let n = (sr * 0.2) as usize;
        let w = 2.0 * PI * freq / sr;
        let mut peak_in = 0.0f64;
        let mut sum_in = 0.0f64;
        let mut sum_out = 0.0f64;
        for i in 0..(n_settle + n) {
            let x = (w * i as f64).sin();
            let y = f(x as f32) as f64;
            if i >= n_settle {
                sum_in += x * x;
                sum_out += y * y;
                peak_in = peak_in.max(x.abs());
            }
        }
        10.0 * (sum_out / sum_in).log10()
    }

    #[test]
    fn lowpass_response_matches_measurement() {
        let sr = 48_000.0;
        for &(mode, f0, q, g) in &[
            (SvfMode::Lowpass, 1000.0, 0.707, 0.0),
            (SvfMode::Highpass, 300.0, 2.0, 0.0),
            (SvfMode::Bell, 2000.0, 1.0, 6.0),
            (SvfMode::LowShelf, 200.0, 0.707, -9.0),
            (SvfMode::HighShelf, 8000.0, 0.707, 4.5),
            (SvfMode::Notch, 1000.0, 4.0, 0.0),
            (SvfMode::Bandpass, 1000.0, 2.0, 0.0),
        ] {
            let mut svf = Svf::new(mode, f0, q, g, sr);
            for &f in &[50.0, 200.0, 1000.0, 3000.0, 10_000.0] {
                svf.reset();
                let meas = measure_gain_db(|x| svf.process(x), f, sr);
                let theory = svf.coeffs.magnitude_db(f, sr);
                if theory > -60.0 {
                    assert!((meas - theory).abs() < 0.1, "{mode:?} {f} Hz: measured {meas} theory {theory}");
                }
            }
        }
    }

    #[test]
    fn bell_and_shelf_gains() {
        let sr = 48_000.0;
        let bell = SvfCoeffs::new(SvfMode::Bell, 1000.0, 1.0, 6.0, sr);
        assert!((bell.magnitude_db(1000.0, sr) - 6.0).abs() < 0.01);
        assert!(bell.magnitude_db(20.0, sr).abs() < 0.05);
        let ls = SvfCoeffs::new(SvfMode::LowShelf, 500.0, 0.707, -12.0, sr);
        assert!((ls.magnitude_db(10.0, sr) + 12.0).abs() < 0.05);
        assert!(ls.magnitude_db(20_000.0, sr).abs() < 0.05);
        assert!((ls.magnitude_db(500.0, sr) + 6.0).abs() < 0.1);
        let hs = SvfCoeffs::new(SvfMode::HighShelf, 4000.0, 0.707, 6.0, sr);
        assert!((hs.magnitude_db(23_000.0, sr) - 6.0).abs() < 0.1);
        assert!(hs.magnitude_db(20.0, sr).abs() < 0.05);
        let lp = SvfCoeffs::new(SvfMode::Lowpass, 1000.0, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr);
        assert!((lp.magnitude_db(1000.0, sr) + 3.01).abs() < 0.02);
        let ap = SvfCoeffs::new(SvfMode::Allpass, 1000.0, 0.7, 0.0, sr);
        for f in [30.0, 1000.0, 15_000.0] {
            assert!(ap.magnitude_db(f, sr).abs() < 1e-6);
        }
    }

    #[test]
    fn one_pole_cutoff() {
        let sr = 48_000.0;
        let mut p = OnePole::new(1000.0, sr);
        let db = measure_gain_db(|x| p.lowpass(x), 1000.0, sr);
        assert!((db + 3.01).abs() < 0.05, "{db}");
        let (r, i) = p.lowpass_response(1000.0, sr);
        assert!((10.0 * (r * r + i * i).log10() + 3.01).abs() < 0.01);
    }

    #[test]
    fn butterworth_qs() {
        assert!((butterworth_q(2, 0) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((butterworth_q(4, 0) - 0.5412).abs() < 1e-3);
        assert!((butterworth_q(4, 1) - 1.3066).abs() < 1e-3);
    }

    #[test]
    fn stable_under_fast_modulation() {
        let sr = 48_000.0;
        let mut st = SvfState::default();
        for i in 0..48_000 {
            let f = 20.0 + 19_000.0 * (((i as f32) * 0.37).sin() * 0.5 + 0.5);
            let c = SvfCoeffs::new(SvfMode::Lowpass, f, 20.0, 0.0, sr);
            let y = st.process(&c, if i % 100 == 0 { 1.0 } else { 0.0 });
            assert!(y.is_finite() && y.abs() < 1e3);
        }
    }
}
