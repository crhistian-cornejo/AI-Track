//! Generates the factory presets of instrument.drumsynth.
//! `cargo run -p aitrack-stock --example presets_drumsynth`
//!
//! A preset names the types it changes (model, tune in st, decay in ms, tone
//! and snap in %, level in dB); every other type keeps its defaults. The
//! "Kits" set several types at once; the other folders tune one type for a
//! style and keep the rest as they are. A few presets play one type on every
//! key (Mono).

use aitrack_stock::instrument::drumsynth::{self as ds, Kind, p};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

/// `(type, model, tune, decay, tone, snap, level)`.
type T = (Kind, usize, f32, f32, f32, f32, f32);

use Kind::*;

struct Def {
    cat: &'static str,
    name: &'static str,
    tags: &'static [&'static str],
    /// `Some(type)` plays that type on every key.
    mono: Option<Kind>,
    types: &'static [T],
}

const fn d(cat: &'static str, name: &'static str, tags: &'static [&'static str], types: &'static [T]) -> Def {
    Def { cat, name, tags, mono: None, types }
}

const fn m(cat: &'static str, name: &'static str, tags: &'static [&'static str], kind: Kind, types: &'static [T]) -> Def {
    Def { cat, name, tags, mono: Some(kind), types }
}

const DEFS: &[Def] = &[
    // Kits: every type set.
    d("Kits", "808 Classic", &["808", "kit", "hip hop"], &[
        (Kick, 0, 0.0, 700.0, 45.0, 30.0, -3.0), (Snare, 0, 0.0, 260.0, 45.0, 60.0, -4.0), (Rimshot, 0, 0.0, 70.0, 50.0, 50.0, -6.0), (Hihat, 0, 0.0, 300.0, 55.0, 40.0, -8.0),
        (Clap, 0, 0.0, 320.0, 45.0, 50.0, -5.0), (Tom, 0, 0.0, 450.0, 50.0, 30.0, -5.0), (Perc, 2, 0.0, 300.0, 50.0, 50.0, -8.0), (Cymbal, 0, 0.0, 1800.0, 50.0, 60.0, -10.0), (Fx, 0, 0.0, 600.0, 50.0, 50.0, -8.0),
    ]),
    d("Kits", "909 Club", &["909", "kit", "house", "techno"], &[
        (Kick, 2, 0.0, 380.0, 55.0, 55.0, -3.0), (Snare, 2, 0.0, 200.0, 60.0, 60.0, -4.0), (Rimshot, 0, 1.0, 80.0, 60.0, 60.0, -7.0), (Hihat, 2, 0.0, 280.0, 60.0, 60.0, -8.0),
        (Clap, 1, 0.0, 280.0, 55.0, 50.0, -5.0), (Tom, 2, 0.0, 350.0, 55.0, 40.0, -6.0), (Perc, 0, 3.0, 180.0, 40.0, 50.0, -9.0), (Cymbal, 2, 0.0, 900.0, 60.0, 60.0, -10.0), (Fx, 1, 0.0, 900.0, 50.0, 50.0, -9.0),
    ]),
    d("Kits", "Electro Punch", &["electro", "kit", "punchy"], &[
        (Kick, 1, 0.0, 420.0, 60.0, 60.0, -3.0), (Snare, 2, 1.0, 180.0, 70.0, 70.0, -4.0), (Rimshot, 1, 0.0, 60.0, 55.0, 70.0, -6.0), (Hihat, 1, 0.0, 220.0, 65.0, 60.0, -8.0),
        (Clap, 2, 0.0, 380.0, 55.0, 40.0, -5.0), (Tom, 2, 2.0, 300.0, 70.0, 50.0, -6.0), (Perc, 0, 0.0, 200.0, 60.0, 60.0, -8.0), (Cymbal, 1, 0.0, 1400.0, 65.0, 40.0, -10.0), (Fx, 0, 0.0, 450.0, 70.0, 70.0, -8.0),
    ]),
    d("Kits", "Deep House", &["house", "kit", "deep", "warm"], &[
        (Kick, 0, -2.0, 520.0, 35.0, 25.0, -3.0), (Snare, 1, -1.0, 240.0, 45.0, 40.0, -5.0), (Rimshot, 1, -2.0, 90.0, 40.0, 40.0, -7.0), (Hihat, 0, -1.0, 260.0, 40.0, 30.0, -9.0),
        (Clap, 2, 0.0, 420.0, 40.0, 60.0, -5.0), (Tom, 1, -2.0, 480.0, 55.0, 30.0, -6.0), (Perc, 1, -3.0, 160.0, 40.0, 40.0, -8.0), (Cymbal, 1, -1.0, 2200.0, 40.0, 30.0, -11.0), (Fx, 1, -2.0, 1500.0, 35.0, 30.0, -10.0),
    ]),
    d("Kits", "Techno Warehouse", &["techno", "kit", "hard", "dark"], &[
        (Kick, 2, -2.0, 340.0, 75.0, 40.0, -2.0), (Snare, 2, -2.0, 160.0, 55.0, 55.0, -5.0), (Rimshot, 0, -2.0, 70.0, 70.0, 60.0, -6.0), (Hihat, 2, 0.0, 200.0, 70.0, 70.0, -8.0),
        (Clap, 1, -1.0, 260.0, 50.0, 40.0, -6.0), (Tom, 0, -3.0, 320.0, 60.0, 40.0, -6.0), (Perc, 2, -4.0, 160.0, 40.0, 60.0, -9.0), (Cymbal, 0, 0.0, 1500.0, 70.0, 70.0, -10.0), (Fx, 2, -3.0, 600.0, 60.0, 50.0, -9.0),
    ]),
    d("Kits", "Trap Heat", &["trap", "kit", "808", "bass"], &[
        (Kick, 0, -7.0, 1500.0, 30.0, 20.0, -2.0), (Snare, 0, 2.0, 200.0, 70.0, 80.0, -4.0), (Rimshot, 0, 3.0, 60.0, 60.0, 50.0, -7.0), (Hihat, 1, 2.0, 250.0, 75.0, 50.0, -7.0),
        (Clap, 0, 1.0, 300.0, 60.0, 40.0, -4.0), (Tom, 0, -5.0, 700.0, 30.0, 20.0, -6.0), (Perc, 2, 4.0, 220.0, 60.0, 50.0, -9.0), (Cymbal, 2, 2.0, 800.0, 80.0, 60.0, -10.0), (Fx, 0, 0.0, 800.0, 40.0, 60.0, -8.0),
    ]),
    d("Kits", "Lo-Fi Dusty", &["lofi", "kit", "dusty", "chill"], &[
        (Kick, 0, 1.0, 300.0, 20.0, 15.0, -3.0), (Snare, 1, -3.0, 160.0, 30.0, 35.0, -5.0), (Rimshot, 1, -3.0, 80.0, 25.0, 30.0, -7.0), (Hihat, 1, -2.0, 140.0, 25.0, 30.0, -9.0),
        (Clap, 1, -1.0, 220.0, 30.0, 70.0, -6.0), (Tom, 1, 0.0, 380.0, 40.0, 30.0, -6.0), (Perc, 1, -2.0, 140.0, 35.0, 30.0, -8.0), (Cymbal, 1, -2.0, 1100.0, 25.0, 25.0, -12.0), (Fx, 2, -4.0, 1200.0, 30.0, 40.0, -10.0),
    ]),
    d("Kits", "DnB Rollers", &["dnb", "jungle", "kit", "fast"], &[
        (Kick, 1, 2.0, 220.0, 70.0, 70.0, -3.0), (Snare, 0, 3.0, 150.0, 70.0, 75.0, -3.0), (Rimshot, 0, 4.0, 50.0, 70.0, 60.0, -7.0), (Hihat, 1, 3.0, 160.0, 80.0, 60.0, -8.0),
        (Clap, 1, 2.0, 200.0, 65.0, 40.0, -6.0), (Tom, 2, 3.0, 240.0, 65.0, 40.0, -6.0), (Perc, 0, 5.0, 130.0, 50.0, 40.0, -9.0), (Cymbal, 2, 3.0, 700.0, 75.0, 55.0, -10.0), (Fx, 0, 3.0, 400.0, 75.0, 70.0, -8.0),
    ]),
    d("Kits", "Minimal Clicks", &["minimal", "kit", "clicks", "tight"], &[
        (Kick, 1, 2.0, 160.0, 30.0, 30.0, -3.0), (Snare, 1, 3.0, 100.0, 30.0, 25.0, -6.0), (Rimshot, 0, 2.0, 40.0, 60.0, 80.0, -5.0), (Hihat, 0, 3.0, 140.0, 70.0, 20.0, -9.0),
        (Clap, 1, 2.0, 130.0, 45.0, 30.0, -7.0), (Tom, 0, 3.0, 180.0, 40.0, 60.0, -7.0), (Perc, 1, 4.0, 90.0, 55.0, 70.0, -7.0), (Cymbal, 2, 3.0, 500.0, 60.0, 30.0, -12.0), (Fx, 2, 2.0, 300.0, 40.0, 40.0, -10.0),
    ]),
    d("Kits", "Big Room Stadium", &["edm", "kit", "big room", "festival"], &[
        (Kick, 2, -1.0, 420.0, 80.0, 65.0, -2.0), (Snare, 0, -1.0, 340.0, 60.0, 90.0, -3.0), (Rimshot, 0, 0.0, 90.0, 60.0, 60.0, -7.0), (Hihat, 1, 0.0, 300.0, 70.0, 50.0, -8.0),
        (Clap, 2, -1.0, 600.0, 55.0, 70.0, -3.0), (Tom, 0, -4.0, 600.0, 45.0, 40.0, -6.0), (Perc, 0, 0.0, 250.0, 50.0, 50.0, -9.0), (Cymbal, 0, 0.0, 3200.0, 60.0, 70.0, -9.0), (Fx, 1, 0.0, 2200.0, 70.0, 40.0, -8.0),
    ]),
    // Kicks.
    d("Kicks", "Sub Boom", &["kick", "sub", "808", "long"], &[(Kick, 0, -5.0, 1600.0, 20.0, 10.0, -2.0)]),
    d("Kicks", "Tight Punch", &["kick", "punchy", "tight"], &[(Kick, 1, 1.0, 240.0, 65.0, 70.0, -3.0)]),
    d("Kicks", "Hard Techno Thud", &["kick", "techno", "distorted"], &[(Kick, 2, -2.0, 360.0, 90.0, 45.0, -3.0)]),
    d("Kicks", "Soft Pillow", &["kick", "soft", "warm", "lofi"], &[(Kick, 0, 0.0, 280.0, 5.0, 0.0, -3.0)]),
    d("Kicks", "Click Beater", &["kick", "click", "clicky", "house"], &[(Kick, 1, 3.0, 300.0, 40.0, 100.0, -4.0)]),
    // Snares.
    d("Snares", "Fat Crack", &["snare", "fat", "pop"], &[(Snare, 0, -1.0, 300.0, 60.0, 80.0, -3.0)]),
    d("Snares", "Wood Tom Snare", &["snare", "modal", "acoustic"], &[(Snare, 1, -2.0, 280.0, 40.0, 55.0, -4.0)]),
    d("Snares", "Electro Whip", &["snare", "electro", "snappy"], &[(Snare, 2, 3.0, 140.0, 80.0, 90.0, -4.0)]),
    d("Snares", "Tight Ghost", &["snare", "ghost", "short"], &[(Snare, 0, 4.0, 90.0, 40.0, 40.0, -9.0)]),
    d("Snares", "Gated Wall", &["snare", "80s", "big", "gated"], &[(Snare, 1, -4.0, 520.0, 55.0, 85.0, -3.0)]),
    // Hats.
    d("Hats", "Crisp Metal", &["hat", "metal", "bright"], &[(Hihat, 0, 2.0, 280.0, 75.0, 50.0, -7.0)]),
    d("Hats", "Airy Noise", &["hat", "noise", "airy"], &[(Hihat, 1, 0.0, 380.0, 55.0, 30.0, -8.0)]),
    d("Hats", "Dirty Crunch", &["hat", "crunch", "gritty", "industrial"], &[(Hihat, 2, -2.0, 240.0, 40.0, 90.0, -8.0)]),
    d("Hats", "Pocket Tick", &["hat", "short", "minimal"], &[(Hihat, 1, 3.0, 120.0, 85.0, 70.0, -8.0)]),
    // Claps.
    d("Claps", "Wide Clap", &["clap", "wide", "room"], &[(Clap, 2, 0.0, 700.0, 50.0, 85.0, -4.0)]),
    d("Claps", "Snap Clap", &["clap", "tight", "snap"], &[(Clap, 1, 2.0, 150.0, 70.0, 20.0, -5.0)]),
    d("Claps", "Classic Hand", &["clap", "classic", "808"], &[(Clap, 0, -1.0, 350.0, 35.0, 55.0, -4.0)]),
    // Toms.
    d("Toms", "Floor Thump", &["tom", "low", "deep"], &[(Tom, 0, -4.0, 800.0, 30.0, 25.0, -4.0)]),
    d("Toms", "Skin Rolls", &["tom", "acoustic", "skin"], &[(Tom, 1, 0.0, 500.0, 70.0, 45.0, -4.0)]),
    d("Toms", "Laser Toms", &["tom", "electro", "synth"], &[(Tom, 2, 2.0, 380.0, 90.0, 20.0, -5.0)]),
    m("Toms", "Tom Melody", &["tom", "melodic", "mono", "tonal"], Tom, &[(Tom, 0, 0.0, 600.0, 15.0, 10.0, -4.0)]),
    // Percussion (with rimshots).
    d("Percussion", "Rim Knock", &["rimshot", "rim", "classic"], &[(Rimshot, 0, 0.0, 100.0, 50.0, 55.0, -5.0)]),
    d("Percussion", "Wood Click", &["rimshot", "wood", "woodblock"], &[(Rimshot, 1, 1.0, 110.0, 60.0, 60.0, -5.0)]),
    d("Percussion", "Cowbell Groove", &["perc", "cowbell", "latin"], &[(Perc, 2, 0.0, 380.0, 55.0, 40.0, -6.0)]),
    d("Percussion", "FM Bongos", &["perc", "fm", "bongo"], &[(Perc, 0, 3.0, 170.0, 35.0, 30.0, -5.0)]),
    m("Percussion", "Marimba Blocks", &["perc", "mallet", "melodic", "mono"], Perc, &[(Perc, 1, 0.0, 420.0, 40.0, 30.0, -5.0)]),
    // Cymbals.
    d("Cymbals", "Crash Wash", &["cymbal", "crash", "long"], &[(Cymbal, 0, -1.0, 3600.0, 45.0, 65.0, -8.0)]),
    d("Cymbals", "Ride Ping", &["cymbal", "ride", "bell", "jazz"], &[(Cymbal, 1, 0.0, 1800.0, 60.0, 30.0, -8.0)]),
    d("Cymbals", "Splash Burst", &["cymbal", "splash", "bright", "short"], &[(Cymbal, 2, 2.0, 800.0, 80.0, 55.0, -9.0)]),
    // FX.
    d("FX", "Laser Zap", &["fx", "zap", "laser", "sci-fi"], &[(Fx, 0, 0.0, 450.0, 85.0, 40.0, -6.0)]),
    d("FX", "Riser Whoosh", &["fx", "sweep", "riser", "transition"], &[(Fx, 1, 0.0, 2600.0, 80.0, 35.0, -8.0)]),
    d("FX", "Down Sweep", &["fx", "sweep", "impact", "transition"], &[(Fx, 1, -6.0, 1200.0, 25.0, 55.0, -8.0)]),
    m("FX", "Bleep Keys", &["fx", "bleep", "arp", "melodic", "mono"], Fx, &[(Fx, 2, 0.0, 600.0, 55.0, 50.0, -6.0)]),
];

fn main() {
    let root = factory_dir();
    let mut n = 0;
    for def in DEFS {
        let mut b = PresetBuilder::new(ds::ID, def.cat, def.name).tags(def.tags);
        if let Some(kind) = def.mono {
            b = b.set(p::MODE, 1.0).set(p::SELECT, kind.index() as f32);
        } else if let Some(&(kind, ..)) = def.types.first() {
            // Kit mode, with the first type shown in the editor.
            b = b.set(p::SELECT, kind.index() as f32);
        }
        for &(kind, model, tune, decay, tone, snap, level) in def.types {
            let k = kind.index();
            b = b
                .set(p::param(k, p::MODEL), model as f32)
                .set(p::param(k, p::TUNE), tune)
                .set(p::param(k, p::DECAY), decay)
                .set(p::param(k, p::TONE), tone)
                .set(p::param(k, p::SNAP), snap)
                .set(p::param(k, p::LEVEL), level);
        }
        b.write(&root).expect("write preset");
        n += 1;
    }
    println!("wrote {n} presets of {}", ds::ID);
}
