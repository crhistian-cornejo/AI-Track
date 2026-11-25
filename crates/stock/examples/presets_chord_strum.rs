//! Generates the strum and chance factory presets of midi.chord (the older
//! ones come from `presets_midi`, which this does not touch).
//! `cargo run -p aitrack-stock --example presets_chord_strum`

use aitrack_stock::midi::chord::{self, dir};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

fn c(cat: &str, name: &str) -> PresetBuilder {
    PresetBuilder::new(chord::ID, cat, name)
}

/// Voices as `(interval, level %, chance %)`; the rest are switched off.
fn voices(mut b: PresetBuilder, v: &[(f32, f32, f32)]) -> PresetBuilder {
    for i in 0..chord::VOICES {
        match v.get(i) {
            Some((interval, level, chance)) => {
                b = b.set(chord::p::on(i), 1.0).set(chord::p::interval(i), *interval).set(chord::p::level(i), *level).set(chord::p::chance(i), *chance);
            }
            None => b = b.set(chord::p::on(i), 0.0).set(chord::p::interval(i), 0.0).set(chord::p::level(i), 100.0).set(chord::p::chance(i), 100.0),
        }
    }
    b
}

fn presets() -> Vec<PresetBuilder> {
    use chord::p;
    vec![
        voices(c("Strum", "Lazy Downstroke").tags(&["house", "strum", "minor", "seventh", "downstroke"]), &[(0.0, 100.0, 100.0), (3.0, 90.0, 100.0), (7.0, 90.0, 100.0), (10.0, 80.0, 100.0)])
            .set(p::STRUM, 45.0)
            .set(p::STRUM_DIR, dir::DOWN)
            .set(p::STRUM_TENSION, 30.0)
            .set(p::STRUM_VEL, -30.0),
        voices(c("Strum", "Harp Roll").tags(&["ambient", "pad", "harp", "roll", "major", "ninth"]), &[(0.0, 100.0, 100.0), (4.0, 85.0, 100.0), (7.0, 85.0, 100.0), (11.0, 75.0, 100.0), (14.0, 65.0, 100.0)])
            .set(p::STRUM, 180.0)
            .set(p::STRUM_DIR, dir::UP)
            .set(p::STRUM_TENSION, -35.0)
            .set(p::STRUM_VEL, 50.0),
        voices(c("Strum", "Funk Down-Up").tags(&["funk", "house", "guitar", "stab", "strum"]), &[(0.0, 100.0, 100.0), (7.0, 90.0, 100.0), (12.0, 80.0, 100.0), (16.0, 70.0, 100.0)])
            .set(p::STRUM, 26.0)
            .set(p::STRUM_DIR, dir::DOWN_UP)
            .set(p::STRUM_VEL, 20.0),
        voices(
            c("Chance", "Ghost Voices").tags(&["techno", "generative", "stab", "minor", "chance"]),
            &[(0.0, 100.0, 100.0), (3.0, 85.0, 85.0), (7.0, 100.0, 100.0), (10.0, 90.0, 60.0), (14.0, 80.0, 45.0), (19.0, 70.0, 70.0)],
        )
        .set(p::SEED, 7.0)
        .set(p::LOOP, 2.0),
        voices(c("Chance", "Loaded Dice").tags(&["minimal", "techno", "generative", "random", "strum"]), &[(0.0, 100.0, 75.0), (3.0, 90.0, 75.0), (7.0, 90.0, 75.0), (12.0, 80.0, 75.0)])
            .set(p::STRUM, 22.0)
            .set(p::STRUM_DIR, dir::RANDOM)
            .set(p::STRUM_TENSION, 20.0)
            .set(p::SEED, 3.0)
            .set(p::LOOP, 1.0),
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
