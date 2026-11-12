//! 6-operator phase-modulation ("FM") synth.
//!
//! Each operator is a sine with its own ratio, level, ADSR, velocity
//! sensitivity and key scaling. An [`ALGORITHMS`] entry wires them: which
//! operators modulate which, which are carriers (heard), and which one
//! feeds back on itself. Modulators always have a higher index than what
//! they modulate, so a voice evaluates op 6 → op 1 in one pass.
//!
//! Levels follow a 0..99 scale of ≈0.75 dB a step; a modulator at 99
//! deviates its target's phase by up to 4π rad. Feedback averages the last
//! two outputs of the feedback operator (keeps high settings from
//! "hunting" into noise). The stock crate had no FM operator code to reuse
//! (the wavetable synth only has phase-FM as an oscillator warp), so this
//! is a new implementation.

use super::Engine;
use super::kit::{CONTROL, Perf, Pool, Voice, flush, key_hz, sin01};
use super::params::{Params, fm as fp};
use aitrack_dsp::envelope::Adsr;

pub const VOICES: usize = 16;
pub const OPS: usize = 6;

/// One algorithm: `mods[i]` = bitmask of the operators modulating op `i`,
/// `carriers` = bitmask of the operators summed to the output, `feedback`
/// = the self-modulating operator.
#[derive(Clone, Copy, Debug)]
pub struct Algorithm {
    pub mods: [u8; OPS],
    pub carriers: u8,
    pub feedback: usize,
}

const fn b(ops: &[usize]) -> u8 {
    let mut m = 0u8;
    let mut i = 0;
    while i < ops.len() {
        m |= 1 << ops[i];
        i += 1;
    }
    m
}

/// Operators are 0-based here (op 1 = 0). Names in [`super::FM_ALGORITHMS`].
pub const ALGORITHMS: [Algorithm; 12] = [
    // 6→5→4→3→2→1
    Algorithm { mods: [b(&[1]), b(&[2]), b(&[3]), b(&[4]), b(&[5]), 0], carriers: b(&[0]), feedback: 5 },
    // 3→2→1, 6→5→4
    Algorithm { mods: [b(&[1]), b(&[2]), 0, b(&[4]), b(&[5]), 0], carriers: b(&[0, 3]), feedback: 5 },
    // 2→1, 4→3, 6→5
    Algorithm { mods: [b(&[1]), 0, b(&[3]), 0, b(&[5]), 0], carriers: b(&[0, 2, 4]), feedback: 5 },
    // 4→3→2→1, 6→5
    Algorithm { mods: [b(&[1]), b(&[2]), b(&[3]), 0, b(&[5]), 0], carriers: b(&[0, 4]), feedback: 3 },
    // (2,3)→1, 6→5→4
    Algorithm { mods: [b(&[1, 2]), 0, 0, b(&[4]), b(&[5]), 0], carriers: b(&[0, 3]), feedback: 5 },
    // 2→1, 6→(3,4,5)
    Algorithm { mods: [b(&[1]), 0, b(&[5]), b(&[5]), b(&[5]), 0], carriers: b(&[0, 2, 3, 4]), feedback: 5 },
    // (2,3,4,5,6)→1
    Algorithm { mods: [b(&[1, 2, 3, 4, 5]), 0, 0, 0, 0, 0], carriers: b(&[0]), feedback: 5 },
    // 3→2→1, carriers 4 5 6
    Algorithm { mods: [b(&[1]), b(&[2]), 0, 0, 0, 0], carriers: b(&[0, 3, 4, 5]), feedback: 2 },
    // 2→1, carriers 3..6
    Algorithm { mods: [b(&[1]), 0, 0, 0, 0, 0], carriers: b(&[0, 2, 3, 4, 5]), feedback: 1 },
    // all carriers
    Algorithm { mods: [0; OPS], carriers: b(&[0, 1, 2, 3, 4, 5]), feedback: 5 },
    // 6→5→4→3→2, op 1 a plain sine
    Algorithm { mods: [0, b(&[2]), b(&[3]), b(&[4]), b(&[5]), 0], carriers: b(&[0, 1]), feedback: 5 },
    // 2→1, 4→3, carriers 5 6
    Algorithm { mods: [b(&[1]), 0, b(&[3]), 0, 0, 0], carriers: b(&[0, 2, 4, 5]), feedback: 3 },
];

/// Phase deviation of a full-level modulator, in cycles (4π rad).
const MOD_DEPTH: f32 = 2.0;

/// Level 0..99 → linear amplitude.
#[inline]
pub fn level_amp(level: f32) -> f32 {
    if level <= 0.0 { 0.0 } else { 2f32.powf((level - 99.0) / 8.0) }
}

/// Coarse index + fine % → frequency ratio.
#[inline]
pub fn ratio(coarse: usize, fine: f32) -> f32 {
    let c = if coarse == 0 { 0.5 } else { coarse as f32 };
    c * (1.0 + fine / 100.0)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OpSettings {
    ratio: f32,
    detune: f32,
    amp: f32,
    env: [f32; 4],
    vel: f32,
    key_scale: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    sr: f32,
    alg: Algorithm,
    feedback: f32,
    brightness: f32,
    lfo_rate: f32,
    vibrato: f32,
    tremolo: f32,
    pitch_env: f32,
    pitch_decay: f32,
    ops: [OpSettings; OPS],
    carrier_gain: f32,
    time: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sr: 48_000.0, alg: ALGORITHMS[0], feedback: 0.0, brightness: 1.0, lfo_rate: 5.0, vibrato: 0.0, tremolo: 0.0, pitch_env: 0.0, pitch_decay: 50.0, ops: [OpSettings::default(); OPS], carrier_gain: 1.0, time: 1.0 }
    }
}

pub struct FmVoice {
    phase: [f32; OPS],
    inc: [f32; OPS],
    env: [Adsr; OPS],
    amp: [f32; OPS],
    out: [f32; OPS],
    fb: [f32; 2],
    key: u8,
    vel: f32,
    lfo: f32,
    pitch_env: f32,
    pitch_coeff: f32,
    trem: f32,
    /// Carriers with a level: the voice lives while their envelopes run.
    carriers: u8,
}

impl FmVoice {
    fn new(sr: f64, seed: u32) -> Self {
        Self {
            phase: [0.0; OPS],
            inc: [0.0; OPS],
            env: [Adsr::new(sr); OPS],
            amp: [0.0; OPS],
            out: [0.0; OPS],
            fb: [0.0; 2],
            key: 60,
            vel: 1.0,
            lfo: (seed % 89) as f32 / 89.0,
            pitch_env: 0.0,
            pitch_coeff: 0.0,
            trem: 1.0,
            carriers: 0,
        }
    }

    fn set_envs(&mut self, s: &Settings) {
        for o in 0..OPS {
            let e = s.ops[o].env;
            self.env[o].set_sample_rate(s.sr as f64);
            self.env[o].set(e[0] * s.time, e[1] * s.time, e[2], e[3] * s.time);
        }
    }
}

impl Voice for FmVoice {
    type Settings = Settings;

    fn start(&mut self, key: u8, vel: f32, retrigger: bool, s: &Settings, _perf: &Perf) {
        self.key = key;
        self.vel = vel;
        self.set_envs(s);
        for o in 0..OPS {
            if !retrigger {
                self.phase[o] = 0.0;
                self.env[o].reset();
            }
            self.env[o].gate_on();
        }
        if !retrigger {
            self.fb = [0.0; 2];
        }
        self.carriers = (0..OPS).filter(|&o| s.alg.carriers & (1 << o) != 0 && s.ops[o].amp > 0.0).fold(0u8, |m, o| m | (1 << o));
        self.pitch_env = 1.0;
        self.pitch_coeff = (-(CONTROL as f32) / (s.pitch_decay.max(1.0) * 0.001 * s.sr)).exp();
    }

    fn release(&mut self, s: &Settings) {
        self.set_envs(s);
        for e in &mut self.env {
            e.gate_off();
        }
    }

    fn active(&self) -> bool {
        // Silent once every carrier envelope has finished.
        (0..OPS).any(|o| self.carriers & (1 << o) != 0 && self.env[o].is_active())
    }

    fn control(&mut self, s: &Settings, perf: &Perf) {
        self.lfo += s.lfo_rate * CONTROL as f32 / s.sr;
        self.lfo -= self.lfo.floor();
        let lfo = sin01(self.lfo);
        let semis = perf.bend + lfo * (s.vibrato / 100.0 + perf.mod_wheel * 0.5) + s.pitch_env * self.pitch_env;
        self.pitch_env *= self.pitch_coeff;
        let hz = key_hz(self.key as f32 + semis);
        let above = (self.key as f32 - 60.0).max(0.0) / 12.0;
        for o in 0..OPS {
            let op = &s.ops[o];
            self.inc[o] = (hz * op.ratio * 2f32.powf(op.detune / 1200.0) / s.sr).min(0.49);
            let vel = 1.0 - op.vel + op.vel * self.vel;
            let ks = 2f32.powf(-op.key_scale * above);
            let carrier = s.alg.carriers & (1 << o) != 0;
            let bright = if carrier { 1.0 } else { s.brightness };
            self.amp[o] = op.amp * vel * ks * bright;
        }
        self.trem = 1.0 - s.tremolo * 0.5 * (1.0 + lfo);
    }

    #[inline]
    fn tick(&mut self, s: &Settings) -> (f32, f32) {
        let alg = &s.alg;
        let mut sum = 0.0;
        for o in (0..OPS).rev() {
            let e = self.env[o].next();
            if self.amp[o] == 0.0 {
                self.out[o] = 0.0;
                self.phase[o] += self.inc[o];
                continue;
            }
            let mut m = 0.0;
            let mask = alg.mods[o];
            if mask != 0 {
                for j in (o + 1)..OPS {
                    if mask & (1 << j) != 0 {
                        m += self.out[j];
                    }
                }
            }
            if o == alg.feedback && s.feedback > 0.0 {
                m += s.feedback * 0.5 * (self.fb[0] + self.fb[1]) * 0.25;
            }
            let y = sin01(self.phase[o] + m * MOD_DEPTH) * e * self.amp[o];
            self.phase[o] += self.inc[o];
            if self.phase[o] >= 1.0 {
                self.phase[o] -= 1.0;
            }
            if o == alg.feedback {
                self.fb[1] = self.fb[0];
                self.fb[0] = y;
            }
            self.out[o] = y;
            if alg.carriers & (1 << o) != 0 {
                sum += y;
            }
        }
        let o = flush(sum * s.carrier_gain * self.trem * 0.25);
        (o, o)
    }

    fn kill(&mut self) {
        for e in &mut self.env {
            e.reset();
        }
        self.out = [0.0; OPS];
        self.fb = [0.0; 2];
    }
}

pub struct Fm {
    s: Settings,
    pool: Pool<FmVoice>,
}

impl Fm {
    pub fn new(sr: f64) -> Self {
        let voices = (0..VOICES as u32).map(|i| FmVoice::new(sr, i * 13 + 5)).collect();
        Self { s: Settings { sr: sr as f32, ..Default::default() }, pool: Pool::new(voices) }
    }
}

impl Engine for Fm {
    fn prepare(&mut self, sr: f64) {
        if sr as f32 != self.s.sr {
            *self = Self::new(sr);
        }
    }

    fn reset(&mut self) {
        self.pool.kill_all();
    }

    fn update(&mut self, p: &Params) {
        let s = &mut self.s;
        s.alg = ALGORITHMS[p.choice(fp::ALGORITHM).min(ALGORITHMS.len() - 1)];
        s.feedback = p.plain(fp::FEEDBACK) / 100.0 * 4.0;
        s.brightness = p.plain(fp::BRIGHTNESS) / 100.0;
        s.time = p.plain(fp::TIME) / 100.0;
        s.lfo_rate = p.plain(fp::LFO_RATE);
        s.vibrato = p.plain(fp::LFO_PITCH);
        s.tremolo = p.plain(fp::LFO_AMP) / 100.0;
        s.pitch_env = p.plain(fp::PITCH_ENV);
        s.pitch_decay = p.plain(fp::PITCH_DECAY);
        let mut carriers = 0;
        for o in 0..OPS {
            let id = |f| fp::op(o as u32, f);
            s.ops[o] = OpSettings {
                ratio: ratio(p.choice(id(fp::COARSE)), p.plain(id(fp::FINE))),
                detune: p.plain(id(fp::DETUNE)),
                amp: level_amp(p.plain(id(fp::LEVEL))),
                env: [p.plain(id(fp::ATTACK)), p.plain(id(fp::DECAY)), p.plain(id(fp::SUSTAIN)) / 100.0, p.plain(id(fp::RELEASE))],
                vel: p.plain(id(fp::VEL)) / 100.0,
                key_scale: p.plain(id(fp::KEY_SCALE)) / 100.0,
            };
            if s.alg.carriers & (1 << o) != 0 && s.ops[o].amp > 0.0 {
                carriers += 1;
            }
        }
        s.carrier_gain = 1.0 / (carriers.max(1) as f32).sqrt();
        self.pool.refresh();
    }

    fn note_on(&mut self, key: u8, vel: f32, perf: &Perf) {
        self.pool.note_on(key, vel, &self.s, perf);
    }

    fn note_off(&mut self, key: u8) {
        self.pool.note_off(key, &self.s);
    }

    fn release_all(&mut self) {
        self.pool.release_all(&self.s);
    }

    fn render(&mut self, perf: &Perf, l: &mut [f32], r: &mut [f32]) {
        self.pool.render(&self.s, perf, l, r);
    }

    fn active(&self) -> usize {
        self.pool.active()
    }

    fn tail_secs(&self) -> f32 {
        self.s.ops.iter().map(|o| o.env[3]).fold(0.0, f32::max) * self.s.time / 1000.0 + 0.05
    }

    fn held_keys(&self, out: &mut [u8]) -> usize {
        let mut n = 0;
        for k in self.pool.gated_keys() {
            if n < out.len() {
                out[n] = k;
                n += 1;
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modulators_come_after_their_targets() {
        for (a, alg) in ALGORITHMS.iter().enumerate() {
            for o in 0..OPS {
                for j in 0..OPS {
                    if alg.mods[o] & (1 << j) != 0 {
                        assert!(j > o, "algorithm {a}: op {j} modulates op {o}");
                    }
                }
            }
            assert!(alg.carriers != 0 && alg.feedback < OPS);
        }
        assert_eq!(ALGORITHMS.len(), super::super::params::FM_ALGORITHMS.len());
    }

    #[test]
    fn level_scale() {
        assert_eq!(level_amp(0.0), 0.0);
        assert!((level_amp(99.0) - 1.0).abs() < 1e-6);
        assert!((level_amp(91.0) - 0.5).abs() < 1e-6);
        assert_eq!(ratio(0, 0.0), 0.5);
        assert_eq!(ratio(2, 50.0), 3.0);
    }
}
