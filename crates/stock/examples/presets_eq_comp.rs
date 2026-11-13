//! Generates the factory presets of fx.eq and fx.compressor (the dynamic /
//! linear-phase EQ and the compressor with models, topology and sidechain EQ).
//! `cargo run -p aitrack-stock --example presets_eq_comp`

use aitrack_stock::fx::{compressor, eq};
use aitrack_stock::presets::{PresetBuilder, factory_dir};
use eq::{ch, dir, phase, slope, ty};

/// Turn on EQ band `i` as `(type, freq, gain, q)`.
fn band(b: PresetBuilder, i: usize, t: f32, f: f32, g: f32, q: f32) -> PresetBuilder {
    b.set(eq::p::band(i, eq::p::ON), 1.0)
        .set(eq::p::band(i, eq::p::TYPE), t)
        .set(eq::p::band(i, eq::p::FREQ), f)
        .set(eq::p::band(i, eq::p::GAIN), g)
        .set(eq::p::band(i, eq::p::Q), q)
}

fn cut(b: PresetBuilder, i: usize, t: f32, f: f32, s: f32) -> PresetBuilder {
    band(b, i, t, f, 0.0, 0.707).set(eq::p::band(i, eq::p::SLOPE), s)
}

fn chan(b: PresetBuilder, i: usize, c: f32) -> PresetBuilder {
    b.set(eq::p::band(i, eq::p::CHANNEL), c)
}

fn dynamic(b: PresetBuilder, i: usize, thr: f32, ratio: f32, att: f32, rel: f32) -> PresetBuilder {
    b.set(eq::p::band(i, eq::p::DYNAMIC), 1.0)
        .set(eq::p::band(i, eq::p::THRESHOLD), thr)
        .set(eq::p::band(i, eq::p::RATIO), ratio)
        .set(eq::p::band(i, eq::p::ATTACK), att)
        .set(eq::p::band(i, eq::p::RELEASE), rel)
}

/// Direction and range of a dynamic band.
fn dyn_mode(b: PresetBuilder, i: usize, d: f32, range: f32) -> PresetBuilder {
    b.set(eq::dynx::band(i, eq::dynx::MODE), d).set(eq::dynx::band(i, eq::dynx::RANGE), range)
}

/// A dynamic band keyed by the external sidechain.
fn external(b: PresetBuilder, i: usize) -> PresetBuilder {
    b.set(eq::dynx::band(i, eq::dynx::SOURCE), 1.0).set(eq::p::SIDECHAIN, 1.0)
}

fn eq_presets() -> Vec<PresetBuilder> {
    let b = |cat: &str, name: &str| PresetBuilder::new(eq::ID, cat, name).tags(&["eq"]);
    vec![
        // Sub cleanup, sub weight, cut the cardboard box, lift the beater.
        band(band(band(cut(b("Drums", "Kick Punch").tags(&["kick", "punchy", "edm", "house", "techno"]), 0, ty::LOW_CUT, 30.0, slope::DB24), 1, ty::BELL, 60.0, 3.0, 1.2), 3, ty::BELL, 350.0, -4.0, 1.5), 5, ty::BELL, 3_500.0, 3.0, 1.0),
        band(band(band(band(cut(b("Drums", "Snare Crack").tags(&["snare", "bright", "pop", "trap"]), 0, ty::LOW_CUT, 80.0, slope::DB12), 2, ty::BELL, 200.0, 2.0, 1.4), 3, ty::BELL, 900.0, -3.0, 2.0), 5, ty::BELL, 5_000.0, 4.0, 1.0), 6, ty::HIGH_SHELF, 10_000.0, 2.0, 0.707),
        band(dynamic(band(cut(b("Drums", "Hi-Hat Tame").tags(&["hi-hat", "smooth", "lo-fi", "house"]), 0, ty::LOW_CUT, 300.0, slope::DB24), 5, ty::BELL, 7_000.0, -2.0, 2.0), 5, -30.0, 3.0, 2.0, 80.0), 6, ty::HIGH_SHELF, 12_000.0, -2.0, 0.707),
        band(band(band(band(cut(b("Drums", "808 Clarity").tags(&["808", "trap", "reggaeton", "dembow"]), 0, ty::LOW_CUT, 25.0, slope::DB24), 1, ty::BELL, 60.0, 2.0, 1.0), 2, ty::BELL, 300.0, -3.0, 1.2), 4, ty::BELL, 1_200.0, 3.0, 1.5), 7, ty::HIGH_CUT, 9_000.0, 0.0, 0.707),
        // Rumble out, a little less mud, presence and air.
        band(band(band(cut(b("Vocals", "Vocal Air").tags(&["vocal", "airy", "pop", "bright"]), 0, ty::LOW_CUT, 90.0, slope::DB18), 2, ty::BELL, 300.0, -2.0, 1.0), 4, ty::BELL, 3_000.0, 1.5, 1.0), 6, ty::HIGH_SHELF, 12_000.0, 4.0, 0.707),
        dynamic(band(cut(b("Vocals", "De-Ess Dynamic").tags(&["vocal", "de-ess", "dynamic", "smooth"]), 0, ty::LOW_CUT, 80.0, slope::DB12), 5, ty::BELL, 6_500.0, 0.0, 3.0), 5, -30.0, 4.0, 1.0, 60.0),
        band(band(band(cut(cut(b("Vocals", "Telephone").tags(&["vocal", "lo-fi", "creative", "fx"]), 0, ty::LOW_CUT, 400.0, slope::DB24), 7, ty::HIGH_CUT, 3_000.0, slope::DB24), 4, ty::BELL, 1_500.0, 6.0, 1.0), 2, ty::BELL, 600.0, 2.0, 1.0), 5, ty::BELL, 2_400.0, 2.0, 2.0),
        // Bass: tight sub, less boom, growl you can hear on small speakers.
        cut(band(band(band(cut(b("Bass", "Bass Focus").tags(&["bass", "tight", "edm", "dubstep"]), 0, ty::LOW_CUT, 28.0, slope::DB48), 1, ty::BELL, 60.0, 2.0, 1.0), 2, ty::BELL, 250.0, -3.0, 1.2), 3, ty::BELL, 800.0, 2.0, 1.5), 7, ty::HIGH_CUT, 8_000.0, slope::DB12),
        cut(b("Mixing", "Low Cut 30").tags(&["cleanup", "mixing", "utility"]), 0, ty::LOW_CUT, 30.0, slope::DB24),
        band(band(b("Mixing", "Mud Remover").tags(&["cleanup", "mixing", "clear"]), 2, ty::BELL, 250.0, -3.0, 1.2), 3, ty::BELL, 500.0, -1.5, 1.5),
        band(band(cut(b("Mixing", "Pad Cleanup").tags(&["pad", "synth", "future bass", "clear"]), 0, ty::LOW_CUT, 200.0, slope::DB18), 4, ty::BELL, 3_000.0, -2.0, 1.5), 6, ty::HIGH_SHELF, 10_000.0, 2.0, 0.707),
        // Gentle master curve with mono lows (side low cut).
        chan(cut(band(band(band(cut(b("Mastering", "Master Polish").tags(&["master", "polish", "edm", "pop"]), 0, ty::LOW_CUT, 20.0, slope::DB12), 1, ty::LOW_SHELF, 80.0, 1.0, 0.707), 3, ty::BELL, 300.0, -1.0, 0.8), 6, ty::HIGH_SHELF, 10_000.0, 1.5, 0.707), 2, ty::LOW_CUT, 150.0, slope::DB12), 2, ch::SIDE),
        chan(chan(band(cut(b("Mastering", "Wide Air, Mono Lows").tags(&["master", "wide", "mid-side", "house"]), 0, ty::LOW_CUT, 200.0, slope::DB24), 6, ty::HIGH_SHELF, 8_000.0, 3.0, 0.707), 0, ch::SIDE), 6, ch::SIDE),
        band(band(cut(b("Creative", "Lo-Fi Warm").tags(&["lo-fi", "warm", "vintage"]), 7, ty::HIGH_CUT, 6_000.0, slope::DB12), 1, ty::LOW_SHELF, 150.0, 3.0, 0.707), 4, ty::BELL, 2_500.0, -2.0, 1.0),
        band(b("Creative", "Dark Tilt").tags(&["dark", "warm", "lo-fi", "techno"]), 3, ty::TILT, 1_000.0, -4.0, 0.707),
        // ── Phase modes ──
        // Linear phase: no smearing of the transients on a master; side low
        // cut keeps the lows mono, a broad air shelf on top.
        chan(band(band(cut(cut(b("Mastering", "Linear Master Air").tags(&["master", "linear-phase", "air", "transparent"]), 0, ty::LOW_CUT, 25.0, slope::DB24), 2, ty::LOW_CUT, 120.0, slope::DB12), 4, ty::BELL, 2_800.0, -0.8, 0.6), 6, ty::HIGH_SHELF, 11_000.0, 2.0, 0.6), 2, ch::SIDE)
            .set(eq::p::PHASE, phase::LINEAR),
        // Natural phase: analogue-style low shelf with no cramping up top.
        band(band(b("Mastering", "Natural Warmth").tags(&["master", "natural-phase", "warm", "analog"]), 1, ty::LOW_SHELF, 110.0, 1.5, 0.6), 6, ty::HIGH_SHELF, 16_000.0, 1.5, 0.5)
            .set(eq::p::PHASE, phase::NATURAL),
        // ── Dynamic EQ ──
        // Two resonances that only poke out on loud notes.
        dyn_mode(dynamic(dyn_mode(dynamic(band(band(cut(b("Vocals", "Vocal Resonance Tamer").tags(&["vocal", "dynamic", "smooth", "harsh"]), 0, ty::LOW_CUT, 90.0, slope::DB18), 3, ty::BELL, 2_600.0, 0.0, 4.0), 5, ty::BELL, 4_800.0, 0.0, 5.0), 3, -32.0, 3.0, 2.0, 80.0), 3, dir::CUT_ABOVE, 6.0), 5, -34.0, 4.0, 1.0, 60.0), 5, dir::CUT_ABOVE, 8.0),
        // Upward expansion of the snare's crack: hits get brighter, the bleed doesn't.
        dyn_mode(dynamic(band(b("Drums", "Snare Upward Snap").tags(&["snare", "dynamic", "punchy", "bright"]), 4, ty::BELL, 5_000.0, 0.0, 1.2), 4, -28.0, 2.0, 0.5, 90.0), 4, dir::BOOST_ABOVE, 5.0),
        // Upward compression of the low mids: quiet passages fill out.
        dyn_mode(dynamic(band(b("Mixing", "Pad Body Fill").tags(&["pad", "dynamic", "warm", "upward"]), 2, ty::BELL, 350.0, 0.0, 0.8), 2, -30.0, 3.0, 20.0, 300.0), 2, dir::BOOST_BELOW, 4.0),
        // Keyed from the kick: the bass dips only under each kick.
        external(dyn_mode(dynamic(band(b("Bass", "Kick-Bass Carve").tags(&["bass", "sidechain", "kick", "dynamic", "edm"]), 1, ty::BELL, 60.0, 0.0, 1.4), 1, -30.0, 6.0, 0.5, 90.0), 1, dir::CUT_ABOVE, 9.0), 1),
        // Keyed from the vocal: the music's presence region ducks around the voice.
        chan(external(dyn_mode(dynamic(band(b("Mixing", "Vocal Pocket").tags(&["sidechain", "vocal", "dynamic", "mid-side", "pop"]), 4, ty::BELL, 2_200.0, 0.0, 0.9), 4, -36.0, 3.0, 5.0, 160.0), 4, dir::CUT_ABOVE, 4.0), 4), 4, ch::MID),
    ]
}

fn compressor_presets() -> Vec<PresetBuilder> {
    use compressor::{model, p, style, topology};
    let b = |cat: &str, name: &str| PresetBuilder::new(compressor::ID, cat, name).tags(&["compressor"]);
    vec![
        b("Bus", "Drum Bus Glue").tags(&["drums", "bus", "glue", "house", "techno"]).set(p::STYLE, style::GLUE).set(p::THRESHOLD, -16.0).set(p::RATIO, 3.0).set(p::ATTACK, 20.0).set(p::RELEASE, 150.0).set(p::AUTO_RELEASE, 1.0).set(p::KNEE, 6.0).set(p::MAKEUP, 2.0),
        b("Bus", "Mix Bus Gentle").tags(&["master", "bus", "glue", "pop", "edm"]).set(p::STYLE, style::GLUE).set(p::THRESHOLD, -12.0).set(p::RATIO, 1.5).set(p::ATTACK, 30.0).set(p::RELEASE, 300.0).set(p::AUTO_RELEASE, 1.0).set(p::KNEE, 12.0).set(p::SC_HPF, 90.0).set(p::MAKEUP, 1.0),
        b("Vocals", "Vocal Leveler").tags(&["vocal", "smooth", "pop", "reggaeton"]).set(p::STYLE, style::OPTO).set(p::THRESHOLD, -24.0).set(p::RATIO, 3.0).set(p::ATTACK, 5.0).set(p::RELEASE, 200.0).set(p::KNEE, 10.0).set(p::RMS, 40.0).set(p::MAKEUP, 4.0),
        b("Vocals", "Vocal Upfront").tags(&["vocal", "aggressive", "trap", "pop"]).set(p::STYLE, style::PUNCH).set(p::THRESHOLD, -26.0).set(p::RATIO, 6.0).set(p::ATTACK, 2.0).set(p::RELEASE, 70.0).set(p::KNEE, 4.0).set(p::SC_HPF, 120.0).set(p::MAKEUP, 6.0),
        b("Drums", "Kick Punch").tags(&["kick", "punchy", "edm", "house"]).set(p::STYLE, style::PUNCH).set(p::THRESHOLD, -20.0).set(p::RATIO, 4.0).set(p::ATTACK, 30.0).set(p::RELEASE, 80.0).set(p::KNEE, 3.0).set(p::MAKEUP, 3.0),
        b("Drums", "Snare Snap").tags(&["snare", "punchy", "pop", "trap"]).set(p::STYLE, style::PUNCH).set(p::THRESHOLD, -22.0).set(p::RATIO, 5.0).set(p::ATTACK, 8.0).set(p::RELEASE, 60.0).set(p::KNEE, 3.0).set(p::MAKEUP, 3.0),
        b("Drums", "Parallel Smash").tags(&["drums", "parallel", "aggressive", "dubstep"]).set(p::STYLE, style::CLEAN).set(p::THRESHOLD, -35.0).set(p::RATIO, 20.0).set(p::ATTACK, 1.0).set(p::RELEASE, 60.0).set(p::KNEE, 0.0).set(p::MIX, 35.0).set(p::MAKEUP, 10.0),
        b("Bass", "Bass Control").tags(&["bass", "tight", "edm", "dembow"]).set(p::STYLE, style::CLEAN).set(p::THRESHOLD, -20.0).set(p::RATIO, 4.0).set(p::ATTACK, 5.0).set(p::RELEASE, 120.0).set(p::KNEE, 6.0).set(p::RMS, 30.0).set(p::MAKEUP, 3.0),
        b("Bass", "808 Sustain").tags(&["808", "trap", "reggaeton", "sustain"]).set(p::STYLE, style::CLEAN).set(p::THRESHOLD, -24.0).set(p::RATIO, 6.0).set(p::ATTACK, 15.0).set(p::RELEASE, 300.0).set(p::KNEE, 6.0).set(p::MAKEUP, 6.0),
        // Keyed from the kick bus: fast grab, release about an eighth note.
        b("Creative", "Sidechain Pump").tags(&["sidechain", "pump", "edm", "house", "future bass"]).set(p::SIDECHAIN, 1.0).set(p::THRESHOLD, -30.0).set(p::RATIO, 20.0).set(p::ATTACK, 0.5).set(p::RELEASE, 200.0).set(p::KNEE, 3.0).set(p::RANGE, 20.0),
        b("Creative", "Pad Tamer").tags(&["pad", "synth", "smooth", "lo-fi"]).set(p::STYLE, style::OPTO).set(p::THRESHOLD, -20.0).set(p::RATIO, 2.0).set(p::ATTACK, 20.0).set(p::RELEASE, 400.0).set(p::KNEE, 12.0).set(p::MAKEUP, 2.0),
        b("Mastering", "Peak Catcher").tags(&["master", "peak", "transparent"]).set(p::STYLE, style::CLEAN).set(p::THRESHOLD, -6.0).set(p::RATIO, 100.0).set(p::ATTACK, 0.05).set(p::RELEASE, 50.0).set(p::KNEE, 0.0).set(p::LOOKAHEAD, 2.0),
        // ── Models, topology, sidechain EQ ──
        // Firm VCA bus: fast grab, constant-rate release that keeps the groove.
        b("Bus", "VCA Drum Bus").tags(&["drums", "bus", "vca", "punchy", "techno"]).set(p::MODEL, model::VCA).set(p::THRESHOLD, -18.0).set(p::RATIO, 4.0).set(p::ATTACK, 10.0).set(p::RELEASE, 120.0).set(p::KNEE, 2.0).set(p::SC_HPF, 60.0).set(p::MAKEUP, 3.0),
        // Slow tube-style glue on the master: wide knee, feedback, low ratio.
        b("Mastering", "Vari-Mu Glue").tags(&["master", "vari-mu", "glue", "warm", "feedback"]).set(p::MODEL, model::VARI_MU).set(p::TOPOLOGY, topology::FEEDBACK).set(p::THRESHOLD, -14.0).set(p::RATIO, 2.0).set(p::ATTACK, 25.0).set(p::RELEASE, 400.0).set(p::SC_HPF, 80.0).set(p::MAKEUP, 1.5),
        // Feedback opto for vocals: soft attack, levelling release.
        b("Vocals", "Feedback Opto Vocal").tags(&["vocal", "opto", "feedback", "smooth", "rnb"]).set(p::STYLE, style::OPTO).set(p::TOPOLOGY, topology::FEEDBACK).set(p::THRESHOLD, -26.0).set(p::RATIO, 4.0).set(p::ATTACK, 8.0).set(p::RELEASE, 250.0).set(p::KNEE, 8.0).set(p::MAKEUP, 5.0),
        // Broadband de-esser: the detector only hears the esses.
        b("Vocals", "De-Esser").tags(&["vocal", "de-ess", "sidechain eq", "smooth"]).set(p::STYLE, style::CLEAN).set(p::THRESHOLD, -30.0).set(p::RATIO, 6.0).set(p::ATTACK, 0.5).set(p::RELEASE, 60.0).set(p::KNEE, 3.0).set(p::SC_HPF, 500.0).set(p::SC_EQ_FREQ, 7_000.0).set(p::SC_EQ_GAIN, 18.0).set(p::RANGE, 8.0),
        // Bass that doesn't pump on the sub: the detector ignores the lows.
        b("Bass", "Bass Mid Grip").tags(&["bass", "sidechain eq", "tight", "dubstep"]).set(p::MODEL, model::VCA).set(p::THRESHOLD, -22.0).set(p::RATIO, 5.0).set(p::ATTACK, 3.0).set(p::RELEASE, 90.0).set(p::SC_HPF, 120.0).set(p::SC_EQ_FREQ, 700.0).set(p::SC_EQ_GAIN, 6.0).set(p::SC_LPF, 5_000.0).set(p::MAKEUP, 4.0),
    ]
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    for group in [eq_presets(), compressor_presets()] {
        for pr in group {
            let path = pr.write(&root)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}
