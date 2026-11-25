//! The special types of `fx.filter` (its `Special` parameter): **DJ**,
//! **Comb**, **Vowel**, **Morph**, **Resample** and **Notch+LP**. With
//! `Special` off the filter is the SVF or ladder it always was; with one of
//! these on, it takes the place of Type, Slope and Ladder and the same
//! cutoff (smoothed, with the LFO and the envelope added in octaves) drives it.
//!
//! What the controls mean for each type (`Shape` is the second dimension, 0…1):
//!
//! | Type | Cutoff | Resonance | Shape |
//! |---|---|---|---|
//! | DJ | one knob: left of the middle a low-pass closes, right of it a high-pass; the middle is flat | resonance of the closing side | — |
//! | Comb | the fundamental (delay = 1 / cutoff) | feedback, up to 0.95 | damping in the loop |
//! | Vowel | sweeps the vowels A – E – I – O – U | how narrow the formants are | raises the formants, up to an octave |
//! | Morph | the corner | resonance | low-pass → band-pass → high-pass → notch |
//! | Resample | the sample rate is 2.4 × cutoff (a low-pass at the cutoff goes before it) | resonance of that low-pass | hold → linear interpolation |
//! | Notch+LP | the low-pass corner | its resonance | the notch from half an octave to two octaves above it |
//!
//! Every design is a pure function of the controls ([`Plan`]), so the editor
//! draws the response with [`response_db`] and it is the one the filter has.

use super::resonance_q;
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState, butterworth_q};
use std::f32::consts::FRAC_1_SQRT_2;

/// The list of the `Special` parameter: `Off` first, then the six types.
pub const NAMES: &[&str] = &["Off", "DJ", "Comb", "Vowel", "Morph", "Resample", "Notch+LP"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dj,
    Comb,
    Vowel,
    Morph,
    Resample,
    NotchLp,
}

impl Kind {
    /// The type a `Special` value stands for (`None` for Off).
    pub fn from_choice(choice: usize) -> Option<Kind> {
        match choice {
            1 => Some(Kind::Dj),
            2 => Some(Kind::Comb),
            3 => Some(Kind::Vowel),
            4 => Some(Kind::Morph),
            5 => Some(Kind::Resample),
            6 => Some(Kind::NotchLp),
            _ => None,
        }
    }
}

/// What a special type is told.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    /// Hz, after the LFO and the envelope.
    pub cutoff: f32,
    /// 0…100.
    pub resonance: f32,
    /// 0…1.
    pub shape: f32,
    /// 24 dB/oct (two sections) instead of 12 where the type has the choice.
    pub slope24: bool,
}

/// Octaves between 20 Hz and 20 kHz: the span of the cutoff knob.
pub const SPAN_OCT: f32 = 9.965_784;

/// Where a cutoff sits along the knob, 0 (20 Hz) … 1 (20 kHz).
pub fn position(cutoff: f32) -> f32 {
    ((cutoff.max(20.) / 20.).log2() / SPAN_OCT).clamp(0., 1.)
}

/// The corners of the DJ filter for a cutoff: `(low-pass, high-pass)`. Left of
/// the middle of the knob the low-pass closes from 20 kHz down and the
/// high-pass is open at 20 Hz; right of it the other way round.
pub fn dj_corners(cutoff: f32) -> (f32, f32) {
    let pos = position(cutoff);
    let lp = if pos < 0.5 { 20. * 1000f32.powf(2. * pos) } else { 20_000. };
    let hp = if pos > 0.5 { 20. * 1000f32.powf(2. * pos - 1.) } else { 20. };
    (lp, hp)
}

pub const VOWEL_NAMES: [&str; 5] = ["A", "E", "I", "O", "U"];
/// First three formants (Hz) of five vowels, average adult values from the
/// phonetics literature.
const VOWELS: [[f32; 3]; 5] = [[730., 1090., 2440.], [530., 1840., 2480.], [270., 2290., 3010.], [570., 840., 2410.], [300., 870., 2240.]];
const FORMANT_BANDWIDTH: [f32; 3] = [60., 90., 120.];
const FORMANT_GAIN: [f32; 3] = [1., 0.6, 0.35];
/// Loop length (samples) up to which a comb keeps the feedback it was given: 200 Hz at 48 kHz.
const COMB_LONGEST_FULL: f32 = 240.;
/// What the three bands make up for (a parallel band-pass bank passes little of a broadband input).
const VOWEL_MAKEUP: f32 = 2.5;

/// The three formants for a position along the knob (0 = A … 1 = U, gliding
/// between neighbours) raised by `shape` (0…1 = up to an octave).
pub fn vowel_formants(pos: f32, shape: f32) -> [f32; 3] {
    let v = pos.clamp(0., 1.) * 4.;
    let i = (v as usize).min(3);
    let t = v - i as f32;
    let raise = 2f32.powf(shape.clamp(0., 1.));
    std::array::from_fn(|k| (VOWELS[i][k].ln() * (1. - t) + VOWELS[i + 1][k].ln() * t).exp() * raise)
}

/// The vowel nearest a position along the knob.
pub fn vowel_name(pos: f32) -> &'static str {
    VOWEL_NAMES[(pos.clamp(0., 1.) * 4.).round() as usize]
}

/// A low-pass or high-pass of one or two sections (Butterworth pair for 24 dB/oct, the
/// resonance lifting the second), as the plain filter does it. Other modes get `q` twice.
pub fn pair(mode: SvfMode, hz: f32, q: f32, two: bool, sr: f64) -> [SvfCoeffs; 2] {
    if !two {
        return [SvfCoeffs::new(mode, hz, q, 0., sr), SvfCoeffs::passthrough()];
    }
    let (q1, q2) = match mode {
        SvfMode::Lowpass | SvfMode::Highpass => (butterworth_q(4, 0), butterworth_q(4, 1) * q / FRAC_1_SQRT_2),
        _ => (q, q),
    };
    [SvfCoeffs::new(mode, hz, q1, 0., sr), SvfCoeffs::new(mode, hz, q2, 0., sr)]
}

/// Down in the sub-bass a high Q would ring for seconds (and say nothing a
/// speaker can play): the ceiling of `q` follows the corner.
fn calm(q: f32, hz: f32) -> f32 {
    q.min((hz * 0.25).max(FRAC_1_SQRT_2))
}

/// The coefficients of one design.
#[derive(Clone, Copy)]
struct Plan {
    /// DJ: low-pass pair, high-pass pair. Notch+LP: low-pass pair, notch.
    /// Vowel: the three band-passes. Morph: low-pass, band-pass, high-pass of
    /// one pole pair. Resample: the anti-alias low-pass.
    c: [SvfCoeffs; 4],
    /// Vowel: the gain of each band. Morph: the weight of low-pass, band-pass and high-pass.
    w: [f32; 3],
    two: bool,
    /// Comb: loop length in samples, feedback, output compensation, damping.
    delay: f32,
    fb: f32,
    norm: f32,
    damp: f32,
    /// Resample: the share of the sample rate it keeps, and how much of the
    /// output is the interpolation rather than the held value.
    ratio: f32,
    mix: f32,
}

fn plan(kind: Kind, s: Spec, sr: f64) -> Plan {
    let mut pl = Plan { c: [SvfCoeffs::passthrough(); 4], w: [0.; 3], two: s.slope24, delay: 2., fb: 0., norm: 1., damp: 0., ratio: 1., mix: 0. };
    let nyq = (sr * 0.45) as f32;
    let q = resonance_q(s.resonance);
    let shape = s.shape.clamp(0., 1.);
    match kind {
        Kind::Dj => {
            let pos = position(s.cutoff);
            let (lp, hp) = dj_corners(s.cutoff);
            let low = pair(SvfMode::Lowpass, lp.min(nyq), if pos < 0.5 { calm(q, lp) } else { FRAC_1_SQRT_2 }, s.slope24, sr);
            let high = pair(SvfMode::Highpass, hp.min(nyq), if pos > 0.5 { calm(q, hp) } else { FRAC_1_SQRT_2 }, s.slope24, sr);
            pl.c = [low[0], low[1], high[0], high[1]];
        }
        Kind::Comb => {
            let g = (s.resonance / 100.).clamp(0., 1.) * 0.95;
            pl.delay = (sr as f32 / s.cutoff.clamp(20., nyq)).max(2.);
            // A comb a few octaves down would ring for ages (each trip around the loop takes longer),
            // so below 200 Hz the feedback shrinks to keep the ring-out about as long as at 200 Hz.
            let g = g.powf((pl.delay / COMB_LONGEST_FULL).max(1.));
            pl.fb = g;
            pl.norm = (1. - g * g).sqrt();
            pl.damp = shape * 0.9;
        }
        Kind::Vowel => {
            let formants = vowel_formants(position(s.cutoff), shape);
            let narrow = 3. * (0.5f32 / 3.).powf((s.resonance / 100.).clamp(0., 1.));
            for k in 0..3 {
                let f = formants[k].min(nyq);
                pl.c[k] = SvfCoeffs::new(SvfMode::Bandpass, f, (f / (FORMANT_BANDWIDTH[k] * narrow)).clamp(1.5, 60.), 0., sr);
            }
            pl.w = FORMANT_GAIN;
        }
        Kind::Morph => {
            let each = if s.slope24 { q.sqrt().max(0.5) } else { q };
            let f = s.cutoff.min(nyq);
            pl.c = [SvfCoeffs::new(SvfMode::Lowpass, f, each, 0., sr), SvfCoeffs::new(SvfMode::Bandpass, f, each, 0., sr), SvfCoeffs::new(SvfMode::Highpass, f, each, 0., sr), SvfCoeffs::passthrough()];
            // low-pass → band-pass → high-pass → notch (high-pass plus low-pass).
            let at = shape * 3.;
            pl.w = if at < 1. {
                [1. - at, at, 0.]
            } else if at < 2. {
                [0., 2. - at, at - 1.]
            } else {
                [at - 2., 0., 1.]
            };
        }
        Kind::Resample => {
            pl.c[0] = SvfCoeffs::new(SvfMode::Lowpass, s.cutoff.min(nyq), calm(q, s.cutoff), 0., sr);
            pl.ratio = (2.4 * s.cutoff / sr as f32).min(1.);
            pl.mix = shape;
        }
        Kind::NotchLp => {
            let low = pair(SvfMode::Lowpass, s.cutoff.min(nyq), calm(q, s.cutoff), s.slope24, sr);
            let notch = (s.cutoff * 2f32.powf(0.5 + 1.5 * shape)).min(nyq);
            pl.c = [low[0], low[1], SvfCoeffs::new(SvfMode::Notch, notch, 2., 0., sr), SvfCoeffs::passthrough()];
        }
    }
    pl
}

/// One channel of a special filter: every type's state, and the design in use.
#[derive(Clone)]
pub struct Ext {
    sr: f64,
    plan: Plan,
    st: [SvfState; 8],
    line: DelayLine,
    damped: f32,
    phase: f32,
    held: f32,
    prev: f32,
}

impl Default for Ext {
    fn default() -> Self {
        Self::new()
    }
}

impl Ext {
    pub fn new() -> Self {
        let sr = 48_000.;
        Self { sr, plan: plan(Kind::Morph, Spec { cutoff: 1000., resonance: 15., shape: 0., slope24: false }, sr), st: [SvfState::default(); 8], line: DelayLine::new(2_410), damped: 0., phase: 0., held: 0., prev: 0. }
    }

    /// Size the comb's delay for the sample rate (allocates: call from `prepare`).
    pub fn prepare(&mut self, sr: f64) {
        self.sr = sr;
        self.line.resize((sr / 20.).ceil() as usize + 8);
        self.reset();
    }

    pub fn reset(&mut self) {
        for s in &mut self.st {
            s.reset();
        }
        self.line.clear();
        self.damped = 0.;
        self.phase = 0.;
        self.held = 0.;
        self.prev = 0.;
    }

    pub fn design(&mut self, kind: Kind, spec: Spec) {
        self.plan = plan(kind, spec, self.sr);
    }

    #[inline]
    pub fn process(&mut self, kind: Kind, x: f32) -> f32 {
        let pl = self.plan;
        match kind {
            Kind::Dj => {
                let mut y = x;
                for k in 0..4 {
                    y = self.st[k].process(&pl.c[k], y);
                }
                y
            }
            Kind::NotchLp => {
                let mut y = x;
                for k in 0..3 {
                    y = self.st[k].process(&pl.c[k], y);
                }
                y
            }
            Kind::Morph => {
                let mut y = self.morph(0, x);
                if pl.two {
                    y = self.morph(4, y);
                }
                y
            }
            Kind::Vowel => {
                let mut y = 0.;
                for k in 0..3 {
                    y += pl.w[k] * self.st[k].process(&pl.c[k], x);
                }
                y * VOWEL_MAKEUP
            }
            Kind::Comb => {
                let echo = self.line.read_linear(pl.delay - 1.);
                self.damped = (1. - pl.damp) * echo + pl.damp * self.damped;
                let v = x + pl.fb * self.damped;
                self.line.push(v);
                pl.norm * v
            }
            Kind::Resample => {
                let x = self.st[0].process(&pl.c[0], x);
                self.phase += pl.ratio;
                if self.phase >= 1. {
                    self.phase -= 1.;
                    self.prev = self.held;
                    self.held = x;
                }
                let between = self.prev + (self.held - self.prev) * self.phase.clamp(0., 1.);
                self.held + (between - self.held) * pl.mix
            }
        }
    }

    /// One pass of the morphing filter: low-pass, band-pass and high-pass mixed.
    #[inline]
    fn morph(&mut self, at: usize, x: f32) -> f32 {
        let pl = self.plan;
        pl.w[0] * self.st[at].process(&pl.c[0], x) + pl.w[1] * self.st[at + 1].process(&pl.c[1], x) + pl.w[2] * self.st[at + 2].process(&pl.c[2], x)
    }
}

/// The magnitude (dB) at `hz` of what [`Ext`] does for these controls: exact
/// for DJ, Morph and Notch+LP (cascades of SVF sections) and Vowel (three
/// bands in parallel), the ideal delay for Comb, and for Resample the
/// anti-alias low-pass in front of the sampler (its images are not drawn).
pub fn response_db(kind: Kind, spec: Spec, hz: f32, sr: f64) -> f32 {
    let pl = plan(kind, spec, sr);
    let at = |c: &SvfCoeffs| c.response(f64::from(hz), sr);
    let mul = |a: (f64, f64), b: (f64, f64)| (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0);
    let (re, im) = match kind {
        Kind::Dj => pl.c.iter().map(at).fold((1., 0.), mul),
        Kind::NotchLp => pl.c[..3].iter().map(at).fold((1., 0.), mul),
        Kind::Resample => at(&pl.c[0]),
        Kind::Morph => {
            let pass = (0..3).map(|k| at(&pl.c[k])).zip(pl.w).fold((0., 0.), |a, (r, w)| (a.0 + f64::from(w) * r.0, a.1 + f64::from(w) * r.1));
            if pl.two { mul(pass, pass) } else { pass }
        }
        Kind::Vowel => (0..3).map(|k| at(&pl.c[k])).zip(pl.w).fold((0., 0.), |a, (r, w)| {
            let w = f64::from(w * VOWEL_MAKEUP);
            (a.0 + w * r.0, a.1 + w * r.1)
        }),
        Kind::Comb => {
            // H = norm / (1 − g · damper(ω) · e^(−jωD)), the damper a one-pole low-pass.
            let w = std::f64::consts::TAU * f64::from(hz) / sr;
            let a = f64::from(pl.damp);
            let (dr, di) = (1. - a * w.cos(), a * w.sin());
            let den = dr * dr + di * di;
            let damper = ((1. - a) * dr / den, -(1. - a) * di / den);
            let (c, s) = ((w * f64::from(pl.delay)).cos(), -(w * f64::from(pl.delay)).sin());
            let loop_gain = mul(damper, (c, s));
            let (br, bi) = (1. - f64::from(pl.fb) * loop_gain.0, -f64::from(pl.fb) * loop_gain.1);
            let n = br * br + bi * bi;
            (f64::from(pl.norm) * br / n, -f64::from(pl.norm) * bi / n)
        }
    };
    (10. * (re * re + im * im).max(1e-30).log10()) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SR: f64 = 48_000.0;

    fn spec(cutoff: f32) -> Spec {
        Spec { cutoff, resonance: 15., shape: 0., slope24: false }
    }

    /// The cutoff at a position along the knob.
    fn at(pos: f32) -> f32 {
        20. * 1000f32.powf(pos)
    }

    /// Gain in dB of a special type for a sine at `hz`, measured through the processor.
    fn measured_db(kind: Kind, s: Spec, hz: f32) -> f32 {
        let mut e = Ext::new();
        e.prepare(SR);
        e.design(kind, s);
        let n = 48_000;
        let (mut input, mut output) = (0f64, 0f64);
        for i in 0..n {
            let x = (0.1 * (TAU * f64::from(hz) * i as f64 / SR).sin()) as f32;
            let y = e.process(kind, x);
            if i >= n / 2 {
                input += f64::from(x * x);
                output += f64::from(y * y);
            }
        }
        (10. * (output / input).log10()) as f32
    }

    fn noise(n: usize, amp: f32) -> Vec<f32> {
        let mut s = 0x1234_5678u32;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                ((s >> 8) as f32 / 8_388_608.0 * 2. - 1.) * amp
            })
            .collect()
    }

    #[test]
    fn the_knob_has_a_position_and_the_choices_a_type() {
        assert_eq!((position(20.), position(20_000.)), (0., 1.));
        assert!((position(632.46) - 0.5).abs() < 1e-3);
        assert_eq!(NAMES.len(), 7);
        assert_eq!(Kind::from_choice(0), None);
        assert_eq!((Kind::from_choice(1), Kind::from_choice(6)), (Some(Kind::Dj), Some(Kind::NotchLp)));
        assert_eq!(Kind::from_choice(7), None);
    }

    #[test]
    fn the_dj_corners_close_one_side_and_leave_the_other_open() {
        let (lp, hp) = dj_corners(at(0.1));
        assert!(lp < 100. && hp == 20., "left: a low-pass closing {lp}, the high-pass open {hp}");
        let (lp, hp) = dj_corners(at(0.9));
        assert!(lp == 20_000. && hp > 4_000., "right: a high-pass closing {hp}, the low-pass open {lp}");
        let (lp, hp) = dj_corners(at(0.5));
        assert!((lp - 20_000.).abs() < 700. && (hp - 20.).abs() < 1., "flat in the middle: {lp} {hp}");
        // The two sides meet without a jump.
        let (a, b) = (dj_corners(at(0.499)), dj_corners(at(0.501)));
        assert!(a.0 > 17_000. && b.1 < 25., "{a:?} {b:?}");
    }

    #[test]
    fn the_vowels_glide_and_the_shape_raises_them() {
        let a = vowel_formants(0., 0.);
        assert_eq!(a.map(|f| f.round()), [730., 1090., 2440.]);
        assert_eq!(vowel_formants(1., 0.).map(|f| f.round()), [300., 870., 2240.]);
        let mid = vowel_formants(0.125, 0.);
        assert!((mid[0] - (730f32 * 530.).sqrt()).abs() < 1., "half way A to E, on a log scale: {}", mid[0]);
        let up = vowel_formants(0., 1.);
        assert!((up[1] / a[1] - 2.).abs() < 1e-3, "an octave up");
        assert_eq!((vowel_name(0.), vowel_name(0.5), vowel_name(1.)), ("A", "I", "U"));
    }

    #[test]
    fn dj_is_a_low_pass_on_the_left_a_high_pass_on_the_right_and_flat_in_the_middle() {
        let left = spec(at(0.25)); // low-pass at about 630 Hz
        assert!(measured_db(Kind::Dj, left, 100.).abs() < 1.5);
        assert!(measured_db(Kind::Dj, left, 5_000.) < -20.);
        let right = spec(at(0.75)); // high-pass at about 630 Hz
        assert!(measured_db(Kind::Dj, right, 5_000.).abs() < 1.5);
        assert!(measured_db(Kind::Dj, right, 100.) < -20.);
        let middle = spec(at(0.5));
        for hz in [100., 1_000., 8_000.] {
            assert!(measured_db(Kind::Dj, middle, hz).abs() < 1., "{hz} Hz: {}", measured_db(Kind::Dj, middle, hz));
        }
        // 24 dB/oct is twice as steep.
        let steeper = Spec { slope24: true, ..left };
        assert!(measured_db(Kind::Dj, steeper, 5_000.) < measured_db(Kind::Dj, left, 5_000.) - 15.);
    }

    #[test]
    fn a_comb_repeats_at_its_delay_and_rings_at_multiples_of_its_fundamental() {
        // 480 Hz at 48 kHz is exactly 100 samples.
        let s = Spec { cutoff: 480., resonance: 60., shape: 0., slope24: false };
        let mut e = Ext::new();
        e.prepare(SR);
        e.design(Kind::Comb, s);
        let out: Vec<f32> = (0..420).map(|i| e.process(Kind::Comb, if i == 0 { 1. } else { 0. })).collect();
        let g: f32 = 0.6 * 0.95;
        let norm = (1. - g * g).sqrt();
        for k in 0..4 {
            assert!((out[100 * k] - norm * g.powi(k as i32)).abs() < 1e-4, "repeat {k}: {}", out[100 * k]);
        }
        assert!(out[50].abs() < 1e-6 && out[150].abs() < 1e-6, "silence between the repeats");
        let (on, off) = (measured_db(Kind::Comb, Spec { resonance: 80., ..s }, 480.), measured_db(Kind::Comb, Spec { resonance: 80., ..s }, 240.));
        assert!(on > off + 15., "rings at 480 Hz ({on:.1} dB), not at 240 Hz ({off:.1} dB)");
    }

    #[test]
    fn damping_darkens_a_combs_repeats() {
        let run = |shape: f32| {
            let mut e = Ext::new();
            e.prepare(SR);
            e.design(Kind::Comb, Spec { cutoff: 480., resonance: 70., shape, slope24: false });
            (0..420).map(|i| e.process(Kind::Comb, if i == 0 { 1. } else { 0. })).collect::<Vec<f32>>()
        };
        let (bright, dark) = (run(0.), run(0.9));
        assert!(dark[300].abs() < bright[300].abs() * 0.5, "the third repeat is quieter and softer: {} vs {}", dark[300], bright[300]);
        // A click in a damped loop spreads over a few samples instead of staying one.
        assert!(dark[101].abs() > bright[101].abs() + 0.01);
    }

    #[test]
    fn the_vowel_filter_peaks_at_the_formants_the_knob_points_at() {
        let peak_near = |pos: f32, target: f32| {
            let s = spec(at(pos));
            (0..200).map(|i| target * (0.8 + 0.4 * i as f32 / 199.)).map(|hz| (hz, response_db(Kind::Vowel, s, hz, SR))).fold((0., f32::MIN), |m, x| if x.1 > m.1 { x } else { m })
        };
        for (pos, formants) in [(0., [730., 1090.]), (0.5, [270., 2290.]), (1., [300., 870.])] {
            for f in formants {
                let (hz, _) = peak_near(pos, f);
                assert!((hz / f - 1.).abs() < 0.12, "pos {pos}: a peak at {hz:.0} Hz, wanted about {f}");
            }
        }
        // A vowel is a band: far from the formants it is well down (checked on the real processor).
        let a = spec(at(0.));
        let (near, far_low, far_high) = (measured_db(Kind::Vowel, a, 730.), measured_db(Kind::Vowel, a, 150.), measured_db(Kind::Vowel, a, 6_000.));
        assert!(near > far_low + 12. && near > far_high + 12., "{near:.1} vs {far_low:.1} and {far_high:.1}");
    }

    #[test]
    fn morph_goes_from_low_pass_through_band_pass_and_high_pass_to_notch() {
        let base = spec(1_000.);
        let at_shape = |shape: f32, hz: f32| measured_db(Kind::Morph, Spec { shape, ..base }, hz);
        // shape 0: low-pass
        assert!(at_shape(0., 125.).abs() < 1.5 && at_shape(0., 8_000.) < -30.);
        // 1/3: band-pass, 0 dB at the centre
        assert!(at_shape(1. / 3., 1_000.).abs() < 1.5 && at_shape(1. / 3., 125.) < -12. && at_shape(1. / 3., 8_000.) < -12.);
        // 2/3: high-pass
        assert!(at_shape(2. / 3., 8_000.).abs() < 1.5 && at_shape(2. / 3., 125.) < -30.);
        // 1: notch
        assert!(at_shape(1., 1_000.) < -25. && at_shape(1., 125.).abs() < 2. && at_shape(1., 8_000.).abs() < 2.);
    }

    #[test]
    fn the_drawn_response_is_what_each_type_does() {
        let cases = [
            (Kind::Dj, Spec { cutoff: at(0.3), resonance: 40., shape: 0., slope24: false }),
            (Kind::Dj, Spec { cutoff: at(0.8), resonance: 30., shape: 0., slope24: true }),
            (Kind::Morph, Spec { cutoff: 900., resonance: 35., shape: 0.5, slope24: false }),
            (Kind::Morph, Spec { cutoff: 2_000., resonance: 20., shape: 0.9, slope24: true }),
            (Kind::NotchLp, Spec { cutoff: 1_000., resonance: 25., shape: 0.3, slope24: false }),
            (Kind::NotchLp, Spec { cutoff: 600., resonance: 10., shape: 0.8, slope24: true }),
            (Kind::Vowel, Spec { cutoff: at(0.25), resonance: 30., shape: 0.2, slope24: false }),
            (Kind::Comb, Spec { cutoff: 480., resonance: 50., shape: 0.4, slope24: false }),
            (Kind::Resample, Spec { cutoff: 3_000., resonance: 20., shape: 0., slope24: false }),
        ];
        for (kind, s) in cases {
            for hz in [120., 340., 700., 1_500., 3_100., 6_000.] {
                let drawn = response_db(kind, s, hz, SR);
                if drawn < -50. {
                    continue;
                }
                let measured = if kind == Kind::Resample {
                    // The sampler is not part of the drawing: measure the low-pass alone.
                    let mut lp = Ext::new();
                    lp.prepare(SR);
                    lp.design(Kind::Morph, Spec { shape: 0., slope24: false, ..s });
                    let n = 48_000;
                    let (mut i2, mut o2) = (0f64, 0f64);
                    for i in 0..n {
                        let x = (0.1 * (TAU * f64::from(hz) * i as f64 / SR).sin()) as f32;
                        let y = lp.process(Kind::Morph, x);
                        if i >= n / 2 {
                            i2 += f64::from(x * x);
                            o2 += f64::from(y * y);
                        }
                    }
                    (10. * (o2 / i2).log10()) as f32
                } else {
                    measured_db(kind, s, hz)
                };
                assert!((drawn - measured).abs() < 0.6, "{kind:?} {s:?} at {hz} Hz: drawn {drawn:.2}, measured {measured:.2}");
            }
        }
    }

    #[test]
    fn notch_lp_cuts_a_notch_above_the_corner_and_low_passes() {
        let s = |shape: f32| Spec { cutoff: 1_000., resonance: 15., shape, slope24: false };
        let near = measured_db(Kind::NotchLp, s(0.), 1_414.);
        let plain_lp = measured_db(Kind::Morph, s(0.), 1_414.);
        assert!(near < plain_lp - 15., "the notch half an octave above the corner: {near:.1} dB against {plain_lp:.1} dB");
        assert!(measured_db(Kind::NotchLp, s(0.), 100.).abs() < 1.5 && measured_db(Kind::NotchLp, s(0.), 8_000.) < -30.);
        // The shape moves the notch up: two octaves above the corner at 100 %.
        assert!(measured_db(Kind::NotchLp, s(1.), 4_000.) < measured_db(Kind::Morph, s(0.), 4_000.) - 15.);
        assert!(measured_db(Kind::NotchLp, s(1.), 1_414.) > near + 10., "and it has left 1.4 kHz");
    }

    #[test]
    fn resampling_holds_each_sample_for_a_while_and_the_shape_interpolates_between_them() {
        let steps = |s: Spec| {
            let mut e = Ext::new();
            e.prepare(SR);
            e.design(Kind::Resample, s);
            let out: Vec<f32> = (0..9_600).map(|i| e.process(Kind::Resample, (0.5 * (TAU * 100. * i as f64 / SR).sin()) as f32)).collect();
            out[2_400..].windows(2).filter(|w| w[0] != w[1]).count()
        };
        // 2 kHz cutoff: the sample rate is 4.8 kHz, a new value every 10 samples.
        let hold = steps(Spec { cutoff: 2_000., resonance: 5., shape: 0., slope24: false });
        assert!((600..=800).contains(&hold), "7200 samples at one change in 10: {hold}");
        let smooth = steps(Spec { cutoff: 2_000., resonance: 5., shape: 1., slope24: false });
        assert!(smooth > 6_500, "interpolated: it moves every sample ({smooth})");
        // Wide open it is out of the way: the sample rate is the plain rate and the sound passes.
        let open = Spec { cutoff: 20_000., resonance: 0., shape: 0., slope24: false };
        assert!(measured_db(Kind::Resample, open, 1_000.).abs() < 1.5);
    }

    #[test]
    fn every_type_stays_finite_and_dies_away_after_loud_noise_at_extreme_settings() {
        for kind in [Kind::Dj, Kind::Comb, Kind::Vowel, Kind::Morph, Kind::Resample, Kind::NotchLp] {
            for (cutoff, resonance, shape, slope24) in [(20., 100., 1., true), (20_000., 100., 1., true), (440., 100., 0., false), (3_000., 0., 0.5, true)] {
                let mut e = Ext::new();
                e.prepare(SR);
                let s = Spec { cutoff, resonance, shape, slope24 };
                for (i, x) in noise(48_000, 8.).into_iter().enumerate() {
                    if i % 8 == 0 {
                        e.design(kind, Spec { cutoff: cutoff * (1. + 0.3 * ((i / 8) as f32 * 0.01).sin()), ..s });
                    }
                    let y = e.process(kind, x);
                    assert!(y.is_finite() && y.abs() < 2_000., "{kind:?} {s:?} sample {i}: {y}");
                }
                let mut last = 0f32;
                for _ in 0..96_000 {
                    last = e.process(kind, 0.);
                    assert!(last.is_finite());
                }
                assert!(last.abs() < 1e-3, "{kind:?} {s:?} left {last} after a second of silence");
            }
        }
    }
}
