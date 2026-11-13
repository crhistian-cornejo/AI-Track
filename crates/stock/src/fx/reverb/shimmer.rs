//! Shimmer: a two-grain delay-line pitch shifter for the reverb's
//! feedback path (Zölzer, *DAFX* 2nd ed., §6.4 "pitch shifting by delay-line
//! modulation"; Puckette, *Theory and Technique*, §7.9).
//!
//! Two read heads sweep a delay line at rate `1 − ratio` (a falling delay
//! raises the pitch), half a grain apart. Each head fades in and out with a
//! raised cosine whose two copies sum to exactly one, so a steady tone keeps
//! its level and the jump of a head back to the far end of the grain is
//! silent. In the reverb, the grain smear is hidden by the tank's diffusion.

use aitrack_dsp::delay::DelayLine;
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
}

impl PitchShifter {
    /// Allocates: call from `prepare`.
    pub fn new(sample_rate: f64) -> Self {
        let grain = GRAIN_MS * 0.001 * sample_rate as f32;
        Self { line: DelayLine::new(grain as usize + 8), grain, phase: 0.0, inc: 0.0 }
    }

    /// Shift by `semitones`.
    pub fn set_semitones(&mut self, semitones: f32) {
        let ratio = 2f32.powf(semitones / 12.0);
        self.inc = (1.0 - ratio) / self.grain;
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
        let a = self.line.read_linear(1.0 + ph * self.grain);
        let b = self.line.read_linear(1.0 + ph2 * self.grain);
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
}
