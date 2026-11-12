//! `instrument.vault` — one instrument, several classic synth engines,
//! picked per preset, with four macros and a browsable factory library.
//!
//! # Engines ([`ENGINES`])
//!
//! | # | engine | module |
//! |---|---|---|
//! | 0 | Mono Ladder: 3 osc + noise, nonlinear self-oscillating 4-pole ladder, glide, legato | [`ladder`] |
//! | 1 | Poly Analog: 2 osc/voice, sync, poly-mod, 24 dB OTA filter, drift, chorus I/II | [`poly`] |
//! | 2 | FM6: 6 operators, 12 algorithms, feedback, pitch envelope | [`fm`] |
//! | 3 | Electric Piano: modal tine / reed + pickup model, tremolo / auto-pan | [`epiano`] |
//! | 4 | Drawbar Organ: 9 drawbars, percussion, key click, vibrato, drive, rotary | [`organ`] |
//! | 5 | String Machine: phase-locked divide-down registers, swell filter, ensemble | [`strings`] |
//! | 6 | Wavetable: the stock wavetable synth, hosted (its ids at [`WT_BASE`]` + id`) | [`crate::instrument::wavetable`] |
//!
//! After the engine, shared by all: drive, low/high shelf, tempo-synced
//! ping-pong delay, reverb and volume (the wavetable synth's compact
//! effect chain).
//!
//! # Real-time behaviour
//!
//! Every engine is allocated when the instrument is created (and again in
//! `prepare` if the sample rate changes), so switching engine — by the
//! `ENGINE` parameter, a preset or automation — never allocates: it is a
//! parameter change. The old engine is released and faded out over 20 ms
//! while the new one starts, then reset. Engine switches take effect at
//! block starts; every other parameter is sample-accurate for the native
//! engines and block-accurate for the hosted wavetable synth.
//!
//! # Parameters
//!
//! See [`params`] for the id layout. All of it is in plain parameters, so a
//! preset is an ordinary stock preset (`presets/instrument.vault/<Type>/<Name>.json`)
//! and loads through the same command path as every other device.
//!
//! # Macros
//!
//! Four macro knobs ([`macro_knob`]) with a depth each ([`macro_depth`]).
//! The mapping is per engine: [`macros::macros`]`(engine)` gives each
//! macro's name and its [`macros::MacroTarget`]s (parameter, offset range,
//! curve). Engines read *effective* values (base + macro offsets, see
//! [`Vault::effective`]); the base values stay what the knobs show.
//! Macro 4 is "Space" on every engine.
//!
//! # Browser metadata
//!
//! A factory preset's folder is its sound type ([`TYPES`]); its tags hold
//! the engine tag ([`ENGINE_TAGS`]) plus character tags ([`CHARACTERS`]).
//! [`preset_info`] reads all of it (and the macro names) for a browser;
//! favourites live on the UI side.
//!
//! # API for an editor
//!
//! * [`ENGINES`], [`params`] (ids, choices), [`macros::macros`],
//!   [`fm::ALGORITHMS`], [`organ::FOOTAGE_RATIOS`].
//! * [`Vault::engine`], [`Vault::effective`], [`Vault::active_voices`].
//! * Meter layout: `[engine, active voices, peak L, peak R, macro 1..4
//!   (0..1), held keys]`.

pub mod epiano;
pub mod fm;
#[doc(hidden)]
pub mod gain;
pub mod kit;
pub mod ladder;
pub mod macros;
pub mod organ;
pub mod params;
pub mod poly;
pub mod strings;

pub use params::{ENGINE_COUNT, ENGINE_TAGS, ENGINES, PARAM_SPACE, WT_BASE, engine, macro_depth, macro_knob, wt};

use super::synth_fx::{FxSettings, SynthFx};
use super::wavetable::WavetableSynth;
use crate::common::{sanitize, write_meter};
use aitrack_dsp::lfo::division_seconds;
use aitrack_dsp::{AudioBuffer, Category, Event, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use kit::Perf;
use params::Params;
use serde_json::{Map, Value};

pub const ID: &str = "instrument.vault";

/// Sound types (preset folders), for browser filters.
pub const TYPES: &[&str] = &["Bass", "Lead", "Pad", "Keys", "Pluck", "Brass", "Strings", "Organ", "FX"];
/// Character tags used by the factory presets, for browser filters.
pub const CHARACTERS: &[&str] = &["warm", "bright", "dark", "aggressive", "vintage", "evolving", "soft", "clean", "dirty", "wide", "punchy", "glassy", "airy", "metallic", "fat"];

/// Frames rendered at once by the native engines.
const CHUNK: usize = 1024;
/// Make-up gain of the hosted wavetable synth (+6 dB).
const WT_GAIN: f32 = 2.0;
/// Engine crossfade length.
const SWITCH_MS: f32 = 20.0;

/// What every engine implements (internal).
pub(crate) trait Engine {
    fn prepare(&mut self, sr: f64);
    fn reset(&mut self);
    /// Read settings from the effective parameters (control side of the block).
    fn update(&mut self, p: &Params);
    fn note_on(&mut self, key: u8, vel: f32, perf: &Perf);
    fn note_off(&mut self, key: u8);
    fn release_all(&mut self);
    /// Add one chunk (≤ 4096 frames) into `l`/`r`.
    fn render(&mut self, perf: &Perf, l: &mut [f32], r: &mut [f32]);
    fn active(&self) -> usize;
    fn tail_secs(&self) -> f32;
    /// Keys currently gated (for tests and meters); returns how many were written.
    fn held_keys(&self, out: &mut [u8]) -> usize;
}

/// Every parameter.
pub fn param_infos() -> Vec<ParamInfo> {
    params::all().to_vec()
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Vault::new())
}

pub struct Vault {
    info: PluginInfo,
    params: Params,
    sr: f64,
    max_frames: usize,
    engine: usize,
    prev: Option<usize>,
    fade: f32,
    fade_step: f32,
    ladder: ladder::Ladder,
    poly: poly::Poly,
    fm: fm::Fm,
    ep: epiano::EPiano,
    organ: organ::Organ,
    strings: strings::Strings,
    wt: Box<WavetableSynth>,
    wt_buf: AudioBuffer,
    wt_events: Vec<Event>,
    wt_kill: bool,
    fx: SynthFx,
    fx_settings: FxSettings,
    delay_div: usize,
    volume: f32,
    gain: f32,
    perf: Perf,
    bend: f32,
    sustain: bool,
    sustained: [bool; 128],
    buf: [Vec<f32>; 2],
    prev_buf: [Vec<f32>; 2],
    peak: [f32; 2],
}

impl Default for Vault {
    fn default() -> Self {
        Self::new()
    }
}

impl Vault {
    pub fn new() -> Self {
        kit::init();
        let sr = 48_000.0;
        let mut wt = Box::new(WavetableSynth::new());
        wt.prepare(sr, 512);
        let mut v = Self {
            info: PluginInfo {
                id: ID,
                name: "Vault",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["synth", "analog", "fm", "electric piano", "organ", "strings", "presets", "macros"],
                params: params::all().to_vec(),
                inputs: 0,
                outputs: 2,
                has_sidechain: false,
            },
            params: Params::new(),
            sr,
            max_frames: 512,
            engine: 0,
            prev: None,
            fade: 0.0,
            fade_step: 1.0 / (SWITCH_MS * 0.001 * sr as f32),
            ladder: ladder::Ladder::new(sr),
            poly: poly::Poly::new(sr),
            fm: fm::Fm::new(sr),
            ep: epiano::EPiano::new(sr),
            organ: organ::Organ::new(sr),
            strings: strings::Strings::new(sr),
            wt,
            wt_buf: AudioBuffer::new(2, 512),
            wt_events: Vec::with_capacity(1024),
            wt_kill: false,
            fx: SynthFx::new(sr),
            fx_settings: FxSettings::default(),
            delay_div: 7,
            volume: 1.0,
            gain: 1.0,
            perf: Perf { polyphony: 16, sr: sr as f32, ..Default::default() },
            bend: 0.0,
            sustain: false,
            sustained: [false; 128],
            buf: [vec![0.0; CHUNK], vec![0.0; CHUNK]],
            prev_buf: [vec![0.0; CHUNK], vec![0.0; CHUNK]],
            peak: [0.0; 2],
        };
        v.engine = v.params.choice(params::ENGINE);
        v.update();
        v
    }

    /// The engine playing now (index into [`ENGINES`]).
    pub fn engine(&self) -> usize {
        self.engine
    }

    /// A parameter's value after the macros (normalised).
    pub fn effective(&self, id: u32) -> f32 {
        self.params.norm(id)
    }

    pub fn active_voices(&self) -> usize {
        let mut n = self.native_active(self.engine);
        if self.engine == engine::WAVETABLE {
            n = self.wt.active_voices();
        }
        n
    }

    /// Keys the current engine holds (tests, meters).
    pub fn held_keys(&self, out: &mut [u8]) -> usize {
        self.with_engine_ref(self.engine, |e| e.held_keys(out)).unwrap_or(0)
    }

    fn native_active(&self, e: usize) -> usize {
        self.with_engine_ref(e, |e| e.active()).unwrap_or(0)
    }

    fn with_engine_ref<R>(&self, e: usize, f: impl FnOnce(&dyn Engine) -> R) -> Option<R> {
        Some(match e {
            engine::LADDER => f(&self.ladder),
            engine::POLY => f(&self.poly),
            engine::FM => f(&self.fm),
            engine::EP => f(&self.ep),
            engine::ORGAN => f(&self.organ),
            engine::STRINGS => f(&self.strings),
            _ => return None,
        })
    }

    fn engine_mut(&mut self, e: usize) -> Option<&mut dyn Engine> {
        Some(match e {
            engine::LADDER => &mut self.ladder,
            engine::POLY => &mut self.poly,
            engine::FM => &mut self.fm,
            engine::EP => &mut self.ep,
            engine::ORGAN => &mut self.organ,
            engine::STRINGS => &mut self.strings,
            _ => return None,
        })
    }

    /// Recompute the effective values (macros) and push them to the engines.
    fn update(&mut self) {
        let p = &mut self.params;
        p.reset_eff(0..PARAM_SPACE);
        let target = p.choice(params::ENGINE).min(ENGINE_COUNT - 1);
        for (m, def) in macros::macros(target).iter().enumerate() {
            let x = p.base(macro_knob(m as u32));
            let depth = p.base_plain(macro_depth(m as u32)) / 100.0;
            if x <= 0.0 || depth == 0.0 {
                continue;
            }
            for t in def.targets {
                p.add_eff(t.param, macros::offset(t, x, depth));
            }
        }
        // Engine switch.
        if target != self.engine {
            let old = self.engine;
            if let Some(prev) = self.prev.take() {
                // A switch during a fade: drop the fading engine at once.
                self.reset_engine(prev);
            }
            if old == engine::WAVETABLE {
                self.wt_kill = true;
            } else if let Some(e) = self.engine_mut(old) {
                e.release_all();
            }
            self.prev = Some(old);
            self.fade = 1.0;
            self.engine = target;
            self.reset_engine(target);
            self.sustained = [false; 128];
        }
        let p = &self.params;
        for e in 0..engine::WAVETABLE {
            match e {
                engine::LADDER => self.ladder.update(p),
                engine::POLY => self.poly.update(p),
                engine::FM => self.fm.update(p),
                engine::EP => self.ep.update(p),
                engine::ORGAN => self.organ.update(p),
                _ => self.strings.update(p),
            }
        }
        if self.engine == engine::WAVETABLE || self.prev == Some(engine::WAVETABLE) {
            for info in params::all().iter().filter(|i| i.id >= WT_BASE) {
                self.wt.set_param(info.id - WT_BASE, p.norm(info.id));
            }
        }
        self.perf.polyphony = p.choice(params::POLYPHONY) + 1;
        self.perf.bend = self.bend * p.plain(params::BEND_RANGE);
        let f = &mut self.fx_settings;
        let drive = p.plain(params::DRIVE) / 100.0;
        f.dist_mode = if drive > 0.0 { 1 } else { 0 };
        f.dist_drive = drive;
        f.dist_mix = 1.0;
        f.eq_low = p.plain(params::EQ_LOW);
        f.eq_mid = 0.0;
        f.eq_high = p.plain(params::EQ_HIGH);
        f.chorus_mix = 0.0;
        f.delay_mix = p.plain(params::DELAY_MIX) / 100.0;
        f.delay_feedback = p.plain(params::DELAY_FEEDBACK) / 100.0 * 0.9;
        f.reverb_mix = p.plain(params::REVERB_MIX) / 100.0;
        f.reverb_decay = p.plain(params::REVERB_DECAY);
        f.reverb_tone = 6000.0;
        self.delay_div = params::DELAY_DIV[p.choice(params::DELAY_TIME).min(7)];
        self.volume = db_to_gain(p.plain(params::VOLUME));
    }

    fn reset_engine(&mut self, e: usize) {
        if e == engine::WAVETABLE {
            self.wt.reset();
        } else if let Some(x) = self.engine_mut(e) {
            x.reset();
        }
    }

    fn note_on(&mut self, key: u8, vel: f32) {
        let key = key.min(127);
        self.sustained[key as usize] = false;
        let perf = self.perf;
        if let Some(e) = self.engine_mut(self.engine) {
            e.note_on(key, vel.clamp(0.0, 1.0), &perf);
        }
    }

    fn note_off(&mut self, key: u8) {
        let key = key.min(127);
        if self.sustain {
            self.sustained[key as usize] = true;
            return;
        }
        if let Some(e) = self.engine_mut(self.engine) {
            e.note_off(key);
        }
    }

    fn set_sustain(&mut self, down: bool) {
        self.sustain = down;
        if !down {
            for k in 0..128u8 {
                if std::mem::take(&mut self.sustained[k as usize])
                    && let Some(e) = self.engine_mut(self.engine)
                {
                    e.note_off(k);
                }
            }
        }
    }

    fn all_notes_off(&mut self) {
        self.sustained = [false; 128];
        if let Some(e) = self.engine_mut(self.engine) {
            e.release_all();
        }
    }

    fn event(&mut self, kind: &EventKind) {
        match *kind {
            EventKind::Param { id, value } => {
                if id != params::ENGINE {
                    self.params.set(id, value);
                }
            }
            EventKind::NoteOn { key, velocity, .. } => {
                if self.params.take_dirty() {
                    self.update();
                }
                if velocity <= 0.0 {
                    self.note_off(key);
                } else {
                    self.note_on(key, velocity);
                }
            }
            EventKind::NoteOff { key, .. } => self.note_off(key),
            EventKind::PitchBend { value, .. } => {
                self.bend = value.clamp(-1.0, 1.0);
                self.perf.bend = self.bend * self.params.plain(params::BEND_RANGE);
            }
            EventKind::ControlChange { cc: 1, value, .. } => self.perf.mod_wheel = value.clamp(0.0, 1.0),
            EventKind::ControlChange { cc: 64, value, .. } => self.set_sustain(value >= 0.5),
            EventKind::ControlChange { cc: 120 | 123, .. } | EventKind::AllNotesOff => self.all_notes_off(),
            EventKind::ChannelPressure { value, .. } => self.perf.aftertouch = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    /// Render frames `start..end` of the native engines into the output.
    fn render_native(&mut self, out_l: &mut [f32], out_r: &mut [f32], start: usize, end: usize) {
        let mut pos = start;
        while pos < end {
            let n = (end - pos).min(CHUNK);
            let perf = self.perf;
            let cur = self.engine;
            {
                let [bl, br] = &mut self.buf;
                bl[..n].fill(0.0);
                br[..n].fill(0.0);
            }
            if cur != engine::WAVETABLE {
                let (mut bl, mut br) = (std::mem::take(&mut self.buf[0]), std::mem::take(&mut self.buf[1]));
                if let Some(e) = self.engine_mut(cur) {
                    e.render(&perf, &mut bl[..n], &mut br[..n]);
                }
                self.buf = [bl, br];
            }
            let mut fading = false;
            if let Some(prev) = self.prev
                && prev != engine::WAVETABLE
            {
                fading = true;
                let (mut pl, mut pr) = (std::mem::take(&mut self.prev_buf[0]), std::mem::take(&mut self.prev_buf[1]));
                pl[..n].fill(0.0);
                pr[..n].fill(0.0);
                if let Some(e) = self.engine_mut(prev) {
                    e.render(&perf, &mut pl[..n], &mut pr[..n]);
                }
                self.prev_buf = [pl, pr];
            }
            let wt_gain = |i: usize, s: &Self| -> f32 {
                if s.engine == engine::WAVETABLE {
                    1.0
                } else if s.prev == Some(engine::WAVETABLE) {
                    (s.fade - s.fade_step * i as f32).max(0.0)
                } else {
                    0.0
                }
            };
            let use_wt = self.engine == engine::WAVETABLE || self.prev == Some(engine::WAVETABLE);
            let wt_frames = self.wt_buf.frames();
            for i in 0..n {
                let mut l = self.buf[0][i];
                let mut r = self.buf[1][i];
                if fading {
                    let g = (self.fade - self.fade_step * i as f32).max(0.0);
                    l += self.prev_buf[0][i] * g;
                    r += self.prev_buf[1][i] * g;
                }
                let at = pos + i;
                if use_wt && at < wt_frames {
                    // The hosted synth leaves chord headroom; bring it to the library level.
                    let g = wt_gain(i, self) * WT_GAIN;
                    l += self.wt_buf.channel(0)[at] * g;
                    r += self.wt_buf.channel(1)[at] * g;
                }
                let (l, r) = if self.fx_settings.active() { self.fx.process(l, r, &self.fx_settings) } else { (l, r) };
                self.gain += (self.volume - self.gain) * 0.002;
                let (l, r) = (kit::flush(sanitize(l * self.gain)), kit::flush(sanitize(r * self.gain)));
                out_l[at] = l;
                out_r[at] = r;
                self.peak[0] = self.peak[0].max(l.abs());
                self.peak[1] = self.peak[1].max(r.abs());
            }
            if self.prev.is_some() {
                self.fade -= self.fade_step * n as f32;
                if self.fade <= 0.0 {
                    let prev = self.prev.take().unwrap_or(self.engine);
                    self.reset_engine(prev);
                    self.fade = 0.0;
                }
            }
            pos += n;
        }
    }

    /// Run the hosted wavetable synth over the whole block (when it plays).
    fn render_wavetable(&mut self, ctx: &ProcessContext<'_>) {
        let frames = ctx.buffer.frames();
        if self.engine != engine::WAVETABLE && self.prev != Some(engine::WAVETABLE) {
            return;
        }
        if self.wt_buf.frames() != frames {
            if frames > self.max_frames {
                // Larger than prepared: render nothing from the hosted synth
                // rather than allocate.
                self.wt_buf.set_frames(0);
                return;
            }
            self.wt_buf.set_frames(frames);
        }
        self.wt_buf.clear();
        self.wt_events.clear();
        if std::mem::take(&mut self.wt_kill) && self.wt_events.len() < self.wt_events.capacity() {
            self.wt_events.push(Event { offset: 0, kind: EventKind::AllNotesOff });
        }
        if self.engine == engine::WAVETABLE {
            for ev in ctx.events {
                let pass = !matches!(ev.kind, EventKind::Param { .. });
                if pass && self.wt_events.len() < self.wt_events.capacity() {
                    self.wt_events.push(*ev);
                }
            }
        }
        let mut sub = ProcessContext { sample_rate: ctx.sample_rate, buffer: &mut self.wt_buf, sidechain: None, events: &self.wt_events, transport: ctx.transport };
        self.wt.process(&mut sub);
    }
}

impl Processor for Vault {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, max_frames: usize) {
        if sample_rate != self.sr {
            self.ladder = ladder::Ladder::new(sample_rate);
            self.poly.prepare(sample_rate);
            self.fm.prepare(sample_rate);
            self.ep.prepare(sample_rate);
            self.organ.prepare(sample_rate);
            self.strings.prepare(sample_rate);
            self.fx = SynthFx::new(sample_rate);
        }
        self.sr = sample_rate;
        self.max_frames = max_frames.max(1);
        self.wt.prepare(sample_rate, self.max_frames);
        self.wt_buf = AudioBuffer::new(2, self.max_frames);
        self.perf.sr = sample_rate as f32;
        self.fade_step = 1.0 / (SWITCH_MS * 0.001 * sample_rate as f32);
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for e in 0..ENGINE_COUNT {
            self.reset_engine(e);
        }
        self.prev = None;
        self.fade = 0.0;
        self.fx.reset();
        self.sustain = false;
        self.sustained = [false; 128];
        self.peak = [0.0; 2];
        self.gain = self.volume;
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        // Engine changes apply at the block start.
        for ev in ctx.events {
            if let EventKind::Param { id: params::ENGINE, value } = ev.kind {
                self.params.set(params::ENGINE, value);
            }
        }
        if self.params.take_dirty() {
            self.update();
        }
        let bpm = ctx.transport.bpm;
        let max = self.fx.max_delay_samples();
        self.fx_settings.delay_samples = ((division_seconds(self.delay_div, bpm) * self.sr) as f32).clamp(1.0, max);
        self.render_wavetable(ctx);
        let frames = ctx.buffer.frames();
        let events = ctx.events;
        let mut start = 0;
        let buffer = &mut *ctx.buffer;
        if buffer.channels() < 2 {
            // The engine always runs stereo; a mono bus gets silence.
            buffer.clear();
            return;
        }
        let (out_l, out_r) = buffer.stereo_mut();
        for ev in events {
            let off = (ev.offset as usize).min(frames);
            if off > start {
                self.render_native(out_l, out_r, start, off);
                start = off;
            }
            self.event(&ev.kind);
            if self.params.take_dirty() {
                self.update();
            }
        }
        if start < frames {
            self.render_native(out_l, out_r, start, frames);
        }
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.base(id)
    }

    fn tail(&self) -> u32 {
        let mut secs = if self.engine == engine::WAVETABLE { self.wt.tail() as f32 / self.sr as f32 } else { self.with_engine_ref(self.engine, |e| e.tail_secs()).unwrap_or(0.0) };
        if self.fx_settings.delay_mix > 0.0 {
            secs += 3.0;
        }
        if self.fx_settings.reverb_mix > 0.0 {
            secs += self.fx_settings.reverb_decay;
        }
        (secs.min(60.0) as f64 * self.sr) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        let mut params = Map::new();
        for info in params::all() {
            params.insert(info.id.to_string(), Value::from(self.params.base(info.id) as f64));
        }
        let mut root = Map::new();
        root.insert("version".into(), Value::from(1));
        root.insert("params".into(), Value::Object(params));
        // The wavetable synth's user tables and drawn LFO curves.
        if let Ok(v) = serde_json::from_slice::<Value>(&self.wt.save_state())
            && let Some(extra) = v.get("extra")
        {
            root.insert("extra".into(), serde_json::json!({ "wavetable": { "version": 1, "extra": extra } }));
        }
        serde_json::to_vec(&Value::Object(root)).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) {
        let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(data) else { return };
        if let Some(Value::Object(ps)) = root.get("params") {
            for (k, v) in ps {
                if let (Ok(id), Some(n)) = (k.parse::<u32>(), v.as_f64()) {
                    self.params.set(id, n as f32);
                }
            }
        }
        if let Some(w) = root.get("extra").and_then(|e| e.get("wavetable")) {
            self.wt.load_state(&serde_json::to_vec(w).unwrap_or_default());
        }
        self.params.mark_dirty();
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let mut keys = [0u8; 16];
        let held = self.held_keys(&mut keys);
        let mut vals = [0.0f32; 9];
        vals[0] = self.engine as f32;
        vals[1] = self.active_voices() as f32;
        vals[2] = self.peak[0];
        vals[3] = self.peak[1];
        for m in 0..4 {
            vals[4 + m] = self.params.base(macro_knob(m as u32));
        }
        vals[8] = held as f32;
        write_meter(out, 0, &vals)
    }
}

/// A factory preset seen by a browser.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetInfo {
    pub path: String,
    pub name: String,
    /// Sound type ([`TYPES`]), the preset folder.
    pub kind: String,
    pub engine: usize,
    /// Character tags (the engine tag removed).
    pub tags: Vec<String>,
    pub macro_names: [&'static str; 4],
}

/// Every Vault factory preset with its browser metadata (control thread).
pub fn preset_info() -> Vec<PresetInfo> {
    let engine_info = params::all().iter().find(|i| i.id == params::ENGINE).cloned();
    crate::presets::list(ID)
        .into_iter()
        .filter_map(|r| {
            let p = crate::presets::load(&r.path).ok()?;
            let n = p.params.get(&params::ENGINE.to_string()).copied().unwrap_or(0.0);
            let engine = engine_info.as_ref().map_or(0, |i| i.to_plain(n).round() as usize).min(ENGINE_COUNT - 1);
            let tags = p.tags.iter().filter(|t| !ENGINE_TAGS.contains(&t.as_str())).cloned().collect();
            let m = macros::macros(engine);
            Some(PresetInfo { path: r.path, name: r.name, kind: r.category, engine, tags, macro_names: [m[0].name, m[1].name, m[2].name, m[3].name] })
        })
        .collect()
}

#[cfg(test)]
mod tests;
