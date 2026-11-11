//! `instrument.drum` — 16-pad drum sampler.
//!
//! MIDI notes 36..=51 trigger pads 1..=16. Per pad: sample, tune, decay,
//! filter cutoff, pan, level, choke group and gate mode. A global velocity
//! curve (linear / soft / hard / fixed) and sensitivity shape the hits.
//! Pads without a loaded sample play a small built-in synthesised kit, so
//! the instrument makes sound out of the box.
//!
//! Voices: 32, oldest stolen with a short fade. A hit chokes every voice of
//! the same choke group (the pad itself included) with a 5 ms fade.
//!
//! State: `extra.pads` is an array of 16 WAV paths (or `null`), decoded on
//! the control thread in `load_state` / [`Drum::load_pad_wav`].
//!
//! Meter layout: `[active voices, last pad hit (0..15, −1 none),
//! pad levels ×16]`.

use super::sample::{SampleData, load_wav};
use crate::common::{ParamStore, Segmented, drive, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::osc::Noise;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use serde_json::{Value, json};

pub const ID: &str = "instrument.drum";
pub const PADS: usize = 16;
pub const FIRST_KEY: u8 = 36;
pub const MAX_VOICES: usize = 32;

pub mod p {
    pub const VOLUME: u32 = 0;
    pub const VEL_CURVE: u32 = 1;
    pub const VEL_SENS: u32 = 2;
    pub const PAD0: u32 = 16;
    pub const PAD_STRIDE: u32 = 8;
    pub const TUNE: u32 = 0;
    pub const DECAY: u32 = 1;
    pub const CUTOFF: u32 = 2;
    pub const PAN: u32 = 3;
    pub const LEVEL: u32 = 4;
    pub const CHOKE: u32 = 5;
    pub const GATE: u32 = 6;
    pub const FINE: u32 = 7;

    /// Id of `offset` (TUNE, DECAY…) for pad `pad` (0..16).
    pub const fn pad(pad: u32, offset: u32) -> u32 {
        PAD0 + pad * PAD_STRIDE + offset
    }
}

pub const VEL_CURVES: &[&str] = &["Linear", "Soft", "Hard", "Fixed"];
const CHOKES: &[&str] = &["Off", "1", "2", "3", "4", "5", "6", "7", "8"];
const OFF_ON: &[&str] = &["Off", "On"];

/// Names of the built-in kit, pad by pad.
pub const KIT: [&str; PADS] =
    ["Kick", "Rim", "Snare", "Clap", "Snare 2", "Low Tom", "Closed Hat", "Mid Tom", "Pedal Hat", "High Tom", "Open Hat", "Shaker", "Crash", "Perc", "Cowbell", "808"];

const PAD_NAMES: [[&str; 8]; PADS] = {
    #[allow(clippy::too_many_arguments)]
    const fn row(t: &'static str, d: &'static str, c: &'static str, p: &'static str, l: &'static str, k: &'static str, g: &'static str, f: &'static str) -> [&'static str; 8] {
        [t, d, c, p, l, k, g, f]
    }
    macro_rules! pad {
        ($n:literal) => {
            row(
                concat!("Pad ", $n, " Tune"),
                concat!("Pad ", $n, " Decay"),
                concat!("Pad ", $n, " Cutoff"),
                concat!("Pad ", $n, " Pan"),
                concat!("Pad ", $n, " Level"),
                concat!("Pad ", $n, " Choke"),
                concat!("Pad ", $n, " Gate"),
                concat!("Pad ", $n, " Fine"),
            )
        };
    }
    [pad!(1), pad!(2), pad!(3), pad!(4), pad!(5), pad!(6), pad!(7), pad!(8), pad!(9), pad!(10), pad!(11), pad!(12), pad!(13), pad!(14), pad!(15), pad!(16)]
};

/// Default choke groups: the three hats share group 1.
const DEFAULT_CHOKE: [f32; PADS] = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0];

pub fn params() -> Vec<ParamInfo> {
    let mut v = vec![
        ParamInfo::new(p::VOLUME, "Volume", -48.0, 6.0, 0.0).unit("dB"),
        ParamInfo::new(p::VEL_CURVE, "Velocity Curve", 0.0, 3.0, 0.0).choices(VEL_CURVES).short("Curve"),
        ParamInfo::new(p::VEL_SENS, "Velocity Sensitivity", 0.0, 100.0, 80.0).unit("%").short("Vel"),
    ];
    for (i, n) in PAD_NAMES.iter().enumerate() {
        let b = i as u32;
        v.push(ParamInfo::new(p::pad(b, p::TUNE), n[0], -24.0, 24.0, 0.0).unit("st").short("Tune"));
        v.push(ParamInfo::new(p::pad(b, p::DECAY), n[1], 10.0, 10_000.0, 10_000.0).unit("ms").log().short("Decay"));
        v.push(ParamInfo::new(p::pad(b, p::CUTOFF), n[2], 100.0, 20_000.0, 20_000.0).unit("Hz").log().short("Cutoff"));
        v.push(ParamInfo::new(p::pad(b, p::PAN), n[3], -100.0, 100.0, 0.0).unit("%").short("Pan"));
        v.push(ParamInfo::new(p::pad(b, p::LEVEL), n[4], -48.0, 12.0, 0.0).unit("dB").short("Level"));
        v.push(ParamInfo::new(p::pad(b, p::CHOKE), n[5], 0.0, 8.0, DEFAULT_CHOKE[i]).choices(CHOKES).short("Choke"));
        v.push(ParamInfo::new(p::pad(b, p::GATE), n[6], 0.0, 1.0, 0.0).choices(OFF_ON).short("Gate"));
        v.push(ParamInfo::new(p::pad(b, p::FINE), n[7], -100.0, 100.0, 0.0).unit("ct").short("Fine"));
    }
    v
}

#[derive(Clone, Copy, Debug)]
struct PadState {
    rate_mul: f64,
    /// Per-sample amplitude decay multiplier (−60 dB after `decay`).
    decay_mul: f32,
    gain_l: f32,
    gain_r: f32,
    choke: usize,
    gate: bool,
    filter_on: bool,
    coeffs: SvfCoeffs,
}

impl Default for PadState {
    fn default() -> Self {
        Self { rate_mul: 1.0, decay_mul: 1.0, gain_l: 1.0, gain_r: 1.0, choke: 0, gate: false, filter_on: false, coeffs: SvfCoeffs::passthrough() }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    active: bool,
    pad: usize,
    key: u8,
    pos: f64,
    amp: f32,
    vel: f32,
    /// 1 → 0 when choked, released or stolen.
    fade: f32,
    fading: bool,
    attack: f32,
    age: u64,
    filt: [SvfState; 2],
}

pub struct Drum {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    samples: [Option<Box<SampleData>>; PADS],
    kit: Vec<SampleData>,
    pads: [PadState; PADS],
    voices: [Voice; MAX_VOICES],
    volume: f32,
    curve: usize,
    vel_sens: f32,
    fade_step: f32,
    attack_step: f32,
    age: u64,
    last_pad: i32,
    levels: [f32; PADS],
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Drum::new())
}

impl Default for Drum {
    fn default() -> Self {
        Self::new()
    }
}

impl Drum {
    pub fn new() -> Self {
        let params = params();
        let mut d = Self {
            info: PluginInfo {
                id: ID,
                name: "Drum Sampler",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["drums", "sampler", "pads"],
                params: params.clone(),
                inputs: 0,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            samples: Default::default(),
            kit: build_kit(48_000.0),
            pads: [PadState::default(); PADS],
            voices: [Voice::default(); MAX_VOICES],
            volume: 1.0,
            curve: 0,
            vel_sens: 0.8,
            fade_step: 0.01,
            attack_step: 1.0,
            age: 0,
            last_pad: -1,
            levels: [0.0; PADS],
        };
        d.prepare(48_000.0, 512);
        d
    }

    /// Decode a WAV file into a pad (control thread).
    pub fn load_pad_wav(&mut self, pad: usize, path: &str) -> Result<(), String> {
        let s = load_wav(path)?;
        self.set_pad_sample(pad, Some(s));
        Ok(())
    }

    /// Replace (or clear, with `None`) a pad's sample (control thread).
    pub fn set_pad_sample(&mut self, pad: usize, data: Option<SampleData>) {
        if pad >= PADS {
            return;
        }
        for v in &mut self.voices {
            if v.pad == pad {
                v.active = false;
            }
        }
        self.samples[pad] = data.filter(|d| d.frames() > 1).map(Box::new);
        self.params.mark_dirty();
    }

    pub fn pad_sample(&self, pad: usize) -> Option<&SampleData> {
        self.samples.get(pad).and_then(|s| s.as_deref())
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    fn update(&mut self) {
        let pr = &self.params;
        self.volume = db_to_gain(pr.plain(p::VOLUME));
        self.curve = pr.choice(p::VEL_CURVE);
        self.vel_sens = pr.plain(p::VEL_SENS) / 100.0;
        for i in 0..PADS {
            let b = i as u32;
            let semis = pr.plain(p::pad(b, p::TUNE)).round() + pr.plain(p::pad(b, p::FINE)) / 100.0;
            let src_sr = self.samples[i].as_deref().map_or(self.sr, |s| s.sample_rate);
            let decay_ms = pr.plain(p::pad(b, p::DECAY));
            // Longest setting means "no decay": play the sample as is.
            let decay_mul = if decay_ms >= 9_999.0 { 1.0 } else { 10f32.powf(-3.0 / (decay_ms * 0.001 * self.sr as f32)) };
            let g = db_to_gain(pr.plain(p::pad(b, p::LEVEL)));
            let pan = pr.plain(p::pad(b, p::PAN)) / 100.0;
            let a = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
            let cutoff = pr.plain(p::pad(b, p::CUTOFF));
            self.pads[i] = PadState {
                rate_mul: 2f64.powf(semis as f64 / 12.0) * src_sr / self.sr,
                decay_mul,
                // Equal-power pan normalised to unity at centre.
                gain_l: g * a.cos() * std::f32::consts::SQRT_2,
                gain_r: g * a.sin() * std::f32::consts::SQRT_2,
                choke: pr.choice(p::pad(b, p::CHOKE)),
                gate: pr.on(p::pad(b, p::GATE)),
                filter_on: cutoff < 19_500.0,
                coeffs: SvfCoeffs::new(SvfMode::Lowpass, cutoff, 0.707, 0.0, self.sr),
            };
        }
    }

    fn velocity_gain(&self, v: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        let c = match self.curve {
            1 => v.sqrt(),
            2 => v * v,
            3 => 1.0,
            _ => v,
        };
        1.0 - self.vel_sens + self.vel_sens * c
    }

    fn note_on(&mut self, key: u8, velocity: f32) {
        if !(FIRST_KEY..FIRST_KEY + PADS as u8).contains(&key) {
            return;
        }
        if velocity <= 0.0 {
            self.note_off(key);
            return;
        }
        let pad = (key - FIRST_KEY) as usize;
        let choke = self.pads[pad].choke;
        if choke > 0 {
            for v in &mut self.voices {
                if v.active && self.pads[v.pad].choke == choke {
                    v.fading = true;
                }
            }
        }
        // Free voice, else the oldest one.
        let idx = match self.voices.iter().position(|v| !v.active) {
            Some(i) => i,
            None => (0..MAX_VOICES).min_by_key(|&i| self.voices[i].age).unwrap_or(0),
        };
        self.age += 1;
        let vel = self.velocity_gain(velocity);
        self.voices[idx] = Voice { active: true, pad, key, pos: 0.0, amp: 1.0, vel, fade: 1.0, fading: false, attack: 0.0, age: self.age, filt: [SvfState::default(); 2] };
        self.last_pad = pad as i32;
    }

    fn note_off(&mut self, key: u8) {
        for v in &mut self.voices {
            if v.active && v.key == key && self.pads[v.pad].gate {
                v.fading = true;
            }
        }
    }
}

impl Segmented for Drum {
    fn event(&mut self, kind: &EventKind) {
        match *kind {
            EventKind::Param { id, value } => self.params.set(id, value),
            EventKind::NoteOn { key, velocity, .. } => {
                if self.params.take_dirty() {
                    self.update();
                }
                self.note_on(key, velocity)
            }
            EventKind::NoteOff { key, .. } => self.note_off(key),
            EventKind::AllNotesOff | EventKind::ControlChange { cc: 120 | 123, .. } => {
                for v in &mut self.voices {
                    v.fading = true;
                }
            }
            _ => {}
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let buf = &mut *ctx.buffer;
        let stereo = buf.channels() >= 2;
        let fade_step = self.fade_step;
        let attack_step = self.attack_step;
        let level_decay = (-((end - start) as f32) / (0.2 * self.sr as f32)).exp();
        for l in &mut self.levels {
            *l *= level_decay;
        }
        for vi in 0..MAX_VOICES {
            if !self.voices[vi].active {
                continue;
            }
            let mut v = self.voices[vi];
            let pad = self.pads[v.pad];
            let src = self.samples[v.pad].as_deref().unwrap_or(&self.kit[v.pad]);
            let frames = src.frames() as f64;
            let two = src.channels() >= 2;
            let mut peak = 0.0f32;
            for i in start..end {
                if v.pos >= frames {
                    v.active = false;
                    break;
                }
                if v.attack < 1.0 {
                    v.attack = (v.attack + attack_step).min(1.0);
                }
                if v.fading {
                    v.fade -= fade_step;
                    if v.fade <= 0.0 {
                        v.active = false;
                        break;
                    }
                }
                let g = v.amp * v.vel * v.fade * v.attack;
                let mut l = src.read(0, v.pos);
                let mut r = if two { src.read(1, v.pos) } else { l };
                if pad.filter_on {
                    l = v.filt[0].process(&pad.coeffs, l);
                    r = v.filt[1].process(&pad.coeffs, r);
                }
                let (ol, or) = (l * g * pad.gain_l * self.volume, r * g * pad.gain_r * self.volume);
                peak = peak.max(ol.abs().max(or.abs()));
                if stereo {
                    let (bl, br) = buf.stereo_mut();
                    bl[i] += ol;
                    br[i] += or;
                } else {
                    buf.channel_mut(0)[i] += 0.5 * (ol + or);
                }
                v.pos += pad.rate_mul;
                v.amp *= pad.decay_mul;
                if v.amp < 1e-4 {
                    v.active = false;
                    break;
                }
            }
            self.levels[v.pad] = self.levels[v.pad].max(peak);
            self.voices[vi] = v;
        }
        for ch in 0..buf.channels().min(2) {
            for s in &mut buf.channel_mut(ch)[start..end] {
                *s = sanitize(*s);
            }
        }
    }
}

impl Processor for Drum {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        if (sample_rate - self.sr).abs() > 0.5 {
            self.kit = build_kit(sample_rate);
        }
        self.sr = sample_rate;
        self.fade_step = 1.0 / (0.005 * sample_rate as f32).max(1.0);
        self.attack_step = 1.0 / (0.0005 * sample_rate as f32).max(1.0);
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.active = false;
        }
        self.levels = [0.0; PADS];
        self.last_pad = -1;
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        drive(self, ctx);
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn tail(&self) -> u32 {
        (self.sr * 0.01) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        let pads: Vec<Value> = self.samples.iter().map(|s| s.as_ref().and_then(|d| d.path.clone()).map_or(Value::Null, Value::from)).collect();
        save_state(&self.params, json!({ "pads": pads }))
    }

    fn load_state(&mut self, data: &[u8]) {
        let extra = load_state(&mut self.params, data);
        if let Some(pads) = extra.get("pads").and_then(Value::as_array) {
            for (i, v) in pads.iter().enumerate().take(PADS) {
                match v.as_str() {
                    Some(path) => {
                        if self.load_pad_wav(i, path).is_err() {
                            self.set_pad_sample(i, None);
                        }
                    }
                    None => self.set_pad_sample(i, None),
                }
            }
        }
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let active = self.voices.iter().filter(|v| v.active).count() as f32;
        let n = write_meter(out, 0, &[active, self.last_pad as f32]);
        write_meter(out, n, &self.levels)
    }
}

// ── Built-in kit ─────────────────────────────────────────────────────────

fn build_kit(sr: f64) -> Vec<SampleData> {
    (0..PADS).map(|i| synth_pad(i, sr)).collect()
}

/// One-pole high-pass for noise colouring.
fn hp(x: &mut [f32], sr: f32, f: f32) {
    let a = (-std::f32::consts::TAU * f / sr).exp();
    let (mut px, mut py) = (0.0, 0.0);
    for s in x.iter_mut() {
        let y = a * (py + *s - px);
        px = *s;
        py = y;
        *s = y;
    }
}

fn synth_pad(pad: usize, sr: f64) -> SampleData {
    let sr32 = sr as f32;
    let secs = match pad {
        12 => 1.6,
        15 => 1.4,
        10 => 0.6,
        0 | 5 | 7 | 9 => 0.5,
        _ => 0.35,
    };
    let n = (secs * sr32) as usize;
    let mut out = vec![0.0f32; n];
    let mut noise = Noise::new(pad as u32 * 7919 + 1);
    let mut nbuf: Vec<f32> = (0..n).map(|_| noise.next()).collect();
    let t = |i: usize| i as f32 / sr32;
    // Pitch-swept sine: f(t) = f1 + (f0 − f1)·e^{−t/τ}.
    let sweep = |out: &mut [f32], f0: f32, f1: f32, tau: f32, decay: f32, amp: f32| {
        let mut ph = 0.0f32;
        for (i, o) in out.iter_mut().enumerate() {
            let tt = t(i);
            let f = f1 + (f0 - f1) * (-tt / tau).exp();
            ph += f / sr32;
            *o += amp * (std::f32::consts::TAU * ph).sin() * (-tt / decay).exp();
        }
    };
    match pad {
        // Kick.
        0 => sweep(&mut out, 160.0, 48.0, 0.035, 0.18, 1.0),
        // Rim.
        1 => {
            sweep(&mut out, 1_700.0, 1_700.0, 1.0, 0.012, 0.6);
            hp(&mut nbuf, sr32, 2_000.0);
            for (i, o) in out.iter_mut().enumerate() {
                *o += 0.4 * nbuf[i] * (-t(i) / 0.006).exp();
            }
        }
        // Snares.
        2 | 4 => {
            let (tone, nd) = if pad == 2 { (190.0, 0.13) } else { (230.0, 0.09) };
            sweep(&mut out, tone * 1.4, tone, 0.01, 0.06, 0.6);
            hp(&mut nbuf, sr32, 1_200.0);
            for (i, o) in out.iter_mut().enumerate() {
                *o += 0.7 * nbuf[i] * (-t(i) / nd).exp();
            }
        }
        // Clap: three short bursts then a tail.
        3 => {
            hp(&mut nbuf, sr32, 900.0);
            for (i, o) in out.iter_mut().enumerate() {
                let tt = t(i);
                let burst = [0.0, 0.011, 0.022].iter().map(|&s| if tt >= s { (-(tt - s) / 0.004).exp() } else { 0.0 }).fold(0.0f32, f32::max);
                let tail = if tt >= 0.022 { 0.6 * (-(tt - 0.022) / 0.12).exp() } else { 0.0 };
                *o += nbuf[i] * burst.max(tail);
            }
        }
        // Toms.
        5 | 7 | 9 => {
            let f = match pad {
                5 => 90.0,
                7 => 130.0,
                _ => 180.0,
            };
            sweep(&mut out, f * 1.6, f, 0.05, 0.22, 0.9);
        }
        // Hats and shaker.
        6 | 8 | 10 | 11 => {
            hp(&mut nbuf, sr32, if pad == 11 { 5_000.0 } else { 7_000.0 });
            let d = match pad {
                6 => 0.035,
                8 => 0.05,
                10 => 0.25,
                _ => 0.06,
            };
            for (i, o) in out.iter_mut().enumerate() {
                let tt = t(i);
                let env = if pad == 11 { (tt / 0.02).min(1.0) * (-tt / d).exp() } else { (-tt / d).exp() };
                *o += 0.8 * nbuf[i] * env;
            }
        }
        // Crash.
        12 => {
            hp(&mut nbuf, sr32, 4_000.0);
            for (i, o) in out.iter_mut().enumerate() {
                *o += 0.7 * nbuf[i] * (-t(i) / 0.6).exp();
            }
        }
        // Perc (wood block).
        13 => sweep(&mut out, 900.0, 800.0, 0.01, 0.04, 0.8),
        // Cowbell: two detuned squares through a decay.
        14 => {
            for (i, o) in out.iter_mut().enumerate() {
                let tt = t(i);
                let sq = |f: f32| if (tt * f).fract() < 0.5 { 1.0 } else { -1.0 };
                *o += 0.3 * (sq(540.0) + sq(800.0)) * (-tt / 0.09).exp();
            }
            // Soften the squares.
            let mut s = SvfState::default();
            let c = SvfCoeffs::new(SvfMode::Bandpass, 2_000.0, 1.0, 0.0, sr);
            for o in out.iter_mut() {
                *o = s.process(&c, *o) * 1.5;
            }
        }
        // 808: long sine with a gentle drop.
        _ => sweep(&mut out, 110.0, 50.0, 0.06, 0.6, 1.0),
    }
    // Short fade-out so nothing clicks at the end, then normalise.
    let fade = (0.005 * sr32) as usize;
    for k in 0..fade.min(n) {
        out[n - 1 - k] *= k as f32 / fade as f32;
    }
    let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    for o in &mut out {
        *o *= 0.9 / peak;
    }
    SampleData::from_interleaved(&out, 1, sr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{peak, render_notes, set_plain};

    #[test]
    fn every_pad_sounds_and_ignores_other_keys() {
        for pad in 0..PADS as u8 {
            let mut d = Drum::new();
            let (l, _) = render_notes(&mut d, &[(0, FIRST_KEY + pad, 1.0, true)], 4800);
            assert!(peak(&l) > 0.1, "pad {pad} silent");
        }
        let mut d = Drum::new();
        let (l, _) = render_notes(&mut d, &[(0, 20, 1.0, true), (0, 90, 1.0, true)], 2000);
        assert_eq!(peak(&l), 0.0);
    }

    #[test]
    fn hats_choke_each_other() {
        let mut d = Drum::new();
        // Open hat, then closed hat 50 ms later: the open hat must stop.
        let (l, _) = render_notes(&mut d, &[(0, FIRST_KEY + 10, 1.0, true), (2400, FIRST_KEY + 6, 1.0, true)], 24_000);
        // 200 ms after the closed hat both are gone (closed hat is short).
        assert!(peak(&l[12_000..]) < 0.01, "{}", peak(&l[12_000..]));
        let mut d = Drum::new();
        let (l, _) = render_notes(&mut d, &[(0, FIRST_KEY + 10, 1.0, true)], 24_000);
        assert!(peak(&l[12_000..]) > 0.01, "open hat alone rings");
    }

    #[test]
    fn velocity_and_level() {
        let mut d = Drum::new();
        let (loud, _) = render_notes(&mut d, &[(0, FIRST_KEY, 1.0, true)], 4800);
        let (soft, _) = render_notes(&mut d, &[(0, FIRST_KEY, 0.25, true)], 4800);
        assert!(peak(&soft) < 0.5 * peak(&loud));
        set_plain(&mut d, p::VEL_CURVE, 3.0);
        let (fixed, _) = render_notes(&mut d, &[(0, FIRST_KEY, 0.25, true)], 4800);
        assert!((peak(&fixed) - peak(&loud)).abs() < 1e-3);
        set_plain(&mut d, p::pad(0, p::LEVEL), -12.0);
        let (quiet, _) = render_notes(&mut d, &[(0, FIRST_KEY, 1.0, true)], 4800);
        assert!((peak(&quiet) / peak(&loud) - 0.251).abs() < 0.02);
    }

    #[test]
    fn tune_and_decay() {
        let mut d = Drum::new();
        set_plain(&mut d, p::pad(15, p::DECAY), 50.0);
        let (l, _) = render_notes(&mut d, &[(0, FIRST_KEY + 15, 1.0, true)], 24_000);
        assert!(peak(&l[9_600..]) < 0.01);
        // +12 st plays the sample twice as fast: the short kit sample ends
        // in half the time.
        let mut d = Drum::new();
        set_plain(&mut d, p::pad(13, p::TUNE), 12.0);
        let (l, _) = render_notes(&mut d, &[(0, FIRST_KEY + 13, 1.0, true)], 24_000);
        let len = d.kit[13].frames();
        assert!(peak(&l[len / 2 + 256..]) < 1e-6);
    }

    #[test]
    fn state_restores_pad_samples() {
        let dir = std::env::temp_dir().join(format!("aitrack-drum-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hit.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..4800 {
            w.write_sample(((i as f32 * 0.05).sin() * 20_000.0) as i16).unwrap();
        }
        w.finalize().unwrap();
        let mut d = Drum::new();
        d.load_pad_wav(3, path.to_str().unwrap()).unwrap();
        let state = d.save_state();
        let mut e = Drum::new();
        e.load_state(&state);
        assert_eq!(e.pad_sample(3).map(|s| s.frames()), Some(4800));
        assert!(e.pad_sample(0).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
