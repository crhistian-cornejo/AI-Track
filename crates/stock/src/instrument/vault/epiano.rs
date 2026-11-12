//! Electric piano: a modal model of a struck tine (or reed) and its pickup.
//!
//! Each voice is three exponentially decaying resonators at the
//! clamped-free beam ratios 1 : 6.27 : 17.55 (fundamental, "bell" and the
//! hammer-contact partial), struck with amplitudes that depend on velocity
//! and hammer hardness. The sum goes through a polynomial pickup model:
//! a centred pickup gives odd harmonics, an off-axis one even harmonics,
//! and the nonlinearity grows with velocity so hard notes "bark". A short
//! noise burst adds hammer thump. Dampers shorten the decay on release
//! (none on the top keys, as on the real thing). The reed model decays
//! faster, has a weaker bell and a stronger, grittier pickup.
//!
//! After the voices: tone low-pass, DC blocker and a tremolo that is either
//! amplitude or stereo auto-pan.

use super::Engine;
use super::kit::{Perf, Pool, Rng, Voice, flush, key_hz, sin01, soft};
use super::params::{Params, ep as ep_p};
use aitrack_dsp::svf::{DcBlocker, OnePole};

pub const VOICES: usize = 16;
const MODES: usize = 3;
const RATIOS: [f32; MODES] = [1.0, 6.27, 17.55];
/// Keys from here up have no damper.
const NO_DAMPER: u8 = 92;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    sr: f32,
    reed: bool,
    hardness: f32,
    bell: f32,
    bark: f32,
    pickup: f32,
    decay: f32,
    release_ms: f32,
    vel: f32,
    noise: f32,
    detune: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sr: 48_000.0, reed: false, hardness: 0.45, bell: 0.4, bark: 0.35, pickup: 0.35, decay: 1.0, release_ms: 120.0, vel: 0.7, noise: 0.25, detune: 2.0 }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Mode {
    re: f32,
    im: f32,
    cw: f32,
    sw: f32,
    /// Per-sample decay while held / after release.
    r_hold: f32,
    r_free: f32,
}

pub struct EpVoice {
    modes: [Mode; MODES],
    damped: bool,
    alive: bool,
    key: u8,
    vel: f32,
    drive: f32,
    thump: f32,
    thump_decay: f32,
    thump_lp: OnePole,
    rng: Rng,
    countdown: u32,
    pitch: f32,
}

/// Per-sample multiplier for a −60 dB time of `t60` seconds.
#[inline]
fn decay_r(t60: f32, sr: f32) -> f32 {
    (-6.907_755 / (t60.max(0.002) * sr)).exp()
}

/// Stable per-key detune in −1..1.
fn key_jitter(key: u8) -> f32 {
    let mut x = (key as u32).wrapping_mul(2_654_435_761) ^ 0x9E37_79B9;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 32_768.0 - 1.0
}

impl EpVoice {
    fn new(sr: f64, seed: u32) -> Self {
        Self {
            modes: [Mode::default(); MODES],
            damped: false,
            alive: false,
            key: 60,
            vel: 1.0,
            drive: 0.0,
            thump: 0.0,
            thump_decay: 0.99,
            thump_lp: OnePole::new(600.0, sr),
            rng: Rng::new(seed.wrapping_mul(1_103_515_245).wrapping_add(12_345)),
            countdown: 0,
            pitch: 0.0,
        }
    }

    fn tune(&mut self, s: &Settings, bend: f32) {
        let f0 = key_hz(self.key as f32 + bend + key_jitter(self.key) * s.detune / 100.0);
        let k = (self.key as f32 - 48.0) / 12.0;
        let base = if s.reed { 3.2 } else { 5.5 } * s.decay * 2f32.powf(-k * 0.75);
        let t60 = [base, base * 0.18 * 2f32.powf(-k * 0.3), 0.12];
        let release = s.release_ms / 1000.0;
        for (m, mode) in self.modes.iter_mut().enumerate() {
            let w = std::f32::consts::TAU * f0 * RATIOS[m] / s.sr;
            mode.cw = w.cos();
            mode.sw = w.sin();
            mode.r_hold = decay_r(t60[m], s.sr);
            mode.r_free = if self.key >= NO_DAMPER { mode.r_hold } else { decay_r(t60[m].min(release), s.sr) };
            if w >= std::f32::consts::PI * 0.9 {
                // Above Nyquist: drop it.
                mode.re = 0.0;
                mode.im = 0.0;
                mode.r_hold = 0.0;
                mode.r_free = 0.0;
            }
        }
        self.pitch = bend;
    }
}

impl Voice for EpVoice {
    type Settings = Settings;

    fn start(&mut self, key: u8, vel: f32, retrigger: bool, s: &Settings, perf: &Perf) {
        self.key = key;
        // Velocity curve: soft notes much quieter, and darker.
        let v = vel.clamp(0.0, 1.0);
        self.vel = 1.0 - s.vel + s.vel * v * v;
        let hard = s.hardness * (0.1 + 0.9 * v);
        self.tune(s, perf.bend);
        let bell = s.bell * if s.reed { 0.35 } else { 1.0 };
        let amps = [1.0, 0.7 * bell * (0.15 + hard), 1.2 * hard * hard];
        if !retrigger {
            for m in &mut self.modes {
                m.re = 0.0;
                m.im = 0.0;
            }
            self.thump_lp.reset();
        }
        for (m, a) in self.modes.iter_mut().zip(amps) {
            // A strike adds energy in phase with the cosine component, so
            // the output (the sine component) starts at zero.
            if m.r_hold > 0.0 {
                m.re += a * self.vel;
            }
        }
        self.drive = s.bark * (0.1 + 0.9 * v * v) * if s.reed { 1.6 } else { 1.0 };
        self.thump = s.noise * self.vel;
        self.thump_decay = decay_r(0.012, s.sr);
        let f0 = key_hz(key as f32);
        self.thump_lp.set_freq((f0 * 3.0).clamp(200.0, 4000.0), s.sr as f64);
        self.damped = false;
        self.alive = true;
        self.countdown = 0;
    }

    fn release(&mut self, _s: &Settings) {
        self.damped = true;
    }

    fn active(&self) -> bool {
        self.alive
    }

    fn control(&mut self, s: &Settings, perf: &Perf) {
        if (perf.bend - self.pitch).abs() > 1e-4 {
            self.tune(s, perf.bend);
        }
        let e: f32 = self.modes.iter().map(|m| m.re.abs() + m.im.abs()).sum();
        if e < 2e-5 && self.thump < 1e-5 {
            self.alive = false;
            for m in &mut self.modes {
                m.re = 0.0;
                m.im = 0.0;
            }
        }
    }

    #[inline]
    fn tick(&mut self, s: &Settings) -> (f32, f32) {
        let mut x = 0.0;
        for m in &mut self.modes {
            let r = if self.damped { m.r_free } else { m.r_hold };
            let re = r * (m.re * m.cw - m.im * m.sw);
            let im = r * (m.re * m.sw + m.im * m.cw);
            m.re = flush(re);
            m.im = flush(im);
            x += im;
        }
        // Pickup: x is the tine displacement; off-axis → even harmonics.
        let d = self.drive;
        let even = s.pickup;
        let mut y = x + d * (even * x * x * 1.5 - (1.0 - even) * x * x * x * 0.8);
        if s.reed {
            y = soft(y * (1.0 + d));
        }
        if self.thump > 1e-6 {
            y += self.thump_lp.lowpass(self.rng.next()) * self.thump;
            self.thump *= self.thump_decay;
        }
        let o = y * 0.3;
        (o, o)
    }

    fn kill(&mut self) {
        self.alive = false;
        for m in &mut self.modes {
            m.re = 0.0;
            m.im = 0.0;
        }
        self.thump = 0.0;
    }
}

pub struct EPiano {
    s: Settings,
    pool: Pool<EpVoice>,
    tone: [[OnePole; 2]; 2],
    dc: [DcBlocker; 2],
    trem_rate: f32,
    trem_depth: f32,
    autopan: bool,
    trem: f32,
    tone_hz: f32,
    scratch: Vec<f32>,
    junk: Vec<f32>,
}

impl EPiano {
    pub fn new(sr: f64) -> Self {
        let voices = (0..VOICES as u32).map(|i| EpVoice::new(sr, i * 31 + 11)).collect();
        Self {
            s: Settings { sr: sr as f32, ..Default::default() },
            pool: Pool::new(voices),
            tone: [[OnePole::new(9000.0, sr); 2]; 2],
            dc: [DcBlocker::new(20.0, sr); 2],
            trem_rate: 4.5,
            trem_depth: 0.0,
            autopan: true,
            trem: 0.0,
            tone_hz: 9000.0,
            scratch: vec![0.0; 4096],
            junk: vec![0.0; 4096],
        }
    }
}

impl Engine for EPiano {
    fn prepare(&mut self, sr: f64) {
        if sr as f32 != self.s.sr {
            *self = Self::new(sr);
        }
    }

    fn reset(&mut self) {
        self.pool.kill_all();
        for c in 0..2 {
            self.dc[c].reset();
            for t in &mut self.tone[c] {
                t.reset();
            }
        }
    }

    fn update(&mut self, p: &Params) {
        let s = &mut self.s;
        s.reed = p.choice(ep_p::MODEL) == 1;
        s.hardness = p.plain(ep_p::HARDNESS) / 100.0;
        s.bell = p.plain(ep_p::BELL) / 100.0;
        s.bark = p.plain(ep_p::BARK) / 100.0;
        s.pickup = p.plain(ep_p::PICKUP) / 100.0;
        s.decay = p.plain(ep_p::DECAY) / 100.0;
        s.release_ms = p.plain(ep_p::RELEASE);
        s.vel = p.plain(ep_p::VEL) / 100.0;
        s.noise = p.plain(ep_p::NOISE) / 100.0;
        s.detune = p.plain(ep_p::DETUNE);
        self.trem_rate = p.plain(ep_p::TREM_RATE);
        self.trem_depth = p.plain(ep_p::TREM_DEPTH) / 100.0;
        self.autopan = p.on(ep_p::AUTOPAN);
        let hz = p.plain(ep_p::TONE);
        if hz != self.tone_hz {
            self.tone_hz = hz;
            for c in &mut self.tone {
                for t in c.iter_mut() {
                    t.set_freq(hz, s.sr as f64);
                }
            }
        }
        // Retune held notes (decay, release, detune).
        let (s, perf) = (self.s, Perf::default());
        for v in &mut self.pool.voices {
            if v.alive {
                v.tune(&s, perf.bend + v.pitch);
            }
        }
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
        let n = l.len().min(self.scratch.len());
        self.scratch[..n].fill(0.0);
        self.junk[..n].fill(0.0);
        self.pool.render(&self.s, perf, &mut self.scratch[..n], &mut self.junk[..n]);
        let sr = self.s.sr;
        for i in 0..n {
            let x = self.scratch[i];
            self.trem += self.trem_rate / sr;
            self.trem -= self.trem.floor();
            let lfo = sin01(self.trem);
            let d = self.trem_depth;
            let (gl, gr) = if self.autopan { (1.0 - d * 0.5 * (1.0 + lfo), 1.0 - d * 0.5 * (1.0 - lfo)) } else { (1.0 - d * 0.5 * (1.0 + lfo), 1.0 - d * 0.5 * (1.0 + lfo)) };
            for (c, (g, out)) in [(gl, &mut l[i]), (gr, &mut r[i])].into_iter().enumerate() {
                let mut y = self.dc[c].process(x);
                y = self.tone[c][0].lowpass(y);
                y = self.tone[c][1].lowpass(y);
                *out += flush(y * g);
            }
        }
    }

    fn active(&self) -> usize {
        self.pool.active()
    }

    fn tail_secs(&self) -> f32 {
        // Undamped top keys ring for their full decay.
        6.0 * self.s.decay.max(0.1)
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
