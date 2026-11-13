//! Tape echo pieces: wow and flutter (speed variation of the transport) and
//! the record / playback colour of the loop.
//!
//! * **Wow** is the slow speed drift of a capstan / reel (≈ 0.3–1.5 Hz):
//!   a sine plus a smoothed random walk, so it never repeats exactly.
//! * **Flutter** is the fast part (≈ 4–15 Hz) from capstan and idler
//!   eccentricity: two incommensurate sines plus a little filtered noise.
//!
//! Both move the read position of the delay line, which is what a speed
//! change does to a playback head (pitch deviation `= dτ/dt`).
//!
//! The loop colour follows the record chain of a tape echo: pre-emphasis,
//! asymmetric saturation (even harmonics, like a biased head), de-emphasis
//! (so the highs saturate first), then the playback gap / spacing loss as a
//! low-pass that falls with Age (Zölzer, *DAFX* ch. 12; Pirkle, *Designing
//! Audio Effect Plugins*, ch. 15). The head bump sits on the wet output, out
//! of the loop, so it can never build up with feedback.

use aitrack_dsp::shaper::{Adaa1, ShaperKind};
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use std::f32::consts::TAU;

/// Peak read excursion at 100 % wow / flutter, ms.
pub const WOW_MS: f32 = 5.0;
pub const FLUTTER_MS: f32 = 0.3;
const WOW_HZ: f32 = 0.55;
const FLUTTER_HZ: [f32; 2] = [6.3, 9.7];

/// Wow + flutter generator for one channel.
#[derive(Clone, Copy, Debug)]
pub struct Transport {
    phase_wow: f32,
    phase_fl: [f32; 2],
    inc_wow: f32,
    inc_fl: [f32; 2],
    seed: u32,
    drift: f32,
    drift_target: f32,
    countdown: u32,
    hold: u32,
    smooth: f32,
    jitter: f32,
    jitter_coeff: f32,
}

impl Transport {
    pub fn new(seed: u32, phase: f32) -> Self {
        Self {
            phase_wow: phase,
            phase_fl: [phase * 0.37, phase * 0.71],
            inc_wow: 0.0,
            inc_fl: [0.0; 2],
            seed: seed.max(1),
            drift: 0.0,
            drift_target: 0.0,
            countdown: 0,
            hold: 1,
            smooth: 0.0,
            jitter: 0.0,
            jitter_coeff: 0.0,
        }
    }

    pub fn set_rate(&mut self, sr: f32) {
        self.inc_wow = WOW_HZ / sr;
        self.inc_fl = FLUTTER_HZ.map(|f| f / sr);
        // A new random drift target every ~0.7 s, followed by a ~0.3 s glide.
        self.hold = (0.7 * sr) as u32;
        self.smooth = 1.0 - (-1.0 / (0.3 * sr)).exp();
        self.jitter_coeff = 1.0 - (-TAU * 20.0 / sr).exp();
    }

    #[inline]
    fn rand(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed >> 8) as f32 / 8_388_608.0 - 1.0
    }

    /// `(wow, flutter)` in −1..1 for this sample.
    #[inline]
    pub fn next(&mut self) -> (f32, f32) {
        let wrap = |p: f32| if p >= 1.0 { p - 1.0 } else { p };
        self.phase_wow = wrap(self.phase_wow + self.inc_wow);
        self.phase_fl = [wrap(self.phase_fl[0] + self.inc_fl[0]), wrap(self.phase_fl[1] + self.inc_fl[1])];
        if self.countdown == 0 {
            self.countdown = self.hold;
            self.drift_target = self.rand();
        }
        self.countdown -= 1;
        self.drift += (self.drift_target - self.drift) * self.smooth;
        let noise = self.rand();
        self.jitter += (noise - self.jitter) * self.jitter_coeff;
        let wow = 0.65 * (TAU * self.phase_wow).sin() + 0.35 * self.drift;
        let fl = 0.55 * (TAU * self.phase_fl[0]).sin() + 0.3 * (TAU * self.phase_fl[1]).sin() + 0.15 * self.jitter.clamp(-1.0, 1.0);
        (wow, fl)
    }
}

/// Record / playback colour of one channel of the tape loop.
#[derive(Clone, Copy, Debug)]
pub struct TapeColor {
    pre: SvfState,
    de: SvfState,
    gap: SvfState,
    sat: Adaa1,
}

/// Shared coefficients of the tape colour.
#[derive(Clone, Copy, Debug, Default)]
pub struct TapeCoeffs {
    pub pre: SvfCoeffs,
    pub de: SvfCoeffs,
    pub gap: SvfCoeffs,
    pub bump: SvfCoeffs,
}

/// Playback bandwidth for an Age of `age` (0..1): 16 kHz new, 2.5 kHz worn.
pub fn gap_hz(age: f32) -> f32 {
    16_000.0 * (2_500.0f32 / 16_000.0).powf(age.clamp(0.0, 1.0))
}

impl TapeCoeffs {
    pub fn new(age: f32, sr: f64) -> Self {
        let nyq = (sr * 0.45) as f32;
        Self {
            pre: SvfCoeffs::new(SvfMode::HighShelf, 3_000.0, 0.707, 6.0, sr),
            de: SvfCoeffs::new(SvfMode::HighShelf, 3_000.0, 0.707, -6.0, sr),
            gap: SvfCoeffs::new(SvfMode::Lowpass, gap_hz(age).min(nyq), 0.6, 0.0, sr),
            bump: SvfCoeffs::new(SvfMode::Bell, 90.0, 1.0, 1.0 + 2.0 * age, sr),
        }
    }
}

/// Asymmetry of the record head's transfer curve.
const BIAS: f64 = 0.15;

impl Default for TapeColor {
    fn default() -> Self {
        Self::new()
    }
}

impl TapeColor {
    pub fn new() -> Self {
        let mut sat = Adaa1::new(ShaperKind::Asym);
        sat.set_bias(BIAS);
        Self { pre: SvfState::default(), de: SvfState::default(), gap: SvfState::default(), sat }
    }

    /// The loop's colour at `drive` (linear gain into the saturator).
    #[inline]
    pub fn process(&mut self, x: f32, c: &TapeCoeffs, drive: f32) -> f32 {
        // Slope of tanh(x + b) − tanh(b) at 0 is 1 − tanh²(b).
        const SLOPE: f32 = 0.977_7;
        let e = self.pre.process(&c.pre, x);
        let s = self.sat.process(e * drive) / (drive * SLOPE);
        let d = self.de.process(&c.de, s);
        self.gap.process(&c.gap, d)
    }

    pub fn reset(&mut self) {
        self.pre.reset();
        self.de.reset();
        self.gap.reset();
        self.sat.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wow_and_flutter_stay_in_range_and_move() {
        let mut t = Transport::new(3, 0.25);
        t.set_rate(48_000.0);
        let (mut lo, mut hi) = (0.0f32, 0.0f32);
        for _ in 0..96_000 {
            let (w, f) = t.next();
            assert!(w.abs() <= 1.0 && f.abs() <= 1.0);
            lo = lo.min(w);
            hi = hi.max(w);
        }
        assert!(hi - lo > 0.8, "wow swings: {lo}..{hi}");
    }

    #[test]
    fn age_darkens_and_the_colour_is_transparent_when_quiet() {
        assert!(gap_hz(0.0) > 15_000.0 && gap_hz(1.0) < 2_600.0);
        let sr = 48_000.0;
        let gain = |age: f32, freq: f32, amp: f32| {
            let c = TapeCoeffs::new(age, sr);
            let mut t = TapeColor::new();
            let y: Vec<f32> = (0..24_000).map(|i| t.process(amp * (TAU * freq * i as f32 / sr as f32).sin(), &c, 1.0)).collect();
            let tail = &y[12_000..];
            (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt() / (amp / 2f32.sqrt())
        };
        assert!((gain(0.0, 1_000.0, 0.01) - 1.0).abs() < 0.05, "quiet mids pass");
        assert!(gain(1.0, 8_000.0, 0.01) < 0.2, "a worn tape loses the highs");
        assert!(gain(0.0, 1_000.0, 2.0) < 0.8, "loud signals saturate");
    }
}
