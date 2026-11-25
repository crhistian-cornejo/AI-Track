//! The drum voices of `instrument.drumsynth`: one [`Voice`] struct renders
//! every type and model (a `match` per sample), so a pool of them is a flat
//! array with no allocation. Every model is built from a handful of
//! envelopes, phase accumulators, TPT filters and modal resonators; all the
//! constants a hit needs are worked out once in [`Voice::trigger`].

use crate::instrument::vault::kit::{flush, soft};
use std::f32::consts::{PI, TAU};

/// The nine kinds of percussion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Kick,
    Snare,
    Rimshot,
    Hihat,
    Clap,
    Tom,
    Perc,
    Cymbal,
    Fx,
}

pub const KINDS: usize = 9;

pub const KICK_MODELS: &[&str] = &["808", "Punch", "Techno"];
pub const SNARE_MODELS: &[&str] = &["Tonal", "Modal", "Electro"];
pub const RIMSHOT_MODELS: &[&str] = &["Classic", "Wood"];
pub const HIHAT_MODELS: &[&str] = &["Metal", "Noise", "Crunch"];
pub const CLAP_MODELS: &[&str] = &["Classic", "Tight", "Room"];
pub const TOM_MODELS: &[&str] = &["Sine", "Skin", "Electro"];
pub const PERC_MODELS: &[&str] = &["FM", "Block", "Cowbell"];
pub const CYMBAL_MODELS: &[&str] = &["Crash", "Ride", "Splash"];
pub const FX_MODELS: &[&str] = &["Zap", "Sweep", "Bleep"];

impl Kind {
    pub const ALL: [Kind; KINDS] = [Kind::Kick, Kind::Snare, Kind::Rimshot, Kind::Hihat, Kind::Clap, Kind::Tom, Kind::Perc, Kind::Cymbal, Kind::Fx];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(i: usize) -> Kind {
        Self::ALL[i.min(KINDS - 1)]
    }

    pub fn label(self) -> &'static str {
        ["Kick", "Snare", "Rimshot", "Hihat", "Clap", "Tom", "Perc", "Cymbal", "FX"][self as usize]
    }

    pub fn models(self) -> &'static [&'static str] {
        [KICK_MODELS, SNARE_MODELS, RIMSHOT_MODELS, HIHAT_MODELS, CLAP_MODELS, TOM_MODELS, PERC_MODELS, CYMBAL_MODELS, FX_MODELS][self as usize]
    }
}

/// What a type's knobs say, read when a hit starts.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub model: usize,
    /// Semitones.
    pub tune: f32,
    pub decay_ms: f32,
    /// 0..1.
    pub tone: f32,
    /// 0..1.
    pub snap: f32,
}

/// What the key adds to the knobs: a pitch offset and a decay factor.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub tune_st: f32,
    pub decay_mul: f32,
}

/// Output trim per type and model, so every default hit peaks near 0.8
/// (a test keeps the table honest).
const TRIM: [[f32; 3]; KINDS] = [
    [0.89, 0.71, 0.71],
    [0.52, 0.24, 0.62],
    [0.82, 0.60, 0.0],
    [0.55, 0.62, 0.57],
    [0.90, 0.76, 1.00],
    [0.75, 0.71, 0.86],
    [0.80, 0.68, 0.72],
    [0.89, 0.76, 1.00],
    [0.80, 0.90, 0.80],
];

const E_AMP: usize = 0;
const E_NOISE: usize = 1;
const E_PITCH: usize = 2;
const E_CLICK: usize = 3;
const E_AUX: usize = 4;

/// Inharmonic square-wave set of the 808 hi-hat circuit.
const HAT_SET: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];
/// A wider set for cymbals.
const CYM_SET: [f32; 6] = [263.2, 403.7, 545.1, 700.4, 833.6, 1047.3];
/// Membrane partials of the modal snare.
const SNARE_RATIOS: [f32; 5] = [1.0, 1.59, 2.14, 2.30, 2.65];
const SNARE_AMPS: [f32; 5] = [1.0, 0.6, 0.45, 0.3, 0.2];
const SNARE_DECS: [f32; 5] = [1.0, 0.7, 0.55, 0.45, 0.4];
/// Scale steps of the bleeps.
const BLEEP_STEPS: [f32; 6] = [0.0, 3.0, 7.0, 10.0, 12.0, 15.0];

/// Coefficient of a decay that falls 60 dB in `ms`.
#[inline]
fn coef60(ms: f32, sr: f32) -> f32 {
    (-6.9078 / (ms.max(0.5) * 0.001 * sr)).exp()
}

/// Coefficient of an exponential with time constant `ms`.
#[inline]
fn tau(ms: f32, sr: f32) -> f32 {
    (-1.0 / (ms.max(0.05) * 0.001 * sr)).exp()
}

#[inline]
fn semis(st: f32) -> f32 {
    (st / 12.0).exp2()
}

#[inline]
fn sq(ph: f32) -> f32 {
    if ph < 0.5 { 1.0 } else { -1.0 }
}

#[inline]
fn tri(ph: f32) -> f32 {
    4.0 * (ph - 0.5).abs() - 1.0
}

/// 1 on the first sample of a hit, 0 after: the strike that rings a body.
#[inline]
fn strike(age: u32) -> f32 {
    if age == 0 { 1.0 } else { 0.0 }
}

/// A TPT state-variable filter.
#[derive(Clone, Copy, Debug, Default)]
struct Sv {
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
    ic1: f32,
    ic2: f32,
}

impl Sv {
    fn set(&mut self, fc: f32, q: f32, sr: f32) {
        let fc = fc.clamp(20.0, sr * 0.45);
        let g = (PI * fc / sr).tan();
        let k = 1.0 / q.max(0.1);
        self.a1 = 1.0 / (1.0 + g * (g + k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
        self.k = k;
    }

    fn clear(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }

    /// `(low, band, high)`; the band is `1/k` times unity at the centre.
    #[inline]
    fn tick(&mut self, x: f32) -> (f32, f32, f32) {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = flush(2.0 * v1 - self.ic1);
        self.ic2 = flush(2.0 * v2 - self.ic2);
        (v2, v1, x - self.k * v1 - v2)
    }
}

/// A two-pole resonator that rings when struck: one mode of a body.
#[derive(Clone, Copy, Debug, Default)]
struct Mode {
    c1: f32,
    c2: f32,
    a: f32,
    y1: f32,
    y2: f32,
}

impl Mode {
    fn set(&mut self, f: f32, dec_ms: f32, amp: f32, sr: f32) {
        let f = f.clamp(20.0, sr * 0.45);
        let r = coef60(dec_ms, sr);
        let w = TAU * f / sr;
        self.c1 = 2.0 * r * w.cos();
        self.c2 = -r * r;
        // An impulse rings at `amp`, whatever the pitch.
        self.a = amp * w.sin();
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        let y = self.a * x + self.c1 * self.y1 + self.c2 * self.y2;
        self.y2 = self.y1;
        self.y1 = flush(y);
        y
    }
}

#[derive(Clone, Debug)]
pub struct Voice {
    sr: f32,
    active: bool,
    kind: Kind,
    model: usize,
    age: u32,
    /// The voice is held for at least this many samples.
    min_len: u32,
    amp: f32,
    f0: f32,
    tone: f32,
    snap: f32,
    dec_ms: f32,
    /// Constants worked out at the start of a hit.
    p: [f32; 4],
    ph: [f32; 8],
    e: [f32; 5],
    c: [f32; 5],
    sv: [Sv; 2],
    modes: [Mode; 5],
    rng: u32,
    /// Trigger times (samples) of bursts and bleeps.
    sched: [u32; 6],
    n_sched: usize,
    next: usize,
    /// The jump a retrigger would have made, fading out.
    fix: f32,
    last: f32,
    fade: f32,
    fading: bool,
    /// A slow peak follower for the meters.
    vis: f32,
}

impl Voice {
    pub fn new(seed: u32) -> Self {
        Self {
            sr: 48_000.0,
            active: false,
            kind: Kind::Kick,
            model: 0,
            age: 0,
            min_len: 0,
            amp: 0.0,
            f0: 100.0,
            tone: 0.5,
            snap: 0.5,
            dec_ms: 200.0,
            p: [0.0; 4],
            ph: [0.0; 8],
            e: [0.0; 5],
            c: [1.0; 5],
            sv: [Sv::default(); 2],
            modes: [Mode::default(); 5],
            rng: seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453) | 1,
            sched: [0; 6],
            n_sched: 0,
            next: 0,
            fix: 0.0,
            last: 0.0,
            fade: 1.0,
            fading: false,
            vis: 0.0,
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn age(&self) -> u32 {
        self.age
    }

    pub fn level(&self) -> f32 {
        if self.active { self.vis } else { 0.0 }
    }

    pub fn kill(&mut self) {
        self.active = false;
        self.fix = 0.0;
        self.last = 0.0;
        self.vis = 0.0;
        self.fading = false;
        self.fade = 1.0;
    }

    /// Ramp out over ~1.3 ms and stop.
    pub fn fade_out(&mut self) {
        if self.active {
            self.fading = true;
        }
    }

    #[inline]
    fn white(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
    }

    /// Advance phase accumulator `i` by `hz` and return it.
    #[inline]
    fn adv(&mut self, i: usize, hz: f32) -> f32 {
        let p = self.ph[i] + hz.clamp(0.0, self.sr * 0.45) / self.sr;
        self.ph[i] = p - p.floor();
        self.ph[i]
    }

    /// Start a hit. `vel_gain` is the velocity curve's gain; `seed` varies the bleeps.
    pub fn trigger(&mut self, kind: Kind, s: &Settings, hit: Hit, vel_gain: f32, seed: u32) {
        if self.active {
            // Keep what the output was doing: it fades out instead of clicking.
            self.fix += self.last;
        }
        let sr = self.sr;
        let model = s.model.min(kind.models().len() - 1);
        self.kind = kind;
        self.model = model;
        self.age = 0;
        self.active = true;
        self.fading = false;
        self.fade = 1.0;
        self.amp = vel_gain * TRIM[kind.index()][model];
        self.tone = s.tone.clamp(0.0, 1.0);
        self.snap = s.snap.clamp(0.0, 1.0);
        self.dec_ms = (s.decay_ms * hit.decay_mul).clamp(4.0, 12_000.0);
        let st = (s.tune + hit.tune_st).clamp(-48.0, 48.0);
        let t = semis(st);
        self.min_len = 0;
        self.p = [0.0; 4];
        self.ph = [0.0; 8];
        self.e = [0.0; 5];
        self.c = [1.0; 5];
        self.sv[0].clear();
        self.sv[1].clear();
        self.n_sched = 0;
        self.next = 0;
        let (tone, snap, dec) = (self.tone, self.snap, self.dec_ms);
        match kind {
            Kind::Kick => match model {
                0 => {
                    self.f0 = 50.0 * t;
                    self.p[0] = 6.0 + 18.0 * tone;
                    self.e = [1.0, 0.0, 1.0, 1.0, 0.0];
                    self.c[E_AMP] = coef60(dec, sr);
                    self.c[E_PITCH] = tau(28.0, sr);
                    self.c[E_CLICK] = tau(2.5, sr);
                }
                1 => {
                    self.f0 = 55.0 * t;
                    self.p[0] = 30.0;
                    self.p[1] = 1.5 + 6.0 * tone;
                    self.e = [1.0, 0.0, 1.0, 1.0, 1.0];
                    self.c[E_AMP] = coef60(dec * 0.8, sr);
                    self.c[E_PITCH] = tau(11.0, sr);
                    self.c[E_CLICK] = tau(1.2, sr);
                    self.c[E_AUX] = tau(9.0, sr);
                }
                _ => {
                    self.f0 = 48.0 * t;
                    self.p[0] = 22.0;
                    self.p[1] = 1.0 + 7.0 * tone;
                    self.e = [1.0, 0.0, 1.0, 1.0, 0.0];
                    self.c[E_AMP] = coef60(dec * 0.75, sr);
                    self.c[E_PITCH] = tau(35.0, sr);
                    self.c[E_CLICK] = tau(3.0, sr);
                }
            },
            Kind::Snare => match model {
                0 => {
                    self.f0 = 185.0 * t;
                    self.p[0] = 4.0;
                    self.e = [1.0, 1.0, 1.0, 0.0, 0.0];
                    self.c[E_AMP] = coef60(dec * 0.45, sr);
                    self.c[E_NOISE] = coef60(dec, sr);
                    self.c[E_PITCH] = tau(14.0, sr);
                    self.sv[0].set(1500.0 + 3500.0 * tone, 0.8, sr);
                }
                1 => {
                    self.f0 = 190.0 * t;
                    let stretch = 0.7 + 0.6 * tone;
                    for i in 0..5 {
                        let ratio = 1.0 + (SNARE_RATIOS[i] - 1.0) * stretch;
                        self.modes[i].set(self.f0 * ratio, dec * 0.5 * SNARE_DECS[i], SNARE_AMPS[i], sr);
                    }
                    self.e = [0.0, 1.0, 0.0, 1.0, 0.0];
                    self.c[E_NOISE] = coef60(dec * 0.8, sr);
                    self.c[E_CLICK] = tau(1.5, sr);
                    self.sv[0].set(5200.0 * semis(st * 0.5), 1.4, sr);
                    self.min_len = (sr * 0.002) as u32;
                }
                _ => {
                    self.f0 = 210.0 * t;
                    self.p[0] = 12.0;
                    self.e = [1.0, 1.0, 1.0, 0.0, 0.0];
                    self.c[E_AMP] = coef60(dec * 0.2, sr);
                    self.c[E_NOISE] = coef60(dec, sr);
                    self.c[E_PITCH] = tau(8.0, sr);
                    self.sv[0].set(1800.0 + 4000.0 * tone, 0.7, sr);
                }
            },
            Kind::Rimshot => match model {
                0 => {
                    self.f0 = 455.0 * t;
                    self.p[0] = 1667.0 * t;
                    self.e = [1.0, 0.0, 0.0, 1.0, 1.0];
                    self.c[E_AMP] = coef60(dec, sr);
                    self.c[E_AUX] = coef60(dec * 0.45, sr);
                    self.c[E_CLICK] = tau(0.6, sr);
                }
                _ => {
                    let sc = t * (0.85 + 0.3 * tone);
                    for (i, (f, a, d)) in [(820.0, 1.0, 1.0), (2050.0, 0.7, 0.55), (3500.0, 0.5, 0.35)].into_iter().enumerate() {
                        self.modes[i].set(f * sc, dec * d, a, sr);
                    }
                    self.e[E_CLICK] = 1.0;
                    self.c[E_CLICK] = tau(0.3, sr);
                    self.min_len = (dec * 0.001 * sr) as u32;
                }
            },
            Kind::Hihat => {
                self.f0 = t;
                self.e = [1.0, 1.0, 0.0, 1.0, 0.0];
                self.c[E_AMP] = coef60(dec, sr);
                self.c[E_NOISE] = coef60(dec * 0.7, sr);
                self.c[E_CLICK] = tau(3.0, sr);
                match model {
                    0 => {
                        self.sv[0].set(5500.0 * (0.7 + 0.9 * tone), 0.8, sr);
                        self.sv[1].set(9500.0, 0.9, sr);
                    }
                    1 => {
                        self.sv[0].set(6000.0 * (0.7 + 0.9 * tone), 0.8, sr);
                        self.sv[1].set(11_000.0, 1.2, sr);
                        self.c[E_CLICK] = tau(4.0, sr);
                    }
                    _ => {
                        self.sv[0].set(6500.0 * (0.7 + 0.9 * tone), 0.8, sr);
                    }
                }
            }
            Kind::Clap => {
                self.f0 = t;
                let spread = 0.5 + snap;
                let (times, burst_tau, tail_mul, center): (&[f32], f32, f32, f32) = match model {
                    0 => (&[0.0, 10.0, 21.0, 33.0], 7.0, 1.0, 1000.0),
                    1 => (&[0.0, 6.0, 12.0], 5.0, 0.6, 1600.0),
                    _ => (&[0.0, 12.0, 25.0, 41.0, 58.0], 8.0, 1.0, 900.0),
                };
                self.n_sched = times.len();
                for (i, ms) in times.iter().enumerate() {
                    self.sched[i] = (ms * spread * 0.001 * sr) as u32;
                }
                self.min_len = self.sched[self.n_sched - 1] + 1;
                self.c[E_CLICK] = tau(burst_tau, sr);
                self.c[E_AMP] = coef60(dec * tail_mul, sr);
                self.c[E_AUX] = coef60(dec * 2.0, sr);
                self.sv[0].set(center * (1.0 + 1.5 * tone) * t, 1.6, sr);
                self.sv[1].set(2500.0, 0.7, sr);
                self.p[0] = if model == 2 { 0.3 } else { 0.0 };
            }
            Kind::Tom => {
                self.f0 = 110.0 * t;
                self.e = [1.0, 0.0, 1.0, 1.0, 1.0];
                self.c[E_CLICK] = tau(1.5, sr);
                match model {
                    0 => {
                        self.p[0] = 4.0 + 12.0 * tone;
                        self.c[E_AMP] = coef60(dec, sr);
                        self.c[E_PITCH] = tau(30.0, sr);
                    }
                    1 => {
                        self.p[0] = 5.0;
                        self.c[E_AMP] = coef60(dec, sr);
                        self.c[E_PITCH] = tau(25.0, sr);
                        self.modes[0].set(self.f0 * 1.59, dec * 0.5, 0.8 * tone, sr);
                        self.modes[1].set(self.f0 * 2.14, dec * 0.35, 0.55 * tone, sr);
                        self.min_len = (sr * 0.002) as u32;
                    }
                    _ => {
                        self.p[0] = 10.0 + 14.0 * tone;
                        self.c[E_AMP] = coef60(dec * 0.8, sr);
                        self.c[E_PITCH] = tau(15.0, sr);
                    }
                }
                // Tom envelopes: the aux slot is unused.
                self.e[E_AUX] = 0.0;
            }
            Kind::Perc => {
                self.f0 = 440.0 * t;
                match model {
                    0 => {
                        self.p[0] = 1.0 + 5.0 * tone;
                        self.p[1] = 2.0 + 6.0 * snap;
                        self.e = [1.0, 0.0, 1.0, 0.0, 1.0];
                        self.c[E_AMP] = coef60(dec, sr);
                        self.c[E_AUX] = coef60(dec * 0.4, sr);
                        self.c[E_PITCH] = tau(10.0, sr);
                    }
                    1 => {
                        let sc = 2.0 * t * (0.8 + 0.4 * tone);
                        for (i, (r, a, d)) in [(440.0, 1.0, 1.0), (1043.0, 0.5, 0.5), (1817.0, 0.3, 0.3)].into_iter().enumerate() {
                            self.modes[i].set(r * sc, dec * d, a, sr);
                        }
                        self.e[E_CLICK] = 1.0;
                        self.c[E_CLICK] = tau(0.4, sr);
                        self.min_len = (dec * 0.001 * sr) as u32;
                    }
                    _ => {
                        self.e = [1.0, 0.0, 0.0, 1.0, 0.0];
                        self.c[E_AMP] = coef60(dec, sr);
                        self.c[E_CLICK] = tau(12.0, sr);
                        self.sv[0].set(2640.0 * t * (0.7 + 0.8 * tone), 1.5, sr);
                    }
                }
            }
            Kind::Cymbal => {
                self.f0 = t;
                self.e = [1.0, 1.0, 0.0, 0.0, if model == 1 { 1.0 } else { 0.0 }];
                match model {
                    0 => {
                        self.c[E_AMP] = coef60(dec, sr);
                        self.c[E_NOISE] = coef60(dec * 0.75, sr);
                        self.sv[0].set(3200.0 + 3000.0 * tone, 0.7, sr);
                        self.sv[1].set(4500.0, 0.7, sr);
                    }
                    1 => {
                        self.c[E_AMP] = coef60(dec * 0.35, sr);
                        self.c[E_NOISE] = coef60(dec, sr);
                        self.c[E_AUX] = coef60(dec * 0.6, sr);
                        self.sv[0].set(5000.0 + 3000.0 * tone, 0.7, sr);
                        self.sv[1].set(6000.0, 0.7, sr);
                    }
                    _ => {
                        self.f0 = t * 1.3;
                        self.c[E_AMP] = coef60(dec * 0.45, sr);
                        self.c[E_NOISE] = coef60(dec * 0.35, sr);
                        self.sv[0].set(6000.0 + 3000.0 * tone, 0.7, sr);
                        self.sv[1].set(7000.0, 0.7, sr);
                    }
                }
            }
            Kind::Fx => match model {
                0 => {
                    self.f0 = 80.0 * t;
                    self.p[0] = 6000.0 * (0.3 + 0.7 * tone) * t;
                    self.p[1] = 0.5 + 2.0 * snap;
                    self.e = [1.0, 0.0, 1.0, 0.0, 0.0];
                    self.c[E_AMP] = coef60(dec, sr);
                    self.c[E_PITCH] = tau(dec * 0.25, sr);
                }
                1 => {
                    self.f0 = 200.0 * t;
                    self.p[0] = 2.0 + 5.0 * tone;
                    self.p[1] = 2.0 + 10.0 * snap;
                    self.min_len = ((dec * 0.001 * sr) as u32).max(64);
                    self.sv[0].set(self.f0, self.p[1], sr);
                }
                _ => {
                    self.f0 = 330.0 * t;
                    self.p[0] = 4.0 * tone;
                    let step = ((dec * 0.001 * sr) as u32 / 4).max(32);
                    self.n_sched = 4;
                    let mut h = seed.wrapping_mul(2_654_435_761).wrapping_add(12_345);
                    for i in 0..4 {
                        self.sched[i] = step * i as u32;
                        h = h.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        // The steps' pitches wait in the phases' spare room.
                        self.ph[4 + i] = BLEEP_STEPS[(h >> 16) as usize % BLEEP_STEPS.len()];
                    }
                    self.c[E_AUX] = tau(step as f32 / sr * 1000.0 * 0.6, sr);
                    self.min_len = step * 4;
                }
            },
        }
    }

    /// One sample of the hit (before the type's level and pan).
    #[inline]
    pub fn tick(&mut self) -> f32 {
        if !self.active {
            return 0.0;
        }
        let raw = match self.kind {
            Kind::Kick => self.kick(),
            Kind::Snare => self.snare(),
            Kind::Rimshot => self.rimshot(),
            Kind::Hihat => self.hihat(),
            Kind::Clap => self.clap(),
            Kind::Tom => self.tom(),
            Kind::Perc => self.perc(),
            Kind::Cymbal => self.cymbal(),
            Kind::Fx => self.fx(),
        };
        for i in 0..5 {
            self.e[i] = flush(self.e[i] * self.c[i]);
        }
        self.age = self.age.saturating_add(1);
        let body = if raw.is_finite() { raw * self.amp } else { 0.0 };
        let mut out = body + self.fix;
        self.fix = flush(self.fix * 0.92);
        if self.fading {
            self.fade -= 1.0 / 64.0;
            if self.fade <= 0.0 {
                self.kill();
                return 0.0;
            }
            out *= self.fade;
        }
        self.last = out;
        self.vis = flush((self.vis * 0.9995).max(out.abs()));
        if self.age >= self.min_len && self.e.iter().all(|&v| v < 1e-4) {
            self.active = false;
            self.last = 0.0;
        }
        out
    }

    fn kick(&mut self) -> f32 {
        let (tone, snap) = (self.tone, self.snap);
        let f = (self.f0 * semis(self.p[0] * self.e[E_PITCH])).max(18.0);
        match self.model {
            0 => {
                let body = (TAU * self.adv(0, f)).sin();
                let click = (TAU * self.adv(1, 900.0)).sin() * self.e[E_CLICK] * snap * 0.8;
                soft((body * self.e[E_AMP] + click) * (1.0 + 0.5 * tone))
            }
            1 => {
                let m = (TAU * self.adv(1, f * 1.5)).sin();
                let c = (TAU * self.adv(0, f) + self.p[1] * self.e[E_AUX] * m).sin();
                let click = self.white() * self.e[E_CLICK] * snap * 0.7;
                c * self.e[E_AMP] + click
            }
            _ => {
                let body = (TAU * self.adv(0, f)).sin();
                let click = self.white() * self.e[E_CLICK] * snap * 0.6;
                soft(body * self.e[E_AMP] * self.p[1]) * 0.9 + click
            }
        }
    }

    fn snare(&mut self) -> f32 {
        let (tone, snap) = (self.tone, self.snap);
        match self.model {
            0 => {
                let f = self.f0 * semis(self.p[0] * self.e[E_PITCH]);
                let b = (TAU * self.adv(0, f)).sin() + 0.5 * (TAU * self.adv(1, f * 1.78)).sin();
                let n = self.white();
                let (_, _, hp) = self.sv[0].tick(n);
                b * self.e[E_AMP] * 0.7 + hp * self.e[E_NOISE] * (0.25 + 0.9 * snap)
            }
            1 => {
                let x = strike(self.age) + self.white() * self.e[E_CLICK] * 0.5;
                let mut sum = 0.0;
                for m in &mut self.modes {
                    sum += m.tick(x);
                }
                let n = self.white();
                let (_, bp, _) = self.sv[0].tick(n);
                sum * 0.5 + bp * self.sv[0].k * self.e[E_NOISE] * snap * 0.9 * (0.6 + 0.8 * tone)
            }
            _ => {
                let f = self.f0 * semis(self.p[0] * self.e[E_PITCH]);
                let body = (TAU * self.adv(0, f)).sin() * self.e[E_AMP] * 0.8;
                let n = self.white();
                let (_, _, hp) = self.sv[0].tick(n);
                let crack = soft(hp * 2.5) * self.e[E_NOISE] * (0.4 + 0.9 * snap) * 0.7;
                body + crack
            }
        }
    }

    fn rimshot(&mut self) -> f32 {
        let (tone, snap) = (self.tone, self.snap);
        match self.model {
            0 => {
                let a = (TAU * self.adv(0, self.f0)).sin() * self.e[E_AMP] * (1.0 - 0.5 * tone);
                let b = (TAU * self.adv(1, self.p[0])).sin() * self.e[E_AUX] * (0.5 + 0.5 * tone);
                let click = self.white() * self.e[E_CLICK] * snap * 0.6;
                soft((a + b) * 1.5 + click)
            }
            _ => {
                let x = strike(self.age) + self.white() * self.e[E_CLICK] * snap * 0.8;
                let mut sum = 0.0;
                for m in &mut self.modes[..3] {
                    sum += m.tick(x);
                }
                sum * 0.6
            }
        }
    }

    fn hihat(&mut self) -> f32 {
        let snap = self.snap;
        let metal = |s: &mut Self| -> [f32; 6] {
            let mut v = [0.0; 6];
            for (i, f) in HAT_SET.iter().enumerate() {
                v[i] = sq(s.adv(i, f * s.f0));
            }
            v
        };
        match self.model {
            0 => {
                let v = metal(self);
                let x = v.iter().sum::<f32>() / 6.0 * 2.0;
                let (_, _, hp) = self.sv[0].tick(x);
                let (_, bp, _) = self.sv[1].tick(hp);
                let y = hp * 0.7 + bp * self.sv[1].k * 0.6;
                y * (self.e[E_AMP] + 0.8 * snap * self.e[E_CLICK])
            }
            1 => {
                let n = self.white();
                let (_, _, hp) = self.sv[0].tick(n);
                let (_, bp, _) = self.sv[1].tick(hp);
                let y = hp * 0.6 + bp * self.sv[1].k * 0.5;
                y * (self.e[E_AMP] * 0.7 + self.e[E_NOISE] * 0.2 + snap * self.e[E_CLICK])
            }
            _ => {
                let v = metal(self);
                let ring = (v[0] * v[1] + v[2] * v[3] + v[4] * v[5]) / 3.0 * 2.0;
                let n = self.white() * 0.3 * snap;
                let (_, _, hp) = self.sv[0].tick(ring + n);
                hp * (self.e[E_AMP] + 0.6 * snap * self.e[E_CLICK])
            }
        }
    }

    fn clap(&mut self) -> f32 {
        while self.next < self.n_sched && self.age >= self.sched[self.next] {
            self.e[E_CLICK] = 1.0;
            self.next += 1;
            if self.next == self.n_sched {
                self.e[E_AMP] = 1.0;
                if self.model == 2 {
                    self.e[E_AUX] = 1.0;
                }
            }
        }
        let n = self.white();
        let (_, bp, _) = self.sv[0].tick(n);
        let band = bp * self.sv[0].k * 1.8;
        let mut out = band * (self.e[E_CLICK] * 0.9 + self.e[E_AMP] * 0.6);
        if self.model == 2 {
            let (lp, _, _) = self.sv[1].tick(band);
            out += lp * self.e[E_AUX] * self.p[0] * 1.4;
        }
        out
    }

    fn tom(&mut self) -> f32 {
        let (tone, snap) = (self.tone, self.snap);
        let f = self.f0 * semis(self.p[0] * self.e[E_PITCH]);
        let click = self.white() * self.e[E_CLICK] * snap * 0.5;
        match self.model {
            0 => (TAU * self.adv(0, f)).sin() * self.e[E_AMP] + click,
            1 => {
                let x = strike(self.age);
                let m = self.modes[0].tick(x) + self.modes[1].tick(x);
                (TAU * self.adv(0, f)).sin() * self.e[E_AMP] * 0.8 + m * 0.5 + click
            }
            _ => {
                let a = tri(self.adv(0, f));
                let b = (TAU * self.adv(1, f * 2.0)).sin() * tone * 0.5;
                soft((a * 0.8 + b) * 1.2) * self.e[E_AMP] + click
            }
        }
    }

    fn perc(&mut self) -> f32 {
        let (tone, snap) = (self.tone, self.snap);
        match self.model {
            0 => {
                let f = self.f0 * semis(2.0 * self.e[E_PITCH]);
                let m = (TAU * self.adv(1, f * self.p[0])).sin();
                (TAU * self.adv(0, f) + self.p[1] * self.e[E_AUX] * m).sin() * self.e[E_AMP]
            }
            1 => {
                let x = strike(self.age) + self.white() * self.e[E_CLICK] * snap * 0.8;
                let mut sum = 0.0;
                for m in &mut self.modes[..3] {
                    sum += m.tick(x);
                }
                sum * 0.6
            }
            _ => {
                let k = self.f0 / 440.0;
                let a = sq(self.adv(0, 540.0 * k));
                let b = sq(self.adv(1, 800.0 * k));
                let (_, bp, _) = self.sv[0].tick((a + b) * (0.6 + 0.4 * snap));
                let env = self.e[E_CLICK] * 0.6 + self.e[E_AMP] * (0.3 + 0.2 * tone);
                bp * self.sv[0].k * 1.2 * env
            }
        }
    }

    fn cymbal(&mut self) -> f32 {
        let (tone, snap) = (self.tone, self.snap);
        let mut sum = 0.0;
        for (i, f) in CYM_SET.iter().enumerate() {
            let hz = f * self.f0;
            sum += sq(self.adv(i, hz));
        }
        let metal = sum / 6.0 * 2.0;
        let n = self.white();
        match self.model {
            1 => {
                let (_, _, hp) = self.sv[0].tick(metal);
                let (_, _, hn) = self.sv[1].tick(n);
                let bell = (TAU * self.adv(6, 3100.0 * self.f0)).sin() * 0.5 + (TAU * self.adv(7, 4720.0 * self.f0)).sin() * 0.3;
                hp * self.e[E_AMP] * (0.9 - 0.4 * tone) + bell * self.e[E_AUX] * (0.2 + 0.8 * tone) + hn * self.e[E_NOISE] * (0.04 + 0.3 * snap)
            }
            _ => {
                let atk = (self.age as f32 / (0.0015 * self.sr)).min(1.0);
                let (_, _, hp) = self.sv[0].tick(metal);
                let (_, _, hn) = self.sv[1].tick(n);
                (hp * self.e[E_AMP] * (1.0 - 0.5 * snap) + hn * self.e[E_NOISE] * (0.15 + 0.7 * snap)) * atk
            }
        }
    }

    fn fx(&mut self) -> f32 {
        let tone = self.tone;
        match self.model {
            0 => {
                // A zap: the pitch falls from the top of the range onto the floor.
                let f = self.f0 + (self.p[0] - self.f0).max(0.0) * self.e[E_PITCH];
                let ph = self.adv(0, f);
                let y = (TAU * ph + self.p[1] * self.ph[1]).sin();
                self.ph[1] = y;
                y * self.e[E_AMP]
            }
            1 => {
                let dur = self.min_len as f32;
                let u = (self.age as f32 / dur).min(1.0);
                if self.age & 7 == 0 {
                    let fc = self.f0 * semis(12.0 * self.p[0] * u);
                    self.sv[0].set(fc, self.p[1], self.sr);
                }
                let n = self.white();
                let (_, bp, _) = self.sv[0].tick(n);
                let env = (PI * u).sin();
                bp * self.sv[0].k * env * (3.0 + 1.8 * tone)
            }
            _ => {
                while self.next < self.n_sched && self.age >= self.sched[self.next] {
                    self.e[E_AUX] = 1.0;
                    self.p[2] = self.ph[4 + self.next];
                    self.next += 1;
                }
                let f = self.f0 * semis(self.p[2]);
                let m = (TAU * self.adv(1, f * 2.0)).sin();
                (TAU * self.adv(0, f) + self.p[0] * m).sin() * self.e[E_AUX]
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn default_settings(kind: Kind, model: usize) -> Settings {
        let dec = [450.0, 220.0, 90.0, 320.0, 300.0, 420.0, 220.0, 1800.0, 700.0][kind.index()];
        Settings { model, tune: 0.0, decay_ms: dec, tone: 0.5, snap: 0.5 }
    }

    pub(crate) fn render(kind: Kind, s: &Settings, hit: Hit, vel: f32, n: usize) -> Vec<f32> {
        let mut v = Voice::new(1);
        v.set_sample_rate(48_000.0);
        v.trigger(kind, s, hit, vel, 60);
        (0..n).map(|_| v.tick()).collect()
    }

    const HIT: Hit = Hit { tune_st: 0.0, decay_mul: 1.0 };

    #[test]
    fn every_model_is_audible_finite_and_trimmed() {
        for kind in Kind::ALL {
            for model in 0..kind.models().len() {
                let out = render(kind, &default_settings(kind, model), HIT, 1.0, 48_000 * 3);
                let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                assert!(out.iter().all(|v| v.is_finite()), "{} {}", kind.label(), kind.models()[model]);
                assert!((0.6..=1.0).contains(&peak), "{} {} peaks at {peak}", kind.label(), kind.models()[model]);
            }
        }
    }

    #[test]
    fn every_model_ends_by_itself() {
        for kind in Kind::ALL {
            for model in 0..kind.models().len() {
                let mut v = Voice::new(3);
                v.set_sample_rate(48_000.0);
                v.trigger(kind, &default_settings(kind, model), HIT, 1.0, 60);
                for _ in 0..48_000 * 20 {
                    v.tick();
                }
                assert!(!v.active(), "{} {} never ends", kind.label(), kind.models()[model]);
            }
        }
    }

    #[test]
    #[ignore]
    fn print_peaks() {
        for kind in Kind::ALL {
            for model in 0..kind.models().len() {
                let out = render(kind, &default_settings(kind, model), HIT, 1.0, 48_000 * 3);
                let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                let rms = (out.iter().map(|v| v * v).sum::<f32>() / out.len() as f32).sqrt();
                let len = out.iter().rposition(|v| v.abs() > 1e-3).unwrap_or(0);
                println!("{:8} {:8} peak {:.3} rms {:.4} audible {} ms", kind.label(), kind.models()[model], peak, rms, len / 48);
            }
        }
    }
}
