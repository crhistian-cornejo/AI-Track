//! Envelope followers and the ADSR envelope generator.

use crate::smooth::one_pole_coeff;

/// What the follower measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectorMode {
    /// Rectified peak.
    Peak,
    /// Windowed RMS, then the attack/release smoother.
    Rms,
}

/// Attack/release envelope follower ("branching" one-pole: attack
/// coefficient while rising, release while falling).
#[derive(Clone, Copy, Debug)]
pub struct EnvelopeFollower {
    mode: DetectorMode,
    attack: f32,
    release: f32,
    rms_coeff: f32,
    ms: f32,
    env: f32,
}

impl EnvelopeFollower {
    pub fn new(mode: DetectorMode) -> Self {
        Self { mode, attack: 1.0, release: 1.0, rms_coeff: 0.01, ms: 0.0, env: 0.0 }
    }

    /// Averaging window of the RMS mode (default ≈ 10 ms at 48 kHz).
    pub fn set_rms_window(&mut self, ms: f32, sample_rate: f64) {
        self.rms_coeff = one_pole_coeff(ms, sample_rate);
    }

    pub fn set_mode(&mut self, mode: DetectorMode) {
        self.mode = mode;
    }

    pub fn set_times(&mut self, attack_ms: f32, release_ms: f32, sample_rate: f64) {
        self.attack = one_pole_coeff(attack_ms, sample_rate);
        self.release = one_pole_coeff(release_ms, sample_rate);
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let v = match self.mode {
            DetectorMode::Peak => x.abs(),
            DetectorMode::Rms => {
                self.ms += (x * x - self.ms) * self.rms_coeff;
                if self.ms < 1e-30 {
                    self.ms = 0.0;
                }
                self.ms.sqrt()
            }
        };
        let c = if v > self.env { self.attack } else { self.release };
        self.env += (v - self.env) * c;
        if self.env < 1e-30 {
            self.env = 0.0;
        }
        self.value()
    }

    #[inline]
    pub fn value(&self) -> f32 {
        self.env
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.ms = 0.0;
    }
}

/// Smoothed gain-reduction detector in the log domain (Giannoulis, Massberg
/// & Reiss 2012, "smooth decoupled"): the release runs on a peak-held state,
/// the attack smooths that state, so attack and release do not interact.
/// Input and output are gain reduction in dB (≥ 0).
#[derive(Clone, Copy, Debug)]
pub struct GainSmoother {
    attack: f32,
    release: f32,
    y1: f32,
    yl: f32,
}

impl Default for GainSmoother {
    fn default() -> Self {
        Self { attack: 1.0, release: 1.0, y1: 0.0, yl: 0.0 }
    }
}

impl GainSmoother {
    pub fn set_times(&mut self, attack_ms: f32, release_ms: f32, sample_rate: f64) {
        self.attack = one_pole_coeff(attack_ms, sample_rate);
        self.release = one_pole_coeff(release_ms, sample_rate);
    }

    pub fn set_release_coeff(&mut self, c: f32) {
        self.release = c;
    }

    /// Smooth decoupled: `y1 = max(x, y1 - rel (y1 - x))`, `yl += att (y1 - yl)`.
    #[inline]
    pub fn process(&mut self, gr_db: f32) -> f32 {
        self.y1 = gr_db.max(self.y1 - self.release * (self.y1 - gr_db));
        self.yl += self.attack * (self.y1 - self.yl);
        if self.yl < 1e-9 {
            self.yl = 0.0;
        }
        self.yl
    }

    /// Branching variant: attack while the reduction grows, release while it
    /// shrinks.
    #[inline]
    pub fn process_branching(&mut self, gr_db: f32) -> f32 {
        let c = if gr_db > self.yl { self.attack } else { self.release };
        self.yl += c * (gr_db - self.yl);
        self.y1 = self.yl;
        if self.yl < 1e-9 {
            self.yl = 0.0;
        }
        self.yl
    }

    pub fn value(&self) -> f32 {
        self.yl
    }

    pub fn reset(&mut self) {
        self.y1 = 0.0;
        self.yl = 0.0;
    }
}

/// Static gain computer: gain reduction in dB (≥ 0) for an input level in
/// dB, with a quadratic soft knee of width `knee_db`.
#[inline]
pub fn gain_computer(level_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    let over = level_db - threshold_db;
    if knee_db > 0.0 && 2.0 * over.abs() <= knee_db {
        let t = over + knee_db * 0.5;
        slope * t * t / (2.0 * knee_db)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

/// Envelope stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdsrStage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Linear-attack, exponential decay/release ADSR (analogue-style curves).
#[derive(Clone, Copy, Debug)]
pub struct Adsr {
    stage: AdsrStage,
    value: f32,
    attack_step: f32,
    decay_coeff: f32,
    sustain: f32,
    release_coeff: f32,
    sample_rate: f64,
}

/// Exponential segments aim past their target by this much so they end in
/// finite time.
const OVERSHOOT: f32 = 0.001;

impl Adsr {
    pub fn new(sample_rate: f64) -> Self {
        let mut a = Self {
            stage: AdsrStage::Idle,
            value: 0.0,
            attack_step: 1.0,
            decay_coeff: 1.0,
            sustain: 1.0,
            release_coeff: 1.0,
            sample_rate,
        };
        a.set(5.0, 100.0, 0.8, 200.0);
        a
    }

    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
    }

    /// Times in milliseconds, sustain 0..=1.
    pub fn set(&mut self, attack_ms: f32, decay_ms: f32, sustain: f32, release_ms: f32) {
        let sr = self.sample_rate as f32;
        self.attack_step = 1.0 / (attack_ms.max(0.0) * 0.001 * sr).max(1.0);
        // Reach the target (within OVERSHOOT) after the stated time.
        let k = |ms: f32| {
            let n = (ms.max(0.0) * 0.001 * sr).max(1.0);
            1.0 - (OVERSHOOT / (1.0 + OVERSHOOT)).powf(1.0 / n)
        };
        self.decay_coeff = k(decay_ms);
        self.release_coeff = k(release_ms);
        self.sustain = sustain.clamp(0.0, 1.0);
    }

    pub fn gate_on(&mut self) {
        self.stage = AdsrStage::Attack;
    }

    /// Restart from zero (no legato).
    pub fn retrigger(&mut self) {
        self.value = 0.0;
        self.stage = AdsrStage::Attack;
    }

    pub fn gate_off(&mut self) {
        if self.stage != AdsrStage::Idle {
            self.stage = AdsrStage::Release;
        }
    }

    pub fn reset(&mut self) {
        self.stage = AdsrStage::Idle;
        self.value = 0.0;
    }

    pub fn stage(&self) -> AdsrStage {
        self.stage
    }

    pub fn is_active(&self) -> bool {
        self.stage != AdsrStage::Idle
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        match self.stage {
            AdsrStage::Idle => {}
            AdsrStage::Attack => {
                self.value += self.attack_step;
                if self.value >= 1.0 {
                    self.value = 1.0;
                    self.stage = AdsrStage::Decay;
                }
            }
            AdsrStage::Decay => {
                let target = self.sustain - OVERSHOOT;
                self.value += (target - self.value) * self.decay_coeff;
                if self.value <= self.sustain {
                    self.value = self.sustain;
                    self.stage = AdsrStage::Sustain;
                }
            }
            AdsrStage::Sustain => {
                self.value = self.sustain;
            }
            AdsrStage::Release => {
                self.value += (-OVERSHOOT - self.value) * self.release_coeff;
                if self.value <= 0.0 {
                    self.value = 0.0;
                    self.stage = AdsrStage::Idle;
                }
            }
        }
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knee_is_continuous() {
        let thr = -20.0;
        let ratio = 4.0;
        let knee = 6.0;
        let below = gain_computer(-23.0, thr, ratio, knee);
        let above = gain_computer(-17.0, thr, ratio, knee);
        assert!(below.abs() < 1e-6);
        assert!((above - 3.0 * 0.75).abs() < 1e-5);
        assert!((gain_computer(-10.0, thr, ratio, knee) - 7.5).abs() < 1e-5);
        assert_eq!(gain_computer(-10.0, thr, 1.0, knee), 0.0);
    }

    #[test]
    fn adsr_timing() {
        let sr = 48_000.0;
        let mut env = Adsr::new(sr);
        env.set(10.0, 50.0, 0.5, 100.0);
        env.gate_on();
        let mut n = 0;
        while env.stage() == AdsrStage::Attack {
            env.next();
            n += 1;
        }
        assert!((n - 480i32).abs() <= 1);
        let mut n = 0;
        while env.stage() == AdsrStage::Decay {
            env.next();
            n += 1;
        }
        assert!(n > 2000 && n < 2500, "{n}");
        env.gate_off();
        let mut n = 0;
        while env.is_active() {
            env.next();
            n += 1;
        }
        // Release from 0.5 is a bit shorter than the full-scale time.
        assert!(n > 3000 && n < 5000, "{n}");
    }

    #[test]
    fn follower_tracks_rms() {
        let mut f = EnvelopeFollower::new(DetectorMode::Rms);
        f.set_times(5.0, 50.0, 48_000.0);
        f.set_rms_window(10.0, 48_000.0);
        let mut v = 0.0;
        for n in 0..48_000 {
            v = f.process((n as f32 * 0.05).sin());
        }
        assert!((v - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.03, "{v}");
    }

    #[test]
    fn smooth_decoupled_rises_and_falls() {
        let mut g = GainSmoother::default();
        g.set_times(1.0, 100.0, 48_000.0);
        for _ in 0..4800 {
            g.process(10.0);
        }
        assert!((g.value() - 10.0).abs() < 0.01);
        for _ in 0..4800 {
            g.process(0.0);
        }
        assert!(g.value() < 4.0 && g.value() > 0.0);
    }
}
