//! Grain sources: a four-level mip chain (full rate, ½, ¼, ⅛) that is read
//! with a cubic Hermite interpolator.
//!
//! **Why mips.** Reading a source faster than it was recorded (a grain
//! transposed up) folds everything above the new Nyquist back into the
//! band. The cure is to low-pass *before* decimating, so each level is the
//! previous one run through a 31-tap windowed-sinc half-band filter and
//! decimated by two. A grain whose step is `s` source samples per output
//! sample reads the two levels around `log2(s)` and cross-fades between
//! them, so the band limit follows the pitch smoothly (the same idea as the
//! mip-mapped wavetables of the synth, see `crate::instrument::wavetable`).
//!
//! The cascade is *streaming*: [`MipBuf::push`] takes one input frame and
//! cheaply updates every level, so the same code builds a static source
//! (file, built-in, on the control thread) and keeps a live ring buffer up
//! to date (audio thread, no allocation). Level `j` sample `m` holds the
//! low-passed signal at level-0 time `2^j m - (2^j - 1) D`, with `D` the
//! half-band group delay; [`level_pos`] does that mapping.

use aitrack_dsp::delay::hermite;

/// Mip levels (0 = full rate).
pub const LEVELS: usize = 4;
/// Guard samples on each side of a level (Hermite reads `i-1 ..= i+2`).
pub const GUARD: usize = 4;
/// Half-band group delay, in samples of its own input rate.
pub const D: usize = 15;
const TAPS: usize = 2 * D + 1;
/// How far behind the newest sample the deepest level is complete, plus the
/// interpolator's reach: live reads stay this far from the write head.
pub const LIVE_GUARD: f64 = 192.0;

/// One mip level of one source: planar, with guard samples at both ends.
pub struct Level {
    pub ch: [Vec<f32>; 2],
    pub cap: usize,
    ring: bool,
}

impl Level {
    fn new(cap: usize, stereo: bool, ring: bool) -> Self {
        let n = cap + 2 * GUARD;
        Self { ch: [vec![0.0; n], if stereo { vec![0.0; n] } else { Vec::new() }], cap, ring }
    }

    #[inline]
    fn write(&mut self, abs: u64, l: f32, r: f32, stereo: bool) {
        let idx = (abs % self.cap as u64) as usize;
        let cap = self.cap;
        self.ch[0][idx + GUARD] = l;
        if stereo {
            self.ch[1][idx + GUARD] = r;
        }
        if self.ring {
            if idx < GUARD {
                self.ch[0][cap + GUARD + idx] = l;
                if stereo {
                    self.ch[1][cap + GUARD + idx] = r;
                }
            }
            if idx + GUARD >= cap {
                let g = idx + GUARD - cap;
                self.ch[0][g] = l;
                if stereo {
                    self.ch[1][g] = r;
                }
            }
        }
    }

    /// Cubic Hermite read at ring coordinate `pos ∈ [0, cap)`.
    #[inline]
    pub fn read(&self, c: usize, pos: f64) -> f32 {
        let i = (pos as usize).min(self.cap - 1);
        let t = (pos - i as f64) as f32;
        let t = if (0.0..1.0).contains(&t) { t } else { 0.0 };
        let b = if c == 0 || self.ch[1].is_empty() { &self.ch[0] } else { &self.ch[1] };
        let k = i + GUARD;
        hermite(t, b[k - 1], b[k], b[k + 1], b[k + 2])
    }
}

/// 31-tap half-band low-pass (Blackman-windowed sinc, cutoff `fs/4`);
/// only the centre and the odd taps are non-zero.
fn halfband() -> [f32; D + 1] {
    // h[k] for k = 0..=D, k = 0 is the centre.
    let mut h = [0.0f64; D + 1];
    let mut sum = 0.0;
    for k in 0..=D {
        let kf = k as f64;
        let dd = (D + 1) as f64;
        let win = 0.42 + 0.5 * (std::f64::consts::PI * kf / dd).cos() + 0.08 * (2.0 * std::f64::consts::PI * kf / dd).cos();
        let v = if k == 0 {
            0.5
        } else if k % 2 == 1 {
            (std::f64::consts::PI * kf / 2.0).sin() / (std::f64::consts::PI * kf)
        } else {
            0.0
        };
        h[k] = v * win;
        sum += if k == 0 { h[k] } else { 2.0 * h[k] };
    }
    let mut out = [0.0f32; D + 1];
    for k in 0..=D {
        out[k] = (h[k] / sum) as f32;
    }
    out
}

/// One decimate-by-two stage with its input history.
#[derive(Clone)]
struct Stage {
    hist: [[f32; 32]; 2],
    n: u64,
}

impl Stage {
    fn new() -> Self {
        Self { hist: [[0.0; 32]; 2], n: 0 }
    }

    /// Feed one frame; every other call returns the filtered frame.
    #[inline]
    fn push(&mut self, h: &[f32; D + 1], l: f32, r: f32, stereo: bool) -> Option<(f32, f32)> {
        let i = (self.n & 31) as usize;
        self.hist[0][i] = l;
        self.hist[1][i] = r;
        let fire = self.n & 1 == 0;
        self.n += 1;
        if !fire {
            return None;
        }
        // Newest sample is at `i`; the filter centre is D samples behind it.
        let c = i.wrapping_sub(D) & 31;
        let mut out = [0.0f32; 2];
        for ch in 0..(if stereo { 2 } else { 1 }) {
            let hb = &self.hist[ch];
            let mut acc = h[0] * hb[c];
            let mut k = 1;
            while k <= D {
                acc += h[k] * (hb[c.wrapping_sub(k) & 31] + hb[(c + k) & 31]);
                k += 2;
            }
            out[ch] = acc;
        }
        Some((out[0], out[1]))
    }

    fn reset(&mut self) {
        self.hist = [[0.0; 32]; 2];
        self.n = 0;
    }
}

/// A source with its mip chain. `frames` is the usable length of a static
/// source in level-0 samples; a live one is a ring (`frames` unused).
pub struct MipBuf {
    pub levels: [Level; LEVELS],
    pub mono: bool,
    /// Sample rate the content was recorded at.
    pub src_sr: f64,
    pub frames: usize,
    /// Frames pushed so far (level 0).
    pub written: u64,
    /// Frames of valid history in a live ring since the last gap.
    pub valid: u64,
    stages: [Stage; LEVELS - 1],
    w: [u64; LEVELS],
    h: [f32; D + 1],
}

impl MipBuf {
    fn with_caps(caps: [usize; LEVELS], mono: bool, ring: bool, src_sr: f64, frames: usize) -> Self {
        let lv = |j: usize| Level::new(caps[j].max(8), !mono, ring);
        Self {
            levels: [lv(0), lv(1), lv(2), lv(3)],
            mono,
            src_sr,
            frames,
            written: 0,
            valid: 0,
            stages: [Stage::new(), Stage::new(), Stage::new()],
            w: [0; LEVELS],
            h: halfband(),
        }
    }

    /// A live ring of `cap0` frames (rounded up to a multiple of 8).
    pub fn new_live(cap0: usize, sr: f64) -> Self {
        let cap0 = cap0.div_ceil(8) * 8;
        Self::with_caps([cap0, cap0 / 2, cap0 / 4, cap0 / 8], false, true, sr, 0)
    }

    /// Build a static source from planar data (`r = None` for mono).
    pub fn from_planar(l: &[f32], r: Option<&[f32]>, src_sr: f64) -> Self {
        let frames = l.len();
        let flush = 8 * TAPS + 16;
        let total = frames + flush;
        let caps = [total + 8, total / 2 + 8, total / 4 + 8, total / 8 + 8];
        let mut m = Self::with_caps(caps, r.is_none(), false, src_sr, frames);
        for i in 0..frames {
            let a = l[i];
            let b = r.map_or(a, |r| r[i]);
            m.push(if a.is_finite() { a } else { 0.0 }, if b.is_finite() { b } else { 0.0 });
        }
        for _ in 0..flush {
            m.push(0.0, 0.0);
        }
        m.written = frames as u64;
        m.valid = frames as u64;
        m
    }

    /// Forget a live ring's history (keeps the memory).
    pub fn clear(&mut self) {
        for lv in &mut self.levels {
            for c in &mut lv.ch {
                c.fill(0.0);
            }
        }
        for s in &mut self.stages {
            s.reset();
        }
        self.w = [0; LEVELS];
        self.written = 0;
        self.valid = 0;
    }

    /// Push one frame through the cascade. No allocation.
    #[inline]
    pub fn push(&mut self, l: f32, r: f32) {
        let stereo = !self.mono;
        self.levels[0].write(self.w[0], l, r, stereo);
        self.w[0] += 1;
        self.written += 1;
        self.valid += 1;
        let (mut cl, mut cr) = (l, r);
        for s in 0..LEVELS - 1 {
            match self.stages[s].push(&self.h, cl, cr, stereo) {
                Some((a, b)) => {
                    self.levels[s + 1].write(self.w[s + 1], a, b, stereo);
                    self.w[s + 1] += 1;
                    cl = a;
                    cr = b;
                }
                None => break,
            }
        }
    }

    /// The newest absolute level-0 index plus one (the write head).
    #[inline]
    pub fn head(&self) -> u64 {
        self.written
    }
}

/// Ring coordinate in level `j` of level-0 absolute time `t0`.
#[inline]
pub fn level_pos(j: usize, t0: f64, cap: usize) -> f64 {
    let m = (t0 + ((1usize << j) - 1) as f64 * D as f64) / (1usize << j) as f64;
    m.rem_euclid(cap as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, sr: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (std::f64::consts::TAU * f * i as f64 / sr).sin() as f32).collect()
    }

    #[test]
    fn halfband_has_unity_dc_gain_and_rejects_the_upper_band() {
        let h = halfband();
        let dc: f32 = h[0] + 2.0 * h.iter().skip(1).sum::<f32>();
        assert!((dc - 1.0).abs() < 1e-5);
        // Response at 0.4 * fs (well into the stop band of a fs/4 cutoff).
        let w = std::f64::consts::TAU * 0.4;
        let mut re = h[0] as f64;
        for k in 1..=D {
            re += 2.0 * h[k] as f64 * (w * k as f64).cos();
        }
        assert!(re.abs() < 0.003, "stop band {re}");
    }

    #[test]
    fn levels_line_up_in_time_with_level_zero() {
        // A 300 Hz sine is far below every cutoff, so each level must be the
        // same signal at the position `level_pos` says.
        let x = sine(300.0, 48_000.0, 20_000);
        let m = MipBuf::from_planar(&x, None, 48_000.0);
        for j in 1..LEVELS {
            let mut worst = 0.0f32;
            for t in (2000..16_000).step_by(37) {
                let t0 = t as f64 + 0.3;
                let a = m.levels[0].read(0, level_pos(0, t0, m.levels[0].cap));
                let b = m.levels[j].read(0, level_pos(j, t0, m.levels[j].cap));
                worst = worst.max((a - b).abs());
            }
            assert!(worst < 0.01, "level {j} off by {worst}");
        }
    }

    #[test]
    fn upper_levels_remove_what_would_alias() {
        // 15 kHz at 48 kHz: level 1 (24 kHz sample rate) cannot carry it, so
        // the half-band must have removed it; a 3 kHz tone must survive.
        let hi = MipBuf::from_planar(&sine(15_000.0, 48_000.0, 20_000), None, 48_000.0);
        let lo = MipBuf::from_planar(&sine(3_000.0, 48_000.0, 20_000), None, 48_000.0);
        let rms = |m: &MipBuf| {
            let l = &m.levels[1];
            let s: f64 = (2000..6000).map(|i| (l.ch[0][i + GUARD] as f64).powi(2)).sum();
            (s / 4000.0).sqrt()
        };
        assert!(rms(&hi) < 0.01, "15 kHz leaks into level 1: {}", rms(&hi));
        assert!(rms(&lo) > 0.6, "3 kHz lost: {}", rms(&lo));
    }

    #[test]
    fn live_ring_matches_the_static_build() {
        let x = sine(440.0, 48_000.0, 6000);
        let s = MipBuf::from_planar(&x, None, 48_000.0);
        let mut live = MipBuf::new_live(2048, 48_000.0);
        for &v in &x {
            live.push(v, v);
        }
        // The ring holds the last 2048 frames; compare a stretch inside.
        let head = live.head();
        for j in 0..LEVELS {
            let mut worst = 0.0f32;
            for back in (400..1500).step_by(11) {
                let t0 = (head - back) as f64 + 0.25;
                let a = s.levels[j].read(0, level_pos(j, t0, s.levels[j].cap));
                let b = live.levels[j].read(0, level_pos(j, t0, live.levels[j].cap));
                worst = worst.max((a - b).abs());
            }
            assert!(worst < 1e-4, "level {j}: {worst}");
        }
    }

    #[test]
    fn reads_never_panic_at_the_edges() {
        let mut live = MipBuf::new_live(64, 48_000.0);
        for i in 0..500 {
            live.push(i as f32, -(i as f32));
        }
        for j in 0..LEVELS {
            let cap = live.levels[j].cap;
            for p in [0.0, 0.5, cap as f64 - 0.001, cap as f64, -3.0, f64::NAN, 1e30] {
                let _ = live.levels[j].read(0, p);
                let _ = live.levels[j].read(1, p);
            }
        }
    }
}
