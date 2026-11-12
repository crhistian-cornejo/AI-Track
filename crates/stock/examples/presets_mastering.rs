//! Generates the factory presets of fx.clipper, fx.multiband and fx.midside.
//! `cargo run -p aitrack-stock --example presets_mastering`

use aitrack_stock::fx::{clipper, midside, multiband};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

fn clipper_presets() -> Vec<PresetBuilder> {
    use clipper::p;
    let (soft, hard) = (clipper::curve::SOFT, clipper::curve::HARD);
    let b = |cat: &str, name: &str| PresetBuilder::new(clipper::ID, cat, name).tags(&["clipper"]);
    vec![
        b("Mastering", "Transparent Peak").tags(&["transparent", "master", "subtle"]).set(p::CEILING, -1.0).set(p::DRIVE, 0.0).set(p::CURVE, soft).set(p::OVERSAMPLING, 1.0),
        b("Mastering", "Loud Master").tags(&["loud", "master", "edm"]).set(p::CEILING, -0.3).set(p::DRIVE, 6.0).set(p::CURVE, soft).set(p::LIFT, 15.0).set(p::OVERSAMPLING, 1.0),
        b("Mastering", "Low End Glue").tags(&["low-end", "glue", "master"]).set(p::CEILING, -0.8).set(p::DRIVE, 3.0).set(p::CURVE, soft).set(p::LIFT, 30.0).set(p::OVERSAMPLING, 1.0),
        b("Drums", "Drum Clip").tags(&["drums", "punchy", "techno"]).set(p::CEILING, -3.0).set(p::DRIVE, 12.0).set(p::CURVE, hard).set(p::OVERSAMPLING, 2.0),
        b("Bass", "808 Lift").tags(&["808", "bass", "trap", "low-end"]).set(p::CEILING, -1.0).set(p::DRIVE, 4.0).set(p::CURVE, soft).set(p::LIFT, 60.0).set(p::OVERSAMPLING, 1.0),
        b("Creative", "Lo-Fi Clip").tags(&["lo-fi", "aliasing", "creative"]).set(p::CEILING, -6.0).set(p::DRIVE, 18.0).set(p::CURVE, hard).set(p::OVERSAMPLING, 0.0),
        b("Creative", "Delta Listen").tags(&["delta", "utility", "check"]).set(p::CEILING, -6.0).set(p::DRIVE, 12.0).set(p::CURVE, hard).set(p::DELTA, 1.0).set(p::OVERSAMPLING, 0.0),
        b("Mastering", "4x Clean Master").tags(&["clean", "master", "4x"]).set(p::CEILING, -0.2).set(p::DRIVE, 5.0).set(p::CURVE, soft).set(p::LIFT, 10.0).set(p::OVERSAMPLING, 2.0),
    ]
}

fn multiband_presets() -> Vec<PresetBuilder> {
    use multiband::p;
    let b = |cat: &str, name: &str| PresetBuilder::new(multiband::ID, cat, name).tags(&["multiband"]);
    let flat = |b: PresetBuilder| {
        b.set(p::band(0, p::GAIN), 0.0).set(p::band(1, p::GAIN), 0.0).set(p::band(2, p::GAIN), 0.0)
    };
    vec![
        b("Utility", "Flat Split").tags(&["neutral", "utility", "crossover"]),
        b("Mastering", "Linear Master").tags(&["linear", "master", "clean"]).set(p::MODE, multiband::mode::LINEAR).set(p::XOVER_LOW, 150.0).set(p::XOVER_HIGH, 3_000.0),
        flat(b("Mastering", "Bass & Air").tags(&["master", "low-end", "air"]).set(p::XOVER_LOW, 120.0).set(p::XOVER_HIGH, 4_000.0))
            .set(p::band(0, p::GAIN), 1.5)
            .set(p::band(1, p::GAIN), -0.5)
            .set(p::band(2, p::GAIN), 2.0),
        flat(b("Mixing", "De-Mud").tags(&["mixing", "mud", "mid"]).set(p::XOVER_LOW, 250.0).set(p::XOVER_HIGH, 3_000.0))
            .set(p::band(1, p::GAIN), -3.0),
        flat(b("Mixing", "Three-Way Balance").tags(&["mixing", "balance", "bus"]).set(p::XOVER_LOW, 200.0).set(p::XOVER_HIGH, 2_500.0))
            .set(p::band(0, p::GAIN), 2.0)
            .set(p::band(1, p::GAIN), -1.5)
            .set(p::band(2, p::GAIN), 1.0),
        flat(b("Mixing", "Dark High Band").tags(&["mixing", "dark", "high"]).set(p::XOVER_HIGH, 5_000.0)).set(p::band(2, p::GAIN), -6.0),
        flat(b("Utility", "Low Solo Check").tags(&["solo", "check", "utility"])).set(p::band(0, p::SOLO), 1.0),
        flat(b("Utility", "Mid Delta Check").tags(&["delta", "check", "utility"])).set(p::band(1, p::DELTA), 1.0),
    ]
}

fn midside_presets() -> Vec<PresetBuilder> {
    use midside::p;
    let b = |cat: &str, name: &str| PresetBuilder::new(midside::ID, cat, name).tags(&["mid-side"]);
    vec![
        b("Stereo", "Neutral").tags(&["neutral", "utility"]),
        b("Mastering", "Wide Master").tags(&["master", "wide", "stereo"]).set(p::MID, 0.5).set(p::WIDTH, 120.0),
        b("Mastering", "Mono Fold").tags(&["mono", "check", "master"]).set(p::WIDTH, 0.0),
        b("Mixing", "Subtle Width").tags(&["mixing", "subtle", "width"]).set(p::WIDTH, 110.0),
        b("Mixing", "Extra Wide").tags(&["wide", "stereo", "edm"]).set(p::SIDE, 0.5).set(p::WIDTH, 160.0),
        b("Mixing", "Mid Focus").tags(&["mid", "focus", "vocal"]).set(p::MID, 3.0).set(p::SIDE, -3.0).set(p::WIDTH, 90.0),
        b("Utility", "Mid Solo Check").tags(&["solo", "check", "mono"]).set(p::SOLO_MID, 1.0),
        b("Utility", "Side Solo Check").tags(&["solo", "check", "stereo"]).set(p::SOLO_SIDE, 1.0),
    ]
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let all = [clipper_presets(), multiband_presets(), midside_presets()];
    for group in all {
        for pr in group {
            let path = pr.write(&root)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}
