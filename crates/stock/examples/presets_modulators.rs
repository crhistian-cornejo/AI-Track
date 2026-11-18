//! Generates the `mod.lfo` and `mod.envfollow` factory presets.
//! `cargo run -p aitrack-stock --example presets_modulators`

use aitrack_stock::fx::{envfollow, lfo};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let l = |cat: &str, name: &str| PresetBuilder::new(lfo::ID, cat, name).tags(&["modulator", "lfo"]);
    let e = |cat: &str, name: &str| PresetBuilder::new(envfollow::ID, cat, name).tags(&["modulator", "envelope"]);
    let presets = [
        l("Smooth", "Slow Sine 2 Bars").tags(&["filter", "pads"]).set(lfo::p::WAVE, 0.0).set(lfo::p::DIVISION, 2.0),
        l("Smooth", "Triangle Half Bar").tags(&["filter"]).set(lfo::p::WAVE, 1.0).set(lfo::p::DIVISION, 4.0),
        l("Smooth", "Drift").tags(&["pads", "organic"]).set(lfo::p::WAVE, 6.0).set(lfo::p::DIVISION, 3.0).set(lfo::p::DEPTH, 60.0).set(lfo::p::SMOOTH, 120.0),
        l("Rhythmic", "Wobble Eighths").tags(&["bass", "dubstep", "house"]).set(lfo::p::WAVE, 0.0).set(lfo::p::DIVISION, 10.0),
        l("Rhythmic", "Wobble Triplet").tags(&["bass", "dubstep"]).set(lfo::p::WAVE, 0.0).set(lfo::p::DIVISION, 12.0),
        l("Rhythmic", "Gate Sixteenths").tags(&["trance", "volume"]).set(lfo::p::WAVE, 4.0).set(lfo::p::DIVISION, 13.0).set(lfo::p::SMOOTH, 6.0),
        l("Rhythmic", "Ramp Down 1 Bar").tags(&["build", "sidechain"]).set(lfo::p::WAVE, 3.0).set(lfo::p::DIVISION, 3.0),
        l("Random", "Sample and Hold").tags(&["acid", "filter"]).set(lfo::p::WAVE, 5.0).set(lfo::p::DIVISION, 13.0),
        l("Random", "Smooth Random").tags(&["movement"]).set(lfo::p::WAVE, 6.0).set(lfo::p::DIVISION, 7.0),
        l("Per Note", "Pluck Sweep").tags(&["pluck", "retrigger"]).set(lfo::p::WAVE, 3.0).set(lfo::p::TIME_MODE, 0.0).set(lfo::p::RATE, 2.0).set(lfo::p::RETRIG, 1.0),
        l("Per Note", "Vibrato 5.5 Hz").tags(&["pitch", "retrigger"]).set(lfo::p::WAVE, 0.0).set(lfo::p::TIME_MODE, 0.0).set(lfo::p::RATE, 5.5).set(lfo::p::DEPTH, 40.0).set(lfo::p::RETRIG, 1.0),
        e("Follow", "Smooth Follow").tags(&["filter"]).set(envfollow::p::RISE, 20.0).set(envfollow::p::FALL, 400.0),
        e("Follow", "Fast Punch").tags(&["drums", "filter"]).set(envfollow::p::RISE, 1.0).set(envfollow::p::FALL, 80.0).set(envfollow::p::GAIN, 18.0),
        e("Follow", "RMS Loudness").tags(&["mix", "bus"]).set(envfollow::p::MODE, 1.0).set(envfollow::p::RISE, 40.0).set(envfollow::p::FALL, 800.0),
        e("Follow", "Slow Swell").tags(&["pads", "filter"]).set(envfollow::p::RISE, 120.0).set(envfollow::p::FALL, 1500.0).set(envfollow::p::GAIN, 6.0),
        e("Ducking", "Gentle Duck").tags(&["sidechain", "pads"]).set(envfollow::p::INVERT, 1.0).set(envfollow::p::DEPTH, 50.0).set(envfollow::p::RISE, 5.0).set(envfollow::p::FALL, 400.0),
        e("Ducking", "Inverse Duck").tags(&["sidechain", "bass"]).set(envfollow::p::INVERT, 1.0).set(envfollow::p::RISE, 2.0).set(envfollow::p::FALL, 250.0).set(envfollow::p::GAIN, 20.0),
    ];
    for pr in presets {
        let path = pr.write(&root)?;
        println!("{}", path.display());
    }
    Ok(())
}
