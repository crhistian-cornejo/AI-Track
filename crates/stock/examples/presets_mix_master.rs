//! The everyday mixing, mastering, reverb and delay presets for fx.reverb,
//! fx.delay, fx.eq, fx.compressor, fx.buscomp, fx.limiter and fx.mbdyn.
//!
//! Every preset is built from plain values (Hz, dB, ms, choice index), then
//! level-matched on the part of the mix its tags name (see
//! `aitrack_stock::test_util::level_match`) so that loading it does not move
//! the level: compare the sound, not the loudness. `tests/preset_levels.rs`
//! is the check.
//! `cargo run -p aitrack-stock --example presets_mix_master`

// Preset helpers take one plain value per audible parameter.
#![allow(clippy::too_many_arguments)]

use aitrack_stock::fx::{buscomp, compressor, delay, eq, limiter, mbdyn, reverb};
use aitrack_stock::presets::{Preset, PresetBuilder, factory_dir, write_preset};
use aitrack_stock::test_util::{BLOCK, SR, level_match, level_vs_bypass, source_for_tags, trim_param};
use eq::{ch, dir, phase, slope, ty};

type B = PresetBuilder;

// ── EQ ──────────────────────────────────────────────────────────────────

/// One EQ band: `Bell(f, gain, q)`, `LowShelf`, `HighShelf`, `LowCut(f,
/// slope)`, `HighCut`, `Tilt(f, gain)`.
#[derive(Clone, Copy)]
enum Bd {
    Bell(f32, f32, f32),
    Ls(f32, f32, f32),
    Hs(f32, f32, f32),
    Lc(f32, f32),
    Hc(f32, f32),
    Tilt(f32, f32),
}

fn eq_preset(cat: &str, name: &str, tags: &[&str], bands: &[Bd]) -> B {
    let mut b = PresetBuilder::new(eq::ID, cat, name).tags(&["eq"]).tags(tags);
    for (i, band) in bands.iter().enumerate() {
        let (t, f, g, q, s) = match *band {
            Bd::Bell(f, g, q) => (ty::BELL, f, g, q, slope::DB12),
            Bd::Ls(f, g, q) => (ty::LOW_SHELF, f, g, q, slope::DB12),
            Bd::Hs(f, g, q) => (ty::HIGH_SHELF, f, g, q, slope::DB12),
            Bd::Lc(f, s) => (ty::LOW_CUT, f, 0.0, 0.707, s),
            Bd::Hc(f, s) => (ty::HIGH_CUT, f, 0.0, 0.707, s),
            Bd::Tilt(f, g) => (ty::TILT, f, g, 0.707, slope::DB12),
        };
        b = b
            .set(eq::p::band(i, eq::p::ON), 1.0)
            .set(eq::p::band(i, eq::p::TYPE), t)
            .set(eq::p::band(i, eq::p::FREQ), f)
            .set(eq::p::band(i, eq::p::GAIN), g)
            .set(eq::p::band(i, eq::p::Q), q)
            .set(eq::p::band(i, eq::p::SLOPE), s);
    }
    b
}

fn on_channel(b: B, i: usize, c: f32) -> B {
    b.set(eq::p::band(i, eq::p::CHANNEL), c)
}

/// Make band `i` dynamic: it only acts when the level at its frequency
/// crosses `thr`; `direction` and `range` say which way and how far.
fn dynamic(b: B, i: usize, thr: f32, ratio: f32, att: f32, rel: f32, direction: f32, range: f32) -> B {
    b.set(eq::p::band(i, eq::p::DYNAMIC), 1.0)
        .set(eq::p::band(i, eq::p::THRESHOLD), thr)
        .set(eq::p::band(i, eq::p::RATIO), ratio)
        .set(eq::p::band(i, eq::p::ATTACK), att)
        .set(eq::p::band(i, eq::p::RELEASE), rel)
        .set(eq::dynx::band(i, eq::dynx::MODE), direction)
        .set(eq::dynx::band(i, eq::dynx::RANGE), range)
}

const DB18: f32 = slope::DB18;
const DB24: f32 = slope::DB24;
const DB12: f32 = slope::DB12;
const DB48: f32 = slope::DB48;

fn eq_presets() -> Vec<B> {
    use Bd::*;
    vec![
        // ── Mastering: output-stage curves, all small moves ──
        eq_preset("Mastering", "Master Rumble Cut", &["master", "utility", "transparent"], &[Lc(22.0, DB24)]),
        eq_preset("Mastering", "Master Low-End Control", &["master", "low-end", "tight"], &[Lc(25.0, DB24), Bell(55.0, -1.0, 1.0), Bell(220.0, -1.2, 0.8)]),
        eq_preset("Mastering", "Master Air Lift", &["master", "air", "bright"], &[Hs(11_000.0, 2.0, 0.6), Bell(3_500.0, -0.5, 0.8)]),
        eq_preset("Mastering", "Master Presence Lift", &["master", "presence", "pop"], &[Bell(3_200.0, 1.0, 0.7), Hs(12_000.0, 1.0, 0.6)]),
        eq_preset("Mastering", "Master Smile Curve", &["master", "loud", "edm"], &[Ls(90.0, 1.5, 0.7), Bell(500.0, -1.0, 0.7), Hs(10_000.0, 1.5, 0.7)]),
        eq_preset("Mastering", "Master Tilt Warmer", &["master", "tilt", "warm"], &[Tilt(1_000.0, -1.5)]),
        eq_preset("Mastering", "Master Tilt Brighter", &["master", "tilt", "bright"], &[Tilt(1_000.0, 1.5)]),
        on_channel(eq_preset("Mastering", "Master Mono Below 120 Hz", &["master", "mono", "mid-side", "house"], &[Lc(120.0, DB24)]), 0, ch::SIDE),
        dynamic(eq_preset("Mastering", "Master Harshness Control", &["master", "dynamic", "smooth", "harsh"], &[Lc(22.0, DB12), Bell(3_200.0, 0.0, 3.0)]), 1, -30.0, 2.0, 5.0, 80.0, dir::CUT_ABOVE, 3.0),
        eq_preset("Mastering", "Master Linear Polish", &["master", "linear-phase", "transparent"], &[Lc(20.0, DB12), Bell(300.0, -0.6, 0.8), Hs(12_000.0, 1.0, 0.6)]).set(eq::p::PHASE, phase::LINEAR),
        // ── Mixing: buses ──
        eq_preset("Mixing", "Mix Bus Sweetener", &["mix bus", "glue", "master"], &[Lc(25.0, DB12), Ls(100.0, 0.8, 0.7), Bell(350.0, -0.8, 0.9), Hs(12_000.0, 1.0, 0.7)]),
        eq_preset("Mixing", "Drum Bus Body and Air", &["drums", "bus", "punchy"], &[Ls(70.0, 2.0, 0.7), Bell(400.0, -2.0, 1.0), Hs(10_000.0, 2.0, 0.7)]),
        eq_preset("Mixing", "Boxy Cut", &["cleanup", "mixing", "box"], &[Bell(450.0, -3.0, 1.5)]),
        eq_preset("Mixing", "Harsh Cut 3 kHz", &["cleanup", "mixing", "harsh", "smooth"], &[Bell(3_000.0, -2.5, 2.0)]),
        // ── Vocals ──
        eq_preset("Vocals", "Lead Vocal Clarity", &["vocal", "lead", "pop"], &[Lc(100.0, DB18), Bell(250.0, -2.0, 1.0), Bell(3_000.0, 1.5, 1.0), Hs(10_000.0, 2.0, 0.7)]),
        eq_preset("Vocals", "Male Vocal Body", &["vocal", "male", "warm"], &[Lc(70.0, DB18), Ls(120.0, 1.5, 0.7), Bell(400.0, -1.5, 1.2), Bell(3_500.0, 2.0, 1.0)]),
        eq_preset("Vocals", "Female Vocal Sheen", &["vocal", "female", "bright"], &[Lc(120.0, DB18), Bell(300.0, -1.5, 1.0), Bell(5_000.0, 2.0, 1.0), Hs(12_000.0, 3.0, 0.6)]),
        eq_preset("Vocals", "Backing Vocal Pocket", &["vocal", "backing", "pocket"], &[Lc(160.0, DB24), Bell(2_500.0, -3.0, 1.2), Hs(9_000.0, -2.0, 0.7)]),
        eq_preset("Vocals", "Rap Vocal Presence", &["vocal", "rap", "trap"], &[Lc(90.0, DB18), Bell(200.0, -1.5, 1.0), Bell(2_000.0, 1.5, 1.0), Bell(5_500.0, 2.5, 1.0)]),
        // ── Drums ──
        eq_preset("Drums", "Kick Deep Sub", &["kick", "sub", "house"], &[Lc(24.0, DB24), Bell(55.0, 3.5, 1.2), Bell(300.0, -3.0, 1.4), Bell(3_500.0, 1.5, 1.0)]),
        eq_preset("Drums", "Kick Click Top", &["kick", "click", "techno"], &[Lc(30.0, DB24), Bell(60.0, 1.0, 1.0), Bell(350.0, -3.0, 1.4), Bell(4_500.0, 3.0, 1.2)]),
        eq_preset("Drums", "Snare Fat Body", &["snare", "fat", "pop"], &[Lc(70.0, DB12), Bell(180.0, 3.0, 1.2), Bell(500.0, -2.0, 1.5), Bell(4_000.0, 2.0, 1.0)]),
        eq_preset("Drums", "Hi-Hat Air", &["hi-hat", "bright", "house"], &[Lc(500.0, DB24), Bell(8_000.0, 1.5, 1.0), Hs(11_000.0, 3.0, 0.7)]),
        eq_preset("Drums", "Overhead Gloss", &["overheads", "gloss", "acoustic"], &[Lc(200.0, DB12), Bell(400.0, -2.0, 0.8), Hs(10_000.0, 2.5, 0.7)]),
        // ── Bass ──
        eq_preset("Bass", "Bass Sub Weight", &["bass", "sub", "edm"], &[Lc(28.0, DB24), Bell(55.0, 3.0, 1.0), Bell(250.0, -2.0, 1.2), Hc(6_000.0, DB12)]),
        eq_preset("Bass", "Bass Growl Mids", &["bass", "growl", "dubstep"], &[Lc(30.0, DB24), Bell(150.0, -2.0, 1.0), Bell(800.0, 3.0, 1.5), Bell(2_200.0, 2.0, 1.5)]),
        eq_preset("Bass", "Sub Bass Clean", &["bass", "sub", "clean"], &[Lc(25.0, DB48), Bell(110.0, -2.0, 1.0), Hc(3_000.0, DB24)]),
        // ── Synths ──
        eq_preset("Synths", "Lead Synth Presence", &["synth", "lead", "edm"], &[Lc(150.0, DB18), Bell(2_500.0, 2.0, 1.0), Hs(10_000.0, 1.5, 0.7)]),
        eq_preset("Synths", "Pluck Clarity", &["synth", "pluck", "future bass"], &[Lc(200.0, DB18), Bell(400.0, -2.0, 1.2), Bell(4_000.0, 2.0, 1.2)]),
        dynamic(eq_preset("Synths", "Supersaw Smoother", &["synth", "supersaw", "dynamic", "smooth"], &[Lc(150.0, DB18), Bell(3_000.0, 0.0, 2.0), Hc(14_000.0, DB12)]), 1, -28.0, 2.5, 5.0, 80.0, dir::CUT_ABOVE, 4.0),
        eq_preset("Synths", "Pad Wide Air", &["pad", "synth", "air"], &[Lc(180.0, DB18), Bell(350.0, -2.0, 1.0), Hs(11_000.0, 3.0, 0.6)]),
    ]
}

// ── Compressor ──────────────────────────────────────────────────────────

fn comp(cat: &str, name: &str, tags: &[&str], style: f32, thr: f32, ratio: f32, att: f32, rel: f32, knee: f32) -> B {
    use compressor::p;
    PresetBuilder::new(compressor::ID, cat, name)
        .tags(&["compressor"])
        .tags(tags)
        .set(p::STYLE, style)
        .set(p::THRESHOLD, thr)
        .set(p::RATIO, ratio)
        .set(p::ATTACK, att)
        .set(p::RELEASE, rel)
        .set(p::KNEE, knee)
}

fn compressor_presets() -> Vec<B> {
    use compressor::{model, p, style};
    vec![
        // Mastering: ratios of 1.5 – 2, slow attacks, 1 – 2 dB of reduction.
        comp("Mastering", "Master Glue 1.5-1", &["master", "glue", "transparent"], style::GLUE, -10.0, 1.5, 30.0, 200.0, 12.0).set(p::AUTO_RELEASE, 1.0).set(p::SC_HPF, 80.0).set(p::RMS, 50.0),
        comp("Mastering", "Master Gentle 2-1 Slow", &["master", "glue", "slow"], style::GLUE, -12.0, 2.0, 40.0, 400.0, 10.0).set(p::AUTO_RELEASE, 1.0).set(p::SC_HPF, 90.0).set(p::MODEL, model::VCA),
        comp("Mastering", "Master Opto Silk", &["master", "opto", "smooth"], style::OPTO, -14.0, 2.0, 15.0, 250.0, 12.0),
        comp("Mastering", "Master Low-End Tamer", &["master", "low-end", "control"], style::CLEAN, -16.0, 2.0, 20.0, 150.0, 8.0).set(p::SC_EQ_FREQ, 100.0).set(p::SC_EQ_GAIN, 6.0),
        comp("Mastering", "Master Transparent Leveler", &["master", "transparent", "leveler"], style::CLEAN, -16.0, 1.8, 50.0, 600.0, 18.0).set(p::LOOKAHEAD, 3.0),
        // Buses.
        comp("Bus", "Mix Bus Glue 2-1", &["mix bus", "glue", "pop", "edm"], style::GLUE, -14.0, 2.0, 30.0, 250.0, 10.0).set(p::AUTO_RELEASE, 1.0).set(p::SC_HPF, 90.0),
        comp("Bus", "Parallel Bus Lift", &["bus", "parallel", "density"], style::CLEAN, -30.0, 8.0, 5.0, 120.0, 6.0).set(p::MIX, 40.0),
        comp("Bus", "Synth Bus Control", &["synth", "bus", "control"], style::CLEAN, -20.0, 3.0, 20.0, 180.0, 8.0),
        // Vocals.
        comp("Vocals", "Lead Vocal 4-1 Smooth", &["vocal", "lead", "smooth"], style::OPTO, -22.0, 4.0, 8.0, 180.0, 8.0),
        comp("Vocals", "Lead Vocal Fast Peaks", &["vocal", "lead", "peaks"], style::PUNCH, -18.0, 6.0, 1.0, 60.0, 2.0).set(p::LOOKAHEAD, 1.0),
        comp("Vocals", "Backing Vocal Squeeze", &["vocal", "backing", "dense"], style::CLEAN, -28.0, 8.0, 3.0, 100.0, 4.0),
        comp("Vocals", "Rap Vocal Control", &["vocal", "rap", "trap"], style::CLEAN, -22.0, 4.0, 4.0, 90.0, 6.0).set(p::SC_HPF, 120.0),
        comp("Vocals", "Vocal Parallel Density", &["vocal", "parallel", "dense"], style::CLEAN, -35.0, 10.0, 2.0, 150.0, 6.0).set(p::MIX, 35.0),
        // Drums.
        comp("Drums", "Kick Tight 4-1", &["kick", "tight", "house"], style::PUNCH, -18.0, 4.0, 10.0, 70.0, 3.0),
        comp("Drums", "Snare Body 3-1", &["snare", "body", "pop"], style::CLEAN, -20.0, 3.0, 15.0, 90.0, 6.0),
        comp("Drums", "Drum Bus 4-1 Punch", &["drums", "bus", "punchy"], style::PUNCH, -16.0, 4.0, 15.0, 100.0, 4.0),
        comp("Drums", "Drum Room Squash", &["drums", "room", "parallel"], style::CLEAN, -32.0, 10.0, 1.0, 120.0, 6.0).set(p::MIX, 45.0),
        comp("Drums", "Hi-Hat Smoother", &["hi-hat", "smooth", "house"], style::OPTO, -26.0, 3.0, 2.0, 80.0, 8.0),
        comp("Drums", "Overheads Glue", &["overheads", "glue", "acoustic"], style::GLUE, -22.0, 2.0, 25.0, 200.0, 8.0),
        // Bass.
        comp("Bass", "Bass Guitar Even", &["bass", "guitar", "even"], style::OPTO, -22.0, 4.0, 20.0, 150.0, 8.0),
        comp("Bass", "Synth Bass Steady", &["bass", "synth", "steady"], style::CLEAN, -20.0, 5.0, 8.0, 120.0, 4.0),
        comp("Bass", "Sub Bass Leveler", &["bass", "sub", "leveler"], style::CLEAN, -24.0, 3.0, 25.0, 200.0, 6.0).set(p::RMS, 60.0),
        // Instruments.
        comp("Instruments", "Piano Even", &["piano", "keys", "even"], style::OPTO, -24.0, 2.5, 18.0, 220.0, 12.0),
        comp("Instruments", "Pluck Control", &["pluck", "synth", "control"], style::PUNCH, -20.0, 3.0, 12.0, 90.0, 4.0),
    ]
}

// ── Bus compressor ──────────────────────────────────────────────────────

/// `ratio`: 0 = 2:1, 1 = 4:1, 2 = 10:1. `attack`: 0..6 = 0.01, 0.1, 0.3, 1,
/// 3, 10, 30 ms. `release`: 0..6 = 0.1, 0.2, 0.4, 0.6, 0.8, 1.2 s, Auto.
/// `hpf`: 0..6 = off, 30, 60, 90, 120, 180, 240 Hz.
fn bus(cat: &str, name: &str, tags: &[&str], thr: f32, ratio: f32, attack: f32, release: f32, hpf: f32) -> B {
    use buscomp::p;
    PresetBuilder::new(buscomp::ID, cat, name).tags(&["bus compressor"]).tags(tags).set(p::THRESHOLD, thr).set(p::RATIO, ratio).set(p::ATTACK, attack).set(p::RELEASE, release).set(p::SC_HPF, hpf)
}

fn buscomp_presets() -> Vec<B> {
    use buscomp::p;
    vec![
        bus("Master", "Master Transparent 1 dB", &["master", "transparent", "gentle"], -10.0, 0.0, 6.0, 5.0, 3.0),
        bus("Master", "Master Glue 2 dB", &["master", "glue", "pop", "edm"], -14.0, 0.0, 6.0, 6.0, 3.0),
        bus("Master", "Master Punch 4-1", &["master", "punch", "edm"], -12.0, 1.0, 5.0, 3.0, 3.0),
        bus("Bus", "Mix Bus Glue 2-1 Slow", &["mix bus", "glue", "slow"], -16.0, 0.0, 6.0, 6.0, 3.0),
        bus("Bus", "Mix Bus Tight 4-1", &["mix bus", "tight", "edm"], -14.0, 1.0, 4.0, 2.0, 3.0),
        bus("Bus", "Vocal Bus Smooth 2-1", &["vocal", "bus", "smooth"], -18.0, 0.0, 4.0, 6.0, 4.0),
        bus("Bus", "Synth Bus Glue", &["synth", "bus", "glue"], -16.0, 0.0, 5.0, 4.0, 3.0),
        bus("Bus", "Bass Bus Control", &["bass", "bus", "control"], -18.0, 1.0, 3.0, 2.0, 0.0),
        bus("Drums", "Drum Bus Snap 4-1", &["drums", "bus", "snap"], -18.0, 1.0, 5.0, 1.0, 3.0),
        bus("Drums", "Drum Bus Fat 10-1", &["drums", "bus", "fat", "parallel"], -22.0, 2.0, 6.0, 3.0, 3.0).set(p::MIX, 50.0),
    ]
}

// ── Limiter ─────────────────────────────────────────────────────────────

fn lim(cat: &str, name: &str, tags: &[&str], gain: f32, ceiling: f32, style: f32, release: f32, lookahead: f32) -> B {
    use limiter::p;
    PresetBuilder::new(limiter::ID, cat, name)
        .tags(&["limiter"])
        .tags(tags)
        .set(p::GAIN, gain)
        .set(p::CEILING, ceiling)
        .set(p::STYLE, style)
        .set(p::RELEASE, release)
        .set(p::AUTO_RELEASE, 1.0)
        .set(p::LOOKAHEAD, lookahead)
}

fn limiter_presets() -> Vec<B> {
    use limiter::{p, style};
    vec![
        // The last stage, from light to firm. Gain is capped by the level
        // match so that loading one never makes the track louder than 2.5 dB.
        lim("Mastering", "Final Limiter Gentle 1 dB", &["master", "transparent", "gentle"], 1.0, -1.0, style::TRANSPARENT, 200.0, 3.0).set(p::OVERSAMPLING, 1.0),
        lim("Mastering", "Final Limiter Medium 2 dB", &["master", "allround", "pop"], 2.0, -1.0, style::ALLROUND, 120.0, 2.0).set(p::OVERSAMPLING, 1.0),
        lim("Mastering", "Final Limiter Firm 3 dB", &["master", "punchy", "edm"], 3.0, -1.0, style::PUNCHY, 80.0, 1.5).set(p::OVERSAMPLING, 1.0),
        lim("Mastering", "Final Limiter Club", &["master", "aggressive", "club", "techno"], 3.0, -0.3, style::AGGRESSIVE, 40.0, 1.0),
        lim("Mastering", "Final Limiter Streaming", &["master", "streaming", "true peak"], 1.5, -1.0, style::TRANSPARENT, 150.0, 3.0).set(p::OVERSAMPLING, 2.0).set(p::TRUE_PEAK, 1.0),
        lim("Mastering", "Final Limiter Vinyl-Safe", &["master", "vinyl", "safe"], 1.0, -1.5, style::TRANSPARENT, 250.0, 4.0).set(p::OVERSAMPLING, 1.0),
        lim("Mixing", "Mix Bus Safety -0.5", &["mix bus", "safety", "transparent"], 0.0, -0.5, style::BUS, 100.0, 1.5).set(p::TRUE_PEAK, 0.0),
        lim("Mixing", "Vocal Peak Safety", &["vocal", "safety", "transparent"], 0.0, -2.0, style::TRANSPARENT, 120.0, 2.0).set(p::TRUE_PEAK, 0.0),
        lim("Mixing", "Bass Peak Safety", &["bass", "safety", "control"], 0.0, -1.5, style::BUS, 100.0, 1.5).set(p::TRUE_PEAK, 0.0),
        lim("Mixing", "Drum Bus Peak Shaver", &["drums", "bus", "punchy"], 2.0, -3.0, style::PUNCHY, 30.0, 0.5).set(p::TRUE_PEAK, 0.0),
    ]
}

// ── Multiband dynamics ──────────────────────────────────────────────────

/// A band's dynamics: down threshold / ratio, up threshold / ratio / max,
/// attack and release in ms, output trim in dB.
#[derive(Clone, Copy)]
struct Bnd {
    down: (f32, f32),
    up: (f32, f32, f32),
    time: (f32, f32),
    out: f32,
    depth: f32,
}

const FLAT: Bnd = Bnd { down: (-26.0, 1.0), up: (-44.0, 1.0, 0.0), time: (30.0, 200.0), out: 0.0, depth: 0.0 };

fn mb(cat: &str, name: &str, tags: &[&str], crossover: (f32, f32), bands: [Bnd; 3]) -> B {
    use mbdyn::p;
    let mut b = PresetBuilder::new(mbdyn::ID, cat, name).tags(&["multiband"]).tags(tags).set(p::XOVER_LOW, crossover.0).set(p::XOVER_HIGH, crossover.1);
    for (i, bd) in bands.iter().enumerate() {
        let i = i as u32;
        b = b
            .set(p::band(i, p::TH_DOWN), bd.down.0)
            .set(p::band(i, p::RATIO_DOWN), bd.down.1)
            .set(p::band(i, p::TH_UP), bd.up.0)
            .set(p::band(i, p::RATIO_UP), bd.up.1)
            .set(p::band(i, p::MAX_UP), bd.up.2)
            .set(p::band(i, p::ATTACK), bd.time.0)
            .set(p::band(i, p::RELEASE), bd.time.1)
            .set(p::band(i, p::BAND_OUT), bd.out)
            .set(p::band(i, p::BAND_DEPTH), bd.depth * 100.0);
    }
    b
}

fn mbdyn_presets() -> Vec<B> {
    let on = |down: (f32, f32), up: (f32, f32, f32), time: (f32, f32)| Bnd { down, up, time, out: 0.0, depth: 1.0 };
    vec![
        mb("Mastering", "Master Gentle Glue", &["master", "glue", "transparent"], (110.0, 3_000.0), [on((-22.0, 1.8), (-60.0, 1.0, 0.0), (45.0, 300.0)), on((-22.0, 1.6), (-60.0, 1.0, 0.0), (30.0, 250.0)), on((-24.0, 1.6), (-60.0, 1.0, 0.0), (15.0, 150.0))]),
        mb("Mastering", "Master Bass Control", &["master", "low-end", "control"], (120.0, 3_000.0), [on((-20.0, 2.5), (-60.0, 1.0, 0.0), (40.0, 250.0)), FLAT, FLAT]),
        mb("Mastering", "Master Air Lift", &["master", "air", "bright"], (150.0, 4_000.0), [FLAT, FLAT, on((-30.0, 1.5), (-50.0, 1.5, 3.0), (10.0, 120.0))]),
        mb("Mastering", "Master Even Out", &["master", "even", "pop"], (100.0, 2_500.0), [on((-22.0, 2.0), (-50.0, 1.3, 2.0), (45.0, 300.0)), on((-22.0, 2.0), (-50.0, 1.3, 2.0), (30.0, 250.0)), on((-24.0, 1.8), (-52.0, 1.3, 2.0), (15.0, 150.0))]),
        mb("Mixing", "Mix Bus Even Tone", &["mix bus", "even", "tone"], (120.0, 3_000.0), [on((-24.0, 2.0), (-54.0, 1.3, 3.0), (40.0, 280.0)), on((-24.0, 2.0), (-54.0, 1.3, 3.0), (25.0, 220.0)), on((-26.0, 2.0), (-56.0, 1.3, 3.0), (12.0, 140.0))]),
        mb("Vocals", "Vocal Even Tone", &["vocal", "even", "smooth"], (180.0, 5_000.0), [FLAT, on((-24.0, 2.5), (-52.0, 1.5, 4.0), (15.0, 160.0)), on((-26.0, 2.0), (-54.0, 1.3, 2.0), (8.0, 100.0))]),
        mb("Drums", "Drum Bus Punch", &["drums", "bus", "punchy"], (100.0, 2_500.0), [on((-20.0, 2.0), (-50.0, 1.4, 3.0), (30.0, 150.0)), on((-18.0, 2.5), (-48.0, 1.3, 2.0), (12.0, 100.0)), on((-22.0, 2.0), (-50.0, 1.3, 2.0), (6.0, 80.0))]),
        mb("Bass", "Bass Tightener", &["bass", "tight", "edm"], (120.0, 1_500.0), [on((-20.0, 3.0), (-52.0, 1.5, 4.0), (35.0, 200.0)), on((-22.0, 2.0), (-54.0, 1.3, 2.0), (20.0, 160.0)), FLAT]),
        mb("Mixing", "Mud Control", &["mix bus", "mud", "cleanup"], (90.0, 700.0), [FLAT, on((-24.0, 3.0), (-60.0, 1.0, 0.0), (25.0, 200.0)), FLAT]),
        mb("Mixing", "Harshness Control", &["mix bus", "harsh", "smooth"], (400.0, 2_500.0), [FLAT, FLAT, on((-26.0, 3.0), (-60.0, 1.0, 0.0), (8.0, 100.0))]),
        mb("Creative", "Pad Smoother", &["pad", "synth", "smooth"], (150.0, 3_000.0), [on((-26.0, 2.0), (-54.0, 1.4, 3.0), (60.0, 400.0)), on((-26.0, 2.0), (-54.0, 1.4, 3.0), (50.0, 350.0)), on((-28.0, 2.0), (-56.0, 1.4, 3.0), (30.0, 250.0))]),
        mb("Creative", "Synth Bus Sparkle", &["synth", "bus", "bright"], (150.0, 3_500.0), [FLAT, on((-24.0, 2.0), (-50.0, 1.4, 3.0), (20.0, 160.0)), on((-30.0, 1.5), (-46.0, 1.6, 5.0), (10.0, 120.0))]),
        mb("Creative", "Parallel Density", &["parallel", "density", "edm"], (110.0, 2_800.0), [on((-24.0, 3.0), (-46.0, 2.0, 8.0), (30.0, 200.0)), on((-24.0, 3.0), (-46.0, 2.0, 8.0), (20.0, 160.0)), on((-26.0, 3.0), (-48.0, 2.0, 8.0), (10.0, 120.0))]).set(mbdyn::p::DEPTH, 45.0),
    ]
}

// ── Reverb ──────────────────────────────────────────────────────────────

// Algorithms (`reverb::voicing::ALGORITHMS`).
const CLASSIC: f32 = 0.0;
const CONCERT: f32 = 1.0;
const BRIGHT: f32 = 2.0;
const PLATE: f32 = 3.0;
const ROOM: f32 = 4.0;
const CHAMBER: f32 = 5.0;
const AMBIENCE: f32 = 6.0;
const SHIMMER: f32 = 7.0;
const CLOUD: f32 = 8.0;
const OCT_UP: f32 = 6.0;

fn verb(cat: &str, name: &str, tags: &[&str], algo: f32, size: f32, decay: f32, pre: f32, mix: f32) -> B {
    use reverb::p;
    PresetBuilder::new(reverb::ID, cat, name).tags(&["reverb"]).tags(tags).set(p::ALGORITHM, algo).set(p::SIZE, size).set(p::DECAY, decay).set(p::PREDELAY, pre).set(p::MIX, mix)
}

fn reverb_presets() -> Vec<B> {
    use reverb::p;
    vec![
        // ── Drums ──
        verb("Drums", "Short Drum Room", &["drums", "room", "tight", "punchy", "house"], ROOM, 25.0, 0.45, 0.0, 18.0).set(p::DAMPING, 55.0).set(p::EARLY, 80.0).set(p::LOW_CUT, 150.0),
        verb("Drums", "Tight Kick Room", &["kick", "room", "tight"], ROOM, 15.0, 0.3, 0.0, 12.0).set(p::DAMPING, 60.0).set(p::EARLY, 90.0).set(p::LOW_CUT, 200.0).set(p::HIGH_CUT, 9_000.0),
        verb("Drums", "Snare Plate Classic", &["snare", "plate", "classic", "pop"], PLATE, 50.0, 1.4, 6.0, 22.0).set(p::DAMPING, 35.0).set(p::LOW_CUT, 300.0),
        verb("Drums", "Snare Plate Short", &["snare", "plate", "tight", "trap"], PLATE, 40.0, 0.8, 3.0, 20.0).set(p::DAMPING, 40.0).set(p::LOW_CUT, 400.0),
        verb("Drums", "Clap Plate", &["clap", "plate", "house"], PLATE, 45.0, 1.0, 4.0, 22.0).set(p::DAMPING, 40.0).set(p::LOW_CUT, 400.0),
        verb("Drums", "Tom Hall", &["toms", "hall", "drums"], CONCERT, 55.0, 1.6, 10.0, 22.0).set(p::DAMPING, 50.0).set(p::LOW_CUT, 200.0),
        verb("Drums", "Overhead Ambience", &["overheads", "ambience", "drums"], AMBIENCE, 40.0, 0.7, 0.0, 14.0).set(p::EARLY, 80.0).set(p::LOW_CUT, 250.0),
        verb("Drums", "Drum Bus Ambience", &["drums", "bus", "ambience", "glue"], AMBIENCE, 35.0, 0.5, 0.0, 12.0).set(p::EARLY, 70.0).set(p::LOW_CUT, 150.0),
        // ── Vocals ──
        verb("Vocals", "Vocal Plate Smooth", &["vocal", "plate", "smooth", "pop"], PLATE, 50.0, 1.6, 20.0, 18.0).set(p::DAMPING, 45.0).set(p::DAMP_FREQ, 6_000.0).set(p::LOW_CUT, 250.0).set(p::HIGH_CUT, 12_000.0),
        verb("Vocals", "Vocal Plate Bright", &["vocal", "plate", "bright", "pop", "r&b"], PLATE, 50.0, 1.8, 25.0, 18.0).set(p::DAMPING, 25.0).set(p::LOW_CUT, 250.0).set(p::HIGH_CUT, 16_000.0),
        verb("Vocals", "Pop Vocal Room", &["vocal", "room", "natural", "pop"], ROOM, 35.0, 0.7, 8.0, 15.0).set(p::DAMPING, 50.0).set(p::EARLY, 60.0).set(p::LOW_CUT, 200.0),
        verb("Vocals", "Lead Vocal Ducked Hall", &["vocal", "hall", "ducking", "pop", "reggaeton"], BRIGHT, 60.0, 2.4, 40.0, 24.0).set(p::DUCK, 65.0).set(p::LOW_CUT, 250.0).set(p::HIGH_CUT, 14_000.0),
        verb("Vocals", "Intimate Vocal Chamber", &["vocal", "chamber", "warm", "intimate"], CHAMBER, 40.0, 1.0, 10.0, 14.0).set(p::DAMPING, 55.0).set(p::LOW_CUT, 200.0),
        verb("Vocals", "Rap Vocal Slap Room", &["vocal", "rap", "trap", "tight"], ROOM, 20.0, 0.4, 2.0, 12.0).set(p::EARLY, 90.0).set(p::LOW_CUT, 250.0),
        verb("Vocals", "Backing Vocal Wash", &["vocal", "backing", "wash", "wide"], CONCERT, 80.0, 3.0, 35.0, 30.0).set(p::DAMPING, 55.0).set(p::LOW_CUT, 300.0).set(p::WIDTH, 130.0),
        verb("Vocals", "Whisper Space", &["vocal", "ambient", "dark", "airy"], CLASSIC, 70.0, 3.5, 50.0, 20.0).set(p::MODE, 0.0).set(p::DAMPING, 65.0).set(p::LOW_CUT, 300.0),
        // ── Dance ──
        verb("Dance", "House Pump Verb", &["house", "pump", "ducking", "edm", "groove"], CONCERT, 60.0, 2.2, 15.0, 28.0).set(p::DAMPING, 50.0).set(p::LOW_CUT, 300.0).set(p::HIGH_CUT, 12_000.0).set(p::DUCK, 85.0),
        verb("Dance", "Ducked Pad Hall", &["pad", "ducking", "house", "hall"], CONCERT, 75.0, 4.0, 20.0, 30.0).set(p::LOW_CUT, 250.0).set(p::DUCK, 50.0),
        verb("Dance", "Techno Dark Chamber", &["techno", "dark", "chamber"], CHAMBER, 55.0, 1.6, 8.0, 25.0).set(p::DAMPING, 75.0).set(p::LOW_CUT, 300.0).set(p::HIGH_CUT, 7_000.0),
        verb("Dance", "Trance Lead Space", &["trance", "lead", "wide", "edm"], BRIGHT, 75.0, 3.0, 30.0, 22.0).set(p::LOW_CUT, 400.0).set(p::WIDTH, 140.0).set(p::MOD_DEPTH, 40.0),
        verb("Dance", "Big Room Snare Hall", &["snare", "big room", "edm"], CONCERT, 90.0, 2.8, 12.0, 28.0).set(p::LOW_CUT, 350.0),
        verb("Dance", "Future Bass Wash", &["future bass", "pads", "wash", "wide"], CLOUD, 85.0, 6.0, 30.0, 30.0).set(p::LOW_CUT, 250.0).set(p::MOD_DEPTH, 55.0).set(p::WIDTH, 150.0),
        verb("Dance", "Dub Chord Reverb", &["dub", "chords", "dark"], PLATE, 55.0, 3.5, 0.0, 28.0).set(p::DAMPING, 70.0).set(p::LOW_CUT, 400.0).set(p::HIGH_CUT, 5_000.0),
        // ── Instruments ──
        verb("Instruments", "Piano Hall", &["piano", "keys", "hall", "natural"], CONCERT, 65.0, 2.2, 18.0, 20.0).set(p::LOW_CUT, 150.0),
        verb("Instruments", "Keys Chamber", &["keys", "chamber", "warm"], CHAMBER, 45.0, 1.2, 10.0, 18.0).set(p::DAMPING, 55.0).set(p::LOW_CUT, 180.0),
        verb("Instruments", "Pluck Plate", &["pluck", "synth", "plate"], PLATE, 45.0, 1.4, 10.0, 18.0).set(p::LOW_CUT, 300.0),
        verb("Instruments", "Synth Pad Space", &["pad", "synth", "wide"], CONCERT, 85.0, 5.0, 40.0, 30.0).set(p::LOW_CUT, 200.0).set(p::WIDTH, 130.0),
        verb("Instruments", "Strings Concert Hall", &["strings", "orchestral", "hall"], CONCERT, 85.0, 3.4, 25.0, 24.0).set(p::DAMPING, 55.0).set(p::LOW_CUT, 120.0),
        verb("Instruments", "Guitar Ambient Room", &["guitar", "room", "natural"], ROOM, 45.0, 0.9, 10.0, 18.0).set(p::DAMPING, 50.0).set(p::LOW_CUT, 150.0),
        // ── Mixing: glue ──
        verb("Mixing", "Mix Bus Ambience Glue", &["mix bus", "glue", "subtle"], AMBIENCE, 40.0, 0.5, 0.0, 10.0).set(p::EARLY, 60.0).set(p::LOW_CUT, 200.0),
        // ── Shimmer and freeze ──
        verb("Shimmer", "Shimmer Octave Subtle", &["shimmer", "subtle", "pads", "keys"], SHIMMER, 80.0, 5.0, 30.0, 28.0).set(p::SHIMMER, 30.0).set(p::SHIFT, OCT_UP).set(p::LOW_CUT, 250.0),
        verb("Creative", "Freeze Drone Hall", &["freeze", "drone", "ambient", "hold"], CONCERT, 90.0, 8.0, 0.0, 40.0).set(p::FREEZE, 1.0),
        verb("Creative", "Freeze Plate Hold", &["freeze", "plate", "hold", "fx"], PLATE, 60.0, 6.0, 0.0, 35.0).set(p::FREEZE, 1.0),
    ]
}

// ── Delay ───────────────────────────────────────────────────────────────

// `aitrack_dsp::lfo::SYNC_DIVISIONS` indices.
const D_HALF: f32 = 4.0;
const D_QUARTER: f32 = 7.0;
const D_DOT_QUARTER: f32 = 8.0;
const D_EIGHTH: f32 = 10.0;
const D_DOT_EIGHTH: f32 = 11.0;
const D_EIGHTH_T: f32 = 12.0;
const D_SIXTEENTH: f32 = 13.0;
const D_THIRTY_SECOND: f32 = 16.0;
// Modes and engines.
const STEREO: f32 = 0.0;
const PING: f32 = 1.0;
const MONO: f32 = 2.0;
const TAPE: f32 = 1.0;
const BBD: f32 = 2.0;

fn echo(cat: &str, name: &str, tags: &[&str], mode: f32, left: f32, right: f32, feedback: f32, mix: f32) -> B {
    use delay::p;
    PresetBuilder::new(delay::ID, cat, name).tags(&["delay"]).tags(tags).set(p::MODE, mode).set(p::SYNC, 1.0).set(p::DIV_L, left).set(p::DIV_R, right).set(p::FEEDBACK, feedback).set(p::MIX, mix)
}

/// Free-running times in ms.
fn echo_ms(cat: &str, name: &str, tags: &[&str], mode: f32, left: f32, right: f32, feedback: f32, mix: f32) -> B {
    use delay::p;
    PresetBuilder::new(delay::ID, cat, name).tags(&["delay"]).tags(tags).set(p::MODE, mode).set(p::SYNC, 0.0).set(p::TIME_L, left).set(p::TIME_R, right).set(p::FEEDBACK, feedback).set(p::MIX, mix)
}

fn delay_presets() -> Vec<B> {
    use delay::p;
    vec![
        // ── Dance ──
        echo("Dance", "House Dotted Eighth", &["house", "dotted", "edm", "groove"], PING, D_DOT_EIGHTH, D_DOT_EIGHTH, 38.0, 22.0).set(p::LOW_CUT, 400.0).set(p::HIGH_CUT, 7_000.0).set(p::DUCK, 30.0).set(p::WIDTH, 120.0),
        echo("Dance", "Sixteenth Filtered", &["sixteenth", "filtered", "tech house", "groove"], STEREO, D_SIXTEENTH, D_SIXTEENTH, 32.0, 18.0).set(p::LOW_CUT, 800.0).set(p::HIGH_CUT, 4_500.0),
        echo("Dance", "Wide Quarter Ping-Pong", &["ping-pong", "quarter", "wide", "edm"], PING, D_QUARTER, D_QUARTER, 45.0, 22.0).set(p::LOW_CUT, 300.0).set(p::HIGH_CUT, 8_000.0).set(p::WIDTH, 140.0),
        echo("Dance", "Techno Triplet Echo", &["techno", "triplet", "dark"], PING, D_EIGHTH_T, D_EIGHTH_T, 50.0, 22.0).set(p::LOW_CUT, 500.0).set(p::HIGH_CUT, 5_000.0).set(p::DRIVE, 3.0),
        echo("Dance", "Trance Dotted Wash", &["trance", "dotted", "wash", "edm"], STEREO, D_DOT_EIGHTH, D_DOT_EIGHTH, 55.0, 25.0).set(p::HIGH_CUT, 8_000.0).set(p::DIFFUSION, 40.0).set(p::LOW_CUT, 300.0),
        echo("Dance", "Dub Chords Dotted Quarter", &["dub", "chords", "dark"], PING, D_DOT_QUARTER, D_DOT_QUARTER, 70.0, 30.0).set(p::LOW_CUT, 300.0).set(p::HIGH_CUT, 2_800.0).set(p::DRIVE, 6.0),
        echo("Dance", "Minimal Click Delay", &["hi-hat", "minimal", "techno"], STEREO, D_THIRTY_SECOND, D_SIXTEENTH, 20.0, 15.0).set(p::LOW_CUT, 1_500.0).set(p::HIGH_CUT, 9_000.0),
        // ── Vocals ──
        echo_ms("Vocals", "Vocal Slapback Short", &["vocal", "slapback", "pop"], MONO, 90.0, 90.0, 8.0, 18.0).set(p::HIGH_CUT, 5_500.0).set(p::LOW_CUT, 200.0),
        echo_ms("Vocals", "Vocal Slapback Stereo", &["vocal", "slapback", "wide"], STEREO, 80.0, 110.0, 10.0, 20.0).set(p::HIGH_CUT, 5_500.0).set(p::LOW_CUT, 200.0).set(p::WIDTH, 130.0),
        echo("Vocals", "Vocal Eighth Ducked", &["vocal", "ducking", "pop"], PING, D_EIGHTH, D_QUARTER, 35.0, 24.0).set(p::DUCK, 70.0).set(p::LOW_CUT, 300.0).set(p::HIGH_CUT, 6_000.0),
        echo("Vocals", "Vocal Quarter Throw", &["vocal", "throw", "sparse"], STEREO, D_QUARTER, D_QUARTER, 25.0, 20.0).set(p::DIFFUSION, 30.0).set(p::LOW_CUT, 400.0).set(p::HIGH_CUT, 5_000.0),
        echo("Vocals", "Rap Adlib Echo", &["vocal", "rap", "adlib"], MONO, D_QUARTER, D_QUARTER, 40.0, 28.0).set(p::HIGH_CUT, 4_500.0).set(p::LOW_CUT, 400.0).set(p::DUCK, 50.0),
        echo("Vocals", "Vocal Tape Echo Warm", &["vocal", "tape", "warm"], STEREO, D_DOT_EIGHTH, D_DOT_EIGHTH, 35.0, 22.0).set(p::ENGINE, TAPE).set(p::WOW, 20.0).set(p::AGE, 35.0).set(p::LOW_CUT, 250.0),
        // ── Instruments ──
        echo("Instruments", "Guitar Dotted Eighth", &["guitar", "dotted", "ambient"], STEREO, D_DOT_EIGHTH, D_DOT_EIGHTH, 40.0, 20.0).set(p::HIGH_CUT, 5_500.0),
        echo("Instruments", "Pluck Ping-Pong Sixteenth", &["pluck", "ping-pong", "synth"], PING, D_SIXTEENTH, D_SIXTEENTH, 35.0, 20.0).set(p::LOW_CUT, 300.0),
        echo("Instruments", "Pad Wide Quarter", &["pad", "wide", "synth"], STEREO, D_QUARTER, D_QUARTER, 40.0, 25.0).set(p::WIDTH, 160.0).set(p::DIFFUSION, 40.0).set(p::HIGH_CUT, 6_000.0),
        echo("Instruments", "Keys Tape Eighth", &["keys", "tape", "vintage"], STEREO, D_EIGHTH, D_EIGHTH, 30.0, 20.0).set(p::ENGINE, TAPE).set(p::WOW, 15.0).set(p::AGE, 40.0),
        echo("Instruments", "Lead Synth Dotted", &["lead", "synth", "dotted", "edm"], PING, D_DOT_EIGHTH, D_DOT_EIGHTH, 45.0, 22.0).set(p::DUCK, 40.0).set(p::LOW_CUT, 300.0),
        // ── Tape and BBD ──
        echo("Tape", "Tape Echo Classic", &["tape", "classic", "vintage", "warm"], STEREO, D_DOT_EIGHTH, D_DOT_EIGHTH, 50.0, 28.0)
            .set(p::ENGINE, TAPE)
            .set(p::WOW, 25.0)
            .set(p::FLUTTER, 20.0)
            .set(p::AGE, 40.0)
            .set(p::DRIVE, 4.0)
            .set(p::LOW_CUT, 200.0)
            .set(p::HIGH_CUT, 4_500.0),
        echo("Tape", "Tape Space Echo Taps", &["tape", "multi-tap", "space", "dub"], STEREO, D_QUARTER, D_QUARTER, 55.0, 28.0).set(p::ENGINE, TAPE).set(p::TAPS, 2.0).set(p::WOW, 30.0).set(p::AGE, 45.0).set(p::LOW_CUT, 250.0),
        echo("Tape", "Tape Ping-Pong Vintage", &["tape", "ping-pong", "vintage"], PING, D_QUARTER, D_QUARTER, 45.0, 25.0).set(p::ENGINE, TAPE).set(p::WOW, 20.0).set(p::AGE, 35.0).set(p::LOW_CUT, 200.0),
        echo("BBD", "Analog Eighth Echo", &["bbd", "analog", "warm"], STEREO, D_EIGHTH, D_EIGHTH, 45.0, 25.0).set(p::ENGINE, BBD).set(p::AGE, 35.0),
        echo("BBD", "BBD Quarter Dark", &["bbd", "dark", "dub"], MONO, D_QUARTER, D_QUARTER, 55.0, 28.0).set(p::ENGINE, BBD).set(p::AGE, 70.0).set(p::LOW_CUT, 250.0),
        // ── Long and frozen ──
        echo("Freeze", "Hold Echo", &["freeze", "hold", "texture"], STEREO, D_QUARTER, D_QUARTER, 50.0, 40.0).set(p::FREEZE, 1.0),
        echo_ms("Freeze", "Frozen Short Loop", &["freeze", "texture", "loop"], STEREO, 120.0, 150.0, 50.0, 45.0).set(p::FREEZE, 1.0).set(p::DIFFUSION, 60.0),
        echo("Freeze", "Frozen Ping-Pong Drone", &["freeze", "drone", "ping-pong"], PING, D_HALF, D_HALF, 50.0, 40.0).set(p::FREEZE, 1.0),
        echo("Freeze", "Infinite Echo 92 percent", &["long", "infinite", "ambient"], STEREO, D_DOT_EIGHTH, D_DOT_EIGHTH, 92.0, 30.0).set(p::DIFFUSION, 50.0).set(p::LOW_CUT, 300.0).set(p::HIGH_CUT, 5_000.0),
    ]
}

/// Compressors: lift the threshold until the preset, with no make-up, takes
/// at most 6 dB off the reference mix. The thresholds above are written for
/// real tracks; the reference is a dense mix at −18 dBFS RMS, and a preset
/// that needs +16 dB of make-up there would be dangerous on a quiet track.
fn tame_threshold(preset: &mut Preset, threshold_id: u32) {
    let info = |id: u32| aitrack_stock::create(&preset.plugin).unwrap().info().params.iter().find(|i| i.id == id).cloned().unwrap();
    let (thr, trim) = (info(threshold_id), info(trim_param(&preset.plugin).unwrap()));
    let trim_key = trim.id.to_string();
    for _ in 0..12 {
        preset.params.insert(trim_key.clone(), trim.to_normalized(0.0));
        let mut p = aitrack_stock::create(&preset.plugin).unwrap();
        p.prepare(SR, BLOCK);
        preset.apply(p.as_mut());
        let (delta, ..) = level_vs_bypass(p.as_mut(), source_for_tags(&preset.tags));
        if delta >= -6.0 {
            break;
        }
        let t = thr.to_plain(preset.params[&threshold_id.to_string()]);
        preset.params.insert(threshold_id.to_string(), thr.to_normalized((t + 2.0).min(thr.max)));
    }
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let groups = [reverb_presets(), delay_presets(), eq_presets(), compressor_presets(), buscomp_presets(), limiter_presets(), mbdyn_presets()];
    let mut total = 0;
    for group in groups {
        for b in group {
            let (category, mut preset) = b.build();
            match preset.plugin.as_str() {
                compressor::ID => tame_threshold(&mut preset, compressor::p::THRESHOLD),
                buscomp::ID => tame_threshold(&mut preset, buscomp::p::THRESHOLD),
                _ => {}
            }
            let level = level_match(&mut preset);
            write_preset(&root, &category, &preset)?;
            println!("{}/{category}/{:<34} {level:+.1} dB", preset.plugin, preset.name);
            total += 1;
        }
    }
    println!("{total} presets");
    Ok(())
}
