//! Generates the `fx.utility` factory presets.
//! `cargo run -p aitrack-stock --example presets_utility`

use aitrack_stock::fx::utility::{ID, p};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let b = |cat: &str, name: &str| PresetBuilder::new(ID, cat, name).tags(&["utility"]);
    let presets = [
        b("Stereo", "Mono Check").tags(&["mixing", "mono"]).set(p::MONO, 1.0),
        b("Stereo", "Bass Mono 120").tags(&["mixing", "edm", "house", "techno"]).set(p::BASS_MONO, 1.0).set(p::BASS_FREQ, 120.0),
        b("Stereo", "Wide 150").tags(&["mixing", "wide", "pads"]).set(p::WIDTH, 150.0).set(p::BASS_MONO, 1.0).set(p::BASS_FREQ, 150.0),
        b("Stereo", "Narrow 50").tags(&["mixing", "narrow"]).set(p::WIDTH, 50.0),
        b("Stereo", "Side Listen").tags(&["mixing", "analysis"]).set(p::MODE, 4.0),
        b("Stereo", "Swap Channels").tags(&["fix"]).set(p::SWAP, 1.0),
        b("Gain", "Trim -6 dB").tags(&["gain staging"]).set(p::GAIN, -6.0),
        b("Gain", "Trim +6 dB").tags(&["gain staging"]).set(p::GAIN, 6.0),
        b("Fix", "Phase Flip Right").tags(&["fix", "phase"]).set(p::INVERT_R, 1.0),
        b("Fix", "DC Clean").tags(&["fix", "mastering"]).set(p::DC_BLOCK, 1.0),
    ];
    for pr in presets {
        let path = pr.write(&root)?;
        println!("{}", path.display());
    }
    Ok(())
}
