//! `instrument.sampler` — 16-voice multi-mode sampler & SFZ v2 instrument player.
//!
//! Supports single-sample mode (*Classic*, *One-Shot*, *Loop*) and multi-zone
//! **SFZ v2** instruments (key ranges, velocity layers, round-robin
//! `seq_length`/`seq_position`, `trigger=release` samples, and windowed disk
//! streaming for large WAV files so multi-GB libraries load with a small RAM
//! preroll and background page streaming). Zero allocations or locks on the
//! audio thread.
//!
//! State: `extra.path` (single WAV) or `extra.sfz_path` (SFZ instrument),
//! loaded on the control thread in `load_state` / [`Sampler::load_sfz`].
//!
//! Meter layout: `[active voices, playhead of the newest voice (0..1), peak, sfz zone count]`.

use super::sample::{DEFAULT_PREROLL_FRAMES, DEFAULT_STREAM_THRESHOLD_FRAMES, SampleData, StreamBacking, StreamWorker, load_wav};
use super::sfz::{LoopMode, RegionDef, Trigger, parse_sfz};
use crate::common::{ParamStore, Segmented, drive, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::envelope::Adsr;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const ID: &str = "instrument.sampler";
pub const MAX_VOICES: usize = 16;
const CHUNK: usize = 32;
const STEAL_FADE: f32 = 1.0 / 64.0;

pub mod p {
    pub const MODE: u32 = 0;
    pub const ROOT_KEY: u32 = 1;
    pub const KEY_TRACK: u32 = 2;
    pub const TRANSPOSE: u32 = 3;
    pub const FINE: u32 = 4;
    pub const START: u32 = 5;
    pub const END: u32 = 6;
    pub const LOOP_START: u32 = 7;
    pub const LOOP_END: u32 = 8;
    pub const CROSSFADE: u32 = 9;
    pub const ATTACK: u32 = 10;
    pub const DECAY: u32 = 11;
    pub const SUSTAIN: u32 = 12;
    pub const RELEASE: u32 = 13;
    pub const FILTER_TYPE: u32 = 14;
    pub const CUTOFF: u32 = 15;
    pub const RESONANCE: u32 = 16;
    pub const FILTER_ENV: u32 = 17;
    pub const VEL_SENS: u32 = 18;
    pub const VOLUME: u32 = 19;
    pub const PAN: u32 = 20;
    pub const GLIDE: u32 = 21;
    pub const POLYPHONY: u32 = 22;
    pub const BEND_RANGE: u32 = 23;
}

pub const MODES: &[&str] = &["Classic", "One-Shot", "Loop"];
pub const FILTER_TYPES: &[&str] = &["Off", "Low-pass", "High-pass", "Band-pass"];

pub fn params() -> Vec<ParamInfo> {
    let t = |id, name, d| ParamInfo::new(id, name, 0.5, 10_000.0, d).unit("ms").log();
    vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 2.0, 0.0).choices(MODES),
        ParamInfo::new(p::ROOT_KEY, "Root Key", 0.0, 127.0, 60.0).choices(&[]).short("Root"),
        ParamInfo::new(p::KEY_TRACK, "Key Tracking", 0.0, 100.0, 100.0).unit("%").short("Key"),
        ParamInfo::new(p::TRANSPOSE, "Transpose", -24.0, 24.0, 0.0).choices(&[]).unit("st").short("Trans"),
        ParamInfo::new(p::FINE, "Fine Tune", -100.0, 100.0, 0.0).unit("ct").short("Fine"),
        ParamInfo::new(p::START, "Start", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::END, "End", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::LOOP_START, "Loop Start", 0.0, 100.0, 0.0).unit("%").short("L.Start"),
        ParamInfo::new(p::LOOP_END, "Loop End", 0.0, 100.0, 100.0).unit("%").short("L.End"),
        ParamInfo::new(p::CROSSFADE, "Loop Crossfade", 0.0, 1000.0, 10.0).unit("ms").power(2.0).short("X-Fade"),
        t(p::ATTACK, "Attack", 1.0).short("A"),
        t(p::DECAY, "Decay", 300.0).short("D"),
        ParamInfo::new(p::SUSTAIN, "Sustain", 0.0, 100.0, 100.0).unit("%").short("S"),
        t(p::RELEASE, "Release", 100.0).short("R"),
        ParamInfo::new(p::FILTER_TYPE, "Filter Type", 0.0, 3.0, 0.0).choices(FILTER_TYPES).short("Filter"),
        ParamInfo::new(p::CUTOFF, "Cutoff", 20.0, 20_000.0, 20_000.0).unit("Hz").log(),
        ParamInfo::new(p::RESONANCE, "Resonance", 0.0, 100.0, 0.0).unit("%").short("Res"),
        ParamInfo::new(p::FILTER_ENV, "Filter Env Amount", -100.0, 100.0, 0.0).unit("%").short("Env"),
        ParamInfo::new(p::VEL_SENS, "Velocity Sensitivity", 0.0, 100.0, 70.0).unit("%").short("Vel"),
        ParamInfo::new(p::VOLUME, "Volume", -48.0, 6.0, 0.0).unit("dB"),
        ParamInfo::new(p::PAN, "Pan", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::GLIDE, "Glide", 0.0, 2000.0, 0.0).unit("ms").power(2.0),
        ParamInfo::new(p::POLYPHONY, "Polyphony", 1.0, 16.0, 16.0).choices(&[]).short("Poly"),
        ParamInfo::new(p::BEND_RANGE, "Pitch Bend Range", 0.0, 24.0, 2.0).choices(&[]).unit("st").short("Bend"),
    ]
}

#[derive(Clone, Copy, Debug, Default)]
struct Region {
    start: f64,
    end: f64,
    loop_start: f64,
    loop_end: f64,
    xfade: f64,
}

#[derive(Clone)]
struct Zone {
    def: RegionDef,
    audio: Arc<StreamBacking>,
    gain_l: f32,
    gain_r: f32,
    start: f64,
    end: f64,
    loop_start: f64,
    loop_end: f64,
    looping: bool,
    one_shot: bool,
    tune_semis: f32,
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    active: bool,
    released: bool,
    key: u8,
    pitch: f32,
    target: f32,
    pos: f64,
    rate: f64,
    vel_gain: f32,
    age: u64,
    env: Adsr,
    filt: [SvfState; 2],
    coeffs: SvfCoeffs,
    fade: f32,
    fading: bool,
    pending: Option<(u8, f32, Option<usize>)>,
    zone_idx: Option<usize>,
    release_trigger: bool,
}

impl Voice {
    fn new(sr: f64) -> Self {
        Self {
            active: false,
            released: false,
            key: 60,
            pitch: 60.0,
            target: 60.0,
            pos: 0.0,
            rate: 1.0,
            vel_gain: 1.0,
            age: 0,
            env: Adsr::new(sr),
            filt: [SvfState::default(); 2],
            coeffs: SvfCoeffs::passthrough(),
            fade: 1.0,
            fading: false,
            pending: None,
            zone_idx: None,
            release_trigger: false,
        }
    }
}

pub struct Sampler {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    sample: Option<Box<SampleData>>,
    sfz_path: Option<String>,
    zones: Vec<Zone>,
    _stream_worker: Option<StreamWorker>,
    rr_counter: [u8; 128],
    last_vel: [f32; 128],
    region: Region,
    voices: [Voice; MAX_VOICES],
    mode: usize,
    root: f32,
    key_track: f32,
    tune: f32,
    filter: usize,
    cutoff: f32,
    reso: f32,
    env_amt: f32,
    vel_sens: f32,
    gain_l: f32,
    gain_r: f32,
    glide_ms: f32,
    poly: usize,
    bend_range: f32,
    bend: f32,
    last_pitch: f32,
    age: u64,
    peak: f32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Sampler::new())
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        let params = params();
        let sr = 48_000.0;
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Sampler",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["sampler", "one-shot", "loop", "sfz", "streaming"],
                params: params.clone(),
                inputs: 0,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr,
            sample: None,
            sfz_path: None,
            zones: Vec::new(),
            _stream_worker: None,
            rr_counter: [0; 128],
            last_vel: [0.8; 128],
            region: Region::default(),
            voices: [Voice::new(sr); MAX_VOICES],
            mode: 0,
            root: 60.0,
            key_track: 1.0,
            tune: 0.0,
            filter: 0,
            cutoff: 20_000.0,
            reso: 0.0,
            env_amt: 0.0,
            vel_sens: 0.7,
            gain_l: 1.0,
            gain_r: 1.0,
            glide_ms: 0.0,
            poly: MAX_VOICES,
            bend_range: 2.0,
            bend: 0.0,
            last_pitch: 60.0,
            age: 0,
            peak: 0.0,
        };
        s.prepare(sr, 512);
        s
    }

    /// Decode and load a WAV file (control thread).
    pub fn load_wav(&mut self, path: &str) -> Result<(), String> {
        let s = load_wav(path)?;
        self.set_sample_data(s);
        Ok(())
    }

    /// Load interleaved audio (control thread).
    pub fn set_sample(&mut self, interleaved: &[f32], channels: usize, sample_rate: f64) {
        self.set_sample_data(SampleData::from_interleaved(interleaved, channels, sample_rate));
    }

    pub fn set_sample_data(&mut self, data: SampleData) {
        self.zones.clear();
        self._stream_worker = None;
        self.sfz_path = None;
        self.sample = if data.frames() > 1 { Some(Box::new(data)) } else { None };
        for v in &mut self.voices {
            v.active = false;
            v.pending = None;
        }
        self.params.mark_dirty();
    }

    /// Load an SFZ v2 instrument (multi-zone key/velocity splits, round-robin,
    /// `trigger=release`, and windowed disk streaming for large WAVs) on the control thread.
    pub fn load_sfz(&mut self, path: &str) -> Result<(), String> {
        self.load_sfz_with_threshold(path, DEFAULT_STREAM_THRESHOLD_FRAMES, DEFAULT_PREROLL_FRAMES)
    }

    /// Like [`Sampler::load_sfz`] with explicit streaming threshold and preroll frames.
    pub fn load_sfz_with_threshold(&mut self, path: &str, stream_threshold: usize, preroll_frames: usize) -> Result<(), String> {
        let sfz_file = Path::new(path);
        let src = std::fs::read_to_string(sfz_file).map_err(|e| format!("{path}: {e}"))?;
        let base_dir = sfz_file.parent().unwrap_or_else(|| Path::new("."));
        let defs = parse_sfz(&src, base_dir);
        let mut cache: HashMap<PathBuf, Arc<StreamBacking>> = HashMap::new();
        let mut zones = Vec::new();
        let mut streams = Vec::new();
        for def in defs {
            let audio = if let Some(existing) = cache.get(&def.sample) {
                existing.clone()
            } else {
                let Some(s) = def.sample.to_str() else { continue };
                let Ok(b) = StreamBacking::open(s, stream_threshold, preroll_frames) else { continue };
                streams.push(b.clone());
                cache.insert(def.sample.clone(), b.clone());
                b
            };
            let frames = audio.frames();
            if frames <= 1 {
                continue;
            }
            let start = (def.offset as f64).min((frames - 2) as f64);
            let end = def.end.map_or(frames as f64, |e| (e as f64).clamp(start + 1.0, frames as f64));
            let ls = (def.loop_start as f64).clamp(start, end);
            let le = def.loop_end.map_or(end, |e| (e as f64).clamp(start, end));
            let (ls, le) = if le > ls + 8.0 { (ls, le) } else { (start, end) };
            let g = db_to_gain(def.volume_db);
            let p_norm = (def.pan / 100.0).clamp(-1.0, 1.0);
            let (pl, pr) = if p_norm >= 0.0 {
                ((p_norm * std::f32::consts::FRAC_PI_2).cos(), 1.0)
            } else {
                (1.0, (p_norm * std::f32::consts::FRAC_PI_2).cos())
            };
            let looping = matches!(def.loop_mode, LoopMode::Continuous | LoopMode::Sustain);
            let one_shot = matches!(def.loop_mode, LoopMode::OneShot);
            let tune_semis = def.transpose as f32 + def.tune_cents / 100.0;
            zones.push(Zone {
                def,
                audio,
                gain_l: g * pl,
                gain_r: g * pr,
                start,
                end,
                loop_start: ls,
                loop_end: le,
                looping,
                one_shot,
                tune_semis,
            });
        }
        if zones.is_empty() {
            return Err(format!("{path}: no playable WAV regions found"));
        }
        self._stream_worker = StreamWorker::spawn(streams);
        self.zones = zones;
        self.sample = None;
        self.sfz_path = Some(path.to_string());
        self.rr_counter.fill(0);
        for v in &mut self.voices {
            v.active = false;
            v.pending = None;
        }
        self.params.mark_dirty();
        Ok(())
    }

    pub fn zone_count(&self) -> usize {
        self.zones.len()
    }

    pub fn streamed_zone_count(&self) -> usize {
        self.zones.iter().filter(|z| z.audio.is_streamed()).count()
    }

    pub fn sfz_path(&self) -> Option<&str> {
        self.sfz_path.as_deref()
    }

    pub fn sample(&self) -> Option<&SampleData> {
        self.sample.as_deref()
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    fn update(&mut self) {
        let pr = &self.params;
        self.mode = pr.choice(p::MODE);
        self.root = pr.plain(p::ROOT_KEY).round();
        self.key_track = pr.plain(p::KEY_TRACK) / 100.0;
        self.tune = pr.plain(p::TRANSPOSE).round() + pr.plain(p::FINE) / 100.0;
        self.filter = pr.choice(p::FILTER_TYPE);
        self.cutoff = pr.plain(p::CUTOFF);
        self.reso = pr.plain(p::RESONANCE) / 100.0;
        self.env_amt = pr.plain(p::FILTER_ENV) / 100.0;
        self.vel_sens = pr.plain(p::VEL_SENS) / 100.0;
        let g = db_to_gain(pr.plain(p::VOLUME));
        let pan = pr.plain(p::PAN) / 100.0;
        let (pl, prr) = if pan >= 0.0 { ((pan * std::f32::consts::FRAC_PI_2).cos(), 1.0) } else { (1.0, (pan * std::f32::consts::FRAC_PI_2).cos()) };
        self.gain_l = g * pl;
        self.gain_r = g * prr;
        self.glide_ms = pr.plain(p::GLIDE);
        self.poly = (pr.plain(p::POLYPHONY).round() as usize).clamp(1, MAX_VOICES);
        self.bend_range = pr.plain(p::BEND_RANGE).round();
        let (a, d, s, r) = (pr.plain(p::ATTACK), pr.plain(p::DECAY), pr.plain(p::SUSTAIN) / 100.0, pr.plain(p::RELEASE));
        for v in &mut self.voices {
            v.env.set(a, d, s, r);
        }
        if let Some(smp) = &self.sample {
            let n = smp.frames() as f64;
            let start = (pr.plain(p::START) as f64 / 100.0 * n).clamp(0.0, n - 2.0);
            let end = (pr.plain(p::END) as f64 / 100.0 * n).clamp(start + 1.0, n);
            let mut ls = (pr.plain(p::LOOP_START) as f64 / 100.0 * n).clamp(start, end);
            let mut le = (pr.plain(p::LOOP_END) as f64 / 100.0 * n).clamp(start, end);
            if le < ls {
                std::mem::swap(&mut ls, &mut le);
            }
            if le - ls < 16.0 {
                ls = start;
                le = end;
            }
            let xf = (pr.plain(p::CROSSFADE) as f64 * 0.001 * smp.sample_rate).min((le - ls) * 0.5).min(ls).max(0.0);
            self.region = Region { start, end, loop_start: ls, loop_end: le, xfade: xf };
        }
    }

    fn alloc_voice(&mut self) -> Option<usize> {
        if let Some(free) = (0..self.poly).find(|&i| !self.voices[i].active) {
            return Some(free);
        }
        None
    }

    fn steal_voice(&mut self, key: u8, vel: f32, zone: Option<usize>) {
        let vi = (0..self.poly)
            .filter(|&i| !self.voices[i].fading)
            .min_by_key(|&i| (!self.voices[i].released as u8, self.voices[i].age))
            .unwrap_or(0);
        self.voices[vi].fading = true;
        self.voices[vi].pending = Some((key, vel, zone));
    }

    fn note_on(&mut self, key: u8, vel: f32) {
        if vel <= 0.0 {
            self.note_off(key);
            return;
        }
        if !self.zones.is_empty() {
            let k_idx = usize::from(key.min(127));
            self.last_vel[k_idx] = vel;
            let vel_midi = (vel.clamp(0.0, 1.0) * 127.0).round().clamp(1.0, 127.0) as u8;
            let rr = self.rr_counter[k_idx];
            self.rr_counter[k_idx] = rr.wrapping_add(1);
            for zi in 0..self.zones.len() {
                let d = &self.zones[zi].def;
                if matches!(d.trigger, Trigger::Release) {
                    continue;
                }
                if key < d.lokey || key > d.hikey || vel_midi < d.lovel || vel_midi > d.hivel {
                    continue;
                }
                if d.seq_length > 1 && ((rr % d.seq_length) + 1) != d.seq_position {
                    continue;
                }
                if let Some(vi) = self.alloc_voice() {
                    self.start(vi, key, vel, Some(zi), false);
                } else {
                    self.steal_voice(key, vel, Some(zi));
                }
            }
            return;
        }
        if self.sample.is_none() {
            return;
        }
        if let Some(vi) = self.alloc_voice() {
            self.start(vi, key, vel, None, false);
        } else {
            self.steal_voice(key, vel, None);
        }
    }

    fn start(&mut self, vi: usize, key: u8, vel: f32, zone: Option<usize>, release_trigger: bool) {
        self.age += 1;
        let start_pos = match zone {
            Some(zi) => self.zones[zi].start,
            None => self.region.start,
        };
        let v = &mut self.voices[vi];
        v.active = true;
        v.released = false;
        v.fading = false;
        v.fade = 1.0;
        v.pending = None;
        v.zone_idx = zone;
        v.release_trigger = release_trigger;
        v.key = key;
        v.target = key as f32;
        v.pitch = if self.glide_ms > 0.0 && !release_trigger { self.last_pitch } else { key as f32 };
        v.pos = start_pos;
        let vel = vel.clamp(0.0, 1.0);
        v.vel_gain = 1.0 - self.vel_sens + self.vel_sens * vel * vel;
        v.age = self.age;
        v.env.retrigger();
        v.filt = [SvfState::default(); 2];
        if !release_trigger {
            self.last_pitch = key as f32;
        }
    }

    fn note_off(&mut self, key: u8) {
        let has_zones = !self.zones.is_empty();
        if !has_zones && self.mode == 1 {
            return; // single-sample one-shot plays out
        }
        for v in &mut self.voices {
            let zone_one_shot = v.zone_idx.is_some_and(|zi| self.zones[zi].one_shot || v.release_trigger);
            if v.active && v.key == key && !v.released && !zone_one_shot {
                v.env.gate_off();
                v.released = true;
            }
            if matches!(v.pending, Some((k, _, _)) if k == key) {
                v.pending = None;
            }
        }
        if has_zones {
            let k_idx = usize::from(key.min(127));
            let vel = self.last_vel[k_idx].clamp(0.05, 1.0);
            let vel_midi = (vel * 127.0).round().clamp(1.0, 127.0) as u8;
            for zi in 0..self.zones.len() {
                let d = &self.zones[zi].def;
                if d.trigger != Trigger::Release {
                    continue;
                }
                if key < d.lokey || key > d.hikey || vel_midi < d.lovel || vel_midi > d.hivel {
                    continue;
                }
                if let Some(vi) = self.alloc_voice() {
                    self.start(vi, key, vel, Some(zi), true);
                } else {
                    self.steal_voice(key, vel, Some(zi));
                }
            }
        }
    }

    fn control(&mut self, vi: usize, n: usize, sample_rate: f64, zone_root: f32, zone_tune: f32) {
        let v = &mut self.voices[vi];
        if self.glide_ms > 0.0 && v.pitch != v.target {
            let k = 1.0 - (-(n as f32) / (self.glide_ms * 0.00025 * self.sr as f32)).exp();
            v.pitch += (v.target - v.pitch) * k;
            if (v.pitch - v.target).abs() < 0.001 {
                v.pitch = v.target;
            }
        } else {
            v.pitch = v.target;
        }
        let semis = self.key_track * (v.pitch - zone_root) + self.tune + zone_tune + self.bend * self.bend_range;
        v.rate = sample_rate / self.sr * 2f64.powf(semis as f64 / 12.0);
        if self.filter > 0 {
            let mode = match self.filter {
                1 => SvfMode::Lowpass,
                2 => SvfMode::Highpass,
                _ => SvfMode::Bandpass,
            };
            let fc = (self.cutoff * 2f32.powf(self.env_amt * v.env.value() * 6.0)).clamp(20.0, (0.45 * self.sr as f32).min(20_000.0));
            let q = std::f32::consts::FRAC_1_SQRT_2 * (20.0f32 / std::f32::consts::FRAC_1_SQRT_2).powf(self.reso);
            v.coeffs = SvfCoeffs::new(mode, fc, q, 0.0, self.sr);
        }
    }
}

impl Segmented for Sampler {
    fn event(&mut self, kind: &EventKind) {
        if self.params.take_dirty() {
            self.update();
        }
        match *kind {
            EventKind::Param { id, value } => self.params.set(id, value),
            EventKind::NoteOn { key, velocity, .. } => self.note_on(key, velocity),
            EventKind::NoteOff { key, .. } => self.note_off(key),
            EventKind::PitchBend { value, .. } => self.bend = value.clamp(-1.0, 1.0),
            EventKind::AllNotesOff | EventKind::ControlChange { cc: 120 | 123, .. } => {
                for v in &mut self.voices {
                    if v.active {
                        v.env.gate_off();
                        v.released = true;
                        v.pending = None;
                    }
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
        for ch in 0..buf.channels() {
            buf.channel_mut(ch)[start..end].fill(0.0);
        }
        if self.sample.is_none() && self.zones.is_empty() {
            return;
        }
        let smp = self.sample.take();
        let zones = std::mem::take(&mut self.zones);
        let single_region = self.region;
        let single_looping = self.mode == 2;
        let filter = self.filter > 0;
        let mut pos = start;
        let peak = self.peak * (-((end - start) as f32) / (0.15 * self.sr as f32)).exp();
        while pos < end {
            let n = CHUNK.min(end - pos);
            let mut stolen: [Option<(u8, f32, Option<usize>)>; MAX_VOICES] = [None; MAX_VOICES];
            for vi in 0..MAX_VOICES {
                if !self.voices[vi].active {
                    continue;
                }
                let zi = self.voices[vi].zone_idx;
                let (src_sr, z_root, z_tune, gl, gr, r, looping) = match (zi, &smp) {
                    (Some(z_idx), _) if z_idx < zones.len() => {
                        let z = &zones[z_idx];
                        let reg = Region { start: z.start, end: z.end, loop_start: z.loop_start, loop_end: z.loop_end, xfade: 0.0 };
                        (z.audio.sample_rate(), f32::from(z.def.pitch_keycenter), z.tune_semis, self.gain_l * z.gain_l, self.gain_r * z.gain_r, reg, z.looping)
                    }
                    (_, Some(s)) => (s.sample_rate, self.root, 0.0, self.gain_l, self.gain_r, single_region, single_looping),
                    _ => {
                        self.voices[vi].active = false;
                        continue;
                    }
                };
                self.control(vi, n, src_sr, z_root, z_tune);
                let v = &mut self.voices[vi];
                let mut done = false;
                for i in pos..pos + n {
                    let (mut a, mut b) = match (zi, &smp) {
                        (Some(z_idx), _) => (zones[z_idx].audio.read(0, v.pos), zones[z_idx].audio.read(1, v.pos)),
                        (_, Some(s)) => (s.read(0, v.pos), s.read(1, v.pos)),
                        _ => (0.0, 0.0),
                    };
                    if looping && r.xfade > 0.0 && v.pos >= r.loop_end - r.xfade {
                        let t = ((v.pos - (r.loop_end - r.xfade)) / r.xfade) as f32;
                        let p2 = v.pos - (r.loop_end - r.loop_start);
                        let (a2, b2) = match (zi, &smp) {
                            (Some(z_idx), _) => (zones[z_idx].audio.read(0, p2), zones[z_idx].audio.read(1, p2)),
                            (_, Some(s)) => (s.read(0, p2), s.read(1, p2)),
                            _ => (0.0, 0.0),
                        };
                        a = a * (1.0 - t) + a2 * t;
                        b = b * (1.0 - t) + b2 * t;
                    }
                    let tail = if looping { 1.0 } else { ((r.end - v.pos) / (32.0 * v.rate.max(1e-6))).clamp(0.0, 1.0) as f32 };
                    if filter {
                        a = v.filt[0].process(&v.coeffs, a);
                        b = v.filt[1].process(&v.coeffs, b);
                    }
                    let env = v.env.next();
                    if v.fading {
                        v.fade = (v.fade - STEAL_FADE).max(0.0);
                    }
                    let g = env * v.vel_gain * v.fade * tail;
                    let (ol, or) = buf.stereo_mut_or_mono();
                    ol[i] += a * g * gl;
                    if let Some(or) = or {
                        or[i] += b * g * gr;
                    }
                    v.pos += v.rate;
                    if looping && v.pos >= r.loop_end {
                        v.pos -= r.loop_end - r.loop_start;
                    }
                    if (!looping && v.pos >= r.end) || !v.env.is_active() || v.fade <= 0.0 {
                        done = true;
                        break;
                    }
                }
                if done {
                    v.active = false;
                    stolen[vi] = v.pending.take();
                }
                if !(v.filt[0].is_finite() && v.filt[1].is_finite()) {
                    v.filt = [SvfState::default(); 2];
                }
            }
            if stolen.iter().any(Option::is_some) {
                self.zones = zones;
                self.sample = smp;
                for (vi, pending) in stolen.into_iter().enumerate() {
                    if let Some((key, vel, z)) = pending {
                        self.start(vi, key, vel, z, false);
                    }
                }
                // Resume cleanly next chunk on remaining frames after restoring borrows:
                return self.finish_render_tail(buf, start, end, peak);
            }
            pos += n;
        }
        self.zones = zones;
        self.sample = smp;
        self.finish_render_tail(buf, start, end, peak);
    }
}

impl Sampler {
    fn finish_render_tail(&mut self, buf: &mut aitrack_dsp::AudioBuffer, start: usize, end: usize, mut peak: f32) {
        for ch in 0..buf.channels() {
            for x in &mut buf.channel_mut(ch)[start..end] {
                *x = sanitize(*x);
                peak = peak.max(x.abs());
            }
        }
        self.peak = peak;
    }
}

/// Mono-or-stereo access to an output buffer.
trait StereoOrMono {
    fn stereo_mut_or_mono(&mut self) -> (&mut [f32], Option<&mut [f32]>);
}

impl StereoOrMono for aitrack_dsp::AudioBuffer {
    #[inline]
    fn stereo_mut_or_mono(&mut self) -> (&mut [f32], Option<&mut [f32]>) {
        if self.channels() >= 2 {
            let (l, r) = self.stereo_mut();
            (l, Some(r))
        } else {
            (self.channel_mut(0), None)
        }
    }
}

impl Processor for Sampler {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for v in &mut self.voices {
            v.env.set_sample_rate(sample_rate);
        }
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.active = false;
            v.pending = None;
            v.env.reset();
            v.filt = [SvfState::default(); 2];
        }
        self.peak = 0.0;
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
        ((self.params.plain(p::RELEASE) / 1000.0 * 1.2 + 0.05) as f64 * self.sr) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        let path = self.sample.as_ref().and_then(|s| s.path.clone());
        save_state(&self.params, json!({ "path": path, "sfz_path": self.sfz_path }))
    }

    fn load_state(&mut self, data: &[u8]) {
        let extra = load_state(&mut self.params, data);
        if let Some(sfz) = extra.get("sfz_path").and_then(Value::as_str) {
            let _ = self.load_sfz(sfz);
        } else if let Some(path) = extra.get("path").and_then(Value::as_str) {
            // A missing file leaves the sampler empty rather than failing.
            let _ = self.load_wav(path);
        }
        self.params.mark_dirty();
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let n = self.voices.iter().filter(|v| v.active).count() as f32;
        let head = match (&self.sample, self.voices.iter().filter(|v| v.active).max_by_key(|v| v.age)) {
            (Some(s), Some(v)) => (v.pos / s.frames() as f64) as f32,
            (None, Some(v)) => v
                .zone_idx
                .and_then(|zi| self.zones.get(zi))
                .map_or(0.0, |z| (v.pos / z.audio.frames().max(1) as f64) as f32),
            _ => 0.0,
        };
        write_meter(out, 0, &[n, head, self.peak, self.zones.len() as f32])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, render_notes, rms, set_plain};

    fn sine_sample(freq: f64, sr: f64, secs: f64) -> Vec<f32> {
        (0..(sr * secs) as usize).map(|i| (std::f64::consts::TAU * freq * i as f64 / sr).sin() as f32 * 0.8).collect()
    }

    fn pitch_hz(x: &[f32]) -> f32 {
        let c: Vec<f32> = (1..x.len()).filter(|&i| x[i - 1] <= 0.0 && x[i] > 0.0).map(|i| i as f32 - 1.0 + x[i - 1] / (x[i - 1] - x[i])).collect();
        (c.len() - 1) as f32 / (c[c.len() - 1] - c[0]) * SR as f32
    }

    #[test]
    fn silent_without_a_sample() {
        let mut s = Sampler::new();
        let (l, _) = render_notes(&mut s, &[(0, 60, 1.0, true)], 4800);
        assert_eq!(rms(&l), 0.0);
    }

    #[test]
    fn root_key_and_resampling() {
        let mut s = Sampler::new();
        // 440 Hz recorded at 44.1 kHz, root A4: A5 must play 880 Hz at 48 kHz.
        s.set_sample(&sine_sample(440.0, 44_100.0, 2.0), 1, 44_100.0);
        set_plain(&mut s, p::ROOT_KEY, 69.0);
        let (l, r) = render_notes(&mut s, &[(0, 81, 1.0, true)], 24_000);
        assert!((pitch_hz(&l[2000..]) - 880.0).abs() < 1.0);
        assert!((l[5000] - r[5000]).abs() < 1e-6, "mono fans out");
        // Key tracking off: every key plays the root pitch.
        let mut s = Sampler::new();
        s.set_sample(&sine_sample(440.0, 44_100.0, 2.0), 1, 44_100.0);
        set_plain(&mut s, p::ROOT_KEY, 69.0);
        set_plain(&mut s, p::KEY_TRACK, 0.0);
        let (l, _) = render_notes(&mut s, &[(0, 40, 1.0, true)], 24_000);
        assert!((pitch_hz(&l[2000..]) - 440.0).abs() < 1.0);
    }

    #[test]
    fn classic_stops_at_end_and_one_shot_ignores_note_off() {
        let mut s = Sampler::new();
        s.set_sample(&sine_sample(200.0, 48_000.0, 0.25), 1, 48_000.0);
        let (l, _) = render_notes(&mut s, &[(0, 60, 1.0, true)], 24_000);
        assert!(rms(&l[1000..10_000]) > 0.1);
        assert_eq!(rms(&l[13_000..]), 0.0);
        assert_eq!(s.active_voices(), 0);

        let mut s = Sampler::new();
        s.set_sample(&sine_sample(200.0, 48_000.0, 0.25), 1, 48_000.0);
        set_plain(&mut s, p::MODE, 1.0);
        set_plain(&mut s, p::RELEASE, 5.0);
        let (l, _) = render_notes(&mut s, &[(0, 60, 1.0, true), (1000, 60, 0.0, false)], 24_000);
        assert!(rms(&l[5000..10_000]) > 0.1, "one-shot keeps playing");
    }

    #[test]
    fn loop_crossfade_is_continuous() {
        let mut s = Sampler::new();
        // 100 Hz, loop points that do not fall on whole cycles.
        s.set_sample(&sine_sample(100.0, 48_000.0, 1.0), 1, 48_000.0);
        set_plain(&mut s, p::MODE, 2.0);
        set_plain(&mut s, p::LOOP_START, 30.0);
        set_plain(&mut s, p::LOOP_END, 61.37);
        set_plain(&mut s, p::CROSSFADE, 50.0);
        let (l, _) = render_notes(&mut s, &[(0, 60, 1.0, true)], 96_000);
        let worst = l[100..].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.03, "step {worst}");
        assert!(rms(&l[90_000..]) > 0.1, "still looping");
        // Without crossfade there is a step at the loop point.
        let mut s = Sampler::new();
        s.set_sample(&sine_sample(100.0, 48_000.0, 1.0), 1, 48_000.0);
        set_plain(&mut s, p::MODE, 2.0);
        set_plain(&mut s, p::LOOP_START, 30.0);
        set_plain(&mut s, p::LOOP_END, 61.37);
        set_plain(&mut s, p::CROSSFADE, 0.0);
        let (l, _) = render_notes(&mut s, &[(0, 60, 1.0, true)], 96_000);
        let worst = l[100..].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(worst > 0.1, "expected a click without crossfade: {worst}");
    }

    #[test]
    fn state_round_trip_reloads_the_wav() {
        let dir = std::env::temp_dir().join(format!("aitrack-sampler-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for v in sine_sample(300.0, 48_000.0, 0.5) {
            w.write_sample((v * 32_000.0) as i16).unwrap();
        }
        w.finalize().unwrap();
        let mut s = Sampler::new();
        s.load_wav(path.to_str().unwrap()).unwrap();
        set_plain(&mut s, p::ROOT_KEY, 72.0);
        let state = s.save_state();
        let mut t = Sampler::new();
        t.load_state(&state);
        assert_eq!(t.sample().map(|s| s.frames()), Some(24_000));
        let (l, _) = render_notes(&mut t, &[(0, 72, 1.0, true)], 12_000);
        assert!((pitch_hz(&l[1000..]) - 300.0).abs() < 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sfz_v2_zones_round_robin_release_trigger_and_disk_streaming() {
        let dir = std::env::temp_dir().join(format!("aitrack-sfz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write_tone = |name: &str, freq: f64, frames: usize, amp: f32| {
            let p = dir.join(name);
            let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
            let mut w = hound::WavWriter::create(&p, spec).unwrap();
            for i in 0..frames {
                let s = (std::f64::consts::TAU * freq * i as f64 / 48_000.0).sin() as f32 * amp;
                w.write_sample((s * 30_000.0) as i16).unwrap();
            }
            w.finalize().unwrap();
        };
        write_tone("rr1.wav", 220.0, 20_000, 0.7);
        write_tone("rr2.wav", 440.0, 20_000, 0.7);
        write_tone("rel.wav", 880.0, 8_000, 0.6);
        write_tone("long_stream.wav", 330.0, 48_000, 0.7);

        let sfz_path = dir.join("piano.sfz");
        std::fs::write(
            &sfz_path,
            r#"
<group> lokey=60 hikey=60 pitch_keycenter=60 seq_length=2
<region> seq_position=1 sample=rr1.wav
<region> seq_position=2 sample=rr2.wav
<group> lokey=60 hikey=60 trigger=release pitch_keycenter=60
<region> sample=rel.wav
<group> lokey=62 hikey=62 pitch_keycenter=62
<region> sample=long_stream.wav
"#,
        )
        .unwrap();

        let mut s = Sampler::new();
        // Force streaming threshold at 24_000 frames with 4_096 preroll so `long_stream.wav` opens in streamed mode!
        s.load_sfz_with_threshold(sfz_path.to_str().unwrap(), 24_000, 4_096).unwrap();
        assert_eq!(s.zone_count(), 4);
        assert_eq!(s.streamed_zone_count(), 1, "large zone stays streamed with small RAM preroll");
        assert!(s.zones[3].audio.preroll_frames() < s.zones[3].audio.frames());

        // 1st hit C4 -> rr1 (220 Hz)
        let (l1, _) = render_notes(&mut s, &[(0, 60, 0.8, true), (6_000, 60, 0.0, false)], 12_000);
        assert!((pitch_hz(&l1[500..4_500]) - 220.0).abs() < 4.0);
        // After NoteOff at frame 6000, release trigger (880 Hz) fires cleanly!
        assert!((pitch_hz(&l1[7_000..10_000]) - 880.0).abs() < 12.0, "release trigger plays 880 Hz");

        // 2nd hit C4 -> rr2 (440 Hz)
        let (l2, _) = render_notes(&mut s, &[(0, 60, 0.8, true)], 6_000);
        assert!((pitch_hz(&l2[500..4_500]) - 440.0).abs() < 6.0, "round-robin cycles to rr2");

        // Streamed zone D4 (62): plays preroll and seamlessly reads past preroll via background page streaming.
        let (ls, _) = render_notes(&mut s, &[(0, 62, 0.9, true)], 32_000);
        // Wait briefly for lookahead page & render tail block if background reader was mid-page:
        std::thread::sleep(std::time::Duration::from_millis(15));
        let mut tail_buf = aitrack_dsp::AudioBuffer::new(2, 1024);
        let tr = aitrack_dsp::Transport::default();
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut tail_buf, sidechain: None, events: &[], transport: &tr };
        s.process(&mut ctx);
        assert!(rms(&ls[500..3_500]) > 0.1, "streamed zone preroll audible immediately");
        assert!(rms(tail_buf.channel(0)) > 0.1, "streamed zone continues past preroll without silence");

        // State save/restore reloads SFZ cleanly.
        let st = s.save_state();
        let mut restored = Sampler::new();
        restored.load_state(&st);
        assert_eq!(restored.zone_count(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
