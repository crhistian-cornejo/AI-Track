//! Generates the `mod.shaper` factory presets. Only creates its own files.
//! `cargo run -p aitrack-stock --example presets_shaper_mod`

use aitrack_stock::fx::shaper_mod::{self, p, trigger};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

fn m(cat: &str, name: &str) -> PresetBuilder {
    PresetBuilder::new(shaper_mod::ID, cat, name).tags(&["modulator", "shaper"])
}

/// The six nodes as `(time, level)`; the first and last times are fixed at 0 and 1.
fn curve(b: PresetBuilder, pts: [(f32, f32); 6]) -> PresetBuilder {
    b.set(p::Y0, pts[0].1)
        .set(p::X1, pts[1].0)
        .set(p::Y1, pts[1].1)
        .set(p::X2, pts[2].0)
        .set(p::Y2, pts[2].1)
        .set(p::X3, pts[3].0)
        .set(p::Y3, pts[3].1)
        .set(p::X4, pts[4].0)
        .set(p::Y4, pts[4].1)
        .set(p::Y5, pts[5].1)
}

fn presets() -> Vec<PresetBuilder> {
    vec![
        // The default curve is the pump of fx.shaper.
        m("Pump", "Quarter Pump").tags(&["sidechain", "house", "pump", "volume"]),
        curve(m("Pump", "Off-beat Pump").tags(&["sidechain", "techno", "pump", "offbeat"]), [(0.0, 1.0), (0.5, 1.0), (0.56, 0.0), (0.8, 0.7), (0.95, 1.0), (1.0, 1.0)]).set(p::DIVISION, 7.0),
        curve(m("Gate", "Eighth Gate").tags(&["trance", "gate", "volume", "chop"]), [(0.0, 1.0), (0.45, 1.0), (0.5, 0.0), (0.97, 0.0), (0.98, 0.0), (1.0, 0.0)])
            .set(p::DIVISION, 10.0)
            .set(p::SMOOTH, 3.0),
        curve(m("Gate", "Triplet Cut").tags(&["house", "gate", "triplet", "filter"]), [(0.0, 1.0), (0.3, 1.0), (0.34, 0.1), (0.64, 0.1), (0.68, 1.0), (1.0, 1.0)])
            .set(p::DIVISION, 9.0)
            .set(p::SMOOTH, 6.0),
        curve(m("Per Note", "Note Decay").tags(&["pluck", "filter", "retrigger", "decay"]), [(0.0, 1.0), (0.1, 0.6), (0.3, 0.3), (0.6, 0.12), (0.85, 0.03), (1.0, 0.0)])
            .set(p::TRIGGER, trigger::NOTE_ONCE as f32)
            .set(p::DIVISION, 4.0)
            .set(p::TENSION, 20.0),
        curve(m("Build", "One Bar Rise").tags(&["build", "riser", "filter", "transition"]), [(0.0, 0.0), (0.25, 0.1), (0.5, 0.3), (0.75, 0.6), (0.95, 0.95), (1.0, 1.0)])
            .set(p::TRIGGER, trigger::NOTE_LOOP as f32)
            .set(p::DIVISION, 3.0)
            .set(p::SMOOTH, 10.0),
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
