//! `instrument.wavetable` — a 16-voice sound-design wavetable synth.
//!
//! # Signal path (per voice)
//!
//! ```text
//!  Osc 1 ─┐ (wavetable, warp, unison ×16)        ┌─ Filter 1 ─┐  serial: F1 → F2
//!  Osc 2 ─┼─ route: F1 / F2 / Both / Direct ─────┤            ├─ parallel: F1 + F2
//!  Osc 3 ─┤                                      └─ Filter 2 ─┘
//!  Sub  ──┤                                         + Direct ──── × Env 1 × velocity → pan
//!  Noise ─┘
//! ```
//! Then, summed over voices: Distortion → EQ → Chorus → Delay → Reverb →
//! Volume.
//!
//! * **Oscillators** play band-limited [`table::MipTable`]s (per-frame FFT
//!   mipmaps, one level per octave, Hermite reads; no aliasing from the
//!   table itself). `Position` morphs between frames. Warp modes ([`WARPS`]):
//!   sync, bend ±, asym/PWM, mirror, quantize, remap, phase-FM from the next
//!   or previous oscillator, ring mod. Unison up to 16 voices with detune,
//!   stereo spread and blend; start phase and random phase.
//! * **Sub** (sine / square, −1 or −2 oct) and **noise** (white, pink,
//!   brown, blue, violet, crackle), each with its own filter route and pan.
//! * **Filters**: two, each with a model ([`filter::MODELS`]: SVF LP/HP/BP
//!   12/24 dB, ladder, notch, comb ±, formant), cutoff, resonance, drive, key
//!   tracking and Env 2 depth; serial or parallel.
//! * **Envelopes**: three AHDSR with curved segments (Env 1 is the amp).
//! * **LFOs**: four; classic shapes or a drawn [`modulation::LfoCurve`];
//!   free / tempo-synced; free-running, retriggered per note, or one-shot;
//!   phase offset and fade-in. Retriggered and one-shot LFOs run per voice.
//! * **Matrix**: 32 slots, any source ([`modulation::SOURCES`]: envelopes,
//!   LFOs, velocity, key, mod wheel, aftertouch, pitch bend, 4 macros, random,
//!   constant, gate) to any parameter id or a virtual destination
//!   ([`vdst`]), with amount, polarity and a "via" depth source. The
//!   original 8 slots still work.
//! * **Voices**: poly with voice stealing (oldest released first, 1.3 ms
//!   fade), mono, legato, glide, pitch bend, sustain pedal, MPE tuning and
//!   pressure.
//!
//! Modulation and coefficients run at control rate (every 32 samples);
//! oscillators, envelopes and filters per sample.
//!
//! # Public API for editors
//!
//! * Parameters: [`params`] / [`p`] (ids, see `params.rs` for the layout),
//!   [`waves`], [`WARPS`], [`ROUTES`], [`NOISE_COLORS`], [`VIRTUAL_DESTS`],
//!   [`dest_name`].
//! * Matrix: [`modulation::ModSlot`] (`read_all`, `assign`, `param_values`,
//!   `targeting`), [`modulation::SOURCES`], [`modulation::is_modulatable`].
//! * Wavetables: [`factory_frames`] (raw frames of the bank for drawing),
//!   [`tablegen`] (draw / additive / formula / morph / normalize), [`wav`]
//!   (import Serum/Vital/Surge/single-cycle WAVs, export).
//! * Handing data to a synth: before it runs, [`WavetableSynth::set_user_table`]
//!   and [`WavetableSynth::set_lfo_curve`]; while it runs,
//!   [`WavetableSynth::remote`] gives a [`SynthRemote`] whose lock-free
//!   mailboxes deliver tables and curves to the audio thread (old ones come
//!   back to be freed on the control thread). User tables and curves are
//!   saved in the device state ([`state_with`] builds one without a synth).
//!   Set the oscillator's `WAVE` to [`USER_WAVE`] to play its user table and
//!   an LFO's `LFO_DRAWN` to use its curve.
//!
//! Meter layout: `[active voices, peak L, peak R, pos 1, pos 2, pos 3,
//! LFO 1..4 (−1..1), Env 1..3]` — positions, LFOs and envelopes of the
//! newest voice, for animated displays.

pub mod filter;
pub mod tablegen;
pub mod modulation;
mod params;
pub mod table;
pub mod wav;

pub use params::{
    DELAY_TIMES, FILTER_ROUTINGS, LEGACY_DESTS, LEGACY_SOURCES, NOISE_COLORS, PARAM_SPACE, ROUTES, SLOPES, SUB_OCTAVES, SUB_SHAPES, TABLES, USER_WAVE, VIRTUAL_DESTS, VOICE_MODES, WARPS,
    dest_name, legacy_dst, legacy_src, p, vdst, waves,
};
pub use modulation::src;

use super::synth_fx::{FxSettings, SynthFx};
use crate::common::{ParamStore, Segmented, drive, key_to_hz, pan_gains, sanitize, write_meter};
use aitrack_dsp::lfo::{division_beats, division_hz, division_seconds};
use aitrack_dsp::osc::{BlepOsc, Noise, VaShape};
use aitrack_dsp::event::Expression;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use filter::{FilterSettings, VoiceFilter};
use tablegen::WaveFrames;
use modulation::{CurveEnv, EnvSettings, LfoCurve, Polarity, SLOTS};
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use table::{Mailbox, MipTable};

pub const ID: &str = "instrument.wavetable";

pub const MAX_VOICES: usize = 16;
pub const MAX_UNISON: usize = 16;
pub const OSCS: usize = 3;
pub const ENVS: usize = 3;
pub const LFOS: usize = 4;
pub const MACROS: usize = 4;
const CHUNK: usize = 32;
const STEAL_FADE: f32 = 1.0 / 64.0;
/// Per-voice output scale (headroom for chords).
const VOICE_GAIN: f32 = 0.25;
/// Pitch modulation at 100 %.
const PITCH_RANGE: f32 = 24.0;
/// The original filter-envelope depth at 100 %.
const ENV_CUTOFF_RANGE: f32 = 72.0;
/// Distinct modulated destinations at once.
const MAX_TARGETS: usize = 48;
const NO_TARGET: u8 = u8::MAX;
const ROUTES_MAX: usize = SLOTS + 8;

/// Every parameter.
pub fn params() -> Vec<ParamInfo> {
    params::all().to_vec()
}

// ── Banks ───────────────────────────────────────────────────────────────────

struct Bank {
    legacy: Vec<MipTable>,
    factory: Vec<MipTable>,
}

/// The factory tables, built once per process off the audio thread (the
/// first `create()` builds them).
fn bank() -> &'static Bank {
    static BANK: OnceLock<Bank> = OnceLock::new();
    BANK.get_or_init(|| Bank { legacy: (0..TABLES.len()).map(tablegen::legacy_mip).collect(), factory: tablegen::factory().iter().map(WaveFrames::build).collect() })
}

/// Raw frames of `WAVE` value `wave` (1.. = factory bank; 0 / unknown →
/// None), for drawing. Allocates; control thread.
pub fn factory_frames(wave: usize) -> Option<WaveFrames> {
    if (1..=tablegen::FACTORY_NAMES.len()).contains(&wave) { Some(tablegen::factory_table(wave - 1)) } else { None }
}

/// Raw frames of an original `TABLE` choice, for drawing.
pub fn legacy_frames(table: usize) -> WaveFrames {
    tablegen::legacy_table(table)
}

// ── Handover ────────────────────────────────────────────────────────────────

/// An oscillator's user table: the editable frames (kept for saving) and
/// the playable mip table.
#[derive(Default)]
pub struct UserTable {
    pub frames: Option<WaveFrames>,
    table: Option<MipTable>,
}

impl UserTable {
    pub fn new(frames: Option<WaveFrames>) -> Self {
        let table = frames.as_ref().map(WaveFrames::build);
        Self { frames, table }
    }
}

/// Control-thread handle to a running synth: posts prepared tables and LFO
/// curves through lock-free mailboxes. Build everything here (allocation,
/// FFTs), never on the audio thread.
#[derive(Default)]
pub struct SynthRemote {
    tables: [Mailbox<UserTable>; OSCS],
    curves: [Mailbox<LfoCurve>; LFOS],
}

impl SynthRemote {
    /// Build and post oscillator `osc`'s user table (`None` clears it).
    pub fn post_user_table(&self, osc: usize, frames: Option<WaveFrames>) {
        if let Some(m) = self.tables.get(osc) {
            m.post(Box::new(UserTable::new(frames)));
        }
    }

    pub fn post_lfo_curve(&self, lfo: usize, curve: LfoCurve) {
        if let Some(m) = self.curves.get(lfo) {
            m.post(Box::new(curve));
        }
    }

    /// Free what the audio thread retired (call now and then).
    pub fn collect(&self) {
        for m in &self.tables {
            m.collect();
        }
        for m in &self.curves {
            m.collect();
        }
    }

    /// True while a post has not reached the audio thread yet.
    pub fn pending(&self) -> bool {
        self.tables.iter().any(Mailbox::pending) || self.curves.iter().any(Mailbox::pending)
    }
}

// ── Cached parameters and the compiled matrix ───────────────────────────────

struct Cache {
    infos: Vec<ParamInfo>,
    norm: Vec<f32>,
    plain: Vec<f32>,
}

impl Cache {
    fn new() -> Self {
        let mut infos: Vec<ParamInfo> = (0..PARAM_SPACE as u32).map(|i| ParamInfo::new(i, "", 0.0, 1.0, 0.0)).collect();
        for i in params::all() {
            infos[i.id as usize] = i.clone();
        }
        Self { infos, norm: vec![0.0; PARAM_SPACE], plain: vec![0.0; PARAM_SPACE] }
    }

    #[inline]
    fn plain(&self, id: u32) -> f32 {
        self.plain[id as usize]
    }

    #[inline]
    fn choice(&self, id: u32) -> usize {
        (self.plain[id as usize] - self.infos[id as usize].min).round().max(0.0) as usize
    }

    #[inline]
    fn on(&self, id: u32) -> bool {
        self.plain[id as usize] >= 0.5
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Route {
    src: u8,
    via: u8,
    pol: Polarity,
    target: u8,
    amt: f32,
}

struct Matrix {
    routes: [Route; ROUTES_MAX],
    n: usize,
    /// Destination id per target slot.
    targets: [u16; MAX_TARGETS],
    n_targets: usize,
    target_of: Vec<u8>,
}

impl Matrix {
    fn new() -> Self {
        Self { routes: [Route::default(); ROUTES_MAX], n: 0, targets: [0; MAX_TARGETS], n_targets: 0, target_of: vec![NO_TARGET; PARAM_SPACE] }
    }

    fn clear(&mut self) {
        for &t in &self.targets[..self.n_targets] {
            self.target_of[t as usize] = NO_TARGET;
        }
        self.n = 0;
        self.n_targets = 0;
    }

    fn add(&mut self, src: u8, via: u8, pol: Polarity, dest: u32, amt: f32) {
        if src == 0 || amt == 0.0 || dest == 0 || dest as usize >= PARAM_SPACE || self.n >= ROUTES_MAX || src as usize >= src::COUNT {
            return;
        }
        let mut t = self.target_of[dest as usize];
        if t == NO_TARGET {
            if self.n_targets >= MAX_TARGETS {
                return;
            }
            t = self.n_targets as u8;
            self.targets[self.n_targets] = dest as u16;
            self.target_of[dest as usize] = t;
            self.n_targets += 1;
        }
        let via = if (via as usize) < src::COUNT { via } else { 0 };
        self.routes[self.n] = Route { src, via, pol, target: t, amt };
        self.n += 1;
    }

    /// Sum the routes for one set of source values.
    #[inline]
    fn eval(&self, sources: &[f32; src::COUNT], out: &mut [f32; MAX_TARGETS]) {
        out[..self.n_targets].fill(0.0);
        for r in &self.routes[..self.n] {
            let mut x = r.pol.map(r.src, sources[r.src as usize]);
            if r.via != 0 {
                x *= Polarity::Unipolar.map(r.via, sources[r.via as usize]);
            }
            out[r.target as usize] += r.amt * x;
        }
    }
}

/// Modulation offsets of one context (a voice, or the global one).
#[inline]
fn m_of(mx: &Matrix, mods: &[f32; MAX_TARGETS], id: u32) -> f32 {
    match mx.target_of[id as usize] {
        NO_TARGET => 0.0,
        t => mods[t as usize],
    }
}

/// Plain value of `id` with its modulation (extra normalised offset `add`).
#[inline]
fn mp(c: &Cache, mx: &Matrix, mods: &[f32; MAX_TARGETS], id: u32, add: f32) -> f32 {
    let t = mx.target_of[id as usize];
    if t == NO_TARGET && add == 0.0 {
        return c.plain[id as usize];
    }
    let m = if t == NO_TARGET { 0.0 } else { mods[t as usize] };
    c.infos[id as usize].to_plain(c.norm[id as usize] + m + add)
}

// ── Settings resolved on parameter changes ──────────────────────────────────

#[derive(Clone, Copy, Debug, Default)]
struct OscCfg {
    wave: usize,
    table: usize,
    unison: usize,
    warp: usize,
    route: usize,
    transpose: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct LfoCfg {
    shape: usize,
    drawn: bool,
    sync: bool,
    div: usize,
    retrig: bool,
    oneshot: bool,
    phase: f32,
    fade_ms: f32,
}

impl LfoCfg {
    fn per_voice(&self) -> bool {
        self.retrig || self.oneshot
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Settings {
    osc: [OscCfg; OSCS],
    lfo: [LfoCfg; LFOS],
    filter_model: [usize; 2],
    filter_kind: [usize; 2],
    filter_slope24: [bool; 2],
    filter_on: [bool; 2],
    parallel: bool,
    sub_square: bool,
    sub_oct: f32,
    sub_route: usize,
    noise_color: usize,
    noise_route: usize,
    mode: usize,
    glide_ms: f32,
    vel_sens: f32,
    bend_range: f32,
    poly: usize,
    drift: f32,
    delay_div: usize,
}

// ── Voice parts ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
struct LfoState {
    phase: f64,
    held: f32,
    prev: f32,
    rng: Noise,
}

impl LfoState {
    fn new(seed: u32) -> Self {
        let mut rng = Noise::new(seed);
        let held = rng.next();
        Self { phase: 0.0, held, prev: held, rng }
    }

    fn reset(&mut self, phase: f64) {
        self.phase = phase;
        self.held = self.rng.next();
        self.prev = self.held;
    }

    /// Advance by `inc` cycles; `oneshot` stops at the end.
    #[inline]
    fn advance(&mut self, inc: f64, oneshot: bool) {
        self.phase += inc;
        if oneshot {
            self.phase = self.phase.min(1.0);
        } else if self.phase >= 1.0 {
            self.phase -= self.phase.floor();
            self.prev = self.held;
            self.held = self.rng.next();
        }
    }

    /// Bipolar value.
    #[inline]
    fn value(&self, cfg: &LfoCfg, curve: &LfoCurve) -> f32 {
        let p = if cfg.per_voice() { self.phase as f32 } else { (self.phase as f32 + cfg.phase).fract() };
        let p = p.clamp(0.0, 1.0);
        if cfg.drawn {
            return 2.0 * curve.eval(p) - 1.0;
        }
        match cfg.shape {
            0 => (p * std::f32::consts::TAU).sin(),
            1 => 1.0 - 4.0 * (p - 0.5).abs(),
            2 => 2.0 * p - 1.0,
            3 => 1.0 - 2.0 * p,
            4 => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            5 => self.held,
            _ => {
                let t = 0.5 - 0.5 * (p * std::f32::consts::PI).cos();
                self.prev + (self.held - self.prev) * t
            }
        }
    }
}

/// Coloured noise for one channel.
#[derive(Clone, Copy, Debug)]
struct NoiseGen {
    rng: Noise,
    pink: [f32; 7],
    brown: f32,
    prev: f32,
}

impl NoiseGen {
    fn new(seed: u32) -> Self {
        Self { rng: Noise::new(seed), pink: [0.0; 7], brown: 0.0, prev: 0.0 }
    }

    #[inline]
    fn next(&mut self, color: usize) -> f32 {
        let w = self.rng.next();
        match color {
            1 | 3 => {
                // Paul Kellet's pink filter (public-domain recipe).
                let b = &mut self.pink;
                b[0] = 0.99886 * b[0] + w * 0.055_517_9;
                b[1] = 0.99332 * b[1] + w * 0.075_075_9;
                b[2] = 0.969 * b[2] + w * 0.153_852;
                b[3] = 0.8665 * b[3] + w * 0.310_485_6;
                b[4] = 0.55 * b[4] + w * 0.532_952_2;
                b[5] = -0.7616 * b[5] - w * 0.016_898;
                let pink = (b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362) * 0.11;
                b[6] = w * 0.115_926;
                if color == 1 {
                    pink
                } else {
                    // Blue: differentiated pink.
                    let y = (pink - self.prev) * 2.0;
                    self.prev = pink;
                    y
                }
            }
            2 => {
                self.brown = (self.brown + 0.02 * w) * 0.998;
                self.brown * 3.5
            }
            4 => {
                let y = (w - self.prev) * 0.5;
                self.prev = w;
                y
            }
            5 => {
                // Sparse random clicks.
                if self.rng.next_unipolar() < 0.002 { w.signum() } else { 0.0 }
            }
            _ => w,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct OscState {
    phases: [f32; MAX_UNISON],
    incs: [f32; MAX_UNISON],
    gains: [(f32, f32); MAX_UNISON],
    n: usize,
    level: usize,
    pos: f32,
    warp_amt: f32,
    /// Last mono output (for FM / RM of the other oscillators).
    out: f32,
    drift: f32,
}

impl Default for OscState {
    fn default() -> Self {
        Self { phases: [0.0; MAX_UNISON], incs: [0.0; MAX_UNISON], gains: [(0.0, 0.0); MAX_UNISON], n: 0, level: 0, pos: 0.0, warp_amt: 0.0, out: 0.0, drift: 0.0 }
    }
}

struct Voice {
    active: bool,
    released: bool,
    /// Key released while the sustain pedal is down.
    sustained: bool,
    key: u8,
    note_id: i32,
    vel: f32,
    vel_gain: f32,
    age: u64,
    samples: u64,
    pitch: f32,
    target: f32,
    tuning: f32,
    pressure: f32,
    rand: f32,
    osc: [OscState; OSCS],
    sub: BlepOsc,
    sub_gain: f32,
    sub_pan: (f32, f32),
    noise: [NoiseGen; 2],
    noise_gain: f32,
    noise_pan: (f32, f32),
    env: [CurveEnv; ENVS],
    lfo: [LfoState; LFOS],
    filt: [VoiceFilter; 2],
    amp_mod: f32,
    amp_step: f32,
    pan: (f32, f32),
    fade: f32,
    fading: bool,
    pending: Option<(u8, f32, i32)>,
    rng: Noise,
    src: [f32; src::COUNT],
    mods: [f32; MAX_TARGETS],
}

impl Voice {
    fn new(sr: f64, seed: u32) -> Self {
        Self {
            active: false,
            released: false,
            sustained: false,
            key: 60,
            note_id: -1,
            vel: 0.0,
            vel_gain: 1.0,
            age: 0,
            samples: 0,
            pitch: 60.0,
            target: 60.0,
            tuning: 0.0,
            pressure: 0.0,
            rand: 0.0,
            osc: [OscState::default(); OSCS],
            sub: BlepOsc::default(),
            sub_gain: 0.0,
            sub_pan: (1.0, 1.0),
            noise: [NoiseGen::new(seed ^ 0x5555), NoiseGen::new(seed ^ 0xAAAA_0000)],
            noise_gain: 0.0,
            noise_pan: (1.0, 1.0),
            env: [CurveEnv::new(sr); ENVS],
            lfo: std::array::from_fn(|i| LfoState::new(seed.wrapping_add(i as u32 * 7919))),
            filt: [VoiceFilter::new(sr), VoiceFilter::new(sr)],
            amp_mod: 1.0,
            amp_step: 0.0,
            pan: (1.0, 1.0),
            fade: 1.0,
            fading: false,
            pending: None,
            rng: Noise::new(seed.wrapping_mul(31)),
            src: [0.0; src::COUNT],
            mods: [0.0; MAX_TARGETS],
        }
    }

    fn gate_off(&mut self) {
        for e in &mut self.env {
            e.gate_off();
        }
        self.released = true;
        self.sustained = false;
    }
}

/// Everything a voice reads while rendering a chunk.
struct Ctx<'a> {
    c: &'a Cache,
    mx: &'a Matrix,
    s: &'a Settings,
    tables: [&'a MipTable; OSCS],
    glfo: &'a [LfoState; LFOS],
    curves: [&'a LfoCurve; LFOS],
    sr: f32,
    bend: f32,
    mw: f32,
    at: f32,
    macros: [f32; MACROS],
    bpm: f64,
}

impl Ctx<'_> {
    fn lfo_rate(&self, l: usize, mods: &[f32; MAX_TARGETS]) -> f64 {
        let cfg = &self.s.lfo[l];
        if cfg.sync { division_hz(cfg.div, self.bpm) } else { mp(self.c, self.mx, mods, p::lfo(l as u32, p::LFO_RATE), 0.0) as f64 }
    }
}

/// Warp the read phase. `other` is the modulating oscillator's last output.
#[inline]
fn warp(ph: f32, mode: usize, a: f32, next: f32, prev: f32) -> f32 {
    if a <= 0.0 && mode != 6 {
        return ph;
    }
    match mode {
        1 => (ph * (1.0 + 15.0 * a)).fract(),
        2 => ph.powf(1.0 / (1.0 + 4.0 * a)),
        3 => ph.powf(1.0 + 4.0 * a),
        4 => {
            let w = 0.5 - 0.49 * a;
            if ph < w { 0.5 * ph / w } else { 0.5 + 0.5 * (ph - w) / (1.0 - w) }
        }
        5 => {
            let m = if ph < 0.5 { 2.0 * ph } else { 2.0 - 2.0 * ph };
            ph + (m - ph) * a
        }
        6 => {
            if a <= 0.0 {
                return ph;
            }
            let steps = 2.0 + 254.0 * (1.0 - a) * (1.0 - a);
            (ph * steps).floor() / steps
        }
        7 => ph - a * 0.9 * (std::f32::consts::TAU * ph).sin() / std::f32::consts::TAU,
        8 => (ph + 1.5 * a * a * next).rem_euclid(1.0),
        9 => (ph + 1.5 * a * a * prev).rem_euclid(1.0),
        _ => ph,
    }
}

/// How much faster than the note a warp reads the table at worst (picks a
/// thinner mip level so the warp does not alias more than it must).
fn warp_speed(mode: usize, a: f32) -> f32 {
    match mode {
        1 => 1.0 + 15.0 * a,
        2 | 3 => 1.0 + 4.0 * a,
        4 => 0.5 / (0.5 - 0.49 * a),
        5 => 1.0 + a,
        7 => 1.0 + 0.9 * a,
        8 | 9 => 1.0 + 6.0 * a * a,
        _ => 1.0,
    }
}

impl Voice {
    /// Control-rate update for the next `n` samples.
    fn control(&mut self, ctx: &Ctx<'_>, n: usize) {
        let s = ctx.s;
        let (c, mx) = (ctx.c, ctx.mx);
        let sr = ctx.sr;
        // Glide.
        if s.glide_ms > 0.0 && self.pitch != self.target {
            let k = 1.0 - (-(n as f32) / (s.glide_ms * 0.00025 * sr)).exp();
            self.pitch += (self.target - self.pitch) * k;
            if (self.pitch - self.target).abs() < 0.001 {
                self.pitch = self.target;
            }
        } else {
            self.pitch = self.target;
        }
        // Sources.
        let sv = &mut self.src;
        sv[src::NONE as usize] = 0.0;
        for e in 0..ENVS {
            sv[src::ENV1 as usize + e] = self.env[e].value();
        }
        let secs = self.samples as f32 / sr;
        for l in 0..LFOS {
            let cfg = &s.lfo[l];
            let st = if cfg.per_voice() { &self.lfo[l] } else { &ctx.glfo[l] };
            let fade = if cfg.fade_ms > 0.0 { (secs * 1000.0 / cfg.fade_ms).min(1.0) } else { 1.0 };
            sv[src::LFO1 as usize + l] = st.value(cfg, ctx.curves[l]) * fade;
        }
        sv[src::VELOCITY as usize] = self.vel;
        sv[src::KEY_TRACK as usize] = (self.pitch - 60.0) / 60.0;
        sv[src::MOD_WHEEL as usize] = ctx.mw;
        sv[src::AFTERTOUCH as usize] = ctx.at.max(self.pressure);
        sv[src::PITCH_BEND as usize] = ctx.bend;
        for m in 0..MACROS {
            sv[src::MACRO1 as usize + m] = ctx.macros[m];
        }
        sv[src::RANDOM as usize] = self.rand;
        sv[src::CONSTANT as usize] = 1.0;
        sv[src::NOTE_GATE as usize] = if self.released { 0.0 } else { 1.0 };
        mx.eval(&self.src, &mut self.mods);
        let mods = &self.mods;
        let vm = |id: u32| m_of(mx, mods, id);
        let m = |id: u32, add: f32| mp(c, mx, mods, id, add);

        // Envelopes follow their (modulated) knobs.
        for e in 0..ENVS {
            let id = |f| p::env(e as u32, f);
            self.env[e].set(EnvSettings {
                attack: m(id(p::E_ATTACK), 0.0),
                hold: m(id(p::E_HOLD), 0.0),
                decay: m(id(p::E_DECAY), 0.0),
                sustain: m(id(p::E_SUSTAIN), 0.0) / 100.0,
                release: m(id(p::E_RELEASE), 0.0),
                attack_curve: m(id(p::E_A_CURVE), 0.0) / 100.0,
                decay_curve: m(id(p::E_D_CURVE), 0.0) / 100.0,
                release_curve: m(id(p::E_R_CURVE), 0.0) / 100.0,
            });
        }

        let base = self.pitch + self.tuning + ctx.bend * s.bend_range + vm(vdst::PITCH) * PITCH_RANGE;
        let mix = vm(vdst::OSC_MIX);
        let det_all = vm(vdst::DETUNE);
        let pos_all = vm(vdst::POS);
        for o in 0..OSCS {
            let cfg = &s.osc[o];
            let id = |f| p::osc(o as u32, f);
            let mix_gain = match o {
                0 => (1.0 - mix).clamp(0.0, 1.0),
                1 => (1.0 + mix).clamp(0.0, 1.0),
                _ => 1.0,
            };
            let level = m(id(p::LEVEL), 0.0) / 100.0 * mix_gain;
            let st = &mut self.osc[o];
            if level <= 0.0 {
                st.n = 0;
                continue;
            }
            let n_u = cfg.unison;
            st.n = n_u;
            let pitch = base + cfg.transpose + m(id(p::FINE), 0.0) / 100.0 + vm(vdst::OSC_PITCH + o as u32) * PITCH_RANGE + st.drift;
            let f0 = key_to_hz(pitch);
            let det = (m(id(p::DETUNE), 0.0) / 100.0 + det_all).clamp(0.0, 1.0);
            let cents = 100.0 * det * det;
            let spread = m(id(p::SPREAD), 0.0) / 100.0;
            let pan = m(id(p::PAN), 0.0) / 100.0;
            let blend = m(id(p::BLEND), 0.0) / 100.0;
            // Centre voice(s) at full level, the others at `blend`.
            let centre = |u: usize| n_u == 1 || (!n_u.is_multiple_of(2) && u == n_u / 2) || (n_u.is_multiple_of(2) && (u == n_u / 2 || u + 1 == n_u / 2));
            let mut power = 0.0;
            for u in 0..n_u {
                let g = if centre(u) { 1.0 } else { blend };
                power += g * g;
            }
            let norm = level / power.max(1e-6).sqrt();
            let mut max_inc = 0.0f32;
            for u in 0..n_u {
                let off = if n_u > 1 { -1.0 + 2.0 * u as f32 / (n_u - 1) as f32 } else { 0.0 };
                let f = f0 * 2f32.powf(off * cents / 1200.0);
                let inc = (f / sr).clamp(0.0, 0.49);
                st.incs[u] = inc;
                max_inc = max_inc.max(inc);
                let (gl, gr) = pan_gains((pan + spread * off).clamp(-1.0, 1.0));
                let g = if centre(u) { 1.0 } else { blend } * norm * std::f32::consts::SQRT_2;
                st.gains[u] = (gl * g, gr * g);
            }
            st.warp_amt = m(id(p::WARP_AMT), 0.0) / 100.0;
            st.level = ctx.tables[o].level_for(max_inc * warp_speed(cfg.warp, st.warp_amt));
            st.pos = (m(id(p::POS), 0.0) / 100.0 + pos_all).clamp(0.0, 1.0);
        }
        // Sub and noise.
        self.sub_gain = m(p::SUB_LEVEL, 0.0) / 100.0;
        if self.sub_gain > 0.0 {
            self.sub.shape = if s.sub_square { VaShape::Pulse } else { VaShape::Sine };
            self.sub.set_freq(key_to_hz(base - s.sub_oct), sr);
            self.sub_pan = pan_pair(m(p::SUB_PAN, 0.0) / 100.0);
        }
        self.noise_gain = m(p::NOISE, 0.0) / 100.0 * 0.5;
        if self.noise_gain > 0.0 {
            self.noise_pan = pan_pair(m(p::NOISE_PAN, 0.0) / 100.0);
        }
        // Filters.
        let env2 = self.env[1].value();
        let both = vm(vdst::CUTOFF);
        for f in 0..2 {
            if !s.filter_on[f] {
                continue;
            }
            let id = |x| p::filt(f as u32, x);
            let semis = m(id(p::F_KEY), 0.0) / 100.0 * (self.pitch - 60.0) + m(id(p::F_ENV), 0.0) / 100.0 * env2 * ENV_CUTOFF_RANGE;
            let cutoff = m(id(p::F_CUTOFF), both) * 2f32.powf(semis / 12.0);
            self.filt[f].configure(FilterSettings {
                model: s.filter_model[f],
                kind: s.filter_kind[f],
                slope24: s.filter_slope24[f],
                cutoff,
                resonance: m(id(p::F_RESO), 0.0) / 100.0,
                drive: m(id(p::F_DRIVE), 0.0) / 100.0,
            });
        }
        // Amp and pan.
        let target = (1.0 + vm(vdst::AMP)).clamp(0.0, 2.0);
        self.amp_step = (target - self.amp_mod) / n as f32;
        self.pan = pan_pair(vm(vdst::PAN));
        // Per-voice LFOs advance (their rate may be modulated per voice).
        for l in 0..LFOS {
            let cfg = s.lfo[l];
            if cfg.per_voice() {
                let rate = ctx.lfo_rate(l, &self.mods);
                self.lfo[l].advance(rate * n as f64 / sr as f64, cfg.oneshot);
            }
        }
    }

    /// Render `l.len()` samples, adding into `l`/`r`.
    fn render(&mut self, ctx: &Ctx<'_>, l: &mut [f32], r: &mut [f32]) {
        let s = ctx.s;
        let route = [s.osc[0].route, s.osc[1].route, s.osc[2].route];
        let warp_mode = [s.osc[0].warp, s.osc[1].warp, s.osc[2].warp];
        let parallel = s.parallel;
        for i in 0..l.len() {
            // Buses: [filter 1, filter 2, direct], stereo.
            let mut bus = [[0.0f32; 2]; 3];
            let mut send = |route: usize, x: (f32, f32)| {
                let (b, g) = match route {
                    1 => (1, 1.0),
                    2 if parallel => (3, 0.5),
                    3 => (2, 1.0),
                    _ => (0, 1.0),
                };
                if b == 3 {
                    bus[0][0] += x.0 * g;
                    bus[0][1] += x.1 * g;
                    bus[1][0] += x.0 * g;
                    bus[1][1] += x.1 * g;
                } else {
                    bus[b][0] += x.0 * g;
                    bus[b][1] += x.1 * g;
                }
            };
            let outs = [self.osc[0].out, self.osc[1].out, self.osc[2].out];
            for o in 0..OSCS {
                let st = &mut self.osc[o];
                if st.n == 0 {
                    st.out = 0.0;
                    continue;
                }
                let t = ctx.tables[o];
                let (lvl, pos, wa, wm) = (st.level, st.pos, st.warp_amt, warp_mode[o]);
                let (next, prev) = (outs[(o + 1) % OSCS], outs[(o + OSCS - 1) % OSCS]);
                let rm = if wm == 10 { 1.0 + (next - 1.0) * wa } else { 1.0 };
                let (mut sl, mut sr_, mut mono) = (0.0f32, 0.0f32, 0.0f32);
                for u in 0..st.n {
                    let ph = st.phases[u];
                    let x = t.sample(lvl, pos, warp(ph, wm, wa, next, prev)) * rm;
                    let mut nph = ph + st.incs[u];
                    if nph >= 1.0 {
                        nph -= 1.0;
                    }
                    st.phases[u] = nph;
                    let (gl, gr) = st.gains[u];
                    sl += x * gl;
                    sr_ += x * gr;
                    mono += x;
                }
                st.out = mono / st.n as f32;
                send(route[o], (sl, sr_));
            }
            if self.sub_gain > 0.0 {
                let x = self.sub.next() * self.sub_gain;
                send(s.sub_route, (x * self.sub_pan.0, x * self.sub_pan.1));
            }
            if self.noise_gain > 0.0 {
                let a = self.noise[0].next(s.noise_color) * self.noise_gain;
                let b = self.noise[1].next(s.noise_color) * self.noise_gain;
                send(s.noise_route, (a * self.noise_pan.0, b * self.noise_pan.1));
            }
            let (yl, yr) = {
                let f1 = if s.filter_on[0] { self.filt[0].process(bus[0][0], bus[0][1]) } else { (bus[0][0], bus[0][1]) };
                let (in2l, in2r) = if parallel { (bus[1][0], bus[1][1]) } else { (bus[1][0] + f1.0, bus[1][1] + f1.1) };
                let f2 = if s.filter_on[1] { self.filt[1].process(in2l, in2r) } else { (in2l, in2r) };
                if parallel { (f1.0 + f2.0 + bus[2][0], f1.1 + f2.1 + bus[2][1]) } else { (f2.0 + bus[2][0], f2.1 + bus[2][1]) }
            };
            let env = self.env[0].next();
            self.env[1].next();
            self.env[2].next();
            self.amp_mod += self.amp_step;
            if self.fading {
                self.fade = (self.fade - STEAL_FADE).max(0.0);
            }
            let g = env * self.vel_gain * self.amp_mod * self.fade * VOICE_GAIN;
            l[i] += yl * g * self.pan.0;
            r[i] += yr * g * self.pan.1;
            if self.fade <= 0.0 || !self.env[0].is_active() {
                break;
            }
        }
        self.samples += l.len() as u64;
        for f in &mut self.filt {
            f.sanitize();
        }
    }
}

/// Unmodulated settings of envelope `e`.
fn env_settings(c: &Cache, e: usize) -> EnvSettings {
    let id = |f| p::env(e as u32, f);
    EnvSettings {
        attack: c.plain(id(p::E_ATTACK)),
        hold: c.plain(id(p::E_HOLD)),
        decay: c.plain(id(p::E_DECAY)),
        sustain: c.plain(id(p::E_SUSTAIN)) / 100.0,
        release: c.plain(id(p::E_RELEASE)),
        attack_curve: c.plain(id(p::E_A_CURVE)) / 100.0,
        decay_curve: c.plain(id(p::E_D_CURVE)) / 100.0,
        release_curve: c.plain(id(p::E_R_CURVE)) / 100.0,
    }
}

/// The table oscillator `o` plays.
fn table_for<'a>(s: &Settings, bank: &'a Bank, user: &'a [Box<UserTable>; OSCS], o: usize) -> &'a MipTable {
    let cfg = &s.osc[o];
    let legacy = &bank.legacy[cfg.table.min(bank.legacy.len() - 1)];
    match cfg.wave {
        0 => legacy,
        w if w == USER_WAVE => user[o].table.as_ref().unwrap_or(legacy),
        w => bank.factory.get(w - 1).unwrap_or(legacy),
    }
}

/// Balance-style pan (centre = unity on both sides), as the original synth.
fn pan_pair(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    if pan >= 0.0 { ((pan * std::f32::consts::FRAC_PI_2).cos(), 1.0) } else { (1.0, (pan * std::f32::consts::FRAC_PI_2).cos()) }
}

// ── The processor ───────────────────────────────────────────────────────────

pub struct WavetableSynth {
    info: PluginInfo,
    params: ParamStore,
    cache: Cache,
    mx: Matrix,
    gmods: [f32; MAX_TARGETS],
    sr: f64,
    s: Settings,
    voices: Vec<Voice>,
    glfo: [LfoState; LFOS],
    last_beats: f64,
    held: [u8; 16],
    n_held: usize,
    age: u64,
    last_pitch: f32,
    bend: f32,
    mod_wheel: f32,
    aftertouch: f32,
    sustain: bool,
    fx: SynthFx,
    fx_settings: FxSettings,
    volume: f32,
    peak: [f32; 2],
    bank: &'static Bank,
    user: [Box<UserTable>; OSCS],
    curves: [Box<LfoCurve>; LFOS],
    remote: Arc<SynthRemote>,
    rng: Noise,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(WavetableSynth::new())
}

impl Default for WavetableSynth {
    fn default() -> Self {
        Self::new()
    }
}

impl WavetableSynth {
    pub fn new() -> Self {
        let params = params();
        let sr = 48_000.0;
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Wavetable Synth",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["synth", "wavetable", "polyphonic"],
                params: params.clone(),
                inputs: 0,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            cache: Cache::new(),
            mx: Matrix::new(),
            gmods: [0.0; MAX_TARGETS],
            sr,
            s: Settings::default(),
            voices: (0..MAX_VOICES).map(|i| Voice::new(sr, 0x9E37_79B9u32.wrapping_mul(i as u32 + 1))).collect(),
            glfo: std::array::from_fn(|i| LfoState::new(11 + 12 * i as u32)),
            last_beats: f64::NAN,
            held: [0; 16],
            n_held: 0,
            age: 0,
            last_pitch: 60.0,
            bend: 0.0,
            mod_wheel: 0.0,
            aftertouch: 0.0,
            sustain: false,
            fx: SynthFx::new(sr),
            fx_settings: FxSettings::default(),
            volume: 1.0,
            peak: [0.0; 2],
            bank: bank(),
            user: std::array::from_fn(|_| Box::default()),
            curves: std::array::from_fn(|_| Box::new(LfoCurve::default())),
            remote: Arc::new(SynthRemote::default()),
            rng: Noise::new(0xC0FF_EE11),
        };
        s.prepare(sr, 512);
        s
    }

    /// Number of sounding voices.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Control-thread handle for live table / curve changes.
    pub fn remote(&self) -> Arc<SynthRemote> {
        self.remote.clone()
    }

    /// Set oscillator `osc`'s user table directly (control thread, when the
    /// synth is not running on the audio thread). Builds the mip table.
    pub fn set_user_table(&mut self, osc: usize, frames: Option<WaveFrames>) {
        if osc < OSCS {
            *self.user[osc] = UserTable::new(frames);
        }
    }

    pub fn user_table(&self, osc: usize) -> Option<&WaveFrames> {
        self.user.get(osc).and_then(|u| u.frames.as_ref())
    }

    pub fn set_lfo_curve(&mut self, lfo: usize, curve: LfoCurve) {
        if lfo < LFOS {
            *self.curves[lfo] = curve;
        }
    }

    pub fn lfo_curve(&self, lfo: usize) -> LfoCurve {
        self.curves.get(lfo).map_or_else(LfoCurve::default, |c| **c)
    }


    fn update(&mut self) {
        let c = &mut self.cache;
        for info in self.params.infos() {
            let i = info.id as usize;
            c.norm[i] = self.params.get(info.id);
            c.plain[i] = info.to_plain(c.norm[i]);
        }
        let c = &self.cache;
        let s = &mut self.s;
        for o in 0..OSCS {
            let id = |f| p::osc(o as u32, f);
            let voices = c.plain(id(p::VOICES)).round() as usize;
            s.osc[o] = OscCfg {
                wave: c.choice(id(p::WAVE)),
                table: c.choice(id(p::TABLE)).min(TABLES.len() - 1),
                unison: if voices == 0 { c.plain(id(p::UNISON)).round() as usize } else { voices }.clamp(1, MAX_UNISON),
                warp: c.choice(id(p::WARP)).min(WARPS.len() - 1),
                route: c.choice(id(p::ROUTE)).min(3),
                transpose: c.plain(id(p::OCTAVE)).round() * 12.0 + c.plain(id(p::SEMI)).round(),
            };
        }
        for l in 0..LFOS {
            let id = |f| p::lfo(l as u32, f);
            s.lfo[l] = LfoCfg {
                shape: c.choice(id(p::LFO_SHAPE)),
                drawn: c.on(id(p::LFO_DRAWN)),
                sync: c.on(id(p::LFO_SYNC)),
                div: c.choice(id(p::LFO_DIVISION)),
                retrig: c.on(id(p::LFO_RETRIGGER)),
                oneshot: c.on(id(p::LFO_ONESHOT)),
                phase: c.plain(id(p::LFO_PHASE)) / 100.0,
                fade_ms: c.plain(id(p::LFO_FADE)),
            };
        }
        for f in 0..2 {
            let id = |x| p::filt(f as u32, x);
            s.filter_model[f] = c.choice(id(p::F_MODEL)).min(filter::MODELS.len() - 1);
            s.filter_kind[f] = c.choice(id(p::F_TYPE)).min(2);
            s.filter_slope24[f] = c.choice(id(p::F_SLOPE)) == 1;
            s.filter_on[f] = c.on(id(p::F_ENABLE));
        }
        s.parallel = c.choice(p::FILTER_ROUTING) == 1;
        s.sub_square = c.choice(p::SUB_SHAPE) == 1;
        s.sub_oct = if c.choice(p::SUB_OCTAVE) == 1 { 24.0 } else { 12.0 };
        s.sub_route = c.choice(p::SUB_ROUTE).min(3);
        s.noise_color = c.choice(p::NOISE_COLOR).min(NOISE_COLORS.len() - 1);
        s.noise_route = c.choice(p::NOISE_ROUTE).min(3);
        s.mode = c.choice(p::VOICE_MODE);
        s.glide_ms = c.plain(p::GLIDE);
        s.vel_sens = c.plain(p::VEL_SENS) / 100.0;
        s.bend_range = c.plain(p::BEND_RANGE).round();
        s.poly = (c.plain(p::POLYPHONY).round() as usize).clamp(1, MAX_VOICES);
        s.drift = c.plain(p::DRIFT) / 100.0;
        s.delay_div = params::DELAY_DIV[c.choice(p::DELAY_TIME).min(params::DELAY_DIV.len() - 1)];
        self.compile_matrix();
    }

    fn compile_matrix(&mut self) {
        let c = &self.cache;
        let mx = &mut self.mx;
        mx.clear();
        for s in 0..SLOTS as u32 {
            let id = |f| p::slot_x(s, f);
            let src_i = c.plain(id(p::M_SRC)).round() as u8;
            let dst = c.plain(id(p::M_DST)).round() as u32;
            let amt = c.plain(id(p::M_AMT)) / 100.0;
            let pol = Polarity::from_index(c.choice(id(p::M_MODE)));
            let via = c.plain(id(p::M_VIA)).round() as u8;
            // Never modulate the matrix itself.
            if (p::MATRIX..p::MATRIX_END).contains(&dst) {
                continue;
            }
            mx.add(src_i, via, pol, dst, amt);
        }
        // The original 8 slots, translated to the new routing.
        const SRC_MAP: [u8; 10] = [src::NONE, src::LFO1, src::LFO2, src::ENV2, src::VELOCITY, src::MOD_WHEEL, src::AFTERTOUCH, src::KEY_TRACK, src::PITCH_BEND, src::ENV1];
        // Cutoff: 72 st at 100 % over a 119.6 st knob; LFO rate: 4 octaves over 11.97.
        let cutoff_scale = 72.0 / (12.0 * 1000f32.log2());
        let rate_scale = 4.0 / 4000f32.log2();
        for s in 0..8u32 {
            let so = c.choice(p::slot(s, p::MOD_SRC)).min(SRC_MAP.len() - 1);
            let d = c.choice(p::slot(s, p::MOD_DST));
            let a = c.plain(p::slot(s, p::MOD_AMT)) / 100.0;
            let (dest, scale) = match d {
                1 => (vdst::OSC_PITCH, 1.0),
                2 => (vdst::OSC_PITCH + 1, 1.0),
                3 => (p::osc(0, p::POS), 1.0),
                4 => (p::osc(1, p::POS), 1.0),
                5 => (p::CUTOFF, cutoff_scale),
                6 => (p::RESONANCE, 1.0),
                7 => (vdst::AMP, 1.0),
                8 => (vdst::PAN, 1.0),
                9 => (p::lfo(0, p::LFO_RATE), rate_scale),
                10 => (vdst::OSC_MIX, 1.0),
                11 => (vdst::DETUNE, 1.0),
                12 => (vdst::PITCH, 1.0),
                _ => continue,
            };
            mx.add(SRC_MAP[so], 0, Polarity::Natural, dest, a * scale);
        }
    }

    // ── Notes ────────────────────────────────────────────────────────────

    fn start_voice(&mut self, vi: usize, key: u8, vel: f32, note_id: i32, glide_from: Option<f32>) {
        self.age += 1;
        let s = self.s;
        let drift = s.drift;
        let rand_ph: [f32; OSCS] = std::array::from_fn(|o| self.cache.plain(p::osc(o as u32, p::RAND_PHASE)) / 100.0);
        let start_ph: [f32; OSCS] = std::array::from_fn(|o| self.cache.plain(p::osc(o as u32, p::PHASE)) / 100.0);
        let v = &mut self.voices[vi];
        v.active = true;
        v.released = false;
        v.sustained = false;
        v.fading = false;
        v.fade = 1.0;
        v.pending = None;
        v.key = key;
        v.note_id = note_id;
        v.vel = vel.clamp(0.0, 1.0);
        v.vel_gain = 1.0 - s.vel_sens + s.vel_sens * v.vel * v.vel;
        v.age = self.age;
        v.samples = 0;
        v.tuning = 0.0;
        v.pressure = 0.0;
        v.rand = self.rng.next_unipolar();
        v.target = key as f32;
        v.pitch = match glide_from {
            Some(p) if s.glide_ms > 0.0 => p,
            _ => key as f32,
        };
        for o in 0..OSCS {
            let n = s.osc[o].unison;
            let st = &mut v.osc[o];
            for u in 0..MAX_UNISON {
                let r = if n > 1 { v.rng.next_unipolar() * rand_ph[o] } else { 0.0 };
                st.phases[u] = (start_ph[o] + r).fract();
            }
            st.out = 0.0;
            st.drift = if drift > 0.0 { v.rng.next() * drift * 0.12 } else { 0.0 };
        }
        v.sub.set_phase(0.0);
        for f in &mut v.filt {
            f.reset();
        }
        for (e, env) in v.env.iter_mut().enumerate() {
            env.set(env_settings(&self.cache, e));
            env.retrigger();
        }
        for l in 0..LFOS {
            if s.lfo[l].per_voice() {
                v.lfo[l].reset(s.lfo[l].phase as f64);
            }
        }
        v.amp_mod = 1.0;
        v.amp_step = 0.0;
        self.last_pitch = key as f32;
    }

    fn note_on(&mut self, key: u8, vel: f32, note_id: i32) {
        if vel <= 0.0 {
            self.note_off(key, note_id);
            return;
        }
        if self.n_held < self.held.len() {
            self.held[self.n_held] = key;
            self.n_held += 1;
        }
        if self.s.mode != 0 {
            // Mono / legato on voice 0.
            let legato = self.s.mode == 2;
            let v = &self.voices[0];
            let sounding = v.active && !v.released && !v.fading;
            if sounding {
                let s = self.s;
                let v = &mut self.voices[0];
                v.target = key as f32;
                v.key = key;
                v.note_id = note_id;
                if s.glide_ms <= 0.0 {
                    v.pitch = key as f32;
                }
                if !legato {
                    for e in &mut v.env {
                        e.gate_on();
                    }
                    for l in 0..LFOS {
                        if s.lfo[l].per_voice() {
                            v.lfo[l].reset(s.lfo[l].phase as f64);
                        }
                    }
                }
                self.last_pitch = key as f32;
            } else {
                let from = if v.active { v.pitch } else { self.last_pitch };
                self.start_voice(0, key, vel, note_id, Some(from));
            }
            return;
        }
        // Poly: retrigger the same key, else a free voice, else steal.
        for v in self.voices.iter_mut() {
            if v.active && v.key == key && !v.released {
                v.gate_off();
            }
        }
        let poly = self.s.poly;
        let glide_from = Some(self.last_pitch);
        if let Some(vi) = (0..poly).find(|&i| !self.voices[i].active) {
            self.start_voice(vi, key, vel, note_id, glide_from);
            return;
        }
        // Steal: oldest released voice, else the oldest one.
        let vi = (0..poly).filter(|&i| !self.voices[i].fading).min_by_key(|&i| (!self.voices[i].released as u8, self.voices[i].age)).unwrap_or(0);
        let v = &mut self.voices[vi];
        v.fading = true;
        v.pending = Some((key, vel, note_id));
    }

    fn note_off(&mut self, key: u8, _note_id: i32) {
        if let Some(i) = self.held[..self.n_held].iter().position(|&k| k == key) {
            self.held.copy_within(i + 1..self.n_held, i);
            self.n_held -= 1;
        }
        let sustain = self.sustain;
        if self.s.mode != 0 {
            let glide = self.s.glide_ms;
            let v = &mut self.voices[0];
            if !v.active || v.released {
                return;
            }
            if self.n_held > 0 {
                let back = self.held[self.n_held - 1];
                if v.key == key {
                    v.key = back;
                    v.target = back as f32;
                    if glide <= 0.0 {
                        v.pitch = back as f32;
                    }
                }
            } else if sustain {
                v.sustained = true;
            } else {
                v.gate_off();
            }
            return;
        }
        for v in self.voices.iter_mut() {
            if v.active && v.key == key && !v.released {
                if sustain {
                    v.sustained = true;
                } else {
                    v.gate_off();
                }
            }
            if let Some((k, _, _)) = v.pending
                && k == key
            {
                // The note ended before its stolen voice was free: drop it.
                v.pending = None;
            }
        }
    }

    fn set_sustain(&mut self, down: bool) {
        self.sustain = down;
        if !down {
            for v in self.voices.iter_mut() {
                if v.active && v.sustained && !v.released {
                    v.gate_off();
                }
            }
        }
    }

    fn all_notes_off(&mut self) {
        self.n_held = 0;
        self.sustain = false;
        for v in self.voices.iter_mut() {
            if v.active {
                v.gate_off();
                v.pending = None;
            }
        }
    }

    fn expression(&mut self, note_id: i32, key: u8, expr: Expression, value: f32) {
        for v in self.voices.iter_mut() {
            if v.active && (v.note_id == note_id || (note_id < 0 && v.key == key)) {
                match expr {
                    Expression::Tuning => v.tuning = value.clamp(-48.0, 48.0),
                    Expression::Pressure => v.pressure = value.clamp(0.0, 1.0),
                    _ => {}
                }
            }
        }
    }

    // ── Rendering ────────────────────────────────────────────────────────

    /// Global modulation (FX, volume, free-running LFO rates): the global
    /// sources plus the per-voice sources of the newest voice.
    fn global_mods(&mut self) {
        let mut sv = [0.0f32; src::COUNT];
        if let Some(v) = self.voices.iter().filter(|v| v.active).max_by_key(|v| v.age) {
            sv = v.src;
        }
        for l in 0..LFOS {
            if !self.s.lfo[l].per_voice() {
                sv[src::LFO1 as usize + l] = self.glfo[l].value(&self.s.lfo[l], &self.curves[l]);
            }
        }
        sv[src::MOD_WHEEL as usize] = self.mod_wheel;
        sv[src::AFTERTOUCH as usize] = self.aftertouch.max(sv[src::AFTERTOUCH as usize]);
        sv[src::PITCH_BEND as usize] = self.bend;
        for m in 0..MACROS {
            sv[src::MACRO1 as usize + m] = self.cache.plain(p::macro_knob(m as u32)) / 100.0;
        }
        sv[src::CONSTANT as usize] = 1.0;
        self.mx.eval(&sv, &mut self.gmods);
    }

    fn update_fx(&mut self, bpm: f64) {
        let (c, mx, g) = (&self.cache, &self.mx, &self.gmods);
        let m = |id: u32| mp(c, mx, g, id, 0.0);
        let f = &mut self.fx_settings;
        f.dist_mode = c.choice(p::DIST_MODE);
        f.dist_drive = m(p::DIST_DRIVE) / 100.0;
        f.dist_mix = m(p::DIST_MIX) / 100.0;
        f.eq_low = m(p::EQ_LOW);
        f.eq_mid = m(p::EQ_MID);
        f.eq_mid_freq = m(p::EQ_MID_FREQ);
        f.eq_high = m(p::EQ_HIGH);
        f.chorus_mix = m(p::CHORUS_MIX) / 100.0;
        f.chorus_rate = m(p::CHORUS_RATE);
        f.chorus_depth = m(p::CHORUS_DEPTH) / 100.0 * 0.005;
        f.delay_mix = m(p::DELAY_MIX) / 100.0;
        f.delay_feedback = m(p::DELAY_FEEDBACK) / 100.0;
        f.reverb_mix = m(p::REVERB_MIX) / 100.0;
        f.reverb_decay = m(p::REVERB_DECAY);
        f.reverb_tone = m(p::REVERB_TONE);
        f.delay_samples = (division_seconds(self.s.delay_div, bpm) * self.sr) as f32;
        self.volume = db_to_gain(m(p::VOLUME));
    }

    fn take_mail(&mut self) {
        let remote = &self.remote;
        for o in 0..OSCS {
            remote.tables[o].swap_into(&mut self.user[o]);
        }
        for l in 0..LFOS {
            remote.curves[l].swap_into(&mut self.curves[l]);
        }
    }
}

impl Segmented for WavetableSynth {
    fn event(&mut self, kind: &EventKind) {
        if self.params.take_dirty() {
            self.update();
        }
        match *kind {
            EventKind::Param { id, value } => self.params.set(id, value),
            EventKind::NoteOn { key, velocity, note_id, .. } => {
                if self.params.take_dirty() {
                    self.update();
                }
                self.note_on(key, velocity, note_id)
            }
            EventKind::NoteOff { key, note_id, .. } => self.note_off(key, note_id),
            EventKind::NoteExpression { note_id, key, expr, value } => self.expression(note_id, key, expr, value),
            EventKind::PitchBend { value, .. } => self.bend = value.clamp(-1.0, 1.0),
            EventKind::ControlChange { cc: 1, value, .. } => self.mod_wheel = value.clamp(0.0, 1.0),
            EventKind::ControlChange { cc: 64, value, .. } => self.set_sustain(value >= 0.5),
            EventKind::ControlChange { cc: 120 | 123, .. } => self.all_notes_off(),
            EventKind::ChannelPressure { value, .. } => self.aftertouch = value.clamp(0.0, 1.0),
            EventKind::AllNotesOff => self.all_notes_off(),
            _ => {}
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        self.take_mail();
        let bpm = ctx.transport.bpm.max(1.0);
        let sr = self.sr;
        // Tempo-locked free LFOs follow the song position while playing.
        let beats = ctx.transport.position_beats + start as f64 / sr * bpm / 60.0;
        for l in 0..LFOS {
            let cfg = self.s.lfo[l];
            if cfg.sync && ctx.transport.playing && !cfg.per_voice() && beats != self.last_beats {
                let cycle = division_beats(cfg.div);
                if cycle > 0.0 {
                    self.glfo[l].phase = (beats / cycle).rem_euclid(1.0);
                }
            }
        }
        self.last_beats = beats;

        let mut pos = start;
        let mut bl = [0.0f32; CHUNK];
        let mut br = [0.0f32; CHUNK];
        while pos < end {
            let n = CHUNK.min(end - pos);
            self.global_mods();
            self.update_fx(bpm);
            bl[..n].fill(0.0);
            br[..n].fill(0.0);
            {
                let tables: [&MipTable; OSCS] = std::array::from_fn(|o| table_for(&self.s, self.bank, &self.user, o));
                let ctx_v = Ctx {
                    c: &self.cache,
                    mx: &self.mx,
                    s: &self.s,
                    tables,
                    glfo: &self.glfo,
                    curves: [&self.curves[0], &self.curves[1], &self.curves[2], &self.curves[3]],
                    sr: sr as f32,
                    bend: self.bend,
                    mw: self.mod_wheel,
                    at: self.aftertouch,
                    macros: std::array::from_fn(|m| self.cache.plain(p::macro_knob(m as u32)) / 100.0),
                    bpm,
                };
                let mut handover = [false; MAX_VOICES];
                for (vi, v) in self.voices.iter_mut().enumerate() {
                    if !v.active {
                        continue;
                    }
                    v.control(&ctx_v, n);
                    v.render(&ctx_v, &mut bl[..n], &mut br[..n]);
                    if v.fading && v.fade <= 0.0 {
                        v.active = false;
                        v.fading = false;
                        handover[vi] = v.pending.is_some();
                    } else if !v.env[0].is_active() {
                        v.active = false;
                    }
                }
                for vi in 0..MAX_VOICES {
                    if handover[vi]
                        && let Some((key, vel, id)) = self.voices[vi].pending.take()
                    {
                        let from = self.last_pitch;
                        self.start_voice(vi, key, vel, id, Some(from));
                    }
                }
            }
            // Free-running LFOs.
            for l in 0..LFOS {
                let cfg = self.s.lfo[l];
                if !cfg.per_voice() {
                    let rate = if cfg.sync { division_hz(cfg.div, bpm) } else { mp(&self.cache, &self.mx, &self.gmods, p::lfo(l as u32, p::LFO_RATE), 0.0) as f64 };
                    self.glfo[l].advance(rate * n as f64 / sr, false);
                }
            }
            let vol = self.volume;
            let fx_on = self.fx_settings.active();
            let settings = self.fx_settings;
            let mut peak = self.peak;
            for i in 0..n {
                let (mut l, mut r) = (bl[i], br[i]);
                if fx_on {
                    (l, r) = self.fx.process(l, r, &settings);
                }
                bl[i] = sanitize(l * vol);
                br[i] = sanitize(r * vol);
                peak[0] = peak[0].max(bl[i].abs());
                peak[1] = peak[1].max(br[i].abs());
            }
            self.peak = peak;
            let buf = &mut *ctx.buffer;
            if buf.channels() >= 2 {
                let (ol, or) = buf.stereo_mut();
                ol[pos..pos + n].copy_from_slice(&bl[..n]);
                or[pos..pos + n].copy_from_slice(&br[..n]);
            } else if buf.channels() == 1 {
                let o = buf.channel_mut(0);
                for i in 0..n {
                    o[pos + i] = 0.5 * (bl[i] + br[i]);
                }
            }
            pos += n;
        }
        // Peak meters fall ~20 dB per second.
        let fall = (-((end - start) as f32) / (0.15 * sr as f32)).exp();
        self.peak[0] *= fall;
        self.peak[1] *= fall;
    }
}

// ── State ───────────────────────────────────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut val = [255u8; 256];
    for (i, &c) in B64.iter().enumerate() {
        val[c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        let v = val[c as usize];
        if v == 255 {
            return None;
        }
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn table_to_json(w: &WaveFrames) -> Value {
    let mut bytes = Vec::with_capacity(w.frames.len() * table::FRAME_LEN * 2);
    for v in w.frames.iter().flatten() {
        bytes.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32_767.0).round() as i16).to_le_bytes());
    }
    json!({ "name": w.name, "frame_len": table::FRAME_LEN, "pcm16": b64_encode(&bytes) })
}

fn table_from_json(v: &Value) -> Option<WaveFrames> {
    let name = v.get("name").and_then(Value::as_str).unwrap_or("User");
    let len = v.get("frame_len").and_then(Value::as_u64).unwrap_or(table::FRAME_LEN as u64) as usize;
    let bytes = b64_decode(v.get("pcm16")?.as_str()?)?;
    let samples: Vec<f32> = bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f32 / 32_767.0).collect();
    if len < 16 || samples.len() < len {
        return None;
    }
    Some(WaveFrames::new(name, samples.chunks_exact(len).map(<[f32]>::to_vec).collect()))
}

/// Device state holding these user tables and LFO curves, with every
/// parameter at the values of `params` (normalised by id). For editors that
/// write the state through a model command instead of a live synth.
pub fn state_with(params: &[(u32, f32)], tables: &[Option<&WaveFrames>], curves: &[Option<LfoCurve>]) -> Vec<u8> {
    let mut store = ParamStore::new(params::all());
    for &(id, v) in params {
        store.set(id, v);
    }
    let extra = json!({
        "user_tables": (0..OSCS).map(|o| tables.get(o).copied().flatten().map_or(Value::Null, table_to_json)).collect::<Vec<_>>(),
        "lfo_curves": (0..LFOS).map(|l| curves.get(l).copied().flatten().map_or(Value::Null, |c| c.to_json())).collect::<Vec<_>>(),
    });
    crate::common::save_state(&store, extra)
}

impl Processor for WavetableSynth {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        if sample_rate != self.sr {
            self.fx = SynthFx::new(sample_rate);
            for v in &mut self.voices {
                for f in &mut v.filt {
                    f.prepare(sample_rate);
                }
            }
        }
        self.sr = sample_rate;
        for v in &mut self.voices {
            for e in &mut v.env {
                e.set_sample_rate(sample_rate);
            }
        }
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.active = false;
            v.fading = false;
            v.pending = None;
            for e in &mut v.env {
                e.reset();
            }
            for f in &mut v.filt {
                f.reset();
            }
        }
        self.n_held = 0;
        self.sustain = false;
        self.fx.reset();
        for (l, g) in self.glfo.iter_mut().enumerate() {
            let _ = l;
            g.reset(0.0);
        }
        self.peak = [0.0; 2];
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
        let mut secs = self.params.plain(p::AMP_RELEASE) / 1000.0 * 1.2 + 0.05;
        if self.fx_settings.delay_mix > 0.0 {
            secs += 3.0;
        }
        if self.fx_settings.reverb_mix > 0.0 {
            secs += self.fx_settings.reverb_decay;
        }
        (secs as f64 * self.sr) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        let tables: Vec<Option<&WaveFrames>> = (0..OSCS).map(|o| self.user[o].frames.as_ref()).collect();
        let curves: Vec<Option<LfoCurve>> = (0..LFOS).map(|l| Some(*self.curves[l])).collect();
        let pairs: Vec<(u32, f32)> = self.params.infos().map(|i| (i.id, self.params.get(i.id))).collect();
        state_with(&pairs, &tables, &curves)
    }

    fn load_state(&mut self, data: &[u8]) {
        let extra = crate::common::load_state(&mut self.params, data);
        if let Some(arr) = extra.get("user_tables").and_then(Value::as_array) {
            for o in 0..OSCS {
                let frames = arr.get(o).and_then(table_from_json);
                *self.user[o] = UserTable::new(frames);
            }
        }
        if let Some(arr) = extra.get("lfo_curves").and_then(Value::as_array) {
            for l in 0..LFOS {
                if let Some(c) = arr.get(l).and_then(LfoCurve::from_json) {
                    *self.curves[l] = c;
                }
            }
        }
        self.params.mark_dirty();
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let n = self.voices.iter().filter(|v| v.active).count() as f32;
        let mut vals = [0.0f32; 3 + OSCS + LFOS + ENVS];
        vals[0] = n;
        vals[1] = self.peak[0];
        vals[2] = self.peak[1];
        if let Some(v) = self.voices.iter().filter(|v| v.active).max_by_key(|v| v.age) {
            for o in 0..OSCS {
                vals[3 + o] = v.osc[o].pos;
            }
            for l in 0..LFOS {
                vals[3 + OSCS + l] = v.src[src::LFO1 as usize + l];
            }
            for e in 0..ENVS {
                vals[3 + OSCS + LFOS + e] = v.env[e].value();
            }
        } else {
            for o in 0..OSCS {
                vals[3 + o] = self.cache.plain(p::osc(o as u32, p::POS)) / 100.0;
            }
        }
        write_meter(out, 0, &vals)
    }
}

#[cfg(test)]
mod tests;
