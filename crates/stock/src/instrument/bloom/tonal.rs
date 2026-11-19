//! Growing tonal scenes: sixteen Wavetable Synth patches of one [`Kind`]
//! from a seed. Pure, deterministic and allocation-free once the priors
//! are built (the device grows on the audio thread when a Seed or Kind
//! automation asks for it).
//!
//! ```text
//! factory presets of the kind ─► priors (parents, value ranges, blocks)
//! seed ─► scene character (lineage, wave family, brightness, width, grit,
//!         movement, time scale, a shared room: reverb / delay / chorus)
//!      └► per cell: two parents crossed by module blocks (oscillators,
//!         filter + mod env, amp, motion = LFOs + matrix + env 3, voicing)
//!         + the kind's design moves + a small mutation + the kind's rules
//!         = the core; + a growth path and a distance = the sound
//! ```
//!
//! **Priors** ([`priors`]): per kind, the hand-made `instrument.wavetable`
//! factory presets listed in [`PARENTS`] (pinned by name: a change of that
//! list or of those presets changes what every tonal seed grows), and for
//! each parameter the range they span. Built once, off the audio thread.
//!
//! **Rules** ([`rules`]): per kind, plain-value bands every patch is held in
//! (a bass is mono with a fast attack, a low-pass under 2.6 kHz and a sub; a
//! pluck has no sustain and a filter envelope; a pad attacks slowly, is
//! wide and moves…), plus cross-parameter fixes in [`constrain`] (enough
//! source level, capped pitch and amp modulation, a movement route where
//! the kind needs one). Applied to every grown sound, at any distance.
//!
//! **Growth paths** ([`grow`]): like the drum scene, a fixed random
//! direction over the explorable parameters ([`genes`]) and distances at
//! which the wavetable, the filter model and LFO 1's shape switch; the sound
//! at distance `d` is the core moved `d` along it, folded into the kind's
//! band, then held to the rules.
//!
//! Only arithmetic, `sqrt` and rounding are used (see `genome::det_log2`).

use super::genome::{SOUNDS, det_exp2, det_log2, quantize, reflect};
use super::patch::{Kind, PATCH_LEN, Patch, PatchScene, det_norm, det_plain, layout};
use super::rng::{Rng, part};
use crate::instrument::wavetable::{self as wt, WavetableSynth, p as w, src as msrc, vdst};
use aitrack_dsp::{AudioBuffer, Event, EventKind, ProcessContext, Processor, Transport};
use std::sync::OnceLock;

/// Packs: one scene per kind from one producer's palette (`pack.rs`).
pub use super::pack::{Pack, PackCharacter, pack};

// ── Priors ──────────────────────────────────────────────────────────────────

/// The factory presets each kind learns from (`instrument.wavetable/…`).
pub const PARENTS: [&[&str]; 8] = [
    &[],
    &[
        "Bass/Sub Bass",
        "Bass/808 Boom",
        "Bass/808 Glide",
        "Bass/Reggaeton Bass",
        "Bass/Dembow Punch",
        "Bass/Reese",
        "Bass/Wobble 8th",
        "Bass/Riddim Triplet",
        "Bass/Growl",
        "Bass/House Organ Bass",
        "Bass/Acid Line",
        "Bass/Techno Rumble",
        "Bass/Pluck Bass",
        "Bass/Synthwave Bass",
        "Sound Design/Fold Bass",
        "Sound Design/Ladder Reese",
        "Sound Design/Talking Growl",
    ],
    &[
        "Lead/Supersaw Lead",
        "Lead/Big Room Lead",
        "Lead/Future Bass Lead",
        "Lead/Pluck Lead",
        "Lead/Square Lead",
        "Lead/Saw Lead Mono",
        "Lead/Dubstep Screech",
        "Lead/Trap Bell",
        "Lead/Reggaeton Lead",
        "Lead/Lo-Fi Lead",
        "Lead/Detuned Lead",
        "Lead/Portamento Lead",
        "Sound Design/Sync Sweep Lead",
        "Sound Design/Bit Glitch Lead",
        "Sound Design/Vox Lead",
    ],
    &[
        "Pad/Warm Pad",
        "Pad/Supersaw Pad",
        "Pad/Dark Pad",
        "Pad/Glass Pad",
        "Pad/Evolving Pad",
        "Pad/String Pad",
        "Pad/Lo-Fi Pad",
        "Pad/Future Bass Pump Pad",
        "Pad/Choir Pad",
        "Sound Design/Harmonic Bloom Pad",
    ],
    &[
        "Pluck/House Pluck",
        "Pluck/Tropical Pluck",
        "Pluck/Future Bass Pluck",
        "Pluck/Marimba",
        "Pluck/Bell Pluck",
        "Pluck/Trance Pluck",
        "Pluck/Lo-Fi Pluck",
        "Pluck/Dembow Pluck",
        "Arp/Trance Arp",
        "Arp/House Arp",
        "Arp/Bubble Arp",
        "Sound Design/Comb Metal Pluck",
    ],
    &[
        "Keys/Electric Piano",
        "Keys/Organ",
        "Keys/House Piano Stab",
        "Keys/Synth Keys",
        "Keys/Lo-Fi Keys",
        "Keys/Clav",
        "Keys/Bright Bell Keys",
        "Chords/House Stab",
        "Chords/Lo-Fi Chords",
        "Chords/Pop Chords",
        "Chords/Reggaeton Chords",
        "Sound Design/PD Keys",
        "Sound Design/Asym Comb Keys",
        "Sound Design/FM Glass Bell",
    ],
    &["FX/White Noise Riser", "FX/Downlifter", "FX/Laser Zap", "FX/Dub Siren", "FX/Impact Boom", "FX/Wind Atmosphere"],
    &[
        "Pad/Evolving Pad",
        "Pad/Dark Pad",
        "Pad/Glass Pad",
        "Pad/Choir Pad",
        "Pad/Lo-Fi Pad",
        "FX/Wind Atmosphere",
        "Sound Design/Spectral Mist",
        "Sound Design/Harmonic Bloom Pad",
    ],
];

/// Module blocks: what crosses over together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    /// Oscillators 1–3, sub, noise, drift.
    Osc,
    /// Filters 1–2, routing, the mod envelope (env 2, which drives the
    /// filter envelopes).
    Filter,
    /// Amp envelope and velocity.
    Amp,
    /// LFOs, both matrices, env 3, macros.
    Motion,
    /// Voice mode, glide, bend, polyphony.
    Voice,
    /// Distortion, EQ, chorus, delay, reverb, volume.
    Fx,
}

/// The block of a synth parameter id.
pub fn block_of(id: u32) -> Block {
    match id {
        0..=23 | 100..=169 | 170..=174 | 620 => Block::Osc,
        24..=30 | 35..=38 | 180..=181 | 190..=200 | 224..=227 => Block::Filter,
        31..=34 | 75 | 214..=217 => Block::Amp,
        73 | 74 | 76 | 77 => Block::Voice,
        78..=84 | 600..=619 => Block::Fx,
        _ => Block::Motion,
    }
}

/// One explorable parameter: growth paths and mutations move it.
#[derive(Clone, Copy, Debug)]
pub struct Gene {
    pub id: u32,
    pub slot: usize,
    /// How far one unit of distance moves it (normalised units).
    pub sens: f32,
    /// A matrix amount (moves only when the slot is routed; pitch and amp
    /// routes move ten times less).
    pub route: bool,
    /// Banded by the parents' range when no rule bands it.
    pub prior: bool,
}

fn gene_list() -> Vec<Gene> {
    let mut v = Vec::with_capacity(128);
    let mut add = |id: u32, sens: f32, route: bool, prior: bool| {
        if let Some(slot) = Patch::slot(id) {
            v.push(Gene { id, slot, sens, route, prior });
        }
    };
    for o in 0..3 {
        add(w::osc(o, w::POS), 0.30, false, false);
        add(w::osc(o, w::LEVEL), 0.08, false, false);
        add(w::osc(o, w::DETUNE), 0.15, false, false);
        add(w::osc(o, w::SPREAD), 0.15, false, false);
        add(w::osc(o, w::FINE), 0.02, false, true);
        add(w::osc(o, w::WARP_AMT), 0.15, false, false);
        add(w::osc(o, w::BLEND), 0.10, false, false);
    }
    add(w::SUB_LEVEL, 0.10, false, true);
    add(w::NOISE, 0.05, false, true);
    add(w::DRIFT, 0.10, false, true);
    add(w::CUTOFF, 0.09, false, true);
    add(w::RESONANCE, 0.12, false, true);
    add(w::DRIVE, 0.12, false, true);
    add(w::KEY_TRACK, 0.08, false, true);
    add(w::FILTER_ENV, 0.07, false, true);
    add(w::filt(1, w::F_CUTOFF), 0.09, false, true);
    add(w::filt(1, w::F_RESO), 0.10, false, true);
    add(w::filt(1, w::F_ENV), 0.06, false, true);
    for e in 0..3 {
        add(w::env(e, w::E_ATTACK), 0.05, false, e != 2);
        add(w::env(e, w::E_DECAY), 0.06, false, e != 2);
        add(w::env(e, w::E_SUSTAIN), 0.10, false, e != 2);
        add(w::env(e, w::E_RELEASE), 0.05, false, e != 2);
    }
    for l in 0..4 {
        add(w::lfo(l, w::LFO_RATE), 0.05, false, false);
    }
    for s in 0..8 {
        add(w::slot(s, w::MOD_AMT), 0.04, true, false);
    }
    for s in 0..32 {
        add(w::slot_x(s, w::M_AMT), 0.04, true, false);
    }
    add(w::GLIDE, 0.06, false, true);
    add(w::VEL_SENS, 0.05, false, true);
    add(w::CHORUS_MIX, 0.08, false, true);
    add(w::DELAY_MIX, 0.06, false, true);
    add(w::DELAY_FEEDBACK, 0.06, false, true);
    add(w::REVERB_MIX, 0.06, false, true);
    add(w::REVERB_DECAY, 0.05, false, true);
    add(w::DIST_DRIVE, 0.08, false, true);
    add(w::EQ_LOW, 0.04, false, true);
    add(w::EQ_MID, 0.04, false, true);
    add(w::EQ_HIGH, 0.04, false, true);
    v
}

/// The explorable parameters, in the fixed order paths draw them.
pub fn genes() -> &'static [Gene] {
    static G: OnceLock<Vec<Gene>> = OnceLock::new();
    G.get_or_init(gene_list)
}

/// The waveform family of a kind (`WAVE` values of the factory bank:
/// 1 Basic Shapes, 2 Harmonic Sweep, 3 Formant Vowels, 4 Sync Sweep, 5 FM
/// Bell, 6 Organ Drawbars, 7 Digital Steps, 8 Sine Fold, 9 Phase
/// Distortion, 10 Spectral Comb, 11 Growl, 12 Noise Spectra, 13 PWM Saw,
/// 14 Bit Glass).
pub fn waves(kind: Kind) -> &'static [usize] {
    match kind {
        Kind::Bass => &[1, 2, 8, 9, 11, 13],
        Kind::Lead => &[1, 2, 3, 4, 7, 9, 13],
        Kind::Pad => &[2, 3, 5, 6, 10, 13, 14],
        Kind::Pluck => &[1, 5, 6, 9, 10, 14],
        Kind::Keys => &[1, 2, 5, 6, 9, 14],
        Kind::Fx => &[4, 7, 8, 10, 11, 12],
        Kind::Texture => &[2, 3, 5, 10, 12, 14],
        Kind::Drums => &[1],
    }
}

/// Filter models a growth path may switch to (`filter::MODELS`).
fn models(kind: Kind) -> &'static [usize] {
    match kind {
        Kind::Bass | Kind::Lead | Kind::Pluck => &[0, 1, 6, 7, 9],
        Kind::Keys => &[0, 1, 7, 8],
        Kind::Pad => &[0, 1, 5, 8],
        Kind::Fx => &[0, 1, 2, 3, 5, 9],
        Kind::Texture => &[0, 2, 3, 5, 8],
        Kind::Drums => &[0],
    }
}

/// Plain-value bands per kind: `(id, lo, hi)`.
pub fn rules(kind: Kind) -> Vec<(u32, f32, f32)> {
    let mut r: Vec<(u32, f32, f32)> = Vec::with_capacity(64);
    let oscs = |r: &mut Vec<(u32, f32, f32)>, f: u32, lo: f32, hi: f32| {
        for o in 0..3 {
            r.push((w::osc(o, f), lo, hi));
        }
    };
    let lfo_rates = |r: &mut Vec<(u32, f32, f32)>, lo: f32, hi: f32| {
        for l in 0..4 {
            r.push((w::lfo(l, w::LFO_RATE), lo, hi));
        }
    };
    r.push((w::VOLUME, -6.0, -6.0));
    r.push((w::BEND_RANGE, 2.0, 2.0));
    r.push((w::filt(0, w::F_ENABLE), 1.0, 1.0));
    match kind {
        Kind::Bass => {
            r.extend([(w::VOICE_MODE, 1.0, 2.0), (w::GLIDE, 0.0, 90.0), (w::osc(0, w::OCTAVE), -1.0, 0.0), (w::osc(1, w::OCTAVE), -1.0, 1.0), (w::osc(2, w::OCTAVE), -2.0, 1.0)]);
            r.extend([(w::SUB_LEVEL, 24.0, 60.0), (w::SUB_OCTAVE, 0.0, 0.0), (w::NOISE, 0.0, 5.0), (w::FILTER_TYPE, 0.0, 0.0), (w::filt(1, w::F_TYPE), 0.0, 0.0)]);
            r.extend([(w::CUTOFF, 140.0, 2600.0), (w::RESONANCE, 0.0, 62.0), (w::DRIVE, 0.0, 70.0), (w::FILTER_ENV, -10.0, 65.0), (w::KEY_TRACK, 0.0, 60.0)]);
            r.extend([(w::AMP_ATTACK, 0.5, 6.0), (w::AMP_DECAY, 90.0, 2500.0), (w::AMP_RELEASE, 15.0, 240.0), (w::MOD_ATTACK, 0.5, 20.0), (w::MOD_DECAY, 40.0, 1500.0), (w::MOD_RELEASE, 20.0, 400.0)]);
            oscs(&mut r, w::UNISON, 1.0, 5.0);
            oscs(&mut r, w::VOICES, 0.0, 6.0);
            oscs(&mut r, w::SPREAD, 0.0, 50.0);
            oscs(&mut r, w::DETUNE, 0.0, 45.0);
            lfo_rates(&mut r, 0.05, 8.0);
            r.extend([(w::CHORUS_MIX, 0.0, 10.0), (w::DELAY_MIX, 0.0, 5.0), (w::REVERB_MIX, 0.0, 8.0), (w::REVERB_DECAY, 0.4, 2.0), (w::DIST_DRIVE, 0.0, 40.0)]);
            r.extend([(w::EQ_LOW, -1.0, 4.0), (w::EQ_HIGH, -9.0, 1.0), (w::EQ_MID, -4.0, 3.0), (w::DRIFT, 0.0, 15.0)]);
        }
        Kind::Lead => {
            oscs(&mut r, w::OCTAVE, -1.0, 1.0);
            r.extend([(w::SUB_LEVEL, 0.0, 20.0), (w::NOISE, 0.0, 8.0), (w::CUTOFF, 2200.0, 13_000.0), (w::RESONANCE, 0.0, 50.0), (w::FILTER_ENV, -10.0, 50.0), (w::DRIVE, 0.0, 60.0)]);
            r.extend([(w::AMP_ATTACK, 0.5, 30.0), (w::AMP_DECAY, 150.0, 3000.0), (w::AMP_SUSTAIN, 62.0, 100.0), (w::AMP_RELEASE, 40.0, 600.0), (w::MOD_ATTACK, 0.5, 200.0), (w::MOD_DECAY, 80.0, 2000.0)]);
            oscs(&mut r, w::VOICES, 0.0, 9.0);
            lfo_rates(&mut r, 0.05, 8.0);
            r.extend([(w::GLIDE, 0.0, 160.0), (w::CHORUS_MIX, 0.0, 30.0), (w::DELAY_MIX, 0.0, 28.0), (w::DELAY_FEEDBACK, 10.0, 50.0), (w::REVERB_MIX, 6.0, 30.0), (w::REVERB_DECAY, 1.0, 3.5)]);
            r.extend([(w::DIST_DRIVE, 0.0, 55.0), (w::EQ_HIGH, -3.0, 3.0), (w::EQ_LOW, -6.0, 1.0), (w::DRIFT, 0.0, 25.0)]);
        }
        Kind::Pad => {
            r.extend([(w::VOICE_MODE, 0.0, 0.0), (w::POLYPHONY, 12.0, 16.0), (w::GLIDE, 0.0, 0.0), (w::osc(0, w::OCTAVE), -1.0, 1.0), (w::osc(1, w::OCTAVE), -1.0, 2.0), (w::osc(2, w::OCTAVE), -1.0, 2.0)]);
            r.extend([(w::SUB_LEVEL, 0.0, 25.0), (w::NOISE, 0.0, 8.0), (w::CUTOFF, 450.0, 8000.0), (w::RESONANCE, 0.0, 32.0), (w::FILTER_ENV, -25.0, 30.0), (w::DRIVE, 0.0, 30.0)]);
            r.extend([(w::AMP_ATTACK, 180.0, 2200.0), (w::AMP_DECAY, 300.0, 4000.0), (w::AMP_SUSTAIN, 65.0, 100.0), (w::AMP_RELEASE, 1300.0, 4500.0), (w::MOD_ATTACK, 100.0, 4000.0), (w::MOD_DECAY, 300.0, 6000.0)]);
            r.extend([(w::osc(0, w::DETUNE), 12.0, 42.0), (w::osc(0, w::SPREAD), 55.0, 100.0)]);
            lfo_rates(&mut r, 0.02, 1.5);
            r.extend([(w::CHORUS_MIX, 0.0, 55.0), (w::DELAY_MIX, 0.0, 20.0), (w::REVERB_MIX, 25.0, 55.0), (w::REVERB_DECAY, 2.5, 7.5), (w::DIST_DRIVE, 0.0, 25.0), (w::DRIFT, 0.0, 55.0)]);
            r.extend([(w::EQ_HIGH, -6.0, 2.0), (w::EQ_LOW, -6.0, 2.0)]);
        }
        Kind::Pluck => {
            r.extend([(w::VOICE_MODE, 0.0, 0.0), (w::GLIDE, 0.0, 0.0)]);
            oscs(&mut r, w::OCTAVE, -1.0, 2.0);
            r.extend([(w::SUB_LEVEL, 0.0, 30.0), (w::NOISE, 0.0, 10.0), (w::CUTOFF, 250.0, 3000.0), (w::RESONANCE, 0.0, 45.0), (w::FILTER_ENV, 28.0, 80.0), (w::DRIVE, 0.0, 40.0)]);
            r.extend([(w::AMP_ATTACK, 0.5, 4.0), (w::AMP_DECAY, 120.0, 650.0), (w::AMP_SUSTAIN, 0.0, 2.0), (w::AMP_RELEASE, 80.0, 500.0), (w::env(0, w::E_HOLD), 0.0, 5.0)]);
            r.extend([(w::MOD_ATTACK, 0.5, 3.0), (w::MOD_DECAY, 60.0, 420.0), (w::MOD_SUSTAIN, 0.0, 8.0), (w::MOD_RELEASE, 60.0, 450.0)]);
            oscs(&mut r, w::VOICES, 0.0, 5.0);
            oscs(&mut r, w::UNISON, 1.0, 5.0);
            oscs(&mut r, w::DETUNE, 0.0, 35.0);
            lfo_rates(&mut r, 0.05, 8.0);
            r.extend([(w::CHORUS_MIX, 0.0, 28.0), (w::DELAY_MIX, 0.0, 32.0), (w::DELAY_FEEDBACK, 15.0, 48.0), (w::REVERB_MIX, 5.0, 32.0), (w::REVERB_DECAY, 1.0, 3.5), (w::DIST_DRIVE, 0.0, 35.0)]);
        }
        Kind::Keys => {
            r.extend([(w::VOICE_MODE, 0.0, 0.0), (w::GLIDE, 0.0, 0.0), (w::VEL_SENS, 65.0, 100.0)]);
            oscs(&mut r, w::OCTAVE, -1.0, 2.0);
            r.extend([(w::SUB_LEVEL, 0.0, 35.0), (w::NOISE, 0.0, 5.0), (w::CUTOFF, 250.0, 8500.0), (w::RESONANCE, 0.0, 25.0), (w::FILTER_ENV, -10.0, 45.0), (w::DRIVE, 0.0, 30.0)]);
            r.extend([(w::AMP_ATTACK, 0.5, 8.0), (w::AMP_DECAY, 350.0, 2400.0), (w::AMP_SUSTAIN, 0.0, 45.0), (w::AMP_RELEASE, 150.0, 850.0), (w::MOD_ATTACK, 0.5, 12.0), (w::MOD_DECAY, 100.0, 1500.0)]);
            oscs(&mut r, w::UNISON, 1.0, 2.0);
            oscs(&mut r, w::VOICES, 0.0, 2.0);
            oscs(&mut r, w::DETUNE, 0.0, 15.0);
            lfo_rates(&mut r, 0.05, 8.0);
            r.extend([(w::CHORUS_MIX, 0.0, 45.0), (w::DELAY_MIX, 0.0, 18.0), (w::REVERB_MIX, 5.0, 28.0), (w::REVERB_DECAY, 1.0, 3.0), (w::DIST_DRIVE, 0.0, 30.0)]);
        }
        Kind::Fx => {
            oscs(&mut r, w::OCTAVE, -2.0, 2.0);
            r.extend([(w::NOISE, 0.0, 85.0), (w::SUB_LEVEL, 0.0, 70.0), (w::CUTOFF, 90.0, 16_000.0), (w::RESONANCE, 0.0, 72.0)]);
            r.extend([(w::AMP_ATTACK, 0.5, 3000.0), (w::AMP_DECAY, 280.0, 6000.0), (w::AMP_RELEASE, 150.0, 4000.0)]);
            lfo_rates(&mut r, 0.05, 12.0);
            // Synced sweeps unfold within a bar (an 8-bar riser is silent for 8 s).
            for l in 0..4 {
                r.push((w::lfo(l, w::LFO_DIVISION), 3.0, 17.0));
            }
            r.extend([(w::CHORUS_MIX, 0.0, 45.0), (w::DELAY_MIX, 0.0, 42.0), (w::DELAY_FEEDBACK, 10.0, 62.0), (w::REVERB_MIX, 12.0, 62.0), (w::REVERB_DECAY, 1.5, 8.0)]);
        }
        Kind::Texture => {
            r.extend([(w::VOICE_MODE, 0.0, 0.0), (w::GLIDE, 0.0, 0.0)]);
            oscs(&mut r, w::OCTAVE, -1.0, 2.0);
            r.extend([(w::SUB_LEVEL, 0.0, 18.0), (w::NOISE, 15.0, 50.0), (w::CUTOFF, 300.0, 11_000.0), (w::RESONANCE, 0.0, 55.0)]);
            r.extend([(w::AMP_ATTACK, 250.0, 2500.0), (w::AMP_DECAY, 500.0, 6000.0), (w::AMP_SUSTAIN, 65.0, 100.0), (w::AMP_RELEASE, 1200.0, 4500.0)]);
            lfo_rates(&mut r, 0.015, 1.0);
            for l in 0..4 {
                r.push((w::lfo(l, w::LFO_DIVISION), 2.0, 17.0));
            }
            r.extend([(w::CHORUS_MIX, 0.0, 55.0), (w::DELAY_MIX, 0.0, 28.0), (w::REVERB_MIX, 35.0, 62.0), (w::REVERB_DECAY, 3.0, 9.0), (w::DRIFT, 10.0, 70.0)]);
        }
        Kind::Drums => {}
    }
    r
}

/// What one kind grows from.
pub struct KindPriors {
    /// `(name, patch)` of every parent preset.
    pub parents: Vec<(&'static str, Patch)>,
    /// Normalised band of every slot (`lo <= hi`).
    pub lo: [f32; PATCH_LEN],
    pub hi: [f32; PATCH_LEN],
    /// The rule bands as normalised `(slot, lo, hi)` clamps.
    pub clamps: Vec<(usize, f32, f32)>,
}

impl KindPriors {
    /// A plain value drawn uniformly (in normalised space) inside `id`'s band.
    fn draw(&self, id: u32, r: &mut Rng) -> f32 {
        let Some(s) = Patch::slot(id) else { return 0.0 };
        let n = self.lo[s] + (self.hi[s] - self.lo[s]) * r.unit();
        det_plain(&layout().infos[s], n)
    }
}

pub struct Priors {
    kinds: Vec<KindPriors>,
}

impl Priors {
    pub fn kind(&self, kind: Kind) -> &KindPriors {
        &self.kinds[kind.index()]
    }
}

fn build_priors() -> Priors {
    let l = layout();
    let kinds = Kind::ALL
        .iter()
        .map(|&kind| {
            let parents: Vec<(&'static str, Patch)> = PARENTS[kind.index()]
                .iter()
                .filter_map(|name| {
                    let preset = crate::presets::load(format!("{}/{name}.json", wt::ID)).ok()?;
                    let pairs = preset.params.iter().filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, *v)));
                    Some((*name, Patch::from_pairs(kind, pairs)))
                })
                .collect();
            let mut lo = [0f32; PATCH_LEN];
            let mut hi = [1f32; PATCH_LEN];
            // Prior bands: the parents' range, widened.
            if !parents.is_empty() {
                for k in 0..l.len {
                    let (mn, mx) = parents.iter().fold((f32::MAX, f32::MIN), |(a, b), (_, p)| (a.min(p.v[k]), b.max(p.v[k])));
                    let wide = 0.06 + 0.25 * (mx - mn);
                    lo[k] = (mn - wide).max(0.0);
                    hi[k] = (mx + wide).min(1.0);
                }
            }
            // Genes outside the priors explore the whole range.
            for g in genes().iter().filter(|g| !g.prior) {
                lo[g.slot] = 0.0;
                hi[g.slot] = 1.0;
            }
            // Rules win.
            let mut clamps = Vec::new();
            for (id, a, b) in rules(kind) {
                if let Some(s) = Patch::slot(id) {
                    let info = &l.infos[s];
                    let (na, nb) = (quantize(det_norm(info, a)), quantize(det_norm(info, b)));
                    lo[s] = na.min(nb);
                    hi[s] = na.max(nb);
                    clamps.push((s, lo[s], hi[s]));
                }
            }
            KindPriors { parents, lo, hi, clamps }
        })
        .collect();
    Priors { kinds }
}

/// The priors of every kind, built once (Bloom builds them at creation, off
/// the audio thread).
pub fn priors() -> &'static Priors {
    static P: OnceLock<Priors> = OnceLock::new();
    P.get_or_init(|| {
        let _ = genes();
        build_priors()
    })
}

// ── Editing helpers ─────────────────────────────────────────────────────────

fn set_wave(x: &mut Patch, o: u32, wave: usize, pos: f32) {
    x.set_plain(w::osc(o, w::WAVE), wave as f32);
    x.set_plain(w::osc(o, w::POS), pos);
}

fn set_unison(x: &mut Patch, o: u32, voices: f32, detune: f32, spread: f32) {
    x.set_plain(w::osc(o, w::VOICES), voices.round());
    x.set_plain(w::osc(o, w::DETUNE), detune);
    x.set_plain(w::osc(o, w::SPREAD), spread);
}

/// Unison voices of oscillator `o` (the 1..16 knob, else the original one).
fn unison(x: &Patch, o: u32) -> usize {
    let v = x.choice(w::osc(o, w::VOICES));
    if v == 0 { x.plain(w::osc(o, w::UNISON)).round() as usize } else { v }
}

/// What a matrix route is, from both matrices: `(slot id of its amount,
/// source class, destination class)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Src {
    None,
    Lfo(u8),
    Env,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dst {
    None,
    Pitch,
    Amp,
    Cutoff,
    Timbre,
    Other,
}

/// Every route: legacy slots first, then the 32-slot matrix.
fn routes(x: &Patch) -> impl Iterator<Item = (u32, Src, Dst)> + '_ {
    let legacy = (0..8u32).map(move |s| {
        let src = match x.choice(w::slot(s, w::MOD_SRC)) {
            0 => Src::None,
            1 => Src::Lfo(0),
            2 => Src::Lfo(1),
            3 | 9 => Src::Env,
            _ => Src::Other,
        };
        let dst = match x.choice(w::slot(s, w::MOD_DST)) {
            0 => Dst::None,
            1 | 2 | 12 => Dst::Pitch,
            7 => Dst::Amp,
            5 => Dst::Cutoff,
            3 | 4 | 10 | 11 => Dst::Timbre,
            _ => Dst::Other,
        };
        (w::slot(s, w::MOD_AMT), src, dst)
    });
    let matrix = (0..32u32).map(move |s| {
        let si = x.plain(w::slot_x(s, w::M_SRC)).round() as u8;
        let di = x.plain(w::slot_x(s, w::M_DST)).round() as u32;
        let src = match si {
            msrc::NONE => Src::None,
            msrc::LFO1..=msrc::LFO4 => Src::Lfo(si - msrc::LFO1),
            msrc::ENV1..=msrc::ENV3 => Src::Env,
            _ => Src::Other,
        };
        let pos = (0..3).any(|o| di == w::osc(o, w::POS) || di == w::osc(o, w::WARP_AMT));
        let dst = match di {
            0 => Dst::None,
            vdst::PITCH..=903 => Dst::Pitch,
            vdst::AMP => Dst::Amp,
            vdst::CUTOFF | w::CUTOFF => Dst::Cutoff,
            _ if di == w::filt(1, w::F_CUTOFF) => Dst::Cutoff,
            vdst::POS | vdst::OSC_MIX | vdst::DETUNE => Dst::Timbre,
            _ if pos => Dst::Timbre,
            _ => Dst::Other,
        };
        (w::slot_x(s, w::M_AMT), src, dst)
    });
    legacy.chain(matrix)
}

/// Add a route in the first free 32-slot matrix slot (its index; `None`
/// when the matrix is full).
fn add_route(x: &mut Patch, src: u8, dst: u32, amount: f32) -> Option<u32> {
    for s in 0..32u32 {
        if x.plain(w::slot_x(s, w::M_SRC)).round() as u8 == msrc::NONE {
            x.set_plain(w::slot_x(s, w::M_SRC), f32::from(src));
            x.set_plain(w::slot_x(s, w::M_DST), dst as f32);
            x.set_plain(w::slot_x(s, w::M_AMT), amount);
            x.set_plain(w::slot_x(s, w::M_MODE), 0.0);
            x.set_plain(w::slot_x(s, w::M_VIA), 0.0);
            return Some(s);
        }
    }
    None
}

/// An LFO no route reads (LFO 4, then 3), set free-running at `hz`.
fn spare_lfo(x: &mut Patch, shape: usize, hz: f32) -> Option<u8> {
    for l in [3u8, 2] {
        if routes(x).any(|(_, s, _)| s == Src::Lfo(l)) {
            continue;
        }
        let l32 = u32::from(l);
        x.set_plain(w::lfo(l32, w::LFO_SHAPE), shape as f32);
        x.set_plain(w::lfo(l32, w::LFO_RATE), hz);
        x.set_plain(w::lfo(l32, w::LFO_SYNC), 0.0);
        x.set_plain(w::lfo(l32, w::LFO_RETRIGGER), 1.0);
        x.set_plain(w::lfo(l32, w::LFO_DRAWN), 0.0);
        x.set_plain(w::lfo(l32, w::LFO_ONESHOT), 0.0);
        x.set_plain(w::lfo(l32, w::LFO_PHASE), 0.0);
        return Some(msrc::LFO1 + l);
    }
    None
}

/// Whether some LFO or envelope moves the timbre, cutoff or pitch by at
/// least `min` % (`pitch_min` for pitch).
fn moves(x: &Patch, min: f32, pitch_min: f32) -> bool {
    routes(x).any(|(amt, s, d)| {
        let a = x.plain(amt).abs();
        matches!(s, Src::Lfo(_) | Src::Env) && match d {
            Dst::Pitch => a >= pitch_min,
            Dst::Cutoff | Dst::Timbre => a >= min,
            _ => false,
        }
    })
}

/// Make sure an LFO moves the timbre or cutoff by at least `min` %: a new
/// slow route (`shape`, `hz`, `amount`) on a spare LFO, else the deepest
/// existing LFO route deepened, else a route from LFO 3 as it is.
fn ensure_movement(x: &mut Patch, min: f32, shape: usize, hz: f32, amount: f32) {
    if moves(x, min, 100.0) {
        return;
    }
    if let Some(l) = spare_lfo(x, shape, hz) {
        add_route(x, l, vdst::CUTOFF, amount);
        return;
    }
    let mut best: Option<(u32, f32)> = None;
    for (amt, s, d) in routes(x) {
        if matches!(s, Src::Lfo(_)) && matches!(d, Dst::Cutoff | Dst::Timbre) {
            let a = x.plain(amt);
            if best.is_none_or(|(_, b)| a.abs() > b.abs()) {
                best = Some((amt, a));
            }
        }
    }
    match best {
        Some((amt, a)) => x.set_plain(amt, if a < 0.0 { -amount } else { amount }),
        None => {
            add_route(x, msrc::LFO3, vdst::CUTOFF, amount);
        }
    }
}

/// Total source level (oscillators, sub, noise), percent.
fn source_level(x: &Patch) -> f32 {
    (0..3).map(|o| x.plain(w::osc(o, w::LEVEL))).sum::<f32>() + x.plain(w::SUB_LEVEL) + x.plain(w::NOISE)
}

// ── Rules ───────────────────────────────────────────────────────────────────

/// Hold a patch to its kind: the rule bands, then the cross-parameter
/// fixes. Idempotent; no allocation.
pub fn constrain(x: &mut Patch) {
    let kind = x.kind;
    if !kind.is_tonal() {
        return;
    }
    let kp = priors().kind(kind);
    for &(s, lo, hi) in &kp.clamps {
        let v = reflect(x.v[s]);
        x.v[s] = quantize(v.clamp(lo, hi));
    }
    // Pitch, amp and drastic modulation caps: (LFO → pitch, env → pitch, → amp) in %.
    let (lfo_pitch, env_pitch, amp) = match kind {
        Kind::Bass => (0.8, 14.0, 30.0),
        Kind::Lead => (2.0, 8.0, 20.0),
        Kind::Pad => (0.8, 3.0, 45.0),
        Kind::Pluck => (1.0, 6.0, 15.0),
        Kind::Keys => (0.6, 2.0, 18.0),
        Kind::Fx => (100.0, 100.0, 100.0),
        Kind::Texture => (3.0, 5.0, 40.0),
        Kind::Drums => (0.0, 0.0, 0.0),
    };
    let mut caps = [(0u32, 0f32); 40];
    let mut n = 0;
    for (amt, s, d) in routes(x) {
        let cap = match (s, d) {
            (Src::None, _) | (_, Dst::None) => continue,
            (Src::Env, Dst::Pitch) => env_pitch,
            (_, Dst::Pitch) => lfo_pitch,
            (_, Dst::Amp) => amp,
            _ => continue,
        };
        if n < caps.len() {
            caps[n] = (amt, cap);
            n += 1;
        }
    }
    for &(amt, cap) in &caps[..n] {
        x.clamp_plain(amt, -cap, cap);
    }
    // Something must sound.
    if source_level(x) < 45.0 {
        x.set_plain(w::osc(0, w::LEVEL), 75.0);
    }
    // No high-pass where the body matters.
    let hp = x.choice(w::FILTER_TYPE) == 1;
    match kind {
        Kind::Keys if hp => x.clamp_plain(w::CUTOFF, 120.0, 500.0),
        Kind::Fx | Kind::Texture => {}
        _ if hp => x.set_plain(w::FILTER_TYPE, 0.0),
        _ => {}
    }
    match kind {
        Kind::Bass => {
            // The fundamental comes from the oscillators at their octave and
            // the sub (held at −1 oct by the rules); keep the top filtered.
            if x.choice(w::FILTER_TYPE) == 2 {
                x.set_plain(w::FILTER_TYPE, 0.0);
            }
        }
        Kind::Pad => {
            if unison(x, 0) < 3 {
                let d = x.plain(w::osc(0, w::DETUNE));
                set_unison(x, 0, 5.0, d, x.plain(w::osc(0, w::SPREAD)));
            }
            ensure_movement(x, 6.0, 1, 0.12, 10.0);
        }
        Kind::Pluck => {
            // The filter envelope is the mod envelope.
            x.clamp_plain(w::MOD_SUSTAIN, 0.0, 8.0);
        }
        Kind::Keys => {
            let vel_route = (0..32u32).any(|s| x.plain(w::slot_x(s, w::M_SRC)).round() as u8 == msrc::VELOCITY) || (0..8u32).any(|s| x.choice(w::slot(s, w::MOD_SRC)) == 4);
            if !vel_route {
                add_route(x, msrc::VELOCITY, w::CUTOFF, 14.0);
            }
            // Harder brightens: through a high-pass, velocity lowers it.
            let sign = if x.choice(w::FILTER_TYPE) == 1 { -1.0 } else { 1.0 };
            for s in 0..32u32 {
                let dst = x.plain(w::slot_x(s, w::M_DST)).round() as u32;
                if x.plain(w::slot_x(s, w::M_SRC)).round() as u8 == msrc::VELOCITY && (dst == w::CUTOFF || dst == vdst::CUTOFF) {
                    let a = x.plain(w::slot_x(s, w::M_AMT)).abs();
                    x.set_plain(w::slot_x(s, w::M_AMT), sign * a);
                }
            }
            for s in 0..8u32 {
                if x.choice(w::slot(s, w::MOD_SRC)) == 4 && x.choice(w::slot(s, w::MOD_DST)) == 5 {
                    let a = x.plain(w::slot(s, w::MOD_AMT)).abs();
                    x.set_plain(w::slot(s, w::MOD_AMT), sign * a);
                }
            }
        }
        Kind::Fx => {
            if !moves(x, 18.0, 6.0)
                && x.plain(w::NOISE) < 30.0
                && let Some(l) = spare_lfo(x, 3, 0.25)
            {
                add_route(x, l, vdst::CUTOFF, 35.0);
            }
        }
        Kind::Texture => {
            // Noise 15–50 % by the rules: always grainy.
            ensure_movement(x, 8.0, 6, 0.07, 16.0);
        }
        Kind::Lead => {
            // Bright: some loud oscillator has harmonics (not a plain sine
            // or triangle table).
            let bright = (0..3).any(|o| x.plain(w::osc(o, w::LEVEL)) >= 25.0 && (x.choice(w::osc(o, w::WAVE)) > 0 || x.choice(w::osc(o, w::TABLE)) <= 3));
            if !bright {
                x.set_plain(w::osc(0, w::TABLE), 3.0);
                x.set_plain(w::osc(0, w::WAVE), 0.0);
                x.clamp_plain(w::osc(0, w::POS), 55.0, 100.0);
                x.clamp_plain(w::osc(0, w::LEVEL), 60.0, 100.0);
            }
        }
        Kind::Drums => {}
    }
}

// ── The scene's character ───────────────────────────────────────────────────

/// What every patch of a scene shares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Character {
    /// The lineage: the parent most cells descend from.
    pub parent: usize,
    /// The scene's wave family (a factory `WAVE`, 0 = the parents' tables).
    pub wave: usize,
    /// Octaves of filter brightness.
    pub bright: f32,
    /// Unison detune and spread scale.
    pub width: f32,
    /// 0..1: distortion.
    pub grit: f32,
    /// 0..1: how much the design moves add movement.
    pub motion: f32,
    /// Multiplies decays and releases (and a pad's attack).
    pub time: f32,
    /// The shared room (plain values).
    pub reverb: f32,
    pub reverb_decay: f32,
    pub delay: f32,
    pub delay_time: f32,
    pub delay_fb: f32,
    pub chorus: f32,
}

impl Character {
    pub fn of(kind: Kind, r: &mut Rng) -> Self {
        let kp = priors().kind(kind);
        let n = kp.parents.len().max(1);
        let list = waves(kind);
        let parent = r.below(n);
        let wave = if r.chance(0.6) { list[r.below(list.len())] } else { 0 };
        let bright = r.between(-0.7, 0.7);
        let width = r.between(0.7, 1.3);
        let grit = r.unit() * r.unit();
        let motion = r.between(0.3, 1.0);
        let time = det_exp2(r.between(-0.5, 0.5));
        let reverb = kp.draw(w::REVERB_MIX, r);
        let reverb_decay = kp.draw(w::REVERB_DECAY, r);
        let delay = if r.chance(0.5) { kp.draw(w::DELAY_MIX, r) } else { 0.0 };
        let delay_time = r.pick(&[0.0, 2.0, 3.0, 5.0]);
        let delay_fb = kp.draw(w::DELAY_FEEDBACK, r);
        let chorus = if r.chance(0.55) { kp.draw(w::CHORUS_MIX, r) } else { 0.0 };
        Self { parent, wave, bright, width, grit, motion, time, reverb, reverb_decay, delay, delay_time, delay_fb, chorus }
    }
}

// ── Cores ───────────────────────────────────────────────────────────────────

fn copy_block(x: &mut Patch, from: &Patch, block: Block) {
    let l = layout();
    for k in 0..l.len {
        if block_of(l.ids[k]) == block {
            x.v[k] = from.v[k];
        }
    }
}

fn scale_plain(x: &mut Patch, id: u32, by: f32) {
    let v = x.plain(id);
    x.set_plain(id, v * by);
}

/// The kind's design moves on a crossed-over patch.
fn design(kind: Kind, x: &mut Patch, ch: &Character, r: &mut Rng) {
    let list = waves(kind);
    let pick_wave = |r: &mut Rng| if ch.wave > 0 && r.chance(0.5) { ch.wave } else { list[r.below(list.len())] };
    match kind {
        Kind::Bass => {
            if r.chance(0.4) {
                let wv = pick_wave(r);
                set_wave(x, 0, wv, r.between(10.0, 80.0));
            }
            if r.chance(0.3) {
                // Reese: two detuned voices.
                set_unison(x, 0, r.pick(&[2.0, 3.0]), r.between(15.0, 35.0), r.between(10.0, 40.0));
            }
            if r.chance(0.3) {
                x.set_plain(w::filt(0, w::F_MODEL), r.pick(&[1.0, 6.0, 9.0]));
                x.set_plain(w::RESONANCE, r.between(15.0, 50.0));
            }
            if r.chance(0.3) {
                // Plucky bass: the filter and amp close.
                x.set_plain(w::AMP_SUSTAIN, r.between(0.0, 40.0));
                x.set_plain(w::AMP_DECAY, r.between(180.0, 700.0));
                x.set_plain(w::MOD_SUSTAIN, 0.0);
                x.set_plain(w::MOD_DECAY, r.between(80.0, 350.0));
                x.set_plain(w::FILTER_ENV, r.between(20.0, 55.0));
            }
            x.set_plain(w::SUB_LEVEL, r.between(22.0, 50.0));
        }
        Kind::Lead => {
            if r.chance(0.45) {
                let wv = pick_wave(r);
                set_wave(x, 0, wv, r.between(0.0, 90.0));
            }
            if r.chance(0.5) {
                set_unison(x, 0, r.between(3.0, 7.0), r.between(15.0, 40.0) * ch.width, r.between(40.0, 90.0));
            }
            if r.chance(0.4) {
                x.set_plain(w::VOICE_MODE, r.pick(&[1.0, 2.0]));
                x.set_plain(w::GLIDE, r.between(25.0, 140.0));
            }
            if r.chance(0.35)
                && let Some(l) = spare_lfo(x, 0, r.between(4.6, 6.2))
            {
                // Vibrato, fading in.
                x.set_plain(w::lfo(u32::from(l - msrc::LFO1), w::LFO_FADE), r.between(250.0, 700.0));
                add_route(x, l, vdst::PITCH, r.between(0.5, 1.2));
            }
            if r.chance(0.4) {
                x.set_plain(w::FILTER_ENV, r.between(15.0, 40.0));
                x.set_plain(w::MOD_DECAY, r.between(200.0, 800.0));
                x.set_plain(w::MOD_SUSTAIN, r.between(10.0, 50.0));
            }
        }
        Kind::Pad => {
            if r.chance(0.55) {
                let wv = pick_wave(r);
                set_wave(x, 0, wv, r.between(5.0, 85.0));
            }
            if r.chance(0.3) {
                let wv = list[r.below(list.len())];
                set_wave(x, 1, wv, r.between(5.0, 85.0));
                x.set_plain(w::osc(1, w::LEVEL), r.between(25.0, 55.0));
                x.set_plain(w::osc(1, w::OCTAVE), r.pick(&[0.0, 1.0]));
            }
            set_unison(x, 0, r.between(4.0, 9.0), r.between(15.0, 38.0) * ch.width, r.between(65.0, 100.0));
            let hz = r.between(0.05, 0.45);
            let shape = r.pick(&[0usize, 1, 6]);
            let wave_on = x.choice(w::osc(0, w::WAVE)) > 0;
            let amt = (r.between(10.0, 35.0) * ch.motion).max(8.0);
            let dst = if wave_on && r.chance(0.6) { w::osc(0, w::POS) } else { vdst::CUTOFF };
            let amt = if dst == vdst::CUTOFF { amt * 0.6 } else { amt };
            if let Some(l) = spare_lfo(x, shape, hz) {
                add_route(x, l, dst, amt);
            }
            if r.chance(0.4)
                && let Some(l) = spare_lfo(x, 0, r.between(0.07, 0.3))
            {
                add_route(x, l, vdst::PAN, r.between(15.0, 35.0));
            }
            scale_plain(x, w::AMP_ATTACK, ch.time);
            scale_plain(x, w::AMP_RELEASE, ch.time);
        }
        Kind::Pluck => {
            if r.chance(0.45) {
                let wv = pick_wave(r);
                set_wave(x, 0, wv, r.between(0.0, 80.0));
            }
            if r.chance(0.35) {
                set_unison(x, 0, r.between(2.0, 4.0), r.between(10.0, 30.0) * ch.width, r.between(40.0, 80.0));
            }
            if r.chance(0.3) {
                x.set_plain(w::osc(1, w::OCTAVE), 1.0);
                x.set_plain(w::osc(1, w::LEVEL), r.between(20.0, 45.0));
            }
            x.set_plain(w::FILTER_ENV, r.between(35.0, 75.0));
            x.set_plain(w::MOD_DECAY, r.between(90.0, 380.0) * ch.time);
            x.set_plain(w::AMP_DECAY, r.between(180.0, 600.0) * ch.time);
        }
        Kind::Keys => {
            if r.chance(0.5) {
                let wv = pick_wave(r);
                set_wave(x, 0, wv, r.between(0.0, 60.0));
            }
            if r.chance(0.4)
                && let Some(l) = spare_lfo(x, 0, r.between(3.5, 6.0))
            {
                add_route(x, l, vdst::AMP, r.between(5.0, 14.0));
            }
            if r.chance(0.3) {
                x.set_plain(w::osc(1, w::OCTAVE), r.pick(&[1.0, 2.0]));
                x.set_plain(w::osc(1, w::LEVEL), r.between(15.0, 35.0));
            }
            add_route(x, msrc::VELOCITY, w::CUTOFF, r.between(10.0, 24.0));
            scale_plain(x, w::AMP_DECAY, ch.time);
        }
        Kind::Fx => fx_archetype(x, ch, r),
        Kind::Texture => {
            if r.chance(0.7) {
                let wv = pick_wave(r);
                set_wave(x, 0, wv, r.between(10.0, 90.0));
            }
            x.set_plain(w::NOISE, r.between(18.0, 45.0));
            x.set_plain(w::NOISE_COLOR, r.pick(&[0.0, 1.0]));
            set_unison(x, 0, r.between(3.0, 8.0), r.between(15.0, 45.0) * ch.width, r.between(70.0, 100.0));
            if r.chance(0.4) {
                x.set_plain(w::filt(0, w::F_MODEL), r.pick(&[2.0, 3.0, 5.0]));
                x.set_plain(w::RESONANCE, r.between(20.0, 50.0));
            }
            if r.chance(0.3) {
                x.set_plain(w::FILTER_TYPE, 2.0);
            }
            let wave_on = x.choice(w::osc(0, w::WAVE)) > 0;
            if let Some(l) = spare_lfo(x, r.pick(&[0usize, 1, 6]), r.between(0.08, 0.4)) {
                let dst = if wave_on { w::osc(0, w::POS) } else { vdst::DETUNE };
                add_route(x, l, dst, (r.between(15.0, 45.0) * ch.motion).max(12.0));
            }
            if let Some(l) = spare_lfo(x, 6, r.between(0.05, 0.2)) {
                add_route(x, l, vdst::CUTOFF, (r.between(8.0, 20.0) * ch.motion).max(8.0));
            }
            x.set_plain(w::DRIFT, r.between(20.0, 60.0));
        }
        Kind::Drums => {}
    }
}

/// FX cells take one of the classic shapes over their parent.
fn fx_archetype(x: &mut Patch, ch: &Character, r: &mut Rng) {
    let env3 = |x: &mut Patch, a: f32, d: f32, s: f32, rel: f32| {
        x.set_plain(w::env(2, w::E_ATTACK), a);
        x.set_plain(w::env(2, w::E_DECAY), d);
        x.set_plain(w::env(2, w::E_SUSTAIN), s);
        x.set_plain(w::env(2, w::E_RELEASE), rel);
    };
    if r.chance(0.35) {
        let wv = waves(Kind::Fx)[r.below(waves(Kind::Fx).len())];
        set_wave(x, 0, wv, r.between(0.0, 100.0));
    }
    match r.below(7) {
        0 => {
            // Riser: noise and pitch climb over seconds.
            x.set_plain(w::NOISE, r.between(30.0, 70.0));
            env3(x, r.between(1500.0, 4000.0), 1000.0, 100.0, 800.0);
            add_route(x, msrc::ENV3, vdst::PITCH, r.between(10.0, 30.0));
            add_route(x, msrc::ENV3, vdst::CUTOFF, r.between(25.0, 45.0));
            x.set_plain(w::AMP_ATTACK, r.between(600.0, 2000.0));
            x.set_plain(w::AMP_SUSTAIN, 100.0);
        }
        1 => {
            // Downer: starts high, falls.
            env3(x, 1.0, r.between(1200.0, 3000.0), 0.0, 1000.0);
            add_route(x, msrc::ENV3, vdst::PITCH, r.between(20.0, 50.0));
            add_route(x, msrc::ENV3, vdst::CUTOFF, r.between(15.0, 35.0));
            x.set_plain(w::AMP_DECAY, r.between(1500.0, 3500.0));
            x.set_plain(w::AMP_SUSTAIN, r.between(0.0, 30.0));
        }
        2 => {
            // Impact: a dropping boom, noise, a big room.
            x.set_plain(w::osc(0, w::OCTAVE), -1.0);
            x.set_plain(w::SUB_LEVEL, r.between(40.0, 70.0));
            x.set_plain(w::NOISE, r.between(25.0, 55.0));
            env3(x, 0.5, r.between(80.0, 300.0), 0.0, 200.0);
            add_route(x, msrc::ENV3, vdst::PITCH, r.between(20.0, 45.0));
            x.set_plain(w::AMP_ATTACK, 0.5);
            x.set_plain(w::AMP_DECAY, r.between(1500.0, 3500.0));
            x.set_plain(w::AMP_SUSTAIN, 0.0);
            x.set_plain(w::REVERB_MIX, r.between(40.0, 60.0));
            x.set_plain(w::REVERB_DECAY, r.between(4.0, 8.0));
        }
        3 => {
            // Zap: a fast, deep pitch fall.
            env3(x, 0.5, r.between(40.0, 200.0), 0.0, 100.0);
            add_route(x, msrc::ENV3, vdst::PITCH, r.between(40.0, 90.0));
            x.set_plain(w::AMP_DECAY, r.between(150.0, 500.0));
            x.set_plain(w::AMP_SUSTAIN, 0.0);
        }
        4 => {
            // Siren: the pitch swings.
            if let Some(l) = spare_lfo(x, r.pick(&[0usize, 1]), r.between(0.3, 2.0)) {
                add_route(x, l, vdst::PITCH, r.between(8.0, 22.0));
            }
        }
        5 => {
            // Sweep: a slow resonant filter wave over noise.
            x.set_plain(w::NOISE, r.between(20.0, 50.0));
            x.set_plain(w::RESONANCE, r.between(35.0, 65.0));
            if let Some(l) = spare_lfo(x, r.pick(&[1usize, 2, 3]), r.between(0.08, 0.4)) {
                add_route(x, l, vdst::CUTOFF, r.between(30.0, 55.0) * ch.motion.max(0.6));
            }
        }
        _ => {
            // The parent as it is (its own riser, siren, wind…).
        }
    }
}

/// The core of a cell: parents crossed by blocks, the kind's moves, the
/// scene's character, a small mutation, the rules.
pub fn core(kind: Kind, ch: &Character, r: &mut Rng) -> Patch {
    let kp = priors().kind(kind);
    let n = kp.parents.len();
    if n == 0 {
        return Patch::neutral(kind);
    }
    let p1 = if r.chance(0.45) { ch.parent.min(n - 1) } else { r.below(n) };
    let p2 = r.below(n);
    let mut x = kp.parents[p1].1;
    x.kind = kind;
    // An FX parent is an archetype (its envelopes and motion make it a
    // riser or a zap): only its timbre crosses over.
    let blocks: &[Block] = if kind == Kind::Fx { &[Block::Osc, Block::Filter] } else { &[Block::Osc, Block::Filter, Block::Amp, Block::Motion, Block::Voice] };
    for &block in blocks {
        if p2 != p1 && r.chance(0.3) {
            copy_block(&mut x, &kp.parents[p2].1, block);
        }
    }
    // The shared room.
    let jitter = |r: &mut Rng| r.between(0.85, 1.15);
    x.set_plain(w::REVERB_MIX, ch.reverb * jitter(r));
    x.set_plain(w::REVERB_DECAY, ch.reverb_decay * jitter(r));
    x.set_plain(w::DELAY_MIX, ch.delay * jitter(r));
    x.set_plain(w::DELAY_TIME, ch.delay_time);
    x.set_plain(w::DELAY_FEEDBACK, ch.delay_fb);
    x.set_plain(w::CHORUS_MIX, ch.chorus * jitter(r));
    design(kind, &mut x, ch, r);
    // The scene's colour.
    let lift = det_exp2(ch.bright + r.between(-0.35, 0.35));
    scale_plain(&mut x, w::CUTOFF, lift);
    scale_plain(&mut x, w::filt(1, w::F_CUTOFF), lift);
    for o in 0..3 {
        scale_plain(&mut x, w::osc(o, w::DETUNE), ch.width);
    }
    if ch.grit > 0.35 && r.chance(ch.grit) && !matches!(kind, Kind::Pad | Kind::Texture) {
        if x.choice(w::DIST_MODE) == 0 {
            x.set_plain(w::DIST_MODE, r.pick(&[1.0, 4.0]));
        }
        x.set_plain(w::DIST_DRIVE, r.between(15.0, 45.0) * ch.grit);
        x.set_plain(w::DIST_MIX, r.between(50.0, 100.0));
    }
    scale_plain(&mut x, w::AMP_DECAY, ch.time.sqrt());
    scale_plain(&mut x, w::AMP_RELEASE, ch.time.sqrt());
    // A small mutation of every explorable value.
    for g in genes() {
        let step = r.normal() * g.sens * 0.3;
        if !g.route {
            x.v[g.slot] = quantize((x.v[g.slot] + step).clamp(kp.lo[g.slot], kp.hi[g.slot]));
        } else {
            x.v[g.slot] = quantize((x.v[g.slot] + step * 0.25).clamp(0.0, 1.0));
        }
    }
    constrain(&mut x);
    x
}

// ── Growth paths ────────────────────────────────────────────────────────────

/// Fold `v` into `lo..=hi` by mirroring at the ends.
fn fold(v: f32, lo: f32, hi: f32) -> f32 {
    if hi - lo < 1e-6 {
        return lo;
    }
    lo + reflect((v - lo) / (hi - lo)) * (hi - lo)
}

/// Categorical genes of a path: oscillator 1 and 2 wavetables, filter 1's
/// model, LFO 1's shape.
const SWITCHES: usize = 4;

/// The sound `d` along growth path `path` from `core` (its kind's rules
/// applied).
pub fn grow(core: &Patch, path: u64, d: f32) -> Patch {
    let d = d.max(0.0);
    let kind = core.kind;
    let mut out = core.folded();
    if !kind.is_tonal() {
        return out;
    }
    let kp = priors().kind(kind);
    let mut r = Rng::new(path);
    for g in genes() {
        let dir = r.normal() * g.sens;
        let s = g.slot;
        if g.route {
            // Moves only when routed; pitch and amp routes ten times less.
            out.v[s] = quantize(reflect(core.v[s] + d * dir * 0.5));
        } else {
            let (lo, hi) = (kp.lo[s], kp.hi[s]);
            out.v[s] = quantize(fold(core.v[s].clamp(lo, hi) + d * dir, lo, hi));
        }
    }
    // Pitch / amp routes: undo most of their move.
    for (amt, src, dst) in routes(core) {
        if src != Src::None && matches!(dst, Dst::Pitch | Dst::Amp)
            && let Some(s) = Patch::slot(amt)
        {
            out.v[s] = quantize(reflect(core.v[s] + (out.v[s] - reflect(core.v[s])) * 0.1));
        }
    }
    for k in 0..SWITCHES {
        let switch_at = r.between(0.3, 1.4);
        let shift = r.unit();
        if d < switch_at {
            continue;
        }
        match k {
            0 | 1 => {
                let id = w::osc(k as u32, w::WAVE);
                let list = waves(kind);
                let now = out.choice(id);
                let at = list.iter().position(|v| *v == now);
                let next = match at {
                    Some(i) => list[(i + 1 + (shift * (list.len() - 1) as f32) as usize) % list.len()],
                    None if k == 0 => list[(shift * list.len() as f32) as usize % list.len()],
                    None => continue,
                };
                if k == 1 && out.plain(w::osc(1, w::LEVEL)) < 5.0 {
                    continue;
                }
                out.set_plain(id, next as f32);
            }
            2 => {
                let list = models(kind);
                let now = out.choice(w::filt(0, w::F_MODEL));
                let i = list.iter().position(|v| *v == now).unwrap_or(0);
                out.set_plain(w::filt(0, w::F_MODEL), list[(i + 1 + (shift * (list.len() - 1) as f32) as usize) % list.len()] as f32);
            }
            _ => {
                let shapes = [0usize, 1, 2, 3, 4, 6];
                let now = out.choice(w::lfo(0, w::LFO_SHAPE));
                let i = shapes.iter().position(|v| *v == now).unwrap_or(0);
                out.set_plain(w::lfo(0, w::LFO_SHAPE), shapes[(i + 1 + (shift * 4.0) as usize) % shapes.len()] as f32);
            }
        }
    }
    constrain(&mut out);
    out
}

/// The core whose path `path` passes near `sound` at distance `d` (an edit
/// keeps its cell's place; the rules may move the regrown sound a little).
pub fn rebase(sound: &Patch, path: u64, d: f32) -> Patch {
    let d = d.max(0.0);
    let mut core = sound.folded();
    let mut r = Rng::new(path);
    for g in genes() {
        let dir = r.normal() * g.sens;
        let k = if g.route { 0.5 } else { 1.0 };
        core.v[g.slot] = quantize(core.v[g.slot] - d * dir * k);
    }
    core
}

// ── Scenes ──────────────────────────────────────────────────────────────────

/// Grow the scene of `seed` and `kind` into `out` (no allocation: the
/// device does this on the audio thread).
pub fn generate_into(out: &mut PatchScene, seed: u64, kind: Kind) {
    out.seed = seed;
    out.kind = kind;
    out.locks = 0;
    if !kind.is_tonal() {
        let p = Patch::neutral(kind);
        out.patches = [p; SOUNDS];
        out.cores = [p; SOUNDS];
        out.paths = [0; SOUNDS];
        out.distances = [0.0; SOUNDS];
        return;
    }
    let mut r = Rng::new(part(seed, 99, kind.salt()));
    let ch = Character::of(kind, &mut r);
    for i in 0..SOUNDS {
        let mut rr = Rng::new(part(seed, i as u64, kind.salt() + 1));
        out.cores[i] = core(kind, &ch, &mut rr);
        out.paths[i] = part(seed, i as u64, kind.salt() + 2);
        out.distances[i] = quantize(rr.between(0.08, 0.6));
        out.patches[i] = grow(&out.cores[i], out.paths[i], out.distances[i]);
    }
}

/// The scene of `seed` and `kind`.
pub fn generate(seed: u64, kind: Kind) -> PatchScene {
    let mut s = PatchScene::empty(kind);
    generate_into(&mut s, seed, kind);
    s
}

/// The genome between `a` (t = 0) and `b` (t = 1): amounts slide, choices
/// switch halfway.
pub fn morph(a: &Patch, b: &Patch, t: f32) -> Patch {
    let t = t.clamp(0.0, 1.0);
    let mut out = if t < 0.5 { *a } else { *b };
    let l = layout();
    for k in 0..l.len {
        if l.infos[k].scale != aitrack_dsp::ParamScale::Stepped && l.infos[k].choices.is_empty() {
            out.v[k] = quantize(a.v[k] + (b.v[k] - a.v[k]) * t);
        }
    }
    out
}

// ── Morph X/Y ───────────────────────────────────────────────────────────────

/// The four tonal morph corners a state may hold (`extra.patch_corners`):
/// each the sixteen patches of a stored scene, A (x 0, y 0), B (1, 0),
/// C (0, 1), D (1, 1). A corner counts only while its kind is the device's.
pub type PatchCorners = [Option<Box<[Patch; SOUNDS]>>; 4];

/// Bilinear weights of the corners A, B, C, D at `(x, y)` (each 0..1).
pub fn corner_weights(x: f32, y: f32) -> [f32; 4] {
    let (x, y) = (if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 }, if y.is_finite() { y.clamp(0.0, 1.0) } else { 0.0 });
    [(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y]
}

/// Whether a slot snaps (a choice or a stepped value) instead of sliding.
fn snaps(k: usize) -> bool {
    let info = &layout().infos[k];
    info.scale == aitrack_dsp::ParamScale::Stepped || !info.choices.is_empty()
}

/// The patch at `(x, y)` between four corners (A top left, B top right, C
/// bottom left, D bottom right; the kind is the heaviest corner's):
/// continuous values slide bilinearly (in normalised space, so times and
/// frequencies move geometrically); choices and stepped values snap to the
/// heaviest corner; a modulation route keeps the heaviest corner's source
/// and destination and blends its depth only with the corners that have
/// the same route there (the others count as depth 0, so a route fades in
/// and out instead of jumping onto another target). Then the kind's rules
/// are applied again. At a corner it is that corner exactly. Deterministic
/// and allocation-free (the device calls it on the audio thread).
pub fn morph4(c: [&Patch; 4], x: f32, y: f32) -> Patch {
    let w = corner_weights(x, y);
    if let Some(k) = w.iter().position(|v| *v >= 1.0) {
        return c[k].folded();
    }
    let mut win = 0;
    for k in 1..4 {
        if w[k] > w[win] {
            win = k;
        }
    }
    let f = [c[0].folded(), c[1].folded(), c[2].folded(), c[3].folded()];
    let mut out = f[win];
    let l = layout();
    for k in 0..l.len {
        if !snaps(k) {
            out.v[k] = quantize(w[0] * f[0].v[k] + w[1] * f[1].v[k] + w[2] * f[2].v[k] + w[3] * f[3].v[k]);
        }
    }
    let route = |out: &mut Patch, src: u32, dst: u32, amt: u32| {
        let (Some(s), Some(d), Some(a)) = (Patch::slot(src), Patch::slot(dst), Patch::slot(amt)) else { return };
        let zero = quantize(det_norm(&l.infos[a], 0.0));
        let mut v = 0.0;
        for k in 0..4 {
            let same = f[k].v[s] == f[win].v[s] && f[k].v[d] == f[win].v[d];
            v += w[k] * if same { f[k].v[a] } else { zero };
        }
        out.v[a] = quantize(v);
    };
    for s in 0..8 {
        route(&mut out, w::slot(s, w::MOD_SRC), w::slot(s, w::MOD_DST), w::slot(s, w::MOD_AMT));
    }
    for s in 0..32 {
        route(&mut out, w::slot_x(s, w::M_SRC), w::slot_x(s, w::M_DST), w::slot_x(s, w::M_AMT));
    }
    out.kind = c[win].kind;
    constrain(&mut out);
    out
}

/// The level gain at `(x, y)` between the corners' measured gains:
/// interpolated in decibels (geometrically), with the bilinear weights of
/// [`morph4`]. Deterministic and allocation-free.
pub fn morph_gain(g: [f32; 4], x: f32, y: f32) -> f32 {
    let w = corner_weights(x, y);
    if let Some(k) = w.iter().position(|v| *v >= 1.0) {
        return g[k];
    }
    let mut lg = 0.0;
    for k in 0..4 {
        lg += w[k] * det_log2(if g[k].is_finite() { g[k].max(1e-6) } else { 1.0 });
    }
    det_exp2(lg)
}

/// Store `scene`'s sixteen patches as corner `k` (0..4 = A..D), or clear
/// it (`None`).
pub fn set_corner(corners: &mut PatchCorners, k: usize, scene: Option<&PatchScene>) {
    if let Some(slot) = corners.get_mut(k) {
        *slot = scene.map(|s| Box::new(s.patches));
    }
}

/// The four corner patches of cell `i` for a scene of `kind`: a stored
/// corner of that kind, else the scene's own patch.
pub fn corner_patches<'a>(scene: &'a PatchScene, corners: &'a PatchCorners, i: usize) -> [&'a Patch; 4] {
    let i = i.min(SOUNDS - 1);
    let pick = |k: usize| match &corners[k] {
        Some(c) if c[i].kind == scene.kind => &c[i],
        _ => &scene.patches[i],
    };
    [pick(0), pick(1), pick(2), pick(3)]
}

/// Whether any corner holds a scene of `kind` (Morph X/Y then morphs it).
pub fn corners_active(corners: &PatchCorners, kind: Kind) -> bool {
    kind.is_tonal() && corners.iter().any(|c| c.as_ref().is_some_and(|c| c[0].kind == kind))
}

/// The scene as Morph X/Y makes it sound at `(x, y)`: every cell's patch
/// morphed between the corners (for display; the cores, paths and distances
/// are the scene's own). Without a corner of the scene's kind it is the
/// scene.
pub fn morph_scene(scene: &PatchScene, corners: &PatchCorners, x: f32, y: f32) -> PatchScene {
    let mut out = scene.clone();
    if corners_active(corners, scene.kind) {
        for i in 0..SOUNDS {
            out.patches[i] = morph4(corner_patches(scene, corners, i), x, y);
        }
    }
    out
}

/// A new roll: the scene of `seed`, reached from `prev` by `deviance`
/// (1 = entirely new). Locked cells stay. A roll to another kind is a new
/// scene of that kind (locks kept only within a kind).
pub fn roll(prev: &PatchScene, seed: u64, kind: Kind, deviance: f32) -> PatchScene {
    let fresh = generate(seed, kind);
    if prev.kind != kind {
        return fresh;
    }
    let t = deviance.clamp(0.0, 1.0);
    let mut out = fresh.clone();
    out.locks = prev.locks;
    for i in 0..SOUNDS {
        if prev.locked(i) {
            out.cores[i] = prev.cores[i];
            out.patches[i] = prev.patches[i];
            out.paths[i] = prev.paths[i];
            out.distances[i] = prev.distances[i];
            continue;
        }
        if t < 1.0 {
            let sound = morph(&prev.patches[i], &fresh.patches[i], t);
            let d = quantize(prev.distances[i] + (fresh.distances[i] - prev.distances[i]) * t);
            out.distances[i] = d;
            out.cores[i] = rebase(&sound, out.paths[i], d);
            out.patches[i] = grow(&out.cores[i], out.paths[i], d);
        }
    }
    out
}

/// Nudge every unlocked core by `amount` (0..1) in a direction drawn from
/// `seed`; each keeps its path and distance.
pub fn mutate(scene: &PatchScene, seed: u64, amount: f32) -> PatchScene {
    let mut out = scene.clone();
    let a = amount.clamp(0.0, 1.0);
    for i in 0..SOUNDS {
        if scene.locked(i) {
            continue;
        }
        let mut r = Rng::new(part(seed, i as u64, 0x3E7));
        let mut c = scene.cores[i];
        for g in genes() {
            let step = r.normal() * g.sens * a * 0.6 * if g.route { 0.25 } else { 1.0 };
            c.v[g.slot] = quantize(c.v[g.slot] + step);
        }
        out.cores[i] = c;
        out.patches[i] = grow(&c, scene.paths[i], scene.distances[i]);
    }
    out
}

/// Move cell `i` to `distance` along its path.
pub fn regrow(scene: &PatchScene, i: usize, distance: f32) -> PatchScene {
    let mut out = scene.clone();
    if i < SOUNDS {
        let d = quantize(distance.clamp(0.0, 1.5));
        out.distances[i] = d;
        out.patches[i] = grow(&scene.cores[i], scene.paths[i], d);
    }
    out
}

/// Put `patch` in cell `i` (an edit) keeping the cell's distance.
pub fn replace(scene: &PatchScene, i: usize, patch: &Patch) -> PatchScene {
    let mut out = scene.clone();
    if i < SOUNDS {
        let mut s = patch.folded();
        s.kind = scene.kind;
        out.cores[i] = rebase(&s, scene.paths[i], scene.distances[i]);
        out.patches[i] = s;
    }
    out
}

// ── Names ───────────────────────────────────────────────────────────────────

pub(crate) const DARK: &[&str] = &["Velvet", "Smoky", "Deep", "Murky", "Midnight", "Shadow", "Umber", "Sunken"];
pub(crate) const BRIGHT: &[&str] = &["Glass", "Crystal", "Neon", "Silver", "Solar", "Prism", "Chrome", "Glacier"];
pub(crate) const WIDE: &[&str] = &["Lush", "Choral", "Swarm", "Cloud", "Wide", "Halo", "Vast", "Plush"];
pub(crate) const GRIT: &[&str] = &["Rusty", "Burnt", "Feral", "Gritty", "Molten", "Scorched", "Raw", "Torn"];
pub(crate) const MOVING: &[&str] = &["Drifting", "Breathing", "Shifting", "Wandering", "Tidal", "Restless", "Orbiting", "Swaying"];
const NOISY: &[&str] = &["Dusty", "Hazy", "Static", "Misty", "Sandy", "Frosted", "Ashen", "Grainy"];
pub(crate) const SOFT: &[&str] = &["Warm", "Soft", "Hollow", "Round", "Amber", "Quiet", "Mellow", "Honey"];

fn nouns(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Bass => &["Reese", "Sub", "Growl", "Rumble", "Thump", "Pulse", "Wobble", "Drone", "Low End", "Boom"],
        Kind::Lead => &["Lead", "Saw", "Whistle", "Scream", "Hook", "Solo", "Spark", "Line", "Siren", "Flare"],
        Kind::Pad => &["Choir", "Haze", "Strings", "Bloom", "Veil", "Halo", "Tide", "Dawn", "Aurora", "Canopy"],
        Kind::Pluck => &["Pluck", "Drop", "Pick", "Harp", "Ping", "Kalimba", "Pizz", "Spark", "Dew", "Pebble"],
        Kind::Keys => &["Keys", "Piano", "Organ", "Bell", "Tines", "Stab", "Clav", "Celeste", "Chime", "Ivory"],
        Kind::Fx => &["Riser", "Zap", "Impact", "Sweep", "Siren", "Laser", "Downer", "Whoosh", "Comet", "Flux"],
        Kind::Texture => &["Mist", "Dust", "Static", "Grain", "Fog", "Field", "Ether", "Drift", "Swarm", "Rain"],
        Kind::Drums => &["Kit"],
    }
}

/// An evocative, deterministic name for a patch ("Velvet Reese", "Glass
/// Choir"): the adjective from what it sounds like (brightness, width,
/// grit, movement, noise), the noun from its kind; the fingerprint picks
/// among the candidates.
pub fn patch_name(patch: &Patch) -> String {
    let x = patch.folded();
    let h = patch.hash();
    let cutoff = x.plain(w::CUTOFF);
    let width = (0..3).map(|o| if x.plain(w::osc(o, w::LEVEL)) > 10.0 { unison(&x, o) } else { 1 }).max().unwrap_or(1);
    let grit = x.plain(w::DIST_DRIVE) * if x.choice(w::DIST_MODE) > 0 { 1.0 } else { 0.0 } + x.plain(w::DRIVE) * 0.5;
    let noisy = x.plain(w::NOISE) > 15.0 || x.choice(w::osc(0, w::WAVE)) == 12;
    let moving = moves(&x, 15.0, 6.0);
    let pool: &[&str] = match h % 3 {
        _ if grit > 35.0 => GRIT,
        0 if noisy => NOISY,
        1 if moving => MOVING,
        _ if width >= 5 => WIDE,
        _ if cutoff > 5_000.0 => BRIGHT,
        _ if cutoff < 900.0 => DARK,
        _ => SOFT,
    };
    let nn = nouns(patch.kind);
    let adj = pool[((h >> 8) % pool.len() as u64) as usize];
    let noun = nn[((h >> 24) % nn.len() as u64) as usize];
    format!("{adj} {noun}")
}

/// Names of a scene's sixteen patches, distinct within the scene.
pub fn scene_names(scene: &PatchScene) -> [String; SOUNDS] {
    let mut out: [String; SOUNDS] = Default::default();
    for i in 0..SOUNDS {
        let mut name = patch_name(&scene.patches[i]);
        let nn = nouns(scene.kind);
        let mut k = 0;
        while out[..i].contains(&name) && k < nn.len() {
            let adj = name.split(' ').next().unwrap_or("").to_string();
            name = format!("{adj} {}", nn[(i + k) % nn.len()]);
            k += 1;
        }
        if out[..i].contains(&name) {
            name = format!("{name} {}", i + 1);
        }
        out[i] = name;
    }
    out
}

// ── The device's knobs ──────────────────────────────────────────────────────

/// What Bloom's knobs do to a patch before the synth plays it: Decay scales
/// the amp, mod and env-3 decays and releases (×0.35..×2.8),
/// Body adds sub and unison width, Tone moves both cutoffs ±2 octaves (and
/// the EQ's top), Grit drives the distortion (switching it on) and the
/// filter, Space scales the patch's own reverb (×1 at the default 25 %),
/// Velocity scales its velocity sensitivity (×1 at the default 80 %). The
/// identity at the defaults. No allocation.
pub fn shape(patch: &Patch, m: &super::voice::Macros, space: f32, velocity: f32) -> Patch {
    shape_knobs(patch, &Knobs { macros: *m, space, velocity, ..Knobs::default() })
}

/// Every knob of the device that reaches a tonal patch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Knobs {
    pub macros: super::voice::Macros,
    /// 0..1 (Space; 0.25 = the patch's own reverb).
    pub space: f32,
    /// 0..1 (Velocity; 0.8 = the patch's own sensitivity).
    pub velocity: f32,
    /// −1..1: amp and mod env attack ×⅛..×8 (+ up to 20 ms).
    pub attack: f32,
    /// −1..1: a pitch envelope at the note start (+ falls from above, −
    /// rises from below, up to 12 st; 0 none).
    pub drop: f32,
    /// 0..2: depth of every LFO and envelope route.
    pub motion: f32,
    /// ms: at least this much glide (0 = the patch's own).
    pub glide: f32,
}

impl Default for Knobs {
    fn default() -> Self {
        Self { macros: super::voice::Macros::default(), space: 0.25, velocity: 0.8, attack: 0.0, drop: 0.0, motion: 1.0, glide: 0.0 }
    }
}

/// [`shape`] with the performance knobs (Attack, Drop, Motion, Glide).
pub fn shape_knobs(patch: &Patch, k: &Knobs) -> Patch {
    let (m, space, velocity) = (&k.macros, k.space, k.velocity);
    let mut x = patch.folded();
    if m.decay != 0.0 {
        let k = det_exp2(m.decay.clamp(-1.0, 1.0) * 1.5);
        for id in [w::AMP_DECAY, w::AMP_RELEASE, w::MOD_DECAY, w::MOD_RELEASE, w::env(2, w::E_DECAY), w::env(2, w::E_RELEASE)] {
            scale_plain(&mut x, id, k);
        }
    }
    if m.body != 0.0 {
        let b = m.body.clamp(-1.0, 1.0);
        let sub = x.plain(w::SUB_LEVEL);
        x.set_plain(w::SUB_LEVEL, sub + b * 35.0);
        for o in 0..3 {
            scale_plain(&mut x, w::osc(o, w::DETUNE), 1.0 + 0.5 * b);
            let s = x.plain(w::osc(o, w::SPREAD));
            x.set_plain(w::osc(o, w::SPREAD), s + b * 30.0);
        }
    }
    if m.tone != 0.0 {
        let t = m.tone.clamp(-1.0, 1.0);
        let k = det_exp2(t * 2.0);
        scale_plain(&mut x, w::CUTOFF, k);
        scale_plain(&mut x, w::filt(1, w::F_CUTOFF), k);
        let hi = x.plain(w::EQ_HIGH);
        x.set_plain(w::EQ_HIGH, hi + t * 3.0);
    }
    if m.grit > 0.0 {
        let g = m.grit.min(1.0);
        if x.choice(w::DIST_MODE) == 0 {
            x.set_plain(w::DIST_MODE, 1.0);
            x.set_plain(w::DIST_DRIVE, g * 55.0);
            x.set_plain(w::DIST_MIX, 100.0);
        } else {
            let d = x.plain(w::DIST_DRIVE);
            x.set_plain(w::DIST_DRIVE, d + g * 40.0);
        }
        let fd = x.plain(w::DRIVE);
        x.set_plain(w::DRIVE, fd + g * 35.0);
    } else if m.grit < 0.0 {
        let k = 1.0 + m.grit.max(-1.0);
        scale_plain(&mut x, w::DIST_DRIVE, k);
        scale_plain(&mut x, w::DIST_MIX, k);
        scale_plain(&mut x, w::DRIVE, k);
    }
    if (space - 0.25).abs() > 1e-6 {
        scale_plain(&mut x, w::REVERB_MIX, (0.4 + 2.4 * space.clamp(0.0, 1.0)).max(0.0));
    }
    if (velocity - 0.8).abs() > 1e-6 {
        scale_plain(&mut x, w::VEL_SENS, (velocity / 0.8).clamp(0.0, 1.25));
    }
    if (k.motion - 1.0).abs() > 1e-6 {
        let mut ids = [0u32; 40];
        let mut n = 0;
        for (amt, s, d) in routes(&x) {
            if matches!(s, Src::Lfo(_) | Src::Env) && d != Dst::None && n < ids.len() {
                ids[n] = amt;
                n += 1;
            }
        }
        for &id in &ids[..n] {
            scale_plain(&mut x, id, k.motion.clamp(0.0, 2.0));
        }
    }
    if k.attack != 0.0 {
        let a = k.attack.clamp(-1.0, 1.0);
        for id in [w::AMP_ATTACK, w::MOD_ATTACK] {
            let ms = x.plain(id);
            x.set_plain(id, ms * det_exp2(a * 3.0) + a.max(0.0) * 20.0);
        }
    }
    if k.glide > 0.0 {
        let g = x.plain(w::GLIDE);
        x.set_plain(w::GLIDE, g.max(k.glide));
    }
    if k.drop != 0.0 {
        let d = k.drop.clamp(-1.0, 1.0);
        let ms = 60.0 + 190.0 * d.abs();
        if !env3_used(&x) {
            x.set_plain(w::env(2, w::E_ATTACK), 0.5);
            x.set_plain(w::env(2, w::E_HOLD), 0.0);
            x.set_plain(w::env(2, w::E_DECAY), ms);
            x.set_plain(w::env(2, w::E_SUSTAIN), 0.0);
            x.set_plain(w::env(2, w::E_RELEASE), 50.0);
            add_route(&mut x, msrc::ENV3, vdst::PITCH, d * 50.0);
        } else if let Some(l) = spare_lfo(&mut x, 3, 1000.0 / (ms * 1.5)) {
            // A one-shot falling ramp, unipolar: 1 → 0 once per note.
            x.set_plain(w::lfo(u32::from(l - msrc::LFO1), w::LFO_ONESHOT), 1.0);
            if let Some(s) = add_route(&mut x, l, vdst::PITCH, d * 50.0) {
                x.set_plain(w::slot_x(s, w::M_MODE), 1.0);
            }
        }
    }
    x
}

/// Whether a route reads env 3.
fn env3_used(x: &Patch) -> bool {
    (0..32u32).any(|s| x.plain(w::slot_x(s, w::M_SRC)).round() as u8 == msrc::ENV3 || x.plain(w::slot_x(s, w::M_VIA)).round() as u8 == msrc::ENV3)
}

/// How noisy a patch is, 0..1, from its sources (the meter's noisiness).
pub fn noise_share(patch: &Patch) -> f32 {
    let x = patch.folded();
    let noise = x.plain(w::NOISE) + (0..3).filter(|&o| x.choice(w::osc(o, w::WAVE)) == 12).map(|o| x.plain(w::osc(o, w::LEVEL))).sum::<f32>();
    (noise / source_level(&x).max(1.0)).clamp(0.0, 1.0)
}

/// How much a patch's modulation moves it, 0..1 (the visual's wobble).
pub fn motion(patch: &Patch) -> f32 {
    let x = patch.folded();
    let mut m = 0.0f32;
    for (amt, s, d) in routes(&x) {
        if matches!(s, Src::Lfo(_)) {
            let a = x.plain(amt).abs() / 100.0;
            m += match d {
                Dst::Pitch => a * 8.0,
                Dst::Cutoff | Dst::Timbre | Dst::Amp => a,
                _ => 0.0,
            };
        }
    }
    m.clamp(0.0, 1.0)
}

// ── Level normalisation ─────────────────────────────────────────────────────

/// Rate of a level measurement (one held note).
pub const LEVEL_RATE: f64 = 24_000.0;
/// Frames rendered per audio block while a measurement runs on the audio
/// thread.
pub const LEVEL_CHUNK: usize = 1_440;
const LEVEL_WIN: usize = 1_200;

/// How long a kind's measurement listens (slow movers longer).
pub fn level_frames(kind: Kind) -> usize {
    match kind {
        Kind::Pad | Kind::Texture => 28_800,
        Kind::Fx => 57_600,
        _ => 8_640,
    }
}

/// The loudest 50 ms of a held note at velocity 0.8 lands at this RMS,
/// unless its peak would pass the second value. Kinds played in chords sit
/// lower, so a chord does not clip.
pub fn level_target(kind: Kind) -> (f32, f32) {
    match kind {
        Kind::Pad | Kind::Texture => (0.07, 0.28),
        Kind::Keys => (0.075, 0.3),
        Kind::Bass => (0.12, 0.45),
        _ => (0.11, 0.42),
    }
}

/// A one-pole DC blocker (8 Hz), per channel: the synth's asymmetric warps
/// and tube distortion leave an offset.
#[derive(Clone, Copy, Debug, Default)]
pub struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    #[inline]
    pub fn process(&mut self, x: f32, r: f32) -> f32 {
        let y = x - self.x1 + r * self.y1;
        self.x1 = x;
        self.y1 = if y.abs() < 1e-20 { 0.0 } else { y };
        self.y1
    }

    /// The pole for a sample rate.
    pub fn pole(sr: f32) -> f32 {
        1.0 - std::f32::consts::TAU * 8.0 / sr.max(1_000.0)
    }
}

/// The key a kind is measured and auditioned at.
pub fn home_key(kind: Kind) -> u8 {
    match kind {
        Kind::Bass => 40,
        Kind::Lead => 67,
        Kind::Pluck => 64,
        _ => 60,
    }
}

/// A patch as it is measured: the attacks shortened (a pad is measured at
/// its sustained level); FX as they are (2.4 s).
fn level_patch(patch: &Patch) -> Patch {
    let mut p = patch.folded();
    if patch.kind == Kind::Fx {
        // Risers and sweeps are measured as they unfold.
        return p;
    }
    p.clamp_plain(w::AMP_ATTACK, 0.5, 25.0);
    p.clamp_plain(w::env(0, w::E_HOLD), 0.0, 20.0);
    p.clamp_plain(w::MOD_ATTACK, 0.5, 25.0);
    p.clamp_plain(w::env(2, w::E_ATTACK), 0.5, 25.0);
    p
}

struct LevelJob {
    cell: usize,
    done: usize,
    peak: f32,
    acc: f64,
    acc_n: usize,
    best: f64,
    key: u8,
    kind: Kind,
    dc: [DcBlock; 2],
}

/// Measures patch levels with its own synth, a chunk at a time, so the
/// device can measure on the audio thread (no allocation after `new`).
pub struct Leveler {
    synth: Box<WavetableSynth>,
    buf: AudioBuffer,
    job: Option<LevelJob>,
}

impl Default for Leveler {
    fn default() -> Self {
        Self::new()
    }
}

impl Leveler {
    pub fn new() -> Self {
        let mut synth = Box::new(WavetableSynth::new());
        synth.prepare(LEVEL_RATE, LEVEL_CHUNK);
        Self { synth, buf: AudioBuffer::new(2, LEVEL_CHUNK), job: None }
    }

    /// The cell being measured.
    pub fn busy(&self) -> Option<usize> {
        self.job.as_ref().map(|j| j.cell)
    }

    pub fn cancel(&mut self) {
        self.job = None;
    }

    /// Start measuring `patch` as cell `cell`.
    pub fn start(&mut self, cell: usize, patch: &Patch) {
        let p = level_patch(patch);
        self.synth.restart();
        for (id, v) in p.pairs() {
            self.synth.set_param(id, v);
        }
        self.job = Some(LevelJob { cell, done: 0, peak: 0.0, acc: 0.0, acc_n: 0, best: 0.0, key: home_key(patch.kind), kind: patch.kind, dc: Default::default() });
    }

    /// Render the next chunk; `(cell, gain)` when the measurement is done.
    pub fn step(&mut self) -> Option<(usize, f32)> {
        let job = self.job.as_mut()?;
        let total = level_frames(job.kind);
        let n = LEVEL_CHUNK.min(total - job.done);
        self.buf.set_frames(n);
        self.buf.clear();
        let on = [Event { offset: 0, kind: EventKind::NoteOn { channel: 0, key: job.key, velocity: 0.8, note_id: -1 } }];
        let events: &[Event] = if job.done == 0 { &on } else { &[] };
        let tr = Transport::default();
        let mut ctx = ProcessContext { sample_rate: LEVEL_RATE, buffer: &mut self.buf, sidechain: None, events, transport: &tr };
        self.synth.process(&mut ctx);
        let (l, r) = (self.buf.channel(0), self.buf.channel(1));
        let pole = DcBlock::pole(LEVEL_RATE as f32);
        for i in 0..n {
            let (a, b) = (job.dc[0].process(l[i], pole), job.dc[1].process(r[i], pole));
            job.peak = job.peak.max(a.abs()).max(b.abs());
            job.acc += 0.5 * (f64::from(a) * f64::from(a) + f64::from(b) * f64::from(b));
            job.acc_n += 1;
            if job.acc_n == LEVEL_WIN {
                job.best = job.best.max(job.acc / LEVEL_WIN as f64);
                job.acc = 0.0;
                job.acc_n = 0;
            }
        }
        job.done += n;
        if job.done < total {
            return None;
        }
        let rms = job.best.sqrt() as f32;
        let peak = job.peak;
        let cell = job.cell;
        let (target, ceiling) = level_target(job.kind);
        self.job = None;
        let gain = if rms.is_finite() && rms > 1e-6 && peak.is_finite() { (target / rms).min(ceiling / peak.max(1e-6)).clamp(0.02, 80.0) } else { 1.0 };
        Some((cell, gain))
    }

    /// Measure `patch` at once (control thread, or a device's first load).
    pub fn measure(&mut self, patch: &Patch) -> f32 {
        self.start(0, patch);
        loop {
            if let Some((_, g)) = self.step() {
                return g;
            }
        }
    }
}

/// The level gain of one patch (allocates a synth: control thread).
pub fn measure_gain(patch: &Patch) -> f32 {
    Leveler::new().measure(patch)
}

#[cfg(test)]
mod tests;
