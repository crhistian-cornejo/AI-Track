//! `instrument.wavetable` — 16-voice wavetable / virtual-analogue synth.
//!
//! Per voice: two mip-mapped wavetable oscillators with up to 7 unison
//! voices (detune + stereo spread), a sub oscillator, noise, a ZDF state-
//! variable filter (LP/HP/BP, 12 or 24 dB/oct, drive, key tracking, envelope
//! amount), an amp ADSR and a mod ADSR. Two global LFOs (free or tempo
//! synced), an 8-slot modulation matrix, glide, poly/mono/legato, and small
//! built-in chorus, delay and reverb.
//!
//! Modulation and filter coefficients run at control rate (every 32
//! samples); oscillators, envelopes and the filter run per sample.
//!
//! Meter layout: `[active voices, peak L, peak R]`.

use super::synth_fx::{FxSettings, SynthFx};
use crate::common::{ParamStore, Segmented, drive, key_to_hz, load_state, pan_gains, sanitize, save_state, write_meter};
use aitrack_dsp::envelope::Adsr;
use aitrack_dsp::lfo::{Lfo, LfoShape, SYNC_DIVISIONS, division_beats, division_hz};
use aitrack_dsp::osc::{BlepOsc, Noise, VaShape};
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::wavetable::Wavetable;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use std::sync::OnceLock;

pub const ID: &str = "instrument.wavetable";

pub const MAX_VOICES: usize = 16;
pub const MAX_UNISON: usize = 7;
const CHUNK: usize = 32;
const STEAL_FADE: f32 = 1.0 / 64.0;
/// Per-voice output scale (headroom for chords).
const VOICE_GAIN: f32 = 0.25;
/// Modulation depth at 100 %.
const PITCH_RANGE: f32 = 24.0;
const CUTOFF_RANGE: f32 = 72.0;

pub mod p {
    pub const OSC1: u32 = 0;
    pub const OSC2: u32 = 10;
    // Offsets inside an oscillator block.
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
    // Offsets inside an LFO block.
    pub const LFO_SHAPE: u32 = 0;
    pub const LFO_RATE: u32 = 1;
    pub const LFO_SYNC: u32 = 2;
    pub const LFO_DIVISION: u32 = 3;
    pub const LFO_RETRIGGER: u32 = 4;

    /// Mod slot `s` (0..8): `MOD + 3 s + {MOD_SRC, MOD_DST, MOD_AMT}`.
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

    /// Id of oscillator `n` (0 or 1) field `f`.
    pub const fn osc(n: u32, f: u32) -> u32 {
        if n == 0 { OSC1 + f } else { OSC2 + f }
    }
    pub const fn lfo(n: u32, f: u32) -> u32 {
        if n == 0 { LFO1 + f } else { LFO2 + f }
    }
    pub const fn slot(s: u32, f: u32) -> u32 {
        MOD + 3 * s + f
    }
}

/// Oscillator tables, in `TABLES` order.
pub const TABLES: &[&str] = &["Saw", "Square", "Pulse", "Sine-Saw", "Triangle", "Sine"];
pub const SOURCES: &[&str] = &["None", "LFO 1", "LFO 2", "Mod Env", "Velocity", "Mod Wheel", "Aftertouch", "Key Track", "Pitch Bend", "Amp Env"];
pub const DESTS: &[&str] =
    &["None", "Osc 1 Pitch", "Osc 2 Pitch", "Osc 1 Pos", "Osc 2 Pos", "Cutoff", "Resonance", "Amp", "Pan", "LFO 1 Rate", "Osc Mix", "Detune", "Pitch"];
pub mod src {
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
pub mod dst {
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
const N_DST: usize = 13;
pub const FILTER_TYPES: &[&str] = &["Low-pass", "High-pass", "Band-pass"];
pub const SLOPES: &[&str] = &["12 dB", "24 dB"];
pub const SUB_SHAPES: &[&str] = &["Sine", "Square"];
pub const SUB_OCTAVES: &[&str] = &["-1 Oct", "-2 Oct"];
pub const VOICE_MODES: &[&str] = &["Poly", "Mono", "Legato"];
/// Delay time choices and their `SYNC_DIVISIONS` index.
pub const DELAY_TIMES: &[&str] = &["1/16", "1/8T", "1/8", "1/8.", "1/4T", "1/4", "1/4.", "1/2"];
const DELAY_DIV: [usize; 8] = [13, 12, 10, 11, 9, 7, 8, 4];
const OFF_ON: &[&str] = &["Off", "On"];

const OSC_NAMES: [[&str; 10]; 2] = [
    ["Osc 1 Table", "Osc 1 Position", "Osc 1 Octave", "Osc 1 Semi", "Osc 1 Fine", "Osc 1 Level", "Osc 1 Pan", "Osc 1 Unison", "Osc 1 Detune", "Osc 1 Spread"],
    ["Osc 2 Table", "Osc 2 Position", "Osc 2 Octave", "Osc 2 Semi", "Osc 2 Fine", "Osc 2 Level", "Osc 2 Pan", "Osc 2 Unison", "Osc 2 Detune", "Osc 2 Spread"],
];
const LFO_NAMES: [[&str; 5]; 2] = [
    ["LFO 1 Shape", "LFO 1 Rate", "LFO 1 Sync", "LFO 1 Division", "LFO 1 Retrigger"],
    ["LFO 2 Shape", "LFO 2 Rate", "LFO 2 Sync", "LFO 2 Division", "LFO 2 Retrigger"],
];
const SLOT_NAMES: [[&str; 3]; 8] = [
    ["Mod 1 Source", "Mod 1 Destination", "Mod 1 Amount"],
    ["Mod 2 Source", "Mod 2 Destination", "Mod 2 Amount"],
    ["Mod 3 Source", "Mod 3 Destination", "Mod 3 Amount"],
    ["Mod 4 Source", "Mod 4 Destination", "Mod 4 Amount"],
    ["Mod 5 Source", "Mod 5 Destination", "Mod 5 Amount"],
    ["Mod 6 Source", "Mod 6 Destination", "Mod 6 Amount"],
    ["Mod 7 Source", "Mod 7 Destination", "Mod 7 Amount"],
    ["Mod 8 Source", "Mod 8 Destination", "Mod 8 Amount"],
];

fn stepped(id: u32, name: &'static str, min: f32, max: f32, default: f32) -> ParamInfo {
    ParamInfo::new(id, name, min, max, default).choices(&[])
}

fn time(id: u32, name: &'static str, default: f32, max: f32) -> ParamInfo {
    ParamInfo::new(id, name, 0.5, max, default).unit("ms").log()
}

pub fn params() -> Vec<ParamInfo> {
    let mut v = Vec::with_capacity(85);
    for o in 0..2u32 {
        let n = &OSC_NAMES[o as usize];
        v.push(ParamInfo::new(p::osc(o, p::TABLE), n[0], 0.0, 5.0, 0.0).choices(TABLES).short("Table"));
        v.push(ParamInfo::new(p::osc(o, p::POS), n[1], 0.0, 100.0, 0.0).unit("%").short("Pos"));
        v.push(stepped(p::osc(o, p::OCTAVE), n[2], -3.0, 3.0, 0.0).short("Oct"));
        v.push(stepped(p::osc(o, p::SEMI), n[3], -12.0, 12.0, if o == 1 { 7.0 } else { 0.0 }).unit("st").short("Semi"));
        v.push(ParamInfo::new(p::osc(o, p::FINE), n[4], -100.0, 100.0, 0.0).unit("ct").short("Fine"));
        v.push(ParamInfo::new(p::osc(o, p::LEVEL), n[5], 0.0, 100.0, if o == 0 { 80.0 } else { 0.0 }).unit("%").short("Level"));
        v.push(ParamInfo::new(p::osc(o, p::PAN), n[6], -100.0, 100.0, 0.0).unit("%").short("Pan"));
        v.push(stepped(p::osc(o, p::UNISON), n[7], 1.0, 7.0, 1.0).short("Voices"));
        v.push(ParamInfo::new(p::osc(o, p::DETUNE), n[8], 0.0, 100.0, 25.0).unit("%").short("Detune"));
        v.push(ParamInfo::new(p::osc(o, p::SPREAD), n[9], 0.0, 100.0, 50.0).unit("%").short("Spread"));
    }
    v.push(ParamInfo::new(p::SUB_LEVEL, "Sub Level", 0.0, 100.0, 0.0).unit("%").short("Sub"));
    v.push(ParamInfo::new(p::SUB_SHAPE, "Sub Shape", 0.0, 1.0, 0.0).choices(SUB_SHAPES).short("Sub Wave"));
    v.push(ParamInfo::new(p::SUB_OCTAVE, "Sub Octave", 0.0, 1.0, 0.0).choices(SUB_OCTAVES).short("Sub Oct"));
    v.push(ParamInfo::new(p::NOISE, "Noise Level", 0.0, 100.0, 0.0).unit("%").short("Noise"));
    v.push(ParamInfo::new(p::FILTER_TYPE, "Filter Type", 0.0, 2.0, 0.0).choices(FILTER_TYPES).short("Type"));
    v.push(ParamInfo::new(p::FILTER_SLOPE, "Filter Slope", 0.0, 1.0, 0.0).choices(SLOPES).short("Slope"));
    v.push(ParamInfo::new(p::CUTOFF, "Cutoff", 20.0, 20_000.0, 20_000.0).unit("Hz").log());
    v.push(ParamInfo::new(p::RESONANCE, "Resonance", 0.0, 100.0, 0.0).unit("%").short("Res"));
    v.push(ParamInfo::new(p::DRIVE, "Filter Drive", 0.0, 100.0, 0.0).unit("%").short("Drive"));
    v.push(ParamInfo::new(p::KEY_TRACK, "Key Tracking", 0.0, 100.0, 0.0).unit("%").short("Key"));
    v.push(ParamInfo::new(p::FILTER_ENV, "Filter Env Amount", -100.0, 100.0, 0.0).unit("%").short("Env"));
    v.push(time(p::AMP_ATTACK, "Amp Attack", 2.0, 10_000.0).short("A"));
    v.push(time(p::AMP_DECAY, "Amp Decay", 300.0, 10_000.0).short("D"));
    v.push(ParamInfo::new(p::AMP_SUSTAIN, "Amp Sustain", 0.0, 100.0, 80.0).unit("%").short("S"));
    v.push(time(p::AMP_RELEASE, "Amp Release", 200.0, 10_000.0).short("R"));
    v.push(time(p::MOD_ATTACK, "Mod Attack", 2.0, 10_000.0).short("A"));
    v.push(time(p::MOD_DECAY, "Mod Decay", 400.0, 10_000.0).short("D"));
    v.push(ParamInfo::new(p::MOD_SUSTAIN, "Mod Sustain", 0.0, 100.0, 0.0).unit("%").short("S"));
    v.push(time(p::MOD_RELEASE, "Mod Release", 300.0, 10_000.0).short("R"));
    for l in 0..2u32 {
        let n = &LFO_NAMES[l as usize];
        v.push(ParamInfo::new(p::lfo(l, p::LFO_SHAPE), n[0], 0.0, 6.0, 0.0).choices(LfoShape::NAMES).short("Shape"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_RATE), n[1], 0.01, 40.0, 2.0).unit("Hz").log().short("Rate"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_SYNC), n[2], 0.0, 1.0, 0.0).choices(OFF_ON).short("Sync"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_DIVISION), n[3], 0.0, (SYNC_DIVISIONS.len() - 1) as f32, 7.0).choices(SYNC_DIVISIONS).short("Div"));
        v.push(ParamInfo::new(p::lfo(l, p::LFO_RETRIGGER), n[4], 0.0, 1.0, 0.0).choices(OFF_ON).short("Retrig"));
    }
    for s in 0..8u32 {
        let n = &SLOT_NAMES[s as usize];
        v.push(ParamInfo::new(p::slot(s, p::MOD_SRC), n[0], 0.0, (SOURCES.len() - 1) as f32, 0.0).choices(SOURCES).short("Src"));
        v.push(ParamInfo::new(p::slot(s, p::MOD_DST), n[1], 0.0, (DESTS.len() - 1) as f32, 0.0).choices(DESTS).short("Dst"));
        v.push(ParamInfo::new(p::slot(s, p::MOD_AMT), n[2], -100.0, 100.0, 0.0).unit("%").short("Amt"));
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
    v
}

struct Bank {
    tables: Vec<Wavetable>,
}

/// The oscillator tables, generated once per process (off the audio thread:
/// the first `create()` builds them).
fn bank() -> &'static Bank {
    static BANK: OnceLock<Bank> = OnceLock::new();
    BANK.get_or_init(|| Bank {
        tables: vec![
            Wavetable::saw(),
            Wavetable::square(),
            Wavetable::pulse_morph(16),
            Wavetable::sine_to_saw(16),
            Wavetable::triangle(),
            Wavetable::sine(),
        ],
    })
}

/// Settings resolved from the parameters (control thread values).
#[derive(Clone, Copy, Debug, Default)]
struct OscSettings {
    table: usize,
    pos: f32,
    transpose: f32,
    level: f32,
    pan: f32,
    unison: usize,
    detune: f32,
    spread: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Settings {
    osc: [OscSettings; 2],
    sub_level: f32,
    sub_square: bool,
    sub_oct: f32,
    noise: f32,
    filter_mode: usize,
    slope24: bool,
    cutoff: f32,
    reso: f32,
    drive: f32,
    key_track: f32,
    env_amt: f32,
    slots: [(u8, u8, f32); 8],
    n_slots: usize,
    mode: usize,
    glide_ms: f32,
    vel_sens: f32,
    bend_range: f32,
    poly: usize,
    volume: f32,
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    active: bool,
    released: bool,
    key: u8,
    note_id: i32,
    vel: f32,
    vel_gain: f32,
    age: u64,
    pitch: f32,
    target: f32,
    phases: [[f32; MAX_UNISON]; 2],
    incs: [[f32; MAX_UNISON]; 2],
    gains: [[(f32, f32); MAX_UNISON]; 2],
    unison: [usize; 2],
    levels: [usize; 2],
    pos: [f32; 2],
    sub: BlepOsc,
    sub_gain: f32,
    noise: Noise,
    noise_gain: f32,
    amp_env: Adsr,
    mod_env: Adsr,
    filt: [[SvfState; 2]; 2],
    coeffs: [SvfCoeffs; 2],
    amp_mod: f32,
    amp_step: f32,
    pan: (f32, f32),
    drive_g: f32,
    drive_makeup: f32,
    fade: f32,
    fading: bool,
    pending: Option<(u8, f32, i32)>,
}

impl Voice {
    fn new(sr: f64, seed: u32) -> Self {
        Self {
            active: false,
            released: false,
            key: 60,
            note_id: -1,
            vel: 0.0,
            vel_gain: 1.0,
            age: 0,
            pitch: 60.0,
            target: 60.0,
            phases: [[0.0; MAX_UNISON]; 2],
            incs: [[0.0; MAX_UNISON]; 2],
            gains: [[(0.0, 0.0); MAX_UNISON]; 2],
            unison: [1; 2],
            levels: [0; 2],
            pos: [0.0; 2],
            sub: BlepOsc::default(),
            sub_gain: 0.0,
            noise: Noise::new(seed),
            noise_gain: 0.0,
            amp_env: Adsr::new(sr),
            mod_env: Adsr::new(sr),
            filt: [[SvfState::default(); 2]; 2],
            coeffs: [SvfCoeffs::passthrough(); 2],
            amp_mod: 1.0,
            amp_step: 0.0,
            pan: (1.0, 1.0),
            drive_g: 1.0,
            drive_makeup: 1.0,
            fade: 1.0,
            fading: false,
            pending: None,
        }
    }
}

pub struct WavetableSynth {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    s: Settings,
    voices: [Voice; MAX_VOICES],
    lfos: [Lfo; 2],
    lfo_sync: [bool; 2],
    lfo_div: [usize; 2],
    lfo_rate: [f32; 2],
    lfo_retrig: [bool; 2],
    last_beats: f64,
    held: [u8; 16],
    n_held: usize,
    age: u64,
    last_pitch: f32,
    bend: f32,
    mod_wheel: f32,
    aftertouch: f32,
    fx: SynthFx,
    fx_settings: FxSettings,
    delay_div: usize,
    peak: [f32; 2],
    bank: &'static Bank,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(WavetableSynth::new())
}

impl Default for WavetableSynth {
    fn default() -> Self {
        Self::new()
    }
}

fn mode_of(i: usize) -> SvfMode {
    match i {
        1 => SvfMode::Highpass,
        2 => SvfMode::Bandpass,
        _ => SvfMode::Lowpass,
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
            sr,
            s: Settings::default(),
            voices: std::array::from_fn(|i| Voice::new(sr, 0x9E37_79B9u32.wrapping_mul(i as u32 + 1))),
            lfos: [Lfo::new(11), Lfo::new(23)],
            lfo_sync: [false; 2],
            lfo_div: [7; 2],
            lfo_rate: [2.0; 2],
            lfo_retrig: [false; 2],
            last_beats: f64::NAN,
            held: [0; 16],
            n_held: 0,
            age: 0,
            last_pitch: 60.0,
            bend: 0.0,
            mod_wheel: 0.0,
            aftertouch: 0.0,
            fx: SynthFx::new(sr),
            fx_settings: FxSettings::default(),
            delay_div: 10,
            peak: [0.0; 2],
            bank: bank(),
        };
        s.prepare(sr, 512);
        s
    }

    /// Number of sounding voices.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    fn update(&mut self) {
        let pr = &self.params;
        for o in 0..2u32 {
            let os = &mut self.s.osc[o as usize];
            os.table = pr.choice(p::osc(o, p::TABLE)).min(TABLES.len() - 1);
            os.pos = pr.plain(p::osc(o, p::POS)) / 100.0;
            os.transpose = pr.plain(p::osc(o, p::OCTAVE)).round() * 12.0 + pr.plain(p::osc(o, p::SEMI)).round() + pr.plain(p::osc(o, p::FINE)) / 100.0;
            os.level = pr.plain(p::osc(o, p::LEVEL)) / 100.0;
            os.pan = pr.plain(p::osc(o, p::PAN)) / 100.0;
            os.unison = (pr.plain(p::osc(o, p::UNISON)).round() as usize).clamp(1, MAX_UNISON);
            os.detune = pr.plain(p::osc(o, p::DETUNE)) / 100.0;
            os.spread = pr.plain(p::osc(o, p::SPREAD)) / 100.0;
        }
        let s = &mut self.s;
        s.sub_level = pr.plain(p::SUB_LEVEL) / 100.0;
        s.sub_square = pr.choice(p::SUB_SHAPE) == 1;
        s.sub_oct = if pr.choice(p::SUB_OCTAVE) == 1 { 24.0 } else { 12.0 };
        s.noise = pr.plain(p::NOISE) / 100.0;
        s.filter_mode = pr.choice(p::FILTER_TYPE);
        s.slope24 = pr.choice(p::FILTER_SLOPE) == 1;
        s.cutoff = pr.plain(p::CUTOFF);
        s.reso = pr.plain(p::RESONANCE) / 100.0;
        s.drive = pr.plain(p::DRIVE) / 100.0;
        s.key_track = pr.plain(p::KEY_TRACK) / 100.0;
        s.env_amt = pr.plain(p::FILTER_ENV) / 100.0;
        s.n_slots = 0;
        for i in 0..8u32 {
            let so = pr.choice(p::slot(i, p::MOD_SRC)) as u8;
            let d = pr.choice(p::slot(i, p::MOD_DST)) as u8;
            let a = pr.plain(p::slot(i, p::MOD_AMT)) / 100.0;
            if so != 0 && d != 0 && a != 0.0 && (d as usize) < N_DST && (so as usize) < SOURCES.len() {
                s.slots[s.n_slots] = (so, d, a);
                s.n_slots += 1;
            }
        }
        s.mode = pr.choice(p::VOICE_MODE);
        s.glide_ms = pr.plain(p::GLIDE);
        s.vel_sens = pr.plain(p::VEL_SENS) / 100.0;
        s.bend_range = pr.plain(p::BEND_RANGE).round();
        s.poly = (pr.plain(p::POLYPHONY).round() as usize).clamp(1, MAX_VOICES);
        s.volume = db_to_gain(pr.plain(p::VOLUME));
        for l in 0..2u32 {
            let i = l as usize;
            self.lfos[i].shape = LfoShape::from_index(pr.choice(p::lfo(l, p::LFO_SHAPE)));
            self.lfo_rate[i] = pr.plain(p::lfo(l, p::LFO_RATE));
            self.lfo_sync[i] = pr.on(p::lfo(l, p::LFO_SYNC));
            self.lfo_div[i] = pr.choice(p::lfo(l, p::LFO_DIVISION));
            self.lfo_retrig[i] = pr.on(p::lfo(l, p::LFO_RETRIGGER));
        }
        let (aa, ad, as_, ar) = (pr.plain(p::AMP_ATTACK), pr.plain(p::AMP_DECAY), pr.plain(p::AMP_SUSTAIN) / 100.0, pr.plain(p::AMP_RELEASE));
        let (ma, md, ms, mr) = (pr.plain(p::MOD_ATTACK), pr.plain(p::MOD_DECAY), pr.plain(p::MOD_SUSTAIN) / 100.0, pr.plain(p::MOD_RELEASE));
        for v in &mut self.voices {
            v.amp_env.set(aa, ad, as_, ar);
            v.mod_env.set(ma, md, ms, mr);
        }
        self.fx_settings.chorus_mix = pr.plain(p::CHORUS_MIX) / 100.0;
        self.fx_settings.delay_mix = pr.plain(p::DELAY_MIX) / 100.0;
        self.fx_settings.delay_feedback = pr.plain(p::DELAY_FEEDBACK) / 100.0;
        self.fx_settings.reverb_mix = pr.plain(p::REVERB_MIX) / 100.0;
        self.fx_settings.reverb_decay = pr.plain(p::REVERB_DECAY);
        self.delay_div = DELAY_DIV[pr.choice(p::DELAY_TIME).min(DELAY_DIV.len() - 1)];
    }

    // ── Notes ────────────────────────────────────────────────────────────

    fn start_voice(&mut self, vi: usize, key: u8, vel: f32, note_id: i32, glide_from: Option<f32>) {
        self.age += 1;
        let s = self.s;
        let v = &mut self.voices[vi];
        v.active = true;
        v.released = false;
        v.fading = false;
        v.fade = 1.0;
        v.pending = None;
        v.key = key;
        v.note_id = note_id;
        v.vel = vel.clamp(0.0, 1.0);
        v.vel_gain = 1.0 - s.vel_sens + s.vel_sens * v.vel * v.vel;
        v.age = self.age;
        v.target = key as f32;
        v.pitch = match glide_from {
            Some(p) if s.glide_ms > 0.0 => p,
            _ => key as f32,
        };
        for o in 0..2 {
            let n = s.osc[o].unison;
            for u in 0..MAX_UNISON {
                v.phases[o][u] = if n > 1 { v.noise.next_unipolar() } else { 0.0 };
            }
        }
        v.sub.set_phase(0.0);
        v.filt = [[SvfState::default(); 2]; 2];
        v.amp_env.retrigger();
        v.mod_env.retrigger();
        v.amp_mod = 1.0;
        v.amp_step = 0.0;
        self.last_pitch = key as f32;
        for l in 0..2 {
            if self.lfo_retrig[l] {
                self.lfos[l].reset(0.0);
            }
        }
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
                let from = v.pitch;
                let v = &mut self.voices[0];
                v.target = key as f32;
                v.key = key;
                v.note_id = note_id;
                if self.s.glide_ms <= 0.0 {
                    v.pitch = key as f32;
                }
                if !legato {
                    v.amp_env.gate_on();
                    v.mod_env.gate_on();
                    for l in 0..2 {
                        if self.lfo_retrig[l] {
                            self.lfos[l].reset(0.0);
                        }
                    }
                }
                let _ = from;
                self.last_pitch = key as f32;
            } else if v.active {
                // Released or fading tail: restart from where it is.
                let from = v.pitch;
                self.start_voice(0, key, vel, note_id, Some(from));
            } else {
                let from = self.last_pitch;
                self.start_voice(0, key, vel, note_id, Some(from));
            }
            return;
        }
        // Poly: retrigger the same key, else a free voice, else steal.
        for v in self.voices.iter_mut() {
            if v.active && v.key == key && !v.released {
                v.amp_env.gate_off();
                v.mod_env.gate_off();
                v.released = true;
            }
        }
        let poly = self.s.poly;
        let glide_from = Some(self.last_pitch);
        if let Some(vi) = (0..poly).find(|&i| !self.voices[i].active) {
            self.start_voice(vi, key, vel, note_id, glide_from);
            return;
        }
        // Steal: oldest released voice, else the oldest one.
        let vi = (0..poly)
            .filter(|&i| !self.voices[i].fading)
            .min_by_key(|&i| (!self.voices[i].released as u8, self.voices[i].age))
            .unwrap_or(0);
        let v = &mut self.voices[vi];
        v.fading = true;
        v.pending = Some((key, vel, note_id));
    }

    fn note_off(&mut self, key: u8, _note_id: i32) {
        if let Some(i) = self.held[..self.n_held].iter().position(|&k| k == key) {
            self.held.copy_within(i + 1..self.n_held, i);
            self.n_held -= 1;
        }
        if self.s.mode != 0 {
            let v = &mut self.voices[0];
            if !v.active || v.released {
                return;
            }
            if self.n_held > 0 {
                let back = self.held[self.n_held - 1];
                if v.key == key {
                    v.key = back;
                    v.target = back as f32;
                    if self.s.glide_ms <= 0.0 {
                        v.pitch = back as f32;
                    }
                }
            } else {
                v.amp_env.gate_off();
                v.mod_env.gate_off();
                v.released = true;
            }
            return;
        }
        for v in self.voices.iter_mut() {
            if v.active && v.key == key && !v.released {
                v.amp_env.gate_off();
                v.mod_env.gate_off();
                v.released = true;
            }
            if let Some((k, _, _)) = v.pending
                && k == key
            {
                // The note ended before its stolen voice was free: drop it.
                v.pending = None;
            }
        }
    }

    fn all_notes_off(&mut self) {
        self.n_held = 0;
        for v in self.voices.iter_mut() {
            if v.active {
                v.amp_env.gate_off();
                v.mod_env.gate_off();
                v.released = true;
                v.pending = None;
            }
        }
    }

    // ── Rendering ────────────────────────────────────────────────────────

    /// Control-rate update of one voice for the next `n` samples.
    fn control(&mut self, vi: usize, n: usize, lfo: [f32; 2]) {
        let s = self.s;
        let sr = self.sr as f32;
        let bend = self.bend;
        let (mw, at) = (self.mod_wheel, self.aftertouch);
        let bank = self.bank;
        let v = &mut self.voices[vi];
        // Glide.
        if s.glide_ms > 0.0 && v.pitch != v.target {
            // Time constant a quarter of the glide time: ~98 % there at `glide_ms`.
            let k = 1.0 - (-(n as f32) / (s.glide_ms * 0.00025 * sr)).exp();
            v.pitch += (v.target - v.pitch) * k;
            if (v.pitch - v.target).abs() < 0.001 {
                v.pitch = v.target;
            }
        } else {
            v.pitch = v.target;
        }
        // Modulation matrix.
        let mut d = [0.0f32; N_DST];
        for &(so, ds, a) in &s.slots[..s.n_slots] {
            let x = match so {
                1 => lfo[0],
                2 => lfo[1],
                3 => v.mod_env.value(),
                4 => v.vel,
                5 => mw,
                6 => at,
                7 => (v.pitch - 60.0) / 60.0,
                8 => bend,
                9 => v.amp_env.value(),
                _ => 0.0,
            };
            d[ds as usize] += a * x;
        }
        let base = v.pitch + bend * s.bend_range + d[12] * PITCH_RANGE;
        let detune = (s.osc[0].detune, s.osc[1].detune);
        let mix = d[10];
        for o in 0..2 {
            let os = s.osc[o];
            let mix_gain = if o == 0 { (1.0 - mix).clamp(0.0, 1.0) } else { (1.0 + mix).clamp(0.0, 1.0) };
            let level = os.level * mix_gain;
            let n_u = os.unison;
            v.unison[o] = if level > 0.0 { n_u } else { 0 };
            if level <= 0.0 {
                continue;
            }
            let pitch = base + os.transpose + d[1 + o] * PITCH_RANGE;
            let f0 = key_to_hz(pitch);
            let det = ((if o == 0 { detune.0 } else { detune.1 }) + d[11]).clamp(0.0, 1.0);
            let cents = 100.0 * det * det;
            let norm = level / (n_u as f32).sqrt();
            let mut max_inc = 0.0f32;
            for u in 0..n_u {
                let off = if n_u > 1 { -1.0 + 2.0 * u as f32 / (n_u - 1) as f32 } else { 0.0 };
                let f = f0 * 2f32.powf(off * cents / 1200.0);
                let inc = (f / sr).clamp(0.0, 0.49);
                v.incs[o][u] = inc;
                max_inc = max_inc.max(inc);
                let (gl, gr) = pan_gains((os.pan + os.spread * off).clamp(-1.0, 1.0));
                v.gains[o][u] = (gl * norm * std::f32::consts::SQRT_2, gr * norm * std::f32::consts::SQRT_2);
            }
            v.levels[o] = bank.tables[os.table].level_for(max_inc);
            v.pos[o] = (os.pos + d[3 + o]).clamp(0.0, 1.0);
        }
        // Sub and noise.
        v.sub_gain = s.sub_level;
        if s.sub_level > 0.0 {
            v.sub.shape = if s.sub_square { VaShape::Pulse } else { VaShape::Sine };
            v.sub.set_freq(key_to_hz(base - s.sub_oct), sr);
        }
        v.noise_gain = s.noise * 0.5;
        // Filter.
        let semis = s.key_track * (v.pitch - 60.0) + s.env_amt * v.mod_env.value() * CUTOFF_RANGE + d[5] * CUTOFF_RANGE;
        let cutoff = (s.cutoff * 2f32.powf(semis / 12.0)).clamp(20.0, (0.45 * sr).min(20_000.0));
        let r = (s.reso + d[6]).clamp(0.0, 1.0);
        if s.slope24 {
            let q1 = 0.541 * (1.0 + r * 0.2);
            let q2 = 1.307 * (20.0f32 / 1.307).powf(r);
            v.coeffs[0] = SvfCoeffs::new(mode_of(s.filter_mode), cutoff, q1, 0.0, self.sr);
            v.coeffs[1] = SvfCoeffs::new(mode_of(s.filter_mode), cutoff, q2, 0.0, self.sr);
        } else {
            let q = std::f32::consts::FRAC_1_SQRT_2 * (20.0f32 / std::f32::consts::FRAC_1_SQRT_2).powf(r);
            v.coeffs[0] = SvfCoeffs::new(mode_of(s.filter_mode), cutoff, q, 0.0, self.sr);
        }
        v.drive_g = 1.0 + 15.0 * s.drive * s.drive;
        v.drive_makeup = v.drive_g.powf(-0.4);
        // Amp modulation ramps across the chunk.
        let target = (1.0 + d[7]).clamp(0.0, 2.0);
        v.amp_step = (target - v.amp_mod) / n as f32;
        v.pan = {
            let pan = d[8].clamp(-1.0, 1.0);
            if pan >= 0.0 { ((pan * std::f32::consts::FRAC_PI_2).cos(), 1.0) } else { (1.0, (pan * std::f32::consts::FRAC_PI_2).cos()) }
        };
    }

    /// Render `n` samples of voice `vi`, adding into `l`/`r`.
    fn render_voice(&mut self, vi: usize, l: &mut [f32], r: &mut [f32]) {
        let s = self.s;
        let bank = self.bank;
        let slope24 = s.slope24;
        let drive = s.drive > 0.001;
        let v = &mut self.voices[vi];
        let tables = [&bank.tables[s.osc[0].table], &bank.tables[s.osc[1].table]];
        for i in 0..l.len() {
            let mut sl = 0.0f32;
            let mut sr_ = 0.0f32;
            for o in 0..2 {
                let t = tables[o];
                let (lvl, pos) = (v.levels[o], v.pos[o]);
                for u in 0..v.unison[o] {
                    let ph = v.phases[o][u];
                    let x = t.sample(lvl, pos, ph);
                    let mut nph = ph + v.incs[o][u];
                    if nph >= 1.0 {
                        nph -= 1.0;
                    }
                    v.phases[o][u] = nph;
                    let (gl, gr) = v.gains[o][u];
                    sl += x * gl;
                    sr_ += x * gr;
                }
            }
            if v.sub_gain > 0.0 {
                let x = v.sub.next() * v.sub_gain;
                sl += x;
                sr_ += x;
            }
            if v.noise_gain > 0.0 {
                sl += v.noise.next() * v.noise_gain;
                sr_ += v.noise.next() * v.noise_gain;
            }
            if drive {
                sl = (sl * v.drive_g).tanh() * v.drive_makeup;
                sr_ = (sr_ * v.drive_g).tanh() * v.drive_makeup;
            }
            let c0 = &v.coeffs[0];
            let mut yl = v.filt[0][0].process(c0, sl);
            let mut yr = v.filt[0][1].process(c0, sr_);
            if slope24 {
                let c1 = &v.coeffs[1];
                yl = v.filt[1][0].process(c1, yl);
                yr = v.filt[1][1].process(c1, yr);
            }
            let env = v.amp_env.next();
            v.mod_env.next();
            v.amp_mod += v.amp_step;
            if v.fading {
                v.fade -= STEAL_FADE;
                if v.fade <= 0.0 {
                    v.fade = 0.0;
                }
            }
            let g = env * v.vel_gain * v.amp_mod * v.fade * VOICE_GAIN;
            l[i] += yl * g * v.pan.0;
            r[i] += yr * g * v.pan.1;
            if v.fade <= 0.0 || !v.amp_env.is_active() {
                break;
            }
        }
        // Voice end / steal hand-over.
        if v.fading && v.fade <= 0.0 {
            v.active = false;
            v.fading = false;
            if let Some((key, vel, id)) = v.pending.take() {
                let from = self.last_pitch;
                self.start_voice(vi, key, vel, id, Some(from));
            }
        } else if !v.amp_env.is_active() {
            v.active = false;
        }
        let v = &mut self.voices[vi];
        if !(v.filt[0][0].is_finite() && v.filt[0][1].is_finite() && v.filt[1][0].is_finite() && v.filt[1][1].is_finite()) {
            v.filt = [[SvfState::default(); 2]; 2];
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
            EventKind::NoteOn { key, velocity, note_id, .. } => self.note_on(key, velocity, note_id),
            EventKind::NoteOff { key, note_id, .. } => self.note_off(key, note_id),
            EventKind::PitchBend { value, .. } => self.bend = value.clamp(-1.0, 1.0),
            EventKind::ControlChange { cc: 1, value, .. } => self.mod_wheel = value.clamp(0.0, 1.0),
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
        let bpm = ctx.transport.bpm.max(1.0);
        let sr = self.sr;
        // Tempo-locked LFOs follow the song position while playing.
        let beats = ctx.transport.position_beats + start as f64 / sr * bpm / 60.0;
        let mut rate = [0.0f32; 2];
        for l in 0..2 {
            rate[l] = if self.lfo_sync[l] { division_hz(self.lfo_div[l], bpm) as f32 } else { self.lfo_rate[l] };
            // Lock to the song position while it advances (a host that
            // stands still, or a free-running preview, lets it run).
            if self.lfo_sync[l] && ctx.transport.playing && !self.lfo_retrig[l] && beats != self.last_beats {
                self.lfos[l].sync_to_beats(beats, division_beats(self.lfo_div[l]), 0.0);
            }
        }
        // LFO 1 rate modulation from the global sources.
        let mut rate_mod = 0.0;
        for &(so, ds, a) in &self.s.slots[..self.s.n_slots] {
            if ds as f32 == dst::LFO1_RATE {
                rate_mod += a * match so {
                    2 => self.lfos[1].value(),
                    5 => self.mod_wheel,
                    6 => self.aftertouch,
                    8 => self.bend,
                    _ => 0.0,
                };
            }
        }
        self.last_beats = beats;
        rate[0] *= 2f32.powf(rate_mod * 4.0);
        self.lfos[0].set_rate(rate[0], sr);
        self.lfos[1].set_rate(rate[1], sr);
        self.fx_settings.delay_samples = (aitrack_dsp::lfo::division_seconds(self.delay_div, bpm) * sr) as f32;

        let mut pos = start;
        let mut bl = [0.0f32; CHUNK];
        let mut br = [0.0f32; CHUNK];
        while pos < end {
            let n = CHUNK.min(end - pos);
            let lfo = [self.lfos[0].value(), self.lfos[1].value()];
            self.lfos[0].advance(n);
            self.lfos[1].advance(n);
            bl[..n].fill(0.0);
            br[..n].fill(0.0);
            for vi in 0..MAX_VOICES {
                if self.voices[vi].active {
                    self.control(vi, n, lfo);
                    self.render_voice(vi, &mut bl[..n], &mut br[..n]);
                }
            }
            let vol = self.s.volume;
            let fx_on = self.fx_settings.chorus_mix > 0.0 || self.fx_settings.delay_mix > 0.0 || self.fx_settings.reverb_mix > 0.0;
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

impl Processor for WavetableSynth {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        if sample_rate != self.sr {
            self.fx = SynthFx::new(sample_rate);
        }
        self.sr = sample_rate;
        for v in &mut self.voices {
            v.amp_env.set_sample_rate(sample_rate);
            v.mod_env.set_sample_rate(sample_rate);
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
            v.amp_env.reset();
            v.mod_env.reset();
            v.filt = [[SvfState::default(); 2]; 2];
        }
        self.n_held = 0;
        self.fx.reset();
        self.lfos[0].reset(0.0);
        self.lfos[1].reset(0.0);
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
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let n = self.voices.iter().filter(|v| v.active).count() as f32;
        write_meter(out, 0, &[n, self.peak[0], self.peak[1]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, render_notes, rms, set_plain};

    /// Estimate the fundamental from upward zero crossings.
    fn pitch_hz(x: &[f32]) -> f32 {
        let mut crossings = Vec::new();
        for i in 1..x.len() {
            if x[i - 1] <= 0.0 && x[i] > 0.0 {
                let t = x[i - 1] / (x[i - 1] - x[i]);
                crossings.push(i as f32 - 1.0 + t);
            }
        }
        let n = crossings.len();
        assert!(n > 4, "no oscillation");
        (n - 1) as f32 / (crossings[n - 1] - crossings[0]) * SR as f32
    }

    fn sine_patch() -> WavetableSynth {
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::osc(0, p::TABLE), 5.0);
        s
    }

    #[test]
    fn plays_the_right_pitch() {
        let mut s = sine_patch();
        let (l, _) = render_notes(&mut s, &[(0, 69, 1.0, true)], 24_000);
        let hz = pitch_hz(&l[4800..]);
        assert!((hz - 440.0).abs() < 1.0, "{hz}");
        assert!(rms(&l[4800..]) > 0.05);
        // Octave and semitone offsets.
        let mut s = sine_patch();
        set_plain(&mut s, p::osc(0, p::OCTAVE), 1.0);
        set_plain(&mut s, p::osc(0, p::SEMI), -12.0);
        let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true)], 24_000);
        assert!((pitch_hz(&l[4800..]) - 220.0).abs() < 1.0);
    }

    #[test]
    fn note_off_releases_to_silence() {
        let mut s = sine_patch();
        set_plain(&mut s, p::AMP_RELEASE, 50.0);
        let (l, _) = render_notes(&mut s, &[(0, 60, 1.0, true), (4800, 60, 0.0, false)], 24_000);
        assert!(rms(&l[2000..4800]) > 0.05);
        assert!(rms(&l[20_000..]) < 1e-4);
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn voices_cap_at_polyphony() {
        let mut s = WavetableSynth::new();
        let notes: Vec<(usize, u8, f32, bool)> = (0..24).map(|i| (i * 10, 40 + i as u8, 0.8, true)).collect();
        render_notes(&mut s, &notes, 4800);
        assert_eq!(s.active_voices(), 16);
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::POLYPHONY, 4.0);
        render_notes(&mut s, &notes, 4800);
        assert_eq!(s.active_voices(), 4);
    }

    #[test]
    fn glide_moves_pitch() {
        let mut s = sine_patch();
        set_plain(&mut s, p::VOICE_MODE, 2.0);
        set_plain(&mut s, p::GLIDE, 200.0);
        let (l, _) = render_notes(&mut s, &[(0, 57, 1.0, true), (12_000, 69, 1.0, true)], 48_000);
        let early = pitch_hz(&l[12_200..13_500]);
        let late = pitch_hz(&l[40_000..]);
        assert!(early > 225.0 && early < 400.0, "{early}");
        assert!((late - 440.0).abs() < 2.0, "{late}");
        // Legato keeps one voice.
        assert_eq!(s.active_voices(), 1);
    }

    #[test]
    fn filter_darkens_a_saw() {
        let mut bright = WavetableSynth::new();
        let (a, _) = render_notes(&mut bright, &[(0, 48, 1.0, true)], 12_000);
        let mut dark = WavetableSynth::new();
        set_plain(&mut dark, p::CUTOFF, 200.0);
        set_plain(&mut dark, p::FILTER_SLOPE, 1.0);
        let (b, _) = render_notes(&mut dark, &[(0, 48, 1.0, true)], 12_000);
        // High-frequency content: first difference energy.
        let hf = |x: &[f32]| x.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum::<f32>();
        assert!(hf(&b[2000..]) < 0.05 * hf(&a[2000..]));
    }

    #[test]
    fn unison_spreads_stereo() {
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::osc(0, p::UNISON), 7.0);
        set_plain(&mut s, p::osc(0, p::SPREAD), 100.0);
        let (l, r) = render_notes(&mut s, &[(0, 60, 1.0, true)], 24_000);
        let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 10.0);
    }

    #[test]
    fn mod_env_pitch_drop_808() {
        let mut s = sine_patch();
        set_plain(&mut s, p::MOD_DECAY, 80.0);
        set_plain(&mut s, p::MOD_SUSTAIN, 0.0);
        set_plain(&mut s, p::slot(0, p::MOD_SRC), src::MOD_ENV);
        set_plain(&mut s, p::slot(0, p::MOD_DST), dst::PITCH);
        set_plain(&mut s, p::slot(0, p::MOD_AMT), 50.0);
        let (l, _) = render_notes(&mut s, &[(0, 45, 1.0, true)], 24_000);
        let start = pitch_hz(&l[0..2400]);
        let end = pitch_hz(&l[16_000..]);
        assert!(start > 125.0, "{start}"); // averaged over the 50 ms drop from 220 Hz
        assert!((end - 110.0).abs() < 1.5, "{end}");
    }

    #[test]
    fn synced_lfo_on_cutoff_wobbles() {
        let mut s = WavetableSynth::new();
        set_plain(&mut s, p::CUTOFF, 300.0);
        set_plain(&mut s, p::lfo(0, p::LFO_SYNC), 1.0);
        set_plain(&mut s, p::lfo(0, p::LFO_DIVISION), 10.0); // 1/8
        set_plain(&mut s, p::slot(0, p::MOD_SRC), src::LFO1);
        set_plain(&mut s, p::slot(0, p::MOD_DST), dst::CUTOFF);
        set_plain(&mut s, p::slot(0, p::MOD_AMT), 50.0);
        let (l, _) = render_notes(&mut s, &[(0, 40, 1.0, true)], 48_000);
        // Envelope of the HF energy must oscillate (wobble).
        let seg = 1200;
        let e: Vec<f32> = l.chunks(seg).map(|c| c.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum::<f32>()).collect();
        let max = e[5..].iter().cloned().fold(0.0, f32::max);
        let min = e[5..].iter().cloned().fold(f32::MAX, f32::min);
        assert!(max > 4.0 * min, "{max} {min}");
    }
}
