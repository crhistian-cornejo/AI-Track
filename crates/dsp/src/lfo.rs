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

/// Waveforms of [`PhaseLfo`], the nine shapes of an auto-pan / tremolo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waveform {
    Sine,
    Triangle,
    /// Rounded rising flank and a hard drop.
    SharkTooth,
    SawUp,
    SawDown,
    Square,
    /// Smooth random: cosine-interpolated random values, one per cycle.
    Random,
    /// Slow 1/f-like drift: two smoothed random sequences at `f` and `f/3`.
    Wander,
    /// Sample & hold: a new random step every cycle.
    SampleHold,
}

impl Waveform {
    pub const ALL: [Waveform; 9] = [
        Waveform::Sine,
        Waveform::Triangle,
        Waveform::SharkTooth,
        Waveform::SawUp,
        Waveform::SawDown,
        Waveform::Square,
        Waveform::Random,
        Waveform::Wander,
        Waveform::SampleHold,
    ];
    pub const NAMES: &'static [&'static str] =
        &["Sine", "Triangle", "Shark Tooth", "Saw Up", "Saw Down", "Square", "Random", "Wander", "S&H"];

    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(Self::ALL.len() - 1)]
    }

    /// Shapes whose sequence is random (they need an independent generator
    /// per channel and ignore phase offsets).
    pub fn is_random(self) -> bool {
        matches!(self, Waveform::Random | Waveform::Wander | Waveform::SampleHold)
    }
}

/// A bipolar (−1..1) waveform evaluated at an externally supplied phase.
///
/// Unlike [`Lfo`] the caller owns the phase (tempo sync, per-channel offsets,
/// frequency modulation); the generator only watches it wrap to draw the
/// next random value. Call [`value`](Self::value) once per sample.
#[derive(Clone, Copy, Debug)]
pub struct PhaseLfo {
    noise: Noise,
    last: f64,
    held: f32,
    prev_held: f32,
    /// Slow companion sequence (`f/3`) of [`Waveform::Wander`].
    slow_phase: f64,
    slow_held: f32,
    slow_prev: f32,
}

impl PhaseLfo {
    pub fn new(seed: u32) -> Self {
        let mut noise = Noise::new(seed);
        let (a, b, c, d) = (noise.next(), noise.next(), noise.next(), noise.next());
        Self { noise, last: 0.0, held: a, prev_held: b, slow_phase: 0.0, slow_held: c, slow_prev: d }
    }

    /// Forget the phase history (keeps the random sequence going).
    pub fn reset(&mut self) {
        self.last = 0.0;
        self.slow_phase = 0.0;
    }

    /// Value at `phase` (any real number, taken modulo 1). `shape` (0..1)
    /// bends the wave: flattening for Sine/Triangle/random shapes, break
    /// point for the saws, pulse width for Square, curvature for Shark Tooth.
    #[inline]
    pub fn value(&mut self, wave: Waveform, phase: f64, shape: f32) -> f32 {
        let phase = phase.rem_euclid(1.0);
        let delta = (phase - self.last).rem_euclid(1.0);
        // A jump backwards (transport relocation) is not a cycle.
        if delta <= 0.5 {
            if phase < self.last {
                self.prev_held = self.held;
                self.held = self.noise.next();
            }
            self.slow_phase += delta / 3.0;
            if self.slow_phase >= 1.0 {
                self.slow_phase -= 1.0;
                self.slow_prev = self.slow_held;
                self.slow_held = self.noise.next();
            }
        }
        self.last = phase;
        let p = phase as f32;
        let shape = shape.clamp(0.0, 1.0);
        let squash = |x: f32| {
            if shape <= 0.0 {
                x
            } else {
                let g = 0.01 + 8.0 * shape;
                (g * x).tanh() / g.tanh()
            }
        };
        match wave {
            Waveform::Sine => squash((p * std::f32::consts::TAU).sin()),
            Waveform::Triangle => squash(1.0 - 4.0 * (p - 0.5).abs()),
            Waveform::SharkTooth => 1.0 - 2.0 * (1.0 - p).powf(1.7 + 3.0 * shape),
            Waveform::SawUp => skewed_triangle(p, 1.0 - 0.5 * shape),
            Waveform::SawDown => skewed_triangle(p, 0.5 * shape),
            Waveform::Square => {
                // Pulse width 50 % down to 10 %.
                if p < 0.5 - 0.4 * shape {
                    1.0
                } else {
                    -1.0
                }
            }
            Waveform::Random => {
                let t = 0.5 - 0.5 * (p * std::f32::consts::PI).cos();
                squash(self.prev_held + (self.held - self.prev_held) * t)
            }
            Waveform::Wander => {
                let t = 0.5 - 0.5 * (p * std::f32::consts::PI).cos();
                let fast = self.prev_held + (self.held - self.prev_held) * t;
                let ts = 0.5 - 0.5 * (self.slow_phase as f32 * std::f32::consts::PI).cos();
                let slow = self.slow_prev + (self.slow_held - self.slow_prev) * ts;
                squash(0.6 * slow + 0.4 * fast)
            }
            Waveform::SampleHold => squash(self.held),
        }
    }
}

/// Triangle rising over `[0, b)` and falling over `[b, 1)`, −1..1; `b = 1`
/// is a rising saw and `b = 0` a falling one.
#[inline]
fn skewed_triangle(p: f32, b: f32) -> f32 {
    if p < b {
        -1.0 + 2.0 * p / b.max(1e-6)
    } else {
        1.0 - 2.0 * (p - b) / (1.0 - b).max(1e-6)
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

    #[test]
    fn phase_lfo_shapes_are_bounded_and_hit_the_extremes() {
        for wave in Waveform::ALL {
            for shape in [0.0f32, 0.3, 1.0] {
                let mut l = PhaseLfo::new(7);
                let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                let mut ph = 0.0f64;
                for _ in 0..40_000 {
                    ph = (ph + 0.0013).rem_euclid(1.0);
                    let v = l.value(wave, ph, shape);
                    assert!((-1.0..=1.0).contains(&v) && v.is_finite(), "{wave:?} {shape} {v}");
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
                if !wave.is_random() {
                    assert!(lo < -0.9 && hi > 0.9, "{wave:?} {shape}: {lo}..{hi}");
                }
            }
        }
    }

    #[test]
    fn phase_lfo_known_points() {
        let mut l = PhaseLfo::new(1);
        let mut at = |w, p| l.value(w, p, 0.0);
        assert!(at(Waveform::Sine, 0.0).abs() < 1e-6);
        assert!((at(Waveform::Sine, 0.25) - 1.0).abs() < 1e-6);
        assert!((at(Waveform::Triangle, 0.5) - 1.0).abs() < 1e-6);
        assert!((at(Waveform::SawUp, 0.0) + 1.0).abs() < 1e-5);
        assert!((at(Waveform::SawDown, 0.0) - 1.0).abs() < 1e-5);
        assert!((at(Waveform::SharkTooth, 0.0) + 1.0).abs() < 1e-6);
        assert!(at(Waveform::SharkTooth, 0.999) > 0.99);
        assert_eq!(at(Waveform::Square, 0.1), 1.0);
        assert_eq!(at(Waveform::Square, 0.9), -1.0);
    }

    #[test]
    fn phase_lfo_shape_bends_the_wave() {
        let mut l = PhaseLfo::new(1);
        // Flattening: a sine at 45 degrees reads higher with Shape.
        let plain = l.value(Waveform::Sine, 0.125, 0.0);
        let flat = l.value(Waveform::Sine, 0.125, 1.0);
        assert!(flat > plain + 0.1);
        // Pulse width.
        assert_eq!(l.value(Waveform::Square, 0.3, 0.0), 1.0);
        assert_eq!(l.value(Waveform::Square, 0.3, 1.0), -1.0);
    }

    #[test]
    fn phase_lfo_random_changes_once_per_cycle() {
        let mut l = PhaseLfo::new(3);
        let mut ph = 0.0f64;
        let mut values = Vec::new();
        for i in 0..4000 {
            ph = (ph + 0.001).rem_euclid(1.0);
            let v = l.value(Waveform::SampleHold, ph, 0.0);
            if i % 1000 == 500 {
                values.push(v);
            }
            // Constant inside a cycle.
            if i % 1000 == 700 {
                assert_eq!(v, *values.last().unwrap());
            }
        }
        assert!(values.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn phase_lfo_wander_is_slower_than_random() {
        // The f/3 companion makes Wander's cycle-to-cycle steps smaller
        // than the plain smooth random on average.
        let step = |w: Waveform| {
            let mut l = PhaseLfo::new(11);
            let mut ph = 0.0f64;
            let mut prev = 0.0f32;
            let mut acc = 0.0f64;
            for i in 0..200_000 {
                ph = (ph + 0.0005).rem_euclid(1.0);
                let v = l.value(w, ph, 0.0);
                if i > 0 && i % 2000 == 0 {
                    acc += (v - prev).abs() as f64;
                }
                if i % 2000 == 0 {
                    prev = v;
                }
            }
            acc
        };
        assert!(step(Waveform::Wander) < step(Waveform::Random));
    }
}
