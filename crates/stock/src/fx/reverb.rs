//! `fx.reverb` — algorithmic reverb on two tank engines, voiced as a set
//! of named algorithms and vintage colours:
//!
//! * **FDN** (halls, rooms, chamber, ambience, shimmer, cloud): feedback
//!   delay network of 8 or 16 lines mixed by an orthonormal fast
//!   Walsh–Hadamard transform (Jot & Chaigne 1991). Each line has slowly
//!   modulated reads and an absorption filter (gain × low shelf × high
//!   shelf on bilinear one-poles) designed from its own loop length, so the
//!   low / mid / high decay times are exactly the requested RT60s. The tank
//!   input is normalised by the loop energy so longer decays ring longer,
//!   not louder. Input diffusion all-passes and an early-reflection pattern
//!   (taps on the pre-delay line) sit in front.
//! * **Plate**: the figure-eight tank of J. Dattorro, *Effect Design Part 1*
//!   (JAES 1997): four input diffusers, two cross-coupled halves with a
//!   modulated all-pass, delays, damping and a second all-pass each, and the
//!   published output taps. Its decay coefficient is solved from the tank's
//!   loop length for the requested RT60.
//!
//! The Algorithm picks a [`voicing`] (line lengths, early pattern, diffusion,
//! modulation); "Classic" keeps the original Hall / Room / Plate of Mode.
//! The Colour band-limits, thins and quantises the tank like 70s / 80s
//! digital units. Shimmer feeds the late field back through a
//! [`shimmer`] pitch shifter; Ducking lowers the wet while the input plays.
//!
//! All buffers are sized in `prepare` for the largest size; parameter edits
//! only move targets (line lengths glide). Zero latency.
//!
//! Meter layout: `[wet peak L, wet peak R, mid-band RT60 (s), duck gain]`.

pub mod shimmer;
pub mod voicing;

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::{AllpassDiffuser, AllpassReader, DelayLine};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::{OnePole, SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower};
use shimmer::{INTERVAL_NAMES, INTERVALS, PitchShifter};
use std::f32::consts::TAU;
use voicing::{ALGORITHMS, COLOR_TABLE, COLORS, Engine, VOICES, quantise, voice_index};

pub const ID: &str = "fx.reverb";

pub mod p {
    pub const MODE: u32 = 0;
    pub const LINES: u32 = 1;
    pub const DECAY: u32 = 2;
    pub const PREDELAY: u32 = 3;
    pub const SIZE: u32 = 4;
    pub const DAMPING: u32 = 5;
    pub const BASS_MULT: u32 = 6;
    pub const BASS_FREQ: u32 = 7;
    pub const DAMP_FREQ: u32 = 8;
    pub const DIFFUSION: u32 = 9;
    pub const MOD_DEPTH: u32 = 10;
    pub const MOD_RATE: u32 = 11;
    pub const EARLY: u32 = 12;
    pub const WIDTH: u32 = 13;
    pub const LOW_CUT: u32 = 14;
    pub const HIGH_CUT: u32 = 15;
    pub const MIX: u32 = 16;
    pub const FREEZE: u32 = 17;
    pub const OUTPUT: u32 = 18;
    pub const ALGORITHM: u32 = 19;
    pub const COLOR: u32 = 20;
    pub const SHIMMER: u32 = 21;
    pub const SHIFT: u32 = 22;
    pub const DUCK: u32 = 23;
}

const MODES: &[&str] = &["Hall", "Room", "Plate"];
const LINE_CHOICES: &[&str] = &["8", "16"];
const OFF_ON: &[&str] = &["Off", "On"];

const MAX_LINES: usize = 16;
const MAX_PREDELAY_MS: f32 = 500.0;
const MAX_EARLY_MS: f32 = 100.0;
const MAX_LINE_MS: f32 = 200.0;
const MAX_MOD_MS: f32 = 1.0;
const DIFFUSERS: usize = 4;
const EARLY_TAPS: usize = 8;
const FREEZE_GAIN: f32 = 0.99995;
/// Overall wet trim so a steady input plays back at about its own level.
const FDN_WET: f32 = 2.1;
const PLATE_WET: f32 = 0.6;
/// Shimmer loop gain at 100 % (the tank's own gain is about one).
const SHIMMER_MAX: f32 = 0.72;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 2.0, 0.0).choices(MODES),
        ParamInfo::new(p::LINES, "Density", 0.0, 1.0, 1.0).choices(LINE_CHOICES).short("Lines"),
        ParamInfo::new(p::DECAY, "Decay", 0.1, 20.0, 2.0).unit("s").log(),
        ParamInfo::new(p::PREDELAY, "Pre-Delay", 0.0, MAX_PREDELAY_MS, 10.0).unit("ms").power(2.0).short("Pre"),
        ParamInfo::new(p::SIZE, "Size", 0.0, 100.0, 50.0).unit("%"),
        ParamInfo::new(p::DAMPING, "Damping", 0.0, 100.0, 50.0).unit("%").short("Damp"),
        ParamInfo::new(p::BASS_MULT, "Bass Decay", 0.25, 4.0, 1.0).unit("x").log().short("Bass"),
        ParamInfo::new(p::BASS_FREQ, "Bass Freq", 50.0, 1000.0, 250.0).unit("Hz").log().short("B.Freq"),
        ParamInfo::new(p::DAMP_FREQ, "Damp Freq", 1000.0, 16_000.0, 5000.0).unit("Hz").log().short("D.Freq"),
        ParamInfo::new(p::DIFFUSION, "Diffusion", 0.0, 100.0, 70.0).unit("%").short("Diff"),
        ParamInfo::new(p::MOD_DEPTH, "Modulation", 0.0, 100.0, 30.0).unit("%").short("Mod"),
        ParamInfo::new(p::MOD_RATE, "Mod Rate", 0.05, 5.0, 0.6).unit("Hz").log().short("Rate"),
        ParamInfo::new(p::EARLY, "Early Reflections", 0.0, 100.0, 50.0).unit("%").short("Early"),
        ParamInfo::new(p::WIDTH, "Width", 0.0, 200.0, 100.0).unit("%"),
        ParamInfo::new(p::LOW_CUT, "Low Cut", 20.0, 1000.0, 20.0).unit("Hz").log().short("LoCut"),
        ParamInfo::new(p::HIGH_CUT, "High Cut", 1000.0, 20_000.0, 18_000.0).unit("Hz").log().short("HiCut"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 25.0).unit("%"),
        ParamInfo::new(p::FREEZE, "Freeze", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::ALGORITHM, "Algorithm", 0.0, (ALGORITHMS.len() - 1) as f32, 0.0).choices(ALGORITHMS).short("Algo"),
        ParamInfo::new(p::COLOR, "Colour", 0.0, (COLORS.len() - 1) as f32, 0.0).choices(COLORS).short("Colour"),
        ParamInfo::new(p::SHIMMER, "Shimmer", 0.0, 100.0, 0.0).unit("%").short("Shim"),
        ParamInfo::new(p::SHIFT, "Shimmer Pitch", 0.0, (INTERVAL_NAMES.len() - 1) as f32, 6.0).choices(INTERVAL_NAMES).short("Pitch"),
        ParamInfo::new(p::DUCK, "Ducking", 0.0, 100.0, 0.0).unit("%").short("Duck"),
    ]
}

// ── FDN tables ───────────────────────────────────────────────────────────

/// Offsets of each rank inside the length spread (keeps ratios irregular).
const SPREAD_JITTER: [f32; MAX_LINES] = [0.0, 0.27, -0.19, 0.33, -0.12, 0.24, -0.31, 0.16, -0.23, 0.29, -0.08, 0.21, -0.27, 0.11, -0.17, 0.0];
/// Which line takes the n-th shortest length (neighbours land far apart in
/// the butterflies).
const ORDER_16: [usize; 16] = [0, 11, 6, 13, 3, 8, 15, 4, 10, 1, 14, 7, 2, 9, 12, 5];
const ORDER_8: [usize; 8] = [0, 5, 3, 6, 1, 7, 2, 4];
const SIGN_IN_L: [f32; MAX_LINES] = [1., -1., 1., 1., -1., 1., -1., -1., 1., 1., -1., 1., -1., 1., 1., -1.];
const SIGN_IN_R: [f32; MAX_LINES] = [1., 1., -1., 1., 1., -1., -1., 1., -1., 1., 1., -1., -1., -1., 1., 1.];
const SIGN_OUT_L: [f32; MAX_LINES] = [1., 1., -1., -1., 1., -1., 1., 1., -1., 1., -1., -1., 1., 1., -1., 1.];
const SIGN_OUT_R: [f32; MAX_LINES] = [-1., 1., 1., -1., 1., 1., -1., 1., 1., -1., -1., 1., 1., -1., 1., -1.];
const MOD_SPREAD: [f32; MAX_LINES] = [1.0, 0.81, 1.19, 0.73, 1.27, 0.93, 1.09, 0.77, 1.35, 0.87, 1.13, 0.71, 1.23, 0.97, 1.31, 0.83];
const DIFF_MS_L: [f32; DIFFUSERS] = [1.7, 2.9, 4.3, 6.1];
const DIFF_MS_R: [f32; DIFFUSERS] = [1.9, 3.3, 4.7, 6.7];
const EARLY_AT_L: [f32; EARLY_TAPS] = [0.05, 0.11, 0.19, 0.28, 0.39, 0.53, 0.71, 0.93];
const EARLY_AT_R: [f32; EARLY_TAPS] = [0.07, 0.14, 0.22, 0.33, 0.45, 0.6, 0.79, 0.98];
const EARLY_SIGN: [f32; EARLY_TAPS] = [1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0];

/// In-place orthonormal fast Walsh–Hadamard transform of `v[..n]`.
#[inline]
fn fwht(v: &mut [f32; MAX_LINES], n: usize) {
    let mut h = 1;
    while h < n {
        let mut i = 0;
        while i < n {
            for j in i..i + h {
                let (a, b) = (v[j], v[j + h]);
                v[j] = a + b;
                v[j + h] = a - b;
            }
            i += 2 * h;
        }
        h *= 2;
    }
    let s = 1.0 / (n as f32).sqrt();
    for x in v.iter_mut().take(n) {
        *x *= s;
    }
}

fn sweep(min: f32, max: f32, t: f32) -> f32 {
    min * (max / min).powf(t.clamp(0.0, 1.0))
}

/// Per-pass loss of one line: middle gain and the DC / Nyquist ratios.
#[derive(Clone, Copy, Debug, Default)]
struct Absorption {
    gain: f32,
    low: f32,
    high: f32,
}

impl Absorption {
    fn design(loop_samples: f32, sr: f32, rt: [f32; 3]) -> Self {
        let g = |t: f32| 10f32.powf(-3.0 * loop_samples / (sr * t.max(0.01)));
        let (gl, gm, gh) = (g(rt[0]), g(rt[1]), g(rt[2]));
        Self { gain: gm, low: gl / gm, high: gh / gm }
    }
}

#[derive(Clone)]
struct Line {
    buf: DelayLine,
    reader: AllpassReader,
    len: f32,
    target: f32,
    loss: Absorption,
    low: OnePole,
    high: OnePole,
    rot: (f32, f32),
    step: (f32, f32),
}

impl Line {
    #[inline]
    fn absorb(&mut self, x: f32) -> f32 {
        let a = self.loss;
        // Low shelf: DC → `low`, high frequencies → 1.
        let shelved = x + (a.low - 1.0) * self.low.lowpass(x);
        // High shelf: DC → 1, Nyquist → `high`.
        let lp = self.high.lowpass(shelved);
        a.gain * (a.high * shelved + (1.0 - a.high) * lp)
    }
}

struct Fdn {
    lines: Vec<Line>,
    n: usize,
    diff_l: Vec<AllpassDiffuser>,
    diff_r: Vec<AllpassDiffuser>,
    early_gain_l: [f32; EARLY_TAPS],
    early_gain_r: [f32; EARLY_TAPS],
    late_norm: f32,
    mod_depth: f32,
    glide: f32,
    renorm: u32,
}

impl Fdn {
    fn new(sr: f64) -> Self {
        let max = ((MAX_LINE_MS + 4.0 * MAX_MOD_MS) * 0.001 * sr as f32) as usize + 8;
        let lines = (0..MAX_LINES)
            .map(|_| Line {
                buf: DelayLine::new(max),
                reader: AllpassReader::default(),
                len: 1000.0,
                target: 1000.0,
                loss: Absorption { gain: 0.5, low: 1.0, high: 1.0 },
                low: OnePole::default(),
                high: OnePole::default(),
                rot: (1.0, 0.0),
                step: (1.0, 0.0),
            })
            .collect();
        let diff = |ms: &[f32; DIFFUSERS]| ms.iter().map(|m| AllpassDiffuser::new((m * 1.6 * 0.001 * sr as f32) as usize + 2)).collect();
        let early = |at: &[f32; EARLY_TAPS]| {
            let raw: [f32; EARLY_TAPS] = std::array::from_fn(|i| (1.0 - 0.7 * at[i]) * EARLY_SIGN[i]);
            let norm = raw.iter().map(|g| g * g).sum::<f32>().sqrt();
            raw.map(|g| g / norm)
        };
        Self {
            lines,
            n: 16,
            diff_l: diff(&DIFF_MS_L),
            diff_r: diff(&DIFF_MS_R),
            early_gain_l: early(&EARLY_AT_L),
            early_gain_r: early(&EARLY_AT_R),
            late_norm: 0.5,
            mod_depth: 0.0,
            glide: 0.001,
            renorm: 0,
        }
    }

    fn clear(&mut self) {
        for l in &mut self.lines {
            l.buf.clear();
            l.reader.reset();
            l.low.reset();
            l.high.reset();
            l.rot = (1.0, 0.0);
        }
        for d in self.diff_l.iter_mut().chain(self.diff_r.iter_mut()) {
            d.clear();
        }
    }

    /// One frame: diffused stereo input in, late stereo out.
    #[inline]
    fn tick(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        let n = self.n;
        let mut d = [0.0f32; MAX_LINES];
        let mut y = [0.0f32; MAX_LINES];
        let depth = self.mod_depth;
        let glide = self.glide;
        for i in 0..n {
            let line = &mut self.lines[i];
            line.len += (line.target - line.len) * glide;
            // Rotate the LFO phasor.
            let (c, s) = line.rot;
            let (sc, ss) = line.step;
            line.rot = (c * sc - s * ss, c * ss + s * sc);
            let delay = line.len + depth * (0.5 + 0.5 * line.rot.1);
            // All-pass interpolation: flat magnitude, so the fractional and
            // modulated reads add no extra high-frequency loss to the loop.
            d[i] = line.reader.read(&line.buf, delay - 1.0);
            y[i] = line.absorb(d[i]);
        }
        fwht(&mut y, n);
        let norm = self.late_norm;
        let mut out_l = 0.0;
        let mut out_r = 0.0;
        for i in 0..n {
            let inj = (SIGN_IN_L[i] * in_l + SIGN_IN_R[i] * in_r) * norm;
            let v = y[i] + inj;
            self.lines[i].buf.push(if v.abs() < 1e-25 { 0.0 } else { v });
            out_l += SIGN_OUT_L[i] * d[i];
            out_r += SIGN_OUT_R[i] * d[i];
        }
        self.renorm += 1;
        if self.renorm >= 4096 {
            self.renorm = 0;
            for l in &mut self.lines[..n] {
                let (c, s) = l.rot;
                let m = (c * c + s * s).sqrt().max(1e-6);
                l.rot = (c / m, s / m);
            }
        }
        let s = 1.0 / (n as f32).sqrt();
        (out_l * s, out_r * s)
    }
}

// ── Plate (Dattorro) ─────────────────────────────────────────────────────

/// Dattorro's tank lengths at his 29 761 Hz reference rate.
const PLATE_SR: f32 = 29_761.0;
const PLATE_IN: [f32; 4] = [142.0, 107.0, 379.0, 277.0];
const PLATE_IN_G: [f32; 4] = [0.75, 0.75, 0.625, 0.625];
const PLATE_AP1: f32 = 672.0;
const PLATE_A: f32 = 4453.0;
const PLATE_AP2: f32 = 1800.0;
const PLATE_B: f32 = 3720.0;
const PLATE_AP3: f32 = 908.0;
const PLATE_C: f32 = 4217.0;
const PLATE_AP4: f32 = 2656.0;
const PLATE_D: f32 = 3163.0;
const PLATE_EXCURSION: f32 = 16.0;
const PLATE_MAX_SCALE: f32 = 1.5;

struct Plate {
    input: Vec<AllpassDiffuser>,
    ap1: AllpassDiffuser,
    ap2: AllpassDiffuser,
    ap3: AllpassDiffuser,
    ap4: AllpassDiffuser,
    a: DelayLine,
    b: DelayLine,
    c: DelayLine,
    d: DelayLine,
    len: [usize; 8],
    scale: f32,
    damp_l: f32,
    damp_r: f32,
    b_out: f32,
    d_out: f32,
    decay: f32,
    damp: f32,
    in_g: f32,
    exc: f32,
    phase: f32,
    inc: f32,
}

impl Plate {
    fn new(sr: f64) -> Self {
        let k = sr as f32 / PLATE_SR * PLATE_MAX_SCALE;
        let n = |l: f32| (l * k) as usize + 64;
        Self {
            input: PLATE_IN.iter().map(|&l| AllpassDiffuser::new(n(l))).collect(),
            ap1: AllpassDiffuser::new(n(PLATE_AP1 + 8.0 * PLATE_EXCURSION)),
            ap2: AllpassDiffuser::new(n(PLATE_AP2)),
            ap3: AllpassDiffuser::new(n(PLATE_AP3 + 8.0 * PLATE_EXCURSION)),
            ap4: AllpassDiffuser::new(n(PLATE_AP4)),
            a: DelayLine::new(n(PLATE_A)),
            b: DelayLine::new(n(PLATE_B)),
            c: DelayLine::new(n(PLATE_C)),
            d: DelayLine::new(n(PLATE_D)),
            len: [0; 8],
            scale: 1.0,
            damp_l: 0.0,
            damp_r: 0.0,
            b_out: 0.0,
            d_out: 0.0,
            decay: 0.5,
            damp: 0.3,
            in_g: 0.7,
            exc: 0.0,
            phase: 0.0,
            inc: 0.0,
        }
    }

    fn clear(&mut self) {
        for d in self.input.iter_mut().chain([&mut self.ap1, &mut self.ap2, &mut self.ap3, &mut self.ap4]) {
            d.clear();
        }
        for l in [&mut self.a, &mut self.b, &mut self.c, &mut self.d] {
            l.clear();
        }
        self.damp_l = 0.0;
        self.damp_r = 0.0;
        self.b_out = 0.0;
        self.d_out = 0.0;
    }

    /// Lengths in samples for a size scale; returns the figure-eight loop
    /// length (all eight elements).
    fn set_scale(&mut self, sr: f32, size_scale: f32) -> f32 {
        let k = sr / PLATE_SR * size_scale;
        self.scale = k;
        let ls = [PLATE_AP1, PLATE_A, PLATE_AP2, PLATE_B, PLATE_AP3, PLATE_C, PLATE_AP4, PLATE_D];
        for (i, l) in ls.iter().enumerate() {
            self.len[i] = ((l * k).round() as usize).max(2);
        }
        self.ap2.set_delay(self.len[2]);
        self.ap4.set_delay(self.len[6]);
        for (d, &l) in self.input.iter_mut().zip(&PLATE_IN) {
            d.set_delay(((l * k).round() as usize).max(1));
        }
        self.len.iter().sum::<usize>() as f32
    }

    #[inline]
    fn tap(&self, line: &DelayLine, at: f32) -> f32 {
        line.read(((at * self.scale) as usize).min(line.max_delay()))
    }

    #[inline]
    fn tick(&mut self, x: f32) -> (f32, f32) {
        let mut v = x;
        for (d, g) in self.input.iter_mut().zip(PLATE_IN_G) {
            v = d.process(v, g * self.in_g);
        }
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        let (ms, mc) = (TAU * self.phase).sin_cos();
        let decay = self.decay;
        let damp = self.damp;
        // Left half.
        let l_in = v + decay * self.d_out;
        let a = self.ap1.process_mod(l_in, -0.7, self.len[0] as f32 + self.exc * (1.0 + ms));
        self.a.push(a);
        let a_out = self.a.read(self.len[1]);
        self.damp_l = a_out * (1.0 - damp) + self.damp_l * damp;
        let b = self.ap2.process(self.damp_l * decay, 0.5);
        self.b.push(b);
        let b_out = self.b.read(self.len[3]);
        // Right half.
        let r_in = v + decay * self.b_out;
        let c = self.ap3.process_mod(r_in, -0.7, self.len[4] as f32 + self.exc * (1.0 + mc));
        self.c.push(c);
        let c_out = self.c.read(self.len[5]);
        self.damp_r = c_out * (1.0 - damp) + self.damp_r * damp;
        let d = self.ap4.process(self.damp_r * decay, 0.5);
        self.d.push(d);
        let d_out = self.d.read(self.len[7]);
        self.b_out = if b_out.abs() < 1e-25 { 0.0 } else { b_out };
        self.d_out = if d_out.abs() < 1e-25 { 0.0 } else { d_out };
        if self.damp_l.abs() < 1e-25 {
            self.damp_l = 0.0;
        }
        if self.damp_r.abs() < 1e-25 {
            self.damp_r = 0.0;
        }
        // Dattorro's output taps.
        let k = self.scale;
        let ap_tap = |ap: &AllpassDiffuser, at: f32| ap.tap((at * k) as usize);
        let yl = self.tap(&self.c, 266.0) + self.tap(&self.c, 2974.0) - ap_tap(&self.ap4, 1913.0) + self.tap(&self.d, 1996.0)
            - self.tap(&self.a, 1990.0)
            - ap_tap(&self.ap2, 187.0)
            - self.tap(&self.b, 1066.0);
        let yr = self.tap(&self.a, 353.0) + self.tap(&self.a, 3627.0) - ap_tap(&self.ap2, 1228.0) + self.tap(&self.b, 2673.0)
            - self.tap(&self.c, 2111.0)
            - ap_tap(&self.ap4, 335.0)
            - self.tap(&self.d, 121.0);
        (yl, yr)
    }
}

// ── Plugin ───────────────────────────────────────────────────────────────

/// Smooth saturation for the shimmer return (rational tanh, |y| ≤ 1).
#[inline]
fn soft(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

pub struct Reverb {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    fdn: Fdn,
    plate: Plate,
    pre: [DelayLine; 2],
    predelay: Smoother,
    early_span: Smoother,
    input_gain: Smoother,
    early_gain: Smoother,
    wet: Smoother,
    dry: Smoother,
    width: Smoother,
    shimmer_gain: Smoother,
    hp: SvfCoeffs,
    lp: SvfCoeffs,
    hp_st: [SvfState; 2],
    lp_st: [SvfState; 2],
    shifter: PitchShifter,
    shim_hp: OnePole,
    shim_src: f32,
    /// Colour band limit: coefficients, tank-input and output states.
    color_lp: SvfCoeffs,
    color_in: [SvfState; 2],
    color_out: [SvfState; 2],
    color_on: bool,
    /// Wet word scale (`2^(bits−1)`), 0 = float.
    quant: f32,
    late_trim: f32,
    duck_env: EnvelopeFollower,
    duck_amount: f32,
    duck_gain: f32,
    diffusion: f32,
    /// Engine the frame loop runs: 2 = plate, otherwise the FDN.
    mode: usize,
    /// Voicing × colour the tank was last cleared for.
    voice: usize,
    rt: [f32; 3],
    peak: [f32; 2],
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Reverb::new())
}

impl Default for Reverb {
    fn default() -> Self {
        Self::new()
    }
}

impl Reverb {
    pub fn new() -> Self {
        let params = params();
        let sr = 48_000.0;
        let mut r = Self {
            info: PluginInfo {
                id: ID,
                name: "Reverb",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["reverb", "space", "plate", "hall"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 0.0,
            fdn: Fdn::new(sr),
            plate: Plate::new(sr),
            pre: [DelayLine::new(16), DelayLine::new(16)],
            predelay: Smoother::new(0.0),
            early_span: Smoother::new(0.0),
            input_gain: Smoother::new(1.0),
            early_gain: Smoother::new(0.5),
            wet: Smoother::new(0.25),
            dry: Smoother::new(1.0),
            width: Smoother::new(1.0),
            shimmer_gain: Smoother::new(0.0),
            hp: SvfCoeffs::default(),
            lp: SvfCoeffs::default(),
            hp_st: [SvfState::default(); 2],
            lp_st: [SvfState::default(); 2],
            shifter: PitchShifter::new(sr),
            shim_hp: OnePole::default(),
            shim_src: 0.0,
            color_lp: SvfCoeffs::default(),
            color_in: [SvfState::default(); 2],
            color_out: [SvfState::default(); 2],
            color_on: false,
            quant: 0.0,
            late_trim: 1.0,
            duck_env: EnvelopeFollower::new(DetectorMode::Peak),
            duck_amount: 0.0,
            duck_gain: 1.0,
            diffusion: 0.5,
            mode: 0,
            voice: usize::MAX,
            rt: [2.0; 3],
            peak: [0.0; 2],
            first: true,
        };
        r.prepare(sr, 512);
        r
    }

    /// The voicing and colour the current Algorithm / Mode / Colour select.
    pub fn voicing(&self) -> (voicing::Voicing, voicing::Color) {
        let algo = self.params.choice(p::ALGORITHM).min(ALGORITHMS.len() - 1);
        let color = self.params.choice(p::COLOR).min(COLORS.len() - 1);
        (VOICES[voice_index(algo, self.params.choice(p::MODE))], COLOR_TABLE[color])
    }

    /// RT60s `[low, mid, high]` in seconds for the current params.
    pub fn decay_times(&self) -> [f32; 3] {
        let mid = self.params.plain(p::DECAY);
        let low = (mid * self.params.plain(p::BASS_MULT)).clamp(0.05, 80.0);
        let damping = (self.params.plain(p::DAMPING) / 100.0 * self.voicing().0.damping).min(1.2);
        let ratio = 0.1f32.powf(damping);
        [low, mid, (mid * ratio).max(0.05)]
    }

    /// Loop gain of the shimmer feedback for the current params.
    pub fn shimmer_amount(&self) -> f32 {
        let floor = self.voicing().0.shimmer_floor;
        (floor + (1.0 - floor) * self.params.plain(p::SHIMMER) / 100.0) * SHIMMER_MAX
    }

    fn update(&mut self) {
        let sr = self.sr as f32;
        let (v, c) = self.voicing();
        let algo = self.params.choice(p::ALGORITHM).min(ALGORITHMS.len() - 1);
        let key = voice_index(algo, self.params.choice(p::MODE)) * COLORS.len() + self.params.choice(p::COLOR).min(COLORS.len() - 1);
        if key != self.voice {
            if self.voice != usize::MAX {
                self.fdn.clear();
                self.plate.clear();
                self.shifter.clear();
                self.shim_src = 0.0;
            }
            self.voice = key;
        }
        self.mode = if v.engine == Engine::Plate { 2 } else { 0 };
        let size = self.params.plain(p::SIZE) / 100.0;
        let freeze = self.params.on(p::FREEZE);
        self.rt = self.decay_times();
        let ms = |m: f32| m * 0.001 * sr;
        let mod_amt = self.params.plain(p::MOD_DEPTH) / 100.0;
        let mod_scale = v.mod_scale * c.mod_scale;
        let rate = self.params.plain(p::MOD_RATE);
        self.diffusion = (self.params.plain(p::DIFFUSION) / 100.0 * v.diffusion * c.diffusion).min(1.07);

        // FDN.
        let n = c.lines.or(v.lines).unwrap_or(if self.params.choice(p::LINES) == 0 { 8 } else { 16 });
        let longest = sweep(v.longest.0, v.longest.1, size);
        let early_span = sweep(v.early_span.0, v.early_span.1, size);
        let shortest = longest * v.spread;
        let fdn = &mut self.fdn;
        if fdn.n != n {
            fdn.n = n;
            fdn.clear();
        }
        fdn.mod_depth = ms(mod_amt * MAX_MOD_MS * mod_scale);
        let order: &[usize] = if n == 16 { &ORDER_16 } else { &ORDER_8 };
        let mut kept = 0.0;
        for rank in 0..n {
            let jitter = SPREAD_JITTER[rank * MAX_LINES / n];
            let at = ((rank as f32 + jitter) / (n - 1) as f32).clamp(0.0, 1.0);
            let line = &mut fdn.lines[order[rank]];
            line.target = ms(sweep(shortest, longest, at)).max(8.0);
            let loop_len = line.target + 0.5 * fdn.mod_depth;
            let loss = Absorption::design(loop_len, sr, self.rt);
            kept += loss.gain * loss.gain;
            line.loss = if freeze { Absorption { gain: FREEZE_GAIN, low: 1.0, high: 1.0 } } else { loss };
            line.low.set_freq(self.params.plain(p::BASS_FREQ), self.sr);
            line.high.set_freq(self.params.plain(p::DAMP_FREQ), self.sr);
            let w = TAU * rate * MOD_SPREAD[rank] / sr;
            line.step = (w.cos(), w.sin());
        }
        // A loop that keeps g² of its energy per pass accumulates
        // 1 / (1 − g²); take that back so the tail level is decay-independent.
        fdn.late_norm = (1.0 - kept / n as f32).max(1e-5).sqrt() / (n as f32).sqrt() * 2.0;
        let dscale = sweep(0.5, 1.6, size);
        for (d, m) in fdn.diff_l.iter_mut().zip(&DIFF_MS_L) {
            d.set_delay((ms(m * dscale) as usize).max(1));
        }
        for (d, m) in fdn.diff_r.iter_mut().zip(&DIFF_MS_R) {
            d.set_delay((ms(m * dscale) as usize).max(1));
        }

        // Plate.
        let loop_len = self.plate.set_scale(sr, 0.5 + size);
        // Four decay multiplications per trip round the figure eight.
        self.plate.decay = if freeze { 0.99999 } else { 10f32.powf(-3.0 * loop_len / (4.0 * sr * self.rt[1])).min(0.9995) };
        let damping = (self.params.plain(p::DAMPING) / 100.0 * v.damping).min(1.0);
        let damp_hz = (self.params.plain(p::DAMP_FREQ) * 2f32.powf(2.0 * (0.5 - damping))).min(sr * 0.45);
        self.plate.damp = (-TAU * damp_hz / sr).exp();
        self.plate.in_g = 0.4 + 0.6 * self.diffusion.min(1.0);
        self.plate.exc = PLATE_EXCURSION * sr / PLATE_SR * (mod_amt / 0.3).min(3.0) * mod_scale.min(1.5);
        self.plate.inc = rate / sr;

        // Colour.
        self.color_on = c.bandwidth.is_some();
        self.color_lp = SvfCoeffs::new(SvfMode::Lowpass, c.bandwidth.unwrap_or(20_000.0).min(sr * 0.45), 0.707, 0.0, self.sr);
        self.quant = c.bits.map_or(0.0, |b| 2f32.powi(b as i32 - 1));
        self.late_trim = v.late;

        // Shimmer and ducking.
        self.shifter.set_semitones(INTERVALS[self.params.choice(p::SHIFT).min(INTERVALS.len() - 1)]);
        self.shim_hp.set_freq(250.0, self.sr);
        self.shimmer_gain.set_target(self.shimmer_amount());
        self.duck_amount = self.params.plain(p::DUCK) / 100.0;

        self.predelay.set_target(ms(self.params.plain(p::PREDELAY)));
        self.early_span.set_target(ms(early_span));
        self.input_gain.set_target(if freeze { 0.0 } else { 1.0 });
        self.early_gain.set_target(self.params.plain(p::EARLY) / 100.0 * v.early_gain);
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target((2.0 * mix).min(1.0) * out);
        self.dry.set_target((2.0 * (1.0 - mix)).min(1.0) * out);
        self.width.set_target(self.params.plain(p::WIDTH) / 100.0);
        self.hp = SvfCoeffs::new(SvfMode::Highpass, self.params.plain(p::LOW_CUT), 0.707, 0.0, self.sr);
        self.lp = SvfCoeffs::new(SvfMode::Lowpass, self.params.plain(p::HIGH_CUT), 0.707, 0.0, self.sr);
        if self.first {
            for l in &mut self.fdn.lines {
                l.len = l.target;
            }
            for s in [
                &mut self.predelay,
                &mut self.early_span,
                &mut self.input_gain,
                &mut self.early_gain,
                &mut self.wet,
                &mut self.dry,
                &mut self.width,
                &mut self.shimmer_gain,
            ] {
                s.snap_to_target();
            }
            self.first = false;
        }
    }
}

impl Segmented for Reverb {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let mode = self.mode;
        let g_diff = self.diffusion * 0.7;
        let decay = (-1.0 / (0.3 * self.sr as f32)).exp();
        let (hp, lp, clp) = (self.hp, self.lp, self.color_lp);
        let (color_on, quant, late, duck_amount) = (self.color_on, self.quant, self.late_trim, self.duck_amount);
        let max_pre = self.pre[0].max_delay() as f32 - 2.0;
        let mut peak = self.peak;
        let mut duck_gain = self.duck_gain;
        let Self {
            fdn,
            plate,
            pre,
            predelay,
            early_span,
            input_gain,
            early_gain,
            wet,
            dry,
            width,
            shimmer_gain,
            hp_st,
            lp_st,
            shifter,
            shim_hp,
            shim_src,
            color_in,
            color_out,
            duck_env,
            ..
        } = self;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let ig = input_gain.next();
            pre[0].push(l * ig);
            pre[1].push(r * ig);
            let pd = predelay.next();
            let (mut pl, mut pr) = if pd < 0.5 { (l * ig, r * ig) } else { (pre[0].read_linear(pd), pre[1].read_linear(pd)) };
            // Shimmer: last frame's late field, pitch-shifted, back in.
            let sg = shimmer_gain.next();
            if sg > 1e-4 {
                let back = soft(shifter.process(shim_hp.highpass(*shim_src)) * sg) * ig;
                pl += back;
                pr += back;
            }
            if color_on {
                pl = color_in[0].process(&clp, pl);
                pr = color_in[1].process(&clp, pr);
            }
            let (mut wl, mut wr) = if mode == 2 {
                let (a, b) = plate.tick(0.5 * (pl + pr));
                *shim_src = 0.5 * (a + b) * PLATE_WET;
                (a * PLATE_WET, b * PLATE_WET)
            } else {
                let mut dl = pl;
                let mut dr = pr;
                for d in fdn.diff_l.iter_mut() {
                    dl = d.process(dl, g_diff);
                }
                for d in fdn.diff_r.iter_mut() {
                    dr = d.process(dr, g_diff);
                }
                let (a, b) = fdn.tick(dl, dr);
                let k = FDN_WET * late;
                *shim_src = 0.5 * (a + b) * k;
                // Early reflections: taps after the pre-delay, crossing
                // sides on odd taps.
                let eg = early_gain.next();
                let span = early_span.next();
                let (mut el, mut er) = (0.0, 0.0);
                if eg > 0.0 {
                    for k in 0..EARLY_TAPS {
                        let (own_l, own_r) = if k % 2 == 0 { (&pre[0], &pre[1]) } else { (&pre[1], &pre[0]) };
                        el += own_l.read_linear((pd + EARLY_AT_L[k] * span).min(max_pre)) * fdn.early_gain_l[k];
                        er += own_r.read_linear((pd + EARLY_AT_R[k] * span).min(max_pre)) * fdn.early_gain_r[k];
                    }
                }
                (a * k + el * eg, b * k + er * eg)
            };
            if color_on {
                wl = color_out[0].process(&clp, wl);
                wr = color_out[1].process(&clp, wr);
            }
            if quant > 0.0 {
                wl = quantise(wl, quant);
                wr = quantise(wr, quant);
            }
            wl = lp_st[0].process(&lp, hp_st[0].process(&hp, wl));
            wr = lp_st[1].process(&lp, hp_st[1].process(&hp, wr));
            let w = width.next();
            let m = 0.5 * (wl + wr);
            let s = 0.5 * (wl - wr) * w;
            // Ducking: the dry input's envelope pushes the tail down.
            let env = duck_env.process(0.5 * (l + r));
            duck_gain = 1.0 - duck_amount * (env * 4.0).min(1.0);
            let g = wet.next() * duck_gain;
            wl = (m + s) * g;
            wr = (m - s) * g;
            peak[0] = (peak[0] * decay).max(wl.abs());
            peak[1] = (peak[1] * decay).max(wr.abs());
            let d = dry.next();
            (sanitize(l * d + wl), sanitize(r * d + wr))
        });
        if !self.shim_src.is_finite() {
            self.shim_src = 0.0;
        }
        self.peak = peak.map(|v| if v.is_finite() { v } else { 0.0 });
        self.duck_gain = duck_gain;
    }
}

impl Processor for Reverb {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        if self.sr != sample_rate {
            self.sr = sample_rate;
            self.fdn = Fdn::new(sample_rate);
            self.plate = Plate::new(sample_rate);
            let n = ((MAX_PREDELAY_MS + MAX_EARLY_MS + 10.0) * 0.001 * sample_rate as f32) as usize;
            self.pre = [DelayLine::new(n), DelayLine::new(n)];
            self.shifter = PitchShifter::new(sample_rate);
        }
        self.fdn.glide = 1.0 - (-1.0 / (0.08 * sample_rate as f32)).exp();
        for s in [&mut self.predelay, &mut self.early_span] {
            s.set_time(60.0, sample_rate);
        }
        for s in [&mut self.input_gain, &mut self.early_gain, &mut self.wet, &mut self.dry, &mut self.width] {
            s.set_time(20.0, sample_rate);
        }
        self.shimmer_gain.set_time(80.0, sample_rate);
        self.duck_env.set_times(5.0, 250.0, sample_rate);
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.fdn.clear();
        self.plate.clear();
        for p in &mut self.pre {
            p.clear();
        }
        self.hp_st = [SvfState::default(); 2];
        self.lp_st = [SvfState::default(); 2];
        self.color_in = [SvfState::default(); 2];
        self.color_out = [SvfState::default(); 2];
        self.shifter.clear();
        self.shim_hp.reset();
        self.shim_src = 0.0;
        self.duck_env.reset();
        self.duck_gain = 1.0;
        self.peak = [0.0; 2];
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        drive(self, ctx);
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn tail(&self) -> u32 {
        let secs = if self.params.on(p::FREEZE) {
            60.0
        } else {
            let rt = self.decay_times();
            // Each trip through the shimmer adds another tank decay.
            let shimmer = 1.0 / (1.0 - self.shimmer_amount().min(0.95));
            1.5 * rt[0].max(rt[1]).max(rt[2]) * shimmer + self.params.plain(p::PREDELAY) * 0.001 + 0.2
        };
        (secs.min(100.0) * self.sr as f32) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.peak[0], self.peak[1], self.rt[1], self.duck_gain])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, peak, process_stereo, rms, set_plain, sine};

    /// RT60 from the Schroeder energy decay curve (T20 fit, −5 → −25 dB).
    fn rt60(ir: &[f32]) -> f32 {
        let mut edc = vec![0.0f64; ir.len()];
        let mut acc = 0.0f64;
        for i in (0..ir.len()).rev() {
            acc += (ir[i] as f64).powi(2);
            edc[i] = acc;
        }
        let total = edc[0];
        let db = |i: usize| 10.0 * (edc[i] / total).log10();
        let t5 = (0..ir.len()).find(|&i| db(i) <= -5.0).unwrap();
        let t25 = (0..ir.len()).find(|&i| db(i) <= -25.0).unwrap();
        (3.0 * (t25 - t5) as f64 / SR) as f32
    }

    fn impulse_response(r: &mut Reverb, secs: f32) -> Vec<f32> {
        let n = (secs * SR as f32) as usize;
        let (l, rr) = process_stereo(r, |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, n);
        l.iter().zip(&rr).map(|(a, b)| a + b).collect()
    }

    fn wet_only(r: &mut Reverb) {
        set_plain(r, p::MIX, 100.0);
        set_plain(r, p::EARLY, 0.0);
        set_plain(r, p::PREDELAY, 0.0);
    }

    #[test]
    fn flat_fdn_decay_is_exact() {
        for (mode, lines, decay) in [(0.0, 1.0, 1.0), (0.0, 1.0, 3.0), (0.0, 0.0, 2.0), (1.0, 1.0, 1.0)] {
            let mut r = Reverb::new();
            wet_only(&mut r);
            set_plain(&mut r, p::MODE, mode);
            set_plain(&mut r, p::LINES, lines);
            set_plain(&mut r, p::DECAY, decay);
            set_plain(&mut r, p::DAMPING, 0.0);
            set_plain(&mut r, p::BASS_MULT, 1.0);
            let ir = impulse_response(&mut r, decay * 1.2 + 0.5);
            let t = rt60(&ir);
            assert!((t / decay - 1.0).abs() < 0.15, "mode {mode} lines {lines}: set {decay} s, measured {t} s");
        }
    }

    #[test]
    fn mid_band_decay_with_damping() {
        let mut r = Reverb::new();
        wet_only(&mut r);
        set_plain(&mut r, p::DECAY, 2.0);
        set_plain(&mut r, p::DAMPING, 60.0);
        let ir = impulse_response(&mut r, 3.0);
        // Band-pass the IR around 1 kHz.
        let c = SvfCoeffs::new(SvfMode::Bandpass, 1000.0, 2.0, 0.0, SR);
        let mut st = SvfState::default();
        let band: Vec<f32> = ir.iter().map(|&x| st.process(&c, x)).collect();
        let t = rt60(&band);
        assert!((t / 2.0 - 1.0).abs() < 0.15, "mid band {t} s");
        // High band decays clearly faster.
        let c = SvfCoeffs::new(SvfMode::Bandpass, 10_000.0, 2.0, 0.0, SR);
        let mut st = SvfState::default();
        let hi: Vec<f32> = ir.iter().map(|&x| st.process(&c, x)).collect();
        assert!(rt60(&hi) < 0.7 * t);
    }

    #[test]
    fn plate_decay_is_close() {
        for decay in [1.5f32, 4.0] {
            let mut r = Reverb::new();
            wet_only(&mut r);
            set_plain(&mut r, p::MODE, 2.0);
            set_plain(&mut r, p::DECAY, decay);
            set_plain(&mut r, p::DAMPING, 0.0);
            set_plain(&mut r, p::DAMP_FREQ, 16_000.0);
            let ir = impulse_response(&mut r, decay * 1.2 + 0.5);
            let t = rt60(&ir);
            assert!((t / decay - 1.0).abs() < 0.25, "plate set {decay} measured {t}");
        }
    }

    #[test]
    fn stereo_is_decorrelated() {
        for mode in [0.0, 2.0] {
            let mut r = Reverb::new();
            wet_only(&mut r);
            set_plain(&mut r, p::MODE, mode);
            let (l, rr) = process_stereo(&mut r, |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, 48_000);
            let (mut xy, mut xx, mut yy) = (0.0f64, 0.0f64, 0.0f64);
            for (a, b) in l.iter().zip(&rr).skip(2400) {
                xy += (*a as f64) * (*b as f64);
                xx += (*a as f64).powi(2);
                yy += (*b as f64).powi(2);
            }
            let corr = xy / (xx * yy).sqrt();
            assert!(corr.abs() < 0.5, "mode {mode}: correlation {corr}");
        }
    }

    #[test]
    fn mix_zero_is_dry() {
        let mut r = Reverb::new();
        set_plain(&mut r, p::MIX, 0.0);
        let (l, _) = process_stereo(&mut r, sine(300.0, 0.5), 10_000);
        let (want, _) = sine(300.0, 0.5)(9000);
        assert!((l[9000] - want).abs() < 1e-6);
    }

    #[test]
    fn wet_level_is_reasonable() {
        for mode in [0.0, 1.0, 2.0] {
            let mut r = Reverb::new();
            wet_only(&mut r);
            set_plain(&mut r, p::MODE, mode);
            let mut n = 1u32;
            let (l, _) = process_stereo(
                &mut r,
                |_| {
                    n ^= n << 13;
                    n ^= n >> 17;
                    n ^= n << 5;
                    let v = (n >> 8) as f32 / 8_388_608.0 - 1.0;
                    (v * 0.3, v * 0.3)
                },
                96_000,
            );
            let db = 20.0 * (rms(&l[48_000..]) / (0.3 / 3f32.sqrt())).log10();
            assert!(db.abs() < 8.0, "mode {mode}: wet level {db} dB");
        }
    }

    #[test]
    fn freeze_and_max_settings_stay_bounded() {
        let mut r = Reverb::new();
        for info in params() {
            r.set_param(info.id, 1.0);
        }
        let (l, rr) = process_stereo(&mut r, sine(200.0, 1.0), 48_000 * 3);
        assert!(peak(&l) < 20.0 && peak(&rr) < 20.0, "{} {}", peak(&l), peak(&rr));
        set_plain(&mut r, p::FREEZE, 1.0);
        set_plain(&mut r, p::MODE, 0.0);
        let (l, _) = process_stereo(&mut r, sine(200.0, 1.0), 48_000 * 3);
        assert!(l.iter().all(|v| v.is_finite()) && peak(&l) < 20.0);
    }

    fn noise(seed: u32) -> impl FnMut(usize) -> (f32, f32) {
        let mut n = seed.max(1);
        move |_| {
            n ^= n << 13;
            n ^= n >> 17;
            n ^= n << 5;
            let v = (n >> 8) as f32 / 8_388_608.0 - 1.0;
            (v * 0.3, v * 0.3)
        }
    }

    /// Energy of `x` at `freq` (Goertzel).
    fn tone(x: &[f32], freq: f32) -> f32 {
        let w = TAU * freq / SR as f32;
        let c = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &v in x {
            let s = v + c * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s1 * s1 + s2 * s2 - c * s1 * s2).max(0.0).sqrt() / x.len() as f32
    }

    #[test]
    fn every_algorithm_and_colour_keeps_its_decay_and_level() {
        for algo in 1..ALGORITHMS.len() {
            for color in 0..COLORS.len() {
                let mut r = Reverb::new();
                wet_only(&mut r);
                set_plain(&mut r, p::ALGORITHM, algo as f32);
                set_plain(&mut r, p::COLOR, color as f32);
                set_plain(&mut r, p::DECAY, 2.0);
                set_plain(&mut r, p::DAMPING, 0.0);
                set_plain(&mut r, p::SHIMMER, 0.0);
                let name = format!("{} / {}", ALGORITHMS[algo], COLORS[color]);
                let ir = impulse_response(&mut r, 3.0);
                assert!(ir.iter().all(|v| v.is_finite()), "{name}");
                // The shimmer voicing always feeds back, which lengthens it.
                if algo != 7 && color == 0 {
                    let t = rt60(&ir);
                    let tol = if VOICES[voice_index(algo, 0)].engine == Engine::Plate { 0.25 } else { 0.15 };
                    assert!((t / 2.0 - 1.0).abs() < tol, "{name}: measured {t} s");
                }
                let (l, _) = process_stereo(&mut r, noise(7), 96_000);
                let db = 20.0 * (rms(&l[48_000..]) / (0.3 / 3f32.sqrt())).log10();
                assert!(db.abs() < 9.0, "{name}: wet level {db} dB");
            }
        }
    }

    #[test]
    fn classic_mode_still_picks_the_engine() {
        let mut r = Reverb::new();
        set_plain(&mut r, p::MODE, 2.0);
        assert_eq!(r.voicing().0.engine, Engine::Plate);
        set_plain(&mut r, p::ALGORITHM, 1.0);
        assert_eq!(r.voicing().0.engine, Engine::Fdn, "an algorithm overrides Mode");
    }

    #[test]
    fn shimmer_adds_the_octave_above() {
        let burst = |i: usize| {
            let v = if i < 24_000 { 0.4 * (TAU * 440.0 * i as f32 / SR as f32).sin() } else { 0.0 };
            (v, v)
        };
        let octave = |shimmer: f32| {
            let mut r = Reverb::new();
            wet_only(&mut r);
            set_plain(&mut r, p::ALGORITHM, 1.0);
            set_plain(&mut r, p::DECAY, 4.0);
            set_plain(&mut r, p::SHIMMER, shimmer);
            let (l, _) = process_stereo(&mut r, burst, 96_000);
            let tail = &l[36_000..84_000];
            (tone(tail, 880.0), tone(tail, 1760.0))
        };
        let (dry8, dry16) = octave(0.0);
        let (wet8, wet16) = octave(100.0);
        assert!(wet8 > 4.0 * dry8, "+12 st: 880 Hz {wet8} vs {dry8}");
        assert!(wet16 > 4.0 * dry16, "it keeps climbing: 1760 Hz {wet16} vs {dry16}");
    }

    #[test]
    fn full_shimmer_on_a_long_decay_still_dies_away() {
        for algo in [1.0, 3.0, 7.0, 8.0] {
            let mut r = Reverb::new();
            wet_only(&mut r);
            set_plain(&mut r, p::ALGORITHM, algo);
            set_plain(&mut r, p::DECAY, 20.0);
            set_plain(&mut r, p::DAMPING, 0.0);
            set_plain(&mut r, p::SHIMMER, 100.0);
            let mut src = noise(3);
            let (l, _) = process_stereo(&mut r, |i| if i < 48_000 { src(i) } else { (0.0, 0.0) }, 48_000 * 40);
            let early = rms(&l[48_000 * 2..48_000 * 4]);
            let late = rms(&l[48_000 * 38..]);
            assert!(late.is_finite() && late < 0.3 * early, "algo {algo}: {early} → {late}");
            assert!(peak(&l) < 4.0, "algo {algo}: bounded {}", peak(&l));
        }
    }

    #[test]
    fn the_70s_colour_is_band_limited_and_grainy() {
        let hf = |color: f32| {
            let mut r = Reverb::new();
            wet_only(&mut r);
            set_plain(&mut r, p::ALGORITHM, 1.0);
            set_plain(&mut r, p::COLOR, color);
            set_plain(&mut r, p::DAMPING, 0.0);
            let (l, _) = process_stereo(&mut r, noise(11), 48_000);
            let c = SvfCoeffs::new(SvfMode::Highpass, 12_000.0, 0.707, 0.0, SR);
            let mut st = SvfState::default();
            let hi: Vec<f32> = l[24_000..].iter().map(|&x| st.process(&c, x)).collect();
            rms(&hi) / rms(&l[24_000..])
        };
        let (now, s80, s70) = (hf(0.0), hf(1.0), hf(2.0));
        assert!(s80 < 0.5 * now && s70 < 0.5 * s80, "now {now} 80s {s80} 70s {s70}");
        // A 12-bit tank rounds the quiet end of the tail to silence.
        let tail = |color: f32| {
            let mut r = Reverb::new();
            wet_only(&mut r);
            set_plain(&mut r, p::COLOR, color);
            set_plain(&mut r, p::DECAY, 1.0);
            let ir = impulse_response(&mut r, 2.0);
            rms(&ir[67_000..96_000])
        };
        let (float, word) = (tail(0.0), tail(2.0));
        assert!(float > 0.0 && word < 0.01 * float, "the tail truncates: {word} vs {float}");
    }

    #[test]
    fn ducking_lowers_the_tail_while_the_input_plays() {
        let level = |duck: f32| {
            let mut r = Reverb::new();
            wet_only(&mut r);
            set_plain(&mut r, p::DUCK, duck);
            let mut src = noise(5);
            let (l, _) = process_stereo(&mut r, |i| if i < 48_000 { src(i) } else { (0.0, 0.0) }, 72_000);
            (rms(&l[24_000..48_000]), rms(&l[60_000..72_000]))
        };
        let (open_during, open_after) = level(0.0);
        let (duck_during, duck_after) = level(100.0);
        assert!(duck_during < 0.3 * open_during, "ducked {duck_during} vs {open_during}");
        assert!(duck_after > 0.6 * open_after, "the tail comes back: {duck_after} vs {open_after}");
    }

    #[test]
    fn cpu_budget_per_instance() {
        // Worst case: 16 lines, max modulation, shimmer, 80s colour, ducking.
        let mut r = Reverb::new();
        set_plain(&mut r, p::ALGORITHM, 8.0);
        set_plain(&mut r, p::COLOR, 1.0);
        set_plain(&mut r, p::MOD_DEPTH, 100.0);
        set_plain(&mut r, p::SHIMMER, 50.0);
        set_plain(&mut r, p::DUCK, 50.0);
        set_plain(&mut r, p::EARLY, 100.0);
        let secs = 2.0;
        let t = std::time::Instant::now();
        let (l, _) = process_stereo(&mut r, noise(9), (secs * SR) as usize);
        let share = t.elapsed().as_secs_f64() / secs;
        assert!(l.iter().all(|v| v.is_finite()));
        // The test profile is optimised; leave room for a loaded CI machine.
        let budget = if cfg!(debug_assertions) { 0.25 } else { 0.06 };
        assert!(share < budget, "reverb uses {:.2} % of a core", share * 100.0);
    }
}
