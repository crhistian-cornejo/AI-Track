//! `instrument.sampler` — one-sample, 16-voice multi-mode sampler.
//!
//! Modes: *Classic* (plays start→end while held, ADSR), *One-Shot* (ignores
//! note-off, plays to the end) and *Loop* (loops between the loop points with
//! a crossfade, also through the release). Root key, key tracking,
//! transpose/fine, pitch bend, glide, an SVF filter with envelope amount
//! and velocity sensitivity. Playback is resampled on the fly with cubic
//! Hermite interpolation.
//!
//! The sample is part of the state (`extra.path`) and is decoded on the
//! control thread in `load_state` / [`Sampler::load_wav`].
//!
//! Meter layout: `[active voices, playhead of the newest voice (0..1), peak]`.

use super::sample::{SampleData, load_wav};
use crate::common::{ParamStore, Segmented, drive, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::envelope::Adsr;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use serde_json::{Value, json};

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
    pending: Option<(u8, f32)>,
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
        }
    }
}

pub struct Sampler {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    sample: Option<Box<SampleData>>,
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
                tags: &["sampler", "one-shot", "loop"],
                params: params.clone(),
                inputs: 0,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr,
            sample: None,
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
        self.sample = if data.frames() > 1 { Some(Box::new(data)) } else { None };
        for v in &mut self.voices {
            v.active = false;
        }
        self.params.mark_dirty();
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

    fn note_on(&mut self, key: u8, vel: f32) {
        if vel <= 0.0 {
            self.note_off(key);
            return;
        }
        if self.sample.is_none() {
            return;
        }
        let free = (0..self.poly).find(|&i| !self.voices[i].active);
        let vi = match free {
            Some(i) => i,
            None => {
                let vi = (0..self.poly).filter(|&i| !self.voices[i].fading).min_by_key(|&i| (!self.voices[i].released as u8, self.voices[i].age)).unwrap_or(0);
                self.voices[vi].fading = true;
                self.voices[vi].pending = Some((key, vel));
                return;
            }
        };
        self.start(vi, key, vel);
    }

    fn start(&mut self, vi: usize, key: u8, vel: f32) {
        self.age += 1;
        let v = &mut self.voices[vi];
        v.active = true;
        v.released = false;
        v.fading = false;
        v.fade = 1.0;
        v.pending = None;
        v.key = key;
        v.target = key as f32;
        v.pitch = if self.glide_ms > 0.0 { self.last_pitch } else { key as f32 };
        v.pos = self.region.start;
        let vel = vel.clamp(0.0, 1.0);
        v.vel_gain = 1.0 - self.vel_sens + self.vel_sens * vel * vel;
        v.age = self.age;
        v.env.retrigger();
        v.filt = [SvfState::default(); 2];
        self.last_pitch = key as f32;
    }

    fn note_off(&mut self, key: u8) {
        if self.mode == 1 {
            return; // one-shot plays out
        }
        for v in &mut self.voices {
            if v.active && v.key == key && !v.released {
                v.env.gate_off();
                v.released = true;
            }
            if matches!(v.pending, Some((k, _)) if k == key) {
                v.pending = None;
            }
        }
    }

    fn control(&mut self, vi: usize, n: usize, sample_rate: f64) {
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
        let semis = self.key_track * (v.pitch - self.root) + self.tune + self.bend * self.bend_range;
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
        let Some(smp) = self.sample.take() else { return };
        let r = self.region;
        let looping = self.mode == 2;
        let one_shot = self.mode == 1;
        let filter = self.filter > 0;
        let mut pos = start;
        let mut peak = self.peak * (-((end - start) as f32) / (0.15 * self.sr as f32)).exp();
        while pos < end {
            let n = CHUNK.min(end - pos);
            for vi in 0..MAX_VOICES {
                if !self.voices[vi].active {
                    continue;
                }
                self.control(vi, n, smp.sample_rate);
                let (gl, gr) = (self.gain_l, self.gain_r);
                let v = &mut self.voices[vi];
                let mut done = false;
                for i in pos..pos + n {
                    let mut a = smp.read(0, v.pos);
                    let mut b = smp.read(1, v.pos);
                    if looping && r.xfade > 0.0 && v.pos >= r.loop_end - r.xfade {
                        // Crossfade the loop end into the material before the
                        // loop start, so the jump lands on identical audio.
                        let t = ((v.pos - (r.loop_end - r.xfade)) / r.xfade) as f32;
                        let p2 = v.pos - (r.loop_end - r.loop_start);
                        a = a * (1.0 - t) + smp.read(0, p2) * t;
                        b = b * (1.0 - t) + smp.read(1, p2) * t;
                    }
                    // Short fade into the end of a non-looping region.
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
                let _ = one_shot;
                if done {
                    v.active = false;
                    if let Some((key, vel)) = v.pending.take() {
                        self.start(vi, key, vel);
                    }
                }
                let v = &mut self.voices[vi];
                if !(v.filt[0].is_finite() && v.filt[1].is_finite()) {
                    v.filt = [SvfState::default(); 2];
                }
            }
            pos += n;
        }
        for ch in 0..buf.channels() {
            for x in &mut buf.channel_mut(ch)[start..end] {
                *x = sanitize(*x);
                peak = peak.max(x.abs());
            }
        }
        self.peak = peak;
        self.sample = Some(smp);
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
        save_state(&self.params, json!({ "path": path }))
    }

    fn load_state(&mut self, data: &[u8]) {
        let extra = load_state(&mut self.params, data);
        if let Some(path) = extra.get("path").and_then(Value::as_str) {
            // A missing file leaves the sampler empty rather than failing.
            let _ = self.load_wav(path);
        }
        self.params.mark_dirty();
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let n = self.voices.iter().filter(|v| v.active).count() as f32;
        let head = match (&self.sample, self.voices.iter().filter(|v| v.active).max_by_key(|v| v.age)) {
            (Some(s), Some(v)) => (v.pos / s.frames() as f64) as f32,
            _ => 0.0,
        };
        write_meter(out, 0, &[n, head, self.peak])
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
}
