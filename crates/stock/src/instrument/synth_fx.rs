//! The wavetable synth's small built-in effects, in order: distortion,
//! 3-band EQ, stereo chorus, ping-pong delay and a compact 4-line FDN
//! reverb. Cheap by design — the full-size effects live in `fx::*`.

use aitrack_dsp::delay::{AllpassDiffuser, DelayLine};
use aitrack_dsp::shaper::Bitcrusher;
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::{OnePole, SvfCoeffs, SvfMode, SvfState};

/// Distortion modes (`dist_mode`).
pub const DIST_MODES: &[&str] = &["Off", "Soft", "Hard", "Fold", "Tube", "Bitcrush"];

/// Longest delay the ping-pong line holds.
const MAX_DELAY_S: f64 = 2.5;
const FDN_MS: [f32; 4] = [29.7, 37.1, 41.3, 47.9];

#[derive(Clone, Copy, Debug)]
pub struct FxSettings {
    /// Index into [`DIST_MODES`] (0 = off).
    pub dist_mode: usize,
    /// 0..1.
    pub dist_drive: f32,
    /// 0..1.
    pub dist_mix: f32,
    /// Shelf / bell gains in dB (0 = bypass).
    pub eq_low: f32,
    pub eq_mid: f32,
    pub eq_mid_freq: f32,
    pub eq_high: f32,
    pub chorus_mix: f32,
    /// Hz.
    pub chorus_rate: f32,
    /// Seconds of modulation depth.
    pub chorus_depth: f32,
    pub delay_mix: f32,
    pub delay_samples: f32,
    pub delay_feedback: f32,
    pub reverb_mix: f32,
    /// Reverb RT60 in seconds.
    pub reverb_decay: f32,
    /// Reverb damping low-pass (Hz).
    pub reverb_tone: f32,
}

impl Default for FxSettings {
    fn default() -> Self {
        Self {
            dist_mode: 0,
            dist_drive: 0.0,
            dist_mix: 1.0,
            eq_low: 0.0,
            eq_mid: 0.0,
            eq_mid_freq: 1000.0,
            eq_high: 0.0,
            chorus_mix: 0.0,
            chorus_rate: 0.6,
            chorus_depth: 0.0025,
            delay_mix: 0.0,
            delay_samples: 0.0,
            delay_feedback: 0.0,
            reverb_mix: 0.0,
            reverb_decay: 2.0,
            reverb_tone: 5500.0,
        }
    }
}

impl FxSettings {
    /// True when any stage changes the signal.
    pub fn active(&self) -> bool {
        self.dist_mode != 0 || self.eq_on() || self.chorus_mix > 0.0 || self.delay_mix > 0.0 || self.reverb_mix > 0.0
    }

    fn eq_on(&self) -> bool {
        self.eq_low.abs() > 0.01 || self.eq_mid.abs() > 0.01 || self.eq_high.abs() > 0.01
    }
}

pub struct SynthFx {
    sr: f64,
    chorus: [DelayLine; 2],
    chorus_phase: f32,
    delay: [DelayLine; 2],
    delay_lp: [OnePole; 2],
    delay_time: Smoother,
    fdn: [DelayLine; 4],
    fdn_len: [usize; 4],
    fdn_lp: [OnePole; 4],
    diff: [AllpassDiffuser; 2],
    fdn_gain: [f32; 4],
    last_decay: f32,
    last_tone: f32,
    crush: [Bitcrusher; 2],
    eq: [[SvfState; 2]; 3],
    eq_coeffs: [SvfCoeffs; 3],
    last_eq: [f32; 4],
}

impl SynthFx {
    pub fn new(sr: f64) -> Self {
        let fdn_len = FDN_MS.map(|ms| (ms as f64 * 0.001 * sr) as usize);
        let mut fx = Self {
            sr,
            chorus: [DelayLine::new((0.04 * sr) as usize), DelayLine::new((0.04 * sr) as usize)],
            chorus_phase: 0.0,
            delay: [DelayLine::new((MAX_DELAY_S * sr) as usize), DelayLine::new((MAX_DELAY_S * sr) as usize)],
            delay_lp: [OnePole::new(6000.0, sr); 2],
            delay_time: Smoother::new(0.0),
            fdn: fdn_len.map(DelayLine::new),
            fdn_len,
            fdn_lp: [OnePole::new(5500.0, sr); 4],
            diff: [AllpassDiffuser::new((0.0047 * sr) as usize), AllpassDiffuser::new((0.0071 * sr) as usize)],
            fdn_gain: [0.0; 4],
            last_decay: -1.0,
            last_tone: 5500.0,
            crush: [Bitcrusher::default(), Bitcrusher::default()],
            eq: [[SvfState::default(); 2]; 3],
            eq_coeffs: [SvfCoeffs::passthrough(); 3],
            last_eq: [f32::NAN; 4],
        };
        fx.delay_time.set_time(60.0, sr);
        fx
    }

    pub fn reset(&mut self) {
        for d in self.chorus.iter_mut().chain(self.delay.iter_mut()).chain(self.fdn.iter_mut()) {
            d.clear();
        }
        for f in self.delay_lp.iter_mut().chain(self.fdn_lp.iter_mut()) {
            f.reset();
        }
        for d in &mut self.diff {
            d.clear();
        }
        for c in &mut self.crush {
            c.reset();
        }
        self.eq = [[SvfState::default(); 2]; 3];
        self.delay_time.snap_to_target();
    }

    #[inline]
    fn distort(&mut self, ch: usize, x: f32, s: &FxSettings) -> f32 {
        let d = s.dist_drive.clamp(0.0, 1.0);
        let g = 1.0 + 24.0 * d * d;
        let y = match s.dist_mode {
            1 => (x * g).tanh() * g.powf(-0.35),
            2 => (x * g).clamp(-1.0, 1.0) * g.powf(-0.45),
            3 => {
                // Triangle wavefold.
                let v = (x * g * 0.5 + 1.0).rem_euclid(4.0);
                (if v < 2.0 { v - 1.0 } else { 3.0 - v }) * 0.8
            }
            4 => {
                // Asymmetric "tube" curve: even harmonics.
                let v = x * g + 0.25 * d;
                (v.tanh() - (0.25 * d).tanh()) * g.powf(-0.35)
            }
            5 => {
                let bits = 16.0 - 13.0 * d;
                let rate = 1.0 - 0.9 * d;
                self.crush[ch].process(x, bits, rate)
            }
            _ => x,
        };
        x + (y - x) * s.dist_mix.clamp(0.0, 1.0)
    }

    fn update_eq(&mut self, s: &FxSettings) {
        let key = [s.eq_low, s.eq_mid, s.eq_mid_freq, s.eq_high];
        if key == self.last_eq {
            return;
        }
        self.last_eq = key;
        let sr = self.sr;
        self.eq_coeffs[0] = SvfCoeffs::new(SvfMode::LowShelf, 200.0, std::f32::consts::FRAC_1_SQRT_2, s.eq_low, sr);
        self.eq_coeffs[1] = SvfCoeffs::new(SvfMode::Bell, s.eq_mid_freq.clamp(40.0, 16_000.0), 0.9, s.eq_mid, sr);
        self.eq_coeffs[2] = SvfCoeffs::new(SvfMode::HighShelf, 6000.0, std::f32::consts::FRAC_1_SQRT_2, s.eq_high, sr);
    }

    pub fn max_delay_samples(&self) -> f32 {
        (MAX_DELAY_S * self.sr) as f32 - 4.0
    }

    /// Process one stereo frame.
    #[inline]
    pub fn process(&mut self, mut l: f32, mut r: f32, s: &FxSettings) -> (f32, f32) {
        if s.dist_mode != 0 {
            l = self.distort(0, l, s);
            r = self.distort(1, r, s);
        }
        if s.eq_on() {
            self.update_eq(s);
            for b in 0..3 {
                let c = self.eq_coeffs[b];
                l = self.eq[b][0].process(&c, l);
                r = self.eq[b][1].process(&c, r);
            }
        }
        if s.chorus_mix > 0.0 {
            // Two voices, quadrature LFOs (0.6 Hz, 7–12 ms by default).
            self.chorus_phase += s.chorus_rate.clamp(0.01, 20.0) / self.sr as f32;
            if self.chorus_phase >= 1.0 {
                self.chorus_phase -= 1.0;
            }
            let ph = self.chorus_phase * std::f32::consts::TAU;
            let base = 0.0095 * self.sr as f32;
            let depth = s.chorus_depth.clamp(0.0, 0.009) * self.sr as f32;
            self.chorus[0].push(l);
            self.chorus[1].push(r);
            let wl = self.chorus[0].read_cubic(base + depth * ph.sin());
            let wr = self.chorus[1].read_cubic(base + depth * ph.cos());
            let m = s.chorus_mix;
            l = l * (1.0 - 0.5 * m) + wl * m * 0.7;
            r = r * (1.0 - 0.5 * m) + wr * m * 0.7;
        }
        if s.delay_mix > 0.0 {
            self.delay_time.set_target(s.delay_samples.clamp(1.0, self.max_delay_samples()));
            let d = self.delay_time.next();
            let ol = self.delay[0].read_linear(d);
            let or = self.delay[1].read_linear(d);
            let fb = s.delay_feedback.clamp(0.0, 0.95);
            // Ping-pong: the mono input enters left, echoes cross sides.
            let inp = 0.5 * (l + r);
            let nl = inp + fb * self.delay_lp[0].lowpass(or);
            let nr = fb * self.delay_lp[1].lowpass(ol);
            self.delay[0].push(nl.clamp(-4.0, 4.0));
            self.delay[1].push(nr.clamp(-4.0, 4.0));
            l += ol * s.delay_mix;
            r += or * s.delay_mix;
        } else {
            self.delay_time.snap(s.delay_samples.clamp(1.0, self.max_delay_samples()));
        }
        if s.reverb_mix > 0.0 {
            if s.reverb_tone != self.last_tone {
                self.last_tone = s.reverb_tone;
                for f in &mut self.fdn_lp {
                    f.set_freq(s.reverb_tone.clamp(500.0, 20_000.0), self.sr);
                }
            }
            if s.reverb_decay != self.last_decay {
                self.last_decay = s.reverb_decay;
                let t = s.reverb_decay.max(0.1);
                for i in 0..4 {
                    self.fdn_gain[i] = 10f32.powf(-3.0 * self.fdn_len[i] as f32 / (t * self.sr as f32));
                }
            }
            let d0 = self.diff[0].process(0.5 * (l + r), 0.6);
            let inp = self.diff[1].process(d0, 0.6) * 0.5;
            let y: [f32; 4] = std::array::from_fn(|i| self.fdn[i].read(self.fdn_len[i] - 1));
            // Householder feedback: x − ½ Σx.
            let half = 0.5 * (y[0] + y[1] + y[2] + y[3]);
            let signs = [1.0, -1.0, 1.0, -1.0];
            for i in 0..4 {
                let fbv = (y[i] - half) * self.fdn_gain[i];
                let v = self.fdn_lp[i].lowpass(fbv) + inp * signs[i];
                self.fdn[i].push(if v.is_finite() { v } else { 0.0 });
            }
            l += (y[0] + y[2]) * s.reverb_mix * 0.6;
            r += (y[1] + y[3]) * s.reverb_mix * 0.6;
        }
        (l, r)
    }
}
