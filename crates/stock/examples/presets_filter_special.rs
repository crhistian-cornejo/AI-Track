//! Generates the factory presets of `fx.filter` that use its special types,
//! the extra LFO shapes and the sidechain follower. Only creates its own
//! files (the older presets come from `presets_fx`).
//! `cargo run -p aitrack-stock --example presets_filter_special`

use aitrack_stock::fx::filter::{self, p};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

fn f(cat: &str, name: &str) -> PresetBuilder {
    PresetBuilder::new(filter::ID, cat, name).tags(&["filter"])
}

/// Index of a `Special` type: DJ 1, Comb 2, Vowel 3, Morph 4, Resample 5, Notch+LP 6.
const DJ: f32 = 1.0;
const COMB: f32 = 2.0;
const VOWEL: f32 = 3.0;
const MORPH: f32 = 4.0;
const RESAMPLE: f32 = 5.0;
const NOTCH_LP: f32 = 6.0;

fn presets() -> Vec<PresetBuilder> {
    vec![
        f("DJ", "DJ Sweep Down").tags(&["dj", "transition", "lfo"]).set(p::SPECIAL, DJ).set(p::CUTOFF, 400.0).set(p::RESONANCE, 30.0).set(p::LFO_EXTRA, 3.0).set(p::LFO_SYNC, 1.0).set(p::LFO_DIV, 3.0).set(p::LFO_DEPTH, -3.0),
        f("DJ", "DJ Rise 24").tags(&["dj", "build", "riser"]).set(p::SPECIAL, DJ).set(p::SLOPE, 1.0).set(p::CUTOFF, 3_000.0).set(p::RESONANCE, 25.0).set(p::LFO_EXTRA, 2.0).set(p::LFO_SYNC, 1.0).set(p::LFO_DIV, 2.0).set(p::LFO_DEPTH, 1.5),
        f("Comb", "Metal Comb 220").tags(&["comb", "metallic", "resonator"]).set(p::SPECIAL, COMB).set(p::CUTOFF, 220.0).set(p::RESONANCE, 78.0).set(p::SHAPE, 35.0),
        f("Comb", "Flanging Comb").tags(&["comb", "flanger", "lfo"]).set(p::SPECIAL, COMB).set(p::CUTOFF, 700.0).set(p::RESONANCE, 55.0).set(p::SHAPE, 20.0).set(p::LFO_SHAPE, 1.0).set(p::LFO_RATE, 0.25).set(p::LFO_DEPTH, 0.6),
        f("Vowel", "Talking Bass").tags(&["vowel", "formant", "bass", "lfo"]).set(p::SPECIAL, VOWEL).set(p::CUTOFF, 180.0).set(p::RESONANCE, 60.0).set(p::LFO_SHAPE, 1.0).set(p::LFO_SYNC, 1.0).set(p::LFO_DIV, 3.0).set(p::LFO_DEPTH, 1.6),
        f("Vowel", "Wandering Vowel").tags(&["vowel", "formant", "pads", "organic"]).set(p::SPECIAL, VOWEL).set(p::CUTOFF, 120.0).set(p::RESONANCE, 45.0).set(p::SHAPE, 15.0).set(p::LFO_EXTRA, 1.0).set(p::LFO_RATE, 0.4).set(p::LFO_DEPTH, 2.0),
        f("Morph", "Low to High Morph").tags(&["morph", "sweep"]).set(p::SPECIAL, MORPH).set(p::CUTOFF, 1_200.0).set(p::RESONANCE, 35.0).set(p::SHAPE, 50.0),
        f("Morph", "Notch Morph Wobble").tags(&["morph", "notch", "wobble", "house"]).set(p::SPECIAL, MORPH).set(p::CUTOFF, 900.0).set(p::RESONANCE, 40.0).set(p::SHAPE, 100.0).set(p::LFO_SYNC, 1.0).set(p::LFO_DIV, 10.0).set(p::LFO_DEPTH, 1.0),
        f("Lo-Fi", "Resample 8 kHz").tags(&["resample", "lofi", "crunch"]).set(p::SPECIAL, RESAMPLE).set(p::CUTOFF, 3_300.0).set(p::RESONANCE, 20.0).set(p::SHAPE, 0.0),
        f("Lo-Fi", "Smooth Downsample").tags(&["resample", "lofi", "tape"]).set(p::SPECIAL, RESAMPLE).set(p::CUTOFF, 1_800.0).set(p::RESONANCE, 30.0).set(p::SHAPE, 100.0),
        f("Special", "Notch Low-Pass").tags(&["notch", "lowpass", "bass"]).set(p::SPECIAL, NOTCH_LP).set(p::CUTOFF, 800.0).set(p::RESONANCE, 35.0).set(p::SHAPE, 40.0).set(p::SLOPE, 1.0),
        f("Envelope", "Kick Sidechain Open").tags(&["sidechain", "follower", "pump"]).set(p::CUTOFF, 300.0).set(p::RESONANCE, 20.0).set(p::ENV_SOURCE, 1.0).set(p::ENV_AMOUNT, -2.5).set(p::ENV_ATTACK, 1.0).set(p::ENV_RELEASE, 220.0),
        f("Envelope", "Sidechain Vowel").tags(&["sidechain", "follower", "vowel", "talk"]).set(p::SPECIAL, VOWEL).set(p::CUTOFF, 100.0).set(p::RESONANCE, 50.0).set(p::ENV_SOURCE, 1.0).set(p::ENV_AMOUNT, 3.0).set(p::ENV_ATTACK, 2.0).set(p::ENV_RELEASE, 300.0),
    ]
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let mut n = 0;
    for b in presets() {
        let path = b.write(&root)?;
        println!("{}", path.strip_prefix(&root).unwrap_or(&path).display());
        n += 1;
    }
    println!("{n} presets");
    Ok(())
}
