//! Generates the factory presets of the saturation family: fx.saturator,
//! fx.tape, fx.amp and fx.console.
//! `cargo run -p aitrack-stock --example presets_saturation`
//!
//! Devices not in the registry yet are skipped (their presets would fail
//! the factory preset test); run it again once they are registered.

use aitrack_stock::fx::saturator::{self, amp, console, style, tape};
use aitrack_stock::presets::{PresetBuilder, factory_dir};

const OFF: f32 = 0.0;

fn saturator_presets() -> Vec<PresetBuilder> {
    use saturator::p;
    let f = |s: usize| s as f32;
    let (tape_s, diode, tube, transistor, clip, rectify, fold, tri, crush) =
        (f(style::TAPE), f(style::DIODE), f(style::TUBE), f(style::TRANSISTOR), f(style::CLIP), f(style::RECTIFY), f(style::FOLD), f(style::TRI_FOLD), f(style::BITCRUSH));
    let b = |cat: &str, name: &str| PresetBuilder::new(saturator::ID, cat, name).tags(&["saturation"]);
    let band = |b: PresetBuilder, i: u32, drive: f32, style: f32, level: f32| {
        b.set(p::band(i, p::DRIVE), drive).set(p::band(i, p::STYLE), style).set(p::band(i, p::LEVEL), level)
    };
    vec![
        // The original twelve (same names, same sound).
        band(b("Warmth", "Tape Warmth").tags(&["tape", "warm", "lo-fi", "mixing"]), 0, 6.0, tape_s, 0.0).set(p::TONE, -1.5),
        band(b("Warmth", "Tube Glow").tags(&["tube", "warm", "vocal", "keys"]), 0, 9.0, tube, 0.0).set(p::band(0, p::BIAS), 35.0).set(p::MIX, 70.0),
        band(b("Warmth", "Gentle Glue").tags(&["subtle", "bus", "mixing", "house"]), 0, 3.0, tape_s, 0.0).set(p::MIX, 60.0),
        band(band(b("Bass", "Bass Grit").tags(&["bass", "gritty", "edm", "dubstep"]).set(p::BANDS, 2.0).set(p::XOVER_LOW, 150.0), 0, 0.0, tape_s, 0.0), 1, 18.0, tube, -3.0),
        band(band(b("Bass", "808 Distortion").tags(&["808", "trap", "reggaeton", "aggressive"]).set(p::BANDS, 2.0).set(p::XOVER_LOW, 100.0), 0, 4.0, tape_s, 0.0), 1, 20.0, clip, -4.0)
            .set(p::OVERSAMPLING, 2.0),
        band(band(band(b("Bass", "Reese Growl").tags(&["reese", "dnb", "dubstep", "aggressive"]).set(p::BANDS, 3.0).set(p::XOVER_LOW, 120.0).set(p::XOVER_HIGH, 2500.0), 0, 2.0, tape_s, 0.0), 1, 16.0, fold, -2.0), 2, 10.0, clip, -6.0)
            .set(p::OVERSAMPLING, 2.0),
        band(band(band(b("Drums", "Drum Crunch").tags(&["drums", "punchy", "techno", "trap"]).set(p::BANDS, 3.0).set(p::XOVER_LOW, 200.0).set(p::XOVER_HIGH, 4000.0), 0, 4.0, tape_s, 0.0), 1, 10.0, tube, 0.0), 2, 12.0, clip, -2.0),
        band(b("Drums", "Kick Clipper").tags(&["kick", "loud", "edm", "dembow"]), 0, 6.0, clip, -1.0).set(p::OVERSAMPLING, 2.0).set(p::AUTO_GAIN, OFF),
        band(b("Creative", "Lo-Fi Crush").tags(&["lo-fi", "bitcrush", "vintage", "creative"]), 0, 0.0, crush, 0.0)
            .set(p::band(0, p::BITS), 10.0)
            .set(p::band(0, p::CRUSH_RATE), 16_000.0)
            .set(p::MIX, 60.0)
            .set(p::TONE, -3.0),
        band(b("Creative", "8-Bit").tags(&["chiptune", "bitcrush", "creative", "fx"]), 0, 0.0, crush, 0.0).set(p::band(0, p::BITS), 5.0).set(p::band(0, p::CRUSH_RATE), 8_000.0),
        band(b("Creative", "Wavefolder Lead").tags(&["lead", "fold", "aggressive", "dubstep"]), 0, 14.0, fold, 0.0).set(p::OVERSAMPLING, 2.0).set(p::TONE, 2.0),
        band(band(b("Mixing", "Vocal Presence").tags(&["vocal", "bright", "pop", "reggaeton"]).set(p::BANDS, 2.0).set(p::XOVER_LOW, 2500.0), 0, 0.0, tape_s, 0.0), 1, 8.0, tube, 0.0).set(p::MIX, 50.0),
        // New styles.
        band(b("Warmth", "Diode Edge").tags(&["diode", "warm", "guitar", "keys"]), 0, 10.0, diode, 0.0).set(p::band(0, p::BIAS), 40.0).set(p::OVERSAMPLING, 1.0).set(p::MIX, 80.0),
        band(b("Warmth", "Transistor Bus").tags(&["transistor", "bus", "mixing", "drums"]), 0, 7.0, transistor, 0.0).set(p::band(0, p::BIAS), 30.0).set(p::MIX, 70.0),
        band(band(b("Bass", "Sub Octave Grit").tags(&["bass", "rectify", "octave", "techno"]).set(p::BANDS, 2.0).set(p::XOVER_LOW, 180.0), 0, 0.0, tape_s, 0.0), 1, 12.0, rectify, -6.0)
            .set(p::band(1, p::BIAS), 70.0)
            .set(p::OVERSAMPLING, 1.0),
        band(b("Creative", "Octave Fuzz").tags(&["rectify", "octave", "fuzz", "lead"]), 0, 14.0, rectify, -3.0).set(p::band(0, p::BIAS), 100.0).set(p::OVERSAMPLING, 2.0),
        band(b("Creative", "Triangle Folder").tags(&["fold", "west coast", "lead", "creative"]), 0, 16.0, tri, -2.0).set(p::OVERSAMPLING, 2.0).set(p::TONE, -2.0),
        band(band(band(b("Drums", "Parallel Smash").tags(&["drums", "parallel", "aggressive", "trap"]).set(p::BANDS, 3.0).set(p::XOVER_LOW, 150.0).set(p::XOVER_HIGH, 5000.0), 0, 8.0, diode, 0.0), 1, 18.0, transistor, -3.0), 2, 12.0, tube, -4.0)
            .set(p::MIX, 40.0)
            .set(p::OVERSAMPLING, 1.0),
    ]
}

fn tape_presets() -> Vec<PresetBuilder> {
    use tape::p;
    let b = |cat: &str, name: &str| PresetBuilder::new(tape::ID, cat, name).tags(&["tape"]);
    let (s375, s75, s15, s30) = (0.0, 1.0, 2.0, 3.0);
    vec![
        b("Mixing", "Gentle 15 ips").tags(&["subtle", "warm", "mixing", "bus"]).set(p::SPEED, s15).set(p::DRIVE, 35.0).set(p::HF_LOSS, 20.0).set(p::HEAD_BUMP, 40.0),
        b("Mixing", "30 ips Master").tags(&["mastering", "clean", "bus", "hi-fi"]).set(p::SPEED, s30).set(p::DRIVE, 25.0).set(p::SATURATION, 35.0).set(p::BIAS, 70.0).set(p::HF_LOSS, 10.0).set(p::HEAD_BUMP, 50.0),
        b("Mixing", "Drum Bus Tape").tags(&["drums", "punchy", "bus", "glue"]).set(p::SPEED, s15).set(p::INPUT, 6.0).set(p::DRIVE, 60.0).set(p::SATURATION, 60.0).set(p::HEAD_BUMP, 70.0).set(p::OUTPUT, -4.0),
        b("Character", "Hot 7.5 ips").tags(&["saturated", "warm", "bass", "vintage"]).set(p::SPEED, s75).set(p::INPUT, 9.0).set(p::DRIVE, 75.0).set(p::SATURATION, 70.0).set(p::BIAS, 35.0).set(p::HF_LOSS, 45.0).set(p::OUTPUT, -6.0),
        b("Character", "Under Biased").tags(&["gritty", "lo-fi", "vintage", "aggressive"]).set(p::SPEED, s75).set(p::INPUT, 6.0).set(p::DRIVE, 70.0).set(p::BIAS, 5.0).set(p::OUTPUT, -4.0),
        b("Character", "Vocal Tape").tags(&["vocal", "warm", "pop", "reggaeton"]).set(p::SPEED, s15).set(p::DRIVE, 45.0).set(p::SATURATION, 40.0).set(p::HF_LOSS, 15.0).set(p::HEAD_BUMP, 20.0).set(p::MIX, 70.0),
        b("Lo-Fi", "Cassette").tags(&["lo-fi", "cassette", "wow", "chill", "hip-hop"]).set(p::SPEED, s375).set(p::DRIVE, 55.0).set(p::HF_LOSS, 70.0).set(p::HEAD_BUMP, 30.0).set(p::WOW, 35.0).set(p::FLUTTER, 40.0).set(p::WIDTH, 80.0),
        b("Lo-Fi", "Worn Dictaphone").tags(&["lo-fi", "mono", "wow", "fx"]).set(p::SPEED, s375).set(p::INPUT, 6.0).set(p::DRIVE, 80.0).set(p::SATURATION, 80.0).set(p::HF_LOSS, 100.0).set(p::WOW, 70.0).set(p::FLUTTER, 80.0).set(p::WIDTH, 0.0).set(p::OUTPUT, -4.0),
        b("Lo-Fi", "Seasick Keys").tags(&["keys", "wow", "lo-fi", "chill"]).set(p::SPEED, s75).set(p::DRIVE, 40.0).set(p::HF_LOSS, 40.0).set(p::WOW, 80.0).set(p::FLUTTER, 20.0),
        b("Character", "Bass Bump").tags(&["bass", "808", "warm", "trap"]).set(p::SPEED, s15).set(p::DRIVE, 50.0).set(p::HEAD_BUMP, 100.0).set(p::HF_LOSS, 25.0),
    ]
}

fn amp_presets() -> Vec<PresetBuilder> {
    use amp::{mode, p};
    let m = |x: usize| x as f32;
    let b = |cat: &str, name: &str| PresetBuilder::new(amp::ID, cat, name).tags(&["distortion"]);
    vec![
        b("Pedals", "Transparent Boost").tags(&["overdrive", "klon", "guitar", "subtle"]).set(p::MODE, m(mode::CENTAUR)).set(p::GAIN, 15.0).set(p::TONE, 55.0),
        b("Pedals", "Centaur Drive").tags(&["overdrive", "klon", "guitar", "warm"]).set(p::MODE, m(mode::CENTAUR)).set(p::GAIN, 55.0).set(p::TONE, 60.0).set(p::OUTPUT, -2.0),
        b("Pedals", "Green Screamer").tags(&["overdrive", "tube screamer", "guitar", "mid"]).set(p::MODE, m(mode::SCREAMER)).set(p::GAIN, 50.0).set(p::TONE, 50.0),
        b("Pedals", "Screaming Lead").tags(&["overdrive", "lead", "guitar", "aggressive"]).set(p::MODE, m(mode::SCREAMER)).set(p::GAIN, 90.0).set(p::TONE, 65.0).set(p::CAB, 2.0).set(p::OUTPUT, -3.0),
        b("Pedals", "Fat Fuzz").tags(&["fuzz", "guitar", "bass", "stoner"]).set(p::MODE, m(mode::FUZZ)).set(p::GAIN, 70.0).set(p::TONE, 35.0).set(p::BIAS, 55.0).set(p::OUTPUT, -4.0),
        b("Pedals", "Sputter Fuzz").tags(&["fuzz", "gated", "creative", "lo-fi"]).set(p::MODE, m(mode::FUZZ)).set(p::GAIN, 60.0).set(p::TONE, 55.0).set(p::BIAS, 95.0).set(p::OUTPUT, -3.0),
        b("Amps", "Clean Combo").tags(&["amp", "clean", "guitar", "keys"]).set(p::MODE, m(mode::AMP)).set(p::GAIN, 15.0).set(p::TREBLE, 60.0).set(p::CAB, 1.0),
        b("Amps", "Plexi Crunch").tags(&["amp", "crunch", "guitar", "rock"]).set(p::MODE, m(mode::AMP)).set(p::GAIN, 55.0).set(p::BASS, 55.0).set(p::MID, 70.0).set(p::TREBLE, 65.0).set(p::CAB, 3.0),
        b("Amps", "High Gain Stack").tags(&["amp", "metal", "guitar", "aggressive"]).set(p::MODE, m(mode::AMP)).set(p::GAIN, 95.0).set(p::BASS, 60.0).set(p::MID, 30.0).set(p::TREBLE, 70.0).set(p::CAB, 3.0).set(p::OVERSAMPLING, 1.0).set(p::OUTPUT, -4.0),
        b("Creative", "Synth Bass Drive").tags(&["bass", "synth", "edm", "dubstep"]).set(p::MODE, m(mode::SCREAMER)).set(p::GAIN, 70.0).set(p::TONE, 60.0).set(p::MIX, 60.0),
        b("Creative", "Radio Fuzz").tags(&["lo-fi", "fuzz", "vocal", "fx"]).set(p::MODE, m(mode::FUZZ)).set(p::GAIN, 45.0).set(p::TONE, 70.0).set(p::CAB, 1.0).set(p::MIX, 70.0),
    ]
}

fn console_presets() -> Vec<PresetBuilder> {
    use console::{mode, p};
    let m = |x: usize| x as f32;
    let b = |cat: &str, name: &str| PresetBuilder::new(console::ID, cat, name).tags(&["console"]);
    vec![
        b("Summing", "Channel").tags(&["summing", "channel", "mixing", "analog"]).set(p::MODE, m(mode::CHANNEL)).set(p::IRON, 0.0),
        b("Summing", "Buss").tags(&["summing", "bus", "mixing", "analog"]).set(p::MODE, m(mode::BUSS)).set(p::IRON, 0.0),
        b("Summing", "Hot Channel").tags(&["summing", "channel", "drive", "warm"]).set(p::MODE, m(mode::CHANNEL)).set(p::DRIVE, 6.0).set(p::IRON, 20.0),
        b("Summing", "Hot Buss").tags(&["summing", "bus", "drive", "warm"]).set(p::MODE, m(mode::BUSS)).set(p::DRIVE, 6.0).set(p::IRON, 30.0),
        b("Glue", "Mix Bus Glue").tags(&["glue", "bus", "mastering", "subtle"]).set(p::MODE, m(mode::GLUE)).set(p::GLUE, 35.0).set(p::IRON, 20.0),
        b("Glue", "Drum Bus Glue").tags(&["glue", "drums", "punchy", "bus"]).set(p::MODE, m(mode::GLUE)).set(p::GLUE, 65.0).set(p::DRIVE, 4.0).set(p::IRON, 40.0),
        b("Glue", "Iron Bass").tags(&["bass", "transformer", "warm", "808"]).set(p::MODE, m(mode::GLUE)).set(p::GLUE, 20.0).set(p::IRON, 90.0),
        b("Glue", "Gentle Master").tags(&["mastering", "subtle", "glue", "hi-fi"]).set(p::MODE, m(mode::GLUE)).set(p::GLUE, 20.0).set(p::IRON, 10.0).set(p::OVERSAMPLING, 2.0),
    ]
}

type Group = (&'static str, fn() -> Vec<PresetBuilder>);

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    let groups: [Group; 4] = [(saturator::ID, saturator_presets), (tape::ID, tape_presets), (amp::ID, amp_presets), (console::ID, console_presets)];
    for (id, make) in groups {
        if aitrack_stock::create(id).is_none() {
            println!("skipped {id}: not in the registry yet");
            continue;
        }
        for pr in make() {
            let path = pr.write(&root)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}
