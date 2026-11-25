//! The sound `fx.filter` had before it learned its special types, the extra
//! LFO shapes and the sidechain: every factory preset and a few ladder
//! settings, run over the same second of programme material, give the same
//! level and peak as when this table was taken. The parameters added since
//! default to "off", so a preset (or a project) that does not know them must
//! not change.
//!
//! `cargo test -p aitrack-stock --test filter_old_sound -- --ignored --nocapture capture`
//! prints the table again (only for a deliberate change of the old sound).

mod harness;

use aitrack_dsp::{AudioBuffer, ProcessContext, Processor, Transport};
use aitrack_stock::presets;
use harness::{BLOCK, SR};

/// One second of a saw and some noise, the same every run.
fn material(n: usize) -> (Vec<f32>, Vec<f32>) {
    let mut s = 0x2468_ACE1u32;
    let mut noise = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s >> 8) as f32 / 8_388_608.0 - 1.0
    };
    let l: Vec<f32> = (0..n).map(|i| 0.4 * (((i as f32 * 110.0 / SR as f32).fract()) * 2.0 - 1.0) + 0.1 * noise()).collect();
    let r: Vec<f32> = (0..n).map(|i| 0.4 * (((i as f32 * 165.0 / SR as f32).fract()) * 2.0 - 1.0) + 0.1 * noise()).collect();
    (l, r)
}

/// `(rms, peak)` of the left channel after a second through `p` on a playing transport at 126 bpm.
fn level(p: &mut dyn Processor) -> (f32, f32) {
    let n = SR as usize;
    let (l, r) = material(n);
    let mut buf = AudioBuffer::new(2, BLOCK);
    let mut tr = Transport { playing: true, bpm: 126.0, ..Transport::default() };
    let (mut sum, mut peak) = (0f64, 0f32);
    for b in 0..n / BLOCK {
        for i in 0..BLOCK {
            buf.channel_mut(0)[i] = l[b * BLOCK + i];
            buf.channel_mut(1)[i] = r[b * BLOCK + i];
        }
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        p.process(&mut ctx);
        for &v in buf.channel(0) {
            sum += f64::from(v * v);
            peak = peak.max(v.abs());
        }
        tr.position_samples += BLOCK as i64;
        tr.position_beats += BLOCK as f64 / SR * tr.bpm / 60.0;
    }
    (((sum / (n / BLOCK * BLOCK) as f64).sqrt()) as f32, peak)
}

/// The first parameter id added after the old sound was taken (`Special`).
const FIRST_NEW_ID: u32 = 17;

/// Every case: a factory preset, or a ladder with some LFO and envelope.
fn cases() -> Vec<(String, Box<dyn Processor>)> {
    let mut out: Vec<(String, Box<dyn Processor>)> = Vec::new();
    for r in presets::list("fx.filter") {
        let mut p = aitrack_stock::create("fx.filter").unwrap();
        p.prepare(SR, BLOCK);
        let preset = presets::load(&r.path).unwrap();
        // The presets written for the special types set the ids added since: they are not the old sound.
        if preset.params.keys().any(|k| k.parse::<u32>().is_ok_and(|id| id >= FIRST_NEW_ID)) {
            continue;
        }
        preset.apply(p.as_mut());
        out.push((r.name, p));
    }
    for ladder in 1..=4 {
        let mut p = aitrack_stock::create("fx.filter").unwrap();
        p.prepare(SR, BLOCK);
        for (id, plain) in [(16, ladder as f32), (2, 700.0), (3, 40.0), (9, 1.5), (5, 6.0), (6, 1.0), (8, 5.0), (11, 1.0), (4, 6.0)] {
            let info = p.info().params.iter().find(|i| i.id == id).unwrap().clone();
            p.set_param(id, info.to_normalized(plain));
        }
        out.push((format!("ladder {ladder}"), p));
    }
    out
}

/// `(case, rms, peak)` as taken before the special types existed.
const GOLDEN: &[(&str, f32, f32)] = &[
    ("Auto-Wah", 0.041706577, 0.16858451),
    ("Duck Filter", 0.2335532, 0.6204725),
    ("Half-Bar Gate", 0.16994925, 0.6658777),
    ("Sample & Hold", 0.040735155, 0.20186722),
    ("Saw Pump", 0.20451725, 0.9034445),
    ("Slow Sweep 2 Bars", 0.25306395, 0.6772838),
    ("Stereo Swirl", 0.29247034, 1.4199741),
    ("Wobble Eighth", 0.3004508, 0.84320706),
    ("Telephone Band", 0.057739984, 0.4041545),
    ("Thin High-Pass", 0.1080744, 0.8398684),
    ("Warm Low-Pass", 0.23086372, 0.50663227),
    ("ladder 1", 0.19160871, 0.3980789),
    ("ladder 2", 0.112124264, 0.30138594),
    ("ladder 3", 0.29092783, 0.6574885),
    ("ladder 4", 0.11188468, 0.2983283),
];

#[test]
fn the_old_sound_is_unchanged() {
    let mut failures = Vec::new();
    for (name, mut p) in cases() {
        let (rms, peak) = level(p.as_mut());
        match GOLDEN.iter().find(|g| g.0 == name) {
            Some(g) if (g.1 - rms).abs() <= 1e-5 * g.1.max(1.0) && (g.2 - peak).abs() <= 1e-4 * g.2.max(1.0) => {}
            Some(g) => failures.push(format!("{name}: rms {rms} peak {peak}, was {} {}", g.1, g.2)),
            None => failures.push(format!("{name}: not in the table")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    assert_eq!(GOLDEN.len(), cases().len(), "a case was added or dropped");
}

#[test]
#[ignore]
fn capture() {
    for (name, mut p) in cases() {
        let (rms, peak) = level(p.as_mut());
        println!("    ({name:?}, {rms:?}, {peak:?}),");
    }
}
