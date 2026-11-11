//! Band-limited wavetables with per-octave mip levels.
//!
//! A table is a stack of single-cycle frames the oscillator morphs through.
//! Every frame is stored at [`LEVELS`] levels an octave apart; level `l`
//! keeps only the first `MAX_HARMONICS >> l` harmonics and is sampled at
//! least eight times finer than its top harmonic needs, so linear
//! interpolation stays clean. A playing oscillator picks the fullest level whose top harmonic
//! stays below Nyquist, so no partial ever folds back.
//!
//! Frames are synthesised from their Fourier series with an inverse real
//! FFT (or measured from a drawn cycle with a forward FFT). Everything is
//! built off the audio thread; reading is allocation free.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

/// Harmonics in the fullest level (a 47 Hz note reaches 24 kHz at 48 kHz).
pub const MAX_HARMONICS: usize = 512;
/// Octave levels: 512, 256, … 1 harmonics.
pub const LEVELS: usize = 10;
/// Table samples per cycle of the top harmonic in the fullest level.
const OVERSAMPLE: usize = 8;
/// Every level is stored at the same length: the thinner levels are then
/// heavily oversampled, which keeps linear-interpolation images below
/// −90 dB.
const LEVEL_SIZE: usize = MAX_HARMONICS * OVERSAMPLE;

#[derive(Clone, Copy, Debug)]
struct Level {
    offset: usize,
    size: usize,
    harmonics: usize,
}

/// A morphing, mip-mapped wavetable.
#[derive(Clone, Debug)]
pub struct Wavetable {
    frames: usize,
    levels: [Level; LEVELS],
    /// Per frame, per level: `size + 1` samples (one guard for interpolation).
    frame_stride: usize,
    data: Vec<f32>,
}

impl Wavetable {
    /// Build from a harmonic description: `spectrum(frame, k)` returns the
    /// `(sin, cos)` amplitudes of harmonic `k >= 1` for `frame`.
    pub fn from_harmonics(frames: usize, spectrum: impl Fn(usize, usize) -> (f32, f32)) -> Self {
        let frames = frames.max(1);
        let mut levels = [Level { offset: 0, size: 0, harmonics: 0 }; LEVELS];
        let mut offset = 0;
        for (l, lv) in levels.iter_mut().enumerate() {
            let harmonics = (MAX_HARMONICS >> l).max(1);
            let size = LEVEL_SIZE;
            *lv = Level { offset, size, harmonics };
            offset += size + 1;
        }
        let frame_stride = offset;
        let mut data = vec![0.0f32; frame_stride * frames];
        let mut planner = RealFftPlanner::<f32>::new();
        for f in 0..frames {
            let mut norm = 0.0f32;
            for (l, lv) in levels.iter().enumerate() {
                let c2r = planner.plan_fft_inverse(lv.size);
                let mut spec = c2r.make_input_vec();
                let mut out = c2r.make_output_vec();
                for (k, bin) in spec.iter_mut().enumerate().take(lv.harmonics.min(lv.size / 2 - 1) + 1).skip(1) {
                    let (s, c) = spectrum(f, k);
                    // x[n] = Σ X_k e^{j2πkn/N} + conj → a sin + b cos
                    *bin = Complex::new(c * 0.5, -s * 0.5);
                }
                // The inverse is exact; ignore the (impossible) length error.
                let _ = c2r.process(&mut spec, &mut out);
                let base = f * frame_stride + lv.offset;
                data[base..base + lv.size].copy_from_slice(&out);
                data[base + lv.size] = out[0];
                if l == 0 {
                    norm = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                }
            }
            // One gain per frame (from its fullest level) keeps the levels
            // consistent and every frame at full scale.
            if norm > 1e-9 {
                let g = 1.0 / norm;
                for v in &mut data[f * frame_stride..(f + 1) * frame_stride] {
                    *v *= g;
                }
            }
        }
        Self { frames, levels, frame_stride, data }
    }

    /// Build from drawn single cycles (`cycles[frame]`, any length ≥ 16) by
    /// measuring their spectra.
    pub fn from_cycles(cycles: &[Vec<f32>]) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let spectra: Vec<Vec<(f32, f32)>> = cycles
            .iter()
            .map(|cycle| {
                let n = cycle.len().max(16);
                let r2c = planner.plan_fft_forward(n);
                let mut input = cycle.clone();
                input.resize(n, 0.0);
                let mut out = r2c.make_output_vec();
                let _ = r2c.process(&mut input, &mut out);
                let scale = 2.0 / n as f32;
                (0..=MAX_HARMONICS).map(|k| out.get(k).map_or((0.0, 0.0), |c| (-c.im * scale, c.re * scale))).collect()
            })
            .collect();
        Self::from_harmonics(cycles.len(), |f, k| spectra[f].get(k).copied().unwrap_or((0.0, 0.0)))
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Fullest level for a note whose frequency is `ratio = f0 / fs`.
    #[inline]
    pub fn level_for(&self, ratio: f32) -> usize {
        // Need harmonics * ratio <= 0.5 → harmonics <= 0.5 / ratio.
        let max_h = 0.5 / ratio.max(1e-9);
        let mut l = 0;
        while l < LEVELS - 1 && self.levels[l].harmonics as f32 > max_h {
            l += 1;
        }
        l
    }

    #[inline]
    fn read_level(&self, frame: usize, level: usize, phase: f32) -> f32 {
        let lv = self.levels[level];
        let pos = phase * lv.size as f32;
        let i = (pos as usize).min(lv.size - 1);
        let t = pos - i as f32;
        let base = frame * self.frame_stride + lv.offset + i;
        let a = self.data[base];
        let b = self.data[base + 1];
        a + (b - a) * t
    }

    /// Sample at `phase ∈ [0, 1)`, frame morph `position ∈ [0, 1]`.
    #[inline]
    pub fn sample(&self, level: usize, position: f32, phase: f32) -> f32 {
        if self.frames == 1 {
            return self.read_level(0, level, phase);
        }
        let fp = position.clamp(0.0, 1.0) * (self.frames - 1) as f32;
        let f0 = (fp as usize).min(self.frames - 2);
        let t = fp - f0 as f32;
        let a = self.read_level(f0, level, phase);
        if t <= 0.0 {
            return a;
        }
        let b = self.read_level(f0 + 1, level, phase);
        a + (b - a) * t
    }

    // ── Built-in generated tables ─────────────────────────────────────────

    pub fn sine() -> Self {
        Self::from_harmonics(1, |_, k| if k == 1 { (1.0, 0.0) } else { (0.0, 0.0) })
    }

    pub fn saw() -> Self {
        Self::from_harmonics(1, |_, k| (saw_harmonic(k), 0.0))
    }

    pub fn square() -> Self {
        Self::from_harmonics(1, |_, k| if k % 2 == 1 { (4.0 / (std::f32::consts::PI * k as f32), 0.0) } else { (0.0, 0.0) })
    }

    pub fn triangle() -> Self {
        Self::from_harmonics(1, |_, k| {
            if k % 2 == 1 {
                let sign = if (k / 2) % 2 == 0 { 1.0 } else { -1.0 };
                (sign * 8.0 / (std::f32::consts::PI.powi(2) * (k * k) as f32), 0.0)
            } else {
                (0.0, 0.0)
            }
        })
    }

    /// Square narrowing to a thin pulse (width 50 % → 5 %).
    pub fn pulse_morph(frames: usize) -> Self {
        Self::from_harmonics(frames, |f, k| {
            let w = 0.5 - 0.45 * f as f32 / (frames.max(2) - 1) as f32;
            let kk = k as f32;
            // Pulse centred on t = w/2: amplitude 4 sin(πkw)/(πk), cos phase.
            let amp = 4.0 * (std::f32::consts::PI * kk * w).sin() / (std::f32::consts::PI * kk);
            let ph = std::f32::consts::TAU * kk * w * 0.5;
            (amp * ph.sin(), amp * ph.cos())
        })
    }

    /// A sine that grows harmonics until it is a full saw.
    pub fn sine_to_saw(frames: usize) -> Self {
        Self::from_harmonics(frames, |f, k| {
            let m = f as f32 / (frames.max(2) - 1) as f32;
            let reach = 0.35 + 600.0 * m.powi(3);
            let roll = (-((k - 1) as f32) / reach).exp();
            (saw_harmonic(k) * roll, 0.0)
        })
    }
}

/// Fourier coefficient of a rising saw (−1..1) at harmonic `k`.
#[inline]
pub fn saw_harmonic(k: usize) -> f32 {
    // 2t − 1 = −(2/π) Σ sin(2πkt) / k
    -2.0 / (std::f32::consts::PI * k as f32)
}

/// Phase accumulator reading a [`Wavetable`].
#[derive(Clone, Copy, Debug, Default)]
pub struct WavetableOsc {
    phase: f32,
    inc: f32,
    level: usize,
}

impl WavetableOsc {
    pub fn set_freq(&mut self, table: &Wavetable, hz: f32, sample_rate: f32) {
        self.inc = (hz / sample_rate).clamp(0.0, 0.49);
        self.level = table.level_for(self.inc);
    }

    pub fn set_phase(&mut self, phase: f32) {
        self.phase = phase.rem_euclid(1.0);
    }

    pub fn phase(&self) -> f32 {
        self.phase
    }

    #[inline]
    pub fn next(&mut self, table: &Wavetable, position: f32) -> f32 {
        let y = table.sample(self.level, position, self.phase);
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_spectrum(table: &Wavetable, hz: f32, sr: f32, n: usize) -> Vec<f32> {
        let mut osc = WavetableOsc::default();
        osc.set_freq(table, hz, sr);
        let mut x: Vec<f32> = (0..n)
            .map(|i| {
                // Hann window.
                let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos();
                osc.next(table, 0.0) * w
            })
            .collect();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let mut out = fft.make_output_vec();
        fft.process(&mut x, &mut out).unwrap();
        out.iter().map(|c| c.norm()).collect()
    }

    #[test]
    fn saw_has_no_aliasing() {
        let sr = 48_000.0;
        let n = 1 << 15;
        let table = Wavetable::saw();
        // A high note: 3.1 kHz. Its harmonics sit at multiples; anything else
        // is aliasing.
        let hz = 3100.0;
        let spec = tone_spectrum(&table, hz, sr, n);
        let bin_hz = sr / n as f32;
        let fundamental = spec[(hz / bin_hz).round() as usize];
        let mut worst = 0.0f32;
        for (i, &m) in spec.iter().enumerate() {
            let f = i as f32 * bin_hz;
            let nearest = (f / hz).round() * hz;
            if (f - nearest).abs() > 60.0 && f > 50.0 {
                worst = worst.max(m);
            }
        }
        let db = 20.0 * (worst / fundamental).log10();
        assert!(db < -80.0, "alias {db} dB");
    }

    #[test]
    fn tables_are_normalised() {
        for t in [Wavetable::saw(), Wavetable::square(), Wavetable::triangle(), Wavetable::sine(), Wavetable::pulse_morph(8), Wavetable::sine_to_saw(8)] {
            let mut peak = 0.0f32;
            for i in 0..1000 {
                peak = peak.max(t.sample(0, 0.3, i as f32 / 1000.0).abs());
            }
            assert!(peak > 0.9 && peak < 1.05, "{peak}");
        }
    }

    #[test]
    fn level_selection() {
        let t = Wavetable::saw();
        assert_eq!(t.level_for(20.0 / 48_000.0), 0);
        let l = t.level_for(1000.0 / 48_000.0);
        assert!((MAX_HARMONICS >> l) as f32 * 1000.0 <= 24_000.0);
        assert!((MAX_HARMONICS >> (l - 1)) as f32 * 1000.0 > 24_000.0);
    }

    #[test]
    fn from_cycles_round_trips_a_saw() {
        let n = 2048;
        let cycle: Vec<f32> = (0..n).map(|i| 2.0 * i as f32 / n as f32 - 1.0).collect();
        let t = Wavetable::from_cycles(&[cycle]);
        let s = Wavetable::saw();
        let mut err = 0.0f32;
        for i in 1..100 {
            let ph = i as f32 / 100.0 + 0.003;
            err = err.max((t.sample(3, 0.0, ph) - s.sample(3, 0.0, ph)).abs());
        }
        assert!(err < 0.05, "{err}");
    }
}
