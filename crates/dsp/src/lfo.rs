//! Low-frequency oscillator and tempo-sync helpers.

use crate::osc::Noise;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfoShape {
    Sine,
    Triangle,
    SawUp,
    SawDown,
    Square,
    /// Sample & hold (a new random value every cycle).
    SampleHold,
    /// Smoothed random (cosine-interpolated S&H).
    SmoothRandom,
}

impl LfoShape {
    pub const ALL: [LfoShape; 7] =
        [LfoShape::Sine, LfoShape::Triangle, LfoShape::SawUp, LfoShape::SawDown, LfoShape::Square, LfoShape::SampleHold, LfoShape::SmoothRandom];
    pub const NAMES: &'static [&'static str] = &["Sine", "Triangle", "Saw Up", "Saw Down", "Square", "S&H", "Smooth"];

    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(Self::ALL.len() - 1)]
    }
}

/// Bipolar LFO (−1..1). Phase 0..1.
#[derive(Clone, Copy, Debug)]
pub struct Lfo {
    pub shape: LfoShape,
    phase: f64,
    inc: f64,
    held: f32,
    prev_held: f32,
    noise: Noise,
}

impl Lfo {
    pub fn new(seed: u32) -> Self {
        Self { shape: LfoShape::Sine, phase: 0.0, inc: 0.0, held: 0.0, prev_held: 0.0, noise: Noise::new(seed) }
    }

    pub fn set_rate(&mut self, hz: f32, sample_rate: f64) {
        self.inc = (hz as f64 / sample_rate).max(0.0);
    }

    pub fn set_phase(&mut self, phase: f64) {
        self.phase = phase.rem_euclid(1.0);
    }

    pub fn phase(&self) -> f64 {
        self.phase
    }

    /// Lock the phase to the song position: `beats` at the block start and a
    /// cycle of `cycle_beats` quarter notes, plus a phase offset.
    pub fn sync_to_beats(&mut self, beats: f64, cycle_beats: f64, offset: f64) {
        if cycle_beats > 0.0 {
            self.phase = (beats / cycle_beats + offset).rem_euclid(1.0);
        }
    }

    pub fn reset(&mut self, phase: f64) {
        self.phase = phase.rem_euclid(1.0);
        self.held = self.noise.next();
        self.prev_held = self.held;
    }

    /// Current value without advancing.
    #[inline]
    pub fn value(&self) -> f32 {
        let p = self.phase as f32;
        match self.shape {
            LfoShape::Sine => (p * std::f32::consts::TAU).sin(),
            LfoShape::Triangle => 1.0 - 4.0 * (p - 0.5).abs(),
            LfoShape::SawUp => 2.0 * p - 1.0,
            LfoShape::SawDown => 1.0 - 2.0 * p,
            LfoShape::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            LfoShape::SampleHold => self.held,
            LfoShape::SmoothRandom => {
                let t = 0.5 - 0.5 * (p * std::f32::consts::PI).cos();
                self.prev_held + (self.held - self.prev_held) * t
            }
        }
    }

    /// Advance one sample and return the new value.
    #[inline]
    pub fn next(&mut self) -> f32 {
        self.advance(1);
        self.value()
    }

    /// Advance `n` samples (control-rate use).
    #[inline]
    pub fn advance(&mut self, n: usize) {
        self.phase += self.inc * n as f64;
        if self.phase >= 1.0 {
            self.phase -= self.phase.floor();
            self.prev_held = self.held;
            self.held = self.noise.next();
        }
    }
}

/// Musical note divisions for tempo sync.
pub const SYNC_DIVISIONS: &[&str] = &[
    "8 bars", "4 bars", "2 bars", "1 bar", "1/2", "1/2.", "1/2T", "1/4", "1/4.", "1/4T", "1/8", "1/8.", "1/8T", "1/16", "1/16.", "1/16T",
    "1/32", "1/32T",
];

/// Length of [`SYNC_DIVISIONS`]`[i]` in quarter notes (4/4).
pub fn division_beats(i: usize) -> f64 {
    const BEATS: [f64; 18] = [
        32.0,
        16.0,
        8.0,
        4.0,
        2.0,
        3.0,
        4.0 / 3.0,
        1.0,
        1.5,
        2.0 / 3.0,
        0.5,
        0.75,
        1.0 / 3.0,
        0.25,
        0.375,
        1.0 / 6.0,
        0.125,
        1.0 / 12.0,
    ];
    BEATS[i.min(BEATS.len() - 1)]
}

/// Period of a division at `bpm`, in seconds.
pub fn division_seconds(i: usize, bpm: f64) -> f64 {
    division_beats(i) * 60.0 / bpm.max(1.0)
}

/// Rate of a division at `bpm`, in Hz.
pub fn division_hz(i: usize, bpm: f64) -> f64 {
    1.0 / division_seconds(i, bpm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_bounded() {
        for shape in LfoShape::ALL {
            let mut l = Lfo::new(1);
            l.shape = shape;
            l.set_rate(3.0, 1000.0);
            for _ in 0..5000 {
                let v = l.next();
                assert!((-1.0..=1.0).contains(&v), "{shape:?} {v}");
            }
        }
    }

    #[test]
    fn sync_math() {
        assert!((division_hz(7, 120.0) - 2.0).abs() < 1e-9); // 1/4 at 120 bpm
        assert!((division_seconds(3, 120.0) - 2.0).abs() < 1e-9); // 1 bar
        let mut l = Lfo::new(0);
        l.sync_to_beats(5.0, 2.0, 0.0);
        assert!((l.phase() - 0.5).abs() < 1e-12);
    }
}
