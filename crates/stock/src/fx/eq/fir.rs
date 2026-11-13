//! Stereo FIR engine of the Natural and Linear phase modes: a uniformly
//! partitioned overlap-save convolver (UPOLS) with a full 2×2 filter
//! matrix, so bands on mid, side, left or right combine exactly.
//!
//! * The filter is designed by frequency sampling: the caller fills one
//!   spectrum per matrix element (`LL`, `LR`, `RL`, `RR`) on an `n`-point
//!   grid, already carrying the bulk delay `D`; [`FirEngine::design`] turns
//!   each into `n` taps (inverse FFT, window), splits them into `n / b`
//!   partitions of `b` taps and stores each partition's `2b`-point spectrum.
//! * Audio runs in blocks of `b` frames: each block's `2b` window is FFT'd
//!   once per channel into a frequency-domain delay line, multiplied with
//!   the partition spectra and summed, then one inverse FFT per output
//!   channel gives `b` new outputs. Latency = `b` (block) + `D` (filter).
//! * A new design goes into the second filter set and is crossfaded in
//!   linearly over one block, so moving a band never clicks.
//!
//! Everything is allocated in [`FirEngine::new`]; `design` and `tick` only
//! use preallocated buffers (realfft's `*_with_scratch` calls).

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Matrix elements: output channel `o`, input channel `i` → `o * 2 + i`.
pub const ELEMS: usize = 4;

/// Shape of the time window applied to the designed impulse response.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Window {
    /// Symmetric around the centre (linear phase): flat middle, raised-cosine
    /// tapers over the outer `taper` fraction on each side.
    Symmetric { taper: f32 },
    /// Causal with a short pre-ring region (natural phase): half-cosine fade
    /// in over the first `pre` taps, flat, half-cosine fade out over the last
    /// `tail` fraction.
    Causal { pre: usize, tail: f32 },
}

pub struct FirEngine {
    n: usize,
    b: usize,
    parts: usize,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    design_inv: Arc<dyn ComplexToReal<f32>>,
    /// Design input: one `n / 2 + 1` spectrum per element.
    pub spec: [Vec<Complex<f32>>; ELEMS],
    /// Which elements carry anything (cross terms are often zero).
    pub used: [bool; ELEMS],
    ir: Vec<f32>,
    design_scratch: Vec<Complex<f32>>,
    /// Two filter sets × elements × `parts · (b + 1)` bins.
    h: [[Vec<Complex<f32>>; ELEMS]; 2],
    h_used: [[bool; ELEMS]; 2],
    cur: usize,
    fading: bool,
    /// Input history per channel: `2b` samples (previous block, current).
    inp: [Vec<f32>; 2],
    /// Frequency-domain delay line per channel: `parts` spectra of `b + 1`.
    fdl: [Vec<Complex<f32>>; 2],
    fdl_pos: usize,
    out: [Vec<f32>; 2],
    pos: usize,
    acc: Vec<Complex<f32>>,
    time: Vec<f32>,
    time2: Vec<f32>,
    fwd_scratch: Vec<Complex<f32>>,
    inv_scratch: Vec<Complex<f32>>,
}

impl FirEngine {
    /// `n` taps (power of two), partitions of `b` frames (power of two, ≤ n).
    pub fn new(n: usize, b: usize) -> Self {
        let n = n.max(64).next_power_of_two();
        let b = b.clamp(16, n).next_power_of_two();
        let parts = n / b;
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(2 * b);
        let inv = planner.plan_fft_inverse(2 * b);
        let design_inv = planner.plan_fft_inverse(n);
        let bins = b + 1;
        let zeros = |len: usize| vec![Complex::new(0.0f32, 0.0); len];
        let fwd_scratch = fwd.make_scratch_vec();
        let mut inv_scratch = inv.make_scratch_vec();
        let ds = design_inv.make_scratch_vec();
        if inv_scratch.len() < ds.len() {
            inv_scratch.resize(ds.len(), Complex::new(0.0, 0.0));
        }
        let mut e = Self {
            n,
            b,
            parts,
            spec: std::array::from_fn(|_| zeros(n / 2 + 1)),
            used: [true, false, false, true],
            ir: vec![0.0; n],
            design_scratch: ds,
            h: std::array::from_fn(|_| std::array::from_fn(|_| zeros(parts * bins))),
            h_used: [[false; ELEMS]; 2],
            cur: 0,
            fading: false,
            inp: [vec![0.0; 2 * b], vec![0.0; 2 * b]],
            fdl: [zeros(parts * bins), zeros(parts * bins)],
            fdl_pos: 0,
            out: [vec![0.0; b], vec![0.0; b]],
            pos: 0,
            acc: zeros(bins),
            time: vec![0.0; 2 * b],
            time2: vec![0.0; 2 * b],
            fwd_scratch,
            inv_scratch,
            fwd,
            inv,
            design_inv,
        };
        e.set_identity(0);
        e
    }

    pub fn taps(&self) -> usize {
        self.n
    }

    pub fn block(&self) -> usize {
        self.b
    }

    /// Whether a new design is still fading in.
    pub fn fading(&self) -> bool {
        self.fading
    }

    /// Clear the audio history (filters stay).
    pub fn reset(&mut self) {
        for c in 0..2 {
            self.inp[c].fill(0.0);
            self.out[c].fill(0.0);
            self.fdl[c].fill(Complex::new(0.0, 0.0));
        }
        self.pos = 0;
        self.fdl_pos = 0;
        if self.fading {
            self.cur ^= 1;
            self.fading = false;
        }
    }

    /// A unit impulse at tap 0 in set `set` (pass-through after `b` frames).
    fn set_identity(&mut self, set: usize) {
        let bins = self.b + 1;
        let g = 1.0 / (2 * self.b) as f32;
        for e in 0..ELEMS {
            self.h[set][e].fill(Complex::new(0.0, 0.0));
        }
        for k in 0..bins {
            self.h[set][0][k] = Complex::new(g, 0.0);
            self.h[set][3][k] = Complex::new(g, 0.0);
        }
        self.h_used[set] = [true, false, false, true];
    }

    /// Turn `spec` (with `used`) into a filter. `immediate` replaces the
    /// running filter at once (after a reset or a mode change); otherwise
    /// the new filter crossfades in over the next block. Returns false (and
    /// does nothing) while a previous crossfade is still running.
    pub fn design(&mut self, window: Window, immediate: bool) -> bool {
        if self.fading && !immediate {
            return false;
        }
        let set = if immediate { self.cur } else { self.cur ^ 1 };
        let (n, b) = (self.n, self.b);
        let bins = b + 1;
        let scale = 1.0 / (n as f32 * 2.0 * b as f32);
        for e in 0..ELEMS {
            if !self.used[e] {
                self.h_used[set][e] = false;
                continue;
            }
            let spec = &mut self.spec[e];
            spec[0].im = 0.0;
            spec[n / 2].im = 0.0;
            if self.design_inv.process_with_scratch(spec, &mut self.ir, &mut self.design_scratch).is_err() {
                self.h_used[set][e] = false;
                continue;
            }
            apply_window(&mut self.ir, window);
            for p in 0..self.parts {
                self.time[..b].copy_from_slice(&self.ir[p * b..(p + 1) * b]);
                self.time[b..].fill(0.0);
                for t in &mut self.time[..b] {
                    *t *= scale;
                }
                let dst = &mut self.h[set][e][p * bins..(p + 1) * bins];
                if self.fwd.process_with_scratch(&mut self.time, dst, &mut self.fwd_scratch).is_err() {
                    dst.fill(Complex::new(0.0, 0.0));
                }
            }
            self.h_used[set][e] = true;
        }
        self.fading = !immediate;
        true
    }

    /// One stereo frame through the convolver (output delayed by `b`).
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        let b = self.b;
        let i = self.pos;
        self.inp[0][b + i] = l;
        self.inp[1][b + i] = r;
        let y = (self.out[0][i], self.out[1][i]);
        self.pos += 1;
        if self.pos == b {
            self.pos = 0;
            self.run_block();
        }
        y
    }

    fn run_block(&mut self) {
        let b = self.b;
        let bins = b + 1;
        // New input spectra into the delay line.
        for c in 0..2 {
            self.time.copy_from_slice(&self.inp[c]);
            let slot = &mut self.fdl[c][self.fdl_pos * bins..(self.fdl_pos + 1) * bins];
            if self.fwd.process_with_scratch(&mut self.time, slot, &mut self.fwd_scratch).is_err() {
                slot.fill(Complex::new(0.0, 0.0));
            }
            self.inp[c].copy_within(b.., 0);
        }
        let fading = self.fading;
        let (old, new) = (self.cur, self.cur ^ 1);
        for o in 0..2 {
            self.accumulate(old, o);
            spectrum_to_time(&self.inv, &mut self.acc, &mut self.time, &mut self.inv_scratch);
            if fading {
                self.accumulate(new, o);
                spectrum_to_time(&self.inv, &mut self.acc, &mut self.time2, &mut self.inv_scratch);
                let step = 1.0 / b as f32;
                for k in 0..b {
                    let t = (k + 1) as f32 * step;
                    let a = self.time[b + k];
                    self.out[o][k] = a + (self.time2[b + k] - a) * t;
                }
            } else {
                self.out[o].copy_from_slice(&self.time[b..]);
            }
        }
        if fading {
            self.cur = new;
            self.fading = false;
        }
        self.fdl_pos = (self.fdl_pos + 1) % self.parts;
    }

    /// `acc = Σ_k Σ_i X_i[now − k] · H[set][o·2+i][k]`.
    fn accumulate(&mut self, set: usize, o: usize) {
        let bins = self.b + 1;
        self.acc.fill(Complex::new(0.0, 0.0));
        for i in 0..2 {
            let e = o * 2 + i;
            if !self.h_used[set][e] {
                continue;
            }
            let h = &self.h[set][e];
            let x = &self.fdl[i];
            for p in 0..self.parts {
                let slot = (self.fdl_pos + self.parts - p) % self.parts;
                let xs = &x[slot * bins..(slot + 1) * bins];
                let hs = &h[p * bins..(p + 1) * bins];
                for ((a, xv), hv) in self.acc.iter_mut().zip(xs).zip(hs) {
                    *a += xv * hv;
                }
            }
        }
    }
}

fn spectrum_to_time(inv: &Arc<dyn ComplexToReal<f32>>, acc: &mut [Complex<f32>], out: &mut [f32], scratch: &mut [Complex<f32>]) {
    let last = acc.len() - 1;
    acc[0].im = 0.0;
    acc[last].im = 0.0;
    if inv.process_with_scratch(acc, out, &mut scratch[..inv.get_scratch_len()]).is_err() {
        out.fill(0.0);
    }
}

fn apply_window(ir: &mut [f32], window: Window) {
    let n = ir.len();
    let half_cos = |t: f32| 0.5 - 0.5 * (std::f32::consts::PI * t.clamp(0.0, 1.0)).cos();
    match window {
        Window::Symmetric { taper } => {
            let w = ((n as f32 * taper * 0.5) as usize).max(1);
            for k in 0..w {
                let g = half_cos(k as f32 / w as f32);
                ir[k] *= g;
                ir[n - 1 - k] *= g;
            }
        }
        Window::Causal { pre, tail } => {
            let pre = pre.min(n / 2);
            for (k, v) in ir.iter_mut().enumerate().take(pre) {
                *v *= half_cos(k as f32 / pre.max(1) as f32);
            }
            let w = ((n as f32 * tail) as usize).max(1);
            for k in 0..w {
                ir[n - 1 - k] *= half_cos(k as f32 / w as f32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fill `e`'s spectrum with a pure delay of `d` taps times `g`.
    fn delay_spec(f: &mut FirEngine, e: usize, d: f64, g: f32) {
        let n = f.taps();
        for k in 0..=n / 2 {
            let ph = -std::f64::consts::TAU * k as f64 * d / n as f64;
            f.spec[e][k] = Complex::new(g * ph.cos() as f32, g * ph.sin() as f32);
        }
    }

    fn run(f: &mut FirEngine, input: impl Fn(usize) -> (f32, f32), len: usize) -> Vec<(f32, f32)> {
        (0..len).map(|i| {
            let (l, r) = input(i);
            f.tick(l, r)
        }).collect()
    }

    #[test]
    fn identity_passes_audio_after_one_block() {
        let mut f = FirEngine::new(1024, 64);
        let out = run(&mut f, |i| if i == 10 { (1.0, -0.5) } else { (0.0, 0.0) }, 400);
        for (i, (l, r)) in out.iter().enumerate() {
            let (wl, wr) = if i == 74 { (1.0, -0.5) } else { (0.0, 0.0) };
            assert!((l - wl).abs() < 1e-5 && (r - wr).abs() < 1e-5, "{i}: {l} {r}");
        }
    }

    #[test]
    fn designed_delay_and_cross_terms_land_where_asked() {
        let mut f = FirEngine::new(1024, 64);
        // L out = 0.5 · L delayed 300; R out = R delayed 300 + 0.25 · L delayed 300.
        delay_spec(&mut f, 0, 300.0, 0.5);
        delay_spec(&mut f, 2, 300.0, 0.25);
        delay_spec(&mut f, 3, 300.0, 1.0);
        f.used = [true, false, true, true];
        assert!(f.design(Window::Symmetric { taper: 0.1 }, true));
        let out = run(&mut f, |i| if i == 0 { (1.0, 0.0) } else if i == 1 { (0.0, 1.0) } else { (0.0, 0.0) }, 1600);
        let at = 64 + 300;
        assert!((out[at].0 - 0.5).abs() < 1e-3, "{:?}", out[at]);
        assert!((out[at].1 - 0.25).abs() < 1e-3, "{:?}", out[at]);
        assert!((out[at + 1].1 - 1.0).abs() < 1e-3, "{:?}", out[at + 1]);
        let rest: f32 = out.iter().enumerate().filter(|(i, _)| *i != at && *i != at + 1).map(|(_, (l, r))| l.abs() + r.abs()).sum();
        assert!(rest < 1e-2, "{rest}");
    }

    #[test]
    fn a_new_design_crossfades_without_a_jump() {
        let mut f = FirEngine::new(512, 32);
        delay_spec(&mut f, 0, 100.0, 1.0);
        delay_spec(&mut f, 3, 100.0, 1.0);
        f.design(Window::Symmetric { taper: 0.1 }, true);
        // DC in; then switch the gain to 0.5 mid-stream.
        let mut prev = 0.0f32;
        let mut max_step = 0.0f32;
        for i in 0..3000 {
            if i == 1500 {
                delay_spec(&mut f, 0, 100.0, 0.5);
                delay_spec(&mut f, 3, 100.0, 0.5);
                assert!(f.design(Window::Symmetric { taper: 0.1 }, false));
                assert!(!f.design(Window::Symmetric { taper: 0.1 }, false), "one fade at a time");
            }
            let (l, _) = f.tick(1.0, 1.0);
            if i > 800 {
                max_step = max_step.max((l - prev).abs());
            }
            prev = l;
        }
        assert!((prev - 0.5).abs() < 1e-3, "{prev}");
        assert!(max_step < 0.05, "{max_step}");
        assert!(!f.fading());
    }
}
