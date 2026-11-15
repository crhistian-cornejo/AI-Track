//! Generates the factory presets of midi.echo, midi.random, midi.length and midi.pitch.
//! `cargo run -p aitrack-stock --example presets_midi_echo_random_length_pitch`

use aitrack_stock::midi::{echo, length, pitch, random};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

// Echo: Time divisions are 1/32, 1/16T, 1/32., 1/16, 1/8T, 1/16., 1/8, 1/4T, 1/8., 1/4, 1/2T, 1/4., 1/2, 1 bar.
const D_16: f32 = 3.0;
const D_8T: f32 = 4.0;
const D_16_DOT: f32 = 5.0;
const D_8: f32 = 6.0;
const D_8_DOT: f32 = 8.0;
const D_4_DOT: f32 = 11.0;
// Loop: Off, 1 bar, 2 bars, 4 bars, 8 bars.
const LOOP_1: f32 = 1.0;
const LOOP_2: f32 = 2.0;
const LOOP_4: f32 = 3.0;
// Scales: 1 = Minor, 10 = Minor Pentatonic. Roots: 0 = C, 9 = A.
const MINOR_PENT: f32 = 10.0;

fn echo(cat: &str, name: &str, tags: &[&str]) -> PresetBuilder {
    PresetBuilder::new(echo::ID, cat, name).tags(tags)
}

fn random(cat: &str, name: &str, tags: &[&str]) -> PresetBuilder {
    PresetBuilder::new(random::ID, cat, name).tags(tags)
}

fn length(cat: &str, name: &str, tags: &[&str]) -> PresetBuilder {
    PresetBuilder::new(length::ID, cat, name).tags(tags)
}

fn pitch(cat: &str, name: &str, tags: &[&str]) -> PresetBuilder {
    PresetBuilder::new(pitch::ID, cat, name).tags(tags)
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    use echo::{length as el, p as e};
    use length::{mode, p as l};
    use pitch::{outside, overflow, p as pi};
    use random::{p as r, sign};

    let presets = [
        // ───────── midi.echo
        echo("House", "Offbeat Dotted Stab", &["house", "dotted", "stab", "chords"]).set(e::DIVISION, D_8_DOT).set(e::REPEATS, 3.0).set(e::FEEDBACK, 60.0),
        echo("House", "Chord Ghosts", &["house", "chords", "ghost", "soft"])
            .set(e::DIVISION, D_8)
            .set(e::REPEATS, 2.0)
            .set(e::FEEDBACK, 40.0)
            .set(e::VEL_RAND, 10.0),
        echo("House", "Triplet Shuffle", &["house", "triplet", "shuffle", "swing"])
            .set(e::DIVISION, D_8T)
            .set(e::REPEATS, 3.0)
            .set(e::FEEDBACK, 65.0)
            .set(e::CHANCE, 85.0)
            .set(e::VEL_RAND, 15.0)
            .set(e::SEED, 5.0),
        echo("Techno", "Dub Pings", &["techno", "dub", "dotted", "ping"])
            .set(e::DIVISION, D_4_DOT)
            .set(e::REPEATS, 5.0)
            .set(e::FEEDBACK, 75.0)
            .set(e::LENGTH, el::INTERVAL)
            .set(e::GATE, 25.0),
        echo("Techno", "Rolling Sixteenths", &["techno", "roll", "sixteenth", "groove"])
            .set(e::DIVISION, D_16)
            .set(e::REPEATS, 3.0)
            .set(e::FEEDBACK, 55.0)
            .set(e::VEL_STEP, -4.0)
            .set(e::LENGTH, el::INTERVAL)
            .set(e::GATE, 50.0),
        echo("Techno", "Rise And Hold", &["techno", "riser", "build", "crescendo"])
            .set(e::DIVISION, D_16)
            .set(e::REPEATS, 8.0)
            .set(e::FEEDBACK, 100.0)
            .set(e::VEL_STEP, 10.0)
            .set(e::LENGTH, el::INTERVAL)
            .set(e::GATE, 80.0),
        echo("Bass", "Octave Bounce", &["bass", "octave", "bounce"]).set(e::DIVISION, D_8).set(e::REPEATS, 2.0).set(e::FEEDBACK, 80.0).set(e::PITCH, 12.0),
        echo("Bass", "Stacked Fifths", &["bass", "fifth", "stack"])
            .set(e::DIVISION, D_16_DOT)
            .set(e::REPEATS, 2.0)
            .set(e::FEEDBACK, 70.0)
            .set(e::PITCH, 7.0)
            .set(e::LENGTH, el::INTERVAL)
            .set(e::GATE, 90.0),
        echo("Creative", "Scatter", &["creative", "random", "scatter", "seed"])
            .set(e::DIVISION, D_16)
            .set(e::REPEATS, 6.0)
            .set(e::FEEDBACK, 80.0)
            .set(e::CHANCE, 70.0)
            .set(e::TIME_RAND, 15.0)
            .set(e::PITCH_RAND, 5.0)
            .set(e::VEL_RAND, 30.0)
            .set(e::SEED, 11.0)
            .set(e::LOOP, LOOP_2),
        echo("Creative", "Cascade Up", &["creative", "cascade", "arp", "pitch"])
            .set(e::DIVISION, D_16)
            .set(e::REPEATS, 8.0)
            .set(e::FEEDBACK, 90.0)
            .set(e::PITCH, 3.0)
            .set(e::LENGTH, el::INTERVAL)
            .set(e::GATE, 70.0),
        // ───────── midi.random
        random("Bass", "Octave Jumps", &["bass", "octave", "jump", "pattern"])
            .set(r::CHANCE, 30.0)
            .set(r::INTERVAL, 12.0)
            .set(r::CHOICES, 1.0)
            .set(r::SEED, 3.0)
            .set(r::LOOP, LOOP_1),
        random("Bass", "Sub Drops", &["bass", "sub", "drop", "octave"])
            .set(r::CHANCE, 25.0)
            .set(r::INTERVAL, 12.0)
            .set(r::CHOICES, 2.0)
            .set(r::SIGN, sign::DOWN)
            .set(r::SEED, 8.0)
            .set(r::LOOP, LOOP_2),
        random("House", "Human Feel", &["house", "humanize", "velocity", "timing"]).set(r::CHOICES, 0.0).set(r::VELOCITY, 18.0).set(r::DELAY, 12.0),
        random("House", "Pentatonic Scatter", &["house", "lead", "pentatonic", "scale"])
            .set(r::CHANCE, 60.0)
            .set(r::INTERVAL, 3.0)
            .set(r::CHOICES, 5.0)
            .set(r::SNAP, 1.0)
            .set(r::ROOT, 9.0)
            .set(r::SCALE, MINOR_PENT)
            .set(r::VELOCITY, 15.0)
            .set(r::SEED, 21.0),
        random("Techno", "Loose Hats", &["techno", "hats", "humanize", "groove"]).set(r::CHOICES, 0.0).set(r::VELOCITY, 30.0).set(r::DELAY, 20.0),
        random("Techno", "Acid Variation", &["techno", "acid", "variation", "scale"])
            .set(r::CHANCE, 45.0)
            .set(r::INTERVAL, 2.0)
            .set(r::CHOICES, 4.0)
            .set(r::SNAP, 1.0)
            .set(r::ROOT, 0.0)
            .set(r::SCALE, MINOR_PENT)
            .set(r::VELOCITY, 20.0)
            .set(r::SEED, 7.0)
            .set(r::LOOP, LOOP_4),
        random("Creative", "Chromatic Chaos", &["creative", "chaos", "chromatic", "random"])
            .set(r::INTERVAL, 1.0)
            .set(r::CHOICES, 12.0)
            .set(r::VELOCITY, 40.0)
            .set(r::DELAY, 40.0)
            .set(r::SEED, 99.0),
        random("Creative", "Sometimes A Fifth", &["creative", "fifth", "interval", "up"])
            .set(r::CHANCE, 35.0)
            .set(r::INTERVAL, 7.0)
            .set(r::CHOICES, 1.0)
            .set(r::SIGN, sign::UP)
            .set(r::SEED, 4.0)
            .set(r::LOOP, LOOP_2),
        // ───────── midi.length
        length("House", "Short Stabs", &["house", "stab", "short", "sixteenth"]).set(l::MODE, mode::SYNC).set(l::DIVISION, D_16).set(l::GATE, 70.0),
        length("House", "Gated Pad Eighths", &["house", "pad", "gate", "eighth"]).set(l::MODE, mode::SYNC).set(l::DIVISION, D_8).set(l::GATE, 50.0),
        length("Techno", "Tight Ticks", &["techno", "tick", "short", "percussive"]).set(l::MODE, mode::SYNC).set(l::DIVISION, 0.0).set(l::GATE, 100.0),
        length("Techno", "Rolling Gate", &["techno", "gate", "rolling", "sixteenth"]).set(l::MODE, mode::SYNC).set(l::DIVISION, D_16).set(l::GATE, 40.0),
        length("Bass", "Staccato Bass", &["bass", "staccato", "tight", "fixed"]).set(l::MODE, mode::FIXED).set(l::TIME, 60.0).set(l::GATE, 100.0),
        length("Bass", "Legato Acid", &["bass", "acid", "legato", "slide"]).set(l::MODE, mode::LEGATO).set(l::MAX, 1_000.0).set(l::MIN, 30.0),
        length("Bass", "Long Tails", &["bass", "sustain", "stretch", "tail"]).set(l::MODE, mode::STRETCH).set(l::STRETCH, 3.0).set(l::MAX, 2_000.0),
        length("Creative", "No Short Notes", &["creative", "clean", "minimum", "length"]).set(l::MODE, mode::STRETCH).set(l::STRETCH, 1.0).set(l::MIN, 80.0),
        // ───────── midi.pitch
        pitch("Bass", "Octave Down Sub", &["bass", "sub", "octave", "down"]).set(pi::OCTAVE, -1.0),
        pitch("Bass", "Octave Layer Sub", &["bass", "sub", "layer", "octave"]).set(pi::OCTAVE, -1.0).set(pi::LAYER, 1.0).set(pi::LAYER_LEVEL, 70.0),
        pitch("Bass", "Low Notes Down", &["bass", "range", "octave", "low"]).set(pi::OCTAVE, -1.0).set(pi::LOWEST, 0.0).set(pi::WIDTH, 48.0),
        pitch("House", "Octave Up Stabs", &["house", "stab", "octave", "up"]).set(pi::OCTAVE, 1.0),
        pitch("House", "Fifth Layer", &["house", "chords", "fifth", "layer"]).set(pi::SEMITONE, 7.0).set(pi::LAYER, 1.0).set(pi::LAYER_LEVEL, 60.0),
        pitch("Techno", "Minor Third Up", &["techno", "third", "minor", "transpose"]).set(pi::SEMITONE, 3.0),
        pitch("Techno", "Lead Range Only", &["techno", "lead", "range", "octave"]).set(pi::OCTAVE, 1.0).set(pi::LOWEST, 60.0).set(pi::WIDTH, 36.0).set(pi::OUTSIDE, outside::PASS),
        pitch("Creative", "Octave Doubler", &["creative", "double", "layer", "octave"]).set(pi::OCTAVE, 1.0).set(pi::LAYER, 1.0).set(pi::LAYER_LEVEL, 50.0),
        pitch("Creative", "Fold Up Two Octaves", &["creative", "fold", "octave", "transpose"]).set(pi::OCTAVE, 2.0).set(pi::OVERFLOW, overflow::FOLD),
    ];
    for b in presets {
        let path = b.write(&root)?;
        println!("{}", path.display());
    }
    Ok(())
}
