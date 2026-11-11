//! Generates the factory presets of fx.saturator, fx.reverb, fx.delay,
//! fx.filter, fx.chorus and fx.analyzer.
//! `cargo run -p aitrack-stock --example presets_fx`

use aitrack_stock::fx::{analyzer, chorus, delay, filter, reverb, saturator};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

// Choice indices shared by several plugins.
const OFF: f32 = 0.0;
const ON: f32 = 1.0;
// `aitrack_dsp::lfo::SYNC_DIVISIONS` indices.
const DIV_1BAR: f32 = 3.0;
const DIV_2BARS: f32 = 2.0;
const DIV_HALF: f32 = 4.0;
const DIV_QUARTER: f32 = 7.0;
const DIV_DOTTED_QUARTER: f32 = 8.0;
const DIV_EIGHTH: f32 = 10.0;
const DIV_DOTTED_EIGHTH: f32 = 11.0;
const DIV_EIGHTH_T: f32 = 12.0;
const DIV_SIXTEENTH: f32 = 13.0;

fn saturator_presets() -> Vec<PresetBuilder> {
    use saturator::p;
    // Styles: 0 Tape, 1 Tube, 2 Clip, 3 Fold, 4 Bitcrush. OS: 0 1x, 1 2x, 2 4x.
    let (tape, tube, clip, fold, crush) = (0.0, 1.0, 2.0, 3.0, 4.0);
    let b = |cat: &str, name: &str| PresetBuilder::new(saturator::ID, cat, name).tags(&["saturation"]);
    let band = |b: PresetBuilder, i: u32, drive: f32, style: f32, level: f32| {
        b.set(p::band(i, p::DRIVE), drive).set(p::band(i, p::STYLE), style).set(p::band(i, p::LEVEL), level)
    };
    vec![
        band(b("Warmth", "Tape Warmth").tags(&["tape", "warm", "lo-fi", "mixing"]), 0, 6.0, tape, 0.0).set(p::TONE, -1.5),
        band(b("Warmth", "Tube Glow").tags(&["tube", "warm", "vocal", "keys"]), 0, 9.0, tube, 0.0).set(p::band(0, p::BIAS), 35.0).set(p::MIX, 70.0),
        band(b("Warmth", "Gentle Glue").tags(&["subtle", "bus", "mixing", "house"]), 0, 3.0, tape, 0.0).set(p::MIX, 60.0),
        // Bass: dirt only above the sub, the sub stays clean.
        band(band(b("Bass", "Bass Grit").tags(&["bass", "gritty", "edm", "dubstep"]).set(p::BANDS, 2.0).set(p::XOVER_LOW, 150.0), 0, 0.0, tape, 0.0), 1, 18.0, tube, -3.0),
        band(band(b("Bass", "808 Distortion").tags(&["808", "trap", "reggaeton", "aggressive"]).set(p::BANDS, 2.0).set(p::XOVER_LOW, 100.0), 0, 4.0, tape, 0.0), 1, 20.0, clip, -4.0)
            .set(p::OVERSAMPLING, 2.0),
        band(band(band(b("Bass", "Reese Growl").tags(&["reese", "dnb", "dubstep", "aggressive"]).set(p::BANDS, 3.0).set(p::XOVER_LOW, 120.0).set(p::XOVER_HIGH, 2500.0), 0, 2.0, tape, 0.0), 1, 16.0, fold, -2.0), 2, 10.0, clip, -6.0)
            .set(p::OVERSAMPLING, 2.0),
        // Drums: punch from the low mids, crunch on top.
        band(band(band(b("Drums", "Drum Crunch").tags(&["drums", "punchy", "techno", "trap"]).set(p::BANDS, 3.0).set(p::XOVER_LOW, 200.0).set(p::XOVER_HIGH, 4000.0), 0, 4.0, tape, 0.0), 1, 10.0, tube, 0.0), 2, 12.0, clip, -2.0),
        band(b("Drums", "Kick Clipper").tags(&["kick", "loud", "edm", "dembow"]), 0, 6.0, clip, -1.0).set(p::OVERSAMPLING, 2.0).set(p::AUTO_GAIN, OFF),
        // Creative.
        band(b("Creative", "Lo-Fi Crush").tags(&["lo-fi", "bitcrush", "vintage", "creative"]), 0, 0.0, crush, 0.0)
            .set(p::band(0, p::BITS), 10.0)
            .set(p::band(0, p::CRUSH_RATE), 16_000.0)
            .set(p::MIX, 60.0)
            .set(p::TONE, -3.0),
        band(b("Creative", "8-Bit").tags(&["chiptune", "bitcrush", "creative", "fx"]), 0, 0.0, crush, 0.0).set(p::band(0, p::BITS), 5.0).set(p::band(0, p::CRUSH_RATE), 8_000.0),
        band(b("Creative", "Wavefolder Lead").tags(&["lead", "fold", "aggressive", "dubstep"]), 0, 14.0, fold, 0.0).set(p::OVERSAMPLING, 2.0).set(p::TONE, 2.0),
        band(band(b("Mixing", "Vocal Presence").tags(&["vocal", "bright", "pop", "reggaeton"]).set(p::BANDS, 2.0).set(p::XOVER_LOW, 2500.0), 0, 0.0, tape, 0.0), 1, 8.0, tube, 0.0).set(p::MIX, 50.0),
    ]
}

fn reverb_presets() -> Vec<PresetBuilder> {
    use reverb::p;
    let (hall, room, plate) = (0.0, 1.0, 2.0);
    let b = |cat: &str, name: &str| PresetBuilder::new(reverb::ID, cat, name).tags(&["reverb"]);
    vec![
        b("Rooms", "Small Room").tags(&["room", "drums", "natural", "tight"]).set(p::MODE, room).set(p::SIZE, 20.0).set(p::DECAY, 0.5).set(p::PREDELAY, 2.0).set(p::DAMPING, 55.0).set(p::EARLY, 75.0).set(p::MIX, 18.0),
        b("Rooms", "Drum Room").tags(&["room", "drums", "punchy", "rock"]).set(p::MODE, room).set(p::SIZE, 40.0).set(p::DECAY, 0.9).set(p::PREDELAY, 5.0).set(p::DAMPING, 45.0).set(p::EARLY, 65.0).set(p::LOW_CUT, 150.0).set(p::MIX, 20.0),
        b("Rooms", "Vocal Booth").tags(&["vocal", "room", "dry", "pop"]).set(p::MODE, room).set(p::SIZE, 10.0).set(p::DECAY, 0.3).set(p::DAMPING, 65.0).set(p::EARLY, 80.0).set(p::MIX, 12.0),
        b("Halls", "Big Hall").tags(&["hall", "big", "cinematic", "pads"]).set(p::MODE, hall).set(p::SIZE, 85.0).set(p::DECAY, 4.5).set(p::PREDELAY, 30.0).set(p::DAMPING, 45.0).set(p::BASS_MULT, 1.2).set(p::MOD_DEPTH, 35.0).set(p::MIX, 30.0),
        b("Halls", "Concert Hall").tags(&["hall", "natural", "orchestral", "keys"]).set(p::MODE, hall).set(p::SIZE, 70.0).set(p::DECAY, 2.6).set(p::PREDELAY, 20.0).set(p::DAMPING, 50.0).set(p::EARLY, 55.0).set(p::MIX, 25.0),
        b("Halls", "Ambient Cloud").tags(&["ambient", "pads", "lo-fi", "future bass", "huge"]).set(p::MODE, hall).set(p::SIZE, 100.0).set(p::DECAY, 12.0).set(p::PREDELAY, 60.0).set(p::DAMPING, 60.0).set(p::DIFFUSION, 90.0).set(p::MOD_DEPTH, 60.0).set(p::EARLY, 15.0).set(p::LOW_CUT, 200.0).set(p::MIX, 45.0),
        b("Plates", "Vocal Plate").tags(&["plate", "vocal", "pop", "reggaeton", "bright"]).set(p::MODE, plate).set(p::DECAY, 1.8).set(p::PREDELAY, 25.0).set(p::DAMPING, 35.0).set(p::LOW_CUT, 250.0).set(p::HIGH_CUT, 12_000.0).set(p::MIX, 22.0),
        b("Plates", "Snare Plate").tags(&["plate", "snare", "drums", "80s"]).set(p::MODE, plate).set(p::DECAY, 1.2).set(p::PREDELAY, 8.0).set(p::DAMPING, 30.0).set(p::LOW_CUT, 300.0).set(p::MIX, 25.0),
        b("Plates", "Dark Plate").tags(&["plate", "dark", "lo-fi", "keys"]).set(p::MODE, plate).set(p::DECAY, 2.5).set(p::DAMPING, 80.0).set(p::DAMP_FREQ, 2500.0).set(p::HIGH_CUT, 6_000.0).set(p::MIX, 25.0),
        b("Creative", "Infinite Freeze").tags(&["freeze", "ambient", "fx", "drone"]).set(p::MODE, hall).set(p::SIZE, 90.0).set(p::DECAY, 20.0).set(p::FREEZE, ON).set(p::MIX, 50.0),
        b("Creative", "EDM Lead Space").tags(&["lead", "edm", "future bass", "wide"]).set(p::MODE, hall).set(p::SIZE, 60.0).set(p::DECAY, 2.0).set(p::PREDELAY, 40.0).set(p::LOW_CUT, 400.0).set(p::WIDTH, 150.0).set(p::MIX, 20.0),
        b("Creative", "Techno Wash").tags(&["techno", "dark", "dub", "wide"]).set(p::MODE, hall).set(p::SIZE, 75.0).set(p::DECAY, 6.0).set(p::DAMPING, 70.0).set(p::LOW_CUT, 300.0).set(p::MOD_DEPTH, 50.0).set(p::MIX, 30.0),
    ]
}

fn delay_presets() -> Vec<PresetBuilder> {
    use delay::p;
    let (stereo, ping, mono) = (0.0, 1.0, 2.0);
    let b = |cat: &str, name: &str| PresetBuilder::new(delay::ID, cat, name).tags(&["delay"]);
    let sync = |b: PresetBuilder, l: f32, r: f32| b.set(p::SYNC, ON).set(p::DIV_L, l).set(p::DIV_R, r);
    vec![
        sync(b("Synced", "Eighth Ping-Pong").tags(&["ping-pong", "edm", "house", "lead"]).set(p::MODE, ping), DIV_EIGHTH, DIV_EIGHTH).set(p::FEEDBACK, 40.0).set(p::MIX, 25.0),
        sync(b("Synced", "Dotted Eighth").tags(&["dotted", "pop", "future bass", "guitar"]).set(p::MODE, stereo), DIV_DOTTED_EIGHTH, DIV_DOTTED_EIGHTH).set(p::FEEDBACK, 35.0).set(p::HIGH_CUT, 6_000.0).set(p::MIX, 22.0),
        sync(b("Synced", "Quarter Wide").tags(&["wide", "pads", "trance", "edm"]).set(p::MODE, stereo), DIV_QUARTER, DIV_DOTTED_QUARTER).set(p::FEEDBACK, 30.0).set(p::WIDTH, 150.0).set(p::MIX, 20.0),
        sync(b("Synced", "Triplet Bounce").tags(&["triplet", "trap", "reggaeton", "groove"]).set(p::MODE, ping), DIV_EIGHTH_T, DIV_EIGHTH_T).set(p::FEEDBACK, 45.0).set(p::MIX, 22.0),
        sync(b("Synced", "Sixteenth Shimmer").tags(&["sixteenth", "pluck", "edm", "bright"]).set(p::MODE, ping), DIV_SIXTEENTH, DIV_SIXTEENTH).set(p::FEEDBACK, 30.0).set(p::LOW_CUT, 400.0).set(p::MIX, 18.0),
        b("Free", "Slapback").tags(&["slapback", "vocal", "rockabilly", "short"]).set(p::MODE, mono).set(p::SYNC, OFF).set(p::TIME_L, 110.0).set(p::TIME_R, 110.0).set(p::FEEDBACK, 10.0).set(p::HIGH_CUT, 5_000.0).set(p::MIX, 25.0),
        b("Free", "Haas Widener").tags(&["wide", "mixing", "stereo"]).set(p::MODE, stereo).set(p::SYNC, OFF).set(p::TIME_L, 1.0).set(p::TIME_R, 18.0).set(p::FEEDBACK, 0.0).set(p::MIX, 35.0),
        sync(b("Character", "Tape Echo").tags(&["tape", "dub", "lo-fi", "warm"]).set(p::MODE, mono), DIV_DOTTED_EIGHTH, DIV_DOTTED_EIGHTH)
            .set(p::FEEDBACK, 55.0)
            .set(p::LOW_CUT, 200.0)
            .set(p::HIGH_CUT, 3_500.0)
            .set(p::MOD_RATE, 0.7)
            .set(p::MOD_DEPTH, 25.0)
            .set(p::DRIVE, 6.0)
            .set(p::MIX, 30.0),
        sync(b("Character", "Dub Throw").tags(&["dub", "techno", "fx", "dark"]).set(p::MODE, ping), DIV_QUARTER, DIV_DOTTED_QUARTER)
            .set(p::FEEDBACK, 75.0)
            .set(p::LOW_CUT, 300.0)
            .set(p::HIGH_CUT, 2_500.0)
            .set(p::DRIVE, 8.0)
            .set(p::MIX, 35.0),
        sync(b("Character", "Ducked Vocal Echo").tags(&["vocal", "ducking", "pop", "reggaeton"]).set(p::MODE, ping), DIV_EIGHTH, DIV_QUARTER).set(p::FEEDBACK, 40.0).set(p::DUCK, 70.0).set(p::LOW_CUT, 250.0).set(p::MIX, 30.0),
        sync(b("Character", "Lo-Fi Wobble").tags(&["lo-fi", "vintage", "keys", "wobble"]).set(p::MODE, stereo), DIV_EIGHTH, DIV_EIGHTH)
            .set(p::FEEDBACK, 35.0)
            .set(p::HIGH_CUT, 2_500.0)
            .set(p::MOD_RATE, 1.2)
            .set(p::MOD_DEPTH, 60.0)
            .set(p::MIX, 30.0),
    ]
}

fn filter_presets() -> Vec<PresetBuilder> {
    use filter::p;
    // Types: 0 LP, 1 HP, 2 BP, 3 Notch, 4 Peak. LFO shapes: 0 Sine, 1 Tri, 2 Saw Up, 3 Saw Down, 4 Square, 5 S&H.
    let b = |cat: &str, name: &str| PresetBuilder::new(filter::ID, cat, name).tags(&["filter"]);
    let lfo = |b: PresetBuilder, shape: f32, div: f32, depth: f32| b.set(p::LFO_SHAPE, shape).set(p::LFO_SYNC, ON).set(p::LFO_DIV, div).set(p::LFO_DEPTH, depth);
    vec![
        b("Static", "Warm Low-Pass").tags(&["lowpass", "warm", "lo-fi", "mixing"]).set(p::TYPE, 0.0).set(p::CUTOFF, 4_000.0).set(p::RESONANCE, 10.0),
        b("Static", "Telephone Band").tags(&["bandpass", "lo-fi", "vocal", "fx"]).set(p::TYPE, 2.0).set(p::CUTOFF, 1_500.0).set(p::RESONANCE, 30.0).set(p::DRIVE, 6.0),
        b("Static", "Thin High-Pass").tags(&["highpass", "breakdown", "edm", "house"]).set(p::TYPE, 1.0).set(p::SLOPE, 1.0).set(p::CUTOFF, 600.0).set(p::RESONANCE, 20.0),
        lfo(b("Motion", "Wobble Eighth").tags(&["wobble", "dubstep", "bass", "lfo"]).set(p::TYPE, 0.0).set(p::SLOPE, 1.0).set(p::CUTOFF, 400.0).set(p::RESONANCE, 45.0).set(p::DRIVE, 6.0), 0.0, DIV_EIGHTH, 3.0),
        lfo(b("Motion", "Slow Sweep 2 Bars").tags(&["sweep", "pads", "trance", "ambient"]).set(p::TYPE, 0.0).set(p::CUTOFF, 1_200.0).set(p::RESONANCE, 30.0), 1.0, DIV_2BARS, 2.5),
        lfo(b("Motion", "Sample & Hold").tags(&["random", "techno", "fx", "glitch"]).set(p::TYPE, 2.0).set(p::CUTOFF, 1_500.0).set(p::RESONANCE, 50.0), 5.0, DIV_SIXTEENTH, 2.0),
        lfo(b("Motion", "Stereo Swirl").tags(&["stereo", "wide", "pads", "lfo"]).set(p::TYPE, 4.0).set(p::CUTOFF, 1_000.0).set(p::RESONANCE, 40.0).set(p::LFO_STEREO, 90.0), 0.0, DIV_1BAR, 2.0),
        lfo(b("Motion", "Saw Pump").tags(&["pump", "house", "edm", "sidechain"]).set(p::TYPE, 1.0).set(p::CUTOFF, 120.0).set(p::RESONANCE, 15.0), 3.0, DIV_QUARTER, 2.0),
        b("Envelope", "Auto-Wah").tags(&["envelope", "funk", "guitar", "keys"]).set(p::TYPE, 2.0).set(p::CUTOFF, 400.0).set(p::RESONANCE, 55.0).set(p::ENV_AMOUNT, 3.0).set(p::ENV_ATTACK, 3.0).set(p::ENV_RELEASE, 120.0),
        b("Envelope", "Duck Filter").tags(&["envelope", "dark", "dynamic", "bass"]).set(p::TYPE, 0.0).set(p::CUTOFF, 6_000.0).set(p::RESONANCE, 20.0).set(p::ENV_AMOUNT, -3.0).set(p::ENV_RELEASE, 250.0),
        lfo(b("Motion", "Half-Bar Gate").tags(&["rhythmic", "trance", "edm", "lfo"]).set(p::TYPE, 0.0).set(p::SLOPE, 1.0).set(p::CUTOFF, 300.0).set(p::RESONANCE, 25.0), 4.0, DIV_HALF, 3.5),
    ]
}

fn chorus_presets() -> Vec<PresetBuilder> {
    use chorus::p;
    let (cho, fla, pha) = (0.0, 1.0, 2.0);
    let b = |cat: &str, name: &str| PresetBuilder::new(chorus::ID, cat, name).tags(&["modulation"]);
    vec![
        b("Chorus", "Classic Chorus").tags(&["chorus", "keys", "guitar", "80s"]).set(p::MODE, cho).set(p::RATE, 0.8).set(p::DEPTH, 45.0).set(p::VOICES, 3.0).set(p::DELAY, 8.0).set(p::MIX, 50.0),
        b("Chorus", "Wide Ensemble").tags(&["chorus", "pads", "wide", "future bass"]).set(p::MODE, cho).set(p::RATE, 0.35).set(p::DEPTH, 60.0).set(p::VOICES, 6.0).set(p::DELAY, 14.0).set(p::SPREAD, 100.0).set(p::MIX, 45.0),
        b("Chorus", "Subtle Thickener").tags(&["chorus", "vocal", "subtle", "pop"]).set(p::MODE, cho).set(p::RATE, 0.5).set(p::DEPTH, 20.0).set(p::VOICES, 2.0).set(p::DELAY, 6.0).set(p::MIX, 25.0),
        b("Chorus", "Lo-Fi Warble").tags(&["chorus", "lo-fi", "vintage", "keys"]).set(p::MODE, cho).set(p::RATE, 2.5).set(p::DEPTH, 40.0).set(p::VOICES, 2.0).set(p::DELAY, 4.0).set(p::MIX, 60.0),
        b("Flanger", "Jet Flanger").tags(&["flanger", "fx", "edm", "drums"]).set(p::MODE, fla).set(p::RATE, 0.15).set(p::DEPTH, 80.0).set(p::FEEDBACK, 70.0).set(p::DELAY, 2.0).set(p::MIX, 50.0),
        b("Flanger", "Synced Flange").tags(&["flanger", "house", "hi-hat", "groove"]).set(p::MODE, fla).set(p::SYNC, ON).set(p::DIVISION, DIV_1BAR).set(p::DEPTH, 60.0).set(p::FEEDBACK, 50.0).set(p::DELAY, 1.5).set(p::MIX, 40.0),
        b("Phaser", "Slow Phaser").tags(&["phaser", "pads", "ambient", "keys"]).set(p::MODE, pha).set(p::RATE, 0.2).set(p::DEPTH, 70.0).set(p::STAGES, 3.0).set(p::FEEDBACK, 40.0).set(p::CENTER, 800.0).set(p::MIX, 50.0),
        b("Phaser", "Funky Phase").tags(&["phaser", "funk", "guitar", "keys"]).set(p::MODE, pha).set(p::RATE, 1.5).set(p::DEPTH, 60.0).set(p::STAGES, 1.0).set(p::FEEDBACK, 30.0).set(p::CENTER, 1_200.0).set(p::MIX, 50.0),
        b("Phaser", "Deep 12-Stage").tags(&["phaser", "techno", "deep", "fx"]).set(p::MODE, pha).set(p::SYNC, ON).set(p::DIVISION, DIV_2BARS).set(p::DEPTH, 90.0).set(p::STAGES, 5.0).set(p::FEEDBACK, 60.0).set(p::CENTER, 600.0).set(p::MIX, 50.0),
    ]
}

fn analyzer_presets() -> Vec<PresetBuilder> {
    use analyzer::p;
    let b = |cat: &str, name: &str| PresetBuilder::new(analyzer::ID, cat, name).tags(&["analyzer"]);
    vec![
        b("Metering", "Default").tags(&["mixing", "metering"]),
        b("Metering", "Mix Tilt 4.5").tags(&["mixing", "tilt", "pink"]).set(p::TILT, 2.0).set(p::FALL, 12.0),
        b("Metering", "High Resolution Lows").tags(&["mastering", "bass", "precise"]).set(p::FFT_SIZE, 2.0).set(p::FALL, 6.0),
        b("Metering", "Side Check").tags(&["stereo", "mastering", "mid-side"]).set(p::SOURCE, 3.0),
        b("Metering", "Fast Response").tags(&["live", "drums", "fast"]).set(p::FFT_SIZE, 0.0).set(p::FALL, 60.0),
        b("Metering", "Left Only").tags(&["stereo", "check"]).set(p::SOURCE, 1.0),
    ]
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let all = [saturator_presets(), reverb_presets(), delay_presets(), filter_presets(), chorus_presets(), analyzer_presets()];
    for group in all {
        for pr in group {
            let path = pr.write(&root)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}
