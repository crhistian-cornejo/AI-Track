//! Generates the factory presets of fx.pitchshift, fx.freqshift and fx.vocoder.
//! `cargo run -p aitrack-stock --example presets_pitch_time`

use aitrack_stock::fx::{freqshift, pitchshift, vocoder};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

// Choice indices.
const OFF: f32 = 0.0;
const RING: f32 = 1.0;
// LFO shapes: Sine, Triangle, Saw Up, Saw Down, Square, S&H, Smooth.
const LFO_TRI: f32 = 1.0;
const LFO_SAW_UP: f32 = 2.0;
const LFO_SH: f32 = 5.0;
const LFO_SMOOTH: f32 = 6.0;
// Vocoder carriers: Sidechain, Noise, Saw, Tracked. Chords: Single, Octave, Fifth, Minor, Major, Minor 7th.
const SIDECHAIN: f32 = 0.0;
const NOISE: f32 = 1.0;
const SAW: f32 = 2.0;
const TRACKED: f32 = 3.0;
const OCTAVE: f32 = 1.0;
const FIFTH: f32 = 2.0;
const MINOR: f32 = 3.0;
const MAJOR: f32 = 4.0;
const MINOR7: f32 = 5.0;

fn pitchshift_presets() -> Vec<PresetBuilder> {
    use pitchshift::p;
    let b = |cat: &str, name: &str| PresetBuilder::new(pitchshift::ID, cat, name).tags(&["pitch"]);
    vec![
        b("Harmony", "Octave Up").tags(&["octave", "transpose", "synth", "vocal"]).set(p::SEMITONES, 12.0),
        b("Harmony", "Octave Down").tags(&["octave", "transpose", "bass", "sub"]).set(p::SEMITONES, -12.0).set(p::GRAIN, 60.0),
        b("Harmony", "Fifth Layer").tags(&["harmony", "fifth", "vocal", "pad"]).set(p::SEMITONES, 7.0).set(p::MIX, 45.0),
        b("Harmony", "Octave Double").tags(&["octave", "thicken", "lead", "vocal"]).set(p::SEMITONES, 12.0).set(p::MIX, 35.0),
        b("Harmony", "Sub Octave Blend").tags(&["bass", "sub", "808", "low end"]).set(p::SEMITONES, -12.0).set(p::GRAIN, 80.0).set(p::MIX, 50.0),
        b("Tuning", "Detune Thick").tags(&["detune", "thicken", "chorus", "wide"]).set(p::CENTS, 14.0).set(p::MIX, 50.0).set(p::GRAIN, 30.0),
        b("Tuning", "Detune Down Fine").tags(&["detune", "subtle", "tuning", "lofi"]).set(p::CENTS, -30.0).set(p::MIX, 100.0).set(p::GRAIN, 40.0),
        b("Tuning", "Semitone Shift").tags(&["transpose", "key change", "sample", "loop"]).set(p::SEMITONES, 1.0).set(p::GRAIN, 50.0),
        b("Creative", "Shimmer Up").tags(&["shimmer", "ambient", "pad", "feedback", "octave"]).set(p::SEMITONES, 12.0).set(p::FEEDBACK, 55.0).set(p::MIX, 40.0).set(p::GRAIN, 80.0),
        b("Creative", "Shimmer Fifth").tags(&["shimmer", "ambient", "feedback", "fifth", "cinematic"]).set(p::SEMITONES, 7.0).set(p::FEEDBACK, 60.0).set(p::MIX, 35.0).set(p::GRAIN, 90.0),
        b("Creative", "Falling Spiral").tags(&["feedback", "dark", "descending", "dub", "fx"]).set(p::SEMITONES, -5.0).set(p::FEEDBACK, 70.0).set(p::MIX, 45.0).set(p::GRAIN, 70.0),
        b("Creative", "Chipmunk").tags(&["vocal", "high", "fun", "fx"]).set(p::SEMITONES, 8.0).set(p::GRAIN, 25.0),
        b("Creative", "Demon Voice").tags(&["vocal", "low", "horror", "fx"]).set(p::SEMITONES, -9.0).set(p::GRAIN, 50.0),
        b("Creative", "Raw Grains").tags(&["glitch", "lofi", "grainy", "texture"]).set(p::SEMITONES, 5.0).set(p::GRAIN, 12.0).set(p::ALIGN, OFF),
    ]
}

fn freqshift_presets() -> Vec<PresetBuilder> {
    use freqshift::p;
    let b = |cat: &str, name: &str| PresetBuilder::new(freqshift::ID, cat, name).tags(&["frequency shifter"]);
    vec![
        b("Subtle", "Stereo Shimmer").tags(&["stereo", "wide", "subtle", "pad"]).set(p::FINE, 3.0).set(p::SPREAD, 100.0).set(p::MIX, 45.0),
        b("Subtle", "Slow Phase Drift").tags(&["phasing", "slow", "subtle", "pad"]).set(p::FINE, 0.5).set(p::MIX, 50.0),
        b("Subtle", "Hat Lift").tags(&["hats", "cymbals", "brighten", "drums"]).set(p::SHIFT, 120.0).set(p::MIX, 25.0),
        b("Barber Pole", "Barber Pole Up").tags(&["barber pole", "rising", "feedback", "classic"]).set(p::FINE, 2.0).set(p::FEEDBACK, 70.0).set(p::MIX, 50.0),
        b("Barber Pole", "Barber Pole Down").tags(&["barber pole", "falling", "feedback", "classic"]).set(p::FINE, -2.0).set(p::FEEDBACK, 70.0).set(p::MIX, 50.0),
        b("Barber Pole", "Wide Spiral").tags(&["barber pole", "stereo", "ambient"]).set(p::FINE, 1.5).set(p::FEEDBACK, 60.0).set(p::SPREAD, 100.0).set(p::MIX, 55.0),
        b("Inharmonic", "Bell Shift").tags(&["bell", "inharmonic", "metallic", "keys"]).set(p::SHIFT, 217.0).set(p::MIX, 50.0),
        b("Inharmonic", "Dub Fall").tags(&["dub", "echo", "feedback", "fx"]).set(p::SHIFT, -50.0).set(p::FEEDBACK, 45.0).set(p::MIX, 40.0),
        b("Inharmonic", "Alien Voice").tags(&["vocal", "alien", "sci-fi", "fx"]).set(p::SHIFT, 380.0).set(p::MIX, 100.0),
        b("Ring Mod", "Robot Ring").tags(&["robot", "ring", "vocal", "fx"]).set(p::MODE, RING).set(p::SHIFT, 440.0).set(p::MIX, 60.0),
        b("Ring Mod", "Metallic Ring").tags(&["metal", "ring", "drums", "industrial"]).set(p::MODE, RING).set(p::SHIFT, 1200.0).set(p::MIX, 45.0),
        b("Ring Mod", "Tremolo Ring").tags(&["tremolo", "ring", "wobble", "pad"]).set(p::MODE, RING).set(p::FINE, 7.0).set(p::MIX, 100.0),
        b("Modulated", "Wobble Shift").tags(&["wobble", "lfo", "movement", "bass"]).set(p::LFO_SHAPE, LFO_TRI).set(p::LFO_RATE, 0.6).set(p::LFO_DEPTH, 160.0).set(p::MIX, 50.0),
        b("Modulated", "Siren Sweep").tags(&["siren", "riser", "lfo", "fx"]).set(p::LFO_SHAPE, LFO_SAW_UP).set(p::LFO_RATE, 0.25).set(p::LFO_DEPTH, 900.0).set(p::MIX, 70.0),
        b("Modulated", "Random Alien").tags(&["random", "sample and hold", "glitch", "sci-fi"]).set(p::LFO_SHAPE, LFO_SH).set(p::LFO_RATE, 4.0).set(p::LFO_DEPTH, 500.0).set(p::MIX, 80.0),
        b("Modulated", "Drifting Choir").tags(&["smooth", "drift", "pad", "ambient"]).set(p::LFO_SHAPE, LFO_SMOOTH).set(p::LFO_RATE, 0.4).set(p::LFO_DEPTH, 12.0).set(p::SPREAD, 100.0).set(p::MIX, 55.0),
    ]
}

fn vocoder_presets() -> Vec<PresetBuilder> {
    use vocoder::p;
    let b = |cat: &str, name: &str| PresetBuilder::new(vocoder::ID, cat, name).tags(&["vocoder"]);
    vec![
        b("Classic", "Classic Robot").tags(&["robot", "classic", "vocal"]).set(p::CARRIER, SAW).set(p::BANDS, 16.0).set(p::PITCH, 45.0).set(p::CHORD, OCTAVE),
        b("Classic", "Sidechain 28").tags(&["sidechain", "synth", "vocal", "classic"]).set(p::CARRIER, SIDECHAIN).set(p::BANDS, 28.0).set(p::ATTACK, 2.0).set(p::RELEASE, 35.0),
        b("Classic", "Hi-Fi 40 Bands").tags(&["sidechain", "clear", "vocal", "intelligible"]).set(p::CARRIER, SIDECHAIN).set(p::BANDS, 40.0).set(p::BANDWIDTH, 120.0).set(p::HIGH, 12_000.0),
        b("Classic", "Lo-Fi 8 Bands").tags(&["lofi", "vintage", "chunky", "robot"]).set(p::CARRIER, SAW).set(p::BANDS, 8.0).set(p::PITCH, 43.0).set(p::LOW, 200.0).set(p::HIGH, 5000.0),
        b("Chords", "Choir Major").tags(&["choir", "chord", "pad", "vocal"]).set(p::CARRIER, SAW).set(p::BANDS, 24.0).set(p::CHORD, MAJOR).set(p::PITCH, 48.0).set(p::DETUNE, 14.0).set(p::RELEASE, 90.0),
        b("Chords", "Choir Minor").tags(&["choir", "chord", "dark", "vocal"]).set(p::CARRIER, SAW).set(p::BANDS, 24.0).set(p::CHORD, MINOR).set(p::PITCH, 45.0).set(p::DETUNE, 14.0).set(p::RELEASE, 90.0),
        b("Chords", "Fifth Power").tags(&["fifth", "power", "vocal", "lead"]).set(p::CARRIER, SAW).set(p::BANDS, 20.0).set(p::CHORD, FIFTH).set(p::PITCH, 50.0),
        b("Chords", "Minor Seventh Glass").tags(&["jazzy", "chord", "vocal", "lush"]).set(p::CARRIER, SAW).set(p::BANDS, 28.0).set(p::CHORD, MINOR7).set(p::PITCH, 52.0).set(p::DETUNE, 18.0).set(p::UNVOICED, 70.0),
        b("Tracked", "Auto Robot").tags(&["robot", "pitch tracking", "vocal", "no keyboard"]).set(p::CARRIER, TRACKED).set(p::BANDS, 20.0),
        b("Tracked", "Octave Dalek").tags(&["dalek", "robot", "low", "vocal", "sci-fi"]).set(p::CARRIER, TRACKED).set(p::BANDS, 12.0).set(p::TRANSPOSE, -12.0).set(p::FORMANT, -5.0),
        b("Tracked", "Tracked Choir").tags(&["choir", "harmony", "vocal", "pitch tracking"]).set(p::CARRIER, TRACKED).set(p::BANDS, 24.0).set(p::CHORD, MAJOR).set(p::DETUNE, 16.0),
        b("Formant", "Munchkin").tags(&["formant", "high", "vocal", "fun"]).set(p::CARRIER, TRACKED).set(p::BANDS, 24.0).set(p::FORMANT, 7.0).set(p::TRANSPOSE, 7.0),
        b("Formant", "Giant").tags(&["formant", "low", "vocal", "monster"]).set(p::CARRIER, SAW).set(p::BANDS, 20.0).set(p::FORMANT, -8.0).set(p::PITCH, 36.0),
        b("Noise", "Whisper").tags(&["whisper", "breath", "vocal", "intimate"]).set(p::CARRIER, NOISE).set(p::BANDS, 24.0).set(p::UNVOICED, 100.0),
        b("Noise", "Ghost Speech").tags(&["ghost", "noise", "vocal", "horror"]).set(p::CARRIER, NOISE).set(p::BANDS, 12.0).set(p::RELEASE, 120.0).set(p::MIX, 80.0),
    ]
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    for group in [pitchshift_presets(), freqshift_presets(), vocoder_presets()] {
        for pr in group {
            let path = pr.write(&root)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}
