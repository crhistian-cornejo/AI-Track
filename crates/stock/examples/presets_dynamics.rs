//! Generates the factory presets of fx.eq, fx.compressor, fx.limiter,
//! fx.gate and fx.transient.
//! `cargo run -p aitrack-stock --example presets_dynamics`

use aitrack_stock::fx::{compressor, eq, gate, limiter, transient};
use aitrack_stock::presets::{PresetBuilder, factory_dir};
use eq::{ch, slope, ty};

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
    ]
}

fn compressor_presets() -> Vec<PresetBuilder> {
    use compressor::{p, style};
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
    ]
}

fn limiter_presets() -> Vec<PresetBuilder> {
    use limiter::p;
    let b = |cat: &str, name: &str| PresetBuilder::new(limiter::ID, cat, name).tags(&["limiter"]);
    vec![
        b("Mastering", "Master -1 dBTP").tags(&["master", "true peak", "edm", "pop"]).set(p::CEILING, -1.0).set(p::TRUE_PEAK, 1.0).set(p::RELEASE, 80.0).set(p::AUTO_RELEASE, 1.0).set(p::LOOKAHEAD, 1.5),
        b("Mastering", "Streaming -1 dBTP Safe").tags(&["master", "streaming", "true peak", "transparent"]).set(p::CEILING, -1.0).set(p::GAIN, 3.0).set(p::TRUE_PEAK, 1.0).set(p::OVERSAMPLING, 2.0).set(p::LOOKAHEAD, 3.0).set(p::RELEASE, 150.0).set(p::AUTO_RELEASE, 1.0),
        b("Mastering", "Transparent").tags(&["master", "transparent", "acoustic", "pop"]).set(p::CEILING, -1.0).set(p::GAIN, 2.0).set(p::LOOKAHEAD, 5.0).set(p::RELEASE, 300.0).set(p::AUTO_RELEASE, 1.0).set(p::OVERSAMPLING, 1.0),
        b("Mastering", "Loud Club").tags(&["master", "loud", "edm", "techno", "dembow"]).set(p::CEILING, -0.3).set(p::GAIN, 8.0).set(p::LOOKAHEAD, 1.0).set(p::RELEASE, 30.0).set(p::AUTO_RELEASE, 1.0).set(p::OVERSAMPLING, 1.0),
        b("Mastering", "Dense Master").tags(&["master", "loud", "dense", "trap", "reggaeton"]).set(p::CEILING, -1.0).set(p::GAIN, 6.0).set(p::LOOKAHEAD, 2.0).set(p::RELEASE, 60.0).set(p::AUTO_RELEASE, 1.0).set(p::OVERSAMPLING, 2.0).set(p::LINK, 80.0),
        b("Mastering", "Broadcast -2 dBTP").tags(&["master", "broadcast", "true peak", "safe"]).set(p::CEILING, -2.0).set(p::TRUE_PEAK, 1.0).set(p::LOOKAHEAD, 2.0).set(p::RELEASE, 200.0).set(p::AUTO_RELEASE, 1.0).set(p::OVERSAMPLING, 2.0),
        b("Mixing", "Drum Peak Catcher").tags(&["drums", "peak", "bus", "punchy"]).set(p::CEILING, -3.0).set(p::GAIN, 3.0).set(p::LOOKAHEAD, 0.5).set(p::RELEASE, 20.0).set(p::AUTO_RELEASE, 0.0).set(p::TRUE_PEAK, 0.0),
        b("Mixing", "Bus Safety").tags(&["bus", "safety", "transparent"]).set(p::CEILING, -0.5).set(p::LOOKAHEAD, 1.0).set(p::RELEASE, 50.0).set(p::TRUE_PEAK, 0.0),
    ]
}

fn gate_presets() -> Vec<PresetBuilder> {
    use gate::{mode, p};
    let b = |cat: &str, name: &str| PresetBuilder::new(gate::ID, cat, name).tags(&["gate"]);
    vec![
        b("Drums", "Kick Tight").tags(&["kick", "tight", "house", "techno"]).set(p::THRESHOLD, -30.0).set(p::HYSTERESIS, 6.0).set(p::ATTACK, 0.1).set(p::HOLD, 30.0).set(p::RELEASE, 60.0).set(p::RANGE, -60.0),
        b("Drums", "Snare Gate").tags(&["snare", "bleed", "rock", "pop"]).set(p::THRESHOLD, -28.0).set(p::HYSTERESIS, 6.0).set(p::ATTACK, 0.2).set(p::HOLD, 40.0).set(p::RELEASE, 120.0).set(p::RANGE, -30.0).set(p::SC_HPF, 150.0).set(p::SC_LPF, 10_000.0),
        b("Drums", "Tom Ring Kill").tags(&["toms", "cleanup", "acoustic"]).set(p::THRESHOLD, -32.0).set(p::ATTACK, 0.5).set(p::HOLD, 80.0).set(p::RELEASE, 250.0).set(p::RANGE, -25.0).set(p::SC_HPF, 60.0),
        b("Drums", "Hi-Hat Clean").tags(&["hi-hat", "tight", "cleanup"]).set(p::THRESHOLD, -35.0).set(p::ATTACK, 0.05).set(p::HOLD, 5.0).set(p::RELEASE, 40.0).set(p::RANGE, -40.0).set(p::SC_HPF, 4_000.0),
        b("Cleanup", "Noise Floor").tags(&["noise", "cleanup", "expander"]).set(p::MODE, mode::EXPANDER).set(p::THRESHOLD, -55.0).set(p::RATIO, 2.0).set(p::ATTACK, 2.0).set(p::RELEASE, 200.0).set(p::RANGE, -20.0),
        b("Cleanup", "Vocal Breath Reduce").tags(&["vocal", "breath", "expander", "smooth"]).set(p::MODE, mode::EXPANDER).set(p::THRESHOLD, -45.0).set(p::RATIO, 1.5).set(p::ATTACK, 5.0).set(p::RELEASE, 150.0).set(p::RANGE, -12.0).set(p::SC_HPF, 100.0),
        b("Creative", "Trance Duck").tags(&["duck", "sidechain", "trance", "edm", "pump"]).set(p::MODE, mode::DUCK).set(p::SIDECHAIN, 1.0).set(p::THRESHOLD, -30.0).set(p::RANGE, -18.0).set(p::ATTACK, 1.0).set(p::HOLD, 0.0).set(p::RELEASE, 180.0),
        b("Creative", "Gated Reverb Snap").tags(&["reverb", "80s", "snare", "creative"]).set(p::THRESHOLD, -24.0).set(p::ATTACK, 0.05).set(p::HOLD, 120.0).set(p::RELEASE, 20.0).set(p::RANGE, -80.0),
    ]
}

fn transient_presets() -> Vec<PresetBuilder> {
    use transient::p;
    let b = |cat: &str, name: &str| PresetBuilder::new(transient::ID, cat, name).tags(&["transient"]);
    vec![
        b("Drums", "Punchy Drums").tags(&["drums", "punchy", "house", "trap"]).set(p::ATTACK, 50.0).set(p::SUSTAIN, -10.0).set(p::WINDOW, 15.0).set(p::LENGTH, 200.0),
        b("Drums", "Snappy Snare").tags(&["snare", "snappy", "pop", "reggaeton"]).set(p::ATTACK, 70.0).set(p::SUSTAIN, -20.0).set(p::WINDOW, 8.0).set(p::LENGTH, 150.0),
        b("Drums", "Kick Click").tags(&["kick", "click", "techno", "edm"]).set(p::ATTACK, 80.0).set(p::WINDOW, 5.0).set(p::CLIP, 1.0),
        b("Drums", "Room Killer").tags(&["drums", "dry", "cleanup"]).set(p::SUSTAIN, -70.0).set(p::LENGTH, 300.0),
        b("Drums", "Soften Attack").tags(&["drums", "soft", "lo-fi"]).set(p::ATTACK, -60.0).set(p::WINDOW, 20.0),
        b("Instruments", "Long Sustain").tags(&["sustain", "ambient", "guitar", "keys"]).set(p::SUSTAIN, 60.0).set(p::LENGTH, 600.0),
        b("Instruments", "Tight Bass").tags(&["bass", "tight", "edm", "dembow"]).set(p::ATTACK, 20.0).set(p::SUSTAIN, -40.0).set(p::LENGTH, 150.0),
        b("Instruments", "Pluck Lift").tags(&["pluck", "synth", "future bass", "pop"]).set(p::ATTACK, 40.0).set(p::SUSTAIN, 20.0).set(p::WINDOW, 10.0).set(p::LENGTH, 300.0).set(p::MIX, 70.0),
    ]
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let all = [eq_presets(), compressor_presets(), limiter_presets(), gate_presets(), transient_presets()];
    for group in all {
        for pr in group {
            let path = pr.write(&root)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}
