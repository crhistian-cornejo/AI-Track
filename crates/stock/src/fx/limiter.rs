//! `fx.limiter` — lookahead brickwall limiter with true-peak detection,
//! selectable release/attack characters, channel modes, loudness metering,
//! auto gain, dither and a difference monitor.
//!
//! Per sample the gain the peak needs (`ceiling / peak`) goes through a
//! sliding minimum as long as the lookahead. A dual release envelope (fast
//! and slow one-poles, instantly attacking to the minimum) feeds a cascade
//! of one or two box smoothers whose total support is the attack time, so the
//! gain reaches its target smoothly before the peak arrives and never rises
//! above what any sample in its window needs. Styles change the attack kernel
//! (single box vs. second-order S-curve), its length, and the fast/slow
//! release ratios and blend — not just a time multiplier.
//!
//! With true peak on, detection uses a 4× polyphase interpolator (BS.1770
//! method) plus the sample values, held over the interpolator's span. With
//! oversampling (2×/4×, polyphase IIR) the whole limiter runs at the high
//! rate; a short base-rate true-peak stage then catches what the decimation
//! filter's ringing adds. A final clamp at the ceiling guarantees the sample
//! peak. Mid/Side mode limits M and S separately and the base-rate safety
//! stage catches any L/R reconstruction peak.
//!
//! `latency()` = core lookahead (in base samples, integer) + oversampler
//! group delay (rounded to a sample) + safety lookahead when it runs.
//!
//! Meter layout: `[0]` gain reduction dB (≤ 0), `[1]` output true peak dBTP,
//! `[2]` input true peak dBTP, `[3]` LUFS integrated, `[4]` LUFS short-term,
//! `[5]` LRA LU, `[6]` LUFS momentary.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::dynamics::{BoxFilter, SlidingMax, TruePeak};
use aitrack_dsp::loudness::LufsMeter;
use aitrack_dsp::osc::Noise;
use aitrack_dsp::oversample::Oversampler;
use aitrack_dsp::smooth::{Smoother, one_pole_coeff};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.limiter";

pub mod p {
    pub const GAIN: u32 = 0;
    pub const CEILING: u32 = 1;
    pub const RELEASE: u32 = 2;
    pub const AUTO_RELEASE: u32 = 3;
    pub const LOOKAHEAD: u32 = 4;
    pub const TRUE_PEAK: u32 = 5;
    pub const OVERSAMPLING: u32 = 6;
    pub const LINK: u32 = 7;
    pub const STYLE: u32 = 8;
    pub const ATTACK: u32 = 9;
    pub const AUTO_ATTACK: u32 = 10;
    pub const CHANNEL: u32 = 11;
    pub const AUTO_GAIN: u32 = 12;
    pub const DITHER: u32 = 13;
    pub const SHAPING: u32 = 14;
    pub const DELTA: u32 = 15;
    pub const RESET: u32 = 16;
}

/// Style indices (parameters are stepped from 0).
pub mod style {
    pub const TRANSPARENT: f32 = 0.0;
    pub const PUNCHY: f32 = 1.0;
    pub const DYNAMIC: f32 = 2.0;
    pub const ALLROUND: f32 = 3.0;
    pub const AGGRESSIVE: f32 = 4.0;
    pub const BUS: f32 = 5.0;
}

pub mod channel {
    pub const STEREO: f32 = 0.0;
    pub const DUAL_MONO: f32 = 1.0;
    pub const MID_SIDE: f32 = 2.0;
}

const OFF_ON: &[&str] = &["Off", "On"];
const OS: &[&str] = &["1x", "2x", "4x"];
const STYLES: &[&str] = &["Transparent", "Punchy", "Dynamic", "Allround", "Aggressive", "Bus"];
const CHANNELS: &[&str] = &["Stereo", "Dual Mono", "Mid/Side"];
const DITHER: &[&str] = &["Off", "16 bit", "24 bit"];
const MAX_LOOKAHEAD_MS: f32 = 10.0;
/// Internal target below the ceiling.
const MARGIN_DB: f32 = 0.02;
const CONTROL: usize = 16;
/// Span of the true-peak hold (covers the interpolator's 16 taps).
const HOLD: usize = 17;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::GAIN, "Input Gain", 0.0, 24.0, 0.0).unit("dB").short("Gain"),
        ParamInfo::new(p::CEILING, "Ceiling", -12.0, 0.0, -1.0).unit("dB").short("Ceil"),
        ParamInfo::new(p::RELEASE, "Release", 1.0, 1_000.0, 80.0).unit("ms").log().short("Rel"),
        ParamInfo::new(p::AUTO_RELEASE, "Auto Release", 0.0, 1.0, 1.0).choices(OFF_ON).short("Auto"),
        ParamInfo::new(p::LOOKAHEAD, "Lookahead", 0.5, MAX_LOOKAHEAD_MS, 1.5).unit("ms").short("Look"),
        ParamInfo::new(p::TRUE_PEAK, "True Peak", 0.0, 1.0, 1.0).choices(OFF_ON).short("TP"),
        ParamInfo::new(p::OVERSAMPLING, "Oversampling", 0.0, 2.0, 0.0).choices(OS).short("OS"),
        ParamInfo::new(p::LINK, "Stereo Link", 0.0, 100.0, 100.0).unit("%").short("Link"),
        ParamInfo::new(p::STYLE, "Style", 0.0, 5.0, style::ALLROUND).choices(STYLES),
        ParamInfo::new(p::ATTACK, "Attack", 0.1, 10.0, 1.0).unit("ms").log().short("Att"),
        ParamInfo::new(p::AUTO_ATTACK, "Auto Attack", 0.0, 1.0, 0.0).choices(OFF_ON).short("Auto Att"),
        ParamInfo::new(p::CHANNEL, "Channel Mode", 0.0, 2.0, channel::STEREO).choices(CHANNELS).short("Chan"),
        ParamInfo::new(p::AUTO_GAIN, "Auto Gain", 0.0, 1.0, 0.0).choices(OFF_ON).short("Auto Gain"),
        ParamInfo::new(p::DITHER, "Dither", 0.0, 2.0, 0.0).choices(DITHER),
        ParamInfo::new(p::SHAPING, "Noise Shaping", 0.0, 1.0, 0.0).choices(OFF_ON).short("Shape"),
        ParamInfo::new(p::DELTA, "Delta", 0.0, 1.0, 0.0).choices(OFF_ON).short("Delta"),
        ParamInfo::new(p::RESET, "Reset Meters", 0.0, 1.0, 0.0).choices(OFF_ON).short("Reset"),
    ]
}

/// One release/attack character. Lengths and ratios, so styles are curves,
/// not single time multipliers.
#[derive(Clone, Copy, Debug)]
struct StyleSpec {
    /// Multiplies the attack knob.
    attack_scale: f32,
    /// Split of the attack kernel into two boxes (0 = single linear ramp).
    attack_split: f32,
    /// Fraction of the lookahead used when Auto Attack is on.
    auto_attack: f32,
    /// Multiplies the release knob.
    release_scale: f32,
    /// Fast release stage relative to the nominal release.
    fast_time: f32,
    /// Slow release stage relative to the nominal release.
    slow_time: f32,
    /// Blend toward the fast stage (0 = only slow, 1 = only fast).
    fast_share: f32,
    /// Strength of the program-dependent release when Auto Release is on.
    adapt: f32,
    /// Weight of a short linear attack path mixed with the main kernel.
    attack_mix: f32,
    /// Length of that short path, relative to the attack.
    attack_short: f32,
}

const STYLE_SPECS: [StyleSpec; 6] = [
    // Transparent: long S-curve attack, long dual release, gently adaptive.
    StyleSpec { attack_scale: 1.6, attack_split: 0.5, auto_attack: 0.8, release_scale: 1.8, fast_time: 0.6, slow_time: 2.2, fast_share: 0.35, adapt: 0.5, attack_mix: 0.0, attack_short: 0.25 },
    // Punchy: short attack with an early edge, fast release with a quick first stage.
    StyleSpec { attack_scale: 0.5, attack_split: 0.0, auto_attack: 0.25, release_scale: 0.6, fast_time: 0.3, slow_time: 1.5, fast_share: 0.7, adapt: 0.8, attack_mix: 0.65, attack_short: 0.2 },
    // Dynamic: medium S-curve, strongly program-dependent dual release.
    StyleSpec { attack_scale: 1.0, attack_split: 0.35, auto_attack: 0.5, release_scale: 1.0, fast_time: 0.25, slow_time: 2.5, fast_share: 0.5, adapt: 1.0, attack_mix: 0.3, attack_short: 0.3 },
    // Allround: balanced single-box attack with a little early edge.
    StyleSpec { attack_scale: 1.0, attack_split: 0.0, auto_attack: 0.4, release_scale: 1.0, fast_time: 0.4, slow_time: 1.8, fast_share: 0.5, adapt: 0.5, attack_mix: 0.25, attack_short: 0.5 },
    // Aggressive: very short, hard early attack and mostly fast release.
    StyleSpec { attack_scale: 0.35, attack_split: 0.0, auto_attack: 0.15, release_scale: 0.4, fast_time: 0.2, slow_time: 1.2, fast_share: 0.8, adapt: 0.25, attack_mix: 0.8, attack_short: 0.15 },
    // Bus: very smooth attack with an early bed, slow dual release.
    StyleSpec { attack_scale: 2.0, attack_split: 0.5, auto_attack: 0.85, release_scale: 3.0, fast_time: 0.7, slow_time: 2.8, fast_share: 0.25, adapt: 0.3, attack_mix: 0.4, attack_short: 0.4 },
];

/// Attack kernel shape: length plus a cascade and a short edge path.
#[derive(Clone, Copy, Debug)]
struct Curve {
    attack: usize,
    split: f32,
    mix: f32,
    short: f32,
}

/// One limiter running at one rate.
struct Core {
    tp_on: bool,
    tp: [TruePeak; 2],
    tp_prev: [f32; 2],
    xdel: [DelayLine; 2],
    hold: [SlidingMax; 2],
    smin: [SlidingMax; 2],
    rel_fast: [f32; 2],
    rel_slow: [f32; 2],
    fast_coeff: f32,
    slow_coeff: f32,
    blend: f32,
    box1: [BoxFilter; 2],
    box2: [BoxFilter; 2],
    box3: [BoxFilter; 2],
    w1: usize,
    w2: usize,
    w3: usize,
    mix: f32,
    main: [DelayLine; 2],
    main_delay: usize,
    max_window: usize,
    target: f32,
    link: f32,
    last_gain: f32,
}

impl Core {
    fn new(max_window: usize) -> Self {
        let max_window = max_window.max(1);
        let det = TruePeak::LATENCY + HOLD;
        Self {
            tp_on: true,
            tp: [TruePeak::new(), TruePeak::new()],
            tp_prev: [0.0; 2],
            xdel: [DelayLine::new(det), DelayLine::new(det)],
            hold: [SlidingMax::new(HOLD), SlidingMax::new(HOLD)],
            smin: [SlidingMax::new(max_window), SlidingMax::new(max_window)],
            rel_fast: [1.0; 2],
            rel_slow: [1.0; 2],
            fast_coeff: 0.001,
            slow_coeff: 0.001,
            blend: 0.5,
            box1: [BoxFilter::new(max_window), BoxFilter::new(max_window)],
            box2: [BoxFilter::new(max_window), BoxFilter::new(max_window)],
            box3: [BoxFilter::new(max_window), BoxFilter::new(max_window)],
            w1: 1,
            w2: 1,
            w3: 1,
            mix: 0.0,
            main: [DelayLine::new(max_window + det + 4), DelayLine::new(max_window + det + 4)],
            main_delay: 0,
            max_window,
            target: 1.0,
            link: 1.0,
            last_gain: 1.0,
        }
    }

    /// Detection delay in samples.
    fn det_delay(&self) -> usize {
        if self.tp_on { TruePeak::LATENCY + HOLD / 2 } else { 0 }
    }

    fn configure(&mut self, window: usize, tp_on: bool, curve: Curve, align: usize) {
        let w = window.clamp(1, self.max_window);
        self.tp_on = tp_on;
        let a = curve.attack.clamp(1, w);
        let (w1, w2) = if a < 2 || curve.split <= 0.001 {
            (a, 1)
        } else {
            let first = ((a as f32 * curve.split).round() as usize).clamp(1, a - 1);
            (first, a + 1 - first)
        };
        self.w1 = w1;
        self.w2 = w2;
        self.w3 = ((a as f32 * curve.short).round() as usize).clamp(1, a);
        self.mix = curve.mix.clamp(0.0, 1.0);
        for c in 0..2 {
            self.smin[c].set_window(w);
            self.hold[c].set_window(HOLD);
            self.box1[c].set_window(w1, 1.0);
            self.box2[c].set_window(w2, 1.0);
            self.box3[c].set_window(self.w3, 1.0);
        }
        // Round the lookahead up to a whole number of base-rate samples so
        // the reported latency is exact in the base-rate domain.
        let raw = self.det_delay() + w - 1;
        self.main_delay = raw.div_ceil(align.max(1)) * align.max(1);
        self.reset();
    }

    fn latency(&self) -> usize {
        self.main_delay
    }

    fn reset(&mut self) {
        for c in 0..2 {
            self.tp[c].reset();
            self.xdel[c].clear();
            self.hold[c].reset();
            self.smin[c].reset();
            self.box1[c].set_window(self.w1, 1.0);
            self.box2[c].set_window(self.w2, 1.0);
            self.box3[c].set_window(self.w3, 1.0);
            self.main[c].clear();
        }
        self.tp_prev = [0.0; 2];
        self.rel_fast = [1.0; 2];
        self.rel_slow = [1.0; 2];
        self.last_gain = 1.0;
    }

    fn set_release(&mut self, fast_ms: f32, slow_ms: f32, sample_rate: f64) {
        self.fast_coeff = one_pole_coeff(fast_ms.max(0.05), sample_rate);
        self.slow_coeff = one_pole_coeff(slow_ms.max(fast_ms).max(0.05), sample_rate);
    }

    #[inline]
    fn detect(&mut self, c: usize, x: f32) -> f32 {
        if self.tp_on {
            let t = self.tp[c].process(x);
            self.xdel[c].push(x);
            let xs = self.xdel[c].read(TruePeak::LATENCY).abs();
            let v = t.max(self.tp_prev[c]).max(xs);
            self.tp_prev[c] = t;
            self.hold[c].process(v)
        } else {
            x.abs()
        }
    }

    #[inline]
    fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let x = [l, r];
        let mut req = [1.0f32; 2];
        for c in 0..2 {
            let d = self.detect(c, x[c]);
            if d > self.target {
                req[c] = self.target / d;
            }
        }
        let linked = req[0].min(req[1]);
        let mut out = [0.0f32; 2];
        let mut gmin = 1.0f32;
        for c in 0..2 {
            let g = req[c] + (linked - req[c]) * self.link;
            let m = 1.0 - self.smin[c].process(1.0 - g);
            let rf = &mut self.rel_fast[c];
            if m < *rf { *rf = m } else { *rf += (m - *rf) * self.fast_coeff }
            let rs = &mut self.rel_slow[c];
            if m < *rs { *rs = m } else { *rs += (m - *rs) * self.slow_coeff }
            let rel = *rs + (*rf - *rs) * self.blend;
            let smooth = self.box2[c].process(self.box1[c].process(rel));
            let edge = self.box3[c].process(rel);
            let gs = (smooth + (edge - smooth) * self.mix).min(1.0);
            self.main[c].push(x[c]);
            out[c] = self.main[c].read(self.main_delay) * gs;
            gmin = gmin.min(gs);
        }
        self.last_gain = gmin;
        (out[0], out[1])
    }
}

/// Configuration that changes the DSP topology (and therefore the reported
/// latency).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Config {
    factor: usize,
    window: usize,
    attack: usize,
    tp: bool,
    /// Fixed-point split/edge for cheap comparison.
    split: u32,
    mix: u32,
    short: u32,
    safety: bool,
}

pub struct Limiter {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    core: Core,
    safety: Core,
    os: [Oversampler; 2],
    /// Rounded oversampler round-trip latency per factor (1, 2, 4).
    os_lat: [usize; 5],
    factor: usize,
    ceiling: f32,
    drive: Smoother,
    release_ms: f32,
    release_base: f32,
    auto_release: bool,
    style: StyleSpec,
    style_idx: usize,
    channel: usize,
    link: f32,
    auto_gain: bool,
    auto_makeup: Smoother,
    gr_slow: f32,
    slow_coeff: f32,
    dither_scale: f32,
    shaper: bool,
    noise: [Noise; 2],
    err: [[f32; 2]; 2],
    delta: [DelayLine; 2],
    delta_on: bool,
    safety_on: bool,
    in_tp: [TruePeak; 2],
    out_tp: [TruePeak; 2],
    lufs: LufsMeter,
    meter_gr: f32,
    meter_tp_out: f32,
    meter_tp_in: f32,
    reset_armed: bool,
    config: Config,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Limiter::new())
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Limiter {
    pub fn new() -> Self {
        let params = params();
        let mut l = Self {
            info: PluginInfo {
                id: ID,
                name: "Limiter",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["dynamics", "limiter", "true peak", "master", "loudness"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            core: Core::new(1),
            safety: Core::new(1),
            os: [Oversampler::new(1), Oversampler::new(1)],
            os_lat: [0; 5],
            factor: 1,
            ceiling: db_to_gain(-1.0),
            drive: Smoother::new(1.0),
            release_ms: 80.0,
            release_base: 80.0,
            auto_release: true,
            style: STYLE_SPECS[style::ALLROUND as usize],
            style_idx: style::ALLROUND as usize,
            channel: 0,
            link: 1.0,
            auto_gain: false,
            auto_makeup: Smoother::new(1.0),
            gr_slow: 0.0,
            slow_coeff: 0.0,
            dither_scale: 0.0,
            shaper: false,
            noise: [Noise::new(0x51ed_2701), Noise::new(0x1b3f_9a2d)],
            err: [[0.0; 2]; 2],
            delta: [DelayLine::new(1), DelayLine::new(1)],
            delta_on: false,
            safety_on: false,
            in_tp: [TruePeak::new(), TruePeak::new()],
            out_tp: [TruePeak::new(), TruePeak::new()],
            lufs: LufsMeter::new(48_000.0),
            meter_gr: 0.0,
            meter_tp_out: -120.0,
            meter_tp_in: -120.0,
            reset_armed: true,
            config: Config { factor: 0, window: 0, attack: 0, tp: false, split: 0, mix: 0, short: 0, safety: false },
            first: true,
        };
        l.prepare(48_000.0, 512);
        l
    }

    fn os_factor(&self) -> usize {
        [1usize, 2, 4][self.params.choice(p::OVERSAMPLING).min(2)]
    }

    fn lookahead_window(&self, factor: usize) -> usize {
        let samples = (self.params.plain(p::LOOKAHEAD).max(0.1) * 0.001) as f64 * self.sr * factor as f64;
        (samples.round() as usize).max(1)
    }

    fn resolved_safety_on(&self, factor: usize) -> bool {
        factor > 1 || self.params.choice(p::CHANNEL) == channel::MID_SIDE as usize || self.params.on(p::AUTO_GAIN)
    }

    fn safety_window(&self) -> usize {
        ((0.5e-3 * self.sr).round() as usize).max(1)
    }

    fn safety_latency(&self) -> usize {
        TruePeak::LATENCY + HOLD / 2 + self.safety_window() - 1
    }

    fn attack_samples(&self, factor: usize, window: usize) -> usize {
        let st = self.style;
        let a = if self.params.on(p::AUTO_ATTACK) {
            (st.auto_attack * window as f32).round() as usize
        } else {
            let ms = self.params.plain(p::ATTACK) * st.attack_scale;
            (ms as f64 * 0.001 * self.sr * factor as f64).round() as usize
        };
        a.clamp(1, window)
    }

    fn gain_time(&mut self, sample_rate: f64) {
        self.drive.set_time(20.0, sample_rate);
        self.auto_makeup.set_time(250.0, sample_rate);
        self.slow_coeff = one_pole_coeff(600.0, sample_rate / CONTROL as f64);
    }

    fn update(&mut self) {
        let (gain_db, ceiling_db, style_idx, auto_release, channel, link, auto_gain, delta_on, dither, shaping, release, tp, reset) = {
            let ps = &self.params;
            (
                ps.plain(p::GAIN),
                ps.plain(p::CEILING),
                ps.choice(p::STYLE).min(STYLE_SPECS.len() - 1),
                ps.on(p::AUTO_RELEASE),
                ps.choice(p::CHANNEL).min(2),
                ps.plain(p::LINK) / 100.0,
                ps.on(p::AUTO_GAIN),
                ps.on(p::DELTA),
                ps.choice(p::DITHER).min(2),
                ps.on(p::SHAPING),
                ps.plain(p::RELEASE),
                ps.on(p::TRUE_PEAK),
                ps.on(p::RESET),
            )
        };
        self.ceiling = db_to_gain(ceiling_db);
        let target = self.ceiling * db_to_gain(-MARGIN_DB);
        self.style_idx = style_idx;
        self.style = STYLE_SPECS[style_idx];
        self.auto_release = auto_release;
        self.channel = channel;
        self.link = if channel == channel::DUAL_MONO as usize { 0.0 } else { link };
        self.auto_gain = auto_gain;
        self.delta_on = delta_on;
        self.dither_scale = match dither {
            1 => 32_768.0,
            2 => 8_388_608.0,
            _ => 0.0,
        };
        self.shaper = shaping && self.dither_scale > 0.0;
        self.drive.set_target(db_to_gain(gain_db));
        if self.first {
            self.drive.snap_to_target();
        }
        self.release_ms = release;
        self.release_base = (self.release_ms * self.style.release_scale).clamp(1.0, 5_000.0);
        let factor = self.os_factor();
        let window = self.lookahead_window(factor);
        let attack = self.attack_samples(factor, window);
        let safety = self.resolved_safety_on(factor);
        let split = self.style.attack_split.to_bits();
        let mix = self.style.attack_mix.to_bits();
        let short = self.style.attack_short.to_bits();
        let cfg = Config { factor, window, attack, tp, split, mix, short, safety };
        if factor != self.factor {
            self.factor = factor;
            for o in &mut self.os {
                o.set_factor(factor);
            }
        }
        if cfg != self.config || self.first {
            self.config = cfg;
            let curve = Curve { attack, split: self.style.attack_split, mix: self.style.attack_mix, short: self.style.attack_short };
            self.core.configure(window, tp, curve, factor);
            self.core.target = target;
            self.core.link = self.link;
            self.safety.configure(self.safety_window(), true, Curve { attack: self.safety_window(), split: 0.0, mix: 0.0, short: 1.0 }, 1);
            self.safety.target = target;
            self.safety.link = 1.0;
            self.safety.set_release(15.0, 60.0, self.sr);
            self.safety.blend = 0.6;
            // A visibly different release is part of each style.
            self.apply_release(self.release_base);
        } else {
            self.core.target = target;
            self.safety.target = target;
            self.core.link = self.link;
        }
        // Release coefficients are re-applied on every update so a style
        // change (or a knob move) is heard immediately.
        if !self.auto_release {
            self.apply_release(self.release_base);
        }
        self.safety_on = safety;
        if !self.auto_gain {
            self.auto_makeup.set_target(1.0);
        }
        if reset && self.reset_armed {
            self.reset_meters();
        }
        self.reset_armed = !reset;
        self.first = false;
    }

    fn apply_release(&mut self, base_ms: f32) {
        let fast = (base_ms * self.style.fast_time).clamp(0.1, 5_000.0);
        let slow = (base_ms * self.style.slow_time).clamp(fast, 10_000.0);
        let sr = self.sr * self.factor as f64;
        self.core.set_release(fast, slow, sr);
        self.core.blend = self.style.fast_share;
    }

    /// Auto release / auto gain at control rate.
    fn control(&mut self) {
        let gr = -gain_to_db(self.core.last_gain);
        self.gr_slow += (gr - self.gr_slow) * self.slow_coeff;
        if self.auto_release {
            let sustain = if gr > 0.05 { (self.gr_slow / gr).clamp(0.0, 1.0) } else { 1.0 };
            let shape = 0.3 + 1.7 * sustain;
            let adapt = 1.0 + self.style.adapt * (shape - 1.0);
            self.apply_release((self.release_base * adapt).clamp(1.0, 5_000.0));
        }
        let makeup = if self.auto_gain { self.gr_slow.clamp(0.0, 24.0) } else { 0.0 };
        self.auto_makeup.set_target(db_to_gain(makeup));
    }

    fn reset_meters(&mut self) {
        self.lufs.reset();
        for t in &mut self.in_tp {
            t.reset();
        }
        for t in &mut self.out_tp {
            t.reset();
        }
        self.meter_gr = 0.0;
        self.meter_tp_out = -120.0;
        self.meter_tp_in = -120.0;
    }

    #[inline]
    fn dither(&mut self, c: usize, x: f32) -> f32 {
        if self.dither_scale == 0.0 {
            return x;
        }
        let lsb = 1.0 / self.dither_scale;
        let mut v = x;
        if self.shaper {
            v += 1.5 * self.err[c][0] - 0.5 * self.err[c][1];
        }
        let a = self.noise[c].next();
        let b = self.noise[c].next();
        v += (a - b) * 0.5 * lsb;
        let q = (v * self.dither_scale).round() / self.dither_scale;
        if self.shaper {
            self.err[c][1] = self.err[c][0];
            self.err[c][0] = q - v;
        }
        q
    }
}

impl Segmented for Limiter {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let ceil = self.ceiling;
        let factor = self.factor;
        let mode = self.channel;
        let safety_on = self.safety_on;
        let delta_on = self.delta_on;
        let latency = self.core.latency() / factor
            + if factor > 1 { self.os_lat[factor] } else { 0 }
            + if safety_on { self.safety.latency() } else { 0 };
        let mut gr_min = 1.0f32;
        let mut tp_out = db_to_gain(self.meter_tp_out) * 0.7;
        let mut tp_in = db_to_gain(self.meter_tp_in) * 0.7;
        let mut i = start;
        while i < end {
            let e = (i + CONTROL).min(end);
            if self.auto_release || self.auto_gain {
                self.control();
            }
            let this = &mut *self;
            for_frames(ctx.buffer, i, e, |_, l, r| {
                let g = this.drive.next();
                let l = sanitize(l * g);
                let r = sanitize(r * g);
                // This is the signal the limiter is asked to remove from.
                this.delta[0].push(l);
                this.delta[1].push(r);
                let inv = std::f32::consts::FRAC_1_SQRT_2;
                let mut ul = [0.0f32; 4];
                let mut ur = [0.0f32; 4];
                this.os[0].upsample(l, &mut ul);
                this.os[1].upsample(r, &mut ur);
                match mode {
                    m if m == channel::MID_SIDE as usize => {
                        for k in 0..factor {
                            let m = (ul[k] + ur[k]) * inv;
                            let s = (ul[k] - ur[k]) * inv;
                            let (m, s) = this.core.process(m, s);
                            ul[k] = (m + s) * inv;
                            ur[k] = (m - s) * inv;
                        }
                    }
                    _ => {
                        for k in 0..factor {
                            let (a, b) = this.core.process(ul[k], ur[k]);
                            ul[k] = a;
                            ur[k] = b;
                        }
                    }
                }
                let mut ol = this.os[0].downsample(&ul);
                let mut or = this.os[1].downsample(&ur);
                let mk = this.auto_makeup.next();
                ol *= mk;
                or *= mk;
                if safety_on {
                    let (a, b) = this.safety.process(ol, or);
                    ol = a;
                    or = b;
                }
                ol = sanitize(ol).clamp(-ceil, ceil);
                or = sanitize(or).clamp(-ceil, ceil);
                let ol = this.dither(0, ol);
                let or = this.dither(1, or);
                tp_in = tp_in.max(this.in_tp[0].process(l)).max(this.in_tp[1].process(r));
                tp_out = tp_out.max(this.out_tp[0].process(ol)).max(this.out_tp[1].process(or));
                this.lufs.push(ol, or);
                gr_min = gr_min.min(this.core.last_gain * if safety_on { this.safety.last_gain } else { 1.0 });
                if delta_on {
                    let dl = this.delta[0].read(latency) - ol;
                    let dr = this.delta[1].read(latency) - or;
                    (sanitize(dl), sanitize(dr))
                } else {
                    (ol, or)
                }
            });
            i = e;
        }
        self.meter_gr = gain_to_db(gr_min).min(0.0);
        self.meter_tp_out = gain_to_db(tp_out).max(-120.0);
        self.meter_tp_in = gain_to_db(tp_in).max(-120.0);
    }
}

impl Processor for Limiter {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        let max_w = ((MAX_LOOKAHEAD_MS * 0.001) as f64 * sample_rate * 4.0).ceil() as usize + 2;
        self.core = Core::new(max_w);
        let sw = self.safety_window();
        self.safety = Core::new((sw + 8).max(16));
        self.safety.configure(sw, true, Curve { attack: sw, split: 0.0, mix: 0.0, short: 1.0 }, 1);
        self.safety.set_release(15.0, 60.0, sample_rate);
        self.safety.blend = 0.6;
        self.delta = [DelayLine::new(max_w + 256), DelayLine::new(max_w + 256)];
        self.os_lat = [0; 5];
        for f in [2usize, 4] {
            self.os_lat[f] = Oversampler::new(f).latency().round() as usize;
        }
        self.gain_time(sample_rate);
        self.lufs = LufsMeter::new(sample_rate);
        self.params.mark_dirty();
        self.first = true;
        self.config = Config { factor: 0, window: 0, attack: 0, tp: false, split: 0, mix: 0, short: 0, safety: false };
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.core.reset();
        self.safety.reset();
        for o in &mut self.os {
            o.reset();
        }
        for d in &mut self.delta {
            d.clear();
        }
        self.drive.snap_to_target();
        self.auto_makeup.snap(1.0);
        self.gr_slow = 0.0;
        self.err = [[0.0; 2]; 2];
        self.reset_meters();
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
        // Resolve from the parameters even before the next block so hosts
        // schedule delay compensation with the value that will be used.
        let factor = self.os_factor();
        let window = self.lookahead_window(factor);
        let tp = self.params.on(p::TRUE_PEAK);
        let det = if tp { TruePeak::LATENCY + HOLD / 2 } else { 0 };
        let raw = det + window - 1;
        let core = raw.div_ceil(factor);
        let safety = if self.resolved_safety_on(factor) { self.safety_latency() } else { 0 };
        let os = if factor > 1 { self.os_lat[factor] } else { 0 };
        (core + safety + os) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let r = self.lufs.reading();
        write_meter(
            out,
            0,
            &[self.meter_gr, self.meter_tp_out, self.meter_tp_in, r.integrated, r.short_term, r.range, r.momentary],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{process_stereo, set_plain};
    use aitrack_dsp::osc::Noise;

    /// Worst output true peak (dBTP) and sample peak after the latency.
    fn worst(lim: &mut Limiter, input: &mut dyn FnMut(usize) -> (f32, f32), n: usize) -> (f32, f32) {
        let (l, r) = process_stereo(lim, input, n);
        let skip = lim.latency() as usize + 64;
        let mut tp = [TruePeak::new(), TruePeak::new()];
        let mut worst_tp = 0.0f32;
        let mut worst_sp = 0.0f32;
        for i in 0..n {
            let a = tp[0].process(l[i]);
            let b = tp[1].process(r[i]);
            if i > skip {
                worst_tp = worst_tp.max(a).max(b);
                worst_sp = worst_sp.max(l[i].abs()).max(r[i].abs());
            }
        }
        (gain_to_db(worst_tp), gain_to_db(worst_sp))
    }

    #[test]
    fn never_exceeds_the_ceiling() {
        let n = 48_000;
        type Signal = Box<dyn Fn() -> Box<dyn FnMut(usize) -> (f32, f32)>>;
        let signals: Vec<(&str, Signal)> = vec![
            (
                "noise +12",
                Box::new(|| {
                    let mut a = Noise::new(1);
                    let mut b = Noise::new(2);
                    Box::new(move |_| (a.next() * 4.0, b.next() * 4.0))
                }),
            ),
            (
                "fs/4 45°",
                Box::new(|| {
                    Box::new(|i| {
                        let v = 2.0 * (std::f32::consts::FRAC_PI_2 * i as f32 + std::f32::consts::FRAC_PI_4).sin();
                        (v, -v)
                    })
                }),
            ),
            ("square", Box::new(|| Box::new(|i| if (i / 37) % 2 == 0 { (1.8, 1.8) } else { (-1.8, -1.8) }))),
            (
                "drums",
                Box::new(|| {
                    let mut nz = Noise::new(9);
                    Box::new(move |i| {
                        let t = (i % 6000) as f32 / 48_000.0;
                        let kick = 3.0 * (-t * 30.0).exp() * (std::f32::consts::TAU * 55.0 * t * (1.0 + 2.0 * (-t * 40.0).exp())).sin();
                        let click = if i % 6000 == 0 { 4.0 } else { 0.0 };
                        let hat = if (i + 3000) % 6000 < 400 { nz.next() * 2.0 } else { 0.0 };
                        let v = kick + click + hat;
                        (v, v * 0.8)
                    })
                }),
            ),
            (
                "hf sines",
                Box::new(|| {
                    Box::new(|i| {
                        let t = i as f64 / 48_000.0;
                        let v = 1.5 * (std::f64::consts::TAU * 19_000.0 * t).sin() + 0.8 * (std::f64::consts::TAU * 11_123.0 * t).sin();
                        (v as f32, (v * 0.7) as f32)
                    })
                }),
            ),
        ];
        let mut excess = f32::MIN;
        for os in [0.0f32, 1.0, 2.0] {
            for ceiling in [-1.0f32, -0.1, -6.0] {
                for (name, make) in &signals {
                    let mut lim = Limiter::new();
                    set_plain(&mut lim, p::OVERSAMPLING, os);
                    set_plain(&mut lim, p::CEILING, ceiling);
                    set_plain(&mut lim, p::GAIN, 6.0);
                    let mut sig = make();
                    let (tp, sp) = worst(&mut lim, &mut *sig, n);
                    excess = excess.max(tp - ceiling);
                    assert!(tp <= ceiling + 0.05, "{name} os {os} ceiling {ceiling}: true peak {tp} dBTP");
                    assert!(sp <= ceiling + 1e-4, "{name} os {os} ceiling {ceiling}: sample peak {sp}");
                }
            }
        }
        println!("worst true peak relative to the ceiling: {excess:+.3} dB");
    }

    #[test]
    fn ceiling_is_kept_in_every_channel_mode() {
        let n = 24_000;
        for mode in [channel::STEREO, channel::DUAL_MONO, channel::MID_SIDE] {
            for os in [0.0f32, 1.0, 2.0] {
                let mut lim = Limiter::new();
                set_plain(&mut lim, p::CHANNEL, mode);
                set_plain(&mut lim, p::OVERSAMPLING, os);
                set_plain(&mut lim, p::GAIN, 8.0);
                let (tp, sp) = worst(
                    &mut lim,
                    &mut |i| {
                        let t = i as f32 / 48_000.0;
                        let l = 2.0 * (std::f32::consts::TAU * 997.0 * t).sin() + 0.7 * (std::f32::consts::TAU * 15_000.0 * t).sin();
                        let r = 1.4 * (std::f32::consts::TAU * 1_511.0 * t).sin() + 1.1 * (std::f32::consts::TAU * 13_000.0 * t).sin();
                        (l, r)
                    },
                    n,
                );
                assert!(tp <= -1.0 + 0.05, "mode {mode} os {os}: {tp} dBTP");
                assert!(sp <= -1.0 + 1e-4, "mode {mode} os {os}: {sp} dBFS");
            }
        }
    }

    #[test]
    fn quiet_signal_passes_unchanged_after_latency() {
        let mut lim = Limiter::new();
        let lat = lim.latency() as usize;
        let sig = |i: usize| {
            let v = 0.3 * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin();
            (v, v)
        };
        let (l, _) = process_stereo(&mut lim, sig, 9600);
        for i in lat + 100..9600 {
            assert!((l[i] - sig(i - lat).0).abs() < 1e-4, "at {i}");
        }
    }

    #[test]
    fn latency_matches_the_impulse_position() {
        for os in [0.0f32, 1.0, 2.0] {
            for mode in [channel::STEREO, channel::MID_SIDE] {
                let mut lim = Limiter::new();
                set_plain(&mut lim, p::OVERSAMPLING, os);
                set_plain(&mut lim, p::CHANNEL, mode);
                set_plain(&mut lim, p::AUTO_GAIN, 0.0);
                let lat = lim.latency() as usize;
                // Exact base-rate PDC at 48 kHz for the default 1.5 ms
                // lookahead: 1×/2×/4× = 87/123/120 samples; Mid/Side adds
                // the 39-sample base-rate safety stage at 1×.
                let want_lat = match (os as usize, mode as usize) {
                    (0, 0) => 87,
                    (0, 2) => 126,
                    (1, 0) | (1, 2) => 123,
                    (2, 0) | (2, 2) => 120,
                    _ => unreachable!(),
                };
                assert_eq!(lat, want_lat, "os {os} mode {mode}: declared latency");
                let (l, _) = process_stereo(&mut lim, |i| if i == 200 { (0.25, 0.25) } else { (0.0, 0.0) }, 3000);
                let peak = (0..3000).max_by(|a, b| l[*a].abs().total_cmp(&l[*b].abs())).unwrap();
                let want = 200 + lat;
                assert_eq!(peak, want, "os {os} mode {mode}: peak {peak}, declared {want} (latency {lat})");
            }
        }
    }

    #[test]
    fn latency_reports_lookahead() {
        let mut lim = Limiter::new();
        set_plain(&mut lim, p::TRUE_PEAK, 0.0);
        set_plain(&mut lim, p::LOOKAHEAD, 2.0);
        assert_eq!(lim.latency(), 95);
        let base = lim.latency();
        set_plain(&mut lim, p::OVERSAMPLING, 2.0);
        assert!(lim.latency() > base);
        let mut m = [0.0f32; 7];
        assert_eq!(lim.meter(&mut m), 7);
    }

    #[test]
    fn delta_plus_output_is_the_input() {
        for os in [0.0f32, 1.0, 2.0] {
            let sig = |i: usize| {
                let t = i as f32 / 48_000.0;
                let v = 1.7 * (std::f32::consts::TAU * 330.0 * t).sin() + 0.5 * (std::f32::consts::TAU * 17_000.0 * t).sin();
                (v, v * 0.6)
            };
            let mut out = Limiter::new();
            set_plain(&mut out, p::OVERSAMPLING, os);
            set_plain(&mut out, p::GAIN, 0.0);
            let lat = out.latency() as usize;
            let (ol, or) = process_stereo(&mut out, sig, 12_000);
            let mut lim = Limiter::new();
            set_plain(&mut lim, p::OVERSAMPLING, os);
            set_plain(&mut lim, p::GAIN, 0.0);
            set_plain(&mut lim, p::DELTA, 1.0);
            let (l, r) = process_stereo(&mut lim, sig, 12_000);
            for i in lat + 100..12_000 {
                let (in_l, in_r) = sig(i - lat);
                assert!((l[i] + ol[i] - in_l).abs() < 2e-5, "os {os} L at {i}: delta {} + out {} != in {}", l[i], ol[i], in_l);
                assert!((r[i] + or[i] - in_r).abs() < 2e-5, "os {os} R at {i}");
            }
        }
    }

    #[test]
    fn delta_tracks_removed_level_above_unity() {
        // A difference monitor reproduces the removed signal, so with input
        // gain it can legitimately be louder than the brickwalled output.
        let sig = |i: usize| {
            let v = 0.5 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin();
            (v, v)
        };
        let mut out = Limiter::new();
        set_plain(&mut out, p::GAIN, 24.0);
        set_plain(&mut out, p::AUTO_GAIN, 0.0);
        let lat = out.latency() as usize;
        let (ol, _) = process_stereo(&mut out, sig, 12_000);
        let mut lim = Limiter::new();
        set_plain(&mut lim, p::GAIN, 24.0);
        set_plain(&mut lim, p::AUTO_GAIN, 0.0);
        set_plain(&mut lim, p::DELTA, 1.0);
        let (l, _) = process_stereo(&mut lim, sig, 12_000);
        let peak = l[lat + 100..].iter().fold(0.0f32, |a, v| a.max(v.abs()));
        assert!(peak > 2.0, "delta should expose the removed level, peak {peak}");
        assert!(peak.is_finite() && peak < 1e3, "delta must stay bounded, peak {peak}");
        let gain = db_to_gain(24.0);
        for i in lat + 100..12_000 {
            let (in_l, _) = sig(i - lat);
            assert!((l[i] + ol[i] - in_l * gain).abs() < 2e-4, "delta {i}");
        }
    }

    #[test]
    fn styles_change_the_envelope() {
        let n = 9_000;
        let mut curves = Vec::new();
        for st in [0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0] {
            let mut lim = Limiter::new();
            set_plain(&mut lim, p::STYLE, st);
            set_plain(&mut lim, p::AUTO_RELEASE, 0.0);
            set_plain(&mut lim, p::GAIN, 9.0);
            let (l, _) = process_stereo(
                &mut lim,
                |i| {
                    // Loud burst, then a quiet tone: the release envelope
                    // modulates the quiet tone, so its shape is measurable.
                    let amp = if (2_000..6_800).contains(&i) { 0.9 } else { 0.05 };
                    let v = amp * (std::f32::consts::TAU * 120.0 * i as f32 / 48_000.0).sin();
                    (v, v)
                },
                n,
            );
            // Envelope after the burst ends, on a log scale.
            let tail: Vec<f32> = (6_900..8_900).map(|i| gain_to_db(l[i].abs().max(1e-6))).collect();
            curves.push(tail);
        }
        for a in 0..6 {
            for b in a + 1..6 {
                let diff = curves[a].iter().zip(&curves[b]).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
                assert!(diff > 0.5, "styles {a} and {b} are too similar ({diff:.2} dB)");
            }
        }
    }

    #[test]
    fn dual_mono_does_not_link_channels() {
        let mut lim = Limiter::new();
        set_plain(&mut lim, p::CHANNEL, channel::DUAL_MONO);
        set_plain(&mut lim, p::GAIN, 0.0);
        let (l, r) = process_stereo(
            &mut lim,
            |i| {
                let t = i as f32 / 48_000.0;
                let loud = 1.8 * (std::f32::consts::TAU * 500.0 * t).sin();
                let quiet = 0.1 * (std::f32::consts::TAU * 500.0 * t).sin();
                (loud, quiet)
            },
            24_000,
        );
        let tail = 12_000;
        let lp = l[tail..].iter().fold(0.0f32, |a, v| a.max(v.abs()));
        let rp = r[tail..].iter().fold(0.0f32, |a, v| a.max(v.abs()));
        assert!(gain_to_db(lp) <= -1.0 + 1e-3);
        assert!((gain_to_db(rp) - gain_to_db(0.1)).abs() < 0.05, "quiet channel was reduced: {rp}");
    }

    #[test]
    fn mid_side_limits_the_mid_alone() {
        let mut lim = Limiter::new();
        set_plain(&mut lim, p::CHANNEL, channel::MID_SIDE);
        set_plain(&mut lim, p::GAIN, 0.0);
        let (l, r) = process_stereo(
            &mut lim,
            |i| {
                let v = 2.0 * (std::f32::consts::TAU * 700.0 * i as f32 / 48_000.0).sin();
                (v, v)
            },
            24_000,
        );
        for i in 12_000..24_000 {
            assert!((l[i] - r[i]).abs() < 1e-6, "mid-only content lost its balance at {i}");
            assert!(l[i].abs() <= 0.892, "mid not limited at {i}");
        }
    }

    #[test]
    fn loudness_meters_read_a_known_level() {
        let mut lim = Limiter::new();
        set_plain(&mut lim, p::GAIN, 0.0);
        set_plain(&mut lim, p::CEILING, 0.0);
        set_plain(&mut lim, p::LOOKAHEAD, 0.5);
        let amp = 0.1; // −20 dBFS, both channels
        let (l, _) = process_stereo(
            &mut lim,
            |i| {
                let v = amp * (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin();
                (v, v)
            },
            4 * 48_000,
        );
        assert!(l.last().unwrap().abs() < 0.2);
        let mut m = [0.0f32; 7];
        lim.meter(&mut m);
        assert!((m[3] + 20.0).abs() < 0.3, "integrated {}", m[3]);
        assert!((m[4] + 20.0).abs() < 0.3, "short-term {}", m[4]);
        assert!(m[5] < 1.0, "LRA {}", m[5]);
    }

    #[test]
    fn auto_gain_does_not_break_the_ceiling() {
        let mut lim = Limiter::new();
        set_plain(&mut lim, p::AUTO_GAIN, 1.0);
        set_plain(&mut lim, p::GAIN, 6.0);
        let (tp, sp) = worst(
            &mut lim,
            &mut |i| {
                let v = 1.6 * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin();
                (v, v)
            },
            48_000,
        );
        assert!(tp <= -1.0 + 0.05, "{tp} dBTP");
        assert!(sp <= -1.0 + 1e-4);
    }

    #[test]
    fn dither_quantizes_to_the_selected_depth() {
        let mut lim = Limiter::new();
        set_plain(&mut lim, p::DITHER, 1.0); // 16 bit
        set_plain(&mut lim, p::GAIN, 0.0);
        let (l, _) = process_stereo(
            &mut lim,
            |i| {
                let v = 0.2 * (std::f32::consts::TAU * 997.0 * i as f32 / 48_000.0).sin();
                (v, v)
            },
            4_800,
        );
        for v in &l[1_000..] {
            let q = (*v * 32_768.0).round() / 32_768.0;
            assert!((*v - q).abs() < 0.5 / 32_768.0 + 1e-6, "{v}");
        }
    }

    #[test]
    fn old_state_still_loads() {
        let old = br#"{"version":1,"params":{"0":0.2,"1":0.5,"2":0.6,"3":1.0,"4":0.2,"5":1.0,"6":0.0,"7":1.0}}"#;
        let mut lim = Limiter::new();
        lim.load_state(old);
        assert!((lim.get_param(p::GAIN) - 0.2).abs() < 1e-6);
        assert!((lim.params.plain(p::STYLE) - style::ALLROUND).abs() < 1e-6);
        assert_eq!(lim.get_param(p::DELTA), lim.params.get(p::DELTA));
        assert!((lim.params.plain(p::ATTACK) - 1.0).abs() < 1e-6);
    }
}
