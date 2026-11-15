//! `fx.crusher` — bit-depth and sample-rate reducer.
//!
//! Signal path per channel: optional pre low-pass (Butterworth, 0.45 × Rate)
//! → sample-and-hold on a jittered clock → quantiser (mid-tread with a
//! μ-law style Shape curve and a movable dead zone) → optional post low-pass
//! (Rate / 2 × 2^octave) → DC blocker unless Keep DC. Zero latency.
//!
//! Bits are rounded to 0.1; 16 means no quantisation. Below 2 bits the
//! quantiser is mid-rise so that 1 bit leaves exactly two output levels
//! (exact digital silence still maps to zero). Rate at or above the host
//! rate with Jitter 0 holds nothing: the stage is a wire.
//!
//! Meter layout: `[held value left, held value right]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::osc::Noise;
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use std::f32::consts::FRAC_1_SQRT_2;

/// DC blocker corner, Hz.
const DC_BLOCK_HZ: f32 = 10.0;
/// Cross-fade between voices on a structural change, ms.
const FADE_MS: f64 = 5.0;
/// μ at Shape = ±100 %.
const MU_MAX: f32 = 255.0;

pub const ID: &str = "fx.crusher";

pub mod p {
    pub const RATE: u32 = 0;
    pub const JITTER: u32 = 1;
    pub const PRE: u32 = 2;
    pub const POST: u32 = 3;
    pub const POST_OCT: u32 = 4;
    pub const BITS: u32 = 5;
    pub const SHAPE: u32 = 6;
    pub const DC_SHIFT: u32 = 7;
    pub const KEEP_DC: u32 = 8;
    pub const STEREO_JITTER: u32 = 9;
    pub const MIX: u32 = 10;
    pub const OUTPUT: u32 = 11;
}

const OFF_ON: &[&str] = &["Off", "On"];
const LINKS: &[&str] = &["Linked", "Independent"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::RATE, "Rate", 200.0, 48_000.0, 16_000.0).unit("Hz").log(),
        ParamInfo::new(p::JITTER, "Jitter", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::PRE, "Pre Filter", 0.0, 1.0, 0.0).choices(OFF_ON).short("Pre"),
        ParamInfo::new(p::POST, "Post Filter", 0.0, 1.0, 0.0).choices(OFF_ON).short("Post"),
        ParamInfo::new(p::POST_OCT, "Post Octave", -2.0, 1.0, 0.0).unit("oct").short("Post Oct"),
        ParamInfo::new(p::BITS, "Bits", 1.0, 16.0, 8.0),
        ParamInfo::new(p::SHAPE, "Shape", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::DC_SHIFT, "DC Shift", -100.0, 100.0, 0.0).unit("%").short("DC"),
        ParamInfo::new(p::KEEP_DC, "Keep DC", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::STEREO_JITTER, "Stereo Jitter", 0.0, 1.0, 1.0).choices(LINKS).short("St Jitter"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
    ]
}

/// Quantiser settings derived from the parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Quantizer {
    active: bool,
    mid_rise: bool,
    /// Levels per unit (`2^(bits − 1)`).
    levels: f32,
    /// Shape: μ and direction (+1 compress before, −1 expand before).
    mu: f32,
    expand: bool,
    shift: f32,
}

impl Quantizer {
    fn new(bits: f32, shape: f32, dc: f32) -> Self {
        let bits = (bits * 10.0).round() / 10.0;
        let levels = 2f32.powf(bits.clamp(1.0, 16.0) - 1.0);
        Self {
            active: bits < 15.95,
            mid_rise: bits < 2.0,
            levels,
            mu: MU_MAX * shape.abs(),
            expand: shape < 0.0,
            shift: dc * 0.5 / levels,
        }
    }

    /// μ-law compression of `x` (odd, maps ±1 to ±1).
    #[inline]
    fn compress(x: f32, mu: f32) -> f32 {
        x.signum() * (1.0 + mu * x.abs()).ln() / (1.0 + mu).ln()
    }

    /// Inverse of [`compress`](Self::compress).
    #[inline]
    fn expand(y: f32, mu: f32) -> f32 {
        y.signum() * ((1.0 + mu).powf(y.abs()) - 1.0) / mu
    }

    #[inline]
    fn apply(&self, x: f32) -> f32 {
        if !self.active {
            return x;
        }
        let shaped = if self.mu < 0.01 {
            x
        } else if self.expand {
            Self::expand(x.clamp(-2.0, 2.0), self.mu)
        } else {
            Self::compress(x, self.mu)
        };
        let v = shaped + self.shift;
        let q = if self.mid_rise {
            if v == 0.0 {
                return 0.0;
            }
            ((v * self.levels).floor() + 0.5) / (self.levels - 0.5)
        } else {
            (v * self.levels).round() / self.levels
        };
        let q = if self.mid_rise { q.clamp(-1.0, 1.0) } else { q };
        if self.mu < 0.01 {
            q
        } else if self.expand {
            Self::compress(q, self.mu)
        } else {
            Self::expand(q, self.mu)
        }
    }
}

/// The quantiser's transfer curve for an editor: `x` in, the level out, for
/// Bits, Shape (%) and DC Shift (%) as the parameters give them.
pub fn transfer(x: f32, bits: f32, shape_pct: f32, dc_pct: f32) -> f32 {
    Quantizer::new(bits, shape_pct / 100.0, dc_pct / 100.0).apply(x)
}

/// Sampling clock with a jittered period.
#[derive(Clone, Copy, Debug)]
struct Clock {
    counter: f32,
    period: f32,
    noise: Noise,
}

impl Clock {
    fn new(seed: u32) -> Self {
        Self { counter: 0.0, period: 1.0, noise: Noise::new(seed) }
    }

    fn restart(&mut self, base: f32) {
        self.period = base;
        self.counter = base - 1.0;
    }

    /// Advance one sample; true when a new sample is taken.
    #[inline]
    fn step(&mut self, base: f32, jitter: f32) -> bool {
        self.counter += 1.0;
        if self.counter < self.period {
            return false;
        }
        self.counter -= self.period;
        let u = if jitter > 0.0 { self.noise.next() } else { 0.0 };
        self.period = (base * (1.0 + jitter * u)).max(1.0);
        true
    }
}

/// Everything `update` derives from the parameters that the sample loop
/// reads. Structural differences cross-fade between two voices.
#[derive(Clone, Copy, Debug)]
struct Settings {
    quant: Quantizer,
    pre_c: SvfCoeffs,
    post_c: SvfCoeffs,
    pre_on: bool,
    post_on: bool,
    base_period: f32,
    jitter: f32,
    linked: bool,
    dc_block: bool,
}

impl Settings {
    fn neutral() -> Self {
        Self {
            quant: Quantizer::new(16.0, 0.0, 0.0),
            pre_c: SvfCoeffs::passthrough(),
            post_c: SvfCoeffs::passthrough(),
            pre_on: false,
            post_on: false,
            base_period: 1.0,
            jitter: 0.0,
            linked: false,
            dc_block: false,
        }
    }

    /// Differences that would click if applied abruptly: filters switching,
    /// the DC blocker, the quantiser mode and large jumps of the rate.
    fn clicks_against(&self, old: &Settings) -> bool {
        let ratio = self.base_period / old.base_period;
        self.pre_on != old.pre_on
            || self.post_on != old.post_on
            || self.dc_block != old.dc_block
            || self.quant.active != old.quant.active
            || self.quant.mid_rise != old.quant.mid_rise
            || !(0.67..=1.5).contains(&ratio)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Channel {
    pre: SvfState,
    post: SvfState,
    hold: f32,
    dc_x: f32,
    dc_y: f32,
}

/// One complete signal path: settings plus the state they run on. Two of
/// them run side by side while a structural change cross-fades.
#[derive(Clone, Copy, Debug)]
struct Voice {
    set: Settings,
    ch: [Channel; 2],
    clock: [Clock; 2],
}

impl Voice {
    fn new() -> Self {
        Self { set: Settings::neutral(), ch: [Channel::default(); 2], clock: [Clock::new(0x1357_9BDF), Clock::new(0x2468_ACE1)] }
    }

    fn clear(&mut self) {
        self.ch = [Channel::default(); 2];
        let base = self.set.base_period;
        for c in &mut self.clock {
            c.restart(base);
        }
    }

    /// Process one stereo frame.
    #[inline]
    fn run(&mut self, input: [f32; 2], dc_r: f32) -> [f32; 2] {
        let cur = self.set;
        let mut out = [0.0f32; 2];
        let tick0 = self.clock[0].step(cur.base_period, cur.jitter);
        let tick1 = if cur.linked { tick0 } else { self.clock[1].step(cur.base_period, cur.jitter) };
        for c in 0..2 {
            let ch = &mut self.ch[c];
            let x = if cur.pre_on { ch.pre.process(&cur.pre_c, input[c]) } else { input[c] };
            if if c == 0 { tick0 } else { tick1 } {
                ch.hold = cur.quant.apply(x);
            }
            let mut y = if cur.post_on { ch.post.process(&cur.post_c, ch.hold) } else { ch.hold };
            if cur.dc_block {
                let z = y - ch.dc_x + dc_r * ch.dc_y;
                ch.dc_x = y;
                ch.dc_y = if z.abs() < 1e-20 { 0.0 } else { z };
                y = ch.dc_y;
            }
            out[c] = y;
        }
        out
    }
}

pub struct Crusher {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    /// Voice in force (the target of a running cross-fade).
    a: Voice,
    /// Voice being faded out.
    b: Voice,
    /// Cross-fade position and length in samples; `pos == len` when idle.
    fade_pos: u32,
    fade_len: u32,
    /// Newest settings that arrived during a cross-fade.
    queued: Option<Settings>,
    dc_r: f32,
    wet: Smoother,
    dry: Smoother,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Crusher::new())
}

impl Default for Crusher {
    fn default() -> Self {
        Self::new()
    }
}

impl Crusher {
    pub fn new() -> Self {
        let params = params();
        let mut c = Self {
            info: PluginInfo {
                id: ID,
                name: "Crusher",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["distortion", "bitcrusher", "lo-fi", "downsample", "crusher"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 0.0,
            a: Voice::new(),
            b: Voice::new(),
            fade_pos: 0,
            fade_len: 1,
            queued: None,
            dc_r: 0.999,
            wet: Smoother::new(1.0),
            dry: Smoother::new(0.0),
            first: true,
        };
        c.prepare(48_000.0, 512);
        c
    }

    fn update(&mut self) {
        let sr = self.sr as f32;
        let rate = self.params.plain(p::RATE).min(sr);
        let nyquist_ok = 0.45 * sr;
        let pre_hz = 0.45 * rate;
        let post_hz = 0.5 * rate * 2f32.powf(self.params.plain(p::POST_OCT));
        let dc = self.params.plain(p::DC_SHIFT) / 100.0;
        let new = Settings {
            quant: Quantizer::new(self.params.plain(p::BITS), self.params.plain(p::SHAPE) / 100.0, dc),
            pre_c: SvfCoeffs::new(SvfMode::Lowpass, pre_hz, FRAC_1_SQRT_2, 0.0, self.sr),
            post_c: SvfCoeffs::new(SvfMode::Lowpass, post_hz, FRAC_1_SQRT_2, 0.0, self.sr),
            pre_on: self.params.on(p::PRE) && pre_hz < nyquist_ok * 0.999,
            post_on: self.params.on(p::POST) && post_hz < nyquist_ok,
            base_period: (sr / rate).max(1.0),
            jitter: self.params.plain(p::JITTER) / 100.0,
            linked: self.params.choice(p::STEREO_JITTER) == 0,
            dc_block: dc != 0.0 && !self.params.on(p::KEEP_DC),
        };
        if self.first {
            self.a.set = new;
            self.b.set = new;
            self.fade_pos = self.fade_len;
            self.queued = None;
        } else if self.fading() {
            self.queued = Some(new);
        } else {
            self.change(new);
        }
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry.set_target((1.0 - mix) * out);
        if self.first {
            self.wet.snap_to_target();
            self.dry.snap_to_target();
            self.first = false;
        }
    }

    #[inline]
    fn fading(&self) -> bool {
        self.fade_pos < self.fade_len
    }

    /// Take new settings: straight away when that cannot click, otherwise
    /// by cross-fading from a copy of the current voice.
    fn change(&mut self, new: Settings) {
        if !new.clicks_against(&self.a.set) {
            self.a.set = new;
            return;
        }
        self.b = self.a;
        let old = self.b.set;
        for ch in &mut self.a.ch {
            if new.pre_on && !old.pre_on {
                ch.pre = SvfState::default();
            }
            if new.post_on && !old.post_on {
                ch.post = SvfState::default();
            }
            if new.dc_block && !old.dc_block {
                ch.dc_x = 0.0;
                ch.dc_y = 0.0;
            }
        }
        self.a.set = new;
        self.fade_pos = 0;
    }

    /// Advance the cross-fade by one sample; returns the weight of the new
    /// voice (1 when idle). A change queued during a fade starts afterwards.
    #[inline]
    fn fade_weight(&mut self) -> f32 {
        if !self.fading() {
            if let Some(q) = self.queued.take() {
                self.change(q);
            }
            if !self.fading() {
                return 1.0;
            }
        }
        self.fade_pos += 1;
        self.fade_pos as f32 / self.fade_len as f32
    }
}

impl Segmented for Crusher {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let dc_r = self.dc_r;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let t = self.fade_weight();
            let (w, d) = (self.wet.next(), self.dry.next());
            let input = [l, r];
            let new = self.a.run(input, dc_r);
            let out = if t < 1.0 {
                let old = self.b.run(input, dc_r);
                [old[0] * (1.0 - t) + new[0] * t, old[1] * (1.0 - t) + new[1] * t]
            } else {
                new
            };
            (sanitize(l * d + out[0] * w), sanitize(r * d + out[1] * w))
        });
    }
}

impl Processor for Crusher {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.dc_r = 1.0 - std::f32::consts::TAU * DC_BLOCK_HZ / sample_rate as f32;
        for s in [&mut self.wet, &mut self.dry] {
            s.set_time(20.0, sample_rate);
        }
        self.fade_len = ((FADE_MS * 0.001 * sample_rate) as u32).max(1);
        self.fade_pos = self.fade_len;
        self.first = true;
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        if let Some(q) = self.queued.take() {
            self.a.set = q;
        }
        self.a.clear();
        self.b = self.a;
        self.fade_pos = self.fade_len;
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
        write_meter(out, 0, &[self.a.ch[0].hold, self.a.ch[1].hold])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{get_plain, process_stereo, rms, set_plain, sine};

    fn noise(seed: u32, amp: f32) -> impl FnMut(usize) -> (f32, f32) {
        let mut a = aitrack_dsp::osc::Noise::new(seed);
        let mut b = aitrack_dsp::osc::Noise::new(seed + 1);
        move |_| (amp * a.next(), amp * b.next())
    }

    /// Plain configuration: no filters, no jitter, full mix.
    fn clean(c: &mut Crusher) {
        set_plain(c, p::PRE, 0.0);
        set_plain(c, p::POST, 0.0);
        set_plain(c, p::JITTER, 0.0);
        set_plain(c, p::MIX, 100.0);
        set_plain(c, p::OUTPUT, 0.0);
    }

    fn goertzel(x: &[f32], hz: f64) -> f64 {
        let w = std::f64::consts::TAU * hz / 48_000.0;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, v) in x.iter().enumerate() {
            re += *v as f64 * (w * i as f64).cos();
            im += *v as f64 * (w * i as f64).sin();
        }
        (re * re + im * im).sqrt() / x.len() as f64
    }

    fn distinct(x: &[f32]) -> Vec<f32> {
        let mut v: Vec<f32> = x.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        v
    }

    #[test]
    fn params_are_stable() {
        let c = Crusher::new();
        assert_eq!(c.info().params.len(), 12);
        assert_eq!(c.latency(), 0);
        assert_eq!(get_plain(&c, p::BITS), 8.0);
    }

    /// Done criterion 2.2a: 16 bits at the maximum Rate is a bit-exact wire.
    #[test]
    fn sixteen_bits_at_max_rate_is_transparent() {
        let mut c = Crusher::new();
        clean(&mut c);
        set_plain(&mut c, p::BITS, 16.0);
        set_plain(&mut c, p::RATE, 48_000.0);
        let (l, r) = process_stereo(&mut c, noise(3, 0.7), 20_000);
        let mut inp = noise(3, 0.7);
        for i in 0..20_000 {
            let (x, y) = inp(i);
            assert!((l[i] - x).abs() <= 1e-6 && (r[i] - y).abs() <= 1e-6, "@{i}: {} vs {x}", l[i]);
        }
        // Also with the filters switched on (they bypass at full rate).
        let mut c = Crusher::new();
        set_plain(&mut c, p::BITS, 16.0);
        set_plain(&mut c, p::RATE, 48_000.0);
        set_plain(&mut c, p::PRE, 1.0);
        set_plain(&mut c, p::POST, 1.0);
        set_plain(&mut c, p::MIX, 100.0);
        let (l, _) = process_stereo(&mut c, sine(1234.0, 0.8), 5000);
        let mut inp = sine(1234.0, 0.8);
        for i in 0..5000 {
            assert!((l[i] - inp(i).0).abs() <= 1e-6, "@{i}");
        }
    }

    /// Done criterion 2.2b: one bit leaves exactly two output levels.
    #[test]
    fn one_bit_has_two_levels() {
        let mut c = Crusher::new();
        clean(&mut c);
        set_plain(&mut c, p::BITS, 1.0);
        set_plain(&mut c, p::RATE, 48_000.0);
        let phase = 0.3;
        let (l, r) = process_stereo(&mut c, |i| {
            let v = 0.9 * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0 + phase).sin();
            (v, -v)
        }, 20_000);
        for out in [&l, &r] {
            let d = distinct(out);
            assert_eq!(d.len(), 2, "{d:?}");
            assert!((d[0] + d[1]).abs() < 1e-6, "levels not symmetric: {d:?}");
        }
    }

    #[test]
    fn quantizer_steps_and_error_bound() {
        for bits in [2.0f32, 4.0, 8.0, 12.0] {
            let mut c = Crusher::new();
            clean(&mut c);
            set_plain(&mut c, p::BITS, bits);
            set_plain(&mut c, p::RATE, 48_000.0);
            let (l, _) = process_stereo(&mut c, noise(8, 0.95), 20_000);
            let step = 1.0 / 2f32.powf(bits - 1.0);
            let mut inp = noise(8, 0.95);
            for i in 0..20_000 {
                assert!((l[i] - inp(i).0).abs() <= 0.5 * step + 1e-5, "bits {bits} @{i}");
            }
            let n = distinct(&l).len() as f32;
            assert!(n <= 2.0f32.powf(bits) + 1.0, "bits {bits}: {n} levels");
        }
        // Fractional depths sit between neighbouring integers.
        let err = |bits: f32| {
            let mut c = Crusher::new();
            clean(&mut c);
            set_plain(&mut c, p::BITS, bits);
            set_plain(&mut c, p::RATE, 48_000.0);
            let (l, _) = process_stereo(&mut c, noise(8, 0.95), 20_000);
            let mut inp = noise(8, 0.95);
            let d: Vec<f32> = (0..20_000).map(|i| l[i] - inp(i).0).collect();
            rms(&d)
        };
        assert!(err(6.0) < err(5.5) && err(5.5) < err(5.0), "{} {} {}", err(6.0), err(5.5), err(5.0));
    }

    #[test]
    fn downsampling_holds_samples() {
        let mut c = Crusher::new();
        clean(&mut c);
        set_plain(&mut c, p::BITS, 16.0);
        set_plain(&mut c, p::RATE, 12_000.0); // every 4th sample
        let (l, _) = process_stereo(&mut c, sine(700.0, 0.8), 4000);
        for g in l[8..].as_chunks::<4>().0 {
            assert!(g.iter().all(|v| *v == g[0]), "{g:?}");
        }
        assert!(distinct(&l[100..]).len() > 20);
    }

    #[test]
    fn zero_jitter_is_deterministic_and_jitter_is_not_periodic() {
        let run = |jitter: f32, link: f32| {
            let mut c = Crusher::new();
            clean(&mut c);
            set_plain(&mut c, p::BITS, 16.0);
            set_plain(&mut c, p::RATE, 8000.0);
            set_plain(&mut c, p::JITTER, jitter);
            set_plain(&mut c, p::STEREO_JITTER, link);
            process_stereo(&mut c, noise(4, 0.6), 12_000)
        };
        assert_eq!(run(0.0, 1.0).0, run(0.0, 1.0).0);
        let run_len = |x: &[f32]| {
            // lengths of runs of equal samples
            let mut lens = Vec::new();
            let mut n = 1;
            for w in x.windows(2) {
                if w[0] == w[1] {
                    n += 1;
                } else {
                    lens.push(n);
                    n = 1;
                }
            }
            lens
        };
        let steady = run_len(&run(0.0, 1.0).0[100..]);
        assert!(steady[1..].iter().all(|&n| n == 6), "{:?}", &steady[..10]);
        let jittered = run_len(&run(100.0, 1.0).0[100..]);
        let (lo, hi) = (jittered.iter().min().unwrap(), jittered.iter().max().unwrap());
        assert!(hi - lo >= 4, "{lo} {hi}");
        // Average rate is preserved.
        let mean = jittered.iter().sum::<usize>() as f32 / jittered.len() as f32;
        assert!((mean - 6.0).abs() < 1.0, "{mean}");
        // Independent channels differ, linked ones match.
        let (l, r) = run(60.0, 1.0);
        assert!(l.iter().zip(&r).any(|(a, b)| a != b));
        let linked = |l: &[f32], r: &[f32]| l.iter().zip(r).all(|(a, b)| a == b);
        let mut c = Crusher::new();
        clean(&mut c);
        set_plain(&mut c, p::BITS, 16.0);
        set_plain(&mut c, p::RATE, 8000.0);
        set_plain(&mut c, p::JITTER, 60.0);
        set_plain(&mut c, p::STEREO_JITTER, 0.0);
        let (l, r) = process_stereo(&mut c, |i| { let v = (i as f32 * 0.37).sin(); (v, v) }, 8000);
        assert!(linked(&l, &r));
    }

    /// The pre filter keeps a tone above the new Nyquist from aliasing.
    #[test]
    fn pre_filter_reduces_aliasing() {
        let alias = |pre: f32| {
            let mut c = Crusher::new();
            clean(&mut c);
            set_plain(&mut c, p::BITS, 16.0);
            set_plain(&mut c, p::RATE, 8000.0);
            set_plain(&mut c, p::PRE, pre);
            let (l, _) = process_stereo(&mut c, sine(7003.0, 0.8), 48_000);
            goertzel(&l[4800..], 8000.0 - 7003.0)
        };
        let (off, on) = (alias(0.0), alias(1.0));
        assert!(off > 0.05, "alias should be audible without the filter: {off}");
        assert!(on < off * 0.4, "off {off} on {on}");
    }

    #[test]
    fn post_filter_darkens_and_follows_its_octave() {
        let hf = |post: f32, oct: f32| {
            let mut c = Crusher::new();
            clean(&mut c);
            set_plain(&mut c, p::BITS, 16.0);
            set_plain(&mut c, p::RATE, 8000.0);
            set_plain(&mut c, p::POST, post);
            set_plain(&mut c, p::POST_OCT, oct);
            let (l, _) = process_stereo(&mut c, noise(6, 0.5), 48_000);
            let d: Vec<f32> = l.windows(2).map(|w| w[1] - w[0]).collect();
            rms(&d[2000..])
        };
        let off = hf(0.0, 0.0);
        let on = hf(1.0, 0.0);
        let lower = hf(1.0, -2.0);
        let higher = hf(1.0, 1.0);
        assert!(on < 0.8 * off && lower < 0.8 * on && higher > on, "{off} {on} {lower} {higher}");
    }

    /// μ-law Shape: positive gives small signals finer steps, negative the
    /// loud ones.
    #[test]
    fn shape_moves_the_resolution() {
        // Error rms over the samples whose magnitude exceeds `min`.
        let err = |shape: f32, amp: f32, min: f32| {
            let mut c = Crusher::new();
            clean(&mut c);
            set_plain(&mut c, p::BITS, 5.0);
            set_plain(&mut c, p::RATE, 48_000.0);
            set_plain(&mut c, p::SHAPE, shape);
            let (l, _) = process_stereo(&mut c, noise(2, amp), 20_000);
            let mut inp = noise(2, amp);
            let d: Vec<f32> = (0..20_000).filter_map(|i| {
                let x = inp(i).0;
                (x.abs() > min).then(|| l[i] - x)
            }).collect();
            rms(&d)
        };
        assert!(err(60.0, 0.05, 0.0) < 0.5 * err(0.0, 0.05, 0.0));
        assert!(err(-60.0, 0.9, 0.7) < 0.8 * err(0.0, 0.9, 0.7), "{} {}", err(-60.0, 0.9, 0.7), err(0.0, 0.9, 0.7));
    }

    #[test]
    fn dc_shift_moves_the_dead_zone_without_leaking_dc() {
        // Silence stays silent while the shift is inside the dead zone.
        let mut c = Crusher::new();
        clean(&mut c);
        set_plain(&mut c, p::BITS, 4.0);
        set_plain(&mut c, p::RATE, 48_000.0);
        set_plain(&mut c, p::DC_SHIFT, 60.0);
        let (l, _) = process_stereo(&mut c, |_| (0.0, 0.0), 4000);
        assert!(l.iter().all(|v| v.abs() < 1e-6));
        // A quiet tone is cut differently on each side of zero.
        let mean_tail = |keep: f32, shift: f32| {
            let mut c = Crusher::new();
            clean(&mut c);
            set_plain(&mut c, p::BITS, 3.0);
            set_plain(&mut c, p::RATE, 48_000.0);
            set_plain(&mut c, p::DC_SHIFT, shift);
            set_plain(&mut c, p::KEEP_DC, keep);
            let (l, _) = process_stereo(&mut c, sine(220.0, 0.3), 96_000);
            l[48_000..].iter().sum::<f32>() / 48_000.0
        };
        let kept = mean_tail(1.0, 100.0);
        let blocked = mean_tail(0.0, 100.0);
        assert!(kept.abs() > 0.02, "{kept}");
        assert!(blocked.abs() < 0.2 * kept.abs(), "kept {kept} blocked {blocked}");
        let (a, b) = (mean_tail(1.0, 0.0), mean_tail(1.0, 100.0));
        assert!(a.abs() < 1e-3 && b != a);
    }

    #[test]
    fn mix_zero_is_dry_and_output_scales() {
        let mut c = Crusher::new();
        clean(&mut c);
        set_plain(&mut c, p::MIX, 0.0);
        let (l, _) = process_stereo(&mut c, noise(5, 0.5), 5000);
        let mut inp = noise(5, 0.5);
        for i in 0..5000 {
            assert!((l[i] - inp(i).0).abs() < 1e-6);
        }
        let mut c = Crusher::new();
        clean(&mut c);
        set_plain(&mut c, p::BITS, 16.0);
        set_plain(&mut c, p::RATE, 48_000.0);
        set_plain(&mut c, p::OUTPUT, -6.0);
        let (l, _) = process_stereo(&mut c, sine(300.0, 0.5), 9600);
        let want = 0.5 * 0.5f32.sqrt() * 10f32.powf(-6.0 / 20.0);
        assert!((rms(&l[4800..]) - want).abs() < 0.01);
    }

    #[test]
    fn state_round_trip() {
        let mut a = Crusher::new();
        set_plain(&mut a, p::BITS, 5.0);
        let bytes = a.save_state();
        let mut b = Crusher::new();
        b.load_state(&bytes);
        assert!((get_plain(&b, p::BITS) - 5.0).abs() < 1e-3);
    }

    /// Runs a 0.7-amplitude sine of `freq` Hz through `c` and returns the
    /// largest step between contiguous output samples around a parameter
    /// event delivered mid-block, and the rms of the 10 ms that follow the
    /// event. `value` of `None` runs without the event.
    fn measure(c: &mut dyn Processor, init: &[(u32, f32)], freq: f64, id: u32, value: Option<f32>) -> (f32, f32) {
        use crate::test_util::{BLOCK, SR};
        use aitrack_dsp::{AudioBuffer, Event, Transport};
        c.prepare(SR, BLOCK);
        for &(i, v) in init {
            c.set_param(i, v);
        }
        let tr = Transport { playing: true, ..Transport::default() };
        let mut buf = AudioBuffer::new(2, BLOCK);
        let mut prev = 0.0f32;
        let mut worst = 0.0f32;
        let mut window = Vec::new();
        for b in 0..60 {
            buf.set_frames(BLOCK);
            for i in 0..BLOCK {
                let n = (b * BLOCK + i) as f64;
                let v = 0.7 * (std::f64::consts::TAU * freq * n / SR).sin() as f32;
                buf.channel_mut(0)[i] = v;
                buf.channel_mut(1)[i] = v;
            }
            let ev = [Event { offset: 64, kind: EventKind::Param { id, value: value.unwrap_or(0.0) } }];
            let events: &[Event] = if b == 40 && value.is_some() { &ev } else { &[] };
            let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events, transport: &tr };
            c.process(&mut ctx);
            for i in 0..BLOCK {
                let y = buf.channel(0)[i];
                if b >= 38 {
                    worst = worst.max((y - prev).abs());
                }
                if b >= 40 && window.len() < 480 && (b > 40 || i >= 64) {
                    window.push(y);
                }
                prev = y;
            }
        }
        (worst, crate::test_util::rms(&window))
    }

    #[test]
    fn discrete_changes_do_not_click_or_drop() {
        // (initial normalised params, param, new normalised value)
        type Case = (&'static [(u32, f32)], u32, f32);
        let cases: [Case; 13] = [
            (&[], p::RATE, 0.75),
            (&[(p::RATE, 1.0)], p::RATE, 0.0),
            (&[(p::RATE, 0.5)], p::RATE, 1.0),
            (&[(p::RATE, 0.75)], p::RATE, 0.5),
            (&[(p::BITS, 1.0)], p::BITS, 0.05),
            (&[(p::BITS, 0.5)], p::BITS, 0.0),
            (&[(p::DC_SHIFT, 0.75)], p::KEEP_DC, 1.0),
            (&[], p::DC_SHIFT, 0.75),
            (&[], p::PRE, 0.344),
            (&[], p::PRE, 1.0),
            (&[(p::PRE, 1.0)], p::PRE, 0.0),
            (&[], p::POST, 1.0),
            (&[(p::POST, 1.0)], p::POST, 0.0),
        ];
        for (init, id, value) in cases {
            let mut after = init.to_vec();
            after.push((id, value));
            // Click test, on a low sine: the steady state of either setting
            // is the yardstick.
            let (before_step, _) = measure(&mut Crusher::new(), init, 103.0, id, None);
            let (after_step, _) = measure(&mut Crusher::new(), &after, 103.0, id, None);
            let reference = before_step.max(after_step);
            let (changed, _) = measure(&mut Crusher::new(), init, 103.0, id, Some(value));
            eprintln!("JUMP {id} {value}: ref {reference} changed {changed}");
            assert!(changed <= 2.5 * reference + 0.004, "param {id} -> {value}: {changed} vs reference {reference}");
            // Level test, on a 1 kHz sine (whole cycles in the window): the
            // output never falls into a hole between the two settings.
            let (_, before) = measure(&mut Crusher::new(), init, 1000.0, id, None);
            let (_, after) = measure(&mut Crusher::new(), &after, 1000.0, id, None);
            let (_, during) = measure(&mut Crusher::new(), init, 1000.0, id, Some(value));
            let floor = before.min(after);
            eprintln!("LEVEL {id} {value}: before {before} after {after} during {during}");
            assert!(during >= floor * 0.891, "param {id} -> {value}: level {during} fell below {floor}");
        }
    }
}
