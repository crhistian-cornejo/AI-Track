//! Level-matched presets: every factory preset of the mixing and mastering
//! devices and of the two time effects, loaded on a mix at −18 dBFS RMS,
//! stays within ±3 dB of bypass, never clips and never produces NaN. A preset
//! that names a source in its tags (kick, bass, vocal, hi-hat…) is measured on
//! that part of the mix.

mod harness;

use aitrack_stock::presets;
use aitrack_stock::test_util::{level_vs_bypass, source_for_tags};
use harness::{BLOCK, SR};

const DEVICES: [&str; 7] = ["fx.reverb", "fx.delay", "fx.eq", "fx.compressor", "fx.buscomp", "fx.limiter", "fx.mbdyn"];

#[test]
fn presets_load_at_the_level_of_bypass() {
    // One thread per device: 330 presets × 6 s of audio.
    let results: Vec<(Vec<String>, (f32, String))> = std::thread::scope(|scope| {
        let handles: Vec<_> = DEVICES
            .iter()
            .map(|&id| {
                scope.spawn(move || {
                    let mut failures = Vec::new();
                    let mut worst = (0.0f32, String::new());
                    for r in presets::list(id) {
                        let preset = presets::load(&r.path).unwrap();
                        let mut p = aitrack_stock::create(id).unwrap();
                        p.prepare(SR, BLOCK);
                        preset.apply(p.as_mut());
                        let (delta, peak, finite) = level_vs_bypass(p.as_mut(), source_for_tags(&preset.tags));
                        if !finite {
                            failures.push(format!("{}: not finite", r.path));
                        } else if delta.abs() > 3.0 {
                            failures.push(format!("{}: {delta:+.1} dB against bypass", r.path));
                        } else if peak > 1.0 {
                            failures.push(format!("{}: peak {peak:.2} clips", r.path));
                        }
                        if delta.abs() > worst.0.abs() {
                            worst = (delta, r.path.clone());
                        }
                    }
                    (failures, worst)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let failures: Vec<&String> = results.iter().flat_map(|(f, _)| f).collect();
    let worst = results.iter().map(|(_, w)| w).max_by(|a, b| a.0.abs().total_cmp(&b.0.abs())).unwrap();
    println!("largest level change {:+.2} dB ({})", worst.0, worst.1);
    assert!(failures.is_empty(), "{} of the presets move the level:\n{}", failures.len(), failures.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
}
