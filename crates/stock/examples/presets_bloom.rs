//! Generates the factory presets of instrument.bloom: a seed and a species
//! each (the device grows the scene from them), with the macros set.
//! `cargo run -p aitrack-stock --example presets_bloom`

use aitrack_stock::instrument::bloom::{ID, p};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

/// (category, name, tags, seed, species, decay, body, tone, grit, space)
type Spec = (&'static str, &'static str, &'static [&'static str], u32, f32, f32, f32, f32, f32, f32);

const PRESETS: &[Spec] = &[
    ("Kits", "First Bloom", &["drums", "kit", "balanced"], 1_337, 0., 0., 0., 0., 0., 25.),
    ("Kits", "Dry Room Kit", &["drums", "kit", "dry", "punchy"], 40_412, 0., -35., -20., 0., 10., 8.),
    ("Kits", "Heavy Bloom", &["drums", "kit", "distorted", "aggressive"], 77_031, 0., 10., 0., 15., 60., 20.),
    ("Tonal", "Marimba Garden", &["percussion", "tonal", "mallet", "melodic"], 5_150, 1., 20., 30., 10., -20., 35.),
    ("Tonal", "Pluck Orchard", &["percussion", "tonal", "pluck", "melodic"], 123_456, 1., 0., 0., 20., 0., 30.),
    ("Metal", "Foundry", &["metallic", "percussion", "industrial"], 90_210, 2., 15., 25., 20., 25., 30.),
    ("Metal", "Bell Tower", &["metallic", "bell", "ambient"], 31_415, 2., 50., 40., 5., -30., 55.),
    ("Glitch", "Circuit Bent", &["glitch", "fx", "digital", "short"], 66_600, 3., -40., -10., 35., 70., 15.),
    ("Glitch", "Data Rain", &["glitch", "blip", "digital"], 404_404, 3., 0., 0., 50., 30., 25.),
    ("Mixed", "Swamp Mix", &["mixed", "drums", "fx", "organic"], 271_828, 4., 20., 20., -30., 10., 35.),
];

fn main() {
    let root = factory_dir();
    for &(cat, name, tags, seed, species, decay, body, tone, grit, space) in PRESETS {
        let path = PresetBuilder::new(ID, cat, name)
            .tags(tags)
            .set(p::SEED, seed as f32)
            .set(p::SPECIES, species)
            .set(p::DECAY, decay)
            .set(p::BODY, body)
            .set(p::TONE, tone)
            .set(p::GRIT, grit)
            .set(p::SPACE, space)
            .write(&root)
            .expect("write preset");
        println!("{}", path.display());
    }
}
