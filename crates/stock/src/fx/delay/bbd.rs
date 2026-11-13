//! Bucket-brigade delay after M. Holters & J. D. Parker, *A Combined Model
//! for a Bucket Brigade Device and its Input and Output Filters* (DAFx-18).
//!
//! A BBD is a chain of `STAGES` capacitors clocked at a rate set by the
//! delay time: `delay = STAGES / f_clock`. It is a sampled system running at
//! its own, varying clock, so long delays alias and darken. The analogue
//! anti-aliasing and reconstruction filters around it are what make the
//! sound; here both are 5th-order Butterworth low-passes written as sums of
//! first-order complex sections (partial fractions), which can be
//! evaluated at any instant:
//!
//! * **input**: the audio input is held for each sample (zero-order hold),
//!   so each section's state moves exactly as
//!   `s(τ) = e^{pτT} s + (e^{pτT} − 1)/p · u` across the sample; a clock
//!   tick a fraction `τ` into the sample loads `Re Σ r s(τ)` into a bucket.
//! * **output**: the BBD output is held between ticks. A held signal is the
//!   integral of its jumps `Δ`, so its filtered value is
//!   `H(0)·w(t) + Re Σ (r/p) s̃(t)` with `s̃` jumping by `Δ` at each tick
//!   and decaying as `e^{pt}` (the step response of each section).
//!
//! Conjugate poles share one state (weight 2), so a 5th-order filter costs
//! three complex states. `e^{pτT}` comes from a table over `τ` filled when
//! the cutoff changes. At most [`MAX_TICKS`] ticks run per audio sample,
//! which bounds the shortest delay to `STAGES / MAX_TICKS` samples.

use std::f64::consts::PI;

/// Buckets in the chain (an MN3005-sized device).
pub const STAGES: usize = 4096;
/// Most clock ticks per audio sample (sets the shortest delay).
pub const MAX_TICKS: usize = 8;
/// Shortest delay in samples.
pub const MIN_DELAY: f32 = (STAGES / MAX_TICKS) as f32 + 1.0;
const ORDER: usize = 5;
/// Unique poles: one real and two of each conjugate pair.
const SECTIONS: usize = 3;
const TABLE: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct C {
    re: f32,
    im: f32,
}

impl C {
    #[inline]
    fn mul(self, o: C) -> C {
        C { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
    #[inline]
    fn scale(self, k: f32) -> C {
        C { re: self.re * k, im: self.im * k }
    }
    #[inline]
    fn add(self, o: C) -> C {
        C { re: self.re + o.re, im: self.im + o.im }
    }
    #[inline]
    fn lerp(self, o: C, t: f32) -> C {
        C { re: self.re + (o.re - self.re) * t, im: self.im + (o.im - self.im) * t }
    }
}

/// `(re, im)` in f64 for the design maths.
type Z = (f64, f64);
fn zmul(a: Z, b: Z) -> Z {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}
fn zdiv(a: Z, b: Z) -> Z {
    let d = b.0 * b.0 + b.1 * b.1;
    ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
}
fn zexp(a: Z) -> Z {
    let m = a.0.exp();
    (m * a.1.cos(), m * a.1.sin())
}

/// One partial-fraction section: `(pole, residue, weight)` (weight 2 for a
/// conjugate pair).
pub type Section = ((f64, f64), (f64, f64), f64);

/// Poles and residues of an analogue Butterworth low-pass at `fc` Hz:
/// `H(s) = Σ r_k / (s − p_k)`, unique sections only (real pole first).
pub fn butterworth(fc: f64) -> [Section; SECTIONS] {
    let wc = 2.0 * PI * fc;
    let poles: [Z; ORDER] = std::array::from_fn(|k| {
        let th = PI / 2.0 + PI * (2 * k + 1) as f64 / (2 * ORDER) as f64;
        (wc * th.cos(), wc * th.sin())
    });
    let residue = |k: usize| {
        let mut den = (1.0, 0.0);
        for (j, pj) in poles.iter().enumerate() {
            if j != k {
                den = zmul(den, (poles[k].0 - pj.0, poles[k].1 - pj.1));
            }
        }
        zdiv((wc.powi(ORDER as i32), 0.0), den)
    };
    // k = 2 is the real pole (θ = π); 0 and 1 have positive imaginary parts.
    [(poles[2], residue(2), 1.0), (poles[0], residue(0), 2.0), (poles[1], residue(1), 2.0)]
}

/// One filter: the per-sample decay, the τ table and the output weights.
#[derive(Clone, Debug)]
struct Bank {
    /// `e^{pT}`.
    step: [C; SECTIONS],
    /// `e^{pτT}` for `τ = i / TABLE`.
    table: [[C; TABLE + 1]; SECTIONS],
    /// Input: `w·r`. Output: `w·r/p`.
    gain: [C; SECTIONS],
    /// Input only: `w·r/p` (the held input's term).
    held: [C; SECTIONS],
    /// Input only: `(e^{pT} − 1)/p`.
    hold_step: [C; SECTIONS],
}

impl Bank {
    fn new() -> Self {
        let z = [C::default(); SECTIONS];
        Self { step: z, table: [[C::default(); TABLE + 1]; SECTIONS], gain: z, held: z, hold_step: z }
    }

    fn design(&mut self, fc: f64, sr: f64, input: bool) {
        let t = 1.0 / sr;
        let secs = butterworth(fc.min(sr * 0.45));
        let c = |z: Z| C { re: z.0 as f32, im: z.1 as f32 };
        for (i, (p, r, w)) in secs.iter().enumerate() {
            let e = zexp((p.0 * t, p.1 * t));
            self.step[i] = c(e);
            for (k, slot) in self.table[i].iter_mut().enumerate() {
                let tau = k as f64 / TABLE as f64;
                *slot = c(zexp((p.0 * t * tau, p.1 * t * tau)));
            }
            let rp = zdiv(*r, *p);
            self.gain[i] = if input { c((r.0 * w, r.1 * w)) } else { c((rp.0 * w, rp.1 * w)) };
            self.held[i] = c((rp.0 * w, rp.1 * w));
            self.hold_step[i] = c(zdiv((e.0 - 1.0, e.1), *p));
        }
    }

    #[inline]
    fn at(&self, i: usize, tau: f32) -> C {
        let x = tau.clamp(0.0, 1.0) * TABLE as f32;
        let k = (x as usize).min(TABLE - 1);
        self.table[i][k].lerp(self.table[i][k + 1], x - k as f32)
    }
}

/// One channel of BBD with its filters.
#[derive(Clone, Debug)]
pub struct Bbd {
    input: Bank,
    output: Bank,
    s_in: [C; SECTIONS],
    s_out: [C; SECTIONS],
    /// The input held over the current sample.
    u: f32,
    buckets: Vec<f32>,
    pos: usize,
    held: f32,
    phase: f32,
    fc: f32,
    sr: f64,
}

impl Bbd {
    /// Allocates: call from `prepare`.
    pub fn new(sample_rate: f64) -> Self {
        let mut b = Self {
            input: Bank::new(),
            output: Bank::new(),
            s_in: [C::default(); SECTIONS],
            s_out: [C::default(); SECTIONS],
            u: 0.0,
            buckets: vec![0.0; STAGES],
            pos: 0,
            held: 0.0,
            phase: 0.0,
            fc: 0.0,
            sr: sample_rate,
        };
        b.set_cutoff(7000.0);
        b
    }

    /// Cutoff of both analogue filters. Redesigns the tables (~ a few µs):
    /// call on parameter changes, not per sample.
    pub fn set_cutoff(&mut self, fc: f32) {
        if fc != self.fc {
            self.fc = fc;
            self.input.design(fc as f64, self.sr, true);
            self.output.design(fc as f64, self.sr, false);
        }
    }

    pub fn clear(&mut self) {
        self.s_in = [C::default(); SECTIONS];
        self.s_out = [C::default(); SECTIONS];
        self.buckets.fill(0.0);
        self.held = 0.0;
        self.u = 0.0;
        self.phase = 0.0;
    }

    /// Advance one audio sample with the clock set for `delay` samples and
    /// return the filtered output. Call [`Bbd::push`] with this sample's
    /// input afterwards.
    #[inline]
    pub fn read(&mut self, delay: f32) -> f32 {
        let rate = STAGES as f32 / delay.max(MIN_DELAY);
        let mut out = [C::default(); SECTIONS];
        for (o, (s, e)) in out.iter_mut().zip(self.s_out.iter().zip(&self.output.step)) {
            *o = s.mul(*e);
        }
        // Ticks fall at τ = (1 − phase) / rate, then every 1 / rate.
        let step = 1.0 / rate;
        let mut tau = (1.0 - self.phase) * step;
        let mut n = 0;
        while tau < 1.0 && n < MAX_TICKS {
            // Sample the input filter into the newest bucket; the oldest
            // bucket's charge moves to the output.
            let mut v = 0.0;
            for i in 0..SECTIONS {
                let e = self.input.at(i, tau);
                v += self.input.gain[i].mul(e.mul(self.s_in[i])).re + self.input.held[i].mul(C { re: e.re - 1.0, im: e.im }).re * self.u;
            }
            let w = self.buckets[self.pos];
            self.buckets[self.pos] = saturate(v);
            self.pos = if self.pos + 1 == STAGES { 0 } else { self.pos + 1 };
            let jump = w - self.held;
            self.held = w;
            for (i, o) in out.iter_mut().enumerate() {
                *o = o.add(self.output.at(i, 1.0 - tau).scale(jump));
            }
            tau += step;
            n += 1;
        }
        let ph = self.phase + rate;
        self.phase = ph - ph.floor();
        let mut y = self.held;
        for (i, o) in out.iter().enumerate() {
            self.s_out[i] = flush(*o);
            y += self.output.gain[i].mul(*o).re;
        }
        y
    }

    /// Feed this sample's input to the input filter (it is held over the
    /// next sample).
    #[inline]
    pub fn push(&mut self, x: f32) {
        let u = self.u;
        for i in 0..SECTIONS {
            self.s_in[i] = flush(self.s_in[i].mul(self.input.step[i]).add(self.input.hold_step[i].scale(u)));
        }
        self.u = x;
    }
}

/// Bucket charge headroom: the chain saturates softly towards ±HEADROOM.
const HEADROOM: f32 = 2.5;

/// Bucket charge saturation (rational tanh): within 0.1 dB of linear up to
/// −6 dBFS, soft above.
#[inline]
fn saturate(x: f32) -> f32 {
    let x = (x / HEADROOM).clamp(-3.0, 3.0);
    HEADROOM * x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

#[inline]
fn flush(c: C) -> C {
    if c.re.abs() < 1e-20 && c.im.abs() < 1e-20 { C::default() } else { c }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(b: &mut Bbd, delay: f32, x: impl Fn(usize) -> f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let y = b.read(delay);
                b.push(x(i));
                y
            })
            .collect()
    }

    #[test]
    fn butterworth_has_unity_dc_and_minus_3_db_at_the_cutoff() {
        let fc = 5_000.0;
        let secs = butterworth(fc);
        let h = |f: f64| {
            let s = (0.0, 2.0 * PI * f);
            let term = |p: Z, r: Z| zdiv(r, (s.0 - p.0, s.1 - p.1));
            let mut sum = (0.0, 0.0);
            for (p, r, w) in secs {
                let mut add = |t: Z| {
                    sum.0 += t.0;
                    sum.1 += t.1;
                };
                add(term(p, r));
                if w > 1.5 {
                    add(term((p.0, -p.1), (r.0, -r.1)));
                }
            }
            (sum.0 * sum.0 + sum.1 * sum.1).sqrt()
        };
        assert!((h(0.0) - 1.0).abs() < 1e-9, "{}", h(0.0));
        assert!((h(fc) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6, "{}", h(fc));
        assert!(h(4.0 * fc) < 0.002, "{}", h(4.0 * fc));
    }

    #[test]
    fn a_step_comes_out_after_the_delay_at_unity() {
        let mut b = Bbd::new(48_000.0);
        let y = run(&mut b, 4_800.0, |i| if i >= 100 { 0.5 } else { 0.0 }, 12_000);
        assert!(y[4_800].abs() < 0.05, "nothing before the delay: {}", y[4_800]);
        assert!((y[11_000] - 0.5).abs() < 0.01 * 0.5 + 0.008, "unity DC gain: {}", y[11_000]);
        let half = y.iter().position(|v| *v > 0.25).unwrap();
        assert!((half as i32 - 4_900).abs() < 12, "edge at {half}");
    }

    #[test]
    fn highs_are_filtered_and_long_delays_alias_down() {
        let sr = 48_000.0;
        let level = |freq: f32, delay: f32| {
            let mut b = Bbd::new(sr as f64);
            let y = run(&mut b, delay, |i| (std::f32::consts::TAU * freq * i as f32 / sr).sin() * 0.5, 30_000);
            let tail = &y[20_000..];
            (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt()
        };
        let low = level(500.0, 2_400.0);
        let high = level(15_000.0, 2_400.0);
        assert!((low - 0.354).abs() < 0.03, "500 Hz passes: {low}");
        assert!(high < 0.02, "15 kHz is filtered: {high}");
        // Slow clock (1 s on 4096 buckets): a 3 kHz tone is above the
        // clock's Nyquist and the output is mostly alias, not the tone.
        let mut b = Bbd::new(sr as f64);
        let y = run(&mut b, 48_000.0, |i| (std::f32::consts::TAU * 3_000.0 * i as f32 / sr).sin() * 0.5, 90_000);
        assert!(y.iter().all(|v| v.is_finite() && v.abs() < 2.0));
    }

    #[test]
    fn short_delays_clamp_to_the_tick_budget() {
        let mut b = Bbd::new(48_000.0);
        let y = run(&mut b, 10.0, |i| if i == 0 { 1.0 } else { 0.0 }, 2_000);
        let at = y.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
        assert!((at as f32 - MIN_DELAY).abs() < 20.0, "peak at {at}");
    }
}
