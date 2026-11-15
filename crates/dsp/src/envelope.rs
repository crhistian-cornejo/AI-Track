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

/// Peak follower with instant attack, a hold time and an exponential
/// release, fed with rectified samples. The hold bridges the gaps between
/// the peaks of a periodic signal, so a steady tone reads as its amplitude
/// with no ripple, while a transient still lets go after the release.
#[derive(Clone, Copy, Debug)]
pub struct PeakHold {
    env: f32,
    hold_left: u32,
    hold: u32,
    release: f32,
}

impl Default for PeakHold {
    fn default() -> Self {
        Self::new()
    }
}

impl PeakHold {
    pub fn new() -> Self {
        Self { env: 0.0, hold_left: 0, hold: 0, release: 1.0 }
    }

    /// Hold in milliseconds, then a release with the given time constant.
    pub fn set(&mut self, hold_ms: f32, release_ms: f32, sample_rate: f64) {
        self.hold = (hold_ms as f64 * 0.001 * sample_rate).round() as u32;
        self.release = one_pole_coeff(release_ms, sample_rate);
    }

    /// `x` is a magnitude (≥ 0).
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        if x >= self.env {
            self.env = x;
            self.hold_left = self.hold;
        } else if self.hold_left > 0 {
            self.hold_left -= 1;
        } else {
            self.env += (x - self.env) * self.release;
            if self.env < 1e-30 {
                self.env = 0.0;
            }
        }
        self.env
    }

    pub fn value(&self) -> f32 {
        self.env
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.hold_left = 0;
    }
}

/// Gain-reduction smoother of a bus compressor: branching (attack while the
/// reduction grows, release while it shrinks), with a fixed release or an
/// **automatic** one. The automatic release is program dependent: the
/// reduction feeds two cascaded poles (the slow one fed by the fast one) and
/// the release time runs from `fast_ms` to `slow_ms` as that memory goes
/// from 0 to `depth_db`, so a brief hit lets go quickly while a sustained
/// reduction recovers slowly and does not pump on bass.
/// Input and output are gain reduction in dB (≥ 0).
#[derive(Clone, Copy, Debug)]
pub struct AutoReleaseSmoother {
    attack: f32,
    release: f32,
    auto: bool,
    fast_ms: f32,
    slow_ms: f32,
    depth_db: f32,
    sample_rate: f64,
    y: f32,
    m1: f32,
    m2: f32,
    c1: f32,
    c2: f32,
    tick: u32,
}

/// How often (samples) the automatic release time is recomputed.
const AUTO_RELEASE_STRIDE: u32 = 16;

impl Default for AutoReleaseSmoother {
    fn default() -> Self {
        Self {
            attack: 1.0,
            release: 1.0,
            auto: false,
            fast_ms: 100.0,
            slow_ms: 1_200.0,
            depth_db: 10.0,
            sample_rate: 48_000.0,
            y: 0.0,
            m1: 0.0,
            m2: 0.0,
            c1: 0.0,
            c2: 0.0,
            tick: 0,
        }
    }
}

impl AutoReleaseSmoother {
    pub fn set_attack_ms(&mut self, ms: f32, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.attack = one_pole_coeff(ms, sample_rate);
    }

    /// Fixed release (time constant, ms).
    pub fn set_release_ms(&mut self, ms: f32, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.auto = false;
        self.release = one_pole_coeff(ms, sample_rate);
    }

    /// Automatic release between `fast_ms` and `slow_ms`, reaching the slow
    /// end when the reduction memory is `depth_db` deep.
    pub fn set_auto(&mut self, fast_ms: f32, slow_ms: f32, depth_db: f32, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.auto = true;
        self.fast_ms = fast_ms;
        self.slow_ms = slow_ms.max(fast_ms);
        self.depth_db = depth_db.max(0.1);
        self.c1 = one_pole_coeff(150.0, sample_rate);
        self.c2 = one_pole_coeff(600.0, sample_rate);
        self.release = one_pole_coeff(self.current_release_ms(), sample_rate);
    }

    /// Release time (ms) in effect now (automatic mode follows the memory).
    pub fn current_release_ms(&self) -> f32 {
        let t = (self.m2 / self.depth_db).clamp(0.0, 1.0);
        self.fast_ms + (self.slow_ms - self.fast_ms) * t
    }

    pub fn is_auto(&self) -> bool {
        self.auto
    }

    #[inline]
    pub fn process(&mut self, gr_db: f32) -> f32 {
        let c = if gr_db > self.y { self.attack } else { self.release };
        self.y += (gr_db - self.y) * c;
        if self.y < 1e-9 {
            self.y = 0.0;
        }
        if self.auto {
            self.m1 += (self.y - self.m1) * self.c1;
            self.m2 += (self.m1 - self.m2) * self.c2;
            self.tick += 1;
            if self.tick >= AUTO_RELEASE_STRIDE {
                self.tick = 0;
                self.release = one_pole_coeff(self.current_release_ms(), self.sample_rate);
            }
        }
        self.y
    }

    pub fn value(&self) -> f32 {
        self.y
    }

    pub fn reset(&mut self) {
        self.y = 0.0;
        self.m1 = 0.0;
        self.m2 = 0.0;
        self.tick = 0;
        if self.auto {
            self.release = one_pole_coeff(self.fast_ms, self.sample_rate);
        }
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

    #[test]
    fn peak_hold_bridges_the_gaps_of_a_rectified_sine() {
        let sr = 48_000.0;
        let mut ph = PeakHold::new();
        ph.set(1.5, 5.0, sr);
        // 1 kHz: rectified peaks every 0.5 ms, well inside the 1.5 ms hold.
        let mut lo = f32::MAX;
        for i in 0..4_800 {
            let x = (std::f64::consts::TAU * 1000.0 * i as f64 / sr).sin() as f32;
            let v = ph.process(x.abs());
            if i > 480 {
                lo = lo.min(v);
            }
        }
        assert!(lo > 0.99, "{lo}");
        // After the signal stops it falls: 1.5 ms hold, then the release.
        let mut v = 0.0;
        for _ in 0..(sr as usize / 10) {
            v = ph.process(0.0);
        }
        assert!(v < 1e-3, "{v}");
    }

    #[test]
    fn auto_release_is_fast_for_hits_and_slow_for_sustained_reduction() {
        let sr = 48_000.0;
        // Time (s) the reduction takes to fall to a quarter after stopping.
        let fall_time = |hold_s: f32| {
            let mut s = AutoReleaseSmoother::default();
            s.set_attack_ms(1.0, sr);
            s.set_auto(100.0, 1_200.0, 10.0, sr);
            for _ in 0..(hold_s * sr as f32) as usize {
                s.process(10.0);
            }
            let start = s.value();
            let mut n = 0usize;
            while s.process(0.0) > start * 0.25 && n < 200_000 {
                n += 1;
            }
            n as f32 / sr as f32
        };
        let brief = fall_time(0.05);
        let long = fall_time(4.0);
        assert!(brief < 0.3, "brief hit released in {brief} s");
        assert!(long > 0.9, "sustained reduction released in {long} s");
        assert!(long > 4.0 * brief);
    }

    #[test]
    fn fixed_release_matches_its_time_constant() {
        let sr = 48_000.0;
        let mut s = AutoReleaseSmoother::default();
        s.set_attack_ms(0.01, sr);
        s.set_release_ms(200.0, sr);
        for _ in 0..48_000 {
            s.process(12.0);
        }
        assert!((s.value() - 12.0).abs() < 0.01);
        for _ in 0..9_600 {
            s.process(0.0);
        }
        // One time constant: 12 · e⁻¹.
        assert!((s.value() - 12.0 * (-1.0f32).exp()).abs() < 0.1, "{}", s.value());
    }
}
