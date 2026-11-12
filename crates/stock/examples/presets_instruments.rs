//! Generates the factory presets of instrument.wavetable, instrument.sampler
//! and instrument.drum.
//! `cargo run -p aitrack-stock --example presets_instruments`
//!
//! Synth reference (see `instrument::wavetable`): tables 0 Saw, 1 Square,
//! 2 Pulse (position = width), 3 Sine→Saw, 4 Triangle, 5 Sine. Mod amounts:
//! 100 % = 24 st of pitch, 6 octaves of cutoff, the full table position or
//! +100 % amplitude; LFOs are bipolar.

use aitrack_stock::instrument::wavetable::modulation::src as msrc;
use aitrack_stock::instrument::wavetable::{self as wt, legacy_dst as dst, legacy_src as src, p, vdst};
use aitrack_stock::instrument::{drum, sampler};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

type B = PresetBuilder;

const SAW: f32 = 0.0;
const SQUARE: f32 = 1.0;
const PULSE: f32 = 2.0;
const SINESAW: f32 = 3.0;
const TRI: f32 = 4.0;
const SINE: f32 = 5.0;

const LP: f32 = 0.0;
const HP: f32 = 1.0;
const BP: f32 = 2.0;

// LFO shapes.
const L_SINE: f32 = 0.0;
const L_TRI: f32 = 1.0;
const L_SAW_UP: f32 = 2.0;
const L_SAW_DOWN: f32 = 3.0;
const L_SQUARE: f32 = 4.0;
const L_SMOOTH: f32 = 6.0;

// Sync divisions (`SYNC_DIVISIONS`).
const D_8BARS: f32 = 0.0;
const D_4BARS: f32 = 1.0;
const D_2BARS: f32 = 2.0;
const D_QUARTER: f32 = 7.0;
const D_EIGHTH: f32 = 10.0;
const D_EIGHTH_T: f32 = 12.0;
const D_SIXTEENTH: f32 = 13.0;

// Built-in delay times (`DELAY_TIMES`).
const T_16: f32 = 0.0;
const T_8: f32 = 2.0;
const T_8D: f32 = 3.0;
const T_4: f32 = 5.0;

const MONO: f32 = 1.0;
const LEGATO: f32 = 2.0;

fn syn(cat: &str, name: &str, tags: &[&str]) -> B {
    // Oscillator 2 defaults to +7 st; presets start it at unison pitch.
    B::new(wt::ID, cat, name).tags(tags).set(p::osc(1, p::SEMI), 0.0)
}

/// Oscillator `n`: table, position %, octave, level %.
fn osc(b: B, n: u32, table: f32, pos: f32, oct: f32, level: f32) -> B {
    b.set(p::osc(n, p::TABLE), table).set(p::osc(n, p::POS), pos).set(p::osc(n, p::OCTAVE), oct).set(p::osc(n, p::LEVEL), level)
}

/// Unison voices, detune %, stereo spread %.
fn uni(b: B, n: u32, voices: f32, detune: f32, spread: f32) -> B {
    b.set(p::osc(n, p::UNISON), voices).set(p::osc(n, p::DETUNE), detune).set(p::osc(n, p::SPREAD), spread)
}

fn tune(b: B, n: u32, semi: f32, fine: f32) -> B {
    b.set(p::osc(n, p::SEMI), semi).set(p::osc(n, p::FINE), fine)
}

/// Filter: type, 24 dB?, cutoff Hz, resonance %, drive %, env amount %.
fn filt(b: B, ty: f32, slope24: bool, cutoff: f32, reso: f32, drive: f32, env: f32) -> B {
    b.set(p::FILTER_TYPE, ty)
        .set(p::FILTER_SLOPE, if slope24 { 1.0 } else { 0.0 })
        .set(p::CUTOFF, cutoff)
        .set(p::RESONANCE, reso)
        .set(p::DRIVE, drive)
        .set(p::FILTER_ENV, env)
        .set(p::KEY_TRACK, 50.0)
}

fn amp(b: B, a: f32, d: f32, s: f32, r: f32) -> B {
    b.set(p::AMP_ATTACK, a).set(p::AMP_DECAY, d).set(p::AMP_SUSTAIN, s).set(p::AMP_RELEASE, r)
}

fn menv(b: B, a: f32, d: f32, s: f32, r: f32) -> B {
    b.set(p::MOD_ATTACK, a).set(p::MOD_DECAY, d).set(p::MOD_SUSTAIN, s).set(p::MOD_RELEASE, r)
}

/// Free-running LFO.
fn lfo(b: B, n: u32, shape: f32, hz: f32) -> B {
    b.set(p::lfo(n, p::LFO_SHAPE), shape).set(p::lfo(n, p::LFO_RATE), hz).set(p::lfo(n, p::LFO_SYNC), 0.0)
}

/// Tempo-synced, retriggered LFO.
fn lfo_sync(b: B, n: u32, shape: f32, div: f32) -> B {
    b.set(p::lfo(n, p::LFO_SHAPE), shape).set(p::lfo(n, p::LFO_SYNC), 1.0).set(p::lfo(n, p::LFO_DIVISION), div).set(p::lfo(n, p::LFO_RETRIGGER), 1.0)
}

fn slot(b: B, s: u32, source: f32, dest: f32, amount: f32) -> B {
    b.set(p::slot(s, p::MOD_SRC), source).set(p::slot(s, p::MOD_DST), dest).set(p::slot(s, p::MOD_AMT), amount)
}

fn mono(b: B, mode: f32, glide: f32) -> B {
    b.set(p::VOICE_MODE, mode).set(p::GLIDE, glide)
}

/// Built-in effects: chorus %, delay % with time and feedback, reverb % with decay.
fn fx(b: B, chorus: f32, delay: f32, time: f32, fb: f32, reverb: f32, decay: f32) -> B {
    b.set(p::CHORUS_MIX, chorus).set(p::DELAY_MIX, delay).set(p::DELAY_TIME, time).set(p::DELAY_FEEDBACK, fb).set(p::REVERB_MIX, reverb).set(p::REVERB_DECAY, decay)
}

fn vol(b: B, db: f32) -> B {
    b.set(p::VOLUME, db)
}

/// Supersaw: 7 voices, wide.
fn supersaw(b: B, n: u32, detune: f32, level: f32) -> B {
    uni(osc(b, n, SAW, 0.0, 0.0, level), n, 7.0, detune, 85.0)
}

/// Gentle tape-style wow on LFO 2 → pitch.
fn wow(b: B, s: u32) -> B {
    slot(lfo(b, 1, L_SMOOTH, 0.6), s, src::LFO2, dst::PITCH, 0.4)
}

fn bass() -> Vec<B> {
    vec![
        vol(mono(amp(osc(syn("Bass", "Sub Bass", &["sub", "edm", "trap", "house", "clean"]), 0, SINE, 0.0, 0.0, 90.0), 2.0, 300.0, 100.0, 80.0), LEGATO, 30.0), -3.0),
        // Decaying sine with a fast pitch drop and some filter drive: 808.
        vol(
            slot(
                menv(mono(amp(filt(osc(syn("Bass", "808 Boom", &["808", "trap", "reggaeton", "dembow", "hip hop"]), 0, SINE, 0.0, 0.0, 90.0), LP, false, 20_000.0, 0.0, 45.0, 0.0), 1.0, 1800.0, 0.0, 300.0), MONO, 0.0), 0.5, 60.0, 0.0, 60.0),
                0,
                src::MOD_ENV,
                dst::PITCH,
                12.0,
            ),
            -4.0,
        ),
        vol(
            slot(
                menv(mono(amp(filt(osc(syn("Bass", "808 Glide", &["808", "trap", "drill", "glide"]), 0, SINE, 0.0, 0.0, 90.0), LP, false, 20_000.0, 0.0, 35.0, 0.0), 1.0, 2500.0, 60.0, 250.0), LEGATO, 120.0), 0.5, 50.0, 0.0, 50.0),
                0,
                src::MOD_ENV,
                dst::PITCH,
                8.0,
            ),
            -4.0,
        ),
        mono(amp(filt(osc(osc(syn("Bass", "Reggaeton Bass", &["reggaeton", "dembow", "latin", "round"]), 0, SINE, 0.0, 0.0, 85.0), 1, TRI, 0.0, 1.0, 25.0), LP, false, 1200.0, 0.0, 30.0, 0.0), 1.0, 400.0, 60.0, 100.0), MONO, 0.0),
        mono(
            menv(amp(filt(osc(syn("Bass", "Dembow Punch", &["dembow", "reggaeton", "punchy", "latin"]), 0, SQUARE, 0.0, 0.0, 60.0), LP, true, 400.0, 20.0, 20.0, 40.0), 1.0, 250.0, 30.0, 60.0), 0.5, 150.0, 0.0, 100.0)
                .set(p::SUB_LEVEL, 70.0),
            MONO,
            0.0,
        ),
        slot(
            lfo(mono(filt(tune(uni(osc(uni(osc(syn("Bass", "Reese", &["reese", "dnb", "dubstep", "techno", "dark"]), 0, SAW, 0.0, 0.0, 80.0), 0, 3.0, 35.0, 40.0), 1, SAW, 0.0, 0.0, 70.0), 1, 2.0, 30.0, 40.0), 1, 0.0, 12.0), LP, true, 900.0, 15.0, 30.0, 0.0), MONO, 0.0), 0, L_SINE, 0.2),
            0,
            src::LFO1,
            dst::CUTOFF,
            10.0,
        ),
        slot(
            lfo_sync(mono(filt(osc(osc(syn("Bass", "Wobble 8th", &["wobble", "dubstep", "lfo", "aggressive"]), 0, SAW, 0.0, 0.0, 80.0), 1, SQUARE, 0.0, -1.0, 50.0), LP, true, 250.0, 45.0, 40.0, 0.0), MONO, 0.0), 0, L_SINE, D_EIGHTH),
            0,
            src::LFO1,
            dst::CUTOFF,
            45.0,
        ),
        slot(
            slot(lfo_sync(mono(filt(osc(osc(syn("Bass", "Riddim Triplet", &["riddim", "dubstep", "wobble", "heavy"]), 0, SINESAW, 50.0, 0.0, 80.0), 1, SQUARE, 0.0, -1.0, 40.0), LP, true, 300.0, 40.0, 60.0, 0.0), MONO, 0.0), 0, L_TRI, D_EIGHTH_T), 0, src::LFO1, dst::CUTOFF, 50.0),
            1,
            src::LFO1,
            dst::OSC1_POS,
            40.0,
        ),
        slot(
            slot(lfo_sync(mono(filt(osc(syn("Bass", "Growl", &["growl", "dubstep", "riddim", "aggressive"]), 0, SINESAW, 60.0, 0.0, 90.0), LP, true, 1200.0, 35.0, 70.0, 0.0), MONO, 0.0), 0, L_SAW_DOWN, D_SIXTEENTH), 0, src::LFO1, dst::OSC1_POS, 40.0),
            1,
            src::LFO1,
            dst::CUTOFF,
            25.0,
        ),
        mono(amp(filt(osc(osc(syn("Bass", "House Organ Bass", &["house", "deep house", "90s", "organ"]), 0, SQUARE, 0.0, 0.0, 70.0), 1, SINE, 0.0, 1.0, 40.0), LP, false, 2000.0, 0.0, 0.0, 0.0), 1.0, 200.0, 40.0, 80.0), MONO, 0.0),
        mono(menv(filt(osc(syn("Bass", "Acid Line", &["acid", "techno", "house", "squelch"]), 0, SAW, 0.0, 0.0, 85.0), LP, true, 300.0, 75.0, 30.0, 60.0), 0.5, 200.0, 0.0, 100.0), LEGATO, 60.0),
        fx(amp(filt(osc(syn("Bass", "Techno Rumble", &["techno", "rumble", "dark", "kick bass"]), 0, SINESAW, 30.0, 0.0, 85.0), LP, false, 200.0, 10.0, 20.0, 0.0), 1.0, 600.0, 0.0, 200.0), 0.0, 0.0, T_8, 30.0, 25.0, 3.0),
        menv(amp(filt(osc(syn("Bass", "Pluck Bass", &["house", "pop", "edm", "pluck"]), 0, SAW, 0.0, 0.0, 85.0), LP, true, 600.0, 15.0, 10.0, 50.0), 1.0, 300.0, 0.0, 100.0), 0.5, 250.0, 0.0, 100.0),
        filt(tune(osc(osc(syn("Bass", "Synthwave Bass", &["synthwave", "pop", "80s", "retro"]), 0, PULSE, 40.0, 0.0, 70.0), 1, SAW, 0.0, 0.0, 40.0), 1, 0.0, 8.0), LP, false, 1500.0, 10.0, 0.0, 30.0),
    ]
}

fn lead() -> Vec<B> {
    vec![
        fx(amp(filt(tune(supersaw(supersaw(syn("Lead", "Supersaw Lead", &["supersaw", "trance", "edm", "festival"]), 0, 45.0, 80.0), 1, 35.0, 60.0), 1, 0.0, 5.0), LP, false, 12_000.0, 0.0, 0.0, 0.0), 5.0, 300.0, 90.0, 300.0), 20.0, 15.0, T_8D, 35.0, 25.0, 2.5),
        fx(filt(osc(supersaw(syn("Lead", "Big Room Lead", &["big room", "edm", "festival", "bright"]), 0, 55.0, 80.0), 1, SQUARE, 0.0, 1.0, 35.0), LP, false, 9000.0, 0.0, 0.0, 0.0), 0.0, 10.0, T_8, 30.0, 30.0, 3.0),
        fx(menv(filt(uni(osc(syn("Lead", "Future Bass Lead", &["future bass", "edm", "bright", "wide"]), 0, SAW, 0.0, 0.0, 80.0), 0, 5.0, 40.0, 80.0), LP, false, 3000.0, 10.0, 0.0, 35.0), 5.0, 400.0, 30.0, 300.0), 15.0, 0.0, T_8, 30.0, 30.0, 2.5),
        fx(
            menv(amp(filt(tune(osc(osc(syn("Lead", "Pluck Lead", &["pluck", "house", "tropical", "pop"]), 0, SAW, 0.0, 0.0, 80.0), 1, SQUARE, 0.0, 0.0, 40.0), 1, 12.0, 0.0), LP, false, 800.0, 10.0, 0.0, 55.0), 1.0, 400.0, 30.0, 200.0), 0.5, 200.0, 10.0, 200.0),
            0.0,
            25.0,
            T_8D,
            35.0,
            15.0,
            2.0,
        ),
        slot(lfo(mono(osc(syn("Lead", "Square Lead", &["chiptune", "retro", "pop", "8-bit"]), 0, SQUARE, 0.0, 0.0, 70.0), LEGATO, 40.0), 0, L_SINE, 5.5), 0, src::LFO1, dst::PITCH, 1.0),
        mono(filt(tune(osc(osc(syn("Lead", "Saw Lead Mono", &["lead", "trance", "synthwave", "classic"]), 0, SAW, 0.0, 0.0, 80.0), 1, SAW, 0.0, 0.0, 70.0), 1, 0.0, -10.0), LP, false, 5000.0, 20.0, 0.0, 0.0), LEGATO, 80.0),
        slot(
            slot(lfo_sync(mono(filt(osc(osc(syn("Lead", "Dubstep Screech", &["dubstep", "aggressive", "screech", "riddim"]), 0, SINESAW, 80.0, 0.0, 80.0), 1, SQUARE, 0.0, 0.0, 50.0), LP, true, 1500.0, 60.0, 80.0, 0.0), MONO, 0.0), 0, L_SQUARE, D_SIXTEENTH), 0, src::LFO1, dst::CUTOFF, 30.0),
            1,
            src::LFO1,
            dst::OSC1_POS,
            30.0,
        ),
        fx(amp(osc(osc(syn("Lead", "Trap Bell", &["trap", "bell", "dark", "drill"]), 0, SINE, 0.0, 0.0, 80.0), 1, SINE, 0.0, 2.0, 30.0), 1.0, 800.0, 0.0, 600.0), 0.0, 15.0, T_8, 30.0, 30.0, 3.0),
        fx(amp(filt(tune(osc(osc(syn("Lead", "Reggaeton Lead", &["reggaeton", "latin pop", "pluck", "bright"]), 0, PULSE, 50.0, 0.0, 70.0), 1, SAW, 0.0, 0.0, 50.0), 1, 0.0, 7.0), LP, false, 3000.0, 10.0, 0.0, 30.0), 1.0, 300.0, 40.0, 200.0), 0.0, 20.0, T_8D, 30.0, 15.0, 1.8),
        fx(wow(filt(osc(osc(syn("Lead", "Lo-Fi Lead", &["lo-fi", "chill", "warm", "vintage"]), 0, TRI, 0.0, 0.0, 80.0), 1, SINE, 0.0, 1.0, 30.0), LP, false, 2500.0, 5.0, 0.0, 0.0).set(p::NOISE, 5.0), 0), 30.0, 0.0, T_8, 30.0, 15.0, 2.0),
        tune(uni(osc(uni(osc(syn("Lead", "Detuned Lead", &["edm", "house", "wide", "bright"]), 0, SAW, 0.0, 0.0, 70.0), 0, 3.0, 30.0, 60.0), 1, SAW, 0.0, 0.0, 70.0), 1, 3.0, 30.0, 60.0), 1, 0.0, -15.0),
        slot(mono(osc(syn("Lead", "Portamento Lead", &["lead", "synthwave", "glide", "expressive"]), 0, SINESAW, 50.0, 0.0, 80.0), LEGATO, 200.0), 0, src::MOD_WHEEL, dst::PITCH, 2.0),
    ]
}

fn pad() -> Vec<B> {
    vec![
        fx(amp(filt(tune(osc(uni(osc(syn("Pad", "Warm Pad", &["pad", "ambient", "lo-fi", "warm"]), 0, SAW, 0.0, 0.0, 70.0), 0, 5.0, 30.0, 70.0), 1, SAW, 0.0, 0.0, 50.0), 1, 0.0, 7.0), LP, false, 1800.0, 10.0, 0.0, 0.0), 800.0, 1000.0, 80.0, 1500.0), 40.0, 0.0, T_8, 30.0, 40.0, 4.0),
        fx(amp(supersaw(syn("Pad", "Supersaw Pad", &["supersaw", "trance", "edm", "future bass"]), 0, 40.0, 75.0), 600.0, 1000.0, 90.0, 2000.0), 20.0, 0.0, T_8, 30.0, 45.0, 4.5),
        slot(fx(lfo(amp(filt(osc(osc(syn("Pad", "Dark Pad", &["dark", "techno", "cinematic", "drone"]), 0, SINESAW, 40.0, 0.0, 70.0), 1, TRI, 0.0, -1.0, 50.0), LP, false, 900.0, 15.0, 10.0, 0.0), 1200.0, 1000.0, 90.0, 2500.0), 0, L_SINE, 0.1), 0.0, 0.0, T_8, 30.0, 50.0, 6.0), 0, src::LFO1, dst::CUTOFF, 15.0),
        slot(fx(lfo(amp(osc(osc(syn("Pad", "Glass Pad", &["glass", "ambient", "pop", "airy"]), 0, SINE, 0.0, 0.0, 70.0), 1, PULSE, 30.0, 1.0, 40.0), 500.0, 1000.0, 80.0, 2000.0), 1, L_TRI, 0.3), 30.0, 0.0, T_8, 30.0, 50.0, 5.0), 0, src::LFO2, dst::OSC2_POS, 30.0),
        slot(
            slot(fx(lfo(lfo(amp(osc(osc(syn("Pad", "Evolving Pad", &["evolving", "ambient", "cinematic", "texture"]), 0, SINESAW, 30.0, 0.0, 70.0), 1, PULSE, 20.0, 0.0, 50.0), 1500.0, 1000.0, 90.0, 3000.0), 0, L_TRI, 0.08), 1, L_SINE, 0.13), 20.0, 0.0, T_8, 30.0, 55.0, 8.0), 0, src::LFO1, dst::OSC1_POS, 60.0),
            1,
            src::LFO2,
            dst::OSC2_POS,
            40.0,
        ),
        fx(amp(filt(uni(osc(syn("Pad", "String Pad", &["strings", "pop", "orchestral", "lush"]), 0, SAW, 0.0, 0.0, 75.0), 0, 4.0, 25.0, 70.0), LP, false, 3500.0, 5.0, 0.0, 0.0), 400.0, 800.0, 85.0, 800.0), 50.0, 0.0, T_8, 30.0, 35.0, 3.0),
        fx(wow(amp(filt(osc(osc(syn("Pad", "Lo-Fi Pad", &["lo-fi", "chill", "vintage", "warm"]), 0, TRI, 0.0, 0.0, 70.0), 1, SAW, 0.0, 0.0, 25.0), LP, false, 1200.0, 5.0, 10.0, 0.0), 600.0, 800.0, 80.0, 1500.0).set(p::NOISE, 8.0), 0), 40.0, 0.0, T_8, 30.0, 30.0, 3.0),
        slot(fx(lfo_sync(amp(supersaw(syn("Pad", "Future Bass Pump Pad", &["future bass", "edm", "pump", "sidechain"]), 0, 40.0, 75.0), 50.0, 800.0, 90.0, 800.0), 0, L_SAW_UP, D_QUARTER), 20.0, 0.0, T_8, 30.0, 30.0, 3.0), 0, src::LFO1, dst::AMP, 45.0),
        fx(amp(filt(osc(syn("Pad", "Choir Pad", &["choir", "ambient", "vocal", "cinematic"]), 0, PULSE, 30.0, 0.0, 80.0), BP, false, 1200.0, 30.0, 0.0, 0.0), 700.0, 1000.0, 85.0, 2000.0), 60.0, 0.0, T_8, 30.0, 50.0, 5.0),
    ]
}

fn pluck() -> Vec<B> {
    vec![
        fx(menv(amp(filt(tune(osc(osc(syn("Pluck", "House Pluck", &["house", "pluck", "deep house", "bright"]), 0, SAW, 0.0, 0.0, 70.0), 1, SAW, 0.0, 0.0, 60.0), 1, 0.0, 8.0), LP, true, 600.0, 15.0, 0.0, 60.0), 1.0, 300.0, 0.0, 150.0), 0.5, 180.0, 0.0, 150.0), 0.0, 20.0, T_8D, 30.0, 20.0, 2.0),
        fx(amp(osc(osc(syn("Pluck", "Tropical Pluck", &["tropical", "pop", "house", "marimba"]), 0, SINESAW, 30.0, 0.0, 70.0), 1, SINE, 0.0, 1.0, 40.0), 1.0, 250.0, 0.0, 150.0), 0.0, 30.0, T_8, 35.0, 15.0, 2.0),
        fx(menv(amp(filt(uni(osc(syn("Pluck", "Future Bass Pluck", &["future bass", "edm", "wide", "pluck"]), 0, SAW, 0.0, 0.0, 75.0), 0, 5.0, 35.0, 80.0), LP, false, 1500.0, 10.0, 0.0, 45.0), 1.0, 350.0, 0.0, 300.0), 0.5, 250.0, 0.0, 250.0), 15.0, 15.0, T_8, 30.0, 30.0, 2.5),
        amp(osc(osc(syn("Pluck", "Marimba", &["marimba", "reggaeton", "pop", "tropical"]), 0, SINE, 0.0, 0.0, 80.0), 1, TRI, 0.0, 2.0, 30.0), 1.0, 400.0, 0.0, 300.0),
        fx(amp(osc(osc(syn("Pluck", "Bell Pluck", &["bell", "trap", "lo-fi", "dark"]), 0, SINE, 0.0, 0.0, 70.0), 1, TRI, 0.0, 2.0, 35.0), 1.0, 1200.0, 0.0, 800.0), 0.0, 15.0, T_8, 30.0, 35.0, 3.0),
        fx(menv(amp(filt(uni(osc(syn("Pluck", "Trance Pluck", &["trance", "edm", "pluck", "uplifting"]), 0, SAW, 0.0, 0.0, 80.0), 0, 3.0, 25.0, 60.0), LP, true, 400.0, 30.0, 0.0, 70.0), 1.0, 250.0, 0.0, 150.0), 0.5, 150.0, 0.0, 150.0), 0.0, 35.0, T_8D, 45.0, 25.0, 2.5),
        fx(wow(menv(amp(filt(osc(syn("Pluck", "Lo-Fi Pluck", &["lo-fi", "chill", "hip hop", "warm"]), 0, TRI, 0.0, 0.0, 80.0), LP, false, 2000.0, 5.0, 10.0, 20.0), 1.0, 500.0, 0.0, 300.0), 0.5, 300.0, 0.0, 200.0).set(p::NOISE, 6.0), 0), 30.0, 0.0, T_8, 30.0, 20.0, 2.0),
        menv(amp(filt(osc(osc(syn("Pluck", "Dembow Pluck", &["dembow", "reggaeton", "latin", "punchy"]), 0, SQUARE, 0.0, 0.0, 60.0), 1, PULSE, 60.0, 0.0, 50.0), LP, false, 2500.0, 10.0, 0.0, 30.0), 1.0, 180.0, 0.0, 80.0), 0.5, 120.0, 0.0, 80.0),
    ]
}

fn keys() -> Vec<B> {
    vec![
        slot(
            slot(fx(lfo(amp(filt(osc(osc(syn("Keys", "Electric Piano", &["keys", "lo-fi", "neo soul", "r&b"]), 0, SINE, 0.0, 0.0, 80.0), 1, TRI, 0.0, 1.0, 25.0), LP, false, 4000.0, 0.0, 0.0, 0.0), 2.0, 1500.0, 20.0, 400.0), 0, L_SINE, 4.0), 30.0, 0.0, T_8, 30.0, 15.0, 2.0), 0, src::LFO1, dst::AMP, 15.0),
            1,
            src::VELOCITY,
            dst::CUTOFF,
            20.0,
        ),
        slot(fx(lfo(amp(osc(osc(syn("Keys", "Organ", &["organ", "house", "gospel", "90s"]), 0, SQUARE, 0.0, 0.0, 60.0), 1, SINE, 0.0, 1.0, 60.0).set(p::SUB_LEVEL, 40.0), 5.0, 50.0, 100.0, 80.0), 0, L_SINE, 6.0), 35.0, 0.0, T_8, 30.0, 15.0, 1.5), 0, src::LFO1, dst::PITCH, 0.5),
        fx(menv(amp(filt(tune(osc(osc(syn("Keys", "House Piano Stab", &["house", "90s", "piano", "stab"]), 0, SAW, 0.0, 0.0, 70.0), 1, SQUARE, 0.0, 0.0, 40.0), 1, 12.0, 0.0), LP, false, 3000.0, 5.0, 0.0, 40.0), 1.0, 600.0, 10.0, 200.0), 0.5, 250.0, 0.0, 200.0), 0.0, 0.0, T_8, 30.0, 20.0, 2.0),
        filt(amp(tune(osc(osc(syn("Keys", "Synth Keys", &["pop", "synthwave", "80s", "keys"]), 0, PULSE, 40.0, 0.0, 70.0), 1, SAW, 0.0, 0.0, 50.0), 1, 0.0, 6.0), 2.0, 800.0, 50.0, 300.0), LP, false, 4000.0, 5.0, 0.0, 0.0),
        fx(wow(filt(amp(osc(osc(syn("Keys", "Lo-Fi Keys", &["lo-fi", "chill", "hip hop", "vintage"]), 0, TRI, 0.0, 0.0, 70.0), 1, SINE, 0.0, 1.0, 40.0), 2.0, 1200.0, 30.0, 400.0), LP, false, 1800.0, 0.0, 10.0, 0.0).set(p::NOISE, 5.0), 0), 40.0, 0.0, T_8, 30.0, 25.0, 2.0),
        amp(filt(osc(syn("Keys", "Clav", &["funk", "clav", "disco", "keys"]), 0, PULSE, 80.0, 0.0, 80.0), HP, false, 300.0, 15.0, 10.0, 0.0), 1.0, 250.0, 20.0, 100.0),
        fx(amp(osc(osc(syn("Keys", "Bright Bell Keys", &["bell", "pop", "bright", "christmas"]), 0, SINE, 0.0, 0.0, 80.0), 1, TRI, 0.0, 2.0, 40.0), 1.0, 2000.0, 0.0, 1000.0), 0.0, 0.0, T_8, 30.0, 30.0, 3.0),
    ]
}

fn arp() -> Vec<B> {
    vec![
        fx(menv(amp(filt(uni(osc(syn("Arp", "Trance Arp", &["trance", "arp", "edm", "uplifting"]), 0, SAW, 0.0, 0.0, 80.0), 0, 3.0, 20.0, 50.0), LP, true, 800.0, 35.0, 0.0, 50.0), 1.0, 200.0, 0.0, 100.0), 0.5, 120.0, 0.0, 100.0), 0.0, 35.0, T_8, 40.0, 20.0, 2.0),
        fx(filt(tune(osc(osc(syn("Arp", "Synthwave Arp", &["synthwave", "retro", "80s", "arp"]), 0, PULSE, 30.0, 0.0, 70.0), 1, SAW, 0.0, 0.0, 50.0), 1, 0.0, -8.0), LP, false, 2000.0, 15.0, 0.0, 30.0), 20.0, 30.0, T_8D, 35.0, 15.0, 2.0),
        slot(lfo_sync(filt(osc(syn("Arp", "Techno Sequence", &["techno", "sequence", "dark", "hypnotic"]), 0, SINESAW, 50.0, 0.0, 80.0), LP, true, 500.0, 50.0, 30.0, 40.0), 0, L_TRI, D_2BARS).set(p::lfo(0, p::LFO_RETRIGGER), 0.0), 0, src::LFO1, dst::CUTOFF, 25.0),
        amp(osc(syn("Arp", "Chiptune Arp", &["chiptune", "8-bit", "retro", "game"]), 0, SQUARE, 0.0, 0.0, 60.0), 1.0, 120.0, 60.0, 40.0),
        fx(slot(menv(amp(osc(osc(syn("Arp", "Bubble Arp", &["bubble", "edm", "pop", "playful"]), 0, SINE, 0.0, 0.0, 80.0), 1, SINE, 0.0, 1.0, 40.0), 1.0, 150.0, 0.0, 100.0), 0.5, 40.0, 0.0, 40.0), 0, src::MOD_ENV, dst::PITCH, 5.0), 0.0, 25.0, T_16, 35.0, 15.0, 1.5),
        fx(menv(amp(filt(osc(osc(syn("Arp", "House Arp", &["house", "arp", "pluck", "groovy"]), 0, SAW, 0.0, 0.0, 70.0), 1, SQUARE, 0.0, 0.0, 40.0), LP, false, 1000.0, 20.0, 0.0, 45.0), 1.0, 200.0, 0.0, 100.0), 0.5, 150.0, 0.0, 100.0), 0.0, 25.0, T_16, 30.0, 15.0, 2.0),
    ]
}

fn fx_presets() -> Vec<B> {
    vec![
        fx(
            slot(lfo_sync(amp(filt(osc(syn("FX", "White Noise Riser", &["riser", "edm", "fx", "build-up"]), 0, SAW, 0.0, 0.0, 0.0).set(p::NOISE, 80.0), HP, false, 200.0, 20.0, 0.0, 0.0), 4000.0, 1000.0, 100.0, 1000.0), 0, L_SAW_UP, D_8BARS), 0, src::LFO1, dst::CUTOFF, 60.0),
            0.0,
            0.0,
            T_8,
            30.0,
            40.0,
            4.0,
        ),
        fx(
            slot(lfo_sync(filt(osc(syn("FX", "Downlifter", &["downlifter", "fx", "transition", "edm"]), 0, SAW, 0.0, 0.0, 40.0).set(p::NOISE, 70.0), LP, false, 6000.0, 20.0, 0.0, 0.0), 0, L_SAW_DOWN, D_4BARS), 0, src::LFO1, dst::CUTOFF, 70.0),
            0.0,
            0.0,
            T_8,
            30.0,
            40.0,
            4.0,
        ),
        slot(menv(amp(osc(syn("FX", "Laser Zap", &["laser", "fx", "dubstep", "zap"]), 0, SINE, 0.0, 1.0, 80.0), 1.0, 200.0, 0.0, 100.0), 0.5, 120.0, 0.0, 100.0), 0, src::MOD_ENV, dst::PITCH, 100.0),
        fx(slot(lfo(osc(syn("FX", "Dub Siren", &["siren", "dancehall", "reggae", "fx"]), 0, SAW, 0.0, 0.0, 70.0), 0, L_SINE, 0.5), 0, src::LFO1, dst::PITCH, 20.0), 0.0, 30.0, T_8D, 50.0, 20.0, 2.0),
        fx(slot(menv(amp(osc(syn("FX", "Impact Boom", &["impact", "cinematic", "trailer", "fx"]), 0, SINE, 0.0, -1.0, 90.0).set(p::NOISE, 40.0), 1.0, 3000.0, 0.0, 1500.0), 0.5, 300.0, 0.0, 300.0), 0, src::MOD_ENV, dst::PITCH, 50.0), 0.0, 0.0, T_8, 30.0, 60.0, 6.0),
        fx(slot(lfo(amp(filt(osc(syn("FX", "Wind Atmosphere", &["ambient", "texture", "wind", "cinematic"]), 0, SAW, 0.0, 0.0, 0.0).set(p::NOISE, 60.0), BP, false, 800.0, 50.0, 0.0, 0.0), 1500.0, 1000.0, 100.0, 2500.0), 0, L_SMOOTH, 0.3), 0, src::LFO1, dst::CUTOFF, 30.0), 0.0, 0.0, T_8, 30.0, 50.0, 6.0),
    ]
}

fn chords() -> Vec<B> {
    vec![
        slot(fx(lfo_sync(osc(supersaw(syn("Chords", "Future Bass Chords", &["future bass", "edm", "supersaw", "pump"]), 0, 45.0, 75.0), 1, SAW, 0.0, 1.0, 40.0), 0, L_SAW_UP, D_QUARTER), 30.0, 0.0, T_8, 30.0, 30.0, 2.5), 0, src::LFO1, dst::AMP, 40.0),
        fx(menv(amp(filt(tune(osc(osc(syn("Chords", "House Stab", &["house", "deep house", "stab", "chords"]), 0, SAW, 0.0, 0.0, 70.0), 1, SQUARE, 0.0, 0.0, 40.0), 1, 12.0, 0.0), LP, false, 2500.0, 10.0, 0.0, 40.0), 1.0, 350.0, 0.0, 200.0), 0.5, 200.0, 0.0, 200.0), 0.0, 0.0, T_8, 30.0, 25.0, 2.0),
        fx(wow(filt(osc(osc(syn("Chords", "Lo-Fi Chords", &["lo-fi", "chill", "hip hop", "jazzy"]), 0, TRI, 0.0, 0.0, 70.0), 1, SAW, 0.0, 0.0, 30.0), LP, false, 1500.0, 5.0, 10.0, 0.0).set(p::NOISE, 6.0), 0), 40.0, 0.0, T_8, 30.0, 30.0, 2.5),
        fx(amp(filt(tune(osc(osc(syn("Chords", "Pop Chords", &["pop", "bright", "chords", "radio"]), 0, PULSE, 20.0, 0.0, 70.0), 1, SAW, 0.0, 0.0, 50.0), 1, 0.0, 6.0), LP, false, 5000.0, 5.0, 0.0, 0.0), 10.0, 800.0, 60.0, 400.0), 25.0, 0.0, T_8, 30.0, 20.0, 2.0),
        fx(amp(filt(osc(osc(syn("Chords", "Reggaeton Chords", &["reggaeton", "latin pop", "dembow", "chords"]), 0, PULSE, 50.0, 0.0, 70.0), 1, TRI, 0.0, 1.0, 40.0), LP, false, 3000.0, 5.0, 0.0, 0.0), 1.0, 400.0, 30.0, 250.0), 0.0, 20.0, T_8D, 30.0, 20.0, 2.0),
        slot(fx(lfo_sync(supersaw(syn("Chords", "Trance Gate Chords", &["trance", "gate", "edm", "rhythmic"]), 0, 40.0, 75.0), 0, L_SQUARE, D_SIXTEENTH), 0.0, 20.0, T_4, 30.0, 25.0, 2.5), 0, src::LFO1, dst::AMP, -60.0),
    ]
}

// ── Sound-design showcase (the newer engine: bank tables, warps, osc 3,
// filter 2, drawn / one-shot LFOs, the 32-slot matrix, macros, FX). ──

// Bank tables (`WAVE` values; see `wt::waves()`).
const W_BASIC: f32 = 1.0;
const W_HARMONIC: f32 = 2.0;
const W_FORMANT: f32 = 3.0;
const W_SYNC: f32 = 4.0;
const W_FM: f32 = 5.0;
const W_ORGAN: f32 = 6.0;
const W_DIGITAL: f32 = 7.0;
const W_FOLD: f32 = 8.0;
const W_PD: f32 = 9.0;
const W_COMB: f32 = 10.0;
const W_GROWL: f32 = 11.0;
const W_NOISE: f32 = 12.0;
const W_PWM: f32 = 13.0;
const W_GLASS: f32 = 14.0;

// Warps (`wt::WARPS`).
const WP_SYNC: f32 = 1.0;
const WP_BEND: f32 = 2.0;
const WP_ASYM: f32 = 4.0;
const WP_MIRROR: f32 = 5.0;
const WP_QUANT: f32 = 6.0;
const WP_FM_NEXT: f32 = 8.0;

// Filter models (`filter::MODELS`).
const M_LADDER: f32 = 1.0;
const M_NOTCH: f32 = 2.0;
const M_COMB: f32 = 3.0;
const M_FORMANT: f32 = 5.0;

/// Oscillator `n` on bank table `wave` at `pos` %, octave, level %.
fn wave(b: B, n: u32, wave: f32, pos: f32, oct: f32, level: f32) -> B {
    b.set(p::osc(n, p::WAVE), wave).set(p::osc(n, p::POS), pos).set(p::osc(n, p::OCTAVE), oct).set(p::osc(n, p::LEVEL), level)
}

fn warp(b: B, n: u32, mode: f32, amount: f32) -> B {
    b.set(p::osc(n, p::WARP), mode).set(p::osc(n, p::WARP_AMT), amount)
}

/// Unison up to 16 voices with blend %.
fn uni16(b: B, n: u32, voices: f32, detune: f32, spread: f32, blend: f32) -> B {
    b.set(p::osc(n, p::VOICES), voices).set(p::osc(n, p::DETUNE), detune).set(p::osc(n, p::SPREAD), spread).set(p::osc(n, p::BLEND), blend)
}

/// Filter `n`: model, cutoff, resonance, drive, env-2 depth (enables it).
fn filt_x(b: B, n: u32, model: f32, cutoff: f32, reso: f32, drive: f32, env: f32) -> B {
    b.set(p::filt(n, p::F_ENABLE), 1.0)
        .set(p::filt(n, p::F_MODEL), model)
        .set(p::filt(n, p::F_CUTOFF), cutoff)
        .set(p::filt(n, p::F_RESO), reso)
        .set(p::filt(n, p::F_DRIVE), drive)
        .set(p::filt(n, p::F_ENV), env)
        .set(p::filt(n, p::F_KEY), 40.0)
}

/// Matrix slot `s`: source → destination id, amount %.
fn mx(b: B, s: u32, source: u8, dest: u32, amount: f32) -> B {
    b.set(p::slot_x(s, p::M_SRC), source as f32).set(p::slot_x(s, p::M_DST), dest as f32).set(p::slot_x(s, p::M_AMT), amount)
}

fn mx_via(b: B, s: u32, source: u8, dest: u32, amount: f32, via: u8) -> B {
    mx(b, s, source, dest, amount).set(p::slot_x(s, p::M_VIA), via as f32)
}

/// Tempo-synced LFO `n` (free running), classic shape.
fn lfo_x(b: B, n: u32, shape: f32, div: f32) -> B {
    b.set(p::lfo(n, p::LFO_SHAPE), shape).set(p::lfo(n, p::LFO_SYNC), 1.0).set(p::lfo(n, p::LFO_DIVISION), div)
}

fn env3(b: B, a: f32, d: f32, s: f32, r: f32) -> B {
    b.set(p::env(2, p::E_ATTACK), a).set(p::env(2, p::E_DECAY), d).set(p::env(2, p::E_SUSTAIN), s).set(p::env(2, p::E_RELEASE), r)
}

fn dist(b: B, mode: f32, drive: f32, mix: f32) -> B {
    b.set(p::DIST_MODE, mode).set(p::DIST_DRIVE, drive).set(p::DIST_MIX, mix)
}

fn eq(b: B, low: f32, mid: f32, freq: f32, high: f32) -> B {
    b.set(p::EQ_LOW, low).set(p::EQ_MID, mid).set(p::EQ_MID_FREQ, freq).set(p::EQ_HIGH, high)
}

fn macros(b: B, m: [f32; 4]) -> B {
    (0..4u32).fold(b, |b, i| b.set(p::macro_knob(i), m[i as usize]))
}

fn sound_design() -> Vec<B> {
    let c = "Sound Design";
    vec![
        // Formant table swept by an LFO + formant filter: talking bass.
        vol(
            mx(
                mx(
                    macros(lfo_x(filt_x(mono(wave(syn(c, "Talking Growl", &["dubstep", "riddim", "growl", "bass", "wavetable"]), 0, W_GROWL, 30.0, -1.0, 85.0), MONO, 20.0), 0, M_FORMANT, 800.0, 40.0, 30.0, 0.0), 0, L_TRI, D_EIGHTH), [50.0, 0.0, 0.0, 0.0]),
                    0,
                    msrc::LFO1,
                    p::osc(0, p::POS),
                    35.0,
                ),
                1,
                msrc::MACRO1,
                p::CUTOFF,
                30.0,
            )
            .set(p::DIST_MODE, 1.0)
            .set(p::DIST_DRIVE, 45.0),
            -6.0,
        ),
        // Sync sweep driven by env 2: classic sync lead.
        fx(
            mx(menv(warp(uni16(wave(syn(c, "Sync Sweep Lead", &["sync", "lead", "edm", "wavetable"]), 0, W_BASIC, 66.0, 0.0, 80.0), 0, 3.0, 20.0, 60.0, 80.0), 0, WP_SYNC, 10.0), 1.0, 500.0, 20.0, 300.0), 0, msrc::ENV2, p::osc(0, p::WARP_AMT), 70.0),
            15.0,
            20.0,
            T_8D,
            30.0,
            20.0,
            2.0,
        ),
        // FM bell table with osc 2 phase-modulating it.
        fx(
            amp(warp(wave(wave(syn(c, "FM Glass Bell", &["bell", "fm", "glass", "ambient", "wavetable"]), 0, W_FM, 40.0, 0.0, 70.0), 1, W_GLASS, 20.0, 1.0, 20.0), 0, WP_FM_NEXT, 35.0), 1.0, 1800.0, 0.0, 1200.0),
            0.0,
            20.0,
            T_8D,
            30.0,
            35.0,
            3.5,
        ),
        // 16-voice harmonic sweep pad, slow LFO on position, macro = brightness.
        fx(
            mx(
                mx(
                    amp(lfo(uni16(wave(syn(c, "Harmonic Bloom Pad", &["pad", "ambient", "cinematic", "supersaw", "wavetable"]), 0, W_HARMONIC, 20.0, 0.0, 70.0), 0, 16.0, 30.0, 100.0, 70.0), 0, L_SMOOTH, 0.15), 1200.0, 1500.0, 80.0, 2500.0),
                    0,
                    msrc::LFO1,
                    p::osc(0, p::POS),
                    25.0,
                ),
                1,
                msrc::MACRO1,
                vdst::POS,
                60.0,
            ),
            35.0,
            15.0,
            T_4,
            40.0,
            45.0,
            6.0,
        ),
        // Digital steps + quantize warp, bitcrush: chip / glitch.
        dist(amp(warp(wave(syn(c, "Bit Glitch Lead", &["glitch", "chiptune", "digital", "lead"]), 0, W_DIGITAL, 50.0, 0.0, 75.0), 0, WP_QUANT, 40.0), 1.0, 400.0, 60.0, 120.0), 5.0, 40.0, 60.0),
        // Comb filter tuned by key on noise + organ table: plucked metal.
        amp(menv(filt_x(wave(syn(c, "Comb Metal Pluck", &["pluck", "metallic", "comb", "percussive"]), 0, W_ORGAN, 50.0, 0.0, 60.0).set(p::NOISE, 30.0), 0, M_COMB, 440.0, 85.0, 0.0, 20.0).set(p::filt(0, p::F_KEY), 100.0), 1.0, 200.0, 0.0, 200.0), 1.0, 900.0, 0.0, 600.0),
        // Ladder bass with osc 3 sub layer and mirror warp; macro 1 = filter, macro 2 = drive.
        vol(
            mx(
                mx(
                    macros(
                        mono(menv(filt_x(warp(wave(wave(syn(c, "Ladder Reese", &["reese", "dnb", "neuro", "bass"]), 0, W_PWM, 40.0, -1.0, 70.0), 2, W_BASIC, 0.0, -2.0, 50.0), 0, WP_MIRROR, 40.0), 0, M_LADDER, 400.0, 45.0, 40.0, 30.0), 1.0, 300.0, 30.0, 200.0), MONO, 15.0),
                        [40.0, 20.0, 0.0, 0.0],
                    ),
                    0,
                    msrc::MACRO1,
                    p::CUTOFF,
                    40.0,
                ),
                1,
                msrc::MACRO2,
                p::DRIVE,
                60.0,
            )
            .set(p::osc(0, p::VOICES), 2.0)
            .set(p::osc(0, p::DETUNE), 35.0),
            -5.0,
        ),
        // Phase-distortion keys with a drawn one-shot LFO "pluck" on position.
        fx(
            mx(
                amp(wave(syn(c, "PD Keys", &["keys", "casio", "phase distortion", "retro"]), 0, W_PD, 0.0, 0.0, 75.0), 2.0, 900.0, 40.0, 400.0)
                    .set(p::lfo(2, p::LFO_DRAWN), 0.0)
                    .set(p::lfo(2, p::LFO_SHAPE), L_SAW_DOWN)
                    .set(p::lfo(2, p::LFO_ONESHOT), 1.0)
                    .set(p::lfo(2, p::LFO_SYNC), 1.0)
                    .set(p::lfo(2, p::LFO_DIVISION), D_QUARTER),
                0,
                msrc::LFO3,
                p::osc(0, p::POS),
                70.0,
            )
            .set(p::slot_x(0, p::M_MODE), 1.0),
            20.0,
            0.0,
            T_8,
            30.0,
            20.0,
            2.0,
        ),
        // Noise-spectra pad through a notch + filter 2 band-pass, parallel.
        fx(
            amp(filt_x(filt_x(wave(syn(c, "Spectral Mist", &["texture", "ambient", "noise", "cinematic"]), 0, W_NOISE, 50.0, 0.0, 60.0), 0, M_NOTCH, 1200.0, 30.0, 0.0, 0.0), 1, 0.0, 3000.0, 30.0, 0.0, 0.0).set(p::filt(1, p::F_TYPE), BP).set(p::FILTER_ROUTING, 1.0).set(p::osc(0, p::ROUTE), 2.0), 1500.0, 2000.0, 80.0, 3000.0),
            30.0,
            0.0,
            T_8,
            30.0,
            60.0,
            8.0,
        ),
        // Folded sine bass: env 3 drives the fold position; velocity via mod wheel.
        vol(
            mx_via(mx(env3(mono(wave(syn(c, "Fold Bass", &["bass", "fold", "west coast", "techno"]), 0, W_FOLD, 10.0, -1.0, 85.0), MONO, 0.0), 1.0, 350.0, 20.0, 150.0), 0, msrc::ENV3, p::osc(0, p::POS), 60.0), 1, msrc::VELOCITY, p::osc(0, p::POS), 30.0, msrc::MOD_WHEEL),
            -5.0,
        ),
        // The sync-sweep table scanned by env 2: hard-sync stab.
        fx(mx(menv(amp(wave(syn(c, "Sync Table Stab", &["stab", "sync", "house", "wavetable"]), 0, W_SYNC, 0.0, 0.0, 75.0), 1.0, 350.0, 0.0, 200.0), 0.5, 250.0, 0.0, 200.0), 0, msrc::ENV2, p::osc(0, p::POS), 80.0), 0.0, 20.0, T_8, 30.0, 20.0, 2.0),
        // Asym PWM warp on the comb table, chorus + EQ: wide keys.
        eq(fx(warp(uni16(wave(syn(c, "Asym Comb Keys", &["keys", "wide", "pop", "wavetable"]), 0, W_COMB, 30.0, 0.0, 70.0), 0, 4.0, 15.0, 70.0, 60.0), 0, WP_ASYM, 50.0), 40.0, 0.0, T_8, 30.0, 20.0, 2.0), 2.0, -3.0, 600.0, 3.0),
        // Formant table, bend warp, vibrato from the mod wheel (via).
        fx(
            mx_via(lfo(warp(wave(syn(c, "Vox Lead", &["vocal", "formant", "lead", "expressive"]), 0, W_FORMANT, 50.0, 0.0, 80.0), 0, WP_BEND, 20.0), 1, L_SINE, 5.5), 0, msrc::LFO2, vdst::PITCH, 2.0, msrc::MOD_WHEEL),
            10.0,
            25.0,
            T_8D,
            30.0,
            25.0,
            2.5,
        ),
    ]
}

fn sampler_presets() -> Vec<B> {
    use sampler::p;
    let b = |cat: &str, name: &str, tags: &[&str]| B::new(sampler::ID, cat, name).tags(tags);
    vec![
        b("Init", "Classic Init", &["sampler", "init"]),
        b("Init", "One-Shot", &["sampler", "one-shot", "drums"]).set(p::MODE, 1.0).set(p::RELEASE, 50.0),
        b("Init", "Sustained Loop", &["sampler", "loop", "pads"]).set(p::MODE, 2.0).set(p::CROSSFADE, 50.0).set(p::ATTACK, 30.0).set(p::RELEASE, 600.0),
        b("Character", "Lo-Fi Sampler", &["sampler", "lo-fi", "warm"]).set(p::FILTER_TYPE, 1.0).set(p::CUTOFF, 3000.0).set(p::RESONANCE, 10.0).set(p::RELEASE, 300.0),
        b("Character", "Vocal Chop", &["sampler", "vocal", "chop", "future bass"]).set(p::MODE, 0.0).set(p::ATTACK, 2.0).set(p::DECAY, 200.0).set(p::SUSTAIN, 60.0).set(p::RELEASE, 80.0).set(p::GLIDE, 40.0),
        b("Character", "Pitched 808", &["sampler", "808", "trap", "glide"]).set(p::MODE, 1.0).set(p::POLYPHONY, 1.0).set(p::GLIDE, 80.0),
    ]
}

fn drum_presets() -> Vec<B> {
    use drum::p;
    let b = |cat: &str, name: &str, tags: &[&str]| B::new(drum::ID, cat, name).tags(tags);
    // Pads: 0 Kick, 1 Rim, 2 Snare, 3 Clap, 4 Snare 2, 5 Low Tom, 6 Closed
    // Hat, 7 Mid Tom, 8 Pedal Hat, 9 High Tom, 10 Open Hat, 11 Shaker,
    // 12 Crash, 13 Perc, 14 Cowbell, 15 808.
    let pad = |b: B, i: u32, tune: f32, decay: f32, level: f32| b.set(p::pad(i, p::TUNE), tune).set(p::pad(i, p::DECAY), decay).set(p::pad(i, p::LEVEL), level);
    vec![
        b("Kits", "Default Kit", &["drums", "kit", "init"]),
        pad(pad(pad(b("Kits", "Trap Kit", &["drums", "trap", "808", "hip hop"]), 15, -2.0, 10_000.0, 2.0), 6, 2.0, 60.0, -3.0), 3, 0.0, 400.0, 0.0).set(p::pad(15, p::CHOKE), 2.0).set(p::pad(0, p::CHOKE), 2.0),
        pad(pad(pad(b("Kits", "Reggaeton Kit", &["drums", "reggaeton", "dembow", "latin"]), 0, -1.0, 400.0, 0.0), 1, 3.0, 10_000.0, -2.0), 2, 2.0, 250.0, 0.0).set(p::VEL_CURVE, 1.0),
        pad(pad(b("Kits", "House Kit", &["drums", "house", "techno", "four on the floor"]), 0, -2.0, 350.0, 1.0), 10, -1.0, 300.0, -4.0).set(p::pad(3, p::PAN), 10.0).set(p::pad(11, p::PAN), -25.0),
        pad(pad(pad(b("Kits", "Lo-Fi Kit", &["drums", "lo-fi", "hip hop", "dusty"]), 0, -3.0, 300.0, 0.0), 2, -3.0, 200.0, 0.0), 6, -4.0, 10_000.0, -4.0)
            .set(p::pad(0, p::CUTOFF), 4000.0)
            .set(p::pad(2, p::CUTOFF), 5000.0)
            .set(p::pad(6, p::CUTOFF), 6000.0)
            .set(p::VEL_CURVE, 1.0),
        pad(b("Kits", "Tight Techno Kit", &["drums", "techno", "tight", "industrial"]), 0, -4.0, 250.0, 2.0).set(p::pad(6, p::DECAY), 40.0).set(p::VEL_SENS, 30.0),
    ]
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let groups = [bass(), lead(), pad(), pluck(), keys(), arp(), fx_presets(), chords(), sound_design(), sampler_presets(), drum_presets()];
    let mut n = 0;
    for group in groups {
        for pr in group {
            let path = pr.write(&root)?;
            println!("{}", path.display());
            n += 1;
        }
    }
    eprintln!("{n} presets");
    Ok(())
}
