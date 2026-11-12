//! String machine: divide-down oscillators, a paraphonic filter and a
//! three-phase ensemble.
//!
//! On the original instruments one master clock is divided down to every
//! note, so all keys are phase-locked to each other. Here every oscillator
//! derives its phase from one shared sample counter (`phase = t·f mod 1`),
//! which gives the same locked, "single instrument" behaviour: the 16', 8'
//! and 4' registers of a key, and repeated notes, always line up. Each key
//! has its own attack/release envelope (the gate), then everything is summed
//! into one low-pass (with a "swell" that opens it as the notes fade in),
//! a high-pass, and the ensemble: three modulated delay taps 120° apart with
//! a slow and a fast component, which is what turns a thin saw organ into a
//! string section.

use super::Engine;
use super::kit::{Ensemble, Perf, Pool, Voice, coeff, flush, key_hz};
use super::params::{Params, strings as sp};
use aitrack_dsp::osc::poly_blep;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};

pub const VOICES: usize = 16;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    sr: f32,
    cello: f32,
    violin: f32,
    viola: f32,
    horn: f32,
    attack: f32,
    release: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sr: 48_000.0, cello: 0.4, violin: 1.0, viola: 0.5, horn: 0.0, attack: 0.001, release: 0.001 }
    }
}

#[inline]
fn saw(t: f32, dt: f32) -> f32 {
    2.0 * t - 1.0 - poly_blep(t, dt)
}

#[inline]
fn square(t: f32, dt: f32) -> f32 {
    let mut y = if t < 0.5 { 1.0 } else { -1.0 };
    y += poly_blep(t, dt);
    y -= poly_blep((t - 0.5).rem_euclid(1.0), dt);
    y
}

pub struct StringVoice {
    /// Cycles per sample of the 8' register.
    f: f64,
    key: u8,
    gate: bool,
    env: f32,
    clock: u64,
    bend: f32,
}

impl StringVoice {
    fn new() -> Self {
        Self { f: 0.0, key: 60, gate: false, env: 0.0, clock: 0, bend: f32::NAN }
    }
}

impl Voice for StringVoice {
    type Settings = Settings;

    fn start(&mut self, key: u8, _vel: f32, _retrigger: bool, s: &Settings, perf: &Perf) {
        self.key = key;
        self.gate = true;
        self.bend = perf.bend;
        self.f = key_hz(key as f32 + perf.bend) as f64 / s.sr as f64;
    }

    fn release(&mut self, _s: &Settings) {
        self.gate = false;
    }

    fn active(&self) -> bool {
        self.gate || self.env > 1e-5
    }

    fn control(&mut self, s: &Settings, perf: &Perf) {
        if perf.bend != self.bend {
            self.bend = perf.bend;
            self.f = key_hz(self.key as f32 + perf.bend) as f64 / s.sr as f64;
        }
    }

    #[inline]
    fn tick(&mut self, s: &Settings) -> (f32, f32) {
        if self.gate {
            self.env += (1.0 - self.env) * s.attack;
        } else {
            self.env -= self.env * s.release;
            if self.env < 1e-5 {
                self.env = 0.0;
            }
        }
        // Phase-locked to the shared clock (`clock` is set by the engine).
        let t = self.clock as f64;
        self.clock += 1;
        let ph = |mul: f64| ((t * self.f * mul).fract()) as f32;
        let dt = self.f as f32;
        let mut x = 0.0;
        if s.violin > 0.0 {
            x += s.violin * saw(ph(1.0), dt);
        }
        if s.cello > 0.0 {
            x += s.cello * saw(ph(0.5), dt * 0.5);
        }
        if s.viola > 0.0 {
            x += s.viola * saw(ph(2.0), (dt * 2.0).min(0.5));
        }
        if s.horn > 0.0 {
            x += s.horn * 0.7 * square(ph(1.0), dt);
        }
        let o = x * self.env * 0.12;
        (o, self.env)
    }

    fn kill(&mut self) {
        self.gate = false;
        self.env = 0.0;
    }
}

pub struct Strings {
    s: Settings,
    pool: Pool<StringVoice>,
    clock: u64,
    lp: SvfState,
    hp: SvfState,
    hp_c: SvfCoeffs,
    tone: f32,
    reso: f32,
    swell: f32,
    ens: Ensemble,
    scratch: Vec<f32>,
    envs: Vec<f32>,
    countdown: usize,
    lp_c: SvfCoeffs,
    env_sum: f32,
}

impl Strings {
    pub fn new(sr: f64) -> Self {
        let voices = (0..VOICES).map(|_| StringVoice::new()).collect();
        Self {
            s: Settings { sr: sr as f32, ..Default::default() },
            pool: Pool::new(voices),
            clock: 0,
            lp: SvfState::default(),
            hp: SvfState::default(),
            hp_c: SvfCoeffs::new(SvfMode::Highpass, 120.0, 0.707, 0.0, sr),
            tone: 5500.0,
            reso: 0.1,
            swell: 0.2,
            ens: Ensemble::new(sr),
            scratch: vec![0.0; 4096],
            envs: vec![0.0; 4096],
            countdown: 0,
            lp_c: SvfCoeffs::new(SvfMode::Lowpass, 5500.0, 0.707, 0.0, sr),
            env_sum: 0.0,
        }
    }
}

impl Engine for Strings {
    fn prepare(&mut self, sr: f64) {
        if sr as f32 != self.s.sr {
            *self = Self::new(sr);
        }
    }

    fn reset(&mut self) {
        self.pool.kill_all();
        self.lp.reset();
        self.hp.reset();
        self.ens.reset();
    }

    fn update(&mut self, p: &Params) {
        let s = &mut self.s;
        s.cello = p.plain(sp::CELLO) / 100.0;
        s.violin = p.plain(sp::VIOLIN) / 100.0;
        s.viola = p.plain(sp::VIOLA) / 100.0;
        s.horn = p.plain(sp::HORN) / 100.0;
        s.attack = coeff(p.plain(sp::ATTACK) / 3.0, s.sr);
        s.release = coeff(p.plain(sp::RELEASE) / 5.0, s.sr);
        self.tone = p.plain(sp::TONE);
        self.reso = p.plain(sp::RESONANCE) / 100.0;
        self.swell = p.plain(sp::SWELL) / 100.0;
        self.hp_c = SvfCoeffs::new(SvfMode::Highpass, p.plain(sp::HIGH_PASS), 0.707, 0.0, s.sr as f64);
        let e = &mut self.ens;
        e.taps = 3;
        let depth = p.plain(sp::ENS_DEPTH) / 100.0;
        e.base_ms = 9.0;
        e.depth_ms = 4.0 * depth;
        e.fast_ms = 0.6 * depth;
        e.rate = p.plain(sp::ENS_RATE);
        e.fast_rate = 6.3;
        e.mix = p.plain(sp::ENSEMBLE) / 100.0;
        self.countdown = 0;
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
        for v in &mut self.pool.voices {
            v.clock = self.clock;
        }
        self.clock = self.clock.wrapping_add(n as u64);
        // Keep the clock small enough for exact f64 phases (≈2 days at 48 kHz).
        if self.clock > 1 << 33 {
            self.clock = 0;
        }
        self.scratch[..n].fill(0.0);
        self.envs[..n].fill(0.0);
        self.pool.render(&self.s, perf, &mut self.scratch[..n], &mut self.envs[..n]);
        let sr = self.s.sr;
        for i in 0..n {
            if self.countdown == 0 {
                // Swell: the filter follows the summed envelopes.
                let held = self.envs[i].min(1.0);
                self.env_sum += (held - self.env_sum) * 0.05;
                let hz = (self.tone * 2f32.powf(self.swell * 2.0 * (self.env_sum - 1.0))).clamp(40.0, 0.45 * sr);
                self.lp_c = SvfCoeffs::new(SvfMode::Lowpass, hz, 0.707 + 3.0 * self.reso, 0.0, sr as f64);
                self.countdown = 32;
            }
            self.countdown -= 1;
            let x = self.scratch[i];
            let y = self.hp.process(&self.hp_c, self.lp.process(&self.lp_c, x));
            let (a, b) = self.ens.process(y, y);
            l[i] += flush(a);
            r[i] += flush(b);
        }
    }

    fn active(&self) -> usize {
        self.pool.active()
    }

    fn tail_secs(&self) -> f32 {
        // Release coefficient back to a time.
        let t = -1.0 / ((1.0 - self.s.release).max(1e-9).ln() * self.s.sr);
        t * 5.0 * 1.6 + 0.1
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
