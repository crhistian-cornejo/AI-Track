//! Parameter ids and descriptions of `instrument.wavetable`.
//!
//! Ids 0–84 are the original synth's and keep their meaning and range, so
//! old projects and presets load unchanged; everything newer sits at higher
//! ids. Block helpers give every id from `(block index, field)`:
//!
//! * [`p::osc`]`(n, f)` — oscillator `n` (0..3), field `TABLE..=WAVE`.
//! * [`p::filt`]`(n, f)` — filter `n` (0..2), field `F_TYPE..=F_ENABLE`.
//! * [`p::env`]`(n, f)` — envelope `n` (0..3; 0 = amp), field `E_ATTACK..=E_R_CURVE`.
//! * [`p::lfo`]`(n, f)` — LFO `n` (0..4), field `LFO_SHAPE..=LFO_FADE`.
//! * [`p::slot_x`]`(s, f)` — matrix slot `s` (0..32), field `M_SRC..=M_VIA`.
//! * [`p::macro_knob`]`(m)` — macro `m` (0..4).
//!
//! Compatibility switches (value 0 means "use the original parameter"):
//! oscillator `VOICES` (0 → the 1..7 `UNISON` knob) and `WAVE` (0 → the
//! six-entry `TABLE` choice).

use super::filter::{MODELS, TYPES};
use super::tablegen::FACTORY_NAMES;
use super::modulation::{DST_MAX, MODES, SLOTS, SRC_MAX};
use crate::instrument::synth_fx::DIST_MODES;
use aitrack_dsp::ParamInfo;
use aitrack_dsp::lfo::{LfoShape, SYNC_DIVISIONS};
use std::sync::OnceLock;

/// Ids are below this (dense lookup tables use it).
pub const PARAM_SPACE: usize = 1024;

pub mod p {
    // ── Original ids (do not change) ──
    pub const OSC1: u32 = 0;
    pub const OSC2: u32 = 10;
    pub const TABLE: u32 = 0;
    pub const POS: u32 = 1;
    pub const OCTAVE: u32 = 2;
    pub const SEMI: u32 = 3;
    pub const FINE: u32 = 4;
    pub const LEVEL: u32 = 5;
    pub const PAN: u32 = 6;
    pub const UNISON: u32 = 7;
    pub const DETUNE: u32 = 8;
    pub const SPREAD: u32 = 9;

    pub const SUB_LEVEL: u32 = 20;
    pub const SUB_SHAPE: u32 = 21;
    pub const SUB_OCTAVE: u32 = 22;
    pub const NOISE: u32 = 23;

    pub const FILTER_TYPE: u32 = 24;
    pub const FILTER_SLOPE: u32 = 25;
    pub const CUTOFF: u32 = 26;
    pub const RESONANCE: u32 = 27;
    pub const DRIVE: u32 = 28;
    pub const KEY_TRACK: u32 = 29;
    pub const FILTER_ENV: u32 = 30;

    pub const AMP_ATTACK: u32 = 31;
    pub const AMP_DECAY: u32 = 32;
    pub const AMP_SUSTAIN: u32 = 33;
    pub const AMP_RELEASE: u32 = 34;
    pub const MOD_ATTACK: u32 = 35;
    pub const MOD_DECAY: u32 = 36;
    pub const MOD_SUSTAIN: u32 = 37;
    pub const MOD_RELEASE: u32 = 38;

    pub const LFO1: u32 = 39;
    pub const LFO2: u32 = 44;
    pub const LFO_SHAPE: u32 = 0;
    pub const LFO_RATE: u32 = 1;
    pub const LFO_SYNC: u32 = 2;
    pub const LFO_DIVISION: u32 = 3;
    pub const LFO_RETRIGGER: u32 = 4;

    /// Original 8-slot matrix (`MOD + 3 s + {MOD_SRC, MOD_DST, MOD_AMT}`),
    /// with the fixed `LEGACY_SOURCES` / `LEGACY_DESTS` lists. Still honoured;
    /// new work uses the 32-slot matrix at [`MATRIX`].
    pub const MOD: u32 = 49;
    pub const MOD_SRC: u32 = 0;
    pub const MOD_DST: u32 = 1;
    pub const MOD_AMT: u32 = 2;

    pub const VOICE_MODE: u32 = 73;
    pub const GLIDE: u32 = 74;
    pub const VEL_SENS: u32 = 75;
    pub const BEND_RANGE: u32 = 76;
    pub const POLYPHONY: u32 = 77;
    pub const CHORUS_MIX: u32 = 78;
    pub const DELAY_MIX: u32 = 79;
    pub const DELAY_TIME: u32 = 80;
    pub const DELAY_FEEDBACK: u32 = 81;
    pub const REVERB_MIX: u32 = 82;
    pub const REVERB_DECAY: u32 = 83;
    pub const VOLUME: u32 = 84;

    // ── Oscillator fields added later ──
    pub const WARP: u32 = 10;
    pub const WARP_AMT: u32 = 11;
    /// Start phase 0..100 %.
    pub const PHASE: u32 = 12;
    /// Unison start-phase randomisation 0..100 %.
    pub const RAND_PHASE: u32 = 13;
    /// Level of the outer unison voices against the centre 0..100 %.
    pub const BLEND: u32 = 14;
    /// Unison voices 1..16 (0 = use `UNISON`).
    pub const VOICES: u32 = 15;
    /// Filter routing: `ROUTES`.
    pub const ROUTE: u32 = 16;
    /// Wavetable: `waves()` (0 = the `TABLE` choice, 63 = the user table).
    pub const WAVE: u32 = 17;
    pub const OSC_FIELDS: u32 = 18;
    /// Original-shaped fields (`TABLE..=SPREAD`) of oscillator 3.
    pub const OSC3: u32 = 160;
    pub const OSC_EXT: u32 = 100;

    pub const SUB_ROUTE: u32 = 170;
    pub const SUB_PAN: u32 = 171;
    pub const NOISE_COLOR: u32 = 172;
    pub const NOISE_ROUTE: u32 = 173;
    pub const NOISE_PAN: u32 = 174;

    // ── Filter fields ──
    pub const F_TYPE: u32 = 0;
    pub const F_SLOPE: u32 = 1;
    pub const F_CUTOFF: u32 = 2;
    pub const F_RESO: u32 = 3;
    pub const F_DRIVE: u32 = 4;
    pub const F_KEY: u32 = 5;
    /// Env 2 → cutoff depth.
    pub const F_ENV: u32 = 6;
    pub const F_MODEL: u32 = 7;
    pub const F_ENABLE: u32 = 8;
    pub const FILTER_FIELDS: u32 = 9;
    /// Serial / Parallel.
    pub const FILTER_ROUTING: u32 = 200;

    // ── Envelope fields (env 0 = amp, 1 = the original mod env) ──
    pub const E_ATTACK: u32 = 0;
    pub const E_DECAY: u32 = 1;
    pub const E_SUSTAIN: u32 = 2;
    pub const E_RELEASE: u32 = 3;
    pub const E_HOLD: u32 = 4;
    pub const E_A_CURVE: u32 = 5;
    pub const E_D_CURVE: u32 = 6;
    pub const E_R_CURVE: u32 = 7;
    pub const ENV_FIELDS: u32 = 8;

    // ── LFO fields added later ──
    /// Use the drawn breakpoint curve instead of `LFO_SHAPE`.
    pub const LFO_DRAWN: u32 = 5;
    /// Run once per note and hold the end value (envelope-like).
    pub const LFO_ONESHOT: u32 = 6;
    /// Phase offset 0..100 %.
    pub const LFO_PHASE: u32 = 7;
    /// Fade-in time (ms) from the note start.
    pub const LFO_FADE: u32 = 8;
    pub const LFO_FIELDS: u32 = 9;

    pub const MACRO: u32 = 280;

    // ── Matrix ──
    pub const MATRIX: u32 = 300;
    pub const SLOT_STRIDE: u32 = 8;
    pub const MATRIX_END: u32 = MATRIX + SLOT_STRIDE * 32;
    pub const M_SRC: u32 = 0;
    pub const M_DST: u32 = 1;
    pub const M_AMT: u32 = 2;
    pub const M_MODE: u32 = 3;
    pub const M_VIA: u32 = 4;

    // ── FX additions ──
    pub const DIST_MODE: u32 = 600;
    pub const DIST_DRIVE: u32 = 601;
    pub const DIST_MIX: u32 = 602;
    pub const EQ_LOW: u32 = 603;
    pub const EQ_MID: u32 = 604;
    pub const EQ_MID_FREQ: u32 = 605;
    pub const EQ_HIGH: u32 = 606;
    pub const CHORUS_RATE: u32 = 607;
    pub const CHORUS_DEPTH: u32 = 608;
    pub const REVERB_TONE: u32 = 609;

    /// Analogue drift: per-note random detune of every oscillator, 0..100 %.
    pub const DRIFT: u32 = 620;

    /// Id of oscillator `n` (0..3) field `f` (`TABLE..=WAVE`).
    pub const fn osc(n: u32, f: u32) -> u32 {
        if f < 10 {
            match n {
                0 => OSC1 + f,
                1 => OSC2 + f,
                _ => OSC3 + f,
            }
        } else {
            OSC_EXT + 20 * n + (f - 10)
        }
    }

    /// Id of filter `n` (0..2) field `f` (`F_TYPE..=F_ENABLE`).
    pub const fn filt(n: u32, f: u32) -> u32 {
        if n == 0 {
            if f < 7 { FILTER_TYPE + f } else { 180 + (f - 7) }
        } else {
            190 + f
        }
    }

    /// Id of envelope `n` (0..3) field `f` (`E_ATTACK..=E_R_CURVE`).
    pub const fn env(n: u32, f: u32) -> u32 {
        if n < 2 && f < 4 { AMP_ATTACK + 4 * n + f } else { 210 + 10 * n + f }
    }

    /// Id of LFO `n` (0..4) field `f` (`LFO_SHAPE..=LFO_FADE`).
    pub const fn lfo(n: u32, f: u32) -> u32 {
        if n < 2 && f < 5 { if n == 0 { LFO1 + f } else { LFO2 + f } } else { 240 + 10 * n + f }
    }

    /// Original 8-slot matrix.
    pub const fn slot(s: u32, f: u32) -> u32 {
        MOD + 3 * s + f
    }

    /// 32-slot matrix slot `s` field `f` (`M_SRC..=M_VIA`).
    pub const fn slot_x(s: u32, f: u32) -> u32 {
        MATRIX + SLOT_STRIDE * s + f
    }

    pub const fn macro_knob(m: u32) -> u32 {
        MACRO + m
    }
}

/// Virtual modulation destinations (matrix `M_DST` values ≥ 900). Units:
/// pitch 100 % = 24 semitones; amp 100 % = +100 % gain; pan 100 % = hard
/// side; the rest are added to the normalised value of the knobs they name.
pub mod vdst {
    pub const PITCH: u32 = 900;
    /// `OSC_PITCH + n` for oscillator `n`.
    pub const OSC_PITCH: u32 = 901;
    pub const AMP: u32 = 904;
    pub const PAN: u32 = 905;
    /// Crossfade oscillator 1 ↔ 2 (−100 % = only osc 1).
    pub const OSC_MIX: u32 = 906;
    /// Unison detune of every oscillator.
    pub const DETUNE: u32 = 907;
    /// Both filters' cutoff.
    pub const CUTOFF: u32 = 908;
    /// Wavetable position of every oscillator.
    pub const POS: u32 = 909;
    pub const FIRST: u32 = 900;
    pub const LAST: u32 = 909;
}

/// `(id, name)` of the virtual destinations, for a destination picker.
pub const VIRTUAL_DESTS: &[(u32, &str)] = &[
    (vdst::PITCH, "Pitch"),
    (vdst::OSC_PITCH, "Osc 1 Pitch"),
    (vdst::OSC_PITCH + 1, "Osc 2 Pitch"),
    (vdst::OSC_PITCH + 2, "Osc 3 Pitch"),
    (vdst::AMP, "Amp"),
    (vdst::PAN, "Pan"),
    (vdst::OSC_MIX, "Osc 1/2 Mix"),
    (vdst::DETUNE, "Detune (all)"),
    (vdst::CUTOFF, "Cutoff (both)"),
    (vdst::POS, "Position (all)"),
];

/// Original oscillator tables (the `TABLE` choice).
pub const TABLES: &[&str] = &["Saw", "Square", "Pulse", "Sine-Saw", "Triangle", "Sine"];
/// `WAVE` value that plays the oscillator's user (imported / edited) table.
pub const USER_WAVE: usize = 63;
pub const WARPS: &[&str] = &["Off", "Sync", "Bend +", "Bend −", "Asym (PWM)", "Mirror", "Quantize", "Remap", "FM ← Next", "FM ← Prev", "RM ← Next"];
pub const ROUTES: &[&str] = &["Filter 1", "Filter 2", "Both", "Direct"];
pub const FILTER_ROUTINGS: &[&str] = &["Serial", "Parallel"];
pub const NOISE_COLORS: &[&str] = &["White", "Pink", "Brown", "Blue", "Violet", "Crackle"];
pub const SLOPES: &[&str] = &["12 dB", "24 dB"];
pub const SUB_SHAPES: &[&str] = &["Sine", "Square"];
pub const SUB_OCTAVES: &[&str] = &["-1 Oct", "-2 Oct"];
pub const VOICE_MODES: &[&str] = &["Poly", "Mono", "Legato"];
/// Delay time choices and their `SYNC_DIVISIONS` index.
pub const DELAY_TIMES: &[&str] = &["1/16", "1/8T", "1/8", "1/8.", "1/4T", "1/4", "1/4.", "1/2"];
pub(crate) const DELAY_DIV: [usize; 8] = [13, 12, 10, 11, 9, 7, 8, 4];
const OFF_ON: &[&str] = &["Off", "On"];

/// Original matrix lists (fixed; the 32-slot matrix uses `SOURCES`).
pub const LEGACY_SOURCES: &[&str] = &["None", "LFO 1", "LFO 2", "Mod Env", "Velocity", "Mod Wheel", "Aftertouch", "Key Track", "Pitch Bend", "Amp Env"];
pub const LEGACY_DESTS: &[&str] =
    &["None", "Osc 1 Pitch", "Osc 2 Pitch", "Osc 1 Pos", "Osc 2 Pos", "Cutoff", "Resonance", "Amp", "Pan", "LFO 1 Rate", "Osc Mix", "Detune", "Pitch"];

/// Plain values of the original slots' `MOD_SRC` choice.
pub mod legacy_src {
    pub const NONE: f32 = 0.0;
    pub const LFO1: f32 = 1.0;
    pub const LFO2: f32 = 2.0;
    pub const MOD_ENV: f32 = 3.0;
    pub const VELOCITY: f32 = 4.0;
    pub const MOD_WHEEL: f32 = 5.0;
    pub const AFTERTOUCH: f32 = 6.0;
    pub const KEY_TRACK: f32 = 7.0;
    pub const PITCH_BEND: f32 = 8.0;
    pub const AMP_ENV: f32 = 9.0;
}

/// Plain values of the original slots' `MOD_DST` choice.
pub mod legacy_dst {
    pub const NONE: f32 = 0.0;
    pub const OSC1_PITCH: f32 = 1.0;
    pub const OSC2_PITCH: f32 = 2.0;
    pub const OSC1_POS: f32 = 3.0;
    pub const OSC2_POS: f32 = 4.0;
    pub const CUTOFF: f32 = 5.0;
    pub const RESONANCE: f32 = 6.0;
    pub const AMP: f32 = 7.0;
    pub const PAN: f32 = 8.0;
    pub const LFO1_RATE: f32 = 9.0;
    pub const OSC_MIX: f32 = 10.0;
    pub const DETUNE: f32 = 11.0;
    pub const PITCH: f32 = 12.0;
}

/// Names of the `WAVE` choice: 0 "Table" (original choice), the factory
/// bank, then numbers, 63 "User".
pub fn waves() -> &'static [&'static str] {
    static W: OnceLock<Vec<&'static str>> = OnceLock::new();
    W.get_or_init(|| {
        let mut v = vec!["Table"];
        v.extend_from_slice(FACTORY_NAMES);
        while v.len() < USER_WAVE {
            v.push("—");
        }
        v.push("User");
        v
    })
}

/// Name of a matrix destination: a parameter's name or a virtual one.
pub fn dest_name(id: u32) -> &'static str {
    if let Some((_, n)) = VIRTUAL_DESTS.iter().find(|(i, _)| *i == id) {
        return n;
    }
    all().iter().find(|i| i.id == id).map_or("None", |i| i.name)
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// `names` padded with "—" to `len` entries (the range is kept wider than
/// the list so normalised values stay stable when entries are appended).
fn padded(names: &'static [&'static str], len: usize) -> &'static [&'static str] {
    let mut v = names.to_vec();
    v.resize(len.max(names.len()), "—");
    Box::leak(v.into_boxed_slice())
}

fn stepped(id: u32, name: &'static str, min: f32, max: f32, default: f32) -> ParamInfo {
    ParamInfo::new(id, name, min, max, default).choices(&[])
}

fn time(id: u32, name: &'static str, default: f32, max: f32) -> ParamInfo {
    ParamInfo::new(id, name, 0.5, max, default).unit("ms").log()
}

/// Every parameter (built once).
pub fn all() -> &'static [ParamInfo] {
    static ALL: OnceLock<Vec<ParamInfo>> = OnceLock::new();
    ALL.get_or_init(build)
}

fn build() -> Vec<ParamInfo> {
    let mut v = Vec::with_capacity(400);
    // Oscillators 1–3: the original fields.
    for o in 0..3u32 {
        let n = |f: &str| leak(format!("Osc {} {f}", o + 1));
        v.push(ParamInfo::new(p::osc(o, p::TABLE), n("Table"), 0.0, 5.0, 0.0).choices(TABLES).short("Table"));
        v.push(ParamInfo::new(p::osc(o, p::POS), n("Position"), 0.0, 100.0, 0.0).unit("%").short("Pos"));
        v.push(stepped(p::osc(o, p::OCTAVE), n("Octave"), -3.0, 3.0, 0.0).short("Oct"));
        v.push(stepped(p::osc(o, p::SEMI), n("Semi"), -12.0, 12.0, if o == 1 { 7.0 } else { 0.0 }).unit("st").short("Semi"));
        v.push(ParamInfo::new(p::osc(o, p::FINE), n("Fine"), -100.0, 100.0, 0.0).unit("ct").short("Fine"));
        v.push(ParamInfo::new(p::osc(o, p::LEVEL), n("Level"), 0.0, 100.0, if o == 0 { 80.0 } else { 0.0 }).unit("%").short("Level"));
        v.push(ParamInfo::new(p::osc(o, p::PAN), n("Pan"), -100.0, 100.0, 0.0).unit("%").short("Pan"));
        v.push(stepped(p::osc(o, p::UNISON), n("Unison"), 1.0, 7.0, 1.0).short("Voices"));
        v.push(ParamInfo::new(p::osc(o, p::DETUNE), n("Detune"), 0.0, 100.0, 25.0).unit("%").short("Detune"));
        v.push(ParamInfo::new(p::osc(o, p::SPREAD), n("Spread"), 0.0, 100.0, 50.0).unit("%").short("Spread"));
    }
    v.push(ParamInfo::new(p::SUB_LEVEL, "Sub Level", 0.0, 100.0, 0.0).unit("%").short("Sub"));
    v.push(ParamInfo::new(p::SUB_SHAPE, "Sub Shape", 0.0, 1.0, 0.0).choices(SUB_SHAPES).short("Sub Wave"));
    v.push(ParamInfo::new(p::SUB_OCTAVE, "Sub Octave", 0.0, 1.0, 0.0).choices(SUB_OCTAVES).short("Sub Oct"));
    v.push(ParamInfo::new(p::NOISE, "Noise Level", 0.0, 100.0, 0.0).unit("%").short("Noise"));
    // Filters.
    for f in 0..2u32 {
        let n = |x: &str| if f == 0 { leak(x.to_string()) } else { leak(format!("Filter 2 {x}")) };
        let (type_name, slope_name, drive_name, key_name, env_name) = if f == 0 {
            ("Filter Type", "Filter Slope", "Filter Drive", "Key Tracking", "Filter Env Amount")
        } else {
            ("Filter 2 Type", "Filter 2 Slope", "Filter 2 Drive", "Filter 2 Key Tracking", "Filter 2 Env Amount")
        };
        v.push(ParamInfo::new(p::filt(f, p::F_TYPE), type_name, 0.0, 2.0, 0.0).choices(TYPES).short("Type"));
        v.push(ParamInfo::new(p::filt(f, p::F_SLOPE), slope_name, 0.0, 1.0, 0.0).choices(SLOPES).short("Slope"));
        v.push(ParamInfo::new(p::filt(f, p::F_CUTOFF), n("Cutoff"), 20.0, 20_000.0, 20_000.0).unit("Hz").log().short("Cutoff"));
        v.push(ParamInfo::new(p::filt(f, p::F_RESO), n("Resonance"), 0.0, 100.0, 0.0).unit("%").short("Res"));
        v.push(ParamInfo::new(p::filt(f, p::F_DRIVE), drive_name, 0.0, 100.0, 0.0).unit("%").short("Drive"));
        v.push(ParamInfo::new(p::filt(f, p::F_KEY), key_name, 0.0, 100.0, 0.0).unit("%").short("Key"));
        v.push(ParamInfo::new(p::filt(f, p::F_ENV), env_name, -100.0, 100.0, 0.0).unit("%").short("Env"));
        let (model, enable) = if f == 0 { ("Filter Model", "Filter Enable") } else { ("Filter 2 Model", "Filter 2 Enable") };
        v.push(ParamInfo::new(p::filt(f, p::F_MODEL), model, 0.0, 15.0, 0.0).choices(padded(MODELS, 16)).short("Model"));
        v.push(ParamInfo::new(p::filt(f, p::F_ENABLE), enable, 0.0, 1.0, if f == 0 { 1.0 } else { 0.0 }).choices(OFF_ON).short("On"));
    }
    v.push(ParamInfo::new(p::FILTER_ROUTING, "Filter Routing", 0.0, 1.0, 0.0).choices(FILTER_ROUTINGS).short("Routing"));
    // Envelopes.
    const ENV_DEFAULTS: [(f32, f32, f32, f32); 3] = [(2.0, 300.0, 80.0, 200.0), (2.0, 400.0, 0.0, 300.0), (2.0, 400.0, 0.0, 300.0)];
    for e in 0..3u32 {
        let pre = ["Amp", "Mod", "Env 3"][e as usize];
        let n = |x: &str| leak(format!("{pre} {x}"));
        let (a, d, s, r) = ENV_DEFAULTS[e as usize];
        v.push(time(p::env(e, p::E_ATTACK), n("Attack"), a, 10_000.0).short("A"));
        v.push(time(p::env(e, p::E_DECAY), n("Decay"), d, 10_000.0).short("D"));
        v.push(ParamInfo::new(p::env(e, p::E_SUSTAIN), n("Sustain"), 0.0, 100.0, s).unit("%").short("S"));
        v.push(time(p::env(e, p::E_RELEASE), n("Release"), r, 10_000.0).short("R"));
        v.push(ParamInfo::new(p::env(e, p::E_HOLD), n("Hold"), 0.0, 5000.0, 0.0).unit("ms").power(3.0).short("H"));
        v.push(ParamInfo::new(p::env(e, p::E_A_CURVE), n("Attack Curve"), -100.0, 100.0, 0.0).unit("%").short("A Crv"));
        v.push(ParamInfo::new(p::env(e, p::E_D_CURVE), n("Decay Curve"), -100.0, 100.0, 70.0).unit("%").short("D Crv"));
        v.push(ParamInfo::new(p::env(e, p::E_R_CURVE), n("Release Curve"), -100.0, 100.0, 70.0).unit("%").short("R Crv"));
    }
    // LFOs.
    for l in 0..4u32 {
        let n = |x: &str| leak(format!("LFO {} {x}", l + 1));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_SHAPE), n("Shape"), 0.0, 6.0, 0.0).choices(LfoShape::NAMES).short("Shape"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_RATE), n("Rate"), 0.01, 40.0, 2.0).unit("Hz").log().short("Rate"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_SYNC), n("Sync"), 0.0, 1.0, 0.0).choices(OFF_ON).short("Sync"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_DIVISION), n("Division"), 0.0, (SYNC_DIVISIONS.len() - 1) as f32, 7.0).choices(SYNC_DIVISIONS).short("Div"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_RETRIGGER), n("Retrigger"), 0.0, 1.0, 0.0).choices(OFF_ON).short("Retrig"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_DRAWN), n("Drawn"), 0.0, 1.0, 0.0).choices(OFF_ON).short("Drawn"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_ONESHOT), n("One-Shot"), 0.0, 1.0, 0.0).choices(OFF_ON).short("1-Shot"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_PHASE), n("Phase"), 0.0, 100.0, 0.0).unit("%").short("Phase"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_FADE), n("Fade In"), 0.0, 10_000.0, 0.0).unit("ms").power(3.0).short("Fade"));
    }
    // Original matrix.
    for s in 0..8u32 {
        let n = |x: &str| leak(format!("Mod {} {x}", s + 1));
        v.push(ParamInfo::new(p::slot(s, p::MOD_SRC), n("Source"), 0.0, (LEGACY_SOURCES.len() - 1) as f32, 0.0).choices(LEGACY_SOURCES).short("Src"));
        v.push(ParamInfo::new(p::slot(s, p::MOD_DST), n("Destination"), 0.0, (LEGACY_DESTS.len() - 1) as f32, 0.0).choices(LEGACY_DESTS).short("Dst"));
        v.push(ParamInfo::new(p::slot(s, p::MOD_AMT), n("Amount"), -100.0, 100.0, 0.0).unit("%").short("Amt"));
    }
    v.push(ParamInfo::new(p::VOICE_MODE, "Voice Mode", 0.0, 2.0, 0.0).choices(VOICE_MODES).short("Mode"));
    v.push(ParamInfo::new(p::GLIDE, "Glide", 0.0, 2000.0, 0.0).unit("ms").power(2.0));
    v.push(ParamInfo::new(p::VEL_SENS, "Velocity Sensitivity", 0.0, 100.0, 70.0).unit("%").short("Vel"));
    v.push(stepped(p::BEND_RANGE, "Pitch Bend Range", 0.0, 24.0, 2.0).unit("st").short("Bend"));
    v.push(stepped(p::POLYPHONY, "Polyphony", 1.0, 16.0, 16.0).short("Poly"));
    v.push(ParamInfo::new(p::CHORUS_MIX, "Chorus Mix", 0.0, 100.0, 0.0).unit("%").short("Chorus"));
    v.push(ParamInfo::new(p::DELAY_MIX, "Delay Mix", 0.0, 100.0, 0.0).unit("%").short("Delay"));
    v.push(ParamInfo::new(p::DELAY_TIME, "Delay Time", 0.0, 7.0, 3.0).choices(DELAY_TIMES).short("Time"));
    v.push(ParamInfo::new(p::DELAY_FEEDBACK, "Delay Feedback", 0.0, 90.0, 35.0).unit("%").short("Fdbk"));
    v.push(ParamInfo::new(p::REVERB_MIX, "Reverb Mix", 0.0, 100.0, 0.0).unit("%").short("Reverb"));
    v.push(ParamInfo::new(p::REVERB_DECAY, "Reverb Decay", 0.2, 12.0, 2.0).unit("s").log().short("Decay"));
    v.push(ParamInfo::new(p::VOLUME, "Volume", -48.0, 6.0, -6.0).unit("dB"));
    // Oscillator additions.
    for o in 0..3u32 {
        let n = |f: &str| leak(format!("Osc {} {f}", o + 1));
        v.push(ParamInfo::new(p::osc(o, p::WARP), n("Warp"), 0.0, 15.0, 0.0).choices(padded(WARPS, 16)).short("Warp"));
        v.push(ParamInfo::new(p::osc(o, p::WARP_AMT), n("Warp Amount"), 0.0, 100.0, 0.0).unit("%").short("Amt"));
        v.push(ParamInfo::new(p::osc(o, p::PHASE), n("Phase"), 0.0, 100.0, 0.0).unit("%").short("Phase"));
        v.push(ParamInfo::new(p::osc(o, p::RAND_PHASE), n("Random Phase"), 0.0, 100.0, 100.0).unit("%").short("Rand"));
        v.push(ParamInfo::new(p::osc(o, p::BLEND), n("Unison Blend"), 0.0, 100.0, 100.0).unit("%").short("Blend"));
        v.push(stepped(p::osc(o, p::VOICES), n("Unison Voices"), 0.0, 16.0, 0.0).short("Voices"));
        v.push(ParamInfo::new(p::osc(o, p::ROUTE), n("Filter Route"), 0.0, 3.0, 0.0).choices(ROUTES).short("Route"));
        v.push(ParamInfo::new(p::osc(o, p::WAVE), n("Wavetable"), 0.0, USER_WAVE as f32, 0.0).choices(waves()).short("Wave"));
    }
    v.push(ParamInfo::new(p::SUB_ROUTE, "Sub Filter Route", 0.0, 3.0, 0.0).choices(ROUTES).short("Route"));
    v.push(ParamInfo::new(p::SUB_PAN, "Sub Pan", -100.0, 100.0, 0.0).unit("%").short("Pan"));
    v.push(ParamInfo::new(p::NOISE_COLOR, "Noise Color", 0.0, 7.0, 0.0).choices(padded(NOISE_COLORS, 8)).short("Color"));
    v.push(ParamInfo::new(p::NOISE_ROUTE, "Noise Filter Route", 0.0, 3.0, 0.0).choices(ROUTES).short("Route"));
    v.push(ParamInfo::new(p::NOISE_PAN, "Noise Pan", -100.0, 100.0, 0.0).unit("%").short("Pan"));
    for m in 0..4u32 {
        v.push(ParamInfo::new(p::macro_knob(m), leak(format!("Macro {}", m + 1)), 0.0, 100.0, 0.0).unit("%").short(leak(format!("M{}", m + 1))));
    }
    for s in 0..SLOTS as u32 {
        let n = |x: &str| leak(format!("Matrix {} {x}", s + 1));
        v.push(stepped(p::slot_x(s, p::M_SRC), n("Source"), 0.0, SRC_MAX, 0.0).short("Src"));
        v.push(stepped(p::slot_x(s, p::M_DST), n("Destination"), 0.0, DST_MAX, 0.0).short("Dst"));
        v.push(ParamInfo::new(p::slot_x(s, p::M_AMT), n("Amount"), -100.0, 100.0, 0.0).unit("%").short("Amt"));
        v.push(ParamInfo::new(p::slot_x(s, p::M_MODE), n("Polarity"), 0.0, 2.0, 0.0).choices(MODES).short("Pol"));
        v.push(stepped(p::slot_x(s, p::M_VIA), n("Via"), 0.0, SRC_MAX, 0.0).short("Via"));
    }
    v.push(ParamInfo::new(p::DIST_MODE, "Distortion", 0.0, 7.0, 0.0).choices(padded(DIST_MODES, 8)).short("Dist"));
    v.push(ParamInfo::new(p::DIST_DRIVE, "Distortion Drive", 0.0, 100.0, 30.0).unit("%").short("Drive"));
    v.push(ParamInfo::new(p::DIST_MIX, "Distortion Mix", 0.0, 100.0, 100.0).unit("%").short("Mix"));
    v.push(ParamInfo::new(p::EQ_LOW, "EQ Low", -18.0, 18.0, 0.0).unit("dB").short("Low"));
    v.push(ParamInfo::new(p::EQ_MID, "EQ Mid", -18.0, 18.0, 0.0).unit("dB").short("Mid"));
    v.push(ParamInfo::new(p::EQ_MID_FREQ, "EQ Mid Freq", 80.0, 12_000.0, 1000.0).unit("Hz").log().short("Freq"));
    v.push(ParamInfo::new(p::EQ_HIGH, "EQ High", -18.0, 18.0, 0.0).unit("dB").short("High"));
    v.push(ParamInfo::new(p::CHORUS_RATE, "Chorus Rate", 0.05, 8.0, 0.6).unit("Hz").log().short("Rate"));
    v.push(ParamInfo::new(p::CHORUS_DEPTH, "Chorus Depth", 0.0, 100.0, 50.0).unit("%").short("Depth"));
    v.push(ParamInfo::new(p::REVERB_TONE, "Reverb Tone", 1000.0, 18_000.0, 5500.0).unit("Hz").log().short("Tone"));
    v.push(ParamInfo::new(p::DRIFT, "Drift", 0.0, 100.0, 0.0).unit("%").short("Drift"));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_in_space() {
        let mut ids: Vec<u32> = all().iter().map(|i| i.id).collect();
        assert!(ids.iter().all(|&i| (i as usize) < PARAM_SPACE && !(vdst::FIRST..=vdst::LAST).contains(&i)));
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n);
        // The original ids keep their names.
        let name = |id| all().iter().find(|i| i.id == id).unwrap().name;
        assert_eq!(name(p::CUTOFF), "Cutoff");
        assert_eq!(name(p::osc(1, p::SPREAD)), "Osc 2 Spread");
        assert_eq!(name(p::lfo(1, p::LFO_RETRIGGER)), "LFO 2 Retrigger");
        assert_eq!(name(p::MOD_RELEASE), "Mod Release");
        assert_eq!(name(p::VOLUME), "Volume");
        assert_eq!(p::filt(0, p::F_ENV), p::FILTER_ENV);
        assert_eq!(p::env(1, p::E_RELEASE), p::MOD_RELEASE);
        assert_eq!(p::lfo(1, p::LFO_RETRIGGER), 48);
        assert_eq!(waves().len(), USER_WAVE + 1);
    }
}
