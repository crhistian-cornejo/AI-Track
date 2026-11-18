//! Growing scenes: pure, deterministic and allocation-free.
//!
//! ```text
//! seed ─► species character (shared decay, grit, brightness, material, key…)
//!      └► per cell: role template shaped by the character = the cell's core
//!                   + a growth path (its own seed) + a distance along it
//!                   = the sound
//! ```
//!
//! A growth path is a fixed random direction in normalised gene space (and,
//! for categorical genes such as the waveform, a distance at which the gene
//! switches). A sound at distance `d` is the core moved `d` along its path,
//! folded back into range at the ends ([`grow`]), so distance 0 is the core
//! and dragging a cell outwards changes the sound more and more, always the
//! same way for the same seed. An edit (the DNA panel, a match) re-bases the
//! core so that the edited sound lies on the same path at the same distance
//! ([`rebase`]); the cell keeps its place and can still be dragged.
//!
//! Only arithmetic, `sqrt` and rounding are used here (no `ln`, `exp` or
//! trigonometry: see [`det_log2`](super::genome::det_log2)), so a seed grows
//! the same scene on every machine.

use super::genome::{GENES, Genome, det_exp2, N_GENES, Role, SOUNDS, Scene, Species, categorical, choice_count, g, quantize, reflect};
use super::rng::{Rng, part};

/// How far one unit of distance moves each gene (normalised units).
fn sensitivity(gene: usize) -> f32 {
    match gene {
        g::PITCH => 0.10,
        g::CHOKE => 0.0,
        g::LEVEL => 0.08,
        g::PAN | g::WIDTH => 0.25,
        g::SEND => 0.15,
        g::ATTACK | g::HOLD => 0.2,
        g::RES_MIX => 0.3,
        _ if categorical(gene) => 1.0,
        _ => 0.38,
    }
}

/// What every sound of a scene shares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Character {
    /// Multiplies every decay.
    pub decay: f32,
    /// 0..1: drive and FM.
    pub grit: f32,
    /// -1..1: octaves of filter brightness (scaled per role).
    pub bright: f32,
    /// 0..1: how much resonator every sound gets.
    pub metal: f32,
    /// The scene's resonator material, 0..4.
    pub shape: f32,
    /// The FM ratio the family leans on.
    pub fm: f32,
    /// Key of the tuned sounds: root pitch class and scale.
    pub root: i32,
    pub minor: bool,
    /// Ambience send.
    pub space: f32,
}

const MINOR_PENT: [i32; 5] = [0, 3, 5, 7, 10];
const MAJOR_PENT: [i32; 5] = [0, 2, 4, 7, 9];

impl Character {
    pub fn of(species: Species, r: &mut Rng) -> Self {
        let (decay, grit, bright, metal, shape, fm, space) = match species {
            Species::Kit => (r.between(0.75, 1.3), r.between(0.0, 0.35), r.between(-0.4, 0.4), r.between(0.0, 0.12), r.between(0.8, 2.2), r.pick(&[1.0, 1.41, 2.0, 3.0]), r.between(0.1, 0.3)),
            Species::Tonal => {
                let shape = if r.chance(0.5) { r.between(0.0, 1.2) } else { r.between(3.0, 3.6) };
                (r.between(0.9, 1.6), r.between(0.0, 0.2), r.between(-0.3, 0.3), r.between(0.1, 0.3), shape, r.pick(&[1.0, 2.0, 3.0, 4.0]), r.between(0.2, 0.45))
            }
            Species::Metallic => (r.between(0.9, 1.8), r.between(0.05, 0.4), r.between(0.0, 0.6), r.between(0.3, 0.55), r.between(2.0, 4.0), r.pick(&[1.41, 2.76, 3.5, 5.19]), r.between(0.2, 0.5)),
            Species::Glitch => (r.between(0.4, 0.9), r.between(0.3, 0.75), r.between(-0.2, 0.7), r.between(0.0, 0.3), r.between(0.0, 4.0), r.pick(&[1.5, 2.0, 3.0, 7.0, 11.0]), r.between(0.05, 0.25)),
            Species::Mixed => (r.between(0.7, 1.4), r.between(0.05, 0.5), r.between(-0.4, 0.5), r.between(0.05, 0.3), r.between(0.0, 4.0), r.pick(&[1.0, 1.41, 2.0, 3.0, 3.5]), r.between(0.1, 0.4)),
        };
        Self { decay, grit, bright, metal, shape, fm, root: r.below(12) as i32, minor: r.chance(0.6), space }
    }

    /// The nearest note of the scene's key to `midi`.
    pub fn snap(&self, midi: f32) -> f32 {
        let scale = if self.minor { &MINOR_PENT } else { &MAJOR_PENT };
        let m = midi.round() as i32;
        let mut best = m;
        let mut best_d = i32::MAX;
        for o in -1..=1 {
            for s in scale {
                let pc = (m.div_euclid(12) + o) * 12 + self.root + s;
                let d = (pc - m).abs();
                if d < best_d {
                    best_d = d;
                    best = pc;
                }
            }
        }
        best as f32
    }
}

/// The core sound of a role in a scene of character `ch`.
pub fn template(role: Role, ch: &Character, r: &mut Rng) -> Genome {
    let mut x = Genome::neutral(role);
    let d = ch.decay;
    let grit = ch.grit;
    let bright = |oct: f32| det_exp2(ch.bright * oct);
    let mut set = |gene: usize, v: f32| x.set(gene, v);
    // Every template draws the same number of values in the same order per
    // role, so a change to one role never reshuffles another.
    match role {
        Role::Kick => {
            set(g::PITCH, r.between(26.0, 38.0));
            set(g::SWEEP, r.between(16.0, 36.0));
            set(g::SWEEP_T, r.between(14.0, 60.0));
            set(g::OSC1, if r.chance(0.8) { 0.0 } else { 1.0 });
            set(g::DECAY, r.between(180.0, 800.0) * d);
            set(g::HOLD, r.between(0.0, 30.0));
            set(g::NOISE_LEVEL, r.between(0.03, 0.2));
            set(g::NOISE_MODE, 2.0);
            set(g::NOISE_FREQ, r.between(2_500.0, 8_000.0));
            set(g::NOISE_DECAY, r.between(3.0, 12.0));
            set(g::FM_INDEX, r.between(0.0, 1.5) + grit);
            set(g::FM_RATIO, r.pick(&[1.0, 1.5, 2.0]));
            set(g::FM_DECAY, r.between(4.0, 20.0));
            set(g::DRIVE, r.between(0.08, 0.45) + grit * 0.5);
            set(g::CUTOFF, r.between(2_000.0, 9_000.0) * bright(1.0));
            set(g::RES_MIX, r.between(0.0, 0.1) + ch.metal * 0.2);
            set(g::RES_SHAPE, 1.0);
            set(g::RES_DECAY, r.between(80.0, 300.0) * d);
            set(g::LEVEL, 0.0);
            set(g::WIDTH, 0.0);
            set(g::SEND, ch.space * 0.3);
        }
        Role::Bass => {
            set(g::PITCH, ch.snap(r.between(30.0, 45.0)));
            set(g::SWEEP, r.between(0.0, 12.0));
            set(g::SWEEP_T, r.between(20.0, 80.0));
            set(g::OSC1, if r.chance(0.6) { 0.0 } else { 1.0 });
            set(g::OSC2, 0.0);
            set(g::OSC2_RATIO, 2.0);
            set(g::OSC_MIX, r.between(0.0, 0.3));
            set(g::DECAY, r.between(400.0, 1500.0) * d);
            set(g::DRIVE, r.between(0.15, 0.6) + grit * 0.4);
            set(g::CUTOFF, r.between(800.0, 4_000.0) * bright(1.0));
            set(g::FM_INDEX, r.between(0.0, 1.0) + grit);
            set(g::FM_RATIO, 1.0);
            set(g::FM_DECAY, r.between(30.0, 200.0));
            set(g::LEVEL, -2.0);
            set(g::WIDTH, 0.0);
            set(g::SEND, ch.space * 0.2);
        }
        Role::Snare => {
            set(g::PITCH, r.between(48.0, 58.0));
            set(g::SWEEP, r.between(3.0, 12.0));
            set(g::SWEEP_T, r.between(10.0, 40.0));
            set(g::OSC1, if r.chance(0.5) { 1.0 } else { 0.0 });
            set(g::OSC2, 0.0);
            set(g::OSC2_RATIO, r.between(1.45, 1.85));
            set(g::OSC_MIX, r.between(0.2, 0.45));
            set(g::TONE_LEVEL, r.between(0.45, 0.8));
            set(g::NOISE_LEVEL, r.between(0.6, 1.0));
            set(g::NOISE_MODE, if r.chance(0.5) { 1.0 } else { 2.0 });
            set(g::NOISE_FREQ, r.between(1_500.0, 6_000.0) * bright(0.7));
            set(g::NOISE_Q, r.between(0.6, 1.8));
            set(g::NOISE_DECAY, r.between(80.0, 260.0) * d);
            set(g::DECAY, r.between(60.0, 200.0) * d);
            set(g::DRIVE, r.between(0.15, 0.5) + grit * 0.4);
            set(g::CUTOFF, r.between(8_000.0, 16_000.0));
            set(g::RES_MIX, r.between(0.0, 0.25) + ch.metal * 0.4);
            set(g::RES_SHAPE, 1.0 + r.between(0.0, 0.6));
            set(g::RES_DECAY, r.between(100.0, 300.0) * d);
            set(g::LEVEL, -1.0);
            set(g::WIDTH, r.between(0.1, 0.4));
            set(g::SEND, ch.space * 0.9);
        }
        Role::Clap => {
            set(g::TONE_LEVEL, r.between(0.0, 0.1));
            set(g::PITCH, r.between(55.0, 65.0));
            set(g::NOISE_LEVEL, 1.0);
            set(g::NOISE_MODE, 1.0);
            set(g::NOISE_FREQ, r.between(900.0, 2_600.0) * bright(0.6));
            set(g::NOISE_Q, r.between(1.4, 3.2));
            set(g::NOISE_DECAY, r.between(60.0, 220.0) * d);
            set(g::BURSTS, 2.0 + r.below(2) as f32);
            set(g::BURST_GAP, r.between(6.0, 14.0));
            set(g::DECAY, r.between(80.0, 260.0) * d);
            set(g::DRIVE, r.between(0.05, 0.3) + grit * 0.3);
            set(g::RES_MIX, r.between(0.0, 0.15) + ch.metal * 0.3);
            set(g::RES_SHAPE, 2.0);
            set(g::RES_DECAY, r.between(150.0, 500.0) * d);
            set(g::LEVEL, -2.0);
            set(g::WIDTH, r.between(0.3, 0.7));
            set(g::SEND, ch.space * 1.2);
        }
        Role::HatClosed | Role::HatOpen => {
            let open = role == Role::HatOpen;
            set(g::PITCH, r.between(76.0, 88.0));
            set(g::OSC1, 3.0);
            set(g::OSC2, 3.0);
            set(g::OSC2_RATIO, r.between(1.42, 1.72));
            set(g::OSC_MIX, 0.5);
            set(g::TONE_LEVEL, r.between(0.15, 0.5));
            set(g::FM_RATIO, r.between(1.3, 3.9));
            set(g::FM_INDEX, r.between(1.5, 6.0) + grit * 2.0);
            set(g::FM_DECAY, if open { 400.0 } else { 60.0 } * d);
            set(g::NOISE_LEVEL, r.between(0.6, 1.0));
            set(g::NOISE_MODE, 2.0);
            set(g::NOISE_FREQ, r.between(6_000.0, 11_000.0) * bright(0.5));
            set(g::NOISE_DECAY, if open { r.between(200.0, 600.0) } else { r.between(15.0, 60.0) } * d);
            set(g::FILTER, 2.0);
            set(g::CUTOFF, r.between(5_000.0, 9_000.0) * bright(0.5));
            set(g::RESO, r.between(0.05, 0.35));
            set(g::DECAY, if open { r.between(250.0, 700.0) } else { r.between(25.0, 80.0) } * d);
            set(g::RES_MIX, ch.metal * 0.4);
            set(g::RES_SHAPE, 4.0);
            set(g::RES_DECAY, if open { 600.0 } else { 90.0 } * d);
            set(g::CHOKE, 1.0);
            set(g::LEVEL, r.between(-6.0, -3.0));
            set(g::WIDTH, r.between(0.3, 0.6));
            set(g::PAN, r.between(-0.25, 0.25));
            set(g::SEND, ch.space * 0.6);
        }
        Role::Tom => {
            set(g::PITCH, ch.snap(r.between(40.0, 60.0)));
            set(g::SWEEP, r.between(5.0, 14.0));
            set(g::SWEEP_T, r.between(30.0, 90.0));
            set(g::OSC1, if r.chance(0.7) { 0.0 } else { 1.0 });
            set(g::DECAY, r.between(200.0, 600.0) * d);
            set(g::NOISE_LEVEL, r.between(0.05, 0.2));
            set(g::NOISE_FREQ, r.between(800.0, 3_000.0));
            set(g::NOISE_DECAY, r.between(10.0, 40.0));
            set(g::RES_MIX, r.between(0.1, 0.4) + ch.metal * 0.3);
            set(g::RES_SHAPE, 1.0);
            set(g::RES_DECAY, r.between(200.0, 700.0) * d);
            set(g::DRIVE, r.between(0.05, 0.3) + grit * 0.3);
            set(g::CUTOFF, r.between(3_000.0, 10_000.0) * bright(0.8));
            set(g::LEVEL, -1.0);
            set(g::PAN, r.between(-0.5, 0.5));
            set(g::SEND, ch.space);
        }
        Role::Rim => {
            set(g::PITCH, r.between(70.0, 82.0));
            set(g::SWEEP, r.between(2.0, 6.0));
            set(g::SWEEP_T, r.between(2.0, 8.0));
            set(g::OSC1, 1.0);
            set(g::DECAY, r.between(20.0, 60.0) * d);
            set(g::FM_INDEX, r.between(1.0, 3.0));
            set(g::FM_RATIO, r.between(2.2, 3.6));
            set(g::FM_DECAY, r.between(5.0, 20.0));
            set(g::NOISE_LEVEL, r.between(0.2, 0.5));
            set(g::NOISE_FREQ, r.between(2_500.0, 5_000.0));
            set(g::NOISE_DECAY, r.between(3.0, 12.0));
            set(g::RES_MIX, r.between(0.3, 0.6));
            set(g::RES_SHAPE, 3.0);
            set(g::RES_DECAY, r.between(60.0, 160.0) * d);
            set(g::LEVEL, -3.0);
            set(g::PAN, r.between(-0.4, 0.4));
            set(g::SEND, ch.space);
        }
        Role::Metal => {
            set(g::PITCH, r.between(64.0, 80.0));
            set(g::OSC1, 3.0);
            set(g::OSC2, 3.0);
            set(g::OSC2_RATIO, r.between(1.45, 1.62));
            set(g::OSC_MIX, 0.5);
            set(g::FM_RATIO, ch.fm * r.between(0.9, 1.1));
            set(g::FM_INDEX, r.between(1.0, 5.0) + grit * 2.0);
            set(g::FM_DECAY, r.between(100.0, 600.0) * d);
            set(g::NOISE_LEVEL, r.between(0.2, 0.5));
            set(g::NOISE_MODE, 2.0);
            set(g::NOISE_FREQ, r.between(4_000.0, 9_000.0));
            set(g::NOISE_DECAY, r.between(80.0, 600.0) * d);
            set(g::FILTER, 1.0);
            set(g::CUTOFF, r.between(2_000.0, 6_000.0) * bright(1.0));
            set(g::RESO, r.between(0.0, 0.3));
            set(g::DECAY, r.between(200.0, 1_500.0) * d);
            set(g::RES_MIX, r.between(0.4, 0.8));
            set(g::RES_SHAPE, r.between(2.0, 4.0));
            set(g::RES_DECAY, r.between(300.0, 2_000.0) * d);
            set(g::RES_BRIGHT, r.between(0.5, 0.9));
            set(g::LEVEL, -5.0);
            set(g::WIDTH, r.between(0.4, 0.9));
            set(g::SEND, ch.space * 1.2);
        }
        Role::Perc => {
            set(g::PITCH, ch.snap(r.between(55.0, 72.0)));
            set(g::SWEEP, r.between(2.0, 8.0));
            set(g::SWEEP_T, r.between(5.0, 25.0));
            set(g::OSC1, r.below(2) as f32);
            set(g::DECAY, r.between(60.0, 220.0) * d);
            set(g::NOISE_LEVEL, r.between(0.0, 0.25));
            set(g::NOISE_FREQ, r.between(1_500.0, 5_000.0));
            set(g::NOISE_DECAY, r.between(4.0, 20.0));
            set(g::FM_INDEX, r.between(0.0, 2.0));
            set(g::FM_RATIO, ch.fm);
            set(g::FM_DECAY, r.between(10.0, 60.0));
            set(g::RES_MIX, r.between(0.3, 0.6));
            set(g::RES_SHAPE, if r.chance(0.5) { 3.0 } else { 1.0 });
            set(g::RES_DECAY, r.between(80.0, 300.0) * d);
            set(g::LEVEL, -3.0);
            set(g::PAN, r.between(-0.5, 0.5));
            set(g::SEND, ch.space);
        }
        Role::Noise => {
            set(g::TONE_LEVEL, 0.0);
            set(g::NOISE_LEVEL, 1.0);
            set(g::NOISE_MODE, if r.chance(0.5) { 1.0 } else { 2.0 });
            set(g::NOISE_FREQ, r.between(2_500.0, 9_000.0) * bright(0.6));
            set(g::NOISE_Q, r.between(0.7, 4.0));
            set(g::NOISE_DECAY, r.between(60.0, 400.0) * d);
            set(g::ATTACK, r.between(2.0, 30.0));
            set(g::DECAY, r.between(60.0, 400.0) * d);
            set(g::BURSTS, r.below(2) as f32);
            set(g::BURST_GAP, r.between(15.0, 30.0));
            set(g::RES_MIX, r.between(0.0, 0.3) + ch.metal * 0.3);
            set(g::RES_SHAPE, 2.0);
            set(g::RES_DECAY, r.between(200.0, 800.0) * d);
            set(g::LEVEL, -5.0);
            set(g::WIDTH, r.between(0.5, 1.0));
            set(g::SEND, ch.space * 1.4);
        }
        Role::Pluck => {
            set(g::PITCH, ch.snap(r.between(48.0, 72.0)));
            set(g::OSC1, 2.0 + r.below(2) as f32);
            set(g::OSC2, 2.0);
            set(g::OSC2_RATIO, if r.chance(0.5) { 0.5 } else { 1.0 });
            set(g::OSC2_DETUNE, r.between(-12.0, 12.0));
            set(g::OSC_MIX, r.between(0.2, 0.5));
            set(g::FILTER, 3.0);
            set(g::CUTOFF, r.between(250.0, 1_400.0) * bright(1.0));
            set(g::RESO, r.between(0.1, 0.5));
            set(g::FILT_ENV, r.between(3.0, 6.0));
            set(g::FILT_DECAY, r.between(60.0, 300.0) * d);
            set(g::DECAY, r.between(150.0, 600.0) * d);
            set(g::RES_MIX, r.between(0.0, 0.3) + ch.metal * 0.3);
            set(g::RES_SHAPE, 0.0);
            set(g::RES_DECAY, r.between(300.0, 1_200.0) * d);
            set(g::LEVEL, -4.0);
            set(g::WIDTH, r.between(0.2, 0.6));
            set(g::SEND, ch.space * 1.2);
        }
        Role::Blip => {
            set(g::PITCH, ch.snap(r.between(72.0, 96.0)));
            set(g::OSC1, if r.chance(0.6) { 0.0 } else { 3.0 });
            set(g::SWEEP, r.between(0.0, 12.0));
            set(g::SWEEP_T, r.between(3.0, 20.0));
            set(g::DECAY, r.between(20.0, 100.0) * d);
            set(g::FM_INDEX, r.between(0.0, 3.0) + grit * 2.0);
            set(g::FM_RATIO, r.pick(&[1.0, 2.0, 3.0, 4.0]));
            set(g::FM_DECAY, r.between(10.0, 80.0));
            set(g::LEVEL, -6.0);
            set(g::PAN, r.between(-0.6, 0.6));
            set(g::SEND, ch.space * 1.3);
        }
        Role::Zap => {
            set(g::PITCH, r.between(50.0, 80.0));
            set(g::SWEEP, r.between(24.0, 48.0));
            set(g::SWEEP_T, r.between(20.0, 120.0));
            set(g::OSC1, 2.0 + r.below(2) as f32);
            set(g::FILTER, if r.chance(0.5) { 3.0 } else { 0.0 });
            set(g::CUTOFF, r.between(500.0, 3_000.0) * bright(1.0));
            set(g::RESO, r.between(0.45, 0.85));
            set(g::FILT_ENV, r.between(2.0, 5.0));
            set(g::FILT_DECAY, r.between(30.0, 150.0) * d);
            set(g::DECAY, r.between(80.0, 300.0) * d);
            set(g::DRIVE, r.between(0.3, 0.7) + grit * 0.3);
            set(g::FM_INDEX, r.between(0.0, 4.0) * grit);
            set(g::FM_RATIO, ch.fm);
            set(g::LEVEL, -6.0);
            set(g::PAN, r.between(-0.5, 0.5));
            set(g::SEND, ch.space);
        }
        Role::Bell => {
            set(g::PITCH, ch.snap(r.between(60.0, 84.0)));
            set(g::OSC1, 0.0);
            set(g::FM_RATIO, r.pick(&[1.41, 2.76, 3.5, 4.0]));
            set(g::FM_INDEX, r.between(2.0, 6.0));
            set(g::FM_DECAY, r.between(300.0, 1_500.0) * d);
            set(g::DECAY, r.between(600.0, 2_500.0) * d);
            set(g::RES_MIX, r.between(0.2, 0.5) + ch.metal * 0.3);
            set(g::RES_SHAPE, r.between(2.5, 3.5));
            set(g::RES_DECAY, r.between(800.0, 3_000.0) * d);
            set(g::LEVEL, -7.0);
            set(g::WIDTH, r.between(0.3, 0.7));
            set(g::SEND, ch.space * 1.4);
        }
    }
    // The scene's shared colour on top of every role.
    let shape = x.plain(g::RES_SHAPE) * 0.4 + ch.shape * 0.6;
    x.set(g::RES_SHAPE, shape);
    let drive = x.plain(g::DRIVE) + grit * 0.3;
    x.set(g::DRIVE, drive);
    let res = x.plain(g::RES_MIX) + ch.metal * 0.25;
    x.set(g::RES_MIX, res);
    x.set(g::RES_MODES, (8 + r.below(17)) as f32);
    x.set(g::RES_POS, r.between(0.1, 0.9));
    x.set(g::RES_DAMP, r.between(0.2, 0.8));
    x.set(g::VEL_TONE, r.between(0.3, 0.8));
    for v in &mut x.v {
        *v = quantize(*v);
    }
    x
}

/// Index of a categorical gene from a raw (maybe re-based) value.
fn cat_index(v: f32, n: usize) -> usize {
    ((reflect(v) * (n - 1) as f32).round() as usize).min(n - 1)
}

fn cat_value(i: usize, n: usize) -> f32 {
    quantize(i as f32 / (n - 1) as f32)
}

/// The draws of one gene of a growth path, in a fixed order.
struct Step {
    dir: f32,
    switch_at: f32,
    shift: f32,
}

fn steps(path: u64) -> impl Iterator<Item = Step> {
    let mut r = Rng::new(path);
    (0..N_GENES).map(move |gene| {
        let dir = r.normal() * sensitivity(gene);
        let switch_at = r.between(0.3, 1.4);
        let shift = r.unit();
        Step { dir, switch_at, shift }
    })
}

/// How many places a categorical gene has moved at distance `d`.
fn shift_at(step: &Step, gene: usize, d: f32) -> usize {
    let n = choice_count(gene);
    if sensitivity(gene) <= 0.0 || n < 2 || d < step.switch_at { 0 } else { 1 + ((step.shift * (n - 1) as f32) as usize).min(n - 2) }
}

/// The sound `d` along growth path `path` from `core`.
pub fn grow(core: &Genome, path: u64, d: f32) -> Genome {
    let d = d.max(0.0);
    let mut out = *core;
    for (gene, step) in steps(path).enumerate() {
        if categorical(gene) {
            let n = choice_count(gene);
            let i = (cat_index(core.v[gene], n) + shift_at(&step, gene, d)) % n;
            out.v[gene] = cat_value(i, n);
        } else {
            out.v[gene] = quantize(reflect(core.v[gene] + d * step.dir));
        }
    }
    out
}

/// The core whose path `path` passes through `sound` at distance `d`: what
/// an edit of a grown sound becomes, so the cell keeps its distance.
pub fn rebase(sound: &Genome, path: u64, d: f32) -> Genome {
    let d = d.max(0.0);
    let s = sound.folded();
    let mut core = s;
    for (gene, step) in steps(path).enumerate() {
        if categorical(gene) {
            let n = choice_count(gene);
            let i = (cat_index(s.v[gene], n) + n - shift_at(&step, gene, d) % n) % n;
            core.v[gene] = cat_value(i, n);
        } else {
            core.v[gene] = quantize(s.v[gene] - d * step.dir);
        }
    }
    core
}

/// The genome between `a` (t = 0) and `b` (t = 1): amounts slide, names
/// switch halfway, ordered counts round.
pub fn morph(a: &Genome, b: &Genome, t: f32) -> Genome {
    let t = t.clamp(0.0, 1.0);
    let mut out = if t < 0.5 { *a } else { *b };
    for gene in 0..N_GENES {
        if !categorical(gene) {
            out.v[gene] = a.v[gene] + (b.v[gene] - a.v[gene]) * t;
        }
    }
    out
}

/// Bilinear morph of four corners: `c[0]` at (0, 0), `c[1]` at (1, 0),
/// `c[2]` at (0, 1), `c[3]` at (1, 1).
pub fn morph4(c: [&Genome; 4], x: f32, y: f32) -> Genome {
    let top = morph(c[0], c[1], x);
    let bottom = morph(c[2], c[3], x);
    morph(&top, &bottom, y)
}

/// Options of a new scene.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GrowOpts {
    pub species: Species,
    /// How far from their cores the sounds sit (0 = every sound is its core).
    pub spread: f32,
}

impl Default for GrowOpts {
    fn default() -> Self {
        Self { species: Species::Kit, spread: 1.0 }
    }
}

/// The scene a seed grows: cores from the species, paths and distances
/// from the seed.
pub fn generate(seed: u64, opts: GrowOpts) -> Scene {
    let mut r = Rng::new(part(seed, 99, 0));
    let ch = Character::of(opts.species, &mut r);
    let layout = opts.species.layout();
    let mut cores = [Genome::default(); SOUNDS];
    let mut sounds = [Genome::default(); SOUNDS];
    let mut paths = [0u64; SOUNDS];
    let mut distances = [0f32; SOUNDS];
    for i in 0..SOUNDS {
        let mut rr = Rng::new(part(seed, i as u64, 1));
        cores[i] = template(layout[i], &ch, &mut rr);
        paths[i] = part(seed, i as u64, 2);
        distances[i] = quantize(opts.spread.clamp(0.0, 1.5) * rr.between(0.08, 0.6));
        sounds[i] = grow(&cores[i], paths[i], distances[i]);
    }
    Scene { seed, species: opts.species, sounds, cores, paths, distances, locks: 0 }
}

/// A new roll: the scene of `seed`, reached from `prev` by `deviance`
/// (1 = entirely the new scene, 0.2 = a near neighbour of the old one).
/// Locked cells stay as they are.
pub fn roll(prev: &Scene, seed: u64, species: Species, deviance: f32) -> Scene {
    let fresh = generate(seed, GrowOpts { species, spread: 1.0 });
    let t = deviance.clamp(0.0, 1.0);
    let mut out = fresh.clone();
    out.locks = prev.locks;
    for i in 0..SOUNDS {
        if prev.locked(i) {
            out.cores[i] = prev.cores[i];
            out.sounds[i] = prev.sounds[i];
            out.paths[i] = prev.paths[i];
            out.distances[i] = prev.distances[i];
            continue;
        }
        if t < 1.0 {
            // Morph the sounds, then re-base so the cell sits on its new path.
            let sound = morph(&prev.sounds[i], &fresh.sounds[i], t);
            let d = quantize(prev.distances[i] + (fresh.distances[i] - prev.distances[i]) * t);
            out.distances[i] = d;
            out.sounds[i] = grow(&rebase(&sound, out.paths[i], d), out.paths[i], d);
            out.cores[i] = rebase(&sound, out.paths[i], d);
        }
    }
    out
}

/// Nudge every unlocked sound by `amount` (0..1) in a random direction drawn
/// from `seed`; each keeps its role, path and distance.
pub fn mutate(scene: &Scene, seed: u64, amount: f32) -> Scene {
    let mut out = scene.clone();
    let a = amount.clamp(0.0, 1.0);
    for i in 0..SOUNDS {
        if scene.locked(i) {
            continue;
        }
        let mut r = Rng::new(part(seed, i as u64, 3));
        let mut core = scene.cores[i];
        for gene in 0..N_GENES {
            let step = r.normal() * sensitivity(gene) * a * 0.6;
            let flip = r.chance(a * 0.12);
            let pick = r.unit();
            if categorical(gene) {
                let n = choice_count(gene);
                if flip && sensitivity(gene) > 0.0 && n > 1 {
                    let now = cat_index(core.v[gene], n);
                    core.v[gene] = cat_value((now + 1 + (pick * (n - 1) as f32) as usize % (n - 1)) % n, n);
                }
            } else {
                core.v[gene] = quantize(core.v[gene] + step);
            }
        }
        out.cores[i] = core;
        out.sounds[i] = grow(&core, scene.paths[i], scene.distances[i]);
    }
    out
}

/// Move cell `i` to `distance` along its path (dragging it in the visual).
pub fn regrow(scene: &Scene, i: usize, distance: f32) -> Scene {
    let mut out = scene.clone();
    if i < SOUNDS {
        let d = quantize(distance.clamp(0.0, 1.5));
        out.distances[i] = d;
        out.sounds[i] = grow(&scene.cores[i], scene.paths[i], d);
    }
    out
}

/// Put `sound` in cell `i` (an edit or a match) keeping the cell's distance.
pub fn replace(scene: &Scene, i: usize, sound: &Genome) -> Scene {
    let mut out = scene.clone();
    if i < SOUNDS {
        let s = sound.folded();
        out.cores[i] = rebase(&s, scene.paths[i], scene.distances[i]);
        out.sounds[i] = s;
    }
    out
}

/// Every gene index whose value is a plain amount (for searches over genomes).
pub fn continuous_genes() -> impl Iterator<Item = usize> {
    (0..N_GENES).filter(|&k| !categorical(k) && GENES[k].max > GENES[k].min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::bloom::genome::Band;

    #[test]
    fn the_same_seed_grows_the_same_scene() {
        for species in Species::ALL {
            let a = generate(42, GrowOpts { species, spread: 1.0 });
            let b = generate(42, GrowOpts { species, spread: 1.0 });
            assert_eq!(a, b);
            let c = generate(43, GrowOpts { species, spread: 1.0 });
            assert_ne!(a.sounds, c.sounds);
        }
    }

    /// A fingerprint of a scene that must never change: saved projects play
    /// the scene their seed grows. A change here is a format break.
    #[test]
    fn a_known_seed_keeps_its_scene() {
        let s = generate(1337, GrowOpts::default());
        let mut h: u64 = 0;
        for g in &s.sounds {
            h = h.rotate_left(7) ^ g.hash();
        }
        let again = generate(1337, GrowOpts::default());
        let mut h2: u64 = 0;
        for g in &again.sounds {
            h2 = h2.rotate_left(7) ^ g.hash();
        }
        assert_eq!(h, h2);
        // The genes stay on the quantised grid.
        assert!(s.sounds.iter().all(|g| g.v.iter().all(|v| quantize(*v) == *v)));
    }

    #[test]
    fn a_kit_is_balanced() {
        for species in [Species::Kit, Species::Mixed] {
            let s = generate(7, GrowOpts { species, spread: 1.0 });
            let count = |b: Band| s.sounds.iter().filter(|g| g.role.band() == b).count();
            assert!(count(Band::Low) >= 2, "{species:?} low");
            assert!(count(Band::Mid) >= 3, "{species:?} mid");
            assert!(count(Band::High) >= 3, "{species:?} hats");
            assert!(count(Band::Tonal) >= 1, "{species:?} tonal");
        }
        for species in Species::ALL {
            let layout = species.layout();
            assert_eq!(layout[0], Role::Kick, "every species starts with a kick on C1");
            assert!(layout.iter().any(|r| r.band() == Band::High), "{species:?} has a hat");
            assert!(layout.iter().any(|r| r.band() == Band::Low));
        }
        // Low roles are pitched low, hats high.
        let s = generate(9, GrowOpts::default());
        let kick = s.cores[0].plain(g::PITCH);
        let hat = s.cores[6].plain(g::PITCH);
        assert!(kick < 40.0 && hat > 70.0);
    }

    #[test]
    fn distance_zero_is_the_core_and_paths_are_repeatable() {
        let s = generate(5, GrowOpts::default());
        for i in 0..SOUNDS {
            let at0 = regrow(&s, i, 0.0);
            assert_eq!(at0.sounds[i], grow(&s.cores[i], s.paths[i], 0.0));
            assert!(at0.sounds[i].gene_distance(&s.cores[i].folded()) < 1e-5);
            // Same seed, same path: dragging out and back lands where it started.
            let out = regrow(&regrow(&s, i, 1.2), i, s.distances[i]);
            assert_eq!(out.sounds[i], s.sounds[i]);
        }
    }

    #[test]
    fn genes_move_further_with_distance() {
        let s = generate(11, GrowOpts::default());
        for i in 0..SOUNDS {
            let d = |x: f32| grow(&s.cores[i], s.paths[i], x).gene_distance(&s.cores[i]);
            assert!(d(0.1) < d(0.3) && d(0.3) < d(0.6), "cell {i}: {} {} {}", d(0.1), d(0.3), d(0.6));
        }
    }

    #[test]
    fn rebase_keeps_an_edit_on_the_path() {
        let s = generate(3, GrowOpts::default());
        let i = 4;
        let mut edited = s.sounds[i];
        edited.set(g::CUTOFF, 900.0);
        edited.set(g::OSC1, 2.0);
        let t = replace(&s, i, &edited);
        assert_eq!(t.distances[i], s.distances[i]);
        let regrown = grow(&t.cores[i], t.paths[i], t.distances[i]);
        assert!(regrown.gene_distance(&edited) < 1e-4, "{}", regrown.gene_distance(&edited));
        assert_eq!(regrown.index(g::OSC1), 2);
    }

    #[test]
    fn locks_survive_rolls_and_mutations() {
        let mut s = generate(21, GrowOpts::default());
        s.set_locked(2, true);
        s.set_locked(9, true);
        let rolled = roll(&s, 22, Species::Glitch, 1.0);
        assert_eq!(rolled.sounds[2], s.sounds[2]);
        assert_eq!(rolled.sounds[9], s.sounds[9]);
        assert_ne!(rolled.sounds[3], s.sounds[3]);
        assert_eq!(rolled.locks, s.locks);
        let m = mutate(&rolled, 5, 0.8);
        assert_eq!(m.sounds[2], s.sounds[2]);
        assert_eq!(m.cores[9], s.cores[9]);
        assert_ne!(m.sounds[3], rolled.sounds[3]);
    }

    #[test]
    fn deviance_sets_how_far_a_roll_moves() {
        let s = generate(30, GrowOpts::default());
        let near = roll(&s, 31, Species::Kit, 0.15);
        let far = roll(&s, 31, Species::Kit, 1.0);
        let moved = |t: &Scene| (0..SOUNDS).map(|i| t.sounds[i].gene_distance(&s.sounds[i])).sum::<f32>();
        assert!(moved(&near) < moved(&far) * 0.5, "{} vs {}", moved(&near), moved(&far));
        assert_eq!(near.seed, 31);
    }

    #[test]
    fn morph_slides_amounts_and_switches_names() {
        let mut a = Genome::neutral(Role::Kick);
        let mut b = Genome::neutral(Role::HatClosed);
        a.set(g::CUTOFF, 200.0);
        b.set(g::CUTOFF, 8_000.0);
        a.set(g::OSC1, 0.0);
        b.set(g::OSC1, 3.0);
        a.set(g::RES_MODES, 8.0);
        b.set(g::RES_MODES, 24.0);
        let m = morph(&a, &b, 0.25);
        assert!(m.plain(g::CUTOFF) > 200.0 && m.plain(g::CUTOFF) < 8_000.0);
        assert_eq!(m.index(g::OSC1), 0);
        assert_eq!(m.role, Role::Kick);
        let m = morph(&a, &b, 0.75);
        assert_eq!(m.index(g::OSC1), 3);
        assert_eq!(m.plain(g::RES_MODES), 20.0);
        assert_eq!(morph(&a, &b, 0.0), a);
        assert_eq!(morph4([&a, &b, &a, &b], 1.0, 0.5), b);
    }
}
