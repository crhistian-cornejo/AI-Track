//! Vault parameter ids and descriptions.
//!
//! Layout (stable, stored in projects and presets):
//!
//! | ids | block |
//! |---|---|
//! | 0..=23 | global: engine, macros, volume, bend, polyphony, FX, macro depths |
//! | 100.. | [`ladder`] — mono ladder synth |
//! | 200.. | [`poly`] — poly analog synth |
//! | 300.. | [`fm`] — 6-operator FM (`fm::op(n, field)`) |
//! | 400.. | [`ep`] — electric piano |
//! | 500.. | [`organ`] — drawbar organ |
//! | 600.. | [`strings`] — string machine |
//! | 4096.. | the hosted wavetable synth: `WT_BASE + wavetable::p::…` |

use aitrack_dsp::ParamInfo;
use crate::instrument::wavetable;
use std::sync::OnceLock;

/// Engine choices of [`ENGINE`], in id order.
pub const ENGINES: &[&str] = &["Mono Ladder", "Poly Analog", "FM6", "Electric Piano", "Drawbar Organ", "String Machine", "Wavetable"];
/// Short machine names of the engines (tags, search).
pub const ENGINE_TAGS: &[&str] = &["ladder", "analog", "fm", "epiano", "organ", "strings", "wavetable"];
pub const ENGINE_COUNT: usize = 7;

pub mod engine {
    pub const LADDER: usize = 0;
    pub const POLY: usize = 1;
    pub const FM: usize = 2;
    pub const EP: usize = 3;
    pub const ORGAN: usize = 4;
    pub const STRINGS: usize = 5;
    pub const WAVETABLE: usize = 6;
}

// ── Global ──
pub const ENGINE: u32 = 0;
/// Macro `m` (0..4) value, 0..100 %.
pub const MACRO1: u32 = 1;
pub const VOLUME: u32 = 5;
/// Pitch-bend range in semitones.
pub const BEND_RANGE: u32 = 6;
/// Voices of the polyphonic engines (1..16).
pub const POLYPHONY: u32 = 7;
pub const DRIVE: u32 = 10;
pub const EQ_LOW: u32 = 11;
pub const EQ_HIGH: u32 = 12;
pub const DELAY_MIX: u32 = 13;
pub const DELAY_TIME: u32 = 14;
pub const DELAY_FEEDBACK: u32 = 15;
pub const REVERB_MIX: u32 = 16;
pub const REVERB_DECAY: u32 = 17;
/// Depth of macro `m` (−100..100 %): scales every target range of it.
pub const MACRO_DEPTH1: u32 = 20;

pub const fn macro_knob(m: u32) -> u32 {
    MACRO1 + m
}

pub const fn macro_depth(m: u32) -> u32 {
    MACRO_DEPTH1 + m
}

/// First id of the hosted wavetable synth's parameters.
pub const WT_BASE: u32 = 4096;
/// Highest id + 1.
pub const PARAM_SPACE: usize = WT_BASE as usize + wavetable::PARAM_SPACE;

/// Wavetable id → Vault id.
pub const fn wt(id: u32) -> u32 {
    WT_BASE + id
}

pub const OSC_WAVES: &[&str] = &["Saw", "Square", "Triangle", "Pulse"];
pub const OCTAVES: &[&str] = &["32'", "16'", "8'", "4'", "2'"];
pub const OFF_ON: &[&str] = &["Off", "On"];
/// Delay divisions shown as [`DELAY_TIMES`] → `SYNC_DIVISIONS` index.
pub const DELAY_TIMES: &[&str] = wavetable::DELAY_TIMES;
pub(crate) const DELAY_DIV: [usize; 8] = [13, 12, 10, 11, 9, 7, 8, 4];

/// Mono ladder synth (100..).
pub mod ladder {
    pub const BASE: u32 = 100;
    /// Oscillator `n` (0..3) field: wave, octave, detune (semitones), level.
    pub const fn osc(n: u32, f: u32) -> u32 {
        BASE + 4 * n + f
    }
    pub const WAVE: u32 = 0;
    pub const OCTAVE: u32 = 1;
    pub const DETUNE: u32 = 2;
    pub const LEVEL: u32 = 3;
    pub const NOISE: u32 = BASE + 12;
    pub const CUTOFF: u32 = BASE + 13;
    pub const RESONANCE: u32 = BASE + 14;
    pub const DRIVE: u32 = BASE + 15;
    pub const ENV_AMOUNT: u32 = BASE + 16;
    pub const KEY_TRACK: u32 = BASE + 17;
    pub const F_ATTACK: u32 = BASE + 18;
    pub const F_DECAY: u32 = BASE + 19;
    pub const F_SUSTAIN: u32 = BASE + 20;
    pub const F_RELEASE: u32 = BASE + 21;
    pub const A_ATTACK: u32 = BASE + 22;
    pub const A_DECAY: u32 = BASE + 23;
    pub const A_SUSTAIN: u32 = BASE + 24;
    pub const A_RELEASE: u32 = BASE + 25;
    pub const GLIDE: u32 = BASE + 26;
    pub const LEGATO: u32 = BASE + 27;
    pub const LFO_RATE: u32 = BASE + 28;
    pub const LFO_PITCH: u32 = BASE + 29;
    pub const LFO_CUTOFF: u32 = BASE + 30;
    pub const VEL_CUTOFF: u32 = BASE + 31;
    pub const VEL_AMP: u32 = BASE + 32;
    pub const END: u32 = BASE + 33;
}

/// Poly analog synth (200..).
pub mod poly {
    pub const BASE: u32 = 200;
    pub const A_WAVE: u32 = BASE;
    pub const A_PW: u32 = BASE + 1;
    pub const B_WAVE: u32 = BASE + 2;
    pub const B_SEMI: u32 = BASE + 3;
    pub const B_FINE: u32 = BASE + 4;
    pub const B_PW: u32 = BASE + 5;
    /// A ↔ B balance, 0 = A only, 100 = B only.
    pub const MIX: u32 = BASE + 6;
    pub const NOISE: u32 = BASE + 7;
    /// Hard-sync oscillator A to B.
    pub const SYNC: u32 = BASE + 8;
    pub const CUTOFF: u32 = BASE + 9;
    pub const RESONANCE: u32 = BASE + 10;
    pub const ENV_AMOUNT: u32 = BASE + 11;
    pub const KEY_TRACK: u32 = BASE + 12;
    pub const F_ATTACK: u32 = BASE + 13;
    pub const F_DECAY: u32 = BASE + 14;
    pub const F_SUSTAIN: u32 = BASE + 15;
    pub const F_RELEASE: u32 = BASE + 16;
    pub const A_ATTACK: u32 = BASE + 17;
    pub const A_DECAY: u32 = BASE + 18;
    pub const A_SUSTAIN: u32 = BASE + 19;
    pub const A_RELEASE: u32 = BASE + 20;
    /// Poly-mod source depths (filter envelope, oscillator B), 0..100 %.
    pub const PM_ENV: u32 = BASE + 21;
    pub const PM_OSC_B: u32 = BASE + 22;
    /// Poly-mod destinations (on/off): oscillator A frequency, A pulse width, filter cutoff.
    pub const PM_FREQ: u32 = BASE + 23;
    pub const PM_PW: u32 = BASE + 24;
    pub const PM_FILTER: u32 = BASE + 25;
    pub const LFO_RATE: u32 = BASE + 26;
    pub const LFO_PITCH: u32 = BASE + 27;
    pub const LFO_PW: u32 = BASE + 28;
    pub const LFO_CUTOFF: u32 = BASE + 29;
    /// Per-voice analog drift of pitch and cutoff, 0..100 %.
    pub const DRIFT: u32 = BASE + 30;
    /// Built-in chorus: Off, I, II, I+II.
    pub const CHORUS: u32 = BASE + 31;
    pub const GLIDE: u32 = BASE + 32;
    pub const VEL_AMP: u32 = BASE + 33;
    pub const VEL_FILTER: u32 = BASE + 34;
    /// Stereo spread of the voices, 0..100 %.
    pub const SPREAD: u32 = BASE + 35;
    pub const END: u32 = BASE + 36;
}

/// 6-operator FM (300..).
pub mod fm {
    pub const BASE: u32 = 300;
    pub const ALGORITHM: u32 = BASE;
    pub const FEEDBACK: u32 = BASE + 1;
    /// Scales every modulator level, 0..200 %.
    pub const BRIGHTNESS: u32 = BASE + 2;
    /// Scales every envelope time, 25..400 %.
    pub const TIME: u32 = BASE + 3;
    pub const LFO_RATE: u32 = BASE + 4;
    pub const LFO_PITCH: u32 = BASE + 5;
    pub const LFO_AMP: u32 = BASE + 6;
    /// Pitch envelope depth (semitones) and decay (ms): a falling blip at the start.
    pub const PITCH_ENV: u32 = BASE + 7;
    pub const PITCH_DECAY: u32 = BASE + 8;
    pub const OPS: u32 = BASE + 10;
    pub const OP_STRIDE: u32 = 12;
    /// Operator `n` (0..6, 0 = op 1) field `f`.
    pub const fn op(n: u32, f: u32) -> u32 {
        OPS + OP_STRIDE * n + f
    }
    /// Coarse ratio: 0 = ½, 1..=16.
    pub const COARSE: u32 = 0;
    /// Fine ratio, 0..99 % added to the coarse ratio.
    pub const FINE: u32 = 1;
    /// Detune, ±10 cents.
    pub const DETUNE: u32 = 2;
    /// Output level 0..99 (≈0.75 dB a step).
    pub const LEVEL: u32 = 3;
    pub const ATTACK: u32 = 4;
    pub const DECAY: u32 = 5;
    pub const SUSTAIN: u32 = 6;
    pub const RELEASE: u32 = 7;
    /// Velocity sensitivity, 0..100 %.
    pub const VEL: u32 = 8;
    /// Level drop for high keys, 0..100 % (100 % = −6 dB an octave above C4).
    pub const KEY_SCALE: u32 = 9;
    pub const END: u32 = OPS + OP_STRIDE * 6;
}

/// Electric piano (400..).
pub mod ep {
    pub const BASE: u32 = 400;
    /// Tine or Reed.
    pub const MODEL: u32 = BASE;
    /// Hammer hardness: level of the upper modes, 0..100 %.
    pub const HARDNESS: u32 = BASE + 1;
    /// The tine's bell partial, 0..100 %.
    pub const BELL: u32 = BASE + 2;
    /// Pickup nonlinearity ("bark" when played hard), 0..100 %.
    pub const BARK: u32 = BASE + 3;
    /// Pickup offset: 0 = centred (odd harmonics), 100 = off-axis (even, bright).
    pub const PICKUP: u32 = BASE + 4;
    /// Decay scale, 10..400 %.
    pub const DECAY: u32 = BASE + 5;
    /// Damper release time.
    pub const RELEASE: u32 = BASE + 6;
    /// Velocity → level and timbre, 0..100 %.
    pub const VEL: u32 = BASE + 7;
    pub const TREM_RATE: u32 = BASE + 8;
    pub const TREM_DEPTH: u32 = BASE + 9;
    /// Tremolo as amplitude (off) or stereo auto-pan (on).
    pub const AUTOPAN: u32 = BASE + 10;
    /// Low-pass tone control.
    pub const TONE: u32 = BASE + 11;
    /// Hammer thump, 0..100 %.
    pub const NOISE: u32 = BASE + 12;
    /// Random per-key detune (cents).
    pub const DETUNE: u32 = BASE + 13;
    pub const END: u32 = BASE + 14;
}

/// Drawbar organ (500..).
pub mod organ {
    pub const BASE: u32 = 500;
    /// Drawbar `n` (0..9: 16', 5⅓', 8', 4', 2⅔', 2', 1⅗', 1⅓', 1'), 0..8.
    pub const fn drawbar(n: u32) -> u32 {
        BASE + n
    }
    pub const PERC: u32 = BASE + 9;
    /// Second or third harmonic.
    pub const PERC_HARM: u32 = BASE + 10;
    pub const PERC_DECAY: u32 = BASE + 11;
    pub const PERC_VOL: u32 = BASE + 12;
    pub const CLICK: u32 = BASE + 13;
    /// Rotary speaker mix, 0..100 %.
    pub const ROTARY: u32 = BASE + 14;
    /// Slow / Fast.
    pub const SPEED: u32 = BASE + 15;
    /// Speed-change ramp, 10..300 % of the natural inertia.
    pub const RAMP: u32 = BASE + 16;
    pub const DRIVE: u32 = BASE + 17;
    /// Scanner vibrato / chorus: Off, V1, V2, V3, C1, C2, C3.
    pub const VIBRATO: u32 = BASE + 18;
    pub const TONE: u32 = BASE + 19;
    pub const RELEASE: u32 = BASE + 20;
    pub const END: u32 = BASE + 21;
}

/// String machine (600..).
pub mod strings {
    pub const BASE: u32 = 600;
    /// Register levels, 0..100 %: 16' cello, 8' violin, 4' viola (saws), 8' horn (square).
    pub const CELLO: u32 = BASE;
    pub const VIOLIN: u32 = BASE + 1;
    pub const VIOLA: u32 = BASE + 2;
    pub const HORN: u32 = BASE + 3;
    pub const ATTACK: u32 = BASE + 4;
    pub const RELEASE: u32 = BASE + 5;
    pub const TONE: u32 = BASE + 6;
    pub const RESONANCE: u32 = BASE + 7;
    pub const HIGH_PASS: u32 = BASE + 8;
    /// Ensemble mix / depth (0..100 %) and rate (Hz).
    pub const ENSEMBLE: u32 = BASE + 9;
    pub const ENS_DEPTH: u32 = BASE + 10;
    pub const ENS_RATE: u32 = BASE + 11;
    /// Swell: the filter opens with the envelope, 0..100 %.
    pub const SWELL: u32 = BASE + 12;
    pub const END: u32 = BASE + 13;
}

pub const CHORUS_MODES: &[&str] = &["Off", "I", "II", "I+II"];
pub const FM_ALGORITHMS: &[&str] = &[
    "Stack 6",
    "Two Stacks",
    "Three Pairs",
    "Stack 4 + Pair",
    "Twin Mods + Stack",
    "One Into Three",
    "Five Into One",
    "Stack 3 + Three",
    "Pair + Four",
    "All Carriers",
    "Stack 5 + Sine",
    "Two Pairs + Two",
];
pub const EP_MODELS: &[&str] = &["Tine", "Reed"];
pub const DRAWBARS: &[&str] = &["16'", "5 1/3'", "8'", "4'", "2 2/3'", "2'", "1 3/5'", "1 1/3'", "1'"];
pub const PERC_HARMS: &[&str] = &["Second", "Third"];
pub const PERC_DECAYS: &[&str] = &["Fast", "Slow"];
pub const PERC_VOLS: &[&str] = &["Soft", "Normal"];
pub const SPEEDS: &[&str] = &["Slow", "Fast"];
pub const VIBRATOS: &[&str] = &["Off", "V1", "V2", "V3", "C1", "C2", "C3"];
pub const COARSE_RATIOS: &[&str] = &["0.5", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16"];

const OP_NAMES: [[&str; 10]; 6] = {
    const fn row(n: usize) -> [&'static str; 10] {
        match n {
            0 => ["Op1 Coarse", "Op1 Fine", "Op1 Detune", "Op1 Level", "Op1 Attack", "Op1 Decay", "Op1 Sustain", "Op1 Release", "Op1 Velocity", "Op1 Key Scale"],
            1 => ["Op2 Coarse", "Op2 Fine", "Op2 Detune", "Op2 Level", "Op2 Attack", "Op2 Decay", "Op2 Sustain", "Op2 Release", "Op2 Velocity", "Op2 Key Scale"],
            2 => ["Op3 Coarse", "Op3 Fine", "Op3 Detune", "Op3 Level", "Op3 Attack", "Op3 Decay", "Op3 Sustain", "Op3 Release", "Op3 Velocity", "Op3 Key Scale"],
            3 => ["Op4 Coarse", "Op4 Fine", "Op4 Detune", "Op4 Level", "Op4 Attack", "Op4 Decay", "Op4 Sustain", "Op4 Release", "Op4 Velocity", "Op4 Key Scale"],
            4 => ["Op5 Coarse", "Op5 Fine", "Op5 Detune", "Op5 Level", "Op5 Attack", "Op5 Decay", "Op5 Sustain", "Op5 Release", "Op5 Velocity", "Op5 Key Scale"],
            _ => ["Op6 Coarse", "Op6 Fine", "Op6 Detune", "Op6 Level", "Op6 Attack", "Op6 Decay", "Op6 Sustain", "Op6 Release", "Op6 Velocity", "Op6 Key Scale"],
        }
    }
    [row(0), row(1), row(2), row(3), row(4), row(5)]
};

const OSC_NAMES: [[&str; 4]; 3] = [
    ["Osc1 Wave", "Osc1 Octave", "Osc1 Detune", "Osc1 Level"],
    ["Osc2 Wave", "Osc2 Octave", "Osc2 Detune", "Osc2 Level"],
    ["Osc3 Wave", "Osc3 Octave", "Osc3 Detune", "Osc3 Level"],
];

fn ms(id: u32, name: &'static str, min: f32, max: f32, default: f32) -> ParamInfo {
    ParamInfo::new(id, name, min, max, default).unit("ms").log()
}

fn pct(id: u32, name: &'static str, default: f32) -> ParamInfo {
    ParamInfo::new(id, name, 0.0, 100.0, default).unit("%")
}

fn hz(id: u32, name: &'static str, min: f32, max: f32, default: f32) -> ParamInfo {
    ParamInfo::new(id, name, min, max, default).unit("Hz").log()
}

fn choice(id: u32, name: &'static str, choices: &'static [&'static str], default: f32) -> ParamInfo {
    ParamInfo::new(id, name, 0.0, choices.len() as f32 - 1.0, default).choices(choices)
}

fn build() -> Vec<ParamInfo> {
    let mut v = Vec::with_capacity(1200);
    v.push(choice(ENGINE, "Engine", ENGINES, 0.0).short("Engine"));
    const MACRO_NAMES: [&str; 4] = ["Macro 1", "Macro 2", "Macro 3", "Macro 4"];
    const DEPTH_NAMES: [&str; 4] = ["Macro 1 Depth", "Macro 2 Depth", "Macro 3 Depth", "Macro 4 Depth"];
    for m in 0..4 {
        v.push(pct(macro_knob(m), MACRO_NAMES[m as usize], 0.0).short(["M1", "M2", "M3", "M4"][m as usize]));
    }
    v.push(ParamInfo::new(VOLUME, "Volume", -36.0, 12.0, 0.0).unit("dB").short("Vol"));
    v.push(ParamInfo::new(BEND_RANGE, "Bend Range", 0.0, 24.0, 2.0).unit("st").short("Bend"));
    v.push(ParamInfo::new(POLYPHONY, "Polyphony", 1.0, 16.0, 16.0).choices(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16"]).short("Voices"));
    v.push(pct(DRIVE, "Drive", 0.0));
    v.push(ParamInfo::new(EQ_LOW, "Low", -12.0, 12.0, 0.0).unit("dB"));
    v.push(ParamInfo::new(EQ_HIGH, "High", -12.0, 12.0, 0.0).unit("dB"));
    v.push(pct(DELAY_MIX, "Delay", 0.0));
    v.push(choice(DELAY_TIME, "Delay Time", DELAY_TIMES, 3.0).short("Time"));
    v.push(pct(DELAY_FEEDBACK, "Delay Feedback", 35.0).short("Fdbk"));
    v.push(pct(REVERB_MIX, "Reverb", 0.0));
    v.push(ParamInfo::new(REVERB_DECAY, "Reverb Decay", 0.3, 12.0, 2.5).unit("s").log().short("Decay"));
    for m in 0..4 {
        v.push(ParamInfo::new(macro_depth(m), DEPTH_NAMES[m as usize], -100.0, 100.0, 100.0).unit("%").short("Depth"));
    }

    // Ladder.
    {
        use ladder::*;
        for n in 0..3u32 {
            let names = OSC_NAMES[n as usize];
            v.push(choice(osc(n, WAVE), names[0], OSC_WAVES, 0.0));
            v.push(choice(osc(n, OCTAVE), names[1], OCTAVES, 2.0));
            v.push(ParamInfo::new(osc(n, DETUNE), names[2], -12.0, 12.0, [0.0, 0.08, -0.08][n as usize]).unit("st"));
            v.push(pct(osc(n, LEVEL), names[3], if n == 0 { 100.0 } else { 0.0 }));
        }
        v.push(pct(NOISE, "Noise", 0.0));
        v.push(hz(CUTOFF, "Cutoff", 20.0, 20_000.0, 1200.0));
        v.push(pct(RESONANCE, "Resonance", 20.0).short("Reso"));
        v.push(pct(ladder::DRIVE, "Filter Drive", 20.0).short("Drive"));
        v.push(ParamInfo::new(ENV_AMOUNT, "Filter Env", -100.0, 100.0, 40.0).unit("%").short("Env"));
        v.push(pct(KEY_TRACK, "Key Track", 50.0).short("Key"));
        v.push(ms(F_ATTACK, "Filter Attack", 0.5, 10_000.0, 2.0));
        v.push(ms(F_DECAY, "Filter Decay", 1.0, 10_000.0, 300.0));
        v.push(pct(F_SUSTAIN, "Filter Sustain", 30.0));
        v.push(ms(F_RELEASE, "Filter Release", 1.0, 10_000.0, 200.0));
        v.push(ms(A_ATTACK, "Attack", 0.5, 10_000.0, 2.0));
        v.push(ms(A_DECAY, "Decay", 1.0, 10_000.0, 300.0));
        v.push(pct(A_SUSTAIN, "Sustain", 80.0));
        v.push(ms(A_RELEASE, "Release", 1.0, 10_000.0, 150.0));
        v.push(ParamInfo::new(GLIDE, "Glide", 0.0, 2000.0, 0.0).unit("ms").power(2.0));
        v.push(choice(LEGATO, "Legato", OFF_ON, 1.0));
        v.push(hz(LFO_RATE, "LFO Rate", 0.05, 30.0, 5.0));
        v.push(ParamInfo::new(LFO_PITCH, "Vibrato", 0.0, 100.0, 0.0).unit("ct"));
        v.push(pct(LFO_CUTOFF, "LFO → Cutoff", 0.0));
        v.push(pct(VEL_CUTOFF, "Velocity → Cutoff", 30.0).short("Vel F"));
        v.push(pct(VEL_AMP, "Velocity → Amp", 30.0).short("Vel A"));
    }

    // Poly analog.
    {
        use poly::*;
        v.push(choice(A_WAVE, "Osc A Wave", &["Saw", "Pulse", "Triangle"], 0.0));
        v.push(ParamInfo::new(A_PW, "Osc A Width", 5.0, 95.0, 50.0).unit("%"));
        v.push(choice(B_WAVE, "Osc B Wave", &["Saw", "Pulse", "Triangle", "Sine"], 0.0));
        v.push(ParamInfo::new(B_SEMI, "Osc B Semi", -24.0, 24.0, 0.0).unit("st"));
        v.push(ParamInfo::new(B_FINE, "Osc B Fine", -50.0, 50.0, 7.0).unit("ct"));
        v.push(ParamInfo::new(B_PW, "Osc B Width", 5.0, 95.0, 50.0).unit("%"));
        v.push(pct(MIX, "Osc Mix", 50.0).short("Mix"));
        v.push(pct(poly::NOISE, "Noise", 0.0));
        v.push(choice(SYNC, "Sync", OFF_ON, 0.0));
        v.push(hz(poly::CUTOFF, "Cutoff", 20.0, 20_000.0, 3000.0));
        v.push(pct(poly::RESONANCE, "Resonance", 15.0).short("Reso"));
        v.push(ParamInfo::new(poly::ENV_AMOUNT, "Filter Env", -100.0, 100.0, 30.0).unit("%").short("Env"));
        v.push(pct(poly::KEY_TRACK, "Key Track", 50.0).short("Key"));
        v.push(ms(poly::F_ATTACK, "Filter Attack", 0.5, 10_000.0, 5.0));
        v.push(ms(poly::F_DECAY, "Filter Decay", 1.0, 10_000.0, 500.0));
        v.push(pct(poly::F_SUSTAIN, "Filter Sustain", 40.0));
        v.push(ms(poly::F_RELEASE, "Filter Release", 1.0, 10_000.0, 400.0));
        v.push(ms(poly::A_ATTACK, "Attack", 0.5, 10_000.0, 5.0));
        v.push(ms(poly::A_DECAY, "Decay", 1.0, 10_000.0, 500.0));
        v.push(pct(poly::A_SUSTAIN, "Sustain", 80.0));
        v.push(ms(poly::A_RELEASE, "Release", 1.0, 10_000.0, 400.0));
        v.push(pct(PM_ENV, "Poly-Mod Env", 0.0).short("PM Env"));
        v.push(pct(PM_OSC_B, "Poly-Mod Osc B", 0.0).short("PM B"));
        v.push(choice(PM_FREQ, "Poly-Mod → Freq A", OFF_ON, 1.0).short("→ Freq"));
        v.push(choice(PM_PW, "Poly-Mod → Width A", OFF_ON, 0.0).short("→ PW"));
        v.push(choice(PM_FILTER, "Poly-Mod → Filter", OFF_ON, 0.0).short("→ Filt"));
        v.push(hz(poly::LFO_RATE, "LFO Rate", 0.05, 30.0, 4.0));
        v.push(ParamInfo::new(poly::LFO_PITCH, "Vibrato", 0.0, 100.0, 0.0).unit("ct"));
        v.push(pct(LFO_PW, "LFO → Width", 0.0));
        v.push(pct(poly::LFO_CUTOFF, "LFO → Cutoff", 0.0));
        v.push(pct(DRIFT, "Drift", 25.0));
        v.push(choice(CHORUS, "Chorus", CHORUS_MODES, 0.0));
        v.push(ParamInfo::new(poly::GLIDE, "Glide", 0.0, 2000.0, 0.0).unit("ms").power(2.0));
        v.push(pct(poly::VEL_AMP, "Velocity → Amp", 40.0).short("Vel A"));
        v.push(pct(VEL_FILTER, "Velocity → Filter", 20.0).short("Vel F"));
        v.push(pct(SPREAD, "Spread", 40.0));
    }

    // FM.
    {
        use fm::*;
        v.push(choice(ALGORITHM, "Algorithm", FM_ALGORITHMS, 0.0).short("Algo"));
        v.push(pct(FEEDBACK, "Feedback", 0.0).short("Fdbk"));
        v.push(ParamInfo::new(BRIGHTNESS, "Brightness", 0.0, 200.0, 100.0).unit("%").short("Bright"));
        v.push(ParamInfo::new(TIME, "Env Time", 25.0, 400.0, 100.0).unit("%").log().short("Time"));
        v.push(hz(fm::LFO_RATE, "LFO Rate", 0.05, 30.0, 5.0));
        v.push(ParamInfo::new(fm::LFO_PITCH, "Vibrato", 0.0, 100.0, 0.0).unit("ct"));
        v.push(pct(LFO_AMP, "Tremolo", 0.0));
        v.push(ParamInfo::new(PITCH_ENV, "Pitch Env", -24.0, 24.0, 0.0).unit("st"));
        v.push(ms(PITCH_DECAY, "Pitch Decay", 1.0, 2000.0, 50.0));
        for n in 0..6u32 {
            let names = OP_NAMES[n as usize];
            // Default patch: op 1 a sine carrier, op 2 a gentle modulator.
            v.push(choice(op(n, COARSE), names[0], COARSE_RATIOS, 1.0).short("Ratio"));
            v.push(pct(op(n, FINE), names[1], 0.0).short("Fine"));
            v.push(ParamInfo::new(op(n, DETUNE), names[2], -10.0, 10.0, 0.0).unit("ct").short("Det"));
            v.push(ParamInfo::new(op(n, LEVEL), names[3], 0.0, 99.0, [99.0, 70.0, 0.0, 0.0, 0.0, 0.0][n as usize]).short("Level"));
            v.push(ms(op(n, ATTACK), names[4], 0.5, 10_000.0, 1.0).short("A"));
            v.push(ms(op(n, DECAY), names[5], 1.0, 20_000.0, 1500.0).short("D"));
            v.push(pct(op(n, SUSTAIN), names[6], 50.0).short("S"));
            v.push(ms(op(n, RELEASE), names[7], 1.0, 10_000.0, 300.0).short("R"));
            v.push(pct(op(n, VEL), names[8], 50.0).short("Vel"));
            v.push(pct(op(n, KEY_SCALE), names[9], 0.0).short("Key"));
        }
    }

    // Electric piano.
    {
        use ep::*;
        v.push(choice(MODEL, "Model", EP_MODELS, 0.0));
        v.push(pct(HARDNESS, "Hardness", 45.0));
        v.push(pct(BELL, "Bell", 40.0));
        v.push(pct(BARK, "Bark", 35.0));
        v.push(pct(PICKUP, "Pickup", 35.0));
        v.push(ParamInfo::new(ep::DECAY, "Decay", 10.0, 400.0, 100.0).unit("%").log());
        v.push(ms(ep::RELEASE, "Release", 5.0, 3000.0, 120.0));
        v.push(pct(ep::VEL, "Velocity", 70.0));
        v.push(hz(TREM_RATE, "Tremolo Rate", 0.1, 12.0, 4.5).short("Rate"));
        v.push(pct(TREM_DEPTH, "Tremolo", 0.0));
        v.push(choice(AUTOPAN, "Auto-Pan", OFF_ON, 1.0).short("Pan"));
        v.push(hz(ep::TONE, "Tone", 500.0, 20_000.0, 9000.0));
        v.push(pct(ep::NOISE, "Thump", 25.0));
        v.push(ParamInfo::new(ep::DETUNE, "Detune", 0.0, 20.0, 2.0).unit("ct"));
    }

    // Organ.
    {
        use organ::*;
        const DEF: [f32; 9] = [8.0, 8.0, 8.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        const NAMES: [&str; 9] = ["Drawbar 16'", "Drawbar 5 1/3'", "Drawbar 8'", "Drawbar 4'", "Drawbar 2 2/3'", "Drawbar 2'", "Drawbar 1 3/5'", "Drawbar 1 1/3'", "Drawbar 1'"];
        for n in 0..9u32 {
            v.push(ParamInfo::new(drawbar(n), NAMES[n as usize], 0.0, 8.0, DEF[n as usize]).choices(&["0", "1", "2", "3", "4", "5", "6", "7", "8"]).short(DRAWBARS[n as usize]));
        }
        v.push(choice(PERC, "Percussion", OFF_ON, 0.0).short("Perc"));
        v.push(choice(PERC_HARM, "Perc Harmonic", PERC_HARMS, 1.0).short("Harm"));
        v.push(choice(PERC_DECAY, "Perc Decay", PERC_DECAYS, 0.0).short("Decay"));
        v.push(choice(PERC_VOL, "Perc Volume", PERC_VOLS, 1.0).short("Vol"));
        v.push(pct(CLICK, "Key Click", 35.0).short("Click"));
        v.push(pct(ROTARY, "Rotary", 70.0));
        v.push(choice(SPEED, "Rotor Speed", SPEEDS, 0.0).short("Speed"));
        v.push(ParamInfo::new(RAMP, "Rotor Ramp", 10.0, 300.0, 100.0).unit("%").log().short("Ramp"));
        v.push(pct(organ::DRIVE, "Overdrive", 15.0).short("Drive"));
        v.push(choice(VIBRATO, "Vibrato", VIBRATOS, 0.0).short("Vib"));
        v.push(hz(organ::TONE, "Tone", 1000.0, 20_000.0, 8000.0));
        v.push(ms(organ::RELEASE, "Release", 2.0, 2000.0, 15.0));
    }

    // Strings.
    {
        use strings::*;
        v.push(pct(CELLO, "Cello 16'", 40.0).short("16'"));
        v.push(pct(VIOLIN, "Violin 8'", 100.0).short("8'"));
        v.push(pct(VIOLA, "Viola 4'", 50.0).short("4'"));
        v.push(pct(HORN, "Horn 8'", 0.0).short("Horn"));
        v.push(ms(strings::ATTACK, "Attack", 1.0, 10_000.0, 300.0));
        v.push(ms(strings::RELEASE, "Release", 5.0, 10_000.0, 900.0));
        v.push(hz(strings::TONE, "Tone", 200.0, 20_000.0, 5500.0));
        v.push(pct(strings::RESONANCE, "Resonance", 10.0).short("Reso"));
        v.push(hz(HIGH_PASS, "High Pass", 20.0, 2000.0, 120.0).short("HP"));
        v.push(pct(ENSEMBLE, "Ensemble", 80.0).short("Ens"));
        v.push(pct(ENS_DEPTH, "Ensemble Depth", 60.0).short("Depth"));
        v.push(hz(ENS_RATE, "Ensemble Rate", 0.1, 8.0, 0.6).short("Rate"));
        v.push(pct(SWELL, "Swell", 20.0));
    }

    // Hosted wavetable synth.
    for info in wavetable::params() {
        let mut i = info.clone();
        i.id = wt(info.id);
        v.push(i);
    }
    v
}

/// Every parameter (built once).
pub fn all() -> &'static [ParamInfo] {
    static ALL: OnceLock<Vec<ParamInfo>> = OnceLock::new();
    ALL.get_or_init(build)
}

/// The engine whose block holds parameter `id` (None for globals).
pub fn engine_of(id: u32) -> Option<usize> {
    match id {
        _ if id >= WT_BASE => Some(engine::WAVETABLE),
        100..=199 => Some(engine::LADDER),
        200..=299 => Some(engine::POLY),
        300..=399 => Some(engine::FM),
        400..=499 => Some(engine::EP),
        500..=599 => Some(engine::ORGAN),
        600..=699 => Some(engine::STRINGS),
        _ => None,
    }
}

/// Parameter values: the base values set by the host / presets and the
/// effective values after the macros (what the engines read).
pub struct Params {
    infos: Vec<Option<ParamInfo>>,
    base: Vec<f32>,
    eff: Vec<f32>,
    dirty: bool,
}

impl Params {
    pub fn new() -> Self {
        let mut infos = vec![None; PARAM_SPACE];
        let mut base = vec![0.0; PARAM_SPACE];
        for p in all() {
            base[p.id as usize] = p.default_normalized();
            infos[p.id as usize] = Some(p.clone());
        }
        let eff = base.clone();
        Self { infos, base, eff, dirty: true }
    }

    #[inline]
    pub fn set(&mut self, id: u32, n: f32) {
        if !n.is_finite() {
            return;
        }
        if let Some(Some(_)) = self.infos.get(id as usize) {
            let n = n.clamp(0.0, 1.0);
            let v = &mut self.base[id as usize];
            if *v != n {
                *v = n;
                self.dirty = true;
            }
        }
    }

    #[inline]
    pub fn base(&self, id: u32) -> f32 {
        self.base.get(id as usize).copied().unwrap_or(0.0)
    }

    /// Effective normalised value.
    #[inline]
    pub fn norm(&self, id: u32) -> f32 {
        self.eff.get(id as usize).copied().unwrap_or(0.0)
    }

    /// Effective plain value.
    #[inline]
    pub fn plain(&self, id: u32) -> f32 {
        match self.infos.get(id as usize) {
            Some(Some(i)) => i.to_plain(self.eff[id as usize]),
            _ => 0.0,
        }
    }

    #[inline]
    pub fn choice(&self, id: u32) -> usize {
        match self.infos.get(id as usize) {
            Some(Some(i)) => (i.to_plain(self.eff[id as usize]) - i.min).round().max(0.0) as usize,
            _ => 0,
        }
    }

    #[inline]
    pub fn on(&self, id: u32) -> bool {
        self.plain(id) >= 0.5
    }

    /// Plain base (pre-macro) value.
    pub fn base_plain(&self, id: u32) -> f32 {
        match self.infos.get(id as usize) {
            Some(Some(i)) => i.to_plain(self.base[id as usize]),
            _ => 0.0,
        }
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Copy the base values to the effective ones for the ids in `range`.
    pub(crate) fn reset_eff(&mut self, range: std::ops::Range<usize>) {
        let end = range.end.min(self.base.len());
        let start = range.start.min(end);
        self.eff[start..end].copy_from_slice(&self.base[start..end]);
    }

    pub(crate) fn add_eff(&mut self, id: u32, delta: f32) {
        if let Some(v) = self.eff.get_mut(id as usize) {
            *v = (*v + delta).clamp(0.0, 1.0);
        }
    }

    pub fn infos(&self) -> impl Iterator<Item = &ParamInfo> {
        self.infos.iter().flatten()
    }

    pub fn info(&self, id: u32) -> Option<&ParamInfo> {
        self.infos.get(id as usize).and_then(Option::as_ref)
    }
}

impl Default for Params {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_inside_their_blocks() {
        let mut ids: Vec<u32> = all().iter().map(|p| p.id).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n);
        const { assert!(ladder::END <= 200 && poly::END <= 300 && fm::END <= 400 && ep::END <= 500 && organ::END <= 600 && strings::END <= 700) };
        for p in all() {
            assert!((p.id as usize) < PARAM_SPACE);
            if (100..700).contains(&p.id) {
                assert!(engine_of(p.id).is_some());
            }
        }
        assert_eq!(ENGINES.len(), ENGINE_COUNT);
        assert_eq!(ENGINE_TAGS.len(), ENGINE_COUNT);
    }
}
