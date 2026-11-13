//! `fx.amp` — overdrive pedals and a preamp built from circuit models.
//!
//! Modes (each a small circuit at 2× or 4×, its tone section at the host
//! rate):
//! - **Centaur** (Klon-style): a gain stage with bass cut feeds a wave
//!   digital filter RC clipper with germanium diodes ([`wdf::DiodeClipper`]);
//!   the clipped path is summed with the clean one, more of it as the gain
//!   goes up (the ganged blend), then an active treble control (shelf).
//! - **Screamer** (TS-808-style): the op-amp stage only amplifies above
//!   ~720 Hz (the mid hump); its feedback diodes are modelled as a WDF
//!   silicon clipper whose RC corner follows the drive pot (the 51 pF cap
//!   across 51 k + drive), the clean signal is added back (non-inverting
//!   stage), then the low-pass tone control.
//! - **Fuzz**: coupling high-pass, very high gain, a bias offset (starved
//!   transistor: asymmetry and gating) into an asymmetric WDF clipper
//!   (silicon one way, germanium the other), DC blocker, then the classic
//!   passive low-pass / high-pass blend tone (mid scoop).
//! - **Amp**: two triode stages (biased tanh with ADAA, coupling caps
//!   between them) into the Fender '59 Bassman passive tone stack, solved
//!   in closed form (Yeh & Smith, DAFx-06) and discretised by the bilinear
//!   transform.
//!
//! An optional cabinet filter (Off / 1x12 / 2x12 / 4x12) follows: resonant
//! high-pass, low resonance, presence and a steep speaker roll-off.
//!
//! Meter layout: `[input peak, output peak, clipping (peak diode voltage
//! over its knee)]`.

use super::shapes::{Adaa, Curve};
use super::wdf::{Diode, DiodeClipper, DiodePair};
use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::oversample::Oversampler;
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::{OnePole, Svf, SvfCoeffs, SvfMode};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "fx.amp";

pub mod p {
    pub const MODE: u32 = 0;
    pub const GAIN: u32 = 1;
    pub const TONE: u32 = 2;
    pub const BASS: u32 = 3;
    pub const MID: u32 = 4;
    pub const TREBLE: u32 = 5;
    pub const BIAS: u32 = 6;
    pub const CAB: u32 = 7;
    pub const INPUT: u32 = 8;
    pub const OUTPUT: u32 = 9;
    pub const MIX: u32 = 10;
    pub const OVERSAMPLING: u32 = 11;
}

pub const MODES: &[&str] = &["Centaur", "Screamer", "Fuzz", "Amp"];
pub const CABS: &[&str] = &["Off", "1x12 Open", "2x12 Combo", "4x12 Closed"];
const OS: &[&str] = &["2x", "4x"];

pub mod mode {
    pub const CENTAUR: usize = 0;
    pub const SCREAMER: usize = 1;
    pub const FUZZ: usize = 2;
    pub const AMP: usize = 3;
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 3.0, 0.0).choices(MODES),
        ParamInfo::new(p::GAIN, "Gain", 0.0, 100.0, 40.0).unit("%"),
        ParamInfo::new(p::TONE, "Tone", 0.0, 100.0, 50.0).unit("%"),
        ParamInfo::new(p::BASS, "Bass", 0.0, 100.0, 50.0).unit("%"),
        ParamInfo::new(p::MID, "Mid", 0.0, 100.0, 50.0).unit("%"),
        ParamInfo::new(p::TREBLE, "Treble", 0.0, 100.0, 50.0).unit("%").short("Treb"),
        ParamInfo::new(p::BIAS, "Bias", 0.0, 100.0, 50.0).unit("%"),
        ParamInfo::new(p::CAB, "Cabinet", 0.0, 3.0, 0.0).choices(CABS).short("Cab"),
        ParamInfo::new(p::INPUT, "Input", -24.0, 24.0, 0.0).unit("dB").short("In"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OVERSAMPLING, "Oversampling", 0.0, 1.0, 0.0).choices(OS).short("OS"),
    ]
}

/// Which knobs a mode uses (the editor hides the others).
pub fn mode_params(m: usize) -> &'static [u32] {
    match m {
        mode::AMP => &[p::GAIN, p::BASS, p::MID, p::TREBLE, p::BIAS],
        mode::FUZZ => &[p::GAIN, p::TONE, p::BIAS],
        _ => &[p::GAIN, p::TONE],
    }
}

// ───────────────────────────── Circuit values ─────────────────────────────

/// Clipping-stage settings of a mode at `gain` and `bias` (0..1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stage {
    /// Linear gain into the clipper.
    pub gain: f32,
    /// High-pass corner before it (Hz).
    pub pre_hp: f32,
    /// Clipper R (Ω) and C (F).
    pub r: f64,
    pub c: f64,
    pub diodes: DiodePairKind,
    /// DC offset into the clipper (fuzz bias), volts.
    pub offset: f32,
    /// Share of the clipped path in the output (the rest is clean).
    pub blend: f32,
    /// Output normalisation (1 / the diodes' knee voltage).
    pub norm: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiodePairKind {
    Germanium,
    Silicon,
    Asymmetric,
}

impl DiodePairKind {
    pub fn pair(self) -> DiodePair {
        match self {
            DiodePairKind::Germanium => DiodePair::symmetric(Diode::GERMANIUM),
            DiodePairKind::Silicon => DiodePair::symmetric(Diode::SILICON),
            DiodePairKind::Asymmetric => DiodePair { pos: Diode::SILICON, neg: Diode::GERMANIUM },
        }
    }
}

pub fn stage(m: usize, gain: f32, bias: f32) -> Stage {
    let g = gain.clamp(0.0, 1.0);
    match m {
        mode::CENTAUR => Stage {
            gain: db_to_gain(6.0 + 36.0 * g),
            pre_hp: 400.0,
            r: 1_000.0,
            c: 1.0 / (std::f64::consts::TAU * 1_000.0 * 4_800.0),
            diodes: DiodePairKind::Germanium,
            offset: 0.0,
            blend: 0.3 + 0.65 * g,
            norm: 1.0 / 0.32,
        },
        mode::SCREAMER => {
            // Non-inverting gain 1 + (51 k + drive) / 4.7 k, with the 51 pF
            // feedback cap's corner sliding from ~60 kHz down to ~5.7 kHz.
            let rf = 51_000.0 + 500_000.0 * (g as f64).powi(2);
            let fc = (1.0 / (std::f64::consts::TAU * 51e-12 * rf)).min(20_000.0);
            Stage {
                gain: (rf / 4_700.0) as f32,
                pre_hp: 720.0,
                r: 10_000.0,
                c: 1.0 / (std::f64::consts::TAU * 10_000.0 * fc),
                diodes: DiodePairKind::Silicon,
                offset: 0.0,
                blend: 1.0,
                norm: 1.0 / 0.62,
            }
        }
        mode::FUZZ => Stage {
            gain: db_to_gain(20.0 + 36.0 * g),
            pre_hp: 90.0,
            r: 10_000.0,
            c: 1.0 / (std::f64::consts::TAU * 10_000.0 * 6_000.0),
            diodes: DiodePairKind::Asymmetric,
            offset: (bias.clamp(0.0, 1.0) - 0.5) * 0.8,
            blend: 1.0,
            norm: 1.0 / 0.5,
        },
        _ => Stage {
            gain: db_to_gain(30.0 * g),
            pre_hp: 30.0,
            r: 0.0,
            c: 0.0,
            diodes: DiodePairKind::Silicon,
            offset: 0.0,
            blend: 1.0,
            norm: 1.0,
        },
    }
}

/// Bias of the triode curves in Amp mode.
fn tube_bias(bias: f32) -> f64 {
    0.15 + 0.5 * bias.clamp(0.0, 1.0) as f64
}

/// Static (DC) transfer of a mode's clipping stage, for the editor's curve:
/// the capacitor is open at DC, leaving the source resistor and the diodes.
pub fn transfer(m: usize, gain: f32, bias: f32, x: f32) -> f32 {
    let st = stage(m, gain, bias);
    if m == mode::AMP {
        let k = tube_bias(bias);
        let c = Curve::Tube;
        let n = (1.0 / c.slope0(k)) as f32;
        let v1 = c.f((x * st.gain) as f64, k) as f32 * n;
        let g2 = db_to_gain(6.0 + 18.0 * gain);
        return c.f((v1 * g2) as f64, k) as f32 * n / (1.0 + 0.2 * g2);
    }
    let a = (x * st.gain + st.offset) as f64;
    let pair = st.diodes.pair();
    let v = 0.5 * (a + pair.reflect(a, st.r));
    let off = 0.5 * (st.offset as f64 + pair.reflect(st.offset as f64, st.r));
    let clipped = ((v - off) as f32) * st.norm;
    let dry = if m == mode::SCREAMER { x } else { 0.0 };
    (1.0 - st.blend) * x + st.blend * clipped + dry
}

/// The Fender '59 Bassman tone stack (Yeh & Smith, DAFx-06, eq. 1–2):
/// analogue `H(s) = (b1 s + b2 s² + b3 s³) / (1 + a1 s + a2 s² + a3 s³)`
/// for treble `t`, mid `m` and bass `l` pot positions (0..1).
pub fn bassman_analog(t: f64, m: f64, l: f64) -> ([f64; 4], [f64; 4]) {
    let (r1, r2, r3, r4) = (250e3, 1e6, 25e3, 56e3);
    let (c1, c2, c3) = (250e-12, 20e-9, 20e-9);
    let b1 = t * c1 * r1 + m * c3 * r3 + l * (c1 * r2 + c2 * r2) + (c1 * r3 + c2 * r3);
    let b2 = t * (c1 * c2 * r1 * r4 + c1 * c3 * r1 * r4) - m * m * (c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
        + m * (c1 * c3 * r1 * r3 + c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
        + l * (c1 * c2 * r1 * r2 + c1 * c2 * r2 * r4 + c1 * c3 * r2 * r4)
        + l * m * (c1 * c3 * r2 * r3 + c2 * c3 * r2 * r3)
        + (c1 * c2 * r1 * r3 + c1 * c2 * r3 * r4 + c1 * c3 * r3 * r4);
    let c123 = c1 * c2 * c3;
    let b3 = l * m * (c123 * r1 * r2 * r3 + c123 * r2 * r3 * r4) - m * m * (c123 * r1 * r3 * r3 + c123 * r3 * r3 * r4)
        + m * (c123 * r1 * r3 * r3 + c123 * r3 * r3 * r4)
        + t * c123 * r1 * r3 * r4
        - t * m * c123 * r1 * r3 * r4
        + t * l * c123 * r1 * r2 * r4;
    let a1 = (c1 * r1 + c1 * r3 + c2 * r3 + c2 * r4 + c3 * r4) + m * c3 * r3 + l * (c1 * r2 + c2 * r2);
    let a2 = m * (c1 * c3 * r1 * r3 - c2 * c3 * r3 * r4 + c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
        + l * m * (c1 * c3 * r2 * r3 + c2 * c3 * r2 * r3)
        - m * m * (c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
        + l * (c1 * c2 * r2 * r4 + c1 * c2 * r1 * r2 + c1 * c3 * r2 * r4 + c2 * c3 * r2 * r4)
        + (c1 * c2 * r1 * r4 + c1 * c3 * r1 * r4 + c1 * c2 * r3 * r4 + c1 * c2 * r1 * r3 + c1 * c3 * r3 * r4 + c2 * c3 * r3 * r4);
    let a3 = l * m * (c123 * r1 * r2 * r3 + c123 * r2 * r3 * r4) - m * m * (c123 * r1 * r3 * r3 + c123 * r3 * r3 * r4)
        + m * (c123 * r3 * r3 * r4 + c123 * r1 * r3 * r3 - c123 * r1 * r3 * r4)
        + l * c123 * r1 * r2 * r4
        + c123 * r1 * r3 * r4;
    ([0.0, b1, b2, b3], [1.0, a1, a2, a3])
}

/// Third-order IIR from an analogue `(b, a)` by the bilinear transform.
pub fn bilinear3(b: [f64; 4], a: [f64; 4], sample_rate: f64) -> ([f64; 4], [f64; 4]) {
    const P: [[f64; 4]; 4] = [[1.0, 3.0, 3.0, 1.0], [1.0, 1.0, -1.0, -1.0], [1.0, -1.0, -1.0, 1.0], [1.0, -3.0, 3.0, -1.0]];
    let c = 2.0 * sample_rate;
    let (mut bz, mut az) = ([0.0; 4], [0.0; 4]);
    for k in 0..4 {
        let ck = c.powi(k as i32);
        for j in 0..4 {
            bz[j] += b[k] * ck * P[k][j];
            az[j] += a[k] * ck * P[k][j];
        }
    }
    let a0 = az[0];
    (bz.map(|v| v / a0), az.map(|v| v / a0))
}

/// `|H(e^{jω})|` in dB of a third-order digital filter.
pub fn iir3_db(b: &[f64; 4], a: &[f64; 4], freq: f64, sample_rate: f64) -> f64 {
    let w = std::f64::consts::TAU * freq / sample_rate;
    let eval = |c: &[f64; 4]| {
        let (mut re, mut im) = (0.0, 0.0);
        for (k, v) in c.iter().enumerate() {
            re += v * (k as f64 * w).cos();
            im -= v * (k as f64 * w).sin();
        }
        re * re + im * im
    };
    10.0 * (eval(b) / eval(a).max(1e-300)).max(1e-30).log10()
}

/// Bass pot taper (audio/log): the stack's bass response is mostly in the
/// top half of a linear pot.
fn bass_taper(l: f32) -> f64 {
    (l.clamp(0.0, 1.0) as f64).powi(2)
}

/// Bassman coefficients for the knobs (0..1) at `sample_rate`.
pub fn tone_stack(bass: f32, mid: f32, treble: f32, sample_rate: f64) -> ([f64; 4], [f64; 4]) {
    let (b, a) = bassman_analog(treble.clamp(0.0, 1.0) as f64, mid.clamp(0.0, 1.0) as f64, bass_taper(bass));
    bilinear3(b, a, sample_rate)
}

/// Cabinet filters: resonant high-pass, low resonance, presence, two
/// speaker low-passes.
pub fn cab_coeffs(cab: usize, sample_rate: f64) -> [SvfCoeffs; 5] {
    // (hp f, hp q, res f, res dB, presence f, presence dB, lp1, lp2)
    let spec = match cab {
        1 => (70.0, 0.8, 120.0, 3.0, 2_200.0, 3.0, 5_500.0, 7_000.0),
        2 => (80.0, 0.9, 110.0, 4.0, 1_800.0, 2.5, 5_000.0, 6_500.0),
        3 => (90.0, 1.1, 100.0, 5.0, 2_800.0, 2.0, 4_200.0, 5_500.0),
        _ => return [SvfCoeffs::passthrough(); 5],
    };
    let (hp, hq, rf, rg, pf, pg, l1, l2) = spec;
    [
        SvfCoeffs::new(SvfMode::Highpass, hp, hq, 0.0, sample_rate),
        SvfCoeffs::new(SvfMode::Bell, rf, 1.0, rg, sample_rate),
        SvfCoeffs::new(SvfMode::Bell, pf, 1.2, pg, sample_rate),
        SvfCoeffs::new(SvfMode::Lowpass, l1, 0.75, 0.0, sample_rate),
        SvfCoeffs::new(SvfMode::Lowpass, l2, 0.55, 0.0, sample_rate),
    ]
}

/// Treble shelf of the Centaur mode (±12 dB at 1.6 kHz).
fn centaur_shelf(tone: f32, sample_rate: f64) -> SvfCoeffs {
    SvfCoeffs::new(SvfMode::HighShelf, 1_600.0, 0.6, (tone.clamp(0.0, 1.0) - 0.5) * 24.0, sample_rate)
}

/// Fixed low-pass corner of the Screamer tone section and its treble gain.
const TS_TONE_HZ: f32 = 723.0;
fn ts_treble(tone: f32) -> f32 {
    0.15 + 1.1 * tone.clamp(0.0, 1.0)
}
/// Fuzz tone: low-pass and high-pass corners of the blend.
const FUZZ_LP_HZ: f32 = 400.0;
const FUZZ_HP_HZ: f32 = 1_200.0;

/// One-pole low-pass magnitude² (analogue prototype, for the UI curve).
fn lp1_mag2(f: f64, fc: f64) -> f64 {
    1.0 / (1.0 + (f / fc).powi(2))
}

/// Magnitude (dB) of a mode's tone section plus the cabinet at `freq`
/// (the editor's response curve).
#[allow(clippy::too_many_arguments)]
pub fn tone_db(m: usize, tone: f32, bass: f32, mid: f32, treble: f32, cab: usize, freq: f64, sample_rate: f64) -> f32 {
    let t = match m {
        mode::CENTAUR => centaur_shelf(tone, sample_rate).magnitude_db(freq, sample_rate),
        mode::SCREAMER => {
            let lp = lp1_mag2(freq, TS_TONE_HZ as f64);
            // lp + t·hp with hp = 1 − lp (complex): |1 + (t − 1)·jf/fc / (1 + jf/fc)|… approximate by magnitudes.
            let x = freq / TS_TONE_HZ as f64;
            let tr = ts_treble(tone) as f64;
            let (re, im) = (1.0, tr * x);
            10.0 * ((re * re + im * im) * lp).log10()
        }
        mode::FUZZ => {
            let x_lp = freq / FUZZ_LP_HZ as f64;
            let x_hp = freq / FUZZ_HP_HZ as f64;
            let t = tone.clamp(0.0, 1.0) as f64;
            // (1 − t)/(1 + j x_lp) + t · j x_hp / (1 + j x_hp)
            let d1 = 1.0 + x_lp * x_lp;
            let (r1, i1) = ((1.0 - t) / d1, -(1.0 - t) * x_lp / d1);
            let d2 = 1.0 + x_hp * x_hp;
            let (r2, i2) = (t * x_hp * x_hp / d2, t * x_hp / d2);
            let (re, im) = (r1 + r2, i1 + i2);
            10.0 * (re * re + im * im).max(1e-30).log10()
        }
        _ => {
            let (b, a) = tone_stack(bass, mid, treble, sample_rate);
            iir3_db(&b, &a, freq, sample_rate) + AMP_MAKEUP_DB as f64
        }
    };
    let cab_db: f64 = cab_coeffs(cab, sample_rate).iter().map(|c| c.magnitude_db(freq, sample_rate)).sum();
    (t + cab_db) as f32
}

/// The Bassman stack loses −2…−12 dB at its centre settings (−11.5 dB at
/// 500 Hz); part of it is made up here.
const AMP_MAKEUP_DB: f32 = 6.0;

// ───────────────────────────── Plugin ─────────────────────────────

/// Third-order transposed direct form II, `f64`.
#[derive(Clone, Copy, Debug, Default)]
struct Iir3 {
    b: [f64; 4],
    a: [f64; 4],
    s: [f64; 3],
}

impl Iir3 {
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let y = self.b[0] * x + self.s[0];
        self.s[0] = self.b[1] * x - self.a[1] * y + self.s[1];
        self.s[1] = self.b[2] * x - self.a[2] * y + self.s[2];
        self.s[2] = self.b[3] * x - self.a[3] * y;
        if !y.is_finite() {
            self.s = [0.0; 3];
            return 0.0;
        }
        for v in &mut self.s {
            if v.abs() < 1e-30 {
                *v = 0.0;
            }
        }
        y as f32
    }
}

#[derive(Clone, Copy)]
struct Channel {
    os: Oversampler,
    pre: OnePole,
    clip: DiodeClipper,
    tube: [Adaa; 2],
    couple: OnePole,
    dc: OnePole,
    tone_a: OnePole,
    tone_b: OnePole,
    shelf: Svf,
    stack: Iir3,
    cab: [Svf; 5],
}

impl Channel {
    fn new() -> Self {
        Self {
            os: Oversampler::new(2),
            pre: OnePole::default(),
            clip: DiodeClipper::new(DiodePairKind::Germanium.pair()),
            tube: [Adaa::new(Curve::Tube, 0.3); 2],
            couple: OnePole::default(),
            dc: OnePole::default(),
            tone_a: OnePole::default(),
            tone_b: OnePole::default(),
            shelf: Svf::default(),
            stack: Iir3::default(),
            cab: [Svf::default(); 5],
        }
    }

    fn reset(&mut self) {
        self.os.reset();
        self.pre.reset();
        self.clip.reset();
        for t in &mut self.tube {
            t.reset();
        }
        self.couple.reset();
        self.dc.reset();
        self.tone_a.reset();
        self.tone_b.reset();
        self.shelf.reset();
        self.stack.s = [0.0; 3];
        for c in &mut self.cab {
            c.reset();
        }
    }
}

/// Resolved per-block settings.
#[derive(Clone, Copy)]
struct Set {
    mode: usize,
    stage: Stage,
    tube_norm: f32,
    tube_g2: f32,
    ts_treble: f32,
    tone: f32,
    makeup: f32,
}

pub struct Amp {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    ch: [Channel; 2],
    dry_delay: [DelayLine; 2],
    set: Set,
    input: Smoother,
    wet: Smoother,
    dry: Smoother,
    factor: usize,
    latency: usize,
    mode: usize,
    peaks: [f32; 3],
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Amp::new())
}

impl Default for Amp {
    fn default() -> Self {
        Self::new()
    }
}

impl Amp {
    pub fn new() -> Self {
        let params = params();
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Amp",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["distortion", "overdrive", "fuzz", "amp", "guitar", "cabinet"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            ch: [Channel::new(); 2],
            dry_delay: [DelayLine::new(16), DelayLine::new(16)],
            set: Set { mode: 0, stage: stage(0, 0.4, 0.5), tube_norm: 1.0, tube_g2: 1.0, ts_treble: 1.0, tone: 0.5, makeup: 1.0 },
            input: Smoother::new(1.0),
            wet: Smoother::new(1.0),
            dry: Smoother::new(0.0),
            factor: 0,
            latency: 0,
            mode: usize::MAX,
            peaks: [0.0; 3],
            first: true,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let factor = if self.params.choice(p::OVERSAMPLING) == 0 { 2 } else { 4 };
        if factor != self.factor {
            self.factor = factor;
            for c in &mut self.ch {
                c.os.set_factor(factor);
            }
            self.latency = self.ch[0].os.latency().round() as usize;
        }
        let osr = self.sr * factor as f64;
        let m = self.params.choice(p::MODE).min(3);
        let pct = |id| self.params.plain(id) / 100.0;
        let (gain, tone, bias) = (pct(p::GAIN), pct(p::TONE), pct(p::BIAS));
        let st = stage(m, gain, bias);
        let k = tube_bias(bias);
        let tube_norm = (1.0 / Curve::Tube.slope0(k)) as f32;
        let tube_g2 = db_to_gain(6.0 + 18.0 * gain);
        let (b, a) = tone_stack(pct(p::BASS), pct(p::MID), pct(p::TREBLE), self.sr);
        let cab = cab_coeffs(self.params.choice(p::CAB).min(3), self.sr);
        let shelf = centaur_shelf(tone, self.sr);
        let mode_changed = m != self.mode;
        self.mode = m;
        for c in &mut self.ch {
            if mode_changed {
                c.reset();
                c.os.set_factor(factor);
            }
            c.pre.set_freq(st.pre_hp, osr);
            c.clip.diodes = st.diodes.pair();
            if st.r > 0.0 {
                c.clip.set(st.r, st.c, osr);
            }
            for t in &mut c.tube {
                t.set(Curve::Tube, k);
            }
            c.couple.set_freq(20.0, osr);
            c.dc.set_freq(8.0, self.sr);
            let (fa, fb) = match m {
                mode::SCREAMER => (TS_TONE_HZ, TS_TONE_HZ),
                _ => (FUZZ_LP_HZ, FUZZ_HP_HZ),
            };
            c.tone_a.set_freq(fa, self.sr);
            c.tone_b.set_freq(fb, self.sr);
            c.shelf.coeffs = shelf;
            c.stack.b = b;
            c.stack.a = a;
            for (f, co) in c.cab.iter_mut().zip(cab) {
                f.coeffs = co;
            }
        }
        // Rough level matching between modes (0 dB in ≈ 0 dB out at 40 % gain).
        let makeup = match m {
            mode::CENTAUR => db_to_gain(-4.0),
            mode::SCREAMER => db_to_gain(-6.0),
            mode::FUZZ => db_to_gain(-8.0),
            _ => db_to_gain(AMP_MAKEUP_DB - 6.0) / (1.0 + 0.2 * tube_g2),
        };
        self.set = Set { mode: m, stage: st, tube_norm, tube_g2, ts_treble: ts_treble(tone), tone, makeup };
        self.input.set_target(db_to_gain(self.params.plain(p::INPUT)));
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry.set_target((1.0 - mix) * out);
        if self.first {
            self.input.snap_to_target();
            self.wet.snap_to_target();
            self.dry.snap_to_target();
            self.first = false;
        }
    }
}

/// The oversampled circuit of one sample; returns `(out, |diode v| · norm)`.
#[inline]
fn circuit(c: &mut Channel, s: &Set, x: f32) -> (f32, f32) {
    let st = &s.stage;
    if s.mode == mode::AMP {
        let v = c.pre.highpass(x) * st.gain;
        let v1 = c.tube[0].process(v) * s.tube_norm;
        let v1 = c.couple.highpass(v1);
        let v2 = c.tube[1].process(v1 * s.tube_g2) * s.tube_norm;
        return (v2, v2.abs());
    }
    let hp = c.pre.highpass(x);
    let drive = hp * st.gain + st.offset;
    let v = c.clip.process(drive as f64) as f32 * st.norm;
    let y = match s.mode {
        mode::SCREAMER => x + v,
        _ => (1.0 - st.blend) * x + st.blend * v,
    };
    (y, v.abs())
}

impl Segmented for Amp {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let factor = self.factor;
        let lat = self.latency;
        let set = self.set;
        let decay = (-1.0 / (0.3 * self.sr as f32)).exp();
        let Self { ch, dry_delay, input, wet, dry, peaks, .. } = self;
        let mut pk = *peaks;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let (l, r) = (sanitize(l), sanitize(r));
            let (g, w, d) = (input.next(), wet.next(), dry.next());
            pk[0] = (pk[0] * decay).max(l.abs().max(r.abs()));
            let mut out = [l, r];
            let mut clip = 0.0f32;
            for (ci, x) in out.iter_mut().enumerate() {
                let c = &mut ch[ci];
                let mut up = [0.0f32; 4];
                c.os.upsample(*x * g, &mut up);
                for s in up.iter_mut().take(factor) {
                    let (y, v) = circuit(c, &set, *s);
                    clip = clip.max(v);
                    *s = y;
                }
                let y = c.dc.highpass(c.os.downsample(&up));
                let y = match set.mode {
                    mode::CENTAUR => c.shelf.process(y),
                    mode::SCREAMER => {
                        let (lp, hp) = c.tone_a.tick(y);
                        lp + set.ts_treble * hp
                    }
                    mode::FUZZ => (1.0 - set.tone) * c.tone_a.lowpass(y) + set.tone * c.tone_b.highpass(y),
                    _ => c.stack.process(y),
                };
                let mut y = y * set.makeup;
                for f in &mut c.cab {
                    y = f.process(y);
                }
                dry_delay[ci].push(*x);
                *x = sanitize(dry_delay[ci].read(lat) * d + y * w);
            }
            pk[1] = (pk[1] * decay).max(out[0].abs().max(out[1].abs()));
            pk[2] = (pk[2] * decay).max(clip);
            (out[0], out[1])
        });
        *peaks = pk.map(|v| if v.is_finite() { v } else { 0.0 });
    }
}

impl Processor for Amp {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for s in [&mut self.input, &mut self.wet, &mut self.dry] {
            s.set_time(20.0, sample_rate);
        }
        self.dry_delay = [DelayLine::new(64), DelayLine::new(64)];
        self.factor = 0;
        self.mode = usize::MAX;
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for c in &mut self.ch {
            c.reset();
        }
        for d in &mut self.dry_delay {
            d.clear();
        }
        self.peaks = [0.0; 3];
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

    fn latency(&self) -> u32 {
        self.latency as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &self.peaks)
    }
}

#[cfg(test)]
mod tests {
    use super::super::shapes::{alias_db, assert_cpu, power};
    use super::*;
    use crate::test_util::{SR, process_stereo, rms, set_plain, sine};

    fn render(m: usize, gain: f32, amp: f32, freq: f32) -> Vec<f32> {
        let mut a = Amp::new();
        set_plain(&mut a, p::MODE, m as f32);
        set_plain(&mut a, p::GAIN, gain);
        let (l, _) = process_stereo(&mut a, sine(freq, amp), 48_000);
        l[24_000..].to_vec()
    }

    #[test]
    fn every_mode_is_finite_and_audible() {
        for m in 0..4 {
            for gain in [0.0, 50.0, 100.0] {
                let x = render(m, gain, 0.3, 220.0);
                assert!(x.iter().all(|v| v.is_finite()), "{} gain {gain}", MODES[m]);
                let level = 20.0 * rms(&x).log10();
                assert!(level > -30.0 && level < 6.0, "{} gain {gain}: {level} dBFS", MODES[m]);
            }
        }
    }

    #[test]
    fn more_gain_more_distortion() {
        for m in 0..4 {
            let thd = |gain: f32| {
                let x = render(m, gain, 0.02, 220.0);
                let h: f64 = (2..8).map(|k| power(&x, 220.0 * k as f64, SR)).sum();
                10.0 * (h / power(&x, 220.0, SR)).log10()
            };
            let (lo, hi) = (thd(0.0), thd(100.0));
            assert!(hi > lo + 6.0, "{}: THD {lo:.1} → {hi:.1} dB", MODES[m]);
            assert!(hi > -25.0, "{}: full gain THD only {hi:.1} dB", MODES[m]);
        }
    }

    #[test]
    fn fuzz_bias_moves_the_even_harmonics() {
        let h2 = |bias: f32| {
            let mut a = Amp::new();
            set_plain(&mut a, p::MODE, mode::FUZZ as f32);
            set_plain(&mut a, p::GAIN, 30.0);
            set_plain(&mut a, p::BIAS, bias);
            let (l, _) = process_stereo(&mut a, sine(220.0, 0.2), 48_000);
            let x = &l[24_000..];
            10.0 * (power(x, 440.0, SR) / power(x, 220.0, SR)).log10()
        };
        assert!((h2(90.0) - h2(50.0)).abs() > 3.0, "{} vs {}", h2(90.0), h2(50.0));
    }

    #[test]
    fn bassman_stack_is_stable_and_responds() {
        let sr = 48_000.0;
        for (t, m, l) in [(0.0, 0.0, 0.0), (0.5, 0.5, 0.5), (1.0, 1.0, 1.0), (0.2, 0.9, 0.7)] {
            let (b, a) = tone_stack(l, m, t, sr);
            // Poles inside the unit circle: the impulse response decays.
            let mut f = Iir3 { b, a, s: [0.0; 3] };
            let mut tail = 0.0f64;
            for i in 0..48_000 {
                let y = f.process(if i == 0 { 1.0 } else { 0.0 });
                if i > 40_000 {
                    tail += (y as f64).abs();
                }
            }
            assert!(tail < 1e-6, "t {t} m {m} l {l}: tail {tail}");
        }
        let at = |bass: f32, mid: f32, treble: f32, f: f64| {
            let (b, a) = tone_stack(bass, mid, treble, sr);
            iir3_db(&b, &a, f, sr)
        };
        // Each knob lifts its own region.
        assert!(at(1.0, 0.5, 0.5, 80.0) > at(0.0, 0.5, 0.5, 80.0) + 6.0);
        // (The real stack's mid control is gentle: ~5.5 dB at 500 Hz.)
        assert!(at(0.5, 1.0, 0.5, 500.0) > at(0.5, 0.0, 0.5, 500.0) + 4.0);
        assert!(at(0.5, 0.5, 1.0, 5_000.0) > at(0.5, 0.5, 0.0, 5_000.0) + 6.0);
        // The classic mid scoop at the centre settings.
        assert!(at(0.5, 0.5, 0.5, 500.0) < at(0.5, 0.5, 0.5, 80.0).max(at(0.5, 0.5, 0.5, 4_000.0)));
    }

    #[test]
    fn screamer_hump_keeps_the_lows_clean() {
        // The gain stage only amplifies above ~720 Hz: a 60 Hz tone barely
        // distorts at a gain that crushes a 1 kHz one.
        let thd = |f: f32| {
            let x = render(mode::SCREAMER, 60.0, 0.1, f);
            let h: f64 = (2..6).map(|k| power(&x, (f * k as f32) as f64, SR)).sum();
            10.0 * (h / power(&x, f as f64, SR)).log10()
        };
        assert!(thd(1000.0) > thd(60.0) + 10.0, "1 kHz {} vs 60 Hz {}", thd(1000.0), thd(60.0));
    }

    #[test]
    fn cabinet_rolls_off_the_fizz() {
        let sr = 48_000.0;
        for cab in 1..4 {
            assert!(tone_db(mode::CENTAUR, 0.5, 0.5, 0.5, 0.5, cab, 10_000.0, sr) < -15.0);
            assert!(tone_db(mode::CENTAUR, 0.5, 0.5, 0.5, 0.5, cab, 30.0, sr) < -6.0);
            assert!(tone_db(mode::CENTAUR, 0.5, 0.5, 0.5, 0.5, cab, 800.0, sr).abs() < 4.0);
        }
        assert!(tone_db(mode::CENTAUR, 0.5, 0.5, 0.5, 0.5, 0, 10_000.0, sr).abs() < 0.5);
    }

    #[test]
    fn transfer_curve_is_odd_and_bounded() {
        // (The Amp mode's triodes are biased: asymmetric by design.)
        for m in [mode::CENTAUR, mode::SCREAMER] {
            for x in [0.05f32, 0.3, 1.0] {
                let (a, b) = (transfer(m, 0.6, 0.5, x), transfer(m, 0.6, 0.5, -x));
                assert!((a + b).abs() < 0.05 * a.abs().max(0.01), "{}: f({x}) {a} vs f(−x) {b}", MODES[m]);
                assert!(a.abs() < 4.0);
            }
        }
        assert_eq!(transfer(mode::FUZZ, 0.5, 0.5, 0.0), 0.0);
    }

    #[test]
    fn low_aliasing_at_4x() {
        for m in 0..4 {
            let mut a = Amp::new();
            set_plain(&mut a, p::MODE, m as f32);
            set_plain(&mut a, p::GAIN, 70.0);
            set_plain(&mut a, p::OVERSAMPLING, 1.0);
            let (l, _) = process_stereo(&mut a, sine(2_987.0, 0.3), 3 << 14);
            let al = alias_db(&l[1 << 15..], 2_987.0, SR);
            assert!(al < -30.0, "{}: aliases {al:.1} dB", MODES[m]);
        }
    }

    #[test]
    fn garbage_in_is_survived() {
        let mut a = Amp::new();
        set_plain(&mut a, p::MODE, mode::FUZZ as f32);
        let (l, _) = process_stereo(&mut a, |i| if i % 3 == 0 { (f32::NAN, 1e9) } else { (-1e9, f32::INFINITY) }, 4800);
        assert!(l.iter().all(|v| v.is_finite()));
        let (l, _) = process_stereo(&mut a, sine(220.0, 0.3), 4800);
        assert!(l.iter().all(|v| v.is_finite()) && rms(&l[2400..]) > 1e-3);
    }

    #[test]
    fn cpu_budget() {
        for m in 0..4 {
            let mut a = Amp::new();
            set_plain(&mut a, p::MODE, m as f32);
            set_plain(&mut a, p::OVERSAMPLING, 1.0);
            set_plain(&mut a, p::CAB, 3.0);
            assert_cpu(&mut a, 4.0);
        }
    }
}
