//! Shimmer: a two-grain delay-line pitch shifter for the reverb's
//! feedback path (Zölzer, *DAFX* 2nd ed., §6.4 "pitch shifting by delay-line
//! modulation"; Puckette, *Theory and Technique*, §7.9).
//!
//! Two read heads sweep a delay line at rate `1 − ratio` (a falling delay
//! raises the pitch), half a grain apart. Each head fades in and out with a
//! raised cosine whose two copies sum to exactly one, so a steady tone keeps
//! its level and the jump of a head back to the far end of the grain is
//! silent. In the reverb, the grain smear is hidden by the tank's diffusion.
//!
//! The heads read through the band-limited kernel of
//! [`aitrack_dsp::resample`] with its cutoff at the new Nyquist, so shifting
//! up does not fold the top of the band back on every trip round the loop
//! (the linear reads left a 13 kHz sine at −2.7 dB an octave up). The heads
//! sit [`PitchShifter::OFFSET`] samples behind the input so the kernel fits.

use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::resample::{Kernel, Quality};
use std::f32::consts::TAU;

/// The intervals the shimmer can shift by, in semitones.
pub const INTERVALS: [f32; 9] = [-12.0, -7.0, -5.0, 0.0, 5.0, 7.0, 12.0, 19.0, 24.0];
pub const INTERVAL_NAMES: &[&str] = &["-12", "-7", "-5", "0", "+5", "+7", "+12", "+19", "+24"];
/// Grain length.
pub const GRAIN_MS: f32 = 80.0;

#[derive(Clone, Debug)]
pub struct PitchShifter {
    line: DelayLine,
    grain: f32,
    phase: f32,
    inc: f32,
    fc: f32,
    kernel: &'static Kernel,
}

impl PitchShifter {
    /// Delay of a head at the start of its grain: room for the kernel at
    /// the largest interval (+24 st).
    pub const OFFSET: f32 = 80.0;

    /// Allocates: call from `prepare`.
    pub fn new(sample_rate: f64) -> Self {
        let grain = GRAIN_MS * 0.001 * sample_rate as f32;
        let kernel = Kernel::get(Quality::High);
        debug_assert!(kernel.reach(kernel.cutoff(4.0)) < Self::OFFSET);
        Self { line: DelayLine::new((grain + 2.0 * Self::OFFSET) as usize + 8), grain, phase: 0.0, inc: 0.0, fc: 1.0, kernel }
    }

    /// Shift by `semitones`.
    pub fn set_semitones(&mut self, semitones: f32) {
        let ratio = 2f32.powf(semitones / 12.0);
        self.inc = (1.0 - ratio) / self.grain;
        self.fc = self.kernel.cutoff(ratio as f64);
    }

    pub fn clear(&mut self) {
        self.line.clear();
        self.phase = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.line.push(x);
        let mut ph = self.phase + self.inc;
        if ph >= 1.0 {
            ph -= 1.0;
        } else if ph < 0.0 {
            ph += 1.0;
        }
        self.phase = ph;
        let ph2 = if ph >= 0.5 { ph - 0.5 } else { ph + 0.5 };
        // Raised cosine: w(φ) + w(φ + ½) = 1.
        let w = 0.5 - 0.5 * (TAU * ph).cos();
        let a = self.kernel.read_delay(&self.line, Self::OFFSET + ph * self.grain, self.fc);
        let b = self.kernel.read_delay(&self.line, Self::OFFSET + ph2 * self.grain, self.fc);
        w * a + (1.0 - w) * b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Energy of `x` at `freq` (Goertzel), normalised.
    fn tone(x: &[f32], freq: f32, sr: f32) -> f32 {
        let w = TAU * freq / sr;
        let c = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &v in x {
            let s = v + c * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s1 * s1 + s2 * s2 - c * s1 * s2).sqrt() / x.len() as f32
    }

    #[test]
    fn an_octave_up_moves_the_tone() {
        let sr = 48_000.0;
        let mut p = PitchShifter::new(sr as f64);
        p.set_semitones(12.0);
        let y: Vec<f32> = (0..48_000).map(|n| p.process((TAU * 440.0 * n as f32 / sr).sin())).collect();
        let tail = &y[8_000..];
        let (up, base) = (tone(tail, 880.0, sr), tone(tail, 440.0, sr));
        assert!(up > 6.0 * base, "880 Hz {up} vs 440 Hz {base}");
    }

    #[test]
    fn unison_is_a_flat_delay_and_levels_hold() {
        let sr = 48_000.0;
        let mut p = PitchShifter::new(sr as f64);
        p.set_semitones(0.0);
        let y: Vec<f32> = (0..20_000).map(|n| p.process((TAU * 300.0 * n as f32 / sr).sin())).collect();
        let peak = y[10_000..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 1.0).abs() < 0.02, "{peak}");
        for st in INTERVALS {
            p.set_semitones(st);
            for n in 0..20_000 {
                let v = p.process((TAU * 300.0 * n as f32 / sr).sin());
                assert!(v.is_finite() && v.abs() <= 1.01);
            }
        }
    }

    /// Report 24.01 §10 #2: the shimmer's octave up must not fold 13 kHz
    /// back to 22 kHz (it is fed back, so every pass would add more).
    #[test]
    fn octave_up_does_not_alias() {
        let sr = 48_000.0f64;
        let run = |freq: f64| {
            let mut p = PitchShifter::new(sr);
            p.set_semitones(12.0);
            let y: Vec<f32> = (0..48_000).map(|n| p.process((0.5 * (std::f64::consts::TAU * freq * n as f64 / sr).sin()) as f32)).collect();
            y[12_000..44_000].to_vec()
        };
        let rms = |x: &[f32]| (x.iter().map(|v| (v * v) as f64).sum::<f64>() / x.len() as f64).sqrt();
        let reference = run(4_000.0);
        let alias = 20.0 * (rms(&run(13_000.0)) / rms(&reference)).log10();
        let pass = 20.0 * (rms(&reference) / (0.5 / 2f64.sqrt())).log10();
        eprintln!("shimmer +12 st: 13 kHz residual {alias:.1} dB, 4 kHz -> 8 kHz gain {pass:+.2} dB");
        assert!(alias < -60.0, "13 kHz an octave up leaves {alias:.1} dB");
        assert!(pass.abs() < 0.5, "passband gain {pass:+.2} dB");
    }

}
