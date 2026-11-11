//! `fx.eq` — 8-band parametric / dynamic EQ with per-band channel routing.
//!
//! Band shapes: bell, low/high shelf, low/high cut (6–96 dB/oct Butterworth
//! cascades), notch, band-pass and a first-order tilt. Each band can work on
//! stereo, mid, side, left or right, and can be made dynamic (it then acts as
//! a compressor on its own band: above the threshold the band's gain moves
//! down by the computed reduction).
//!
//! Processing is zero latency. In **Matched** mode static bells, shelves and
//! band-passes use magnitude-matched biquads (no cramping near Nyquist);
//! dynamic bands, cuts, notches and tilt always run on TPT SVFs, which stay
//! clean under modulation. **Classic** mode runs every band on SVFs. Cut
//! bands ignore Q (pure Butterworth). Parameter moves glide (log-frequency,
//! gain and Q smoothing, coefficients refreshed every 16 samples).
//!
//! Linear-phase processing is not implemented yet.
//!
//! Meter layout (265 values):
//! * `[0..128)`   EQ curve in dB at 128 log-spaced points, 20 Hz – 20 kHz;
//! * `[128..256)` post-EQ spectrum in dB, same points;
//! * `[256..264)` per-band dynamic gain change in dB (≤ 0, 0 when static);
//! * `[264]`      auto-gain currently applied, dB.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::biquad::{Biquad32, BiquadCoeffs, BiquadShape, BiquadState};
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower, gain_computer};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::spectrum::SpectrumAnalyzer;
use aitrack_dsp::svf::{OnePole, SvfCoeffs, SvfMode, SvfState, butterworth_q};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.eq";

pub const BANDS: usize = 8;
/// Parameters per band; band `b`'s parameter `x` has id `b * BAND_STRIDE + x`.
pub const BAND_STRIDE: u32 = 12;
/// Points of the curve / spectrum in `meter()`.
pub const CURVE_POINTS: usize = 128;

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
    pub const MODE: u32 = 98;

    /// Id of `param` in band `band`.
    pub const fn band(band: usize, param: u32) -> u32 {
        band as u32 * super::BAND_STRIDE + param
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

const OFF_ON: &[&str] = &["Off", "On"];
const TYPES: &[&str] = &["Bell", "Low Shelf", "High Shelf", "Low Cut", "High Cut", "Notch", "Band Pass", "Tilt"];
const SLOPES: &[&str] = &["6 dB/oct", "12 dB/oct", "18 dB/oct", "24 dB/oct", "36 dB/oct", "48 dB/oct", "72 dB/oct", "96 dB/oct"];
const SLOPE_ORDER: [usize; 8] = [1, 2, 3, 4, 6, 8, 12, 16];
const CHANNELS: &[&str] = &["Stereo", "Mid", "Side", "Left", "Right"];
const MODES: &[&str] = &["Matched", "Classic"];

const DEFAULT_FREQS: [f32; BANDS] = [30.0, 100.0, 250.0, 800.0, 2_000.0, 5_000.0, 10_000.0, 16_000.0];
const DEFAULT_TYPES: [f32; BANDS] = [ty::LOW_CUT, ty::LOW_SHELF, ty::BELL, ty::BELL, ty::BELL, ty::BELL, ty::HIGH_SHELF, ty::HIGH_CUT];

const NAMES: [[&str; 12]; BANDS] = band_names();

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

const SHORTS: [&str; 12] = ["On", "Type", "Freq", "Gain", "Q", "Slope", "Chan", "Dyn", "Thresh", "Ratio", "Att", "Rel"];

pub fn params() -> Vec<ParamInfo> {
    let mut v = Vec::with_capacity(BANDS * 12 + 3);
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
    v
}

/// Frames between coefficient refreshes while gliding or dynamic.
const CONTROL: usize = 16;
/// Samples between curve refreshes while something moves.
const CURVE_INTERVAL: usize = 2048;
/// Output clamp (+36 dBFS).
const OVERLOAD: f32 = 64.0;
/// Most a dynamic band can pull its gain down.
const MAX_REDUCTION_DB: f32 = 24.0;

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
    log_freq: Smoother,
    gain: Smoother,
    q: Smoother,
    env: EnvelopeFollower,
    det: SvfCoeffs,
    reduction: f32,
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
            log_freq: Smoother::new(10.0),
            gain: Smoother::new(0.0),
            q: Smoother::new(0.707),
            env: EnvelopeFollower::new(DetectorMode::Peak),
            det: SvfCoeffs::default(),
            reduction: 0.0,
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

    /// Recompute coefficients from the smoothed values.
    fn design(&mut self, matched: bool, sr: f64) {
        let f = self.freq();
        let q = self.q.value();
        let g = self.gain.value() - if self.dynamic { self.reduction } else { 0.0 };
        let static_ok = matched && !self.dynamic;
        let prev = self.engine;
        let shape = match self.ty {
            0 => Some((BiquadShape::Bell, SvfMode::Bell)),
            1 => Some((BiquadShape::LowShelf, SvfMode::LowShelf)),
            2 => Some((BiquadShape::HighShelf, SvfMode::HighShelf)),
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
            Engine::Svf => st.svf.process(&self.svf, x),
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

    /// Process one frame through this band.
    #[inline]
    fn frame(&mut self, l: f32, r: f32) -> (f32, f32) {
        match self.chan {
            1 | 2 => {
                let m = 0.5 * (l + r);
                let s = 0.5 * (l - r);
                if self.chan == 1 {
                    if self.dynamic {
                        let d = self.detect(0, m);
                        self.env.process(d);
                    }
                    let m = self.tick(0, m);
                    (m + s, m - s)
                } else {
                    if self.dynamic {
                        let d = self.detect(1, s);
                        self.env.process(d);
                    }
                    let s = self.tick(1, s);
                    (m + s, m - s)
                }
            }
            3 => {
                if self.dynamic {
                    let d = self.detect(0, l);
                    self.env.process(d);
                }
                (self.tick(0, l), r)
            }
            4 => {
                if self.dynamic {
                    let d = self.detect(1, r);
                    self.env.process(d);
                }
                (l, self.tick(1, r))
            }
            _ => {
                if self.dynamic {
                    let d = self.detect(0, l).max(self.detect(1, r));
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
            Engine::Svf => self.svf.response(f, sr),
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
    if n % 2 == 0 {
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

fn curve_freq(i: usize) -> f64 {
    20.0 * 1000f64.powf(i as f64 / (CURVE_POINTS - 1) as f64)
}

pub struct Eq {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    bands: [Band; BANDS],
    matched: bool,
    output: Smoother,
    auto_gain: bool,
    auto_db: f32,
    curve: [f32; CURVE_POINTS],
    curve_dirty: bool,
    auto_dirty: bool,
    curve_countdown: usize,
    spectrum: SpectrumAnalyzer,
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
        let mut e = Self {
            info: PluginInfo {
                id: ID,
                name: "EQ",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["eq", "filter", "dynamic", "mid-side"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            bands: [Band::new(); BANDS],
            matched: true,
            output: Smoother::new(1.0),
            auto_gain: false,
            auto_db: 0.0,
            curve: [0.0; CURVE_POINTS],
            curve_dirty: true,
            auto_dirty: true,
            curve_countdown: 0,
            spectrum: SpectrumAnalyzer::new(4096, CURVE_POINTS, 20.0, 20_000.0, 48_000.0),
            first: true,
        };
        e.prepare(48_000.0, 512);
        e
    }

    /// Pull parameter values into the bands (targets only).
    fn update_params(&mut self) {
        let ps = &self.params;
        self.matched = ps.choice(p::MODE) == 0;
        for (b, band) in self.bands.iter_mut().enumerate() {
            let id = |x: u32| p::band(b, x);
            let on = ps.on(id(p::ON));
            let ty = ps.choice(id(p::TYPE)).min(7);
            let order = SLOPE_ORDER[ps.choice(id(p::SLOPE)).min(7)];
            let chan = ps.choice(id(p::CHANNEL)).min(4);
            let dynamic = ps.on(id(p::DYNAMIC)) && matches!(ty, 0 | 1 | 2 | 6);
            if on != band.on || ty != band.ty || order != band.order || chan != band.chan || dynamic != band.dynamic {
                band.stale = true;
            }
            if on && !band.on {
                band.st = [ChState::default(); 2];
            }
            band.on = on;
            band.ty = ty;
            band.order = order;
            band.chan = chan;
            if dynamic != band.dynamic {
                band.reduction = 0.0;
                band.env.reset();
            }
            band.dynamic = dynamic;
            band.thr = ps.plain(id(p::THRESHOLD));
            band.ratio = ps.plain(id(p::RATIO));
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
    }

    /// Advance smoothers / dynamics by `n` samples and refresh what moved.
    fn control(&mut self, n: usize) {
        let sr = self.sr;
        let matched = self.matched;
        let mut moved = false;
        for band in self.bands.iter_mut().filter(|b| b.on) {
            let mut redo = band.stale;
            if band.moving() {
                band.log_freq.advance(n);
                band.gain.advance(n);
                band.q.advance(n);
                redo = true;
            }
            if band.dynamic {
                let lvl = gain_to_db(band.env.value());
                let red = gain_computer(lvl, band.thr, band.ratio, 6.0).min(MAX_REDUCTION_DB);
                if (red - band.reduction).abs() > 1e-3 {
                    band.reduction = red;
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

    fn compute_curve(&mut self) {
        let sr = self.sr;
        let mut weighted = 0.0f64;
        let mut wsum = 0.0f64;
        for i in 0..CURVE_POINTS {
            let f = curve_freq(i);
            let mut acc = (1.0, 0.0);
            for b in self.bands.iter().filter(|b| b.on) {
                acc = cmul(acc, b.response(f, sr));
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
        let mut s = start;
        while s < end {
            let e = (s + CONTROL).min(end);
            self.control(e - s);
            let bands = &mut self.bands;
            let output = &mut self.output;
            let spectrum = &mut self.spectrum;
            for_frames(ctx.buffer, s, e, |_, mut l, mut r| {
                for band in bands.iter_mut() {
                    if band.on {
                        (l, r) = band.frame(l, r);
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
        self.spectrum = SpectrumAnalyzer::new(4096, CURVE_POINTS, 20.0, 20_000.0, sample_rate);
        self.first = true;
        self.params.mark_dirty();
        self.update_params();
        for b in &mut self.bands {
            b.design(self.matched, sample_rate);
        }
        self.compute_curve();
        self.output.snap(db_to_gain(self.params.plain(p::OUTPUT) + if self.auto_gain { self.auto_db } else { 0.0 }));
        self.reset();
    }

    fn reset(&mut self) {
        for b in &mut self.bands {
            b.st = [ChState::default(); 2];
            b.env.reset();
            b.reduction = 0.0;
            b.stale = true;
        }
        self.spectrum.reset();
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
                *d = -b.reduction;
            }
        }
        o = write_meter(out, o, &dynamic);
        write_meter(out, o, &[if self.auto_gain { self.auto_db } else { 0.0 }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{process_stereo, rms, set_plain, sine};
    use aitrack_dsp::biquad::analog_magnitude_db;

    fn band(eq: &mut Eq, b: usize, t: f32, f: f32, g: f32, q: f32) {
        set_plain(eq, p::band(b, p::ON), 1.0);
        set_plain(eq, p::band(b, p::TYPE), t);
        set_plain(eq, p::band(b, p::FREQ), f);
        set_plain(eq, p::band(b, p::GAIN), g);
        set_plain(eq, p::band(b, p::Q), q);
    }

    fn gain_at(eq: &mut Eq, f: f32) -> f32 {
        let n = 24_000;
        let (l, _) = process_stereo(eq, sine(f, 0.25), n);
        let x: Vec<f32> = (0..n).map(|i| sine(f, 0.25)(i).0).collect();
        20.0 * (rms(&l[n / 2..]) / rms(&x[n / 2..])).log10()
    }

    #[test]
    fn matched_bell_follows_analog() {
        for &(f0, g, q) in &[(1000.0f32, 6.0f32, 1.0f32), (16_000.0, 9.0, 1.2)] {
            let mut eq = Eq::new();
            band(&mut eq, 2, ty::BELL, f0, g, q);
            for f in [200.0, f0 * 0.7, f0, (f0 * 1.25).min(21_000.0), 21_000.0] {
                let m = gain_at(&mut eq, f);
                let a = analog_magnitude_db(BiquadShape::Bell, f as f64, f0 as f64, q as f64, g as f64) as f32;
                assert!((m - a).abs() < 1.0, "bell {f0}: at {f} measured {m} analog {a}");
            }
        }
    }

    #[test]
    fn shelves_and_cuts() {
        let mut eq = Eq::new();
        band(&mut eq, 1, ty::LOW_SHELF, 200.0, -6.0, 0.707);
        assert!((gain_at(&mut eq, 30.0) + 6.0).abs() < 0.3);
        assert!(gain_at(&mut eq, 5000.0).abs() < 0.1);

        let mut eq = Eq::new();
        band(&mut eq, 0, ty::LOW_CUT, 100.0, 0.0, 0.707);
        set_plain(&mut eq, p::band(0, p::SLOPE), slope::DB12);
        assert!((gain_at(&mut eq, 100.0) + 3.01).abs() < 0.2);

        // 96 dB/oct: order-16 Butterworth.
        let mut eq = Eq::new();
        band(&mut eq, 7, ty::HIGH_CUT, 1000.0, 0.0, 0.707);
        set_plain(&mut eq, p::band(7, p::SLOPE), slope::DB96);
        assert!((gain_at(&mut eq, 1000.0) + 3.01).abs() < 0.3);
        let at = gain_at(&mut eq, 1400.0);
        let want = -10.0 * (1.0 + 1.4f32.powi(32)).log10();
        assert!((at - want).abs() < 1.5, "96 dB/oct at 1.4 kHz: {at} vs {want}");
        assert!(gain_at(&mut eq, 500.0).abs() < 0.1);

        // 18 dB/oct uses the first-order section too.
        let mut eq = Eq::new();
        band(&mut eq, 0, ty::LOW_CUT, 200.0, 0.0, 0.707);
        set_plain(&mut eq, p::band(0, p::SLOPE), slope::DB18);
        assert!((gain_at(&mut eq, 200.0) + 3.01).abs() < 0.3);
        let at = gain_at(&mut eq, 100.0);
        assert!((at + 18.1).abs() < 0.6, "{at}");
    }

    #[test]
    fn curve_matches_measurement() {
        let mut eq = Eq::new();
        band(&mut eq, 3, ty::BELL, 800.0, -9.0, 2.0);
        band(&mut eq, 6, ty::TILT, 1000.0, 6.0, 0.7);
        process_stereo(&mut eq, |_| (0.0, 0.0), 4096);
        for i in [10usize, 50, 64, 100] {
            let f = Eq::curve_frequency(i);
            let c = eq.curve()[i];
            let m = gain_at(&mut eq, f);
            assert!((c - m).abs() < 0.2, "point {i} ({f} Hz): curve {c} measured {m}");
        }
    }

    #[test]
    fn side_band_leaves_mono_untouched() {
        let mut eq = Eq::new();
        band(&mut eq, 3, ty::BELL, 1000.0, 12.0, 1.0);
        set_plain(&mut eq, p::band(3, p::CHANNEL), ch::SIDE);
        let (l, r) = process_stereo(&mut eq, sine(1000.0, 0.3), 9600);
        let x: Vec<f32> = (0..9600).map(|i| sine(1000.0, 0.3)(i).0).collect();
        for i in 4800..9600 {
            assert!((l[i] - x[i]).abs() < 1e-4 && (r[i] - x[i]).abs() < 1e-4);
        }
        // Anti-phase content is the side: it gets the boost.
        let (l, _) = process_stereo(&mut eq, |i| {
            let (a, _) = sine(1000.0, 0.1)(i);
            (a, -a)
        }, 9600);
        assert!(20.0 * (rms(&l[4800..]) / (0.1 / 2f32.sqrt())).log10() > 11.0);
    }

    #[test]
    fn dynamic_band_compresses_loud_input() {
        let mut eq = Eq::new();
        band(&mut eq, 3, ty::BELL, 1000.0, 0.0, 1.0);
        set_plain(&mut eq, p::band(3, p::DYNAMIC), 1.0);
        set_plain(&mut eq, p::band(3, p::THRESHOLD), -30.0);
        set_plain(&mut eq, p::band(3, p::RATIO), 4.0);
        set_plain(&mut eq, p::band(3, p::ATTACK), 1.0);
        let loud = gain_at(&mut eq, 1000.0); // −12 dBFS sine: 18 dB over
        assert!(loud < -6.0, "loud {loud}");
        let mut eq2 = Eq::new();
        band(&mut eq2, 3, ty::BELL, 1000.0, 0.0, 1.0);
        set_plain(&mut eq2, p::band(3, p::DYNAMIC), 1.0);
        set_plain(&mut eq2, p::band(3, p::THRESHOLD), -6.0);
        assert!(gain_at(&mut eq2, 1000.0).abs() < 0.2);
        let mut m = vec![0.0; 300];
        eq.meter(&mut m);
        assert!(m[256 + 3] < -6.0);
    }

    #[test]
    fn auto_gain_compensates() {
        let mut eq = Eq::new();
        band(&mut eq, 4, ty::BELL, 2000.0, 12.0, 0.7);
        set_plain(&mut eq, p::AUTO_GAIN, 1.0);
        process_stereo(&mut eq, |_| (0.0, 0.0), 1024);
        let mut m = vec![0.0; 300];
        assert_eq!(eq.meter(&mut m), 265);
        assert!(m[264] < -1.0, "auto gain {}", m[264]);
        // Curve peak is the boost.
        assert!((m[..128].iter().cloned().fold(f32::MIN, f32::max) - 12.0).abs() < 0.5);
    }

    #[test]
    fn spectrum_shows_signal() {
        let mut eq = Eq::new();
        process_stereo(&mut eq, sine(1000.0, 0.5), 16_384);
        let mut m = vec![0.0; 300];
        eq.meter(&mut m);
        let spec = &m[128..256];
        let (i, _) = spec.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
        assert!((Eq::curve_frequency(i) / 1000.0 - 1.0).abs() < 0.1);
    }
}
