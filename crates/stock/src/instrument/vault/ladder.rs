//! Mono "ladder" synth: three oscillators and noise into a nonlinear,
//! self-oscillating 4-pole transistor-ladder low-pass, with glide, legato
//! (last-note priority), filter and amp envelopes and a vibrato LFO.

use super::Engine;
use super::kit::{CONTROL, Cascade4, FilterModel, Perf, Rng, coeff, flush, key_hz, sin01};
use super::params::{Params, ladder as lp};
use aitrack_dsp::envelope::Adsr;
use aitrack_dsp::osc::{BlepOsc, VaShape};

const STACK: usize = 16;

#[derive(Clone, Copy, Debug, Default)]
struct Settings {
    octave: [f32; 3],
    detune: [f32; 3],
    level: [f32; 3],
    noise: f32,
    cutoff: f32,
    reso: f32,
    drive: f32,
    env: f32,
    key_track: f32,
    glide_ms: f32,
    legato: bool,
    lfo_rate: f32,
    vibrato: f32,
    lfo_cutoff: f32,
    vel_cutoff: f32,
    vel_amp: f32,
    release_ms: f32,
}

fn shape(i: usize) -> (VaShape, bool) {
    match i {
        0 => (VaShape::Saw, false),
        1 => (VaShape::Pulse, false),
        2 => (VaShape::Triangle, false),
        _ => (VaShape::Pulse, true),
    }
}

pub struct Ladder {
    s: Settings,
    sr: f32,
    osc: [BlepOsc; 3],
    rng: Rng,
    filter: Cascade4,
    fenv: Adsr,
    aenv: Adsr,
    key: f32,
    target: f32,
    vel: f32,
    lfo: f32,
    stack: [u8; STACK],
    vels: [f32; STACK],
    held: usize,
    countdown: usize,
    gate: bool,
}

impl Ladder {
    pub fn new(sr: f64) -> Self {
        let mut l = Self {
            s: Settings::default(),
            sr: sr as f32,
            osc: [BlepOsc::default(); 3],
            rng: Rng::new(0x1ADD),
            filter: Cascade4::new(FilterModel::Ladder),
            fenv: Adsr::new(sr),
            aenv: Adsr::new(sr),
            key: 60.0,
            target: 60.0,
            vel: 1.0,
            lfo: 0.0,
            stack: [0; STACK],
            vels: [0.0; STACK],
            held: 0,
            countdown: 0,
            gate: false,
        };
        l.prepare(sr);
        l
    }

    fn trigger(&mut self, key: u8, vel: f32, legato: bool) {
        self.target = key as f32;
        if !legato || !self.gate {
            if self.s.glide_ms <= 0.0 || !self.aenv.is_active() {
                self.key = self.target;
            }
            self.vel = vel;
            self.fenv.gate_on();
            self.aenv.gate_on();
        }
        self.gate = true;
        self.countdown = 0;
    }

    fn control(&mut self, perf: &Perf) {
        let s = &self.s;
        let n = CONTROL as f32;
        if s.glide_ms > 0.0 {
            let c = 1.0 - (1.0 - coeff(s.glide_ms / 3.0, self.sr)).powf(n);
            self.key += (self.target - self.key) * c;
        } else {
            self.key = self.target;
        }
        self.lfo += s.lfo_rate * n / self.sr;
        self.lfo -= self.lfo.floor();
        let lfo = sin01(self.lfo);
        let vib = lfo * s.vibrato / 100.0 + perf.mod_wheel * lfo * 0.5;
        let base = self.key + perf.bend + vib;
        for o in 0..3 {
            let hz = key_hz(base + 12.0 * s.octave[o] + s.detune[o]);
            self.osc[o].set_freq(hz, self.sr);
        }
        let fe = self.fenv.value();
        let vel = 1.0 - s.vel_cutoff + s.vel_cutoff * self.vel;
        let oct = s.env * 5.0 * fe * vel + s.key_track * (self.key - 60.0) / 12.0 + s.lfo_cutoff * 2.0 * lfo + (vel - 1.0) * 1.5;
        let cutoff = s.cutoff * 2f32.powf(oct);
        self.filter.set(cutoff, s.reso, self.sr);
    }
}

impl Engine for Ladder {
    fn prepare(&mut self, sr: f64) {
        self.sr = sr as f32;
        self.fenv.set_sample_rate(sr);
        self.aenv.set_sample_rate(sr);
    }

    fn reset(&mut self) {
        self.fenv.reset();
        self.aenv.reset();
        self.filter.reset();
        self.held = 0;
        self.gate = false;
    }

    fn update(&mut self, p: &Params) {
        let s = &mut self.s;
        for o in 0..3u32 {
            let (w, pulse) = shape(p.choice(lp::osc(o, lp::WAVE)));
            self.osc[o as usize].shape = w;
            self.osc[o as usize].pulse_width = if pulse { 0.25 } else { 0.5 };
            s.octave[o as usize] = p.choice(lp::osc(o, lp::OCTAVE)) as f32 - 2.0;
            s.detune[o as usize] = p.plain(lp::osc(o, lp::DETUNE));
            s.level[o as usize] = p.plain(lp::osc(o, lp::LEVEL)) / 100.0;
        }
        s.noise = p.plain(lp::NOISE) / 100.0;
        s.cutoff = p.plain(lp::CUTOFF);
        s.reso = p.plain(lp::RESONANCE) / 100.0;
        s.drive = 1.0 + 3.0 * p.plain(lp::DRIVE) / 100.0;
        s.env = p.plain(lp::ENV_AMOUNT) / 100.0;
        s.key_track = p.plain(lp::KEY_TRACK) / 100.0;
        s.glide_ms = p.plain(lp::GLIDE);
        s.legato = p.on(lp::LEGATO);
        s.lfo_rate = p.plain(lp::LFO_RATE);
        s.vibrato = p.plain(lp::LFO_PITCH);
        s.lfo_cutoff = p.plain(lp::LFO_CUTOFF) / 100.0;
        s.vel_cutoff = p.plain(lp::VEL_CUTOFF) / 100.0;
        s.vel_amp = p.plain(lp::VEL_AMP) / 100.0;
        s.release_ms = p.plain(lp::A_RELEASE).max(p.plain(lp::F_RELEASE));
        self.fenv.set(p.plain(lp::F_ATTACK), p.plain(lp::F_DECAY), p.plain(lp::F_SUSTAIN) / 100.0, p.plain(lp::F_RELEASE));
        self.aenv.set(p.plain(lp::A_ATTACK), p.plain(lp::A_DECAY), p.plain(lp::A_SUSTAIN) / 100.0, p.plain(lp::A_RELEASE));
        self.filter.drive = s.drive;
        self.countdown = 0;
    }

    fn note_on(&mut self, key: u8, vel: f32, _perf: &Perf) {
        // Move the key to the top of the stack.
        if let Some(i) = self.stack[..self.held].iter().position(|&k| k == key) {
            self.stack.copy_within(i + 1..self.held, i);
            self.vels.copy_within(i + 1..self.held, i);
            self.held -= 1;
        }
        if self.held == STACK {
            self.stack.copy_within(1.., 0);
            self.vels.copy_within(1.., 0);
            self.held -= 1;
        }
        self.stack[self.held] = key;
        self.vels[self.held] = vel;
        self.held += 1;
        let legato = self.s.legato && self.held > 1;
        self.trigger(key, vel, legato);
    }

    fn note_off(&mut self, key: u8) {
        let Some(i) = self.stack[..self.held].iter().position(|&k| k == key) else { return };
        let was_top = i + 1 == self.held;
        self.stack.copy_within(i + 1..self.held, i);
        self.vels.copy_within(i + 1..self.held, i);
        self.held -= 1;
        if !was_top {
            return;
        }
        if self.held > 0 {
            // Fall back to the previous held key without retriggering.
            let (k, v) = (self.stack[self.held - 1], self.vels[self.held - 1]);
            let legato = self.s.legato;
            if legato {
                self.target = k as f32;
            } else {
                self.trigger(k, v, false);
            }
        } else {
            self.gate = false;
            self.fenv.gate_off();
            self.aenv.gate_off();
        }
    }

    fn release_all(&mut self) {
        self.held = 0;
        self.gate = false;
        self.fenv.gate_off();
        self.aenv.gate_off();
    }

    fn render(&mut self, perf: &Perf, l: &mut [f32], r: &mut [f32]) {
        if !self.aenv.is_active() {
            return;
        }
        for i in 0..l.len() {
            if self.countdown == 0 {
                self.control(perf);
                self.countdown = CONTROL;
            }
            self.countdown -= 1;
            let s = &self.s;
            let mut x = 0.0;
            for o in 0..3 {
                if s.level[o] > 0.0 {
                    x += s.level[o] * self.osc[o].next();
                } else {
                    self.osc[o].next();
                }
            }
            x += s.noise * self.rng.next();
            let y = self.filter.process(0.5 * x);
            self.fenv.next();
            let a = self.aenv.next();
            let g = a * (1.0 - s.vel_amp + s.vel_amp * self.vel);
            let out = flush(y * g * 1.0);
            l[i] += out;
            r[i] += out;
            if !self.aenv.is_active() {
                self.filter.reset();
                break;
            }
        }
    }

    fn active(&self) -> usize {
        usize::from(self.aenv.is_active())
    }

    fn tail_secs(&self) -> f32 {
        self.s.release_ms / 1000.0
    }

    fn held_keys(&self, out: &mut [u8]) -> usize {
        if self.gate && self.held > 0 && !out.is_empty() {
            out[0] = self.stack[self.held - 1];
            1
        } else {
            0
        }
    }
}
