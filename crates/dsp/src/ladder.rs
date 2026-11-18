//! Virtual-analog ladder filters with the non-linearity inside a zero-delay
//! feedback loop.
//!
//! Every pole is a trapezoidal (TPT) integrator, so the cutoff is exact and
//! stays well behaved under per-sample modulation. The feedback path is
//! solved instantly: the linear part of the loop is inverted in closed form
//! (the state-dependent term `z` and the instantaneous gain `w` of the
//! cascade), and the saturating non-linearity at the loop input is resolved
//! with a few Newton steps on a scalar equation. Everything runs at 2× with a
//! half-band interpolator and decimator, so the saturation does not alias.
//!
//! Based on V. Zavalishin, *The Art of VA Filter Design* (rev. 2.1: the
//! ladder, the diode ladder and the instantaneous-response method for
//! non-linear feedback), A. Huovilainen, *Non-linear digital implementation
//! of the Moog ladder filter* (DAFx-04), and S. D'Angelo & V. Välimäki,
//! *Generalized Moog ladder filter* (IEEE TASLP 2014).
//!
//! Types ([`LadderKind`]):
//! * `Transistor` — 4-pole, 24 dB/oct Moog-style. `tanh` at the loop input
//!   and in front of every pole, bass-loss compensation.
//! * `Ota` — 4-pole, 24 dB/oct Prophet/CEM3320-style: gentler saturation
//!   that only limits the feedback and the later poles, and the bass loss is
//!   left in.
//! * `Ota12` — 2-pole, 12 dB/oct state-variable filter (SEM-like) with a
//!   saturating integrator.
//! * `Diode` — TB-303 style diode ladder: four coupled stages (each node
//!   also loads its neighbour), output taken after the third (18 dB/oct), an
//!   asymmetric limiter and an optional high-pass in the feedback.
//!
//! The cutoff is the frequency at which the filter self-oscillates for every
//! type, so resonance stays tuned to the cutoff.

use crate::oversample::Halfband;
use std::f64::consts::PI;

/// Filter topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LadderKind {
    #[default]
    Transistor,
    Ota,
    Ota12,
    Diode,
}

impl LadderKind {
    /// Slope of the stop band, dB per octave.
    pub const fn slope_db(self) -> f32 {
        match self {
            LadderKind::Transistor | LadderKind::Ota => 24.0,
            LadderKind::Ota12 => 12.0,
            LadderKind::Diode => 18.0,
        }
    }

    /// Bass-loss compensation a fresh filter of this type starts with.
    pub const fn default_comp(self) -> f32 {
        match self {
            LadderKind::Transistor => 0.5,
            LadderKind::Ota | LadderKind::Ota12 => 0.0,
            LadderKind::Diode => 0.3,
        }
    }

    pub const ALL: [LadderKind; 4] = [LadderKind::Transistor, LadderKind::Ota, LadderKind::Ota12, LadderKind::Diode];
}

#[inline]
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}

/// Eigenvalues of the diode ladder's coupling matrix, in units of the
/// integrator rate (Dirichlet at the input end, free at the last node).
fn diode_lambdas() -> [f64; 4] {
    let mut l = [0.0; 4];
    for (m, v) in l.iter_mut().enumerate() {
        *v = 2.0 - 2.0 * ((2 * m + 1) as f64 * PI / 9.0).cos();
    }
    l
}

/// `(oscillation frequency / integrator rate, critical feedback gain)` of
/// the four-stage diode ladder.
fn diode_consts() -> (f32, f32) {
    let l = diode_lambdas();
    let (mut lo, mut hi) = (0.01f64, 100.0f64);
    for _ in 0..60 {
        let w = 0.5 * (lo + hi);
        let phase: f64 = l.iter().map(|&x| (w / x).atan()).sum();
        if phase < PI {
            lo = w;
        } else {
            hi = w;
        }
    }
    let w = 0.5 * (lo + hi);
    let k = l.iter().map(|&x| (w * w + x * x).sqrt()).product::<f64>();
    (w as f32, k as f32)
}

/// One resonant low-pass ladder with a 2× oversampled core.
#[derive(Clone, Copy, Debug)]
pub struct Ladder {
    kind: LadderKind,
    sr: f32,
    cutoff: f32,
    res: f32,
    drive: f32,
    comp: f32,
    fb_hp_hz: f32,
    // derived (per set_params)
    g: f32,
    gg: f32,
    k: f32,
    in_gain: f32,
    ghp: f32,
    // states
    s: [f32; 4],
    hp: f32,
    dc_x: f32,
    dc_y: f32,
    up: Halfband,
    down: Halfband,
    diode_ratio: f32,
    diode_k: f32,
}

impl Default for Ladder {
    fn default() -> Self {
        Self::new(LadderKind::Transistor)
    }
}

impl Ladder {
    /// Half-band pair: low latency (a couple of samples), > 70 dB rejection.
    fn halfband() -> Halfband {
        Halfband::new(8, 0.05)
    }

    pub fn new(kind: LadderKind) -> Self {
        let (ratio, kc) = diode_consts();
        let mut f = Self {
            kind,
            sr: 48_000.0,
            cutoff: 1000.0,
            res: 0.0,
            drive: 1.0,
            comp: Self::default_comp(kind),
            fb_hp_hz: 0.0,
            g: 0.1,
            gg: 0.1,
            k: 0.0,
            in_gain: 1.0,
            ghp: 0.0,
            s: [0.0; 4],
            hp: 0.0,
            dc_x: 0.0,
            dc_y: 0.0,
            up: Self::halfband(),
            down: Self::halfband(),
            diode_ratio: ratio,
            diode_k: kc,
        };
        f.set_params(1000.0, 0.0);
        f
    }

    fn default_comp(kind: LadderKind) -> f32 {
        kind.default_comp()
    }

    pub fn kind(&self) -> LadderKind {
        self.kind
    }

    /// Change the topology (state is cleared).
    pub fn set_kind(&mut self, kind: LadderKind) {
        if kind != self.kind {
            self.kind = kind;
            self.comp = Self::default_comp(kind);
            self.reset();
            self.set_params(self.cutoff, self.res);
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr.max(8_000.0);
        self.set_params(self.cutoff, self.res);
    }

    /// Input gain before the filter (the non-linearity sees `x * drive`).
    pub fn set_drive(&mut self, drive: f32) {
        self.drive = drive.max(0.0);
        self.set_params(self.cutoff, self.res);
    }

    /// Bass-loss compensation, 0 (none) .. 1 (full): input gain `1 + comp·k`.
    pub fn set_bass_comp(&mut self, comp: f32) {
        self.comp = comp.clamp(0.0, 1.0);
        self.set_params(self.cutoff, self.res);
    }

    /// One-pole high-pass in the feedback path (0 = off). It thins the
    /// resonance at low cutoffs, as the 303 does.
    pub fn set_feedback_highpass(&mut self, hz: f32) {
        self.fb_hp_hz = hz.max(0.0);
        self.set_params(self.cutoff, self.res);
    }

    /// Cutoff in Hz and resonance 0..1 (1 = self-oscillation). Cheap enough
    /// to call every sample.
    pub fn set_params(&mut self, cutoff_hz: f32, resonance: f32) {
        self.cutoff = cutoff_hz;
        let r = resonance.clamp(0.0, 1.0);
        self.res = r;
        let sr2 = self.sr * 2.0;
        let fc = cutoff_hz.clamp(10.0, self.sr * 0.45);
        let g = (std::f32::consts::PI * fc / sr2).tan();
        self.g = match self.kind {
            LadderKind::Diode => g / self.diode_ratio,
            _ => g,
        };
        self.gg = self.g;
        self.k = match self.kind {
            LadderKind::Transistor => 4.1 * r,
            LadderKind::Ota => 4.06 * r,
            LadderKind::Ota12 => 2.0 * (1.0 - r).powf(1.5) - 0.04 * r,
            LadderKind::Diode => 1.03 * self.diode_k * r,
        };
        self.in_gain = self.drive * (1.0 + self.comp * self.k.max(0.0));
        if self.kind == LadderKind::Ota12 {
            self.in_gain = self.drive;
        }
        self.ghp = if self.fb_hp_hz > 0.0 {
            let gh = (std::f32::consts::PI * self.fb_hp_hz.min(sr2 * 0.4) / sr2).tan();
            gh / (1.0 + gh)
        } else {
            0.0
        };
    }

    /// False once any state is NaN or infinite.
    pub fn is_finite(&self) -> bool {
        self.s.iter().all(|v| v.is_finite()) && self.hp.is_finite() && self.dc_y.is_finite()
    }

    pub fn reset(&mut self) {
        self.s = [0.0; 4];
        self.hp = 0.0;
        self.dc_x = 0.0;
        self.dc_y = 0.0;
        self.up.reset();
        self.down.reset();
    }

    /// One sample in, one sample out (2× inside).
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let (a, b) = self.up.upsample(x * self.in_gain);
        let ya = self.core(a);
        let yb = self.core(b);
        let y = self.down.downsample(ya, yb);
        if self.kind == LadderKind::Diode {
            // The asymmetric limiter creates a DC offset; the real circuit
            // has coupling capacitors. 6 Hz one-pole DC blocker.
            let r = 1.0 - 2.0 * std::f32::consts::PI * 6.0 / self.sr;
            let out = y - self.dc_x + r * self.dc_y;
            self.dc_x = y;
            self.dc_y = flush(out);
            return self.dc_y;
        }
        flush(y)
    }

    /// Loop-input saturation: `(T(u), T'(u))`.
    #[inline]
    fn sat(&self, u: f32) -> (f32, f32) {
        match self.kind {
            LadderKind::Transistor | LadderKind::Ota12 => {
                let t = u.tanh();
                (t, 1.0 - t * t)
            }
            LadderKind::Ota => {
                let t = (0.5 * u).tanh();
                (2.0 * t, 1.0 - t * t)
            }
            LadderKind::Diode => {
                const B: f32 = 0.15;
                let n = 1.0 - B.tanh().powi(2);
                let t = (u + B).tanh();
                ((t - B.tanh()) / n, (1.0 - t * t) / n)
            }
        }
    }

    #[inline]
    fn core(&mut self, x: f32) -> f32 {
        match self.kind {
            LadderKind::Transistor | LadderKind::Ota => self.core_ladder(x),
            LadderKind::Ota12 => self.core_ota12(x),
            LadderKind::Diode => self.core_diode(x),
        }
    }

    /// Solve `u + kk·(z4 + w4·T(u)) − c = x` for the loop input `u`.
    #[inline]
    fn solve_loop(&self, x: f32, z4: f32, w4: f32, kk: f32, c: f32) -> f32 {
        let mut u = (x + c - kk * z4) / (1.0 + kk * w4);
        for _ in 0..3 {
            let (t, dt) = self.sat(u);
            let f = u + kk * (z4 + w4 * t) - c - x;
            let d = 1.0 + kk * w4 * dt;
            u -= (f / d).clamp(-4.0, 4.0);
        }
        u
    }

    /// Feedback high-pass bookkeeping: returns `(kk, c)` for the loop.
    #[inline]
    fn fb_terms(&self) -> (f32, f32) {
        let a = 1.0 - self.ghp;
        (self.k * a, a * self.hp)
    }

    #[inline]
    fn fb_update(&mut self, y4: f32) {
        if self.ghp > 0.0 {
            let x = self.k * y4;
            let lp = self.ghp * x + (1.0 - self.ghp) * self.hp;
            self.hp = flush(2.0 * lp - self.hp);
        }
    }

    fn core_ladder(&mut self, x: f32) -> f32 {
        let big_g = self.g / (1.0 + self.g);
        let a = 1.0 - big_g;
        let g2 = big_g * big_g;
        let sigma = a * (g2 * big_g * self.s[0] + g2 * self.s[1] + big_g * self.s[2] + self.s[3]);
        let w4 = g2 * g2;
        let (kk, c) = self.fb_terms();
        let u = self.solve_loop(x, sigma, w4, kk, c);
        let (mut inp, _) = self.sat(u);
        let transistor = self.kind == LadderKind::Transistor;
        let mut y = 0.0;
        for i in 0..4 {
            // The OTA ladder only limits its later poles; the transistor
            // ladder saturates in front of every pole.
            let stage_in = if i == 0 {
                inp
            } else if transistor || i >= 2 {
                inp.tanh()
            } else {
                inp
            };
            y = big_g * stage_in + a * self.s[i];
            self.s[i] = flush(2.0 * y - self.s[i]);
            inp = y;
        }
        self.fb_update(y);
        y
    }

    fn core_ota12(&mut self, x: f32) -> f32 {
        // Resonance as damping `k`; the second integrator is fed through a
        // saturator (secant gain γ), which bounds the self-oscillation.
        // Past full resonance the damping goes slightly negative and grows
        // with the band-pass level (Van der Pol style), so the oscillation
        // settles at a fixed level instead of running away.
        let g1 = self.g;
        let mut gam = 1.0f32;
        let mut bp = self.s[0];
        #[allow(unused_assignments)]
        let mut hp = 0.0;
        let mut k = self.k;
        for _ in 0..3 {
            let g2 = g1 * gam;
            k = self.k + 0.3 * bp * bp;
            hp = (x - (k + g2) * self.s[0] - self.s[1]) / (1.0 + g1 * (k + g2));
            bp = self.s[0] + g1 * hp;
            gam = if bp.abs() > 1e-4 { bp.tanh() / bp } else { 1.0 };
        }
        let _ = k;
        let lp = self.s[1] + g1 * gam * bp;
        self.s[0] = flush(2.0 * bp - self.s[0]);
        self.s[1] = flush(2.0 * lp - self.s[1]);
        lp
    }

    fn core_diode(&mut self, x: f32) -> f32 {
        let g = self.g;
        // Tridiagonal factorisation of M = I + g·L (L: 2,2,2,1 diagonal, −1 off).
        let d = [1.0 + 2.0 * g, 1.0 + 2.0 * g, 1.0 + 2.0 * g, 1.0 + g];
        let mut cp = [0.0f32; 4];
        let mut inv = [0.0f32; 4];
        inv[0] = 1.0 / d[0];
        cp[0] = -g * inv[0];
        for i in 1..4 {
            let den = d[i] + g * cp[i - 1];
            inv[i] = 1.0 / den;
            cp[i] = -g * inv[i];
        }
        let solve = |rhs: [f32; 4]| -> [f32; 4] {
            let mut r = [0.0f32; 4];
            r[0] = rhs[0] * inv[0];
            for i in 1..4 {
                r[i] = (rhs[i] + g * r[i - 1]) * inv[i];
            }
            let mut y = r;
            for i in (0..3).rev() {
                y[i] = r[i] - cp[i] * y[i + 1];
            }
            y
        };
        let z = solve(self.s);
        let w = solve([g, 0.0, 0.0, 0.0]);
        let (kk, c) = self.fb_terms();
        let u = self.solve_loop(x, z[3], w[3], kk, c);
        let (t, _) = self.sat(u);
        let mut y = [0.0f32; 4];
        for i in 0..4 {
            y[i] = z[i] + w[i] * t;
            self.s[i] = flush(2.0 * y[i] - self.s[i]);
        }
        self.fb_update(y[3]);
        y[2]
    }
}

/// Small-signal magnitude response in dB of the analog prototype (no
/// saturation), for drawing curves. `hz` and `cutoff_hz` in Hz.
pub fn response_db(kind: LadderKind, hz: f32, cutoff_hz: f32, resonance: f32, comp: f32) -> f32 {
    let r = f64::from(resonance.clamp(0.0, 1.0));
    let w = f64::from(hz / cutoff_hz.max(1.0));
    type C = (f64, f64);
    let mul = |a: C, b: C| (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0);
    let div = |a: C, b: C| {
        let d = b.0 * b.0 + b.1 * b.1;
        ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
    };
    let mag = match kind {
        LadderKind::Transistor | LadderKind::Ota => {
            let (k, c) = if kind == LadderKind::Transistor { (4.1 * r, f64::from(comp)) } else { (4.06 * r, 0.0) };
            let p1 = div((1.0, 0.0), (1.0, w));
            let p4 = mul(mul(p1, p1), mul(p1, p1));
            let den = (1.0 + k * p4.0, k * p4.1);
            let h = div(p4, den);
            let m = (h.0 * h.0 + h.1 * h.1).sqrt();
            m * (1.0 + c * k)
        }
        LadderKind::Ota12 => {
            let k = 2.0 * (1.0 - r).powf(1.5) - 0.04 * r;
            let den = (1.0 - w * w, k * w);
            let h = div((1.0, 0.0), den);
            (h.0 * h.0 + h.1 * h.1).sqrt()
        }
        LadderKind::Diode => {
            let (ratio, kc) = diode_consts();
            let k = 1.03 * f64::from(kc) * r;
            let wn = w * f64::from(ratio);
            // Solve (jw·I + L) y = b·x with the output fed back (Thomas, complex).
            let diag = [(2.0, wn), (2.0, wn), (2.0, wn), (1.0, wn)];
            let mut cp = [(0.0, 0.0); 4];
            let mut inv = [(0.0, 0.0); 4];
            inv[0] = div((1.0, 0.0), diag[0]);
            cp[0] = mul((-1.0, 0.0), inv[0]);
            for i in 1..4 {
                let m = mul((1.0, 0.0), cp[i - 1]);
                let den = (diag[i].0 + m.0, diag[i].1 + m.1);
                inv[i] = div((1.0, 0.0), den);
                cp[i] = mul((-1.0, 0.0), inv[i]);
            }
            let solve = |rhs: [C; 4]| {
                let mut rr = [(0.0, 0.0); 4];
                rr[0] = mul(rhs[0], inv[0]);
                for i in 1..4 {
                    let t = (rhs[i].0 + rr[i - 1].0, rhs[i].1 + rr[i - 1].1);
                    rr[i] = mul(t, inv[i]);
                }
                let mut y = rr;
                for i in (0..3).rev() {
                    let t = mul(cp[i], y[i + 1]);
                    y[i] = (rr[i].0 - t.0, rr[i].1 - t.1);
                }
                y
            };
            let y = solve([(1.0, 0.0), (0.0, 0.0), (0.0, 0.0), (0.0, 0.0)]);
            // y3 / (1 + k·y4) with y = y_j per unit input.
            let den = (1.0 + k * y[3].0, k * y[3].1);
            let h = div(y[2], den);
            let m = (h.0 * h.0 + h.1 * h.1).sqrt();
            m * (1.0 + f64::from(comp) * k)
        }
    };
    (20.0 * mag.max(1e-9).log10()) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    }

    fn make(kind: LadderKind, sr: f32, fc: f32, res: f32) -> Ladder {
        let mut f = Ladder::new(kind);
        f.set_sample_rate(sr);
        f.set_params(fc, res);
        f
    }

    /// RMS gain (dB) of a sine at `hz` through a fresh filter.
    fn gain_db(kind: LadderKind, sr: f32, fc: f32, res: f32, hz: f32) -> f32 {
        let mut f = make(kind, sr, fc, res);
        let n = (sr * 0.5) as usize;
        let (mut acc, mut cnt) = (0.0f64, 0usize);
        for i in 0..n {
            let x = 0.05 * (2.0 * std::f32::consts::PI * hz * i as f32 / sr).sin();
            let y = f.process(x);
            if i > n / 2 {
                acc += f64::from(y * y);
                cnt += 1;
            }
        }
        let rms = (acc / cnt as f64).sqrt();
        (20.0 * (rms / (0.05 / std::f64::consts::SQRT_2)).log10()) as f32
    }

    /// Frequency of the self-oscillation kicked by noise, plus its amplitude
    /// drift between the two halves of the measurement.
    fn oscillation(kind: LadderKind, sr: f32, fc: f32) -> (f32, f32, f32) {
        let mut f = make(kind, sr, fc, 1.0);
        let mut seed = 1;
        for _ in 0..2000 {
            f.process(0.1 * lcg(&mut seed));
        }
        for _ in 0..(sr as usize) {
            f.process(0.0);
        }
        let n = (sr * 0.6) as usize;
        let mut ys = Vec::with_capacity(n);
        for _ in 0..n {
            ys.push(f.process(0.0));
        }
        let tail = &ys[n / 4..];
        let mut crossings = 0usize;
        let mut first = 0usize;
        let mut last = 0usize;
        for i in 1..tail.len() {
            if tail[i - 1] <= 0.0 && tail[i] > 0.0 {
                if crossings == 0 {
                    first = i;
                }
                last = i;
                crossings += 1;
            }
        }
        let freq = if crossings > 3 { (crossings - 1) as f32 * sr / (last - first) as f32 } else { 0.0 };
        let h = tail.len() / 2;
        let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
        let peak = tail.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        (freq, rms(&tail[h..]) / rms(&tail[..h]).max(1e-9), peak)
    }

    #[test]
    fn cutoff_tracks_across_range_and_rates() {
        for &sr in &[44_100.0, 48_000.0, 96_000.0] {
            for &fc in &[100.0, 440.0, 1_000.0, 3_000.0, 8_000.0] {
                for kind in LadderKind::ALL {
                    let (f, drift, peak) = oscillation(kind, sr, fc);
                    assert!((f / fc - 1.0).abs() < 0.04, "{kind:?} sr {sr} fc {fc}: oscillates at {f}");
                    assert!((0.85..1.15).contains(&drift), "{kind:?} sr {sr} fc {fc}: amplitude drift {drift}");
                    assert!(peak > 0.01 && peak < 6.0, "{kind:?} sr {sr} fc {fc}: peak {peak}");
                }
            }
        }
    }

    #[test]
    fn four_pole_gain_at_cutoff_is_minus_12_db() {
        for kind in [LadderKind::Transistor, LadderKind::Ota] {
            let g = gain_db(kind, 48_000.0, 1_000.0, 0.0, 1_000.0);
            assert!((g + 12.0).abs() < 1.0, "{kind:?}: {g}");
        }
        let g = gain_db(LadderKind::Ota12, 48_000.0, 1_000.0, 0.0, 1_000.0);
        assert!((g + 6.0).abs() < 1.5, "{g}");
    }

    #[test]
    fn slopes_match_the_topology() {
        for kind in LadderKind::ALL {
            let a = gain_db(kind, 48_000.0, 800.0, 0.0, 4_000.0);
            let b = gain_db(kind, 48_000.0, 800.0, 0.0, 8_000.0);
            let slope = a - b;
            let want = kind.slope_db();
            assert!((slope - want).abs() < 3.0, "{kind:?}: {slope} dB/oct, want {want}");
        }
    }

    #[test]
    fn analytic_response_agrees_with_the_filter() {
        for kind in LadderKind::ALL {
            for &hz in &[200.0, 1_000.0, 3_000.0] {
                let m = gain_db(kind, 96_000.0, 1_000.0, 0.0, hz);
                let a = response_db(kind, hz, 1_000.0, 0.0, 0.0);
                assert!((m - a).abs() < 1.5, "{kind:?} {hz} Hz: measured {m}, analytic {a}");
            }
        }
    }

    #[test]
    fn stable_under_per_sample_modulation() {
        for kind in LadderKind::ALL {
            for &res in &[0.5, 1.0] {
                let mut f = make(kind, 48_000.0, 1_000.0, res);
                f.set_drive(4.0);
                let mut seed = 7;
                let mut peak = 0.0f32;
                for i in 0..96_000 {
                    // Fast random-walk-ish sweep over almost the whole range.
                    let ph = i as f32 * 0.003;
                    let fc = 40.0 * 2f32.powf(8.0 * (0.5 + 0.5 * ph.sin())) + 100.0 * lcg(&mut seed).abs();
                    f.set_params(fc, res);
                    let y = f.process(0.5 * lcg(&mut seed));
                    assert!(y.is_finite(), "{kind:?} res {res} at {i}");
                    peak = peak.max(y.abs());
                }
                assert!(peak < 12.0, "{kind:?} res {res}: peak {peak}");
                // And it settles once the input stops.
                f.set_params(1_000.0, 0.3);
                let mut last = 1.0;
                for _ in 0..48_000 {
                    last = f.process(0.0);
                }
                assert!(last.abs() < 1e-3, "{kind:?}: still ringing {last}");
            }
        }
    }

    #[test]
    fn feedback_highpass_keeps_the_bass() {
        let plain = gain_db(LadderKind::Diode, 48_000.0, 1_000.0, 0.8, 30.0);
        let mut f = Ladder::new(LadderKind::Diode);
        f.set_feedback_highpass(150.0);
        f.set_params(1_000.0, 0.8);
        let n = 24_000;
        let mut acc = 0.0f64;
        for i in 0..n {
            let y = f.process(0.05 * (2.0 * std::f32::consts::PI * 30.0 * i as f32 / 48_000.0).sin());
            if i > n / 2 {
                acc += f64::from(y * y);
            }
        }
        let hp = (20.0 * ((acc / (n / 2) as f64).sqrt() / (0.05 / std::f64::consts::SQRT_2)).log10()) as f32;
        assert!(hp > plain + 6.0, "bass with feedback HP {hp} dB vs {plain} dB");
    }

    #[test]
    fn processing_is_allocation_free_by_construction() {
        // `Ladder` is `Copy`: it owns no heap data, so `process` cannot allocate.
        fn assert_copy<T: Copy>() {}
        assert_copy::<Ladder>();
    }
}
