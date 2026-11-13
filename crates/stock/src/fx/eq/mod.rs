//! `fx.eq` — 8-band parametric / dynamic EQ with per-band channel routing
//! and three phase modes.
//!
//! Band shapes: bell, low/high shelf, low/high cut (6–96 dB/oct Butterworth
//! cascades), notch, band-pass and a first-order tilt. Each band can work on
//! stereo, mid, side, left or right, and bells, shelves and band-passes can
//! be made dynamic: a band-filtered detector (its own input, or the external
//! sidechain) drives a gain change of up to `Dyn Range` dB in one of four
//! directions — cut above the threshold (de-essing, resonance taming), boost
//! above (upward expansion), cut below (downward expansion), boost below
//! (upward compression) — with its own threshold, ratio, knee and times.
//!
//! Phase modes (`Phase`):
//! * **Minimum** — zero latency IIR. In **Matched** processing static bells,
//!   shelves and band-passes are magnitude-matched biquads (Vicanek), no
//!   cramping near Nyquist; dynamic bands, cuts, notches and tilt run on TPT
//!   SVFs, which stay clean under modulation. **Classic** runs every band on
//!   SVFs.
//! * **Natural** — the analogue prototypes' magnitude *and* phase, sampled to
//!   Nyquist and run as an FIR (see [`fir`]): no cramping and the phase of an
//!   analogue EQ, for `b + D_nat` samples of latency (512 at 48 kHz).
//! * **Linear** — the analogue magnitudes with zero phase, centred in the FIR
//!   (`b + n/2` samples: 2304 at 48 kHz, 4096 taps).
//!
//! In the FIR modes the static part of every band (gain, shape, routing) is
//! in the FIR as a full 2×2 L/R matrix; a dynamic band's moving gain change
//! runs as a minimum-phase SVF after it (detected on the delayed signal, the
//! sidechain delayed to match), so dynamics stay sample-accurate without
//! redesigning the FIR every few samples. Static changes redesign the FIR at
//! most every 2048 samples and crossfade in over one 256-frame block.
//! `latency()` reports the current mode's latency.
//!
//! Parameter moves glide (log-frequency, gain and Q smoothing, coefficients
//! refreshed every 16 samples).
//!
//! Meter layout (315 values):
//! * `[0..128)`   EQ curve in dB at 128 log-spaced points, 20 Hz – 20 kHz;
//! * `[128..256)` post-EQ spectrum in dB, same points;
//! * `[256..264)` per-band dynamic gain change in dB (signed, 0 when static);
//! * `[264]`      auto-gain currently applied, dB;
//! * `[265]`      latency in samples;
//! * `[266]`      1 when an external sidechain is feeding the EQ;
//! * `[267..315)` sidechain spectrum in dB at 48 log-spaced points (the
//!   collision display compares it with the post-EQ spectrum).

pub mod analog;
pub mod fir;

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, sidechain_frame, write_meter};
use aitrack_dsp::biquad::{Biquad32, BiquadCoeffs, BiquadShape, BiquadState};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower, gain_computer};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::spectrum::SpectrumAnalyzer;
use aitrack_dsp::svf::{OnePole, SvfCoeffs, SvfMode, SvfState, butterworth_q};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};
use fir::{FirEngine, Window};
use realfft::num_complex::Complex;

pub const ID: &str = "fx.eq";

pub const BANDS: usize = 8;
/// Parameters per band; band `b`'s parameter `x` has id `b * BAND_STRIDE + x`.
pub const BAND_STRIDE: u32 = 12;
/// Points of the curve / spectrum in `meter()`.
pub const CURVE_POINTS: usize = 128;
/// Points of the sidechain spectrum in `meter()`.
pub const SC_POINTS: usize = 48;
/// Where the meter fields start.
pub mod meter {
    pub const CURVE: usize = 0;
    pub const SPECTRUM: usize = 128;
    pub const DYNAMIC: usize = 256;
    pub const AUTO_GAIN: usize = 264;
    pub const LATENCY: usize = 265;
    pub const SC_ACTIVE: usize = 266;
    pub const SC_SPECTRUM: usize = 267;
    pub const LEN: usize = 267 + super::SC_POINTS;
}

pub mod p {
    //! Per-band offsets (add `band * BAND_STRIDE`) and global ids.
    pub const ON: u32 = 0;
    pub const TYPE: u32 = 1;
    pub const FREQ: u32 = 2;
    pub const GAIN: u32 = 3;
    pub const Q: u32 = 4;
    pub const SLOPE: u32 = 5;
    pub const CHANNEL: u32 = 6;
    pub const DYNAMIC: u32 = 7;
    pub const THRESHOLD: u32 = 8;
    pub const RATIO: u32 = 9;
    pub const ATTACK: u32 = 10;
    pub const RELEASE: u32 = 11;

    pub const OUTPUT: u32 = 96;
    pub const AUTO_GAIN: u32 = 97;
    /// Minimum-phase flavour: Matched / Classic.
    pub const MODE: u32 = 98;
    /// Minimum / Natural / Linear.
    pub const PHASE: u32 = 99;
    /// Internal / External: whether bands set to listen to the sidechain do.
    pub const SIDECHAIN: u32 = 100;

    /// Id of `param` in band `band`.
    pub const fn band(band: usize, param: u32) -> u32 {
        band as u32 * super::BAND_STRIDE + param
    }
}

/// The second block of per-band ids (dynamics), added after the first
/// release: band `b`'s `x` is `BASE + b * STRIDE + x`.
pub mod dynx {
    pub const BASE: u32 = 104;
    pub const STRIDE: u32 = 8;
    /// Cut Above / Boost Above / Cut Below / Boost Below.
    pub const MODE: u32 = 0;
    /// Most gain change, dB.
    pub const RANGE: u32 = 1;
    /// Internal / External detector.
    pub const SOURCE: u32 = 2;
    pub const KNEE: u32 = 3;

    pub const fn band(band: usize, param: u32) -> u32 {
        BASE + band as u32 * STRIDE + param
    }
}

/// Band types (index = `TYPE` value).
pub mod ty {
    pub const BELL: f32 = 0.0;
    pub const LOW_SHELF: f32 = 1.0;
    pub const HIGH_SHELF: f32 = 2.0;
    pub const LOW_CUT: f32 = 3.0;
    pub const HIGH_CUT: f32 = 4.0;
    pub const NOTCH: f32 = 5.0;
    pub const BAND_PASS: f32 = 6.0;
    pub const TILT: f32 = 7.0;
}

/// Slope choices (index = `SLOPE` value): 6, 12, 18, 24, 36, 48, 72, 96 dB/oct.
pub mod slope {
    pub const DB6: f32 = 0.0;
    pub const DB12: f32 = 1.0;
    pub const DB18: f32 = 2.0;
    pub const DB24: f32 = 3.0;
    pub const DB36: f32 = 4.0;
    pub const DB48: f32 = 5.0;
    pub const DB72: f32 = 6.0;
    pub const DB96: f32 = 7.0;
}

/// Channel choices (index = `CHANNEL` value).
pub mod ch {
    pub const STEREO: f32 = 0.0;
    pub const MID: f32 = 1.0;
    pub const SIDE: f32 = 2.0;
    pub const LEFT: f32 = 3.0;
    pub const RIGHT: f32 = 4.0;
}

/// Phase choices (index = `PHASE` value).
pub mod phase {
    pub const MINIMUM: f32 = 0.0;
    pub const NATURAL: f32 = 1.0;
    pub const LINEAR: f32 = 2.0;
}

/// Dynamic directions (index = `dynx::MODE` value).
pub mod dir {
    pub const CUT_ABOVE: f32 = 0.0;
    pub const BOOST_ABOVE: f32 = 1.0;
    pub const CUT_BELOW: f32 = 2.0;
    pub const BOOST_BELOW: f32 = 3.0;
}

const OFF_ON: &[&str] = &["Off", "On"];
const TYPES: &[&str] = &["Bell", "Low Shelf", "High Shelf", "Low Cut", "High Cut", "Notch", "Band Pass", "Tilt"];
const SLOPES: &[&str] = &["6 dB/oct", "12 dB/oct", "18 dB/oct", "24 dB/oct", "36 dB/oct", "48 dB/oct", "72 dB/oct", "96 dB/oct"];
const SLOPE_ORDER: [usize; 8] = [1, 2, 3, 4, 6, 8, 12, 16];
const CHANNELS: &[&str] = &["Stereo", "Mid", "Side", "Left", "Right"];
const MODES: &[&str] = &["Matched", "Classic"];
pub const PHASES: &[&str] = &["Minimum", "Natural", "Linear"];
pub const DIRECTIONS: &[&str] = &["Cut Above", "Boost Above", "Cut Below", "Boost Below"];
const SOURCES: &[&str] = &["Internal", "External"];

const DEFAULT_FREQS: [f32; BANDS] = [30.0, 100.0, 250.0, 800.0, 2_000.0, 5_000.0, 10_000.0, 16_000.0];
const DEFAULT_TYPES: [f32; BANDS] = [ty::LOW_CUT, ty::LOW_SHELF, ty::BELL, ty::BELL, ty::BELL, ty::BELL, ty::HIGH_SHELF, ty::HIGH_CUT];

const NAMES: [[&str; 12]; BANDS] = band_names();
const DYN_NAMES: [[&str; 4]; BANDS] = dyn_names();

const fn band_names() -> [[&'static str; 12]; BANDS] {
    macro_rules! band {
        ($n:literal) => {
            [
                concat!("Band ", $n, " On"),
                concat!("Band ", $n, " Type"),
                concat!("Band ", $n, " Freq"),
                concat!("Band ", $n, " Gain"),
                concat!("Band ", $n, " Q"),
                concat!("Band ", $n, " Slope"),
                concat!("Band ", $n, " Channel"),
                concat!("Band ", $n, " Dynamic"),
                concat!("Band ", $n, " Threshold"),
                concat!("Band ", $n, " Ratio"),
                concat!("Band ", $n, " Attack"),
                concat!("Band ", $n, " Release"),
            ]
        };
    }
    [band!("1"), band!("2"), band!("3"), band!("4"), band!("5"), band!("6"), band!("7"), band!("8")]
}

const fn dyn_names() -> [[&'static str; 4]; BANDS] {
    macro_rules! band {
        ($n:literal) => {
            [concat!("Band ", $n, " Dyn Mode"), concat!("Band ", $n, " Dyn Range"), concat!("Band ", $n, " Dyn Source"), concat!("Band ", $n, " Dyn Knee")]
        };
    }
    [band!("1"), band!("2"), band!("3"), band!("4"), band!("5"), band!("6"), band!("7"), band!("8")]
}

const SHORTS: [&str; 12] = ["On", "Type", "Freq", "Gain", "Q", "Slope", "Chan", "Dyn", "Thresh", "Ratio", "Att", "Rel"];
const DYN_SHORTS: [&str; 4] = ["Dir", "Range", "Source", "Knee"];

pub fn params() -> Vec<ParamInfo> {
    let mut v = Vec::with_capacity(BANDS * 16 + 5);
    for b in 0..BANDS {
        let n = &NAMES[b];
        let id = |x: u32| p::band(b, x);
        v.push(ParamInfo::new(id(p::ON), n[0], 0.0, 1.0, 0.0).choices(OFF_ON).short(SHORTS[0]));
        v.push(ParamInfo::new(id(p::TYPE), n[1], 0.0, 7.0, DEFAULT_TYPES[b]).choices(TYPES).short(SHORTS[1]));
        v.push(ParamInfo::new(id(p::FREQ), n[2], 20.0, 20_000.0, DEFAULT_FREQS[b]).unit("Hz").log().short(SHORTS[2]));
        v.push(ParamInfo::new(id(p::GAIN), n[3], -24.0, 24.0, 0.0).unit("dB").short(SHORTS[3]));
        v.push(ParamInfo::new(id(p::Q), n[4], 0.1, 18.0, 0.707).log().short(SHORTS[4]));
        v.push(ParamInfo::new(id(p::SLOPE), n[5], 0.0, 7.0, slope::DB24).choices(SLOPES).short(SHORTS[5]));
        v.push(ParamInfo::new(id(p::CHANNEL), n[6], 0.0, 4.0, ch::STEREO).choices(CHANNELS).short(SHORTS[6]));
        v.push(ParamInfo::new(id(p::DYNAMIC), n[7], 0.0, 1.0, 0.0).choices(OFF_ON).short(SHORTS[7]));
        v.push(ParamInfo::new(id(p::THRESHOLD), n[8], -60.0, 0.0, -24.0).unit("dB").short(SHORTS[8]));
        v.push(ParamInfo::new(id(p::RATIO), n[9], 1.0, 20.0, 2.0).log().short(SHORTS[9]));
        v.push(ParamInfo::new(id(p::ATTACK), n[10], 0.1, 200.0, 10.0).unit("ms").log().short(SHORTS[10]));
        v.push(ParamInfo::new(id(p::RELEASE), n[11], 5.0, 2_000.0, 150.0).unit("ms").log().short(SHORTS[11]));
    }
    v.push(ParamInfo::new(p::OUTPUT, "Output", -24.0, 24.0, 0.0).unit("dB").short("Out"));
    v.push(ParamInfo::new(p::AUTO_GAIN, "Auto Gain", 0.0, 1.0, 0.0).choices(OFF_ON).short("Auto"));
    v.push(ParamInfo::new(p::MODE, "Processing", 0.0, 1.0, 0.0).choices(MODES).short("Mode"));
    v.push(ParamInfo::new(p::PHASE, "Phase", 0.0, 2.0, phase::MINIMUM).choices(PHASES));
    v.push(ParamInfo::new(p::SIDECHAIN, "Sidechain", 0.0, 1.0, 0.0).choices(SOURCES).short("SC"));
    for b in 0..BANDS {
        let n = &DYN_NAMES[b];
        let id = |x: u32| dynx::band(b, x);
        v.push(ParamInfo::new(id(dynx::MODE), n[0], 0.0, 3.0, dir::CUT_ABOVE).choices(DIRECTIONS).short(DYN_SHORTS[0]));
        v.push(ParamInfo::new(id(dynx::RANGE), n[1], 0.0, 24.0, 24.0).unit("dB").short(DYN_SHORTS[1]));
        v.push(ParamInfo::new(id(dynx::SOURCE), n[2], 0.0, 1.0, 0.0).choices(SOURCES).short(DYN_SHORTS[2]));
        v.push(ParamInfo::new(id(dynx::KNEE), n[3], 0.0, 24.0, 6.0).unit("dB").short(DYN_SHORTS[3]));
    }
    v
}

/// Frames between coefficient refreshes while gliding or dynamic.
const CONTROL: usize = 16;
/// Samples between curve refreshes while something moves.
const CURVE_INTERVAL: usize = 2048;
/// Samples between FIR redesigns while something moves.
const FIR_INTERVAL: usize = 2048;
/// FIR partition (and crossfade) length.
const FIR_BLOCK: usize = 256;
/// Output clamp (+36 dBFS).
const OVERLOAD: f32 = 64.0;

/// FIR length, natural-phase pre-delay and partition for a sample rate:
/// 4096 taps up to 50 kHz, 8192 up to 100 kHz, 16384 above (same span in
/// time, ≈ 85 ms).
pub fn fir_geometry(sample_rate: f64) -> (usize, usize, usize) {
    let scale = if sample_rate <= 50_000.0 { 1 } else if sample_rate <= 100_000.0 { 2 } else { 4 };
    (4096 * scale, FIR_BLOCK * scale, FIR_BLOCK)
}

/// Latency of a phase mode (`PHASE` index) at a sample rate, in samples.
pub fn mode_latency(phase: usize, sample_rate: f64) -> u32 {
    let (n, nat, b) = fir_geometry(sample_rate);
    match phase {
        1 => (b + nat) as u32,
        2 => (b + n / 2) as u32,
        _ => 0,
    }
}

/// Signed dynamic gain change (dB) for a detector level, per direction.
pub fn dynamic_gain(direction: usize, level_db: f32, threshold: f32, ratio: f32, knee: f32, range: f32) -> f32 {
    let amount = match direction {
        0 | 1 => gain_computer(level_db, threshold, ratio, knee),
        // Below: mirror the level around the threshold.
        _ => gain_computer(2.0 * threshold - level_db, threshold, ratio, knee),
    }
    .clamp(0.0, range.max(0.0));
    if direction.is_multiple_of(2) { -amount } else { amount }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Engine {
    Biquad,
    Svf,
    Cut,
    Tilt,
}

#[derive(Clone, Copy, Debug, Default)]
struct ChState {
    bq: BiquadState,
    svf: SvfState,
    cut: [SvfState; 8],
    op: OnePole,
    det: SvfState,
}

#[derive(Clone, Copy, Debug)]
struct Band {
    on: bool,
    ty: usize,
    order: usize,
    chan: usize,
    dynamic: bool,
    thr: f32,
    ratio: f32,
    direction: usize,
    range: f32,
    knee: f32,
    external: bool,
    /// The static part runs in the FIR; this band's filter carries only the
    /// dynamic gain change.
    fir: bool,
    log_freq: Smoother,
    gain: Smoother,
    q: Smoother,
    env: EnvelopeFollower,
    det: SvfCoeffs,
    /// Signed dynamic gain change, dB.
    delta: f32,
    /// Output gain of a dynamic band-pass.
    out_gain: f32,
    engine: Engine,
    bq64: BiquadCoeffs,
    bq: Biquad32,
    svf: SvfCoeffs,
    cut: [SvfCoeffs; 8],
    n_cut2: usize,
    cut_first: bool,
    high_cut: bool,
    tilt_gl: f32,
    tilt_gh: f32,
    st: [ChState; 2],
    stale: bool,
}

impl Band {
    fn new() -> Self {
        Self {
            on: false,
            ty: 0,
            order: 4,
            chan: 0,
            dynamic: false,
            thr: -24.0,
            ratio: 2.0,
            direction: 0,
            range: 24.0,
            knee: 6.0,
            external: false,
            fir: false,
            log_freq: Smoother::new(10.0),
            gain: Smoother::new(0.0),
            q: Smoother::new(0.707),
            env: EnvelopeFollower::new(DetectorMode::Peak),
            det: SvfCoeffs::default(),
            delta: 0.0,
            out_gain: 1.0,
            engine: Engine::Svf,
            bq64: BiquadCoeffs::identity(),
            bq: Biquad32::default(),
            svf: SvfCoeffs::passthrough(),
            cut: [SvfCoeffs::passthrough(); 8],
            n_cut2: 0,
            cut_first: false,
            high_cut: false,
            tilt_gl: 1.0,
            tilt_gh: 1.0,
            st: [ChState::default(); 2],
            stale: true,
        }
    }

    fn moving(&self) -> bool {
        !(self.log_freq.is_settled() && self.gain.is_settled() && self.q.is_settled())
    }

    fn freq(&self) -> f32 {
        self.log_freq.value().exp2()
    }

    /// Whether the per-sample path runs this band.
    fn runs(&self) -> bool {
        self.on && (!self.fir || self.dynamic)
    }

    /// Analogue prototype of the static part (FIR design, FIR-mode curve).
    fn proto(&self, with_dynamics: bool) -> analog::Proto {
        let mut g = self.gain.value();
        if with_dynamics && self.dynamic && self.ty != 6 {
            g += self.delta;
        }
        analog::Proto::new(self.ty, self.freq() as f64, g as f64, self.q.value() as f64, self.order)
    }

    /// Recompute coefficients from the smoothed values.
    fn design(&mut self, matched: bool, sr: f64) {
        let f = self.freq();
        let q = self.q.value();
        let delta = if self.dynamic { self.delta } else { 0.0 };
        let g = if self.fir { delta } else { self.gain.value() + delta };
        let static_ok = matched && !self.dynamic && !self.fir;
        let prev = self.engine;
        self.out_gain = 1.0;
        let shape = match self.ty {
            0 => Some((BiquadShape::Bell, SvfMode::Bell)),
            1 => Some((BiquadShape::LowShelf, SvfMode::LowShelf)),
            2 => Some((BiquadShape::HighShelf, SvfMode::HighShelf)),
            // In FIR mode a dynamic band-pass moves a bell at its place.
            6 if self.fir => Some((BiquadShape::Bell, SvfMode::Bell)),
            6 => Some((BiquadShape::Bandpass, SvfMode::Bandpass)),
            _ => None,
        };
        match (self.ty, shape) {
            (_, Some((bs, sm))) => {
                if static_ok {
                    self.bq64 = BiquadCoeffs::matched(bs, f as f64, q as f64, g as f64, sr);
                    self.bq = self.bq64.into();
                    self.engine = Engine::Biquad;
                } else {
                    self.svf = SvfCoeffs::new(sm, f, q, g, sr);
                    self.engine = Engine::Svf;
                    if self.ty == 6 && !self.fir {
                        // A dynamic band-pass turns its whole output up or down.
                        self.out_gain = db_to_gain(delta);
                    }
                }
            }
            (5, _) => {
                self.svf = SvfCoeffs::new(SvfMode::Notch, f, q, 0.0, sr);
                self.engine = Engine::Svf;
            }
            (3 | 4, _) => {
                self.high_cut = self.ty == 4;
                let n = self.order;
                self.n_cut2 = n / 2;
                self.cut_first = n % 2 == 1;
                let mode = if self.high_cut { SvfMode::Lowpass } else { SvfMode::Highpass };
                for k in 0..self.n_cut2 {
                    self.cut[k] = SvfCoeffs::new(mode, f, bw_q(n, k), 0.0, sr);
                }
                for c in &mut self.st {
                    c.op.set_freq(f, sr);
                }
                self.engine = Engine::Cut;
            }
            _ => {
                // Tilt: ±half the gain either side of the pivot.
                for c in &mut self.st {
                    c.op.set_freq(f, sr);
                }
                self.tilt_gl = db_to_gain(-0.5 * g);
                self.tilt_gh = db_to_gain(0.5 * g);
                self.engine = Engine::Tilt;
            }
        }
        if self.engine != prev {
            for c in &mut self.st {
                c.bq.reset();
                c.svf.reset();
                c.cut = [SvfState::default(); 8];
            }
        }
        if self.dynamic {
            let dq = if self.ty == 0 || self.ty == 6 { q.clamp(0.3, 8.0) } else { 0.7 };
            self.det = SvfCoeffs::new(SvfMode::Bandpass, f, dq, 0.0, sr);
        }
        self.stale = false;
    }

    #[inline]
    fn tick(&mut self, c: usize, x: f32) -> f32 {
        let st = &mut self.st[c];
        match self.engine {
            Engine::Biquad => st.bq.process(&self.bq, x),
            Engine::Svf => st.svf.process(&self.svf, x) * self.out_gain,
            Engine::Cut => {
                let mut y = x;
                for k in 0..self.n_cut2 {
                    y = st.cut[k].process(&self.cut[k], y);
                }
                if self.cut_first {
                    let (lp, hp) = st.op.tick(y);
                    y = if self.high_cut { lp } else { hp };
                }
                y
            }
            Engine::Tilt => {
                let (lp, hp) = st.op.tick(x);
                self.tilt_gl * lp + self.tilt_gh * hp
            }
        }
    }

    #[inline]
    fn detect(&mut self, c: usize, x: f32) -> f32 {
        self.st[c].det.process(&self.det, x).abs()
    }

    /// Process one frame through this band; `ext` is the sidechain frame
    /// (`None` when there is none).
    #[inline]
    fn frame(&mut self, l: f32, r: f32, ext: Option<(f32, f32)>) -> (f32, f32) {
        let (dl, dr) = match ext {
            Some(e) if self.external => e,
            _ => (l, r),
        };
        match self.chan {
            1 | 2 => {
                let m = 0.5 * (l + r);
                let s = 0.5 * (l - r);
                if self.chan == 1 {
                    if self.dynamic {
                        let d = self.detect(0, 0.5 * (dl + dr));
                        self.env.process(d);
                    }
                    let m = self.tick(0, m);
                    (m + s, m - s)
                } else {
                    if self.dynamic {
                        let d = self.detect(1, 0.5 * (dl - dr));
                        self.env.process(d);
                    }
                    let s = self.tick(1, s);
                    (m + s, m - s)
                }
            }
            3 => {
                if self.dynamic {
                    let d = self.detect(0, dl);
                    self.env.process(d);
                }
                (self.tick(0, l), r)
            }
            4 => {
                if self.dynamic {
                    let d = self.detect(1, dr);
                    self.env.process(d);
                }
                (l, self.tick(1, r))
            }
            _ => {
                if self.dynamic {
                    let d = self.detect(0, dl).max(self.detect(1, dr));
                    self.env.process(d);
                }
                (self.tick(0, l), self.tick(1, r))
            }
        }
    }

    /// Complex response of the band as designed now.
    fn response(&self, f: f64, sr: f64) -> (f64, f64) {
        match self.engine {
            Engine::Biquad => self.bq64.response(f, sr),
            Engine::Svf => {
                let r = self.svf.response(f, sr);
                let g = self.out_gain as f64;
                (r.0 * g, r.1 * g)
            }
            Engine::Cut => {
                let mut acc = (1.0, 0.0);
                for k in 0..self.n_cut2 {
                    acc = cmul(acc, self.cut[k].response(f, sr));
                }
                if self.cut_first {
                    let lp = self.st[0].op.lowpass_response(f, sr);
                    let r = if self.high_cut { lp } else { (1.0 - lp.0, -lp.1) };
                    acc = cmul(acc, r);
                }
                acc
            }
            Engine::Tilt => {
                let lp = self.st[0].op.lowpass_response(f, sr);
                let (gl, gh) = (self.tilt_gl as f64, self.tilt_gh as f64);
                (gl * lp.0 + gh * (1.0 - lp.0), gl * lp.1 - gh * lp.1)
            }
        }
    }
}

/// Butterworth Q of 2nd-order section `k` of an order-`n` cascade. Odd
/// orders put their complex pairs at `θ = π (k + 1) / n` (the real pole is
/// the first-order section).
fn bw_q(n: usize, k: usize) -> f32 {
    if n.is_multiple_of(2) {
        butterworth_q(n, k)
    } else {
        let theta = std::f64::consts::PI * (k + 1) as f64 / n as f64;
        (1.0 / (2.0 * theta.cos())) as f32
    }
}

#[inline]
fn cmul(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

type Mat = [(f64, f64); 4];

/// `a · b` for 2×2 complex matrices stored `[00, 01, 10, 11]`.
#[inline]
fn mat_mul(a: &Mat, b: &Mat) -> Mat {
    let add = |x: (f64, f64), y: (f64, f64)| (x.0 + y.0, x.1 + y.1);
    [
        add(cmul(a[0], b[0]), cmul(a[1], b[2])),
        add(cmul(a[0], b[1]), cmul(a[1], b[3])),
        add(cmul(a[2], b[0]), cmul(a[3], b[2])),
        add(cmul(a[2], b[1]), cmul(a[3], b[3])),
    ]
}

/// The L/R matrix of a band with response `h` on channel `chan`.
#[inline]
fn band_matrix(chan: usize, h: (f64, f64)) -> Mat {
    let one = (1.0, 0.0);
    let zero = (0.0, 0.0);
    match chan {
        // Mid: L' = L (h+1)/2 + R (h−1)/2, R' = L (h−1)/2 + R (h+1)/2.
        1 => {
            let a = (0.5 * (h.0 + 1.0), 0.5 * h.1);
            let b = (0.5 * (h.0 - 1.0), 0.5 * h.1);
            [a, b, b, a]
        }
        // Side: L' = L (1+h)/2 + R (1−h)/2, R' = L (1−h)/2 + R (1+h)/2.
        2 => {
            let a = (0.5 * (1.0 + h.0), 0.5 * h.1);
            let b = (0.5 * (1.0 - h.0), -0.5 * h.1);
            [a, b, b, a]
        }
        3 => [h, zero, zero, one],
        4 => [one, zero, zero, h],
        _ => [h, zero, zero, h],
    }
}

fn curve_freq(i: usize) -> f64 {
    20.0 * 1000f64.powf(i as f64 / (CURVE_POINTS - 1) as f64)
}

pub struct Eq {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    bands: [Band; BANDS],
    matched: bool,
    phase: usize,
    sidechain: bool,
    output: Smoother,
    auto_gain: bool,
    auto_db: f32,
    curve: [f32; CURVE_POINTS],
    curve_dirty: bool,
    auto_dirty: bool,
    curve_countdown: usize,
    spectrum: SpectrumAnalyzer,
    sc_spectrum: SpectrumAnalyzer,
    sc_active: bool,
    fir: FirEngine,
    fir_dirty: bool,
    fir_immediate: bool,
    fir_countdown: usize,
    nat_delay: usize,
    sc_delay: [DelayLine; 2],
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Eq::new())
}

impl Default for Eq {
    fn default() -> Self {
        Self::new()
    }
}

impl Eq {
    pub fn new() -> Self {
        let params = params();
        let (n, nat, b) = fir_geometry(48_000.0);
        let mut e = Self {
            info: PluginInfo {
                id: ID,
                name: "EQ",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["eq", "filter", "dynamic", "mid-side", "linear-phase"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: true,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            bands: [Band::new(); BANDS],
            matched: true,
            phase: 0,
            sidechain: false,
            output: Smoother::new(1.0),
            auto_gain: false,
            auto_db: 0.0,
            curve: [0.0; CURVE_POINTS],
            curve_dirty: true,
            auto_dirty: true,
            curve_countdown: 0,
            spectrum: SpectrumAnalyzer::new(4096, CURVE_POINTS, 20.0, 20_000.0, 48_000.0),
            sc_spectrum: SpectrumAnalyzer::new(4096, SC_POINTS, 20.0, 20_000.0, 48_000.0),
            sc_active: false,
            fir: FirEngine::new(n, b),
            fir_dirty: true,
            fir_immediate: true,
            fir_countdown: 0,
            nat_delay: nat,
            sc_delay: [DelayLine::new(1), DelayLine::new(1)],
            first: true,
        };
        e.prepare(48_000.0, 512);
        e
    }

    fn fir_mode(&self) -> bool {
        self.phase > 0
    }

    /// Pull parameter values into the bands (targets only).
    fn update_params(&mut self) {
        let ps = &self.params;
        self.matched = ps.choice(p::MODE) == 0;
        let phase = ps.choice(p::PHASE).min(2);
        if phase != self.phase {
            // A new latency: start the FIR from silence with the new design.
            self.fir.reset();
            for d in &mut self.sc_delay {
                d.clear();
            }
            self.fir_immediate = true;
        }
        self.phase = phase;
        self.sidechain = ps.choice(p::SIDECHAIN) == 1;
        let fir = phase > 0;
        for (b, band) in self.bands.iter_mut().enumerate() {
            let id = |x: u32| p::band(b, x);
            let xid = |x: u32| dynx::band(b, x);
            let on = ps.on(id(p::ON));
            let ty = ps.choice(id(p::TYPE)).min(7);
            let order = SLOPE_ORDER[ps.choice(id(p::SLOPE)).min(7)];
            let chan = ps.choice(id(p::CHANNEL)).min(4);
            let dynamic = ps.on(id(p::DYNAMIC)) && matches!(ty, 0 | 1 | 2 | 6);
            if on && !band.on {
                band.st = [ChState::default(); 2];
            }
            if fir != band.fir {
                band.st = [ChState::default(); 2];
            }
            band.on = on;
            band.ty = ty;
            band.order = order;
            band.chan = chan;
            band.fir = fir;
            if dynamic != band.dynamic {
                band.delta = 0.0;
                band.env.reset();
            }
            band.dynamic = dynamic;
            band.thr = ps.plain(id(p::THRESHOLD));
            band.ratio = ps.plain(id(p::RATIO));
            band.direction = ps.choice(xid(dynx::MODE)).min(3);
            band.range = ps.plain(xid(dynx::RANGE));
            band.knee = ps.plain(xid(dynx::KNEE));
            band.external = ps.choice(xid(dynx::SOURCE)) == 1;
            band.env.set_times(ps.plain(id(p::ATTACK)), ps.plain(id(p::RELEASE)), self.sr);
            band.log_freq.set_target(ps.plain(id(p::FREQ)).log2());
            band.gain.set_target(ps.plain(id(p::GAIN)));
            band.q.set_target(ps.plain(id(p::Q)));
            if self.first || !on {
                band.log_freq.snap_to_target();
                band.gain.snap_to_target();
                band.q.snap_to_target();
            }
            band.stale = true;
        }
        self.auto_gain = ps.on(p::AUTO_GAIN);
        self.first = false;
        self.curve_dirty = true;
        self.auto_dirty = true;
        self.curve_countdown = 0;
        self.fir_dirty = true;
    }

    /// Advance smoothers / dynamics by `n` samples and refresh what moved.
    fn control(&mut self, n: usize) {
        let sr = self.sr;
        let matched = self.matched;
        let mut moved = false;
        let mut static_moved = false;
        for band in self.bands.iter_mut().filter(|b| b.on) {
            let mut redo = band.stale;
            if band.moving() {
                band.log_freq.advance(n);
                band.gain.advance(n);
                band.q.advance(n);
                redo = true;
                static_moved = true;
            }
            if band.dynamic {
                let lvl = gain_to_db(band.env.value());
                let d = dynamic_gain(band.direction, lvl, band.thr, band.ratio, band.knee, band.range);
                if (d - band.delta).abs() > 1e-3 {
                    band.delta = d;
                    redo = true;
                }
            }
            if redo {
                band.design(matched, sr);
                moved = true;
            }
        }
        if moved {
            self.curve_dirty = true;
        }
        if static_moved {
            self.fir_dirty = true;
        }
        if self.fir_mode() {
            self.fir_countdown = self.fir_countdown.saturating_sub(n);
            if self.fir_immediate || (self.fir_dirty && self.fir_countdown == 0 && !self.fir.fading()) {
                let immediate = self.fir_immediate;
                if self.design_fir(immediate) {
                    self.fir_dirty = false;
                    self.fir_immediate = false;
                    self.fir_countdown = FIR_INTERVAL;
                }
            }
        }
        let out_db = self.params.plain(p::OUTPUT) + if self.auto_gain { self.auto_db } else { 0.0 };
        self.output.set_target(db_to_gain(out_db));
        if self.curve_dirty {
            if self.curve_countdown == 0 {
                self.compute_curve();
                self.curve_countdown = CURVE_INTERVAL;
            } else {
                self.curve_countdown = self.curve_countdown.saturating_sub(n);
            }
        } else {
            self.curve_countdown = self.curve_countdown.saturating_sub(n);
        }
    }

    /// Design the FIR of the Natural / Linear mode from the analogue
    /// prototypes of the enabled bands (static gains). Allocation free.
    fn design_fir(&mut self, immediate: bool) -> bool {
        if !immediate && self.fir.fading() {
            return false;
        }
        let linear = self.phase == 2;
        let n = self.fir.taps();
        let sr = self.sr;
        let delay = if linear { n / 2 } else { self.nat_delay };
        let mut protos = [(0usize, analog::Proto::new(0, 1000.0, 0.0, 1.0, 2)); BANDS];
        let mut count = 0;
        let mut cross = false;
        for b in self.bands.iter().filter(|b| b.on) {
            protos[count] = (b.chan, b.proto(false));
            count += 1;
            cross |= matches!(b.chan, 1 | 2);
        }
        let protos = &protos[..count];
        let w = std::f64::consts::TAU * delay as f64 / n as f64;
        for k in 0..=n / 2 {
            let f = k as f64 * sr / n as f64;
            let mut m: Mat = [(1.0, 0.0), (0.0, 0.0), (0.0, 0.0), (1.0, 0.0)];
            for (chan, proto) in protos {
                let mut h = proto.at(f);
                if linear {
                    h = ((h.0 * h.0 + h.1 * h.1).sqrt(), 0.0);
                }
                m = mat_mul(&band_matrix(*chan, h), &m);
            }
            let z = ((w * k as f64).cos(), -(w * k as f64).sin());
            for (e, v) in m.iter().enumerate() {
                let c = cmul(*v, z);
                self.fir.spec[e][k] = Complex::new(c.0 as f32, c.1 as f32);
            }
        }
        self.fir.used = [true, cross, cross, true];
        let window = if linear { Window::Symmetric { taper: 0.25 } } else { Window::Causal { pre: delay, tail: 0.125 } };
        self.fir.design(window, immediate)
    }

    fn compute_curve(&mut self) {
        let sr = self.sr;
        let fir = self.fir_mode();
        let mut protos = [analog::Proto::new(0, 1000.0, 0.0, 1.0, 2); BANDS];
        let mut count = 0;
        if fir {
            for b in self.bands.iter().filter(|b| b.on) {
                protos[count] = b.proto(true);
                count += 1;
            }
        }
        let mut weighted = 0.0f64;
        let mut wsum = 0.0f64;
        for i in 0..CURVE_POINTS {
            let f = curve_freq(i);
            let mut acc = (1.0, 0.0);
            if fir {
                for pr in &protos[..count] {
                    acc = cmul(acc, pr.at(f));
                }
            } else {
                for b in self.bands.iter().filter(|b| b.on) {
                    acc = cmul(acc, b.response(f, sr));
                }
            }
            let db = 10.0 * (acc.0 * acc.0 + acc.1 * acc.1).max(1e-30).log10();
            self.curve[i] = db.max(-120.0) as f32;
            // Loudness-ish weighting: full weight 150 Hz – 6 kHz.
            let w = if (150.0..=6_000.0).contains(&f) { 1.0 } else { 0.35 };
            weighted += w * db.clamp(-40.0, 40.0);
            wsum += w;
        }
        if self.auto_dirty {
            self.auto_db = (-(weighted / wsum) as f32).clamp(-24.0, 24.0);
            self.auto_dirty = false;
        }
        self.curve_dirty = false;
    }

    /// Current EQ curve (dB at the 128 meter points).
    pub fn curve(&self) -> &[f32; CURVE_POINTS] {
        &self.curve
    }

    /// Frequency of curve point `i`.
    pub fn curve_frequency(i: usize) -> f32 {
        curve_freq(i) as f32
    }

    /// Frequency of sidechain-spectrum point `i`.
    pub fn sc_frequency(i: usize) -> f32 {
        (20.0 * 1000f64.powf(i as f64 / (SC_POINTS - 1) as f64)) as f32
    }
}

impl Segmented for Eq {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update_params();
        }
        let sc = if self.sidechain { ctx.sidechain } else { None };
        self.sc_active = sc.is_some();
        let fir_mode = self.fir_mode();
        let lat = mode_latency(self.phase, self.sr) as usize;
        let mut s = start;
        while s < end {
            let e = (s + CONTROL).min(end);
            self.control(e - s);
            let bands = &mut self.bands;
            let output = &mut self.output;
            let spectrum = &mut self.spectrum;
            let sc_spectrum = &mut self.sc_spectrum;
            let fir = &mut self.fir;
            let sc_delay = &mut self.sc_delay;
            for_frames(ctx.buffer, s, e, |n, mut l, mut r| {
                let mut ext = None;
                if sc.is_some() {
                    let (a, b) = sidechain_frame(sc, n);
                    sc_spectrum.push_sample(0.5 * (a + b));
                    ext = Some(if fir_mode {
                        sc_delay[0].push(a);
                        sc_delay[1].push(b);
                        (sc_delay[0].read(lat), sc_delay[1].read(lat))
                    } else {
                        (a, b)
                    });
                }
                if fir_mode {
                    (l, r) = fir.tick(l, r);
                }
                for band in bands.iter_mut() {
                    if band.runs() {
                        (l, r) = band.frame(l, r, ext);
                    }
                }
                let g = output.next();
                // Overload guard: eight stacked +24 dB bands can reach
                // +200 dB; clamp at +36 dBFS so nothing absurd leaves.
                l = sanitize(l * g).clamp(-OVERLOAD, OVERLOAD);
                r = sanitize(r * g).clamp(-OVERLOAD, OVERLOAD);
                spectrum.push_sample(0.5 * (l + r));
                (l, r)
            });
            s = e;
        }
    }
}

impl Processor for Eq {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.output.set_time(20.0, sample_rate);
        for b in &mut self.bands {
            // `advance(n)` takes n per-sample steps: use the sample rate.
            b.log_freq.set_time(15.0, sample_rate);
            b.gain.set_time(15.0, sample_rate);
            b.q.set_time(15.0, sample_rate);
        }
        let (n, nat, blk) = fir_geometry(sample_rate);
        if self.fir.taps() != n || self.fir.block() != blk {
            self.fir = FirEngine::new(n, blk);
        }
        self.nat_delay = nat;
        let max_lat = mode_latency(2, sample_rate) as usize + 1;
        self.sc_delay = [DelayLine::new(max_lat), DelayLine::new(max_lat)];
        self.spectrum = SpectrumAnalyzer::new(4096, CURVE_POINTS, 20.0, 20_000.0, sample_rate);
        self.sc_spectrum = SpectrumAnalyzer::new(4096, SC_POINTS, 20.0, 20_000.0, sample_rate);
        self.first = true;
        self.params.mark_dirty();
        self.update_params();
        for b in &mut self.bands {
            b.design(self.matched, sample_rate);
        }
        self.compute_curve();
        self.output.snap(db_to_gain(self.params.plain(p::OUTPUT) + if self.auto_gain { self.auto_db } else { 0.0 }));
        self.fir_immediate = true;
        if self.fir_mode() && self.design_fir(true) {
            self.fir_immediate = false;
            self.fir_dirty = false;
        }
        self.reset();
    }

    fn reset(&mut self) {
        for b in &mut self.bands {
            b.st = [ChState::default(); 2];
            b.env.reset();
            b.delta = 0.0;
            b.stale = true;
        }
        self.fir.reset();
        for d in &mut self.sc_delay {
            d.clear();
        }
        self.spectrum.reset();
        self.sc_spectrum.reset();
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
        mode_latency(self.params.choice(p::PHASE).min(2), self.sr)
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let mut o = write_meter(out, 0, &self.curve);
        o = write_meter(out, o, self.spectrum.bands());
        let mut dynamic = [0.0f32; BANDS];
        for (d, b) in dynamic.iter_mut().zip(&self.bands) {
            if b.on && b.dynamic {
                *d = b.delta;
            }
        }
        o = write_meter(out, o, &dynamic);
        o = write_meter(out, o, &[if self.auto_gain { self.auto_db } else { 0.0 }, mode_latency(self.phase, self.sr) as f32, if self.sc_active { 1.0 } else { 0.0 }]);
        if self.sc_active {
            write_meter(out, o, self.sc_spectrum.bands())
        } else {
            write_meter(out, o, &[aitrack_dsp::spectrum::FLOOR_DB; SC_POINTS])
        }
    }
}

#[cfg(test)]
mod tests;
