//! The wavetable synth's small built-in effects: stereo chorus, ping-pong
//! delay and a compact 4-line FDN reverb. Cheap by design — the full-size
//! effects live in `fx::*`.

use aitrack_dsp::delay::{AllpassDiffuser, DelayLine};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::OnePole;

/// Longest delay the ping-pong line holds.
const MAX_DELAY_S: f64 = 2.5;
const FDN_MS: [f32; 4] = [29.7, 37.1, 41.3, 47.9];

#[derive(Clone, Copy, Debug, Default)]
pub struct FxSettings {
    pub chorus_mix: f32,
    pub delay_mix: f32,
    pub delay_samples: f32,
    pub delay_feedback: f32,
    pub reverb_mix: f32,
    /// Reverb RT60 in seconds.
    pub reverb_decay: f32,
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
        self.delay_time.snap_to_target();
    }

    pub fn max_delay_samples(&self) -> f32 {
        (MAX_DELAY_S * self.sr) as f32 - 4.0
    }

    /// Process one stereo frame.
    #[inline]
    pub fn process(&mut self, mut l: f32, mut r: f32, s: &FxSettings) -> (f32, f32) {
        if s.chorus_mix > 0.0 {
            // Two voices, quadrature LFOs at 0.6 Hz, 7–12 ms.
            self.chorus_phase += 0.6 / self.sr as f32;
            if self.chorus_phase >= 1.0 {
                self.chorus_phase -= 1.0;
            }
            let ph = self.chorus_phase * std::f32::consts::TAU;
            let base = 0.0095 * self.sr as f32;
            let depth = 0.0025 * self.sr as f32;
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
