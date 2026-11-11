//! Loudness metering after ITU-R BS.1770 / EBU R128: K-weighting,
//! momentary (400 ms), short-term (3 s), gated integrated loudness and
//! loudness range (EBU Tech 3342).
//!
//! Fixed-size state only: mean-square energy is summed per 100 ms step into
//! a ring of 30 steps; each finished 400 ms block (75 % overlap) and each 3 s
//! window is binned into a 0.1 LU histogram that keeps counts and energy
//! sums, from which the gated readings are computed. No allocation after
//! construction; the per-step work is bounded.

use crate::biquad::{Biquad64, BiquadCoeffs};
use std::f64::consts::PI;

const STEPS: usize = 30; // 3 s of 100 ms
const BLOCK_STEPS: usize = 4; // 400 ms
const HIST_MIN: f64 = -70.0;
const HIST_STEP: f64 = 0.1;
const HIST_BINS: usize = 800; // −70 … +10 LUFS
/// What a meter with nothing measured reads.
pub const SILENT_LUFS: f32 = -120.0;

#[inline]
fn to_lufs(ms: f64) -> f64 {
    if ms <= 1e-20 { SILENT_LUFS as f64 } else { -0.691 + 10.0 * ms.log10() }
}

#[inline]
fn bin_of(lufs: f64) -> usize {
    (((lufs - HIST_MIN) / HIST_STEP).floor().max(0.0) as usize).min(HIST_BINS - 1)
}

#[inline]
fn bin_lufs(bin: usize) -> f64 {
    HIST_MIN + (bin as f64 + 0.5) * HIST_STEP
}

/// K-weighting filter pair for one channel, designed from the analogue
/// prototypes of the BS.1770 48 kHz coefficients so any rate gets the same
/// curve.
pub fn k_weighting(sample_rate: f64) -> [BiquadCoeffs; 2] {
    let fs = sample_rate.max(1.0);
    // Stage 1: high shelf (+4 dB at high frequencies, the head model).
    let shelf = {
        let f0 = 1_681.974_450_955_533;
        let g = 3.999_843_853_973_347;
        let q = 0.707_175_236_955_419_6;
        let k = (PI * f0 / fs).tan();
        let vh = 10f64.powf(g / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1.0 + k / q + k * k;
        BiquadCoeffs {
            b0: (vh + vb * k / q + k * k) / a0,
            b1: 2.0 * (k * k - vh) / a0,
            b2: (vh - vb * k / q + k * k) / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
        }
    };
    // Stage 2: the RLB high-pass at ~38 Hz.
    let hp = {
        let f0 = 38.135_470_876_024_44;
        let q = 0.500_327_037_323_877_3;
        let k = (PI * f0 / fs).tan();
        let a0 = 1.0 + k / q + k * k;
        BiquadCoeffs { b0: 1.0, b1: -2.0, b2: 1.0, a1: 2.0 * (k * k - 1.0) / a0, a2: (1.0 - k / q + k * k) / a0 }
    };
    [shelf, hp]
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoudnessReading {
    pub momentary: f32,
    pub short_term: f32,
    pub integrated: f32,
    /// Loudness range in LU.
    pub range: f32,
    /// Highest momentary reading so far.
    pub max_momentary: f32,
}

/// Stereo (or mono) BS.1770 loudness meter.
#[derive(Clone, Debug)]
pub struct LufsMeter {
    filters: [[Biquad64; 2]; 2],
    step_len: usize,
    step_pos: usize,
    step_acc: f64,
    steps: [f64; STEPS],
    step_write: usize,
    steps_done: u64,
    block_count: Box<[u32; HIST_BINS]>,
    block_energy: Box<[f64; HIST_BINS]>,
    st_count: Box<[u32; HIST_BINS]>,
    reading: LoudnessReading,
}

impl LufsMeter {
    pub fn new(sample_rate: f64) -> Self {
        let [s, h] = k_weighting(sample_rate);
        let ch = [Biquad64::new(s), Biquad64::new(h)];
        Self {
            filters: [ch, ch],
            step_len: ((sample_rate * 0.1).round() as usize).max(1),
            step_pos: 0,
            step_acc: 0.0,
            steps: [0.0; STEPS],
            step_write: 0,
            steps_done: 0,
            block_count: Box::new([0; HIST_BINS]),
            block_energy: Box::new([0.0; HIST_BINS]),
            st_count: Box::new([0; HIST_BINS]),
            reading: Self::silent(),
        }
    }

    fn silent() -> LoudnessReading {
        LoudnessReading { momentary: SILENT_LUFS, short_term: SILENT_LUFS, integrated: SILENT_LUFS, range: 0.0, max_momentary: SILENT_LUFS }
    }

    pub fn reset(&mut self) {
        for ch in &mut self.filters {
            for f in ch {
                f.reset();
            }
        }
        self.step_pos = 0;
        self.step_acc = 0.0;
        self.steps = [0.0; STEPS];
        self.step_write = 0;
        self.steps_done = 0;
        self.block_count.fill(0);
        self.block_energy.fill(0.0);
        self.st_count.fill(0);
        self.reading = Self::silent();
    }

    pub fn reading(&self) -> LoudnessReading {
        self.reading
    }

    /// Push one stereo frame (pass the same sample twice for mono content
    /// played on both channels, or `right = 0` for a single channel).
    #[inline]
    pub fn push(&mut self, left: f32, right: f32) {
        let [[s0, h0], [s1, h1]] = &mut self.filters;
        let l = h0.process(s0.process(left as f64));
        let r = h1.process(s1.process(right as f64));
        self.step_acc += l * l + r * r;
        self.step_pos += 1;
        if self.step_pos >= self.step_len {
            self.finish_step();
        }
    }

    fn mean_square(&self, steps: usize) -> f64 {
        let n = (steps as u64).min(self.steps_done).max(1) as usize;
        let mut sum = 0.0;
        for age in 0..n {
            sum += self.steps[(self.step_write + STEPS - 1 - age) % STEPS];
        }
        sum / (n * self.step_len) as f64
    }

    fn finish_step(&mut self) {
        self.steps[self.step_write] = self.step_acc;
        self.step_write = (self.step_write + 1) % STEPS;
        self.steps_done += 1;
        self.step_acc = 0.0;
        self.step_pos = 0;

        let block_ms = self.mean_square(BLOCK_STEPS);
        let m = to_lufs(block_ms);
        let s = to_lufs(self.mean_square(STEPS));
        self.reading.momentary = m as f32;
        self.reading.short_term = s as f32;
        if self.steps_done >= BLOCK_STEPS as u64 {
            self.reading.max_momentary = self.reading.max_momentary.max(m as f32);
            if m > -70.0 {
                let b = bin_of(m);
                self.block_count[b] += 1;
                self.block_energy[b] += block_ms;
                self.reading.integrated = self.integrated() as f32;
            }
        }
        if self.steps_done >= STEPS as u64 && s > -70.0 {
            self.st_count[bin_of(s)] += 1;
            self.reading.range = self.range() as f32;
        }
    }

    fn integrated(&self) -> f64 {
        let (mut n, mut e) = (0u64, 0.0);
        for b in 0..HIST_BINS {
            n += self.block_count[b] as u64;
            e += self.block_energy[b];
        }
        if n == 0 {
            return SILENT_LUFS as f64;
        }
        let gate = to_lufs(e / n as f64) - 10.0;
        let (mut n2, mut e2) = (0u64, 0.0);
        for b in bin_of(gate)..HIST_BINS {
            if bin_lufs(b) > gate {
                n2 += self.block_count[b] as u64;
                e2 += self.block_energy[b];
            }
        }
        if n2 == 0 { SILENT_LUFS as f64 } else { to_lufs(e2 / n2 as f64) }
    }

    fn range(&self) -> f64 {
        // Mean energy of the short-term values for the relative gate.
        let (mut n, mut e) = (0u64, 0.0);
        for b in 0..HIST_BINS {
            let c = self.st_count[b] as u64;
            if c > 0 {
                n += c;
                e += c as f64 * 10f64.powf((bin_lufs(b) + 0.691) / 10.0);
            }
        }
        if n == 0 {
            return 0.0;
        }
        let gate = to_lufs(e / n as f64) - 20.0;
        let first = (0..HIST_BINS).find(|&b| bin_lufs(b) > gate).unwrap_or(HIST_BINS);
        let kept: u64 = self.st_count[first..].iter().map(|&c| c as u64).sum();
        if kept == 0 {
            return 0.0;
        }
        let pct = |p: f64| {
            let rank = ((kept as f64 - 1.0) * p).round() as u64;
            let mut seen = 0u64;
            for b in first..HIST_BINS {
                seen += self.st_count[b] as u64;
                if seen > rank {
                    return bin_lufs(b);
                }
            }
            bin_lufs(HIST_BINS - 1)
        };
        (pct(0.95) - pct(0.10)).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(m: &mut LufsMeter, amp: f32, secs: f32, stereo: bool) {
        let sr = 48_000.0;
        for n in 0..(secs * sr) as usize {
            let x = amp * (std::f32::consts::TAU * 1000.0 * n as f32 / sr).sin();
            m.push(x, if stereo { x } else { 0.0 });
        }
    }

    #[test]
    fn reference_levels() {
        // BS.1770: a 0 dBFS 1 kHz sine on one channel reads −3.01 LUFS.
        let mut m = LufsMeter::new(48_000.0);
        sine(&mut m, 1.0, 4.0, false);
        assert!((m.reading().integrated + 3.01).abs() < 0.1, "{:?}", m.reading());
        // −20 dBFS on both channels reads −20.
        let mut m = LufsMeter::new(48_000.0);
        sine(&mut m, 0.1, 5.0, true);
        let r = m.reading();
        for v in [r.momentary, r.short_term, r.integrated] {
            assert!((v + 20.0).abs() < 0.15, "{r:?}");
        }
        assert!(r.range < 0.5);
    }

    #[test]
    fn gating_and_range() {
        let mut m = LufsMeter::new(48_000.0);
        sine(&mut m, 0.1, 6.0, true);
        sine(&mut m, 0.0, 6.0, true);
        assert!((m.reading().integrated + 20.0).abs() < 0.2, "{:?}", m.reading());
        let mut m = LufsMeter::new(48_000.0);
        sine(&mut m, 0.1, 10.0, true);
        sine(&mut m, 0.1 * 10f32.powf(-0.5), 10.0, true);
        let lra = m.reading().range;
        assert!(lra > 9.0 && lra < 11.0, "{lra}");
    }

    #[test]
    fn k_weighting_at_48k_matches_the_standard() {
        // BS.1770 Table 1/2 coefficients at 48 kHz.
        let [s, h] = k_weighting(48_000.0);
        assert!((s.b0 - 1.535_124_859_586_97).abs() < 1e-6);
        assert!((s.a1 + 1.690_659_293_182_41).abs() < 1e-6);
        assert!((h.a1 + 1.990_047_454_833_98).abs() < 1e-6);
        assert!((h.a2 - 0.990_072_250_366_21).abs() < 1e-6);
    }
}
