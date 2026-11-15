//! Generates the factory presets of midi.arp, midi.chord, midi.scale and midi.velocity.
//! `cargo run -p aitrack-stock --example presets_midi`

use aitrack_stock::midi::{arp, chord, scale, velocity};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

// Arpeggiator styles.
const UP: f32 = 0.0;
const DOWN: f32 = 1.0;
const UP_DOWN: f32 = 2.0;
const UP_AND_DOWN: f32 = 4.0;
const CONVERGE: f32 = 6.0;
const LOW_PEDAL: f32 = 8.0;
const HIGH_PEDAL: f32 = 9.0;
const PLAY_ORDER: f32 = 10.0;
const CHORD: f32 = 11.0;
const RANDOM_OTHER: f32 = 13.0;
// Rates: 1/2, 1/4, 1/4., 1/4T, 1/8, 1/8., 1/8T, 1/16, 1/16., 1/16T, 1/32, 1/32., 1/32T.
const R_4: f32 = 1.0;
const R_8: f32 = 4.0;
const R_8_DOT: f32 = 5.0;
const R_8T: f32 = 6.0;
const R_16: f32 = 7.0;
const R_16_DOT: f32 = 8.0;
const R_16T: f32 = 9.0;
// Retrigger: Off, Note, Beat, Bar.
const RETRIG_OFF: f32 = 0.0;
const RETRIG_NOTE: f32 = 1.0;
const RETRIG_BAR: f32 = 3.0;
// Velocity modes of the arpeggiator: Input, Fixed, Steps.
const VEL_STEPS: f32 = 2.0;

fn a(cat: &str, name: &str) -> PresetBuilder {
    PresetBuilder::new(arp::ID, cat, name)
}

/// Eight step velocities (1..127) and how many of them are used.
fn steps(b: PresetBuilder, v: &[f32]) -> PresetBuilder {
    let mut b = b.set(arp::p::VEL_MODE, VEL_STEPS).set(arp::p::VEL_LEN, v.len() as f32);
    for (i, x) in v.iter().enumerate() {
        b = b.set(arp::p::VEL_STEP0 + i as u32, *x);
    }
    b
}

fn arp_presets() -> Vec<PresetBuilder> {
    use arp::p;
    vec![
        // House
        a("House", "Offbeat Stab Chord").tags(&["house", "stab", "offbeat", "chord"]).set(p::STYLE, CHORD).set(p::RATE, R_4).set(p::OFFSET, 50.0).set(p::GATE, 38.0).set(p::RETRIGGER, RETRIG_OFF),
        steps(a("House", "Piano House Upbeat").tags(&["house", "piano", "groove", "swing"]).set(p::STYLE, UP).set(p::RATE, R_8).set(p::GATE, 60.0).set(p::OCTAVES, 2.0).set(p::SWING, 30.0), &[110.0, 70.0, 95.0, 70.0, 105.0, 70.0, 95.0, 80.0]),
        a("House", "Deep Dotted Eighths").tags(&["house", "deep", "dotted", "pad"]).set(p::STYLE, UP_DOWN).set(p::RATE, R_8_DOT).set(p::GATE, 75.0).set(p::OCTAVES, 2.0),
        a("House", "Disco Converge").tags(&["house", "disco", "funk", "converge"]).set(p::STYLE, CONVERGE).set(p::RATE, R_16).set(p::GATE, 45.0).set(p::SWING, 22.0).set(p::OCTAVES, 2.0).set(p::RETRIGGER, RETRIG_BAR),
        // Techno
        steps(a("Techno", "Pluck 16ths Accented").tags(&["techno", "pluck", "16th", "accent"]).set(p::STYLE, UP).set(p::RATE, R_16).set(p::GATE, 30.0).set(p::RETRIGGER, RETRIG_OFF), &[127.0, 60.0, 90.0, 60.0, 112.0, 60.0, 90.0, 72.0]),
        steps(a("Techno", "Rolling Random Other").tags(&["techno", "rolling", "random", "minimal"]).set(p::STYLE, RANDOM_OTHER).set(p::RATE, R_16).set(p::GATE, 50.0).set(p::OCTAVES, 2.0).set(p::RETRIGGER, RETRIG_OFF).set(p::SEED, 4.0), &[120.0, 75.0, 90.0, 75.0]),
        a("Techno", "Hypnotic Triplets").tags(&["techno", "hypnotic", "triplet", "loop"]).set(p::STYLE, UP_DOWN).set(p::RATE, R_16T).set(p::GATE, 55.0).set(p::OCTAVES, 2.0).set(p::RETRIGGER, RETRIG_BAR),
        a("Techno", "Rave Stab Dotted").tags(&["techno", "rave", "stab", "chord", "dotted"]).set(p::STYLE, CHORD).set(p::RATE, R_16_DOT).set(p::GATE, 28.0).set(p::RETRIGGER, RETRIG_BAR),
        // Bass
        steps(a("Bass", "Octave Pulse").tags(&["bass", "octave", "pulse", "techno", "house"]).set(p::STYLE, UP).set(p::RATE, R_16).set(p::GATE, 62.0).set(p::OCTAVES, 2.0).set(p::RETRIGGER, RETRIG_OFF), &[127.0, 85.0, 105.0, 85.0, 120.0, 85.0, 105.0, 90.0]),
        a("Bass", "Acid Walker").tags(&["bass", "acid", "play order", "swing"]).set(p::STYLE, PLAY_ORDER).set(p::RATE, R_16).set(p::GATE, 55.0).set(p::SWING, 40.0).set(p::RETRIGGER, RETRIG_NOTE),
        a("Bass", "Sub Roller Triplets").tags(&["bass", "sub", "triplet", "roller"]).set(p::STYLE, DOWN).set(p::RATE, R_8T).set(p::GATE, 80.0).set(p::OCTAVES, 2.0).set(p::DISTANCE, -12.0),
        a("Bass", "Reese Gallop").tags(&["bass", "reese", "gallop", "dnb"]).set(p::STYLE, HIGH_PEDAL).set(p::RATE, R_16).set(p::GATE, 45.0).set(p::OCTAVES, 2.0),
        a("Bass", "Low Pedal Drive").tags(&["bass", "pedal", "driving", "techno"]).set(p::STYLE, LOW_PEDAL).set(p::RATE, R_16).set(p::GATE, 40.0).set(p::RETRIGGER, RETRIG_OFF),
        // Pads
        a("Pads", "Held Wave").tags(&["pad", "hold", "ambient", "house"]).set(p::STYLE, UP_AND_DOWN).set(p::RATE, R_8_DOT).set(p::GATE, 100.0).set(p::OCTAVES, 3.0).set(p::HOLD, 1.0),
        a("Pads", "Trance Gate Climb").tags(&["trance", "techno", "climb", "gate"]).set(p::STYLE, UP_DOWN).set(p::RATE, R_16).set(p::GATE, 50.0).set(p::OCTAVES, 3.0).set(p::RETRIGGER, RETRIG_NOTE),
    ]
}

fn c(cat: &str, name: &str) -> PresetBuilder {
    PresetBuilder::new(chord::ID, cat, name)
}

/// Voices as `(interval, level %)`; the rest are switched off.
fn voices(mut b: PresetBuilder, v: &[(f32, f32)]) -> PresetBuilder {
    for i in 0..chord::VOICES {
        match v.get(i) {
            Some((interval, level)) => b = b.set(chord::p::on(i), 1.0).set(chord::p::interval(i), *interval).set(chord::p::level(i), *level),
            None => b = b.set(chord::p::on(i), 0.0).set(chord::p::interval(i), 0.0).set(chord::p::level(i), 100.0),
        }
    }
    b
}

fn chord_presets() -> Vec<PresetBuilder> {
    use chord::p;
    vec![
        voices(c("House", "Minor Seventh Stab").tags(&["house", "minor", "seventh", "stab"]), &[(0.0, 100.0), (3.0, 90.0), (7.0, 90.0), (10.0, 80.0)]),
        voices(c("House", "Dominant Ninth").tags(&["house", "dominant", "ninth", "soul"]), &[(0.0, 100.0), (4.0, 85.0), (7.0, 85.0), (10.0, 80.0), (14.0, 70.0)]),
        voices(c("House", "Suspended Fourth").tags(&["house", "sus4", "open", "piano"]), &[(0.0, 100.0), (5.0, 85.0), (7.0, 85.0), (12.0, 70.0)]),
        voices(c("House", "Major Seventh Dream").tags(&["house", "major", "seventh", "deep"]), &[(0.0, 100.0), (4.0, 80.0), (7.0, 80.0), (11.0, 75.0), (16.0, 55.0)]),
        voices(c("Techno", "Minor Rave Stack").tags(&["techno", "rave", "minor", "stab"]), &[(0.0, 100.0), (3.0, 80.0), (7.0, 80.0), (12.0, 60.0)]),
        voices(c("Techno", "Power Fifth").tags(&["techno", "power", "fifth", "dark"]), &[(0.0, 100.0), (7.0, 90.0), (12.0, 70.0)]),
        voices(c("Techno", "Phrygian Cluster").tags(&["techno", "phrygian", "dark", "cluster"]), &[(0.0, 100.0), (1.0, 70.0), (7.0, 90.0), (13.0, 60.0)]).set(p::FIT, 1.0).set(p::ROOT, 0.0).set(p::SCALE, 5.0),
        voices(c("Bass", "Octave Sub").tags(&["bass", "sub", "octave"]), &[(0.0, 100.0), (-12.0, 85.0), (12.0, 35.0)]),
        voices(c("Bass", "Fifth Below Weight").tags(&["bass", "fifth", "weight", "techno"]), &[(0.0, 100.0), (-5.0, 75.0), (-12.0, 90.0)]),
        voices(c("Scale", "Diatonic Triad Minor").tags(&["scale", "diatonic", "minor", "triad"]), &[(0.0, 100.0), (4.0, 85.0), (7.0, 85.0)]).set(p::FIT, 1.0).set(p::ROOT, 0.0).set(p::SCALE, 1.0),
        voices(c("Scale", "Diatonic Seventh Dorian").tags(&["scale", "dorian", "seventh", "house"]), &[(0.0, 100.0), (4.0, 85.0), (7.0, 85.0), (11.0, 75.0)]).set(p::FIT, 1.0).set(p::ROOT, 9.0).set(p::SCALE, 4.0),
    ]
}

fn s(cat: &str, name: &str) -> PresetBuilder {
    PresetBuilder::new(scale::ID, cat, name)
}

fn scale_presets() -> Vec<PresetBuilder> {
    use scale::p;
    vec![
        s("House", "C Minor Keeper").tags(&["house", "minor", "nearest"]).set(p::ROOT, 0.0).set(p::SCALE, 1.0),
        s("House", "A Dorian Groove").tags(&["house", "dorian", "groove"]).set(p::ROOT, 9.0).set(p::SCALE, 4.0),
        s("House", "F Major Pentatonic Lead").tags(&["house", "pentatonic", "lead", "major"]).set(p::ROOT, 5.0).set(p::SCALE, 9.0).set(p::MODE, 1.0),
        s("Techno", "E Phrygian Dark").tags(&["techno", "phrygian", "dark"]).set(p::ROOT, 4.0).set(p::SCALE, 5.0),
        s("Techno", "A Harmonic Minor Rave").tags(&["techno", "rave", "harmonic minor"]).set(p::ROOT, 9.0).set(p::SCALE, 2.0).set(p::MODE, 2.0),
        s("Techno", "D Locrian Tension").tags(&["techno", "locrian", "tension"]).set(p::ROOT, 2.0).set(p::SCALE, 8.0),
        s("Bass", "Blues Bass Upward").tags(&["bass", "blues", "up"]).set(p::ROOT, 0.0).set(p::SCALE, 11.0).set(p::MODE, 1.0),
        s("Bass", "Minor Pentatonic Low Range").tags(&["bass", "pentatonic", "range", "minor"]).set(p::ROOT, 9.0).set(p::SCALE, 10.0).set(p::LOWEST, 24.0).set(p::RANGE, 36.0),
        s("Utility", "Diatonic Step Up").tags(&["utility", "diatonic", "step", "transpose"]).set(p::ROOT, 0.0).set(p::SCALE, 0.0).set(p::MODE, 1.0).set(p::PASS, 0.0),
        s("Utility", "Fifth Up Corrected").tags(&["utility", "transpose", "fifth", "minor"]).set(p::ROOT, 0.0).set(p::SCALE, 1.0).set(p::TRANSPOSE, 7.0),
    ]
}

fn v(cat: &str, name: &str) -> PresetBuilder {
    PresetBuilder::new(velocity::ID, cat, name)
}

fn velocity_presets() -> Vec<PresetBuilder> {
    use velocity::p;
    vec![
        v("House", "Even Out Keys").tags(&["house", "even", "compress", "keys"]).set(p::COMPAND, -60.0).set(p::PIVOT, 90.0),
        v("House", "Human Wobble").tags(&["house", "human", "random", "groove"]).set(p::RANDOM, 25.0).set(p::SEED, 7.0).set(p::MIN, 50.0),
        v("House", "Soft Touch").tags(&["house", "soft", "deep", "curve"]).set(p::CURVE, -40.0).set(p::MAX, 96.0),
        v("Techno", "Hard Hitting").tags(&["techno", "hard", "hit", "curve"]).set(p::CURVE, 50.0).set(p::GAIN, 10.0).set(p::MIN, 80.0),
        v("Techno", "Machine Fixed 110").tags(&["techno", "fixed", "machine", "tight"]).set(p::MODE, 2.0).set(p::FIXED, 110.0),
        v("Techno", "Accent Range").tags(&["techno", "accent", "range", "punch"]).set(p::MIN, 90.0).set(p::MAX, 127.0).set(p::COMPAND, -30.0),
        v("Bass", "Steady Bass").tags(&["bass", "steady", "compress", "sub"]).set(p::COMPAND, -80.0).set(p::PIVOT, 105.0),
        v("Bass", "Wide Dynamics").tags(&["bass", "dynamic", "expand", "funk"]).set(p::COMPAND, 60.0).set(p::PIVOT, 80.0),
        v("Utility", "Wide Random").tags(&["utility", "random", "variation"]).set(p::RANDOM, 60.0).set(p::SEED, 21.0).set(p::MODE, 1.0).set(p::MIN, 40.0).set(p::MAX, 120.0),
        v("Utility", "Clip To Mid").tags(&["utility", "clip", "limit"]).set(p::MODE, 1.0).set(p::MIN, 60.0).set(p::MAX, 100.0),
    ]
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let mut n = 0;
    for b in arp_presets().into_iter().chain(chord_presets()).chain(scale_presets()).chain(velocity_presets()) {
        let path = b.write(&root)?;
        println!("{}", path.strip_prefix(&root).unwrap_or(&path).display());
        n += 1;
    }
    println!("{n} presets");
    Ok(())
}
