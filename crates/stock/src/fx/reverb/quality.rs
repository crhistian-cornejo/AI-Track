//! Measured quality of the reverb tails: echo density, decay per band,
//! spectral smoothness, stereo correlation and a stable freeze.
//!
//! The numbers in the comments are what the tank measured before the
//! windowed-sinc line reads, the phase-spread modulation, the modal-overlap
//! floor, the six-stage input diffusion and the early-field bridge; run
//! `cargo test -p aitrack-stock --lib reverb::quality::table -- --ignored
//! --nocapture` for the current table.

use super::*;
use crate::measure::*;
use crate::test_util::{SR, process_stereo, set_plain};

fn wet(r: &mut Reverb, early: bool) {
    set_plain(r, p::MIX, 100.0);
    set_plain(r, p::PREDELAY, 0.0);
    set_plain(r, p::EARLY, if early { 50.0 } else { 0.0 });
}

fn ir(r: &mut Reverb, secs: f32) -> (Vec<f32>, Vec<f32>) {
    let n = (secs * SR as f32) as usize;
    process_stereo(r, |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, n)
}

struct Cfg {
    name: &'static str,
    algo: f32,
    size: f32,
    decay: f32,
}

const CFGS: [Cfg; 8] = [
    Cfg { name: "classic hall", algo: 0.0, size: 50.0, decay: 2.0 },
    Cfg { name: "concert hall", algo: 1.0, size: 70.0, decay: 2.6 },
    Cfg { name: "big hall", algo: 1.0, size: 100.0, decay: 6.0 },
    Cfg { name: "plate", algo: 3.0, size: 50.0, decay: 1.8 },
    Cfg { name: "small room", algo: 4.0, size: 20.0, decay: 0.5 },
    Cfg { name: "drum room", algo: 4.0, size: 40.0, decay: 0.9 },
    Cfg { name: "chamber", algo: 5.0, size: 50.0, decay: 1.3 },
    Cfg { name: "cloud", algo: 8.0, size: 100.0, decay: 12.0 },
];

#[test]
#[ignore]
fn table() {
    for c in &CFGS {
        let mut r = Reverb::new();
        wet(&mut r, true);
        set_plain(&mut r, p::ALGORITHM, c.algo);
        set_plain(&mut r, p::SIZE, c.size);
        set_plain(&mut r, p::DECAY, c.decay);
        let secs = (c.decay * 1.3 + 0.6).min(8.0);
        let (l, rr) = ir(&mut r, secs);
        let m: Vec<f32> = l.iter().zip(&rr).map(|(a, b)| 0.5 * (a + b)).collect();
        let ned: Vec<f32> = [20, 50, 100, 200].iter().map(|&ms| echo_density(&l, (ms as f64 * 0.001 * SR) as usize)).collect();
        let bands: Vec<f32> = [125.0, 500.0, 2000.0, 8000.0].iter().map(|&f| t20(&octave_band(&m, f))).collect();
        let e = edt(&m);
        // Roughness of a stretch of the tail half an RT60 long, flattened
        // by the mid-band decay so it is a stationary noise-like signal.
        let tau = c.decay / 6.9078;
        let len = ((0.5 * c.decay * SR as f32) as usize).next_power_of_two() / 2;
        let len = len.clamp(8192, 65_536);
        let a = (0.08 * SR) as usize;
        let seg: Vec<f32> = (0..len).map(|i| l[a + i] * ((i as f32 / SR as f32) / tau).exp()).collect();
        let (sd, pk) = roughness(&seg, 300.0, 6000.0, 150.0);
        let corr = correlation(&l, &rr, (0.1 * SR) as usize);
        println!("{:<13} NED20/50/100/200 {:.2} {:.2} {:.2} {:.2} | EDT {:.2} T20 125/500/2k/8k {:.2} {:.2} {:.2} {:.2} | rough sd {:.2} pk {:.1} | corr {:.2}", c.name, ned[0], ned[1], ned[2], ned[3], e, bands[0], bands[1], bands[2], bands[3], sd, pk, corr);
    }
}

/// Wet-only impulse response of one configuration (left, right).
fn render(c: &Cfg) -> (Vec<f32>, Vec<f32>) {
    let mut r = Reverb::new();
    wet(&mut r, true);
    if c.algo == 3.0 {
        set_plain(&mut r, p::MODE, 2.0);
    }
    set_plain(&mut r, p::ALGORITHM, c.algo);
    set_plain(&mut r, p::SIZE, c.size);
    set_plain(&mut r, p::DECAY, c.decay);
    ir(&mut r, (c.decay * 1.3 + 0.6).min(8.0))
}

/// Roughness of the decay-flattened tail (see `table`).
fn tail_roughness(c: &Cfg, l: &[f32]) -> (f32, f32) {
    let tau = c.decay / 6.9078;
    let len = (((0.5 * c.decay * SR as f32) as usize).next_power_of_two() / 2).clamp(8192, 65_536);
    let a = (0.08 * SR) as usize;
    let seg: Vec<f32> = (0..len).map(|i| l[a + i] * ((i as f32 / SR as f32) / tau).exp()).collect();
    roughness(&seg, 300.0, 6000.0, 150.0)
}

fn ms(t: f32) -> usize {
    (t * 0.001 * SR as f32) as usize
}

#[test]
fn tails_are_smooth_not_metallic() {
    // Log-power spectrum of the tail against its own local mean: a Gaussian
    // (smooth) tail deviates 5.6 dB. Before: small room 6.85 / drum room
    // 7.11 / chamber 6.01 / plate 6.28 dB, ringing peaks up to 16.6 dB.
    for c in &CFGS {
        let (l, _) = render(c);
        let (sd, pk) = tail_roughness(c, &l);
        let limit = if c.algo == 3.0 { 6.3 } else { 6.1 };
        assert!(sd < limit, "{}: spectral deviation {sd} dB", c.name);
        assert!(pk < 15.0, "{}: strongest peak {pk} dB over its surroundings", c.name);
    }
}

#[test]
fn echoes_thicken_quickly() {
    // Normalised echo density (1 = noise): before, concert hall 0.59 at
    // 50 ms and big hall 0.02, because nothing answered before the shortest
    // line; plate 0.59 / 0.75 at 100 / 200 ms.
    let by_name = |n: &str| CFGS.iter().find(|c| c.name == n).unwrap();
    for (name, at50, at100) in [("classic hall", 0.7, 0.9), ("concert hall", 0.7, 0.9), ("big hall", 0.25, 0.6), ("small room", 0.85, 0.9), ("drum room", 0.85, 0.9), ("chamber", 0.8, 0.9), ("plate", 0.4, 0.55), ("cloud", 0.25, 0.6)] {
        let (l, _) = render(by_name(name));
        let (a, b) = (echo_density(&l, ms(50.0)), echo_density(&l, ms(100.0)));
        assert!(a >= at50 && b >= at100, "{name}: density {a:.2} at 50 ms, {b:.2} at 100 ms");
    }
    let (l, _) = render(by_name("plate"));
    assert!(echo_density(&l, ms(200.0)) >= 0.75, "the plate is dense by 200 ms");
}

#[test]
fn stereo_tail_stays_decorrelated() {
    for c in &CFGS {
        let (l, r) = render(c);
        let corr = correlation(&l, &r, ms(100.0));
        assert!(corr.abs() < 0.35, "{}: correlation {corr}", c.name);
    }
}

#[test]
fn bass_and_damping_set_the_decay_curve() {
    let t20s = |bass: f32, damping: f32| {
        let mut r = Reverb::new();
        wet(&mut r, false);
        set_plain(&mut r, p::ALGORITHM, 1.0);
        set_plain(&mut r, p::DECAY, 2.0);
        set_plain(&mut r, p::BASS_MULT, bass);
        set_plain(&mut r, p::DAMPING, damping);
        let (l, rr) = ir(&mut r, 8.0);
        let m: Vec<f32> = l.iter().zip(&rr).map(|(a, b)| 0.5 * (a + b)).collect();
        [63.0, 1000.0, 8000.0].map(|f| t20(&octave_band(&m, f)))
    };
    let long = t20s(2.0, 50.0);
    assert!(long[0] / long[1] > 1.6 && long[0] / long[1] < 2.3, "bass ×2: 63 Hz {} s against 1 kHz {} s", long[0], long[1]);
    let short = t20s(0.5, 50.0);
    assert!(short[0] / short[1] > 0.4 && short[0] / short[1] < 0.7, "bass ×0.5: 63 Hz {} s against 1 kHz {} s", short[0], short[1]);
    let dark = t20s(1.0, 100.0);
    let bright = t20s(1.0, 0.0);
    assert!(dark[2] < 0.45 * dark[1] && bright[2] > 0.8 * bright[1], "8 kHz against 1 kHz: dark {} / {}, bright {} / {}", dark[2], dark[1], bright[2], bright[1]);
}

#[test]
fn the_lines_do_not_sweep_together() {
    // One phasor per line, spread by the golden ratio: their mean vector is
    // short. Starting them all at zero (before) gave 1.0, one big flanger.
    let fdn = Fdn::new(SR);
    let (mut x, mut y) = (0.0f32, 0.0f32);
    for l in &fdn.lines {
        x += l.rot.0;
        y += l.rot.1;
    }
    let coherence = (x * x + y * y).sqrt() / fdn.lines.len() as f32;
    assert!(coherence < 0.2, "modulation coherence {coherence}");
}

#[test]
fn frozen_tails_hold_their_level() {
    // Before: the plate lost 8.5 dB between 1 and 60 s of freeze and went
    // dull (its damping and moving all-passes kept filtering); the FDN
    // lost 2.2 dB (a per-pass gain of 0.99995).
    for (name, algo, mode, limit) in [("concert hall", 1.0, 0.0, 1.5), ("cloud", 8.0, 0.0, 1.5), ("plate", 3.0, 2.0, 0.5)] {
        let mut r = Reverb::new();
        wet(&mut r, true);
        set_plain(&mut r, p::ALGORITHM, algo);
        set_plain(&mut r, p::MODE, mode);
        set_plain(&mut r, p::DECAY, 4.0);
        let mut src = noise(5, 0.3);
        let _ = process_stereo(&mut r, |i| if i < 48_000 { src(i) } else { (0.0, 0.0) }, 48_000);
        set_plain(&mut r, p::FREEZE, 1.0);
        let tr = aitrack_dsp::Transport::default();
        let (l, _) = crate::test_util::process_stereo_prepared(&mut r, |_| (0.0, 0.0), 48_000 * 61, &tr);
        let level = |s: usize| 20.0 * crate::test_util::rms(&l[s * 48_000..(s + 1) * 48_000]).log10();
        let drift = level(5) - level(60);
        assert!(level(5) > -40.0, "{name}: the frozen tail plays ({} dB)", level(5));
        assert!(drift.abs() < limit, "{name}: {drift} dB over a minute");
    }
}
