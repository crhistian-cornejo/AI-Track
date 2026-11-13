//! Generates the factory presets of fx.reverb and fx.delay (the classic
//! ones plus the algorithm / colour / engine presets).
//! `cargo run -p aitrack-stock --example presets_reverb_delay`

use aitrack_stock::fx::{delay, reverb};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

const OFF: f32 = 0.0;
const ON: f32 = 1.0;
// `aitrack_dsp::lfo::SYNC_DIVISIONS` indices.
const DIV_1BAR: f32 = 3.0;
const DIV_HALF: f32 = 4.0;
const DIV_QUARTER: f32 = 7.0;
const DIV_DOTTED_QUARTER: f32 = 8.0;
const DIV_EIGHTH: f32 = 10.0;
const DIV_DOTTED_EIGHTH: f32 = 11.0;
const DIV_EIGHTH_T: f32 = 12.0;
const DIV_SIXTEENTH: f32 = 13.0;
// `reverb::shimmer::INTERVALS` indices.
const OCT_DOWN: f32 = 0.0;
const FIFTH_UP: f32 = 5.0;
const OCT_UP: f32 = 6.0;

fn reverb_presets() -> Vec<PresetBuilder> {
    use reverb::p;
    let (hall, room, plate) = (0.0, 1.0, 2.0);
    let b = |cat: &str, name: &str| PresetBuilder::new(reverb::ID, cat, name).tags(&["reverb"]);
    // Algorithms (`reverb::voicing::ALGORITHMS`) and colours.
    let (concert, bright, plate_a, room_a, chamber, ambience, shimmer, cloud) = (1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0);
    let (now, s80, s70) = (0.0, 1.0, 2.0);
    let algo = |b: PresetBuilder, a: f32, c: f32| b.set(p::ALGORITHM, a).set(p::COLOR, c);
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
        // Algorithms and colours.
        algo(b("Halls", "Grand Concert").tags(&["hall", "orchestral", "cinematic", "natural"]), concert, now).set(p::SIZE, 80.0).set(p::DECAY, 3.2).set(p::PREDELAY, 24.0).set(p::DAMPING, 45.0).set(p::MIX, 25.0),
        algo(b("Halls", "Bright Vocal Hall").tags(&["hall", "vocal", "pop", "bright"]), bright, now).set(p::SIZE, 60.0).set(p::DECAY, 2.2).set(p::PREDELAY, 35.0).set(p::LOW_CUT, 200.0).set(p::MIX, 20.0),
        algo(b("Halls", "80s Hall").tags(&["hall", "80s", "vintage", "synthwave", "pads"]), concert, s80).set(p::SIZE, 85.0).set(p::DECAY, 3.5).set(p::PREDELAY, 20.0).set(p::MOD_DEPTH, 60.0).set(p::MIX, 28.0),
        algo(b("Halls", "70s Hall").tags(&["hall", "70s", "vintage", "lo-fi", "keys"]), concert, s70).set(p::SIZE, 70.0).set(p::DECAY, 2.8).set(p::PREDELAY, 15.0).set(p::MIX, 25.0),
        algo(b("Halls", "Ducked Vocal Hall").tags(&["hall", "vocal", "ducking", "reggaeton", "pop"]), bright, now).set(p::DECAY, 2.5).set(p::PREDELAY, 30.0).set(p::DUCK, 70.0).set(p::LOW_CUT, 250.0).set(p::MIX, 30.0),
        algo(b("Plates", "Gold Vocal Plate").tags(&["plate", "vocal", "pop", "bright", "r&b"]), plate_a, now).set(p::DECAY, 2.0).set(p::PREDELAY, 30.0).set(p::DAMPING, 25.0).set(p::LOW_CUT, 220.0).set(p::MIX, 20.0),
        algo(b("Plates", "80s Snare Plate").tags(&["plate", "snare", "80s", "drums", "synthwave"]), plate_a, s80).set(p::DECAY, 1.4).set(p::PREDELAY, 5.0).set(p::DAMPING, 30.0).set(p::LOW_CUT, 300.0).set(p::MIX, 28.0),
        algo(b("Plates", "70s Plate").tags(&["plate", "70s", "vintage", "soul", "vocal"]), plate_a, s70).set(p::DECAY, 1.8).set(p::PREDELAY, 12.0).set(p::MIX, 22.0),
        algo(b("Rooms", "Studio Chamber").tags(&["chamber", "vocal", "drums", "natural", "warm"]), chamber, now).set(p::SIZE, 50.0).set(p::DECAY, 1.3).set(p::PREDELAY, 8.0).set(p::DAMPING, 50.0).set(p::MIX, 20.0),
        algo(b("Rooms", "Live Room").tags(&["room", "drums", "rock", "natural"]), room_a, now).set(p::SIZE, 45.0).set(p::DECAY, 0.8).set(p::EARLY, 70.0).set(p::MIX, 20.0),
        algo(b("Rooms", "Ambience Glue").tags(&["ambience", "mixing", "subtle", "bus"]), ambience, now).set(p::SIZE, 40.0).set(p::DECAY, 0.6).set(p::EARLY, 70.0).set(p::MIX, 15.0),
        algo(b("Rooms", "Drum Ambience").tags(&["ambience", "drums", "punchy", "trap", "dembow"]), ambience, now).set(p::SIZE, 30.0).set(p::DECAY, 0.4).set(p::EARLY, 90.0).set(p::LOW_CUT, 120.0).set(p::MIX, 18.0),
        algo(b("Shimmer", "Shimmer Heaven").tags(&["shimmer", "ambient", "pads", "worship", "huge"]), shimmer, now).set(p::SIZE, 90.0).set(p::DECAY, 8.0).set(p::PREDELAY, 40.0).set(p::SHIMMER, 60.0).set(p::SHIFT, OCT_UP).set(p::LOW_CUT, 200.0).set(p::MIX, 40.0),
        algo(b("Shimmer", "Fifth Shimmer").tags(&["shimmer", "ambient", "keys", "future bass"]), shimmer, now).set(p::SIZE, 80.0).set(p::DECAY, 6.0).set(p::SHIMMER, 40.0).set(p::SHIFT, FIFTH_UP).set(p::MIX, 35.0),
        algo(b("Shimmer", "Octave Down Drone").tags(&["shimmer", "drone", "dark", "cinematic"]), shimmer, now).set(p::SIZE, 100.0).set(p::DECAY, 10.0).set(p::SHIMMER, 50.0).set(p::SHIFT, OCT_DOWN).set(p::DAMPING, 60.0).set(p::MIX, 40.0),
        algo(b("Shimmer", "80s Shimmer Pad").tags(&["shimmer", "80s", "synthwave", "pads"]), shimmer, s80).set(p::SIZE, 85.0).set(p::DECAY, 7.0).set(p::SHIMMER, 45.0).set(p::SHIFT, OCT_UP).set(p::MIX, 35.0),
        algo(b("Creative", "Cloud Wash").tags(&["cloud", "ambient", "lo-fi", "pads", "huge"]), cloud, now).set(p::SIZE, 100.0).set(p::DECAY, 15.0).set(p::PREDELAY, 50.0).set(p::MOD_DEPTH, 60.0).set(p::LOW_CUT, 150.0).set(p::MIX, 50.0),
        algo(b("Creative", "Frozen Cloud").tags(&["cloud", "freeze", "drone", "fx"]), cloud, now).set(p::SIZE, 100.0).set(p::DECAY, 20.0).set(p::FREEZE, ON).set(p::MIX, 50.0),
    ]
}

fn delay_presets() -> Vec<PresetBuilder> {
    use delay::p;
    let (stereo, ping, mono) = (0.0, 1.0, 2.0);
    let b = |cat: &str, name: &str| PresetBuilder::new(delay::ID, cat, name).tags(&["delay"]);
    let sync = |b: PresetBuilder, l: f32, r: f32| b.set(p::SYNC, ON).set(p::DIV_L, l).set(p::DIV_R, r);
    // Engines: 0 Digital, 1 Tape, 2 BBD.
    let (tape, bbd) = (1.0, 2.0);
    let engine = |b: PresetBuilder, e: f32| b.set(p::ENGINE, e);
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
        // Engines: tape and bucket brigade.
        engine(sync(b("Tape", "Space Tape").tags(&["tape", "dub", "vintage", "warm"]).set(p::MODE, mono), DIV_DOTTED_EIGHTH, DIV_DOTTED_EIGHTH), tape)
            .set(p::FEEDBACK, 55.0)
            .set(p::WOW, 30.0)
            .set(p::FLUTTER, 25.0)
            .set(p::AGE, 40.0)
            .set(p::DRIVE, 4.0)
            .set(p::LOW_CUT, 150.0)
            .set(p::MIX, 30.0),
        engine(sync(b("Tape", "Worn Cassette").tags(&["tape", "lo-fi", "wobble", "keys", "vintage"]).set(p::MODE, stereo), DIV_QUARTER, DIV_QUARTER), tape)
            .set(p::FEEDBACK, 35.0)
            .set(p::WOW, 60.0)
            .set(p::FLUTTER, 50.0)
            .set(p::AGE, 80.0)
            .set(p::MIX, 30.0),
        engine(b("Tape", "Tape Slap").tags(&["tape", "slapback", "vocal", "rockabilly", "short"]).set(p::MODE, mono).set(p::SYNC, OFF).set(p::TIME_L, 120.0).set(p::TIME_R, 120.0), tape)
            .set(p::FEEDBACK, 12.0)
            .set(p::AGE, 30.0)
            .set(p::WOW, 10.0)
            .set(p::MIX, 25.0),
        engine(sync(b("Tape", "Dub Siren Tape").tags(&["tape", "dub", "reggae", "fx", "dark"]).set(p::MODE, ping), DIV_DOTTED_QUARTER, DIV_DOTTED_QUARTER), tape)
            .set(p::FEEDBACK, 85.0)
            .set(p::AGE, 50.0)
            .set(p::DRIVE, 10.0)
            .set(p::LOW_CUT, 250.0)
            .set(p::MIX, 35.0),
        engine(sync(b("BBD", "Analog Bucket").tags(&["bbd", "analog", "warm", "guitar", "synth"]).set(p::MODE, stereo), DIV_DOTTED_EIGHTH, DIV_QUARTER), bbd)
            .set(p::FEEDBACK, 45.0)
            .set(p::AGE, 30.0)
            .set(p::MIX, 28.0),
        engine(sync(b("BBD", "Dark BBD").tags(&["bbd", "dark", "analog", "techno", "dub"]).set(p::MODE, ping), DIV_QUARTER, DIV_QUARTER), bbd)
            .set(p::FEEDBACK, 55.0)
            .set(p::AGE, 80.0)
            .set(p::MIX, 30.0),
        engine(sync(b("BBD", "BBD Chorus Echo").tags(&["bbd", "chorus", "80s", "keys", "pads"]).set(p::MODE, stereo), DIV_SIXTEENTH, DIV_EIGHTH_T), bbd)
            .set(p::FEEDBACK, 30.0)
            .set(p::MOD_RATE, 0.8)
            .set(p::MOD_DEPTH, 40.0)
            .set(p::AGE, 20.0)
            .set(p::MIX, 30.0),
        engine(sync(b("BBD", "Long BBD Grit").tags(&["bbd", "lo-fi", "gritty", "fx", "ambient"]).set(p::MODE, mono), DIV_HALF, DIV_HALF), bbd)
            .set(p::FEEDBACK, 50.0)
            .set(p::DRIVE, 6.0)
            .set(p::MIX, 30.0),
        // Rhythms and textures.
        sync(b("Rhythmic", "Triplet Taps").tags(&["multi-tap", "triplet", "trap", "groove"]).set(p::MODE, stereo).set(p::TAPS, 2.0), DIV_QUARTER, DIV_QUARTER).set(p::FEEDBACK, 30.0).set(p::MIX, 25.0),
        sync(b("Rhythmic", "Quarter Taps").tags(&["multi-tap", "house", "techno", "groove"]).set(p::MODE, stereo).set(p::TAPS, 3.0), DIV_1BAR, DIV_1BAR).set(p::FEEDBACK, 20.0).set(p::LOW_CUT, 300.0).set(p::MIX, 22.0),
        sync(b("Rhythmic", "Diffused Echo").tags(&["diffusion", "ambient", "pads", "wide"]).set(p::MODE, ping), DIV_DOTTED_QUARTER, DIV_DOTTED_QUARTER).set(p::FEEDBACK, 60.0).set(p::DIFFUSION, 70.0).set(p::MIX, 30.0),
        engine(sync(b("Rhythmic", "Ambient Wash Delay").tags(&["diffusion", "tape", "ambient", "lo-fi", "huge"]).set(p::MODE, ping), DIV_HALF, DIV_HALF), tape)
            .set(p::FEEDBACK, 70.0)
            .set(p::DIFFUSION, 80.0)
            .set(p::WOW, 25.0)
            .set(p::AGE, 45.0)
            .set(p::MIX, 35.0),
    ]
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    for group in [reverb_presets(), delay_presets()] {
        for pr in group {
            let path = pr.write(&root)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}
