//! `fx.mbdyn` — three-band dynamics with downward AND upward compression in
//! every band, the technique behind the dense, bright sound of electronic
//! bass, leads and drum buses.
//!
//! Signal path: input gain → LR4 split into low / mid / high (`Crossover3`,
//! the bands sum to a 2nd-order all-pass, so a flat magnitude with no comb
//! filtering) → per band a level detector (peak or RMS, stereo-linked) → the
//! static curve below → a gain smoother → the band gain mixed **in phase** →
//! sum → output gain.
//!
//! **Static curve** of a band, in dB, for a detector level `L`, an upper
//! threshold `Ta` (downward) and a lower threshold `Tb ≤ Ta` (upward):
//! `g = −(L − Ta)·sd` above `Ta` and `g = +(Tb − L)·su` below `Tb`, `0 dB`
//! in between. The slopes are `sd = (1 − 1/Rd)·Amount·Down %` and
//! `su = (1 − 1/Ru)·Amount·Up %`; both corners have the same quadratic knee
//! (Giannoulis, Massberg & Reiss, JAES 2012). The boost is capped at
//! *Max Up* and faded out by **Floor**: it is exactly zero at or below the
//! Floor level and reaches full strength 6 dB above it, so upward
//! compression never lifts the noise floor or the tail of a note into the
//! noise.
//!
//! **Smoothing** runs on the gain in dB: it falls with the attack time and
//! rises with the release time (so a loud hit is caught fast and the quiet
//! that follows is lifted slowly). *Time* scales every attack and release.
//!
//! **Depth** mixes per band, never the unprocessed input: with `G` the band's
//! dynamics gain times its output gain, the band is scaled by
//! `1 − d + d·G`, where `d` is the global Depth times the band's Depth. The
//! dry path is therefore the same all-pass-shifted band, and at `d = 0` the
//! output is exactly the LR4 sum of the input (input and output gains aside).
//!
//! *Linear* mode swaps the split for the complementary linear-phase FIR
//! (`LinearPhaseCrossover3`): the bands sum to the input delayed by the FIR's
//! half length, and the detectors (still IIR-split, undelayed) run that far
//! ahead of the audio. **The latency is fixed**: `latency()` always reports
//! that FIR delay, and the Minimum path delays its summed output by the same
//! amount, so changing the mode (or automating it) never changes what the
//! host compensates. There is no dry path to align: Depth mixes per band.
//! A mode change dips the output (`common::Dip`, 4 ms each way) and swaps
//! the splitter on the silent sample (then waiting out the new path's latency
//! before fading in), so it never clicks.
//! Likewise Mute / Solo, Detection (peak ↔ RMS) and Sidechain
//! (internal ↔ external) are faded or crossfaded over a few milliseconds.
//! With *Sidechain* set to External the detectors listen to the sidechain
//! bus (split with the same crossovers) instead of the input.
//!
//! The output goes through [`soft_bound`]: transparent up to +18 dBFS, then
//! a smooth shoulder towards ±64 (+36 dBFS), a last resort against the
//! stacking of the gain stages (input, Max Up, band output and output can
//! add up to +108 dB). The meter reports when it is engaged.
//!
//! Meter layout ([`METER_LEN`] values): `[0..3)` band detector level in dB
//! (held, 40 dB/s fall), `[3..6)` applied dynamics gain in dB (negative =
//! reducing, positive = lifting), `[6..9)` band output peak dBFS, `[9]`
//! input peak dBFS, `[10]` output peak dBFS, `[11]` latency in samples,
//! `[12]` 1 when the external sidechain is in effect, `[13]` 1 in linear
//! mode, `[14]` output bound engaged: 0 when idle, else how deep into the
//! shoulder the output went (0..1, held ~1 s; the editor should warn), then from `[15]` three static curves of
//! [`METER_CURVE_POINTS`] values each (low, mid, high): the band's level in
//! dB after the dynamics for input levels from [`METER_CURVE_MIN_DB`] to 0 dB
//! in equal steps (dynamics only: depth and band output gain excluded).

use crate::common::{Dip, ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, sidechain_frame, write_meter};
use aitrack_dsp::crossover::{Crossover3, LinearPhaseCrossover3};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::smooth::{Smoother, one_pole_coeff};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.mbdyn";

pub mod p {
    pub const XOVER_LOW: u32 = 0;
    pub const XOVER_HIGH: u32 = 1;
    /// Scales both slopes (0 = no dynamics).
    pub const AMOUNT: u32 = 2;
    /// Scales every attack and release.
    pub const TIME: u32 = 3;
    /// Scales the upward slope only.
    pub const UPWARD: u32 = 4;
    /// Scales the downward slope only.
    pub const DOWNWARD: u32 = 5;
    /// Peak / RMS.
    pub const DETECT: u32 = 6;
    pub const KNEE: u32 = 7;
    pub const INPUT: u32 = 8;
    pub const OUTPUT: u32 = 9;
    /// Global in-phase mix; multiplies each band's own Depth.
    pub const DEPTH: u32 = 10;
    /// Minimum / Linear split.
    pub const MODE: u32 = 11;
    /// Internal / External detector input.
    pub const SIDECHAIN: u32 = 12;
    /// First per-band id; band `b` uses `BAND0 + b * BAND_STRIDE + offset`.
    pub const BAND0: u32 = 16;
    pub const BAND_STRIDE: u32 = 16;
    pub const TH_DOWN: u32 = 0;
    pub const RATIO_DOWN: u32 = 1;
    pub const TH_UP: u32 = 2;
    pub const RATIO_UP: u32 = 3;
    pub const MAX_UP: u32 = 4;
    pub const FLOOR: u32 = 5;
    pub const ATTACK: u32 = 6;
    pub const RELEASE: u32 = 7;
    pub const BAND_OUT: u32 = 8;
    pub const BAND_DEPTH: u32 = 9;
    pub const MUTE: u32 = 10;
    pub const SOLO: u32 = 11;

    /// Id of `offset` (`TH_DOWN`, `MUTE`…) for band `b` (0 low, 1 mid, 2 high).
    pub const fn band(b: u32, offset: u32) -> u32 {
        BAND0 + b * BAND_STRIDE + offset
    }
}

/// Split modes (`MODE`).
pub mod mode {
    pub const MINIMUM: f32 = 0.0;
    pub const LINEAR: f32 = 1.0;
}

/// Number of values `meter()` writes.
pub const METER_LEN: usize = 15 + 3 * METER_CURVE_POINTS;
/// Offsets into the meter (see the module docs).
pub const METER_LEVEL: usize = 0;
pub const METER_GAIN: usize = 3;
pub const METER_BAND_PEAK: usize = 6;
pub const METER_IN_PEAK: usize = 9;
pub const METER_OUT_PEAK: usize = 10;
pub const METER_LATENCY: usize = 11;
pub const METER_EXTERNAL: usize = 12;
pub const METER_LINEAR: usize = 13;
pub const METER_LIMIT: usize = 14;
pub const METER_CURVES: usize = 15;
pub const METER_CURVE_POINTS: usize = 64;
pub const METER_CURVE_MIN_DB: f32 = -90.0;

const OFF_ON: &[&str] = &["Off", "On"];
const DETECTORS: &[&str] = &["Peak", "RMS"];
const MODES: &[&str] = &["Minimum", "Linear"];
const SC: &[&str] = &["Internal", "External"];
const TH_DOWN_NAMES: [&str; 3] = ["Low Down Threshold", "Mid Down Threshold", "High Down Threshold"];
const RATIO_DOWN_NAMES: [&str; 3] = ["Low Down Ratio", "Mid Down Ratio", "High Down Ratio"];
const TH_UP_NAMES: [&str; 3] = ["Low Up Threshold", "Mid Up Threshold", "High Up Threshold"];
const RATIO_UP_NAMES: [&str; 3] = ["Low Up Ratio", "Mid Up Ratio", "High Up Ratio"];
const MAX_UP_NAMES: [&str; 3] = ["Low Max Up", "Mid Max Up", "High Max Up"];
const FLOOR_NAMES: [&str; 3] = ["Low Floor", "Mid Floor", "High Floor"];
const ATTACK_NAMES: [&str; 3] = ["Low Attack", "Mid Attack", "High Attack"];
const RELEASE_NAMES: [&str; 3] = ["Low Release", "Mid Release", "High Release"];
const OUT_NAMES: [&str; 3] = ["Low Output", "Mid Output", "High Output"];
const DEPTH_NAMES: [&str; 3] = ["Low Depth", "Mid Depth", "High Depth"];
const MUTE_NAMES: [&str; 3] = ["Low Mute", "Mid Mute", "High Mute"];
const SOLO_NAMES: [&str; 3] = ["Low Solo", "Mid Solo", "High Solo"];
const DEFAULT_ATTACK: [f32; 3] = [48.0, 22.0, 13.0];
const DEFAULT_RELEASE: [f32; 3] = [280.0, 280.0, 130.0];

/// Width of the fade that brings the upward boost from 0 (at Floor) to full.
const FLOOR_FADE_DB: f32 = 6.0;
/// Level that stands for silence.
const SILENCE_DB: f32 = -120.0;
/// Last-resort bound on the output (+36 dBFS): the input, band and output
/// gains can stack to +72 dB, and a few seconds of slow attack let a hot
/// signal through at that gain; nothing downstream makes use of more.
const OUT_LIMIT: f32 = 64.0;
/// Fall of the held meter levels, dB per second.
const METER_FALL_DB_S: f32 = 40.0;

/// Level (linear) up to which the output bound is perfectly transparent.
const BOUND_KNEE: f32 = 8.0;
/// Each half of the dip that hides a mode change.
const MODE_FADE_MS: f32 = 4.0;

/// Soft bound on the output: identity up to [`BOUND_KNEE`] (+18 dBFS), then a
/// `tanh` shoulder (value and slope continuous) that approaches
/// [`OUT_LIMIT`] without ever reaching it, so nothing is hard-clipped.
pub fn soft_bound(x: f32) -> f32 {
    let a = x.abs();
    if a <= BOUND_KNEE {
        x
    } else {
        let w = OUT_LIMIT - BOUND_KNEE;
        (BOUND_KNEE + w * ((a - BOUND_KNEE) / w).tanh()).copysign(x)
    }
}

pub fn params() -> Vec<ParamInfo> {
    let mut v = vec![
        ParamInfo::new(p::XOVER_LOW, "Crossover Low", 40.0, 1000.0, 90.0).unit("Hz").log().short("X Low"),
        ParamInfo::new(p::XOVER_HIGH, "Crossover High", 500.0, 12_000.0, 2500.0).unit("Hz").log().short("X High"),
        ParamInfo::new(p::AMOUNT, "Amount", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::TIME, "Time", 10.0, 1000.0, 100.0).unit("%").log(),
        ParamInfo::new(p::UPWARD, "Upward", 0.0, 100.0, 100.0).unit("%").short("Up"),
        ParamInfo::new(p::DOWNWARD, "Downward", 0.0, 100.0, 100.0).unit("%").short("Down"),
        ParamInfo::new(p::DETECT, "Detection", 0.0, 1.0, 1.0).choices(DETECTORS).short("Detect"),
        ParamInfo::new(p::KNEE, "Knee", 0.0, 18.0, 6.0).unit("dB"),
        ParamInfo::new(p::INPUT, "Input", -24.0, 24.0, 0.0).unit("dB").short("In"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 24.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::DEPTH, "Depth", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::MODE, "Mode", 0.0, 1.0, mode::MINIMUM).choices(MODES),
        ParamInfo::new(p::SIDECHAIN, "Sidechain", 0.0, 1.0, 0.0).choices(SC).short("SC"),
    ];
    for b in 0..3u32 {
        let i = b as usize;
        v.push(ParamInfo::new(p::band(b, p::TH_DOWN), TH_DOWN_NAMES[i], -60.0, 0.0, -26.0).unit("dB").short("Down Thr"));
        v.push(ParamInfo::new(p::band(b, p::RATIO_DOWN), RATIO_DOWN_NAMES[i], 1.0, 100.0, 3.0).log().short("Down Ratio"));
        v.push(ParamInfo::new(p::band(b, p::TH_UP), TH_UP_NAMES[i], -80.0, -10.0, -44.0).unit("dB").short("Up Thr"));
        v.push(ParamInfo::new(p::band(b, p::RATIO_UP), RATIO_UP_NAMES[i], 1.0, 8.0, 2.0).log().short("Up Ratio"));
        v.push(ParamInfo::new(p::band(b, p::MAX_UP), MAX_UP_NAMES[i], 0.0, 36.0, 12.0).unit("dB").short("Max Up"));
        v.push(ParamInfo::new(p::band(b, p::FLOOR), FLOOR_NAMES[i], -80.0, -40.0, -66.0).unit("dB").short("Floor"));
        v.push(ParamInfo::new(p::band(b, p::ATTACK), ATTACK_NAMES[i], 0.1, 200.0, DEFAULT_ATTACK[i]).unit("ms").log().short("Attack"));
        v.push(ParamInfo::new(p::band(b, p::RELEASE), RELEASE_NAMES[i], 5.0, 2000.0, DEFAULT_RELEASE[i]).unit("ms").log().short("Release"));
        v.push(ParamInfo::new(p::band(b, p::BAND_OUT), OUT_NAMES[i], -24.0, 24.0, 0.0).unit("dB").short("Out"));
        v.push(ParamInfo::new(p::band(b, p::BAND_DEPTH), DEPTH_NAMES[i], 0.0, 100.0, 100.0).unit("%").short("Depth"));
        v.push(ParamInfo::new(p::band(b, p::MUTE), MUTE_NAMES[i], 0.0, 1.0, 0.0).choices(OFF_ON).short("Mute"));
        v.push(ParamInfo::new(p::band(b, p::SOLO), SOLO_NAMES[i], 0.0, 1.0, 0.0).choices(OFF_ON).short("Solo"));
    }
    v
}

/// Static gain curve of one band (all values in dB; slopes already scaled).
#[derive(Clone, Copy, Debug)]
struct Curve {
    ta: f32,
    tb: f32,
    slope_down: f32,
    slope_up: f32,
    max_up: f32,
    floor: f32,
    knee: f32,
}

impl Default for Curve {
    fn default() -> Self {
        Self { ta: 0.0, tb: -80.0, slope_down: 0.0, slope_up: 0.0, max_up: 0.0, floor: -80.0, knee: 0.0 }
    }
}

/// Excess of `over` above 0 dB with a quadratic soft knee of width `knee`.
#[inline]
fn knee_excess(over: f32, knee: f32) -> f32 {
    if knee > 0.0 && 2.0 * over.abs() <= knee {
        let t = over + knee * 0.5;
        t * t / (2.0 * knee)
    } else if over > 0.0 {
        over
    } else {
        0.0
    }
}

impl Curve {
    /// Gain in dB (negative reduces, positive lifts) for a detector level.
    #[inline]
    fn gain_db(&self, level: f32) -> f32 {
        let down = knee_excess(level - self.ta, self.knee) * self.slope_down;
        let fade = ((level - self.floor) * (1.0 / FLOOR_FADE_DB)).clamp(0.0, 1.0);
        let up = (knee_excess(self.tb - level, self.knee) * self.slope_up).min(self.max_up) * fade;
        (up - down).clamp(-100.0, 60.0)
    }
}

/// Per-band running state of the detector and the gain smoother.
#[derive(Clone, Copy, Debug, Default)]
struct BandState {
    /// Mean square (RMS detector).
    ms: f32,
    /// Held peak (peak detector).
    pk: f32,
    /// Smoothed dynamics gain in dB.
    gain: f32,
}

pub struct Mbdyn {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    /// Minimum-phase splitters of the input (one per channel). Always
    /// running: they are the detector's bands, and the audio's in Minimum mode.
    det: [Crossover3; 2],
    /// Minimum-phase splitters of the external key.
    kdet: [Crossover3; 2],
    /// Linear-phase splitters of the audio (running in Linear mode only).
    lin: [LinearPhaseCrossover3; 2],
    /// Brings the Minimum-mode output to the FIR's latency.
    out_delay: [DelayLine; 2],
    curves: [Curve; 3],
    state: [BandState; 3],
    att: [f32; 3],
    rel: [f32; 3],
    rms_coeff: f32,
    peak_decay: f32,
    in_gain: Smoother,
    out_gain: Smoother,
    depth: Smoother,
    band_out: [Smoother; 3],
    band_depth: [Smoother; 3],
    /// 1 when the band is heard (mute / solo), faded.
    audible: [Smoother; 3],
    /// 1 = RMS detector, 0 = peak; faded so the level does not jump.
    mix_rms: Smoother,
    /// 1 = external key, 0 = internal; faded.
    mix_ext: Smoother,
    /// Mode in effect and mode asked for; the output is faded out, the
    /// splitter swapped and the output faded back in when they differ.
    linear: bool,
    want_linear: bool,
    dip: Dip,
    /// Samples the fade-in still waits for a freshly reset FIR.
    wait: u32,
    lin_freqs: (f32, f32),
    // Meter state.
    level_disp: [f32; 3],
    gain_disp: [f32; 3],
    band_peak: [f32; 3],
    in_peak: f32,
    out_peak: f32,
    limit_seen: f32,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Mbdyn::new())
}

impl Default for Mbdyn {
    fn default() -> Self {
        Self::new()
    }
}

impl Mbdyn {
    pub fn new() -> Self {
        let params = params();
        let mut m = Self {
            info: PluginInfo {
                id: ID,
                name: "Multiband Dynamics",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["dynamics", "multiband", "upward", "compressor", "bass", "edm"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: true,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            det: [Crossover3::default(); 2],
            kdet: [Crossover3::default(); 2],
            lin: [LinearPhaseCrossover3::default(), LinearPhaseCrossover3::default()],
            out_delay: [DelayLine::new(8), DelayLine::new(8)],
            curves: [Curve::default(); 3],
            state: [BandState::default(); 3],
            att: [1.0; 3],
            rel: [1.0; 3],
            rms_coeff: 0.01,
            peak_decay: 0.99,
            in_gain: Smoother::new(1.0),
            out_gain: Smoother::new(1.0),
            depth: Smoother::new(1.0),
            band_out: [Smoother::new(1.0); 3],
            band_depth: [Smoother::new(1.0); 3],
            audible: [Smoother::new(1.0); 3],
            mix_rms: Smoother::new(1.0),
            mix_ext: Smoother::new(0.0),
            linear: false,
            want_linear: false,
            dip: Dip::default(),
            wait: 0,
            lin_freqs: (0.0, 0.0),
            level_disp: [SILENCE_DB; 3],
            gain_disp: [0.0; 3],
            band_peak: [0.0; 3],
            in_peak: 0.0,
            out_peak: 0.0,
            limit_seen: 0.0,
            first: true,
        };
        m.prepare(48_000.0, 512);
        m
    }

    /// The one latency of the device, in samples: the linear-phase split's.
    fn fixed_latency(&self) -> usize {
        LinearPhaseCrossover3::latency_for(self.sr)
    }

    fn update(&mut self) {
        let want = self.params.choice(p::MODE) == 1;
        let (f1, f2) = (self.params.plain(p::XOVER_LOW), self.params.plain(p::XOVER_HIGH));
        let f2 = f2.max(f1 * 1.2);
        for c in 0..2 {
            self.det[c].set(3, f1, f2, self.sr);
            self.kdet[c].set(3, f1, f2, self.sr);
        }
        // The FIR is only redesigned while it is (about to be) heard.
        if (want || self.linear) && self.lin_freqs != (f1, f2) {
            for c in &mut self.lin {
                c.set(3, f1, f2, self.sr);
            }
            self.lin_freqs = (f1, f2);
        }
        self.want_linear = want;
        if self.first {
            self.linear = want;
        }
        let external = self.params.on(p::SIDECHAIN);
        if external && self.mix_ext.target() == 0.0 {
            // The key splitters were idle; drop their stale history.
            for c in &mut self.kdet {
                c.reset();
            }
        }
        self.mix_ext.set_target(if external { 1.0 } else { 0.0 });
        self.mix_rms.set_target(if self.params.choice(p::DETECT) == 1 { 1.0 } else { 0.0 });

        let amount = self.params.plain(p::AMOUNT) * 0.01;
        let up = self.params.plain(p::UPWARD) * 0.01 * amount;
        let down = self.params.plain(p::DOWNWARD) * 0.01 * amount;
        let time = self.params.plain(p::TIME) * 0.01;
        let knee = self.params.plain(p::KNEE);
        let any_solo = (0..3u32).any(|b| self.params.on(p::band(b, p::SOLO)));
        for b in 0..3u32 {
            let i = b as usize;
            let ta = self.params.plain(p::band(b, p::TH_DOWN));
            let tb = self.params.plain(p::band(b, p::TH_UP)).min(ta);
            let rd = self.params.plain(p::band(b, p::RATIO_DOWN)).max(1.0);
            let ru = self.params.plain(p::band(b, p::RATIO_UP)).max(1.0);
            self.curves[i] = Curve {
                ta,
                tb,
                slope_down: (1.0 - 1.0 / rd) * down,
                slope_up: (1.0 - 1.0 / ru) * up,
                max_up: self.params.plain(p::band(b, p::MAX_UP)),
                floor: self.params.plain(p::band(b, p::FLOOR)),
                knee,
            };
            self.att[i] = one_pole_coeff(self.params.plain(p::band(b, p::ATTACK)) * time, self.sr);
            self.rel[i] = one_pole_coeff(self.params.plain(p::band(b, p::RELEASE)) * time, self.sr);
            let heard = if any_solo { self.params.on(p::band(b, p::SOLO)) } else { !self.params.on(p::band(b, p::MUTE)) };
            self.audible[i].set_target(if heard { 1.0 } else { 0.0 });
            self.band_out[i].set_target(db_to_gain(self.params.plain(p::band(b, p::BAND_OUT))));
            self.band_depth[i].set_target(self.params.plain(p::band(b, p::BAND_DEPTH)) * 0.01);
        }
        self.in_gain.set_target(db_to_gain(self.params.plain(p::INPUT)));
        self.out_gain.set_target(db_to_gain(self.params.plain(p::OUTPUT)));
        self.depth.set_target(self.params.plain(p::DEPTH) * 0.01);
        if self.first {
            self.in_gain.snap_to_target();
            self.out_gain.snap_to_target();
            self.depth.snap_to_target();
            self.mix_rms.snap_to_target();
            self.mix_ext.snap_to_target();
            for b in 0..3 {
                self.band_out[b].snap_to_target();
                self.band_depth[b].snap_to_target();
                self.audible[b].snap_to_target();
            }
            self.first = false;
        }
    }

    /// Dynamics gain in dB of band `b` for a static input level (no smoothing).
    pub fn static_gain_db(&self, b: usize, level_db: f32) -> f32 {
        self.curves[b.min(2)].gain_db(level_db)
    }
}

impl Segmented for Mbdyn {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let sc = ctx.sidechain;
        let curves = self.curves;
        let (att, rel) = (self.att, self.rel);
        let (rms_coeff, peak_decay) = (self.rms_coeff, self.peak_decay);
        let meter_decay = (-1.0 / (0.3 * self.sr as f32)).exp();
        let lat = self.fixed_latency();
        let want = self.want_linear;
        let this = &mut *self;
        let mut linear = this.linear;
        let mut dip = this.dip;
        let mut wait = this.wait;
        let mut pk_in = this.in_peak;
        let mut pk_out = this.out_peak;
        let mut pk_band = this.band_peak;
        let mut limit_seen = this.limit_seen;
        let mut top_level = this.level_disp;
        let frames = (end - start) as f32;
        let fall = METER_FALL_DB_S * frames / this.sr as f32;
        for l in &mut top_level {
            *l -= fall;
        }
        for_frames(ctx.buffer, start, end, |i, l, r| {
            // Mode change: dip the output ([`Dip`], 4 ms each way) and swap the
            // splitter on the silent sample.
            if linear != want {
                dip.start();
            }
            // A fresh FIR (or a cleared delay line) is silent until the signal
            // reaches its centre, so after a swap the fade-in waits that latency.
            let (fade, swap) = if wait > 0 {
                wait -= 1;
                (0.0, false)
            } else {
                dip.tick()
            };
            if swap {
                linear = want;
                wait = lat as u32;
                if linear {
                    this.lin[0].reset();
                    this.lin[1].reset();
                } else {
                    this.out_delay[0].clear();
                    this.out_delay[1].clear();
                }
            }

            let in_g = this.in_gain.next();
            let out_g = this.out_gain.next();
            let depth_g = this.depth.next();
            let xl = sanitize(l) * in_g;
            let xr = sanitize(r) * in_g;
            pk_in = (pk_in * meter_decay).max(xl.abs().max(xr.abs()));

            let mut dl = [0.0f32; 3];
            let mut dr = [0.0f32; 3];
            this.det[0].split(xl, &mut dl);
            this.det[1].split(xr, &mut dr);
            let (mut bl, mut br) = (dl, dr);
            if linear {
                this.lin[0].split(xl, &mut bl);
                this.lin[1].split(xr, &mut br);
            }
            // Detector bands: the input's, the key's, or a crossfade of both.
            let ext = this.mix_ext.next();
            if ext > 0.0 {
                let (a, b) = sidechain_frame(sc, i);
                let mut kl = [0.0f32; 3];
                let mut kr = [0.0f32; 3];
                this.kdet[0].split(sanitize(a), &mut kl);
                this.kdet[1].split(sanitize(b), &mut kr);
                for b in 0..3 {
                    dl[b] += (kl[b] - dl[b]) * ext;
                    dr[b] += (kr[b] - dr[b]) * ext;
                }
            }
            let rms_mix = this.mix_rms.next();

            let mut acc = [0.0f32; 2];
            for b in 0..3 {
                let st = &mut this.state[b];
                st.ms += (0.5 * (dl[b] * dl[b] + dr[b] * dr[b]) - st.ms) * rms_coeff;
                if st.ms < 1e-30 {
                    st.ms = 0.0;
                }
                st.pk = dl[b].abs().max(dr[b].abs()).max(st.pk * peak_decay);
                if st.pk < 1e-30 {
                    st.pk = 0.0;
                }
                let level = if rms_mix >= 1.0 {
                    10.0 * st.ms.max(1e-12).log10()
                } else if rms_mix <= 0.0 {
                    20.0 * st.pk.max(1e-6).log10()
                } else {
                    let peak_db = 20.0 * st.pk.max(1e-6).log10();
                    peak_db + (10.0 * st.ms.max(1e-12).log10() - peak_db) * rms_mix
                };
                let target = curves[b].gain_db(level);
                let c = if target < st.gain { att[b] } else { rel[b] };
                st.gain += (target - st.gain) * c;
                if st.gain.abs() < 1e-6 && target == 0.0 {
                    st.gain = 0.0;
                }
                top_level[b] = top_level[b].max(level);

                let d = depth_g * this.band_depth[b].next();
                let g = db_to_gain(st.gain) * this.band_out[b].next();
                let m = 1.0 - d + d * g;
                let (yl, yr) = (bl[b] * m, br[b] * m);
                pk_band[b] = (pk_band[b] * meter_decay).max(yl.abs().max(yr.abs()));
                let heard = this.audible[b].next();
                acc[0] += yl * heard;
                acc[1] += yr * heard;
            }
            if !linear {
                for c in 0..2 {
                    this.out_delay[c].push(acc[c]);
                    acc[c] = this.out_delay[c].read(lat);
                }
            }
            let gain = fade * out_g;
            let (ol, or) = (sanitize(acc[0] * gain), sanitize(acc[1] * gain));
            let excess = ((ol.abs().max(or.abs()) - BOUND_KNEE) * (1.0 / (OUT_LIMIT - BOUND_KNEE))).clamp(0.0, 1.0);
            limit_seen = (limit_seen * meter_decay).max(excess);
            let (ol, or) = (soft_bound(ol), soft_bound(or));
            pk_out = (pk_out * meter_decay).max(ol.abs().max(or.abs()));
            (ol, or)
        });
        self.linear = linear;
        self.dip = dip;
        self.wait = wait;
        self.in_peak = pk_in;
        self.out_peak = pk_out;
        self.band_peak = pk_band;
        self.limit_seen = limit_seen;
        self.level_disp = top_level;
        for b in 0..3 {
            self.gain_disp[b] = self.state[b].gain;
        }
    }
}

impl Processor for Mbdyn {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for s in [&mut self.in_gain, &mut self.out_gain, &mut self.depth] {
            s.set_time(20.0, sample_rate);
        }
        for b in 0..3 {
            self.band_out[b].set_time(20.0, sample_rate);
            self.band_depth[b].set_time(20.0, sample_rate);
            self.audible[b].set_time(4.0, sample_rate);
        }
        self.mix_rms.set_time(8.0, sample_rate);
        self.mix_ext.set_time(8.0, sample_rate);
        self.dip.set_time(MODE_FADE_MS, sample_rate);
        self.rms_coeff = one_pole_coeff(10.0, sample_rate);
        self.peak_decay = (-1.0 / (0.010 * sample_rate)).exp() as f32;
        let f1 = self.params.plain(p::XOVER_LOW);
        let f2 = self.params.plain(p::XOVER_HIGH).max(f1 * 1.2);
        self.det = [Crossover3::default(); 2];
        self.kdet = [Crossover3::default(); 2];
        self.lin = [LinearPhaseCrossover3::new(3, f1, f2, sample_rate), LinearPhaseCrossover3::new(3, f1, f2, sample_rate)];
        self.lin_freqs = (f1, f2);
        let lat = self.fixed_latency();
        self.out_delay = [DelayLine::new(lat + 8), DelayLine::new(lat + 8)];
        self.mix_ext.snap(0.0);
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for c in 0..2 {
            self.det[c].reset();
            self.kdet[c].reset();
            self.lin[c].reset();
            self.out_delay[c].clear();
        }
        self.state = [BandState::default(); 3];
        self.linear = self.want_linear;
        self.dip.reset();
        self.wait = 0;
        self.level_disp = [SILENCE_DB; 3];
        self.gain_disp = [0.0; 3];
        self.band_peak = [0.0; 3];
        self.in_peak = 0.0;
        self.out_peak = 0.0;
        self.limit_seen = 0.0;
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

    /// Constant: the Minimum path is delayed to match the linear-phase one.
    fn latency(&self) -> u32 {
        self.fixed_latency() as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let mut o = write_meter(out, 0, &self.level_disp);
        o = write_meter(out, o, &self.gain_disp);
        let bands = [gain_to_db(self.band_peak[0]), gain_to_db(self.band_peak[1]), gain_to_db(self.band_peak[2])];
        o = write_meter(out, o, &bands);
        o = write_meter(
            out,
            o,
            &[
                gain_to_db(self.in_peak),
                gain_to_db(self.out_peak),
                self.latency() as f32,
                if self.params.on(p::SIDECHAIN) { 1.0 } else { 0.0 },
                if self.params.choice(p::MODE) == 1 { 1.0 } else { 0.0 },
                if self.limit_seen > 0.001 { self.limit_seen } else { 0.0 },
            ],
        );
        for b in 0..3 {
            let mut curve = [0.0f32; METER_CURVE_POINTS];
            for (i, c) in curve.iter_mut().enumerate() {
                let level = METER_CURVE_MIN_DB - METER_CURVE_MIN_DB * i as f32 / (METER_CURVE_POINTS - 1) as f32;
                *c = level + self.curves[b].gain_db(level);
            }
            o = write_meter(out, o, &curve);
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{BLOCK, SR, peak, process_stereo, rms, set_plain, sine};
    use aitrack_dsp::crossover::{Crossover3, LinearPhaseCrossover3};
    use aitrack_dsp::{AudioBuffer, Transport};

    const BANDS: [u32; 3] = [0, 1, 2];

    fn all_bands(m: &mut Mbdyn, offset: u32, plain: f32) {
        for b in BANDS {
            set_plain(m, p::band(b, offset), plain);
        }
    }

    /// Dynamics off everywhere (ratios 1:1), unity gains, full depth, hard knee.
    fn neutral(m: &mut Mbdyn) {
        set_plain(m, p::XOVER_LOW, 90.0);
        set_plain(m, p::XOVER_HIGH, 2500.0);
        set_plain(m, p::AMOUNT, 100.0);
        set_plain(m, p::TIME, 100.0);
        set_plain(m, p::UPWARD, 100.0);
        set_plain(m, p::DOWNWARD, 100.0);
        set_plain(m, p::DETECT, 1.0);
        set_plain(m, p::KNEE, 0.0);
        set_plain(m, p::INPUT, 0.0);
        set_plain(m, p::OUTPUT, 0.0);
        set_plain(m, p::DEPTH, 100.0);
        set_plain(m, p::MODE, 0.0);
        set_plain(m, p::SIDECHAIN, 0.0);
        all_bands(m, p::RATIO_DOWN, 1.0);
        all_bands(m, p::RATIO_UP, 1.0);
        all_bands(m, p::TH_DOWN, -20.0);
        all_bands(m, p::TH_UP, -40.0);
        all_bands(m, p::MAX_UP, 36.0);
        all_bands(m, p::FLOOR, -80.0);
        all_bands(m, p::ATTACK, 5.0);
        all_bands(m, p::RELEASE, 20.0);
        all_bands(m, p::BAND_OUT, 0.0);
        all_bands(m, p::BAND_DEPTH, 100.0);
        all_bands(m, p::MUTE, 0.0);
        all_bands(m, p::SOLO, 0.0);
    }

    fn noise_gen(seed: u32, amp: f32) -> impl FnMut(usize) -> (f32, f32) {
        let mut s = seed;
        move |_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let a = ((s >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 2.0 * amp;
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let b = ((s >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 2.0 * amp;
            (a, b)
        }
    }

    fn db(x: f32) -> f32 {
        gain_to_db(x)
    }

    /// Steady-state level (dB re 1) of the last quarter second of `x`.
    fn tail_db(x: &[f32]) -> f32 {
        db(rms(&x[x.len() - 12_000..]))
    }

    fn sine_rms_db(amp: f32) -> f32 {
        db(amp / std::f32::consts::SQRT_2)
    }

    /// Output level change in dB for a steady sine.
    fn gain_for_sine(m: &mut Mbdyn, freq: f32, amp: f32) -> f32 {
        let (l, _) = process_stereo(m, sine(freq, amp), 72_000);
        tail_db(&l) - sine_rms_db(amp)
    }

    // ---- Depth 0 / Amount 0: the plain LR4 sum --------------------------

    #[test]
    fn depth_zero_is_exactly_the_lr4_sum() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        // Aggressive dynamics and uneven band gains must not matter at Depth 0.
        all_bands(&mut m, p::RATIO_DOWN, 100.0);
        all_bands(&mut m, p::RATIO_UP, 8.0);
        all_bands(&mut m, p::TH_DOWN, -50.0);
        all_bands(&mut m, p::TH_UP, -50.0);
        set_plain(&mut m, p::band(0, p::BAND_OUT), 12.0);
        set_plain(&mut m, p::band(2, p::BAND_OUT), -9.0);
        set_plain(&mut m, p::DEPTH, 0.0);
        let n = 24_000;
        let mut src = noise_gen(7, 0.3);
        let input: Vec<(f32, f32)> = (0..n).map(&mut src).collect();
        let (l, r) = process_stereo(&mut m, |i| input[i], n);
        let lat = m.latency() as usize;
        let mut xo = [Crossover3::new(3, 90.0, 2500.0, SR), Crossover3::new(3, 90.0, 2500.0, SR)];
        let (mut sum_l, mut sum_r) = (Vec::new(), Vec::new());
        for i in 0..n {
            let mut bl = [0.0f32; 3];
            let mut br = [0.0f32; 3];
            xo[0].split(input[i].0, &mut bl);
            xo[1].split(input[i].1, &mut br);
            sum_l.push(bl.iter().sum::<f32>());
            sum_r.push(br.iter().sum::<f32>());
        }
        // The Minimum path is delayed to the fixed latency.
        let mut worst = 0.0f32;
        for i in lat..n {
            worst = worst.max((l[i] - sum_l[i - lat]).abs()).max((r[i] - sum_r[i - lat]).abs());
        }
        assert!(worst < 1e-5, "worst deviation from the LR4 sum {worst}");
    }

    #[test]
    fn band_depth_zero_ignores_dynamics_and_band_gain() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        all_bands(&mut m, p::RATIO_DOWN, 20.0);
        all_bands(&mut m, p::TH_DOWN, -40.0);
        all_bands(&mut m, p::BAND_DEPTH, 0.0);
        all_bands(&mut m, p::BAND_OUT, 7.0);
        let g = gain_for_sine(&mut m, 700.0, 0.2);
        assert!(g.abs() < 0.05, "{g} dB");
    }

    #[test]
    fn depth_zero_sum_is_flat_without_cancellation() {
        for f in [40.0, 90.0, 230.0, 900.0, 2500.0, 6000.0, 15_000.0] {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            all_bands(&mut m, p::RATIO_DOWN, 50.0);
            set_plain(&mut m, p::DEPTH, 0.0);
            let g = gain_for_sine(&mut m, f, 0.2);
            assert!(g.abs() < 0.1, "{f} Hz: {g} dB");
        }
        let mut m = Mbdyn::new();
        neutral(&mut m);
        set_plain(&mut m, p::DEPTH, 0.0);
        let mut input = noise_gen(99, 0.25);
        let src: Vec<(f32, f32)> = (0..48_000).map(&mut input).collect();
        let (l, _) = process_stereo(&mut m, |i| src[i], 48_000);
        let a: Vec<f32> = src.iter().map(|s| s.0).collect();
        let g = db(rms(&l[8000..]) / rms(&a[8000..]));
        assert!(g.abs() < 0.05, "noise: {g} dB");
    }

    #[test]
    fn amount_zero_leaves_the_signal_alone() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        all_bands(&mut m, p::RATIO_DOWN, 100.0);
        all_bands(&mut m, p::RATIO_UP, 8.0);
        all_bands(&mut m, p::TH_DOWN, -50.0);
        set_plain(&mut m, p::AMOUNT, 0.0);
        let g = gain_for_sine(&mut m, 700.0, 0.2);
        assert!(g.abs() < 0.05, "{g} dB");
    }

    // ---- static curves ---------------------------------------------------

    #[test]
    fn downward_curve_matches_the_ratio() {
        for &(amp, thr, ratio) in &[(0.1f32, -35.0f32, 4.0f32), (0.5, -30.0, 2.0), (0.3, -25.0, 100.0), (0.05, -20.0, 4.0)] {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            all_bands(&mut m, p::TH_DOWN, thr);
            all_bands(&mut m, p::RATIO_DOWN, ratio);
            let g = gain_for_sine(&mut m, 700.0, amp);
            let level = sine_rms_db(amp);
            let want = -(level - thr).max(0.0) * (1.0 - 1.0 / ratio);
            assert!((g - want).abs() < 0.4, "amp {amp} thr {thr} ratio {ratio}: {g} dB, want {want}");
        }
    }

    #[test]
    fn upward_curve_matches_the_ratio_and_max_up() {
        for &(amp, tb, ratio, max_up) in &[(0.001f32, -40.0f32, 2.0f32, 36.0f32), (0.002, -35.0, 4.0, 36.0), (0.001, -40.0, 8.0, 6.0), (0.05, -40.0, 4.0, 36.0)] {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            all_bands(&mut m, p::TH_DOWN, 0.0);
            all_bands(&mut m, p::TH_UP, tb);
            all_bands(&mut m, p::RATIO_UP, ratio);
            all_bands(&mut m, p::MAX_UP, max_up);
            // Floor above the neighbouring bands' leakage of the tone.
            all_bands(&mut m, p::FLOOR, -70.0);
            let g = gain_for_sine(&mut m, 700.0, amp);
            let level = sine_rms_db(amp);
            let want = ((tb - level).max(0.0) * (1.0 - 1.0 / ratio)).min(max_up);
            assert!((g - want).abs() < 0.4, "amp {amp} tb {tb} ratio {ratio} max {max_up}: {g} dB, want {want}");
        }
    }

    #[test]
    fn up_and_down_in_one_band_work_together() {
        // A loud tone is pulled down, a quiet one is pushed up, towards each other.
        let run = |amp: f32| {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            all_bands(&mut m, p::TH_DOWN, -30.0);
            all_bands(&mut m, p::RATIO_DOWN, 4.0);
            all_bands(&mut m, p::TH_UP, -40.0);
            all_bands(&mut m, p::RATIO_UP, 4.0);
            gain_for_sine(&mut m, 700.0, amp) + sine_rms_db(amp)
        };
        let (loud, quiet) = (run(0.5), run(0.004));
        assert!(loud < sine_rms_db(0.5) - 5.0, "loud {loud}");
        assert!(quiet > sine_rms_db(0.004) + 5.0, "quiet {quiet}");
        let before = sine_rms_db(0.5) - sine_rms_db(0.004);
        assert!(loud - quiet < before - 15.0, "spread {before} → {}", loud - quiet);
    }

    #[test]
    fn depth_mixes_the_gain_in_phase() {
        let run = |depth: f32| {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            all_bands(&mut m, p::TH_DOWN, -35.0);
            all_bands(&mut m, p::RATIO_DOWN, 4.0);
            set_plain(&mut m, p::DEPTH, depth);
            gain_for_sine(&mut m, 700.0, 0.1)
        };
        let full = run(100.0);
        assert!((full + 9.0).abs() < 0.4, "{full}");
        let gl = 10f32.powf(full / 20.0);
        let half = run(50.0);
        let want = 20.0 * (0.5 + 0.5 * gl).log10();
        assert!((half - want).abs() < 0.3, "half {half}, want {want}");
        assert!(run(0.0).abs() < 0.1);
    }

    #[test]
    fn amount_up_and_down_scale_the_slopes() {
        let run = |amount: f32, up: f32, down: f32, amp: f32| {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            all_bands(&mut m, p::TH_DOWN, -35.0);
            all_bands(&mut m, p::RATIO_DOWN, 4.0);
            all_bands(&mut m, p::TH_UP, -40.0);
            all_bands(&mut m, p::RATIO_UP, 4.0);
            all_bands(&mut m, p::FLOOR, -66.0);
            set_plain(&mut m, p::AMOUNT, amount);
            set_plain(&mut m, p::UPWARD, up);
            set_plain(&mut m, p::DOWNWARD, down);
            gain_for_sine(&mut m, 700.0, amp)
        };
        let full = run(100.0, 100.0, 100.0, 0.1);
        assert!((run(50.0, 100.0, 100.0, 0.1) - full * 0.5).abs() < 0.3);
        assert!(run(100.0, 100.0, 0.0, 0.1).abs() < 0.1);
        assert!((run(100.0, 0.0, 100.0, 0.1) - full).abs() < 0.2);
        let up_full = run(100.0, 100.0, 100.0, 0.003);
        assert!(up_full > 8.0, "{up_full}");
        assert!((run(100.0, 50.0, 100.0, 0.003) - up_full * 0.5).abs() < 0.3);
        assert!(run(100.0, 0.0, 100.0, 0.003).abs() < 0.1);
    }

    // ---- Floor: upward compression never amplifies the noise floor -----

    #[test]
    fn upward_compression_never_lifts_signal_below_floor() {
        let setup = |floor: f32| {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            all_bands(&mut m, p::TH_DOWN, 0.0);
            all_bands(&mut m, p::TH_UP, -30.0);
            all_bands(&mut m, p::RATIO_UP, 8.0);
            all_bands(&mut m, p::MAX_UP, 36.0);
            all_bands(&mut m, p::FLOOR, floor);
            m
        };
        // A -70 dBFS tone: well under a -60 dB floor, well above a -80 dB one.
        let amp = 10f32.powf(-70.0 / 20.0);
        let guarded = gain_for_sine(&mut setup(-60.0), 700.0, amp);
        assert!(guarded.abs() < 0.05, "below Floor: {guarded} dB");
        let open = gain_for_sine(&mut setup(-80.0), 700.0, amp);
        assert!(open > 10.0, "Floor at -80 dB should let it be lifted: {open} dB");
        // Noise: every band quiet.
        let mut m = setup(-60.0);
        let mut src = noise_gen(5, 10f32.powf(-62.0 / 20.0));
        let input: Vec<(f32, f32)> = (0..72_000).map(&mut src).collect();
        let (l, _) = process_stereo(&mut m, |i| input[i], 72_000);
        let a: Vec<f32> = input.iter().map(|s| s.0).collect();
        let g = db(rms(&l[36_000..]) / rms(&a[36_000..]));
        assert!(g.abs() < 0.2, "noise under Floor: {g} dB");
    }

    #[test]
    fn floor_hands_the_boost_back_when_the_signal_dies() {
        // A tone, then a -75 dB tail with Floor at -60: the gain returns to
        // 0 dB instead of lifting the tail.
        let mut m = Mbdyn::new();
        neutral(&mut m);
        all_bands(&mut m, p::TH_DOWN, 0.0);
        all_bands(&mut m, p::TH_UP, -20.0);
        all_bands(&mut m, p::RATIO_UP, 8.0);
        all_bands(&mut m, p::FLOOR, -60.0);
        let tail = 10f32.powf(-75.0 / 20.0);
        let (l, _) = process_stereo(
            &mut m,
            |i| {
                let amp = if i < 24_000 { 0.05 } else { tail };
                let v = amp * (std::f64::consts::TAU * 700.0 * i as f64 / SR).sin() as f32;
                (v, v)
            },
            96_000,
        );
        let g = tail_db(&l) - sine_rms_db(tail);
        assert!(g.abs() < 0.3, "tail lifted by {g} dB");
    }

    #[test]
    fn digital_silence_stays_silent_and_finite() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        all_bands(&mut m, p::RATIO_UP, 8.0);
        all_bands(&mut m, p::TH_UP, -10.0);
        all_bands(&mut m, p::FLOOR, -80.0);
        all_bands(&mut m, p::RATIO_DOWN, 100.0);
        all_bands(&mut m, p::TH_DOWN, -60.0);
        all_bands(&mut m, p::BAND_OUT, 24.0);
        for mode in [0.0, 1.0] {
            set_plain(&mut m, p::MODE, mode);
            let (l, r) = process_stereo(&mut m, |_| (0.0, 0.0), 24_000);
            assert!(l.iter().chain(&r).all(|v| *v == 0.0), "mode {mode}");
        }
        // Silence after a hit settles to exact silence, never NaN.
        let (l, r) = process_stereo(&mut m, |i| if i < 4800 { ((i as f32 * 0.05).sin() * 0.9, 0.3) } else { (0.0, 0.0) }, 96_000);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        assert!(peak(&l[90_000..]) < 1e-6 && peak(&r[90_000..]) < 1e-6);
    }

    // ---- detector, time, stereo, bands -----------------------------------

    #[test]
    fn peak_and_rms_detectors_differ_on_a_crest() {
        // A sparse burst has a higher peak than RMS level: peak detection
        // pulls it down harder.
        let run = |detect: f32| {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            set_plain(&mut m, p::DETECT, detect);
            all_bands(&mut m, p::TH_DOWN, -30.0);
            all_bands(&mut m, p::RATIO_DOWN, 8.0);
            all_bands(&mut m, p::ATTACK, 1.0);
            all_bands(&mut m, p::RELEASE, 50.0);
            let burst = |i: usize| {
                let v = if i % 4800 < 48 { 0.4 * (std::f64::consts::TAU * 700.0 * i as f64 / SR).sin() as f32 } else { 0.0 };
                (v, v)
            };
            let (l, _) = process_stereo(&mut m, burst, 48_000);
            rms(&l[24_000..])
        };
        let (pk, rm) = (run(0.0), run(1.0));
        assert!(pk < rm, "peak {pk} rms {rm}");
    }

    #[test]
    fn time_scales_attack_and_release() {
        // 40 ms after a loud onset a long attack has hardly bitten yet.
        let burst = |time: f32| {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            all_bands(&mut m, p::TH_DOWN, -40.0);
            all_bands(&mut m, p::RATIO_DOWN, 20.0);
            all_bands(&mut m, p::ATTACK, 20.0);
            set_plain(&mut m, p::TIME, time);
            let (l, _) = process_stereo(&mut m, sine(700.0, 0.5), 4800);
            rms(&l[1920..2400])
        };
        let fast = burst(10.0);
        let slow = burst(1000.0);
        assert!(slow > fast * 3.0, "fast {fast}, slow {slow}");
    }

    #[test]
    fn stereo_link_keeps_the_image() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        all_bands(&mut m, p::TH_DOWN, -30.0);
        all_bands(&mut m, p::RATIO_DOWN, 8.0);
        let (l, r) = process_stereo(
            &mut m,
            |i| {
                let s = (std::f64::consts::TAU * 700.0 * i as f64 / SR).sin() as f32;
                (0.5 * s, 0.05 * s)
            },
            48_000,
        );
        let ratio_in = 0.5 / 0.05;
        let ratio_out = rms(&l[36_000..]) / rms(&r[36_000..]);
        assert!((ratio_out / ratio_in - 1.0).abs() < 0.01, "{ratio_out}");
        assert!(rms(&l[36_000..]) < 0.5 * std::f32::consts::FRAC_1_SQRT_2 * 0.5, "left was compressed");
    }

    #[test]
    fn bands_are_independent() {
        // Compress only the low band hard: a high tone does not change.
        let mut m = Mbdyn::new();
        neutral(&mut m);
        set_plain(&mut m, p::band(0, p::TH_DOWN), -50.0);
        set_plain(&mut m, p::band(0, p::RATIO_DOWN), 100.0);
        let high = gain_for_sine(&mut m, 8000.0, 0.3);
        assert!(high.abs() < 0.2, "{high}");
        let low = gain_for_sine(&mut m, 50.0, 0.3);
        assert!(low < -12.0, "{low}");
    }

    #[test]
    fn solo_mute_and_gains() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        set_plain(&mut m, p::band(2, p::SOLO), 1.0);
        assert!(gain_for_sine(&mut m, 8000.0, 0.2).abs() < 0.3);
        assert!(gain_for_sine(&mut m, 80.0, 0.2) < -30.0);
        let mut m = Mbdyn::new();
        neutral(&mut m);
        all_bands(&mut m, p::MUTE, 1.0);
        let (l, _) = process_stereo(&mut m, sine(440.0, 0.2), 8000);
        assert!(peak(&l[2000..]) < 1e-6);
        let mut m = Mbdyn::new();
        neutral(&mut m);
        set_plain(&mut m, p::band(1, p::BAND_OUT), 6.0);
        let g = gain_for_sine(&mut m, 700.0, 0.1);
        assert!((g - 6.0).abs() < 0.3, "{g}");
        let mut m = Mbdyn::new();
        neutral(&mut m);
        set_plain(&mut m, p::INPUT, -6.0);
        set_plain(&mut m, p::OUTPUT, 6.0);
        assert!(gain_for_sine(&mut m, 700.0, 0.1).abs() < 0.1);
    }

    #[test]
    fn external_sidechain_keys_the_detector() {
        let run = |external: bool, key_amp: f32| {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            set_plain(&mut m, p::SIDECHAIN, if external { 1.0 } else { 0.0 });
            all_bands(&mut m, p::TH_DOWN, -25.0);
            all_bands(&mut m, p::RATIO_DOWN, 8.0);
            m.prepare(SR, BLOCK);
            let mut buf = AudioBuffer::new(2, BLOCK);
            let mut sc = AudioBuffer::new(2, BLOCK);
            let tr = Transport::default();
            let mut out = Vec::new();
            for blk in 0..375 {
                buf.set_frames(BLOCK);
                sc.set_frames(BLOCK);
                for i in 0..BLOCK {
                    let n = (blk * BLOCK + i) as f64;
                    let a = 0.02 * (std::f64::consts::TAU * 700.0 * n / SR).sin() as f32;
                    let k = key_amp * (std::f64::consts::TAU * 700.0 * n / SR).sin() as f32;
                    for ch in 0..2 {
                        buf.channel_mut(ch)[i] = a;
                        sc.channel_mut(ch)[i] = k;
                    }
                }
                let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: Some(&sc), events: &[], transport: &tr };
                m.process(&mut ctx);
                out.extend_from_slice(buf.channel(0));
            }
            db(rms(&out[out.len() - 12_000..]) / (0.02 * std::f32::consts::FRAC_1_SQRT_2))
        };
        // The programme itself (-37 dB) is far under the -25 dB threshold.
        assert!(run(false, 0.8).abs() < 0.2, "internal ignores the key");
        let keyed = run(true, 0.8);
        assert!(keyed < -8.0, "external key {keyed}");
        assert!(run(true, 0.0).abs() < 0.2, "silent key leaves the signal alone");
    }

    // ---- linear mode / latency -------------------------------------------

    #[test]
    fn linear_mode_at_depth_zero_is_the_delayed_input() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        all_bands(&mut m, p::RATIO_DOWN, 50.0);
        set_plain(&mut m, p::DEPTH, 0.0);
        set_plain(&mut m, p::MODE, 1.0);
        let lat = m.latency() as usize;
        assert_eq!(lat, LinearPhaseCrossover3::latency_for(SR));
        let n = 24_000;
        let (l, _) = process_stereo(&mut m, sine(440.0, 0.2), n);
        let mut worst = 0.0f32;
        for i in lat + 200..n {
            let expect = 0.2 * (std::f64::consts::TAU * 440.0 * (i - lat) as f64 / SR).sin() as f32;
            worst = worst.max((l[i] - expect).abs());
        }
        assert!(worst < 1e-4, "{worst}");
        set_plain(&mut m, p::MODE, 0.0);
        assert_eq!(m.latency() as usize, lat);
    }

    #[test]
    fn linear_mode_compresses_too() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        all_bands(&mut m, p::TH_DOWN, -35.0);
        all_bands(&mut m, p::RATIO_DOWN, 4.0);
        set_plain(&mut m, p::MODE, 1.0);
        let g = gain_for_sine(&mut m, 700.0, 0.1);
        assert!((g + 9.0).abs() < 0.5, "{g}");
    }

    // ---- plumbing --------------------------------------------------------

    #[test]
    fn parameter_layout_is_stable_and_complete() {
        let m = Mbdyn::new();
        let info = &m.info().params;
        let mut ids: Vec<u32> = info.iter().map(|i| i.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), info.len(), "duplicate ids");
        assert_eq!(info.len(), 13 + 3 * 12);
        for b in 0..3 {
            for off in 0..12 {
                assert!(ids.contains(&p::band(b, off)), "band {b} offset {off}");
            }
        }
        assert!(m.info().has_sidechain);
        let get = |id: u32| info.iter().find(|i| i.id == id).unwrap();
        assert_eq!((get(p::band(0, p::MAX_UP)).min, get(p::band(0, p::MAX_UP)).max), (0.0, 36.0));
        assert_eq!((get(p::band(0, p::FLOOR)).min, get(p::band(0, p::FLOOR)).max), (-80.0, -40.0));
        assert_eq!((get(p::TIME).min, get(p::TIME).max), (10.0, 1000.0));
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let ids: Vec<u32> = Mbdyn::new().info().params.iter().map(|i| i.id).collect();
        for pass in 0..5u32 {
            let mut m = Mbdyn::new();
            m.prepare(SR, BLOCK);
            for (j, id) in ids.iter().enumerate() {
                let v = match pass {
                    0 => 0.0,
                    1 => 1.0,
                    2 => (j % 2) as f32,
                    3 => ((j * 7919 + 13) % 101) as f32 / 100.0,
                    _ => ((j + 1) % 2) as f32,
                };
                m.set_param(*id, v);
            }
            for amp in [0.0f32, 1e-9, 1.0, 100.0] {
                let mut src = noise_gen(11 + pass, amp);
                let (l, r) = process_stereo(&mut m, &mut src, 12_000);
                assert!(l.iter().chain(&r).all(|v| v.is_finite()), "pass {pass} amp {amp}");
            }
        }
    }

    #[test]
    fn meter_reports_levels_gain_and_curves() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        all_bands(&mut m, p::TH_DOWN, -30.0);
        all_bands(&mut m, p::RATIO_DOWN, 4.0);
        process_stereo(&mut m, sine(700.0, 0.5), 48_000);
        let mut meter = [0.0f32; 320];
        let n = m.meter(&mut meter);
        assert_eq!(n, METER_LEN);
        assert!(n <= 320);
        // Mid band level near -9 dB and the others far lower.
        assert!((meter[METER_LEVEL + 1] + 9.0).abs() < 1.0, "{}", meter[METER_LEVEL + 1]);
        assert!(meter[METER_LEVEL] < -40.0 && meter[METER_LEVEL + 2] < -40.0);
        // Applied gain: mid band reduced by (-9 + 30) * 0.75 = 15.8 dB.
        assert!((meter[METER_GAIN + 1] + 15.8).abs() < 1.0, "{}", meter[METER_GAIN + 1]);
        // The curve of the mid band agrees with the static formula.
        let i = ((-9.0 - METER_CURVE_MIN_DB) / -METER_CURVE_MIN_DB * (METER_CURVE_POINTS - 1) as f32).round() as usize;
        let level = METER_CURVE_MIN_DB - METER_CURVE_MIN_DB * i as f32 / (METER_CURVE_POINTS - 1) as f32;
        let out = meter[METER_CURVES + METER_CURVE_POINTS + i];
        assert!((out - (level - (level + 30.0) * 0.75)).abs() < 0.05, "{out} at {level}");
        assert!((meter[METER_IN_PEAK] + 6.0).abs() < 1.0, "{}", meter[METER_IN_PEAK]);
        assert_eq!(meter[METER_LATENCY] as usize, LinearPhaseCrossover3::latency_for(SR));
    }

    #[test]
    fn state_round_trips() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        set_plain(&mut m, p::band(2, p::FLOOR), -55.0);
        let bytes = m.save_state();
        let mut n = Mbdyn::new();
        n.load_state(&bytes);
        assert!((n.params.plain(p::band(2, p::FLOOR)) + 55.0).abs() < 1e-3);
        n.load_state(b"junk");
    }

    #[test]
    fn dense_preset_exists_and_the_factory_set_is_complete() {
        let list = crate::presets::list(ID);
        assert!(list.len() >= 10, "{} presets", list.len());
        assert!(list.iter().any(|r| r.name == "Denso"), "no Denso preset");
        for name in ["Denso Suave", "Master Suave", "Bajo Controlado", "Voz al Frente", "Bateria Viva"] {
            assert!(list.iter().any(|r| r.name == name), "missing {name}");
        }
    }

    #[test]
    fn dense_preset_compresses_both_ways() {
        let list = crate::presets::list(ID);
        let r = list.iter().find(|r| r.name == "Denso").unwrap();
        let preset = crate::presets::load(&r.path).unwrap();
        let level = |amp: f32| {
            let mut m = Mbdyn::new();
            preset.apply(&mut m);
            let (l, _) = process_stereo(&mut m, sine(700.0, amp), 96_000);
            tail_db(&l)
        };
        // 40 dB of input range collapses to far less: quiet is lifted, loud is held.
        let (quiet, loud) = (level(0.003), level(0.3));
        assert!(loud - quiet < 20.0, "output spread {} dB for 40 dB in", loud - quiet);
        assert!(quiet > sine_rms_db(0.003) + 6.0, "quiet lifted to {quiet}");
    }

    // ---- fixed latency, soft output bound, click-free discrete params -----

    #[test]
    fn latency_is_the_same_in_both_modes_and_the_impulse_lands_on_it() {
        let lat = LinearPhaseCrossover3::latency_for(SR);
        for mode in [0.0, 1.0] {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            set_plain(&mut m, p::DEPTH, 0.0);
            set_plain(&mut m, p::MODE, mode);
            assert_eq!(m.latency() as usize, lat, "mode {mode}");
            let (l, _) = process_stereo(&mut m, |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, 2048);
            let first = l.iter().position(|v| v.abs() > 1e-3).unwrap();
            let top = l.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
            assert!(first >= lat, "mode {mode}: energy before the latency at {first}");
            assert!(top >= lat && top <= lat + 8, "mode {mode}: peak at {top}, latency {lat}");
        }
        // Changing the mode while running keeps the reported latency.
        let mut m = Mbdyn::new();
        let before = m.latency();
        set_plain(&mut m, p::MODE, 1.0);
        assert_eq!(m.latency(), before);
    }

    #[test]
    fn output_bound_is_transparent_then_soft() {
        assert_eq!(soft_bound(0.5), 0.5);
        assert_eq!(soft_bound(-7.9), -7.9);
        let mut prev = soft_bound(0.0);
        let mut prev_dy = 0.05f32;
        let mut x = 0.0f32;
        while x < 500.0 {
            x += 0.05;
            let y = soft_bound(x);
            assert!(y >= prev && y < OUT_LIMIT, "monotonic and bounded at {x}: {y}");
            // No corner anywhere (a hard clip would change the step by 0.05).
            assert!(((y - prev) - prev_dy).abs() < 1e-3, "corner at {x}");
            prev_dy = y - prev;
            prev = y;
        }
        assert!(soft_bound(500.0) > 0.99 * OUT_LIMIT);
        assert_eq!(soft_bound(-3.0), -soft_bound(3.0));
        assert!(soft_bound(f32::MAX).is_finite());
    }

    #[test]
    fn hot_stacked_gains_are_bounded_and_flagged() {
        let mut m = Mbdyn::new();
        neutral(&mut m);
        set_plain(&mut m, p::INPUT, 24.0);
        set_plain(&mut m, p::OUTPUT, 24.0);
        all_bands(&mut m, p::BAND_OUT, 24.0);
        all_bands(&mut m, p::RATIO_UP, 8.0);
        all_bands(&mut m, p::MAX_UP, 36.0);
        all_bands(&mut m, p::TH_UP, -10.0);
        let mut src = noise_gen(3, 10f32.powf(-50.0 / 20.0));
        let (l, r) = process_stereo(&mut m, &mut src, 48_000);
        assert!(peak(&l).max(peak(&r)) <= OUT_LIMIT);
        assert!(l.iter().all(|v| v.is_finite()));
        let mut meter = [0.0f32; 320];
        m.meter(&mut meter);
        assert!(meter[METER_LIMIT] > 0.0, "meter must say the bound is engaged");
        // Ordinary material never touches it.
        let mut m = Mbdyn::new();
        neutral(&mut m);
        process_stereo(&mut m, sine(700.0, 0.5), 24_000);
        m.meter(&mut meter);
        assert_eq!(meter[METER_LIMIT], 0.0);
    }

    /// Largest sample-to-sample step of a 100 Hz sine after a parameter event
    /// lands mid-block, and the same without the event.
    fn event_step(setup: &dyn Fn(&mut Mbdyn), id: u32, plain: f32) -> (f32, f32) {
        let run = |with_event: bool| {
            let mut m = Mbdyn::new();
            neutral(&mut m);
            setup(&mut m);
            m.prepare(SR, BLOCK);
            let norm = m.info().params.iter().find(|i| i.id == id).unwrap().to_normalized(plain);
            let mut buf = AudioBuffer::new(2, BLOCK);
            let mut sc = AudioBuffer::new(2, BLOCK);
            let tr = Transport::default();
            let mut out = Vec::new();
            for blk in 0..160 {
                buf.set_frames(BLOCK);
                sc.set_frames(BLOCK);
                for i in 0..BLOCK {
                    let n = (blk * BLOCK + i) as f64;
                    let a = 0.5 * (std::f64::consts::TAU * 100.0 * n / SR).sin() as f32;
                    // The key is a different, much quieter signal.
                    let k = 0.01 * (std::f64::consts::TAU * 1500.0 * n / SR).sin() as f32;
                    for ch in 0..2 {
                        buf.channel_mut(ch)[i] = a;
                        sc.channel_mut(ch)[i] = k;
                    }
                }
                let ev = [aitrack_dsp::Event { offset: 64, kind: aitrack_dsp::EventKind::Param { id, value: norm } }];
                let events: &[aitrack_dsp::Event] = if with_event && blk == 100 { &ev } else { &[] };
                let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: Some(&sc), events, transport: &tr };
                m.process(&mut ctx);
                out.extend_from_slice(buf.channel(0));
            }
            // Steps from just before the event on, long enough to cover the fades.
            let from = 100 * BLOCK + 32;
            out[from..from + 4000].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max)
        };
        (run(true), run(false))
    }

    fn assert_no_click(name: &str, setup: &dyn Fn(&mut Mbdyn), id: u32, plain: f32) {
        let (jump, reference) = event_step(setup, id, plain);
        eprintln!("{name}: step {jump:.4} (reference {reference:.4})");
        assert!(jump <= reference * 3.0 + 0.003, "{name}: step {jump} vs reference {reference}");
    }

    fn punchy(m: &mut Mbdyn) {
        all_bands(m, p::TH_DOWN, -30.0);
        all_bands(m, p::RATIO_DOWN, 20.0);
        all_bands(m, p::TH_UP, -25.0);
        all_bands(m, p::RATIO_UP, 4.0);
        all_bands(m, p::ATTACK, 0.1);
        all_bands(m, p::RELEASE, 5.0);
        all_bands(m, p::BAND_OUT, 6.0);
    }

    #[test]
    fn automating_mode_does_not_click() {
        assert_no_click("mode min→lin", &punchy, p::MODE, 1.0);
        assert_no_click("mode lin→min", &|m| {
            punchy(m);
            set_plain(m, p::MODE, 1.0);
        }, p::MODE, 0.0);
    }

    #[test]
    fn automating_detection_and_sidechain_does_not_click() {
        assert_no_click("detect rms→peak", &punchy, p::DETECT, 0.0);
        assert_no_click("detect peak→rms", &|m| {
            punchy(m);
            set_plain(m, p::DETECT, 0.0);
        }, p::DETECT, 1.0);
        // The key changes the gain on purpose, so use gentle times: the step
        // must come from the (smooth) gain move only, not from a switch.
        let calm = |m: &mut Mbdyn| {
            punchy(m);
            all_bands(m, p::ATTACK, 5.0);
            all_bands(m, p::RELEASE, 60.0);
            all_bands(m, p::RATIO_UP, 1.0);
        };
        assert_no_click("sidechain int→ext", &calm, p::SIDECHAIN, 1.0);
        assert_no_click("sidechain ext→int", &|m| {
            calm(m);
            set_plain(m, p::SIDECHAIN, 1.0);
        }, p::SIDECHAIN, 0.0);
        assert_no_click("sidechain int→ext (linear)", &|m| {
            calm(m);
            set_plain(m, p::MODE, 1.0);
        }, p::SIDECHAIN, 1.0);
    }

    #[test]
    fn automating_mute_and_solo_does_not_click() {
        assert_no_click("mute low", &punchy, p::band(0, p::MUTE), 1.0);
        assert_no_click("unmute low", &|m| {
            punchy(m);
            set_plain(m, p::band(0, p::MUTE), 1.0);
        }, p::band(0, p::MUTE), 0.0);
        assert_no_click("solo high", &punchy, p::band(2, p::SOLO), 1.0);
        assert_no_click("unsolo high", &|m| {
            punchy(m);
            set_plain(m, p::band(2, p::SOLO), 1.0);
        }, p::band(2, p::SOLO), 0.0);
    }
}
