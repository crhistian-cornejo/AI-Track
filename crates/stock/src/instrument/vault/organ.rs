//! Drawbar organ: nine sine "tonewheels" per key at the classic footages
//! (16', 5⅓', 8', 4', 2⅔', 2', 1⅗', 1⅓', 1'), each drawbar 0..8 (≈3 dB a
//! step); single-triggered percussion on the 2nd or 3rd harmonic (fast /
//! slow decay, soft / normal, and the 1' bar drops out while it is on, as
//! on the original circuit); key click from contact bounce; a scanner-style
//! vibrato / chorus; tube overdrive; and a rotary speaker.
//!
//! Rotary: a crossover at 800 Hz splits the signal between a treble horn
//! and a bass drum. Each rotor is a modulated delay (Doppler) with an
//! amplitude modulation, picked up by two virtual mics 90° apart for the
//! stereo image. Slow / fast speeds are approached with each rotor's own
//! inertia (the light horn ramps in under a second, the heavy drum takes a
//! few seconds), scaled by the Ramp parameter.

use super::Engine;
use super::kit::{Perf, Pool, Rng, Voice, flush, key_hz, sin01, soft};
use super::params::{Params, organ as op};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::svf::{OnePole, SvfCoeffs, SvfMode, SvfState};

pub const VOICES: usize = 16;
pub const BARS: usize = 9;
/// Harmonic ratio of each drawbar to the 8' fundamental.
pub const FOOTAGE_RATIOS: [f32; BARS] = [0.5, 1.5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0];

// Rotor speeds (Hz) and inertia (s, time constant).
const HORN: [f32; 2] = [0.83, 6.7];
const DRUM: [f32; 2] = [0.67, 5.8];
const HORN_TAU: f32 = 0.45;
const DRUM_TAU: f32 = 2.2;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    sr: f32,
    gains: [f32; BARS],
    perc: bool,
    perc_ratio: f32,
    perc_decay: f32,
    perc_level: f32,
    click: f32,
    release_ms: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sr: 48_000.0, gains: [0.0; BARS], perc: false, perc_ratio: 3.0, perc_decay: 0.99, perc_level: 0.5, click: 0.3, release_ms: 15.0 }
    }
}

/// Drawbar position 0..8 → gain (8 = 1, ≈−3 dB a step, 0 = off).
#[inline]
pub fn bar_gain(pos: f32) -> f32 {
    if pos <= 0.0 { 0.0 } else { 10f32.powf(-3.0 * (8.0 - pos) / 20.0) }
}

pub struct OrganVoice {
    phase: [f32; BARS],
    inc: [f32; BARS],
    perc_phase: f32,
    perc_inc: f32,
    perc: f32,
    key: u8,
    gate: bool,
    level: f32,
    attack: f32,
    release: f32,
    click: f32,
    click_lp: OnePole,
    rng: Rng,
    pitch: f32,
    perc_on: bool,
}

impl OrganVoice {
    fn new(sr: f64, seed: u32) -> Self {
        Self {
            phase: [0.0; BARS],
            inc: [0.0; BARS],
            perc_phase: 0.0,
            perc_inc: 0.0,
            perc: 0.0,
            key: 60,
            gate: false,
            level: 0.0,
            attack: 1.0,
            release: 0.0,
            click: 0.0,
            click_lp: OnePole::new(3000.0, sr),
            rng: Rng::new(seed.wrapping_mul(22_695_477).wrapping_add(1)),
            pitch: f32::NAN,
            perc_on: false,
        }
    }

    fn tune(&mut self, s: &Settings, bend: f32) {
        let f = key_hz(self.key as f32 + bend);
        for b in 0..BARS {
            // Tonewheels fold back an octave at the top of the compass.
            let mut hz = f * FOOTAGE_RATIOS[b];
            while hz > 6000.0 {
                hz *= 0.5;
            }
            self.inc[b] = hz / s.sr;
        }
        self.perc_inc = (f * s.perc_ratio / s.sr).min(0.49);
        self.pitch = bend;
    }
}

impl Voice for OrganVoice {
    type Settings = Settings;

    fn start(&mut self, key: u8, _vel: f32, retrigger: bool, s: &Settings, perf: &Perf) {
        // Organs ignore velocity.
        self.key = key;
        self.tune(s, perf.bend);
        if !retrigger {
            self.level = 0.0;
        }
        self.gate = true;
        // ≈1 ms contact attack.
        self.attack = 1.0 / (0.001 * s.sr);
        self.release = 1.0 - (-1.0 / (s.release_ms.max(1.0) * 0.001 * s.sr)).exp();
        self.click = s.click;
        self.click_lp.set_freq(2500.0 + 30.0 * key as f32, s.sr as f64);
    }

    fn release(&mut self, s: &Settings) {
        self.gate = false;
        self.release = 1.0 - (-1.0 / (s.release_ms.max(1.0) * 0.001 * s.sr)).exp();
        self.click = self.click.max(s.click * 0.5);
    }

    fn active(&self) -> bool {
        self.gate || self.level > 1e-4 || self.click > 1e-4
    }

    fn control(&mut self, s: &Settings, perf: &Perf) {
        if (perf.bend - self.pitch).abs() > 1e-4 || self.pitch.is_nan() {
            self.tune(s, perf.bend);
        }
    }

    #[inline]
    fn tick(&mut self, s: &Settings) -> (f32, f32) {
        if self.gate {
            self.level = (self.level + self.attack).min(1.0);
        } else {
            self.level -= self.level * self.release;
            if self.level < 1e-4 {
                self.level = 0.0;
            }
        }
        let mut x = 0.0;
        for b in 0..BARS {
            let g = s.gains[b];
            if g > 0.0 {
                x += g * sin01(self.phase[b]);
            }
            self.phase[b] += self.inc[b];
            if self.phase[b] >= 1.0 {
                self.phase[b] -= 1.0;
            }
        }
        let mut y = x * self.level;
        if self.perc_on && self.perc > 1e-5 {
            y += sin01(self.perc_phase) * self.perc * s.perc_level;
            self.perc_phase += self.perc_inc;
            self.perc_phase -= self.perc_phase.floor();
            self.perc *= s.perc_decay;
        }
        if self.click > 1e-4 {
            y += self.click_lp.lowpass(self.rng.next()) * self.click * 0.5;
            self.click *= 0.995;
        }
        let o = y * 0.12;
        (o, o)
    }

    fn kill(&mut self) {
        self.gate = false;
        self.level = 0.0;
        self.click = 0.0;
        self.perc = 0.0;
    }
}

struct Rotor {
    angle: f32,
    speed: f32,
}

pub struct Organ {
    s: Settings,
    pool: Pool<OrganVoice>,
    held: usize,
    scratch: Vec<f32>,
    junk: Vec<f32>,
    // Vibrato scanner.
    vib_line: DelayLine,
    vib_phase: f32,
    vib_depth: f32,
    vib_mix: f32,
    drive: f32,
    tone: SvfCoeffs,
    tone_state: SvfState,
    tone_hz: f32,
    // Rotary.
    rotary: f32,
    fast: bool,
    ramp: f32,
    split: OnePole,
    horn: Rotor,
    drum: Rotor,
    horn_line: [DelayLine; 2],
    drum_line: [DelayLine; 2],
}

impl Organ {
    pub fn new(sr: f64) -> Self {
        let voices = (0..VOICES as u32).map(|i| OrganVoice::new(sr, i * 17 + 9)).collect();
        let line = |ms: f64| DelayLine::new((ms * 0.001 * sr) as usize + 8);
        Self {
            s: Settings { sr: sr as f32, ..Default::default() },
            pool: Pool::new(voices),
            held: 0,
            scratch: vec![0.0; 4096],
            junk: vec![0.0; 4096],
            vib_line: line(8.0),
            vib_phase: 0.0,
            vib_depth: 0.0,
            vib_mix: 0.0,
            drive: 0.0,
            tone: SvfCoeffs::new(SvfMode::Lowpass, 8000.0, 0.707, 0.0, sr),
            tone_state: SvfState::default(),
            tone_hz: 8000.0,
            rotary: 0.7,
            fast: false,
            ramp: 1.0,
            split: OnePole::new(800.0, sr),
            horn: Rotor { angle: 0.0, speed: HORN[0] },
            drum: Rotor { angle: 0.3, speed: DRUM[0] },
            horn_line: [line(4.0), line(4.0)],
            drum_line: [line(6.0), line(6.0)],
        }
    }

    /// Current rotor speeds (Hz), horn and drum.
    pub fn rotor_speeds(&self) -> (f32, f32) {
        (self.horn.speed, self.drum.speed)
    }
}

impl Engine for Organ {
    fn prepare(&mut self, sr: f64) {
        if sr as f32 != self.s.sr {
            *self = Self::new(sr);
        }
    }

    fn reset(&mut self) {
        self.pool.kill_all();
        self.held = 0;
        self.vib_line.clear();
        for l in self.horn_line.iter_mut().chain(&mut self.drum_line) {
            l.clear();
        }
        self.tone_state.reset();
        self.split.reset();
    }

    fn update(&mut self, p: &Params) {
        let s = &mut self.s;
        s.perc = p.on(op::PERC);
        for b in 0..BARS {
            s.gains[b] = bar_gain(p.plain(op::drawbar(b as u32)));
        }
        if s.perc {
            // The percussion borrows the 1' bar's keying.
            s.gains[8] = 0.0;
        }
        s.perc_ratio = if p.choice(op::PERC_HARM) == 0 { 2.0 } else { 3.0 };
        let t60 = if p.choice(op::PERC_DECAY) == 0 { 0.35 } else { 1.2 };
        s.perc_decay = (-6.907_755 / (t60 * s.sr)).exp();
        s.perc_level = if p.choice(op::PERC_VOL) == 0 { 0.5 } else { 1.0 };
        s.click = p.plain(op::CLICK) / 100.0;
        s.release_ms = p.plain(op::RELEASE);
        self.rotary = p.plain(op::ROTARY) / 100.0;
        self.fast = p.choice(op::SPEED) == 1;
        self.ramp = p.plain(op::RAMP) / 100.0;
        self.drive = p.plain(op::DRIVE) / 100.0;
        let (depth, mix) = match p.choice(op::VIBRATO) {
            1 => (0.35, 1.0),
            2 => (0.65, 1.0),
            3 => (1.0, 1.0),
            4 => (0.35, 0.5),
            5 => (0.65, 0.5),
            6 => (1.0, 0.5),
            _ => (0.0, 0.0),
        };
        self.vib_depth = depth;
        self.vib_mix = mix;
        let hz = p.plain(op::TONE);
        if hz != self.tone_hz {
            self.tone_hz = hz;
            self.tone = SvfCoeffs::new(SvfMode::Lowpass, hz, 0.707, 0.0, s.sr as f64);
        }
        self.pool.refresh();
    }

    fn note_on(&mut self, key: u8, vel: f32, perf: &Perf) {
        // Single-trigger percussion: only when no other key is held.
        let perc = self.s.perc && self.held == 0;
        self.held += 1;
        self.pool.note_on(key, vel, &self.s, perf);
        if let Some(v) = self.pool.voices.iter_mut().find(|v| v.gate && v.key == key) {
            v.perc_on = perc;
            if perc {
                v.perc = 1.0;
                v.perc_phase = 0.0;
            }
        }
    }

    fn note_off(&mut self, key: u8) {
        self.held = self.held.saturating_sub(1);
        self.pool.note_off(key, &self.s);
    }

    fn release_all(&mut self) {
        self.held = 0;
        self.pool.release_all(&self.s);
    }

    fn render(&mut self, perf: &Perf, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(self.scratch.len());
        self.scratch[..n].fill(0.0);
        self.junk[..n].fill(0.0);
        self.pool.render(&self.s, perf, &mut self.scratch[..n], &mut self.junk[..n]);
        let sr = self.s.sr;
        let ms = sr * 0.001;
        let target = usize::from(self.fast);
        let ramp = self.ramp.max(0.05);
        let horn_c = 1.0 - (-1.0 / (HORN_TAU * ramp * sr)).exp();
        let drum_c = 1.0 - (-1.0 / (DRUM_TAU * ramp * sr)).exp();
        let drive_gain = 1.0 + 6.0 * self.drive;
        for i in 0..n {
            let mut x = self.scratch[i];
            // Scanner vibrato: ~7 Hz pitch wobble from a modulated delay.
            if self.vib_mix > 0.0 {
                self.vib_line.push(x);
                self.vib_phase += 6.9 / sr;
                self.vib_phase -= self.vib_phase.floor();
                let d = (1.0 + self.vib_depth * (1.0 + sin01(self.vib_phase))) * ms;
                let wet = self.vib_line.read_linear(d);
                x = x * (1.0 - self.vib_mix) + wet * self.vib_mix;
            }
            if self.drive > 0.0 {
                x = soft(x * drive_gain) / drive_gain.sqrt();
            }
            x = self.tone_state.process(&self.tone, x);
            // Rotary.
            self.horn.speed += (HORN[target] - self.horn.speed) * horn_c;
            self.drum.speed += (DRUM[target] - self.drum.speed) * drum_c;
            self.horn.angle += self.horn.speed / sr;
            self.horn.angle -= self.horn.angle.floor();
            self.drum.angle += self.drum.speed / sr;
            self.drum.angle -= self.drum.angle.floor();
            let (dl, dr) = if self.rotary > 0.0 {
                let (low, high) = self.split.tick(x);
                let mut out = [0.0f32; 2];
                for (c, o) in out.iter_mut().enumerate() {
                    // Mics 90° apart.
                    let off = 0.25 * c as f32;
                    let hs = sin01(self.horn.angle + off);
                    let hc = sin01(self.horn.angle + off + 0.25);
                    self.horn_line[c].push(high);
                    let h = self.horn_line[c].read_linear((1.2 + 0.9 * hs) * ms) * (1.0 + 0.45 * hc);
                    let ds = sin01(self.drum.angle + off);
                    let dc = sin01(self.drum.angle + off + 0.25);
                    self.drum_line[c].push(low);
                    let d = self.drum_line[c].read_linear((2.0 + 1.0 * ds) * ms) * (1.0 + 0.25 * dc);
                    *o = h + d;
                }
                let m = self.rotary;
                (x * (1.0 - m) + out[0] * m, x * (1.0 - m) + out[1] * m)
            } else {
                (x, x)
            };
            l[i] += flush(dl);
            r[i] += flush(dr);
        }
    }

    fn active(&self) -> usize {
        self.pool.active()
    }

    fn tail_secs(&self) -> f32 {
        self.s.release_ms / 1000.0 + 0.05
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
    fn drawbar_law() {
        assert_eq!(bar_gain(0.0), 0.0);
        assert!((bar_gain(8.0) - 1.0).abs() < 1e-6);
        assert!((20.0 * bar_gain(7.0).log10() + 3.0).abs() < 1e-3);
    }
}
