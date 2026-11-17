//! Parameter ids and descriptions of `instrument.granular`.
//!
//! Ids are stable (presets and projects store normalised values by id):
//! append new ones, never renumber, and keep stepped ranges as they are.

use super::builtin;
use super::window;
use crate::midi::theory::ROOT_NAMES;
use aitrack_dsp::ParamInfo;

pub mod p {
    // Source
    pub const SOURCE: u32 = 0;
    pub const BUILTIN: u32 = 1;
    pub const FREEZE: u32 = 2;
    pub const LIVE_LEN: u32 = 3;
    // Position
    pub const POSITION: u32 = 4;
    pub const SCAN_SPEED: u32 = 5;
    pub const SCAN_RATE: u32 = 6;
    pub const SCAN_DEPTH: u32 = 7;
    pub const SPRAY: u32 = 8;
    // Grain stream
    pub const SIZE: u32 = 9;
    pub const SIZE_RAND: u32 = 10;
    pub const DENSITY: u32 = 11;
    pub const SYNC: u32 = 12;
    pub const SYNC_MULT: u32 = 13;
    pub const JITTER: u32 = 14;
    pub const WINDOW: u32 = 15;
    pub const WIN_SHAPE: u32 = 16;
    pub const DIRECTION: u32 = 17;
    // Pitch
    pub const PITCH: u32 = 18;
    pub const PITCH_SPREAD: u32 = 19;
    pub const QUANT: u32 = 20;
    pub const SCALE: u32 = 21;
    pub const ROOT: u32 = 22;
    pub const ROOT_KEY: u32 = 23;
    pub const KEY_TRACK: u32 = 24;
    pub const PAN_SPREAD: u32 = 25;
    pub const SEED: u32 = 26;
    // Amp envelope
    pub const ATTACK: u32 = 27;
    pub const DECAY: u32 = 28;
    pub const SUSTAIN: u32 = 29;
    pub const RELEASE: u32 = 30;
    pub const VEL_SENS: u32 = 31;
    // Filter
    pub const FILTER_TYPE: u32 = 32;
    pub const CUTOFF: u32 = 33;
    pub const RESONANCE: u32 = 34;
    pub const FILTER_ENV: u32 = 35;
    pub const FILTER_KEY: u32 = 36;
    // Output
    pub const VOLUME: u32 = 37;
    pub const WIDTH: u32 = 38;
    pub const POLYPHONY: u32 = 39;
    pub const BEND_RANGE: u32 = 40;
}

pub const SOURCES: &[&str] = &["Sample", "Live", "Built-in"];
pub const DIRECTIONS: &[&str] = &["Forward", "Reverse", "Alternate", "Random"];
pub const FILTER_TYPES: &[&str] = &["Off", "Low-pass", "High-pass", "Band-pass"];
pub const OFF_ON: &[&str] = &["Off", "On"];
/// `Off`; `Scale` snaps the final grain pitch to the scale; `Interval` snaps
/// only the offset from the played key (so chords stay chords: octaves,
/// fifths, triads…).
pub const QUANTS: &[&str] = &["Off", "Scale", "Interval"];
/// The 13 scales of `aitrack_model::ScaleKind` (same order), then interval
/// sets that are mostly for `Interval` mode.
pub const SCALES: &[&str] = &[
    "Major",
    "Minor",
    "Harmonic Minor",
    "Melodic Minor",
    "Dorian",
    "Phrygian",
    "Lydian",
    "Mixolydian",
    "Locrian",
    "Major Pentatonic",
    "Minor Pentatonic",
    "Blues",
    "Chromatic",
    "Octaves",
    "Fifths",
    "Whole Tone",
    "Major Triad",
    "Minor Triad",
    "Minor 7th",
];

/// 12-bit pitch-class mask (bit 0 = C) of scale `kind` on `root`.
pub fn scale_mask_ext(kind: usize, root: usize) -> u16 {
    let set = |iv: &[usize]| iv.iter().fold(0u16, |m, i| m | 1 << ((root + i) % 12));
    match kind {
        0..=12 => crate::midi::theory::scale_mask(kind, root),
        13 => set(&[0]),
        14 => set(&[0, 7]),
        15 => set(&[0, 2, 4, 6, 8, 10]),
        16 => set(&[0, 4, 7]),
        17 => set(&[0, 3, 7]),
        _ => set(&[0, 3, 7, 10]),
    }
}
/// `Free`, then `aitrack_dsp::lfo::SYNC_DIVISIONS` in order (index `i` ≥ 1 is
/// division `i - 1`).
pub const SYNCS: &[&str] = &[
    "Free", "8 bars", "4 bars", "2 bars", "1 bar", "1/2", "1/2.", "1/2T", "1/4", "1/4.", "1/4T", "1/8", "1/8.", "1/8T", "1/16", "1/16.", "1/16T", "1/32", "1/32T",
];

pub const SOURCE_SAMPLE: usize = 0;
pub const SOURCE_LIVE: usize = 1;
pub const SOURCE_BUILTIN: usize = 2;

pub fn params() -> Vec<ParamInfo> {
    use p::*;
    let t = |id, name, d| ParamInfo::new(id, name, 0.5, 10_000.0, d).unit("ms").log();
    vec![
        ParamInfo::new(SOURCE, "Source", 0.0, 2.0, 2.0).choices(SOURCES),
        ParamInfo::new(BUILTIN, "Built-in", 0.0, (builtin::COUNT - 1) as f32, 0.0).choices(builtin::NAMES),
        ParamInfo::new(FREEZE, "Freeze", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(LIVE_LEN, "Live Length", 0.25, 8.0, 4.0).unit("s").log().short("Live"),
        ParamInfo::new(POSITION, "Position", 0.0, 100.0, 30.0).unit("%").short("Pos"),
        ParamInfo::new(SCAN_SPEED, "Scan", -200.0, 200.0, 0.0).unit("%/s"),
        ParamInfo::new(SCAN_RATE, "Scan LFO Rate", 0.02, 20.0, 0.3).unit("Hz").log().short("LFO Rate"),
        ParamInfo::new(SCAN_DEPTH, "Scan LFO Depth", 0.0, 100.0, 0.0).unit("%").short("LFO Depth"),
        ParamInfo::new(SPRAY, "Spray", 0.0, 100.0, 15.0).unit("%"),
        ParamInfo::new(SIZE, "Size", 2.0, 1000.0, 100.0).unit("ms").log(),
        ParamInfo::new(SIZE_RAND, "Size Random", 0.0, 100.0, 0.0).unit("%").short("Size Rnd"),
        ParamInfo::new(DENSITY, "Density", 0.5, 400.0, 20.0).unit("Hz").log().short("Dens"),
        ParamInfo::new(SYNC, "Sync", 0.0, (SYNCS.len() - 1) as f32, 0.0).choices(SYNCS),
        ParamInfo::new(SYNC_MULT, "Grains per Step", 1.0, 8.0, 1.0).choices(&[]).short("Per Step"),
        ParamInfo::new(JITTER, "Timing Jitter", 0.0, 100.0, 25.0).unit("%").short("Jitter"),
        ParamInfo::new(WINDOW, "Window", 0.0, (window::COUNT - 1) as f32, 0.0).choices(window::NAMES),
        ParamInfo::new(WIN_SHAPE, "Window Shape", 0.0, 100.0, 50.0).unit("%").short("Shape"),
        ParamInfo::new(DIRECTION, "Direction", 0.0, 3.0, 0.0).choices(DIRECTIONS).short("Dir"),
        ParamInfo::new(PITCH, "Pitch", -24.0, 24.0, 0.0).unit("st"),
        ParamInfo::new(PITCH_SPREAD, "Pitch Spread", 0.0, 24.0, 0.0).unit("st").short("Spread"),
        ParamInfo::new(QUANT, "Quantize", 0.0, 2.0, 0.0).choices(QUANTS).short("Quant"),
        ParamInfo::new(SCALE, "Scale", 0.0, (SCALES.len() - 1) as f32, 1.0).choices(SCALES),
        ParamInfo::new(ROOT, "Scale Root", 0.0, 11.0, 0.0).choices(ROOT_NAMES).short("Root"),
        ParamInfo::new(ROOT_KEY, "Root Key", 0.0, 127.0, 60.0).choices(&[]).short("Key"),
        ParamInfo::new(KEY_TRACK, "Key Tracking", 0.0, 100.0, 100.0).unit("%").short("Track"),
        ParamInfo::new(PAN_SPREAD, "Pan Spread", 0.0, 100.0, 50.0).unit("%").short("Pan"),
        ParamInfo::new(SEED, "Seed", 0.0, 255.0, 0.0).choices(&[]),
        t(ATTACK, "Attack", 5.0).short("A"),
        t(DECAY, "Decay", 300.0).short("D"),
        ParamInfo::new(SUSTAIN, "Sustain", 0.0, 100.0, 100.0).unit("%").short("S"),
        t(RELEASE, "Release", 300.0).short("R"),
        ParamInfo::new(VEL_SENS, "Velocity Sensitivity", 0.0, 100.0, 50.0).unit("%").short("Vel"),
        ParamInfo::new(FILTER_TYPE, "Filter Type", 0.0, 3.0, 0.0).choices(FILTER_TYPES).short("Filter"),
        ParamInfo::new(CUTOFF, "Cutoff", 20.0, 20_000.0, 20_000.0).unit("Hz").log(),
        ParamInfo::new(RESONANCE, "Resonance", 0.0, 100.0, 0.0).unit("%").short("Res"),
        ParamInfo::new(FILTER_ENV, "Filter Env Amount", -100.0, 100.0, 0.0).unit("%").short("Env"),
        ParamInfo::new(FILTER_KEY, "Filter Key Tracking", 0.0, 100.0, 0.0).unit("%").short("F.Key"),
        ParamInfo::new(VOLUME, "Volume", -48.0, 6.0, -3.0).unit("dB"),
        ParamInfo::new(WIDTH, "Width", 0.0, 200.0, 100.0).unit("%"),
        ParamInfo::new(POLYPHONY, "Polyphony", 1.0, 8.0, 4.0).choices(&[]).short("Poly"),
        ParamInfo::new(BEND_RANGE, "Pitch Bend Range", 0.0, 24.0, 2.0).choices(&[]).unit("st").short("Bend"),
    ]
}
