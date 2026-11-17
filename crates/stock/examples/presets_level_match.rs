//! Brings every factory preset of the mixing and mastering devices and of
//! the two time effects to the level of bypass on a mix at −18 dBFS: a
//! preset that changes the level by more than 2 dB (or clips) gets its
//! output trim corrected (limiters get a lower input gain). Run it
//! after the other generators; `tests/preset_levels.rs` is the check.
//! `cargo run -p aitrack-stock --example presets_level_match`

use aitrack_stock::presets::{self, Preset, factory_dir};
use aitrack_stock::test_util::{BLOCK, SR, level_match, level_vs_bypass, source_for_tags};

const DEVICES: [&str; 7] = ["fx.reverb", "fx.delay", "fx.eq", "fx.compressor", "fx.buscomp", "fx.limiter", "fx.mbdyn"];

fn measure(preset: &Preset) -> (f32, f32) {
    let mut p = aitrack_stock::create(&preset.plugin).unwrap();
    p.prepare(SR, BLOCK);
    preset.apply(p.as_mut());
    let (delta, peak, _) = level_vs_bypass(p.as_mut(), source_for_tags(&preset.tags));
    (delta, peak)
}

fn main() -> std::io::Result<()> {
    let root = factory_dir();
    for id in DEVICES {
        for r in presets::list(id) {
            let mut preset = presets::load(&r.path).expect("factory preset");
            let (delta, peak) = measure(&preset);
            // (A limiter's gain is its job: up to 2.5 dB is where it is kept.)
            if (delta.abs() <= 2.0 || (id == "fx.limiter" && delta <= 2.5)) && peak <= 1.0 {
                continue;
            }
            level_match(&mut preset);
            let (after, peak_after) = measure(&preset);
            println!("{:<48} {delta:+5.1} dB -> {after:+5.1} dB (peak {peak:.2} -> {peak_after:.2})", r.path);
            std::fs::write(root.join(&r.path), preset.to_json() + "\n")?;
        }
    }
    Ok(())
}
