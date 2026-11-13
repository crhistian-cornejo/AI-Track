//! `fx.delay` — stereo / ping-pong / mono delay with three engines:
//!
//! * **Digital**: clean cubic-interpolated lines (the original delay).
//! * **Tape**: the same lines played by a wobbling transport (wow and
//!   flutter), with tape record colour in the loop (pre-emphasis,
//!   asymmetric saturation, de-emphasis, gap loss set by Age) and a head
//!   bump on the output ([`tape`]).
//! * **BBD**: a bucket-brigade chain clocked by the delay time, with its
//!   anti-aliasing and reconstruction filters, after Holters & Parker
//!   (DAFx-18) ([`bbd`]). Long delays alias and darken like the real chips.
//!
//! Shared around them: tempo sync (straight, dotted, triplet), filtered and
//! soft-clipped feedback (HP → LP → ADAA tanh, so even 100 % feedback stays
//! bounded), multi-tap rhythms inside the main time, all-pass diffusion in
//! the loop, ducking, width and freeze (input muted, loop held at unity
//! with its filters bypassed). Time changes glide (tape-style pitch bend)
//! instead of jumping. Zero latency.
//!
//! Meter layout: `[wet peak L, wet peak R, duck gain (0..1), freeze (0..1)]`.

pub mod bbd;
pub mod tape;

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::{AllpassDiffuser, DelayLine};
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower};
use aitrack_dsp::lfo::{SYNC_DIVISIONS, division_seconds};
use aitrack_dsp::shaper::{Adaa1, ShaperKind};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::{OnePole, SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use bbd::Bbd;
use tape::{FLUTTER_MS, TapeColor, TapeCoeffs, Transport, WOW_MS};

pub const ID: &str = "fx.delay";

pub mod p {
    pub const MODE: u32 = 0;
    pub const SYNC: u32 = 1;
    pub const TIME_L: u32 = 2;
    pub const TIME_R: u32 = 3;
    pub const DIV_L: u32 = 4;
    pub const DIV_R: u32 = 5;
    pub const FEEDBACK: u32 = 6;
    pub const LOW_CUT: u32 = 7;
    pub const HIGH_CUT: u32 = 8;
    pub const MOD_RATE: u32 = 9;
    pub const MOD_DEPTH: u32 = 10;
    pub const DUCK: u32 = 11;
    pub const WIDTH: u32 = 12;
    pub const MIX: u32 = 13;
    pub const OUTPUT: u32 = 14;
    pub const DRIVE: u32 = 15;
    pub const ENGINE: u32 = 16;
    pub const WOW: u32 = 17;
    pub const FLUTTER: u32 = 18;
    pub const AGE: u32 = 19;
    pub const TAPS: u32 = 20;
    pub const DIFFUSION: u32 = 21;
    pub const FREEZE: u32 = 22;
}

const MODES: &[&str] = &["Stereo", "Ping-Pong", "Mono"];
pub const ENGINES: &[&str] = &["Digital", "Tape", "BBD"];
const TAP_COUNTS: &[&str] = &["1", "2", "3", "4"];
const OFF_ON: &[&str] = &["Off", "On"];
/// Longest time a tap can reach (sync divisions are clamped to it).
pub const MAX_TIME_MS: f32 = 4000.0;
/// Peak modulation excursion at 100 % depth.
const MAX_MOD_MS: f32 = 6.0;
const DIFFUSERS: usize = 4;
const DIFF_MS: [[f32; DIFFUSERS]; 2] = [[2.3, 3.9, 6.1, 8.7], [2.7, 4.3, 6.7, 9.3]];

pub fn params() -> Vec<ParamInfo> {
    let n_div = (SYNC_DIVISIONS.len() - 1) as f32;
    vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 2.0, 0.0).choices(MODES),
        ParamInfo::new(p::SYNC, "Sync", 0.0, 1.0, 1.0).choices(OFF_ON),
        ParamInfo::new(p::TIME_L, "Time Left", 1.0, MAX_TIME_MS, 375.0).unit("ms").log().short("Time L"),
        ParamInfo::new(p::TIME_R, "Time Right", 1.0, MAX_TIME_MS, 500.0).unit("ms").log().short("Time R"),
        ParamInfo::new(p::DIV_L, "Division Left", 0.0, n_div, 11.0).choices(SYNC_DIVISIONS).short("Div L"),
        ParamInfo::new(p::DIV_R, "Division Right", 0.0, n_div, 7.0).choices(SYNC_DIVISIONS).short("Div R"),
        ParamInfo::new(p::FEEDBACK, "Feedback", 0.0, 100.0, 35.0).unit("%").short("FB"),
        ParamInfo::new(p::LOW_CUT, "Low Cut", 20.0, 5000.0, 100.0).unit("Hz").log().short("LoCut"),
        ParamInfo::new(p::HIGH_CUT, "High Cut", 500.0, 20_000.0, 8000.0).unit("Hz").log().short("HiCut"),
        ParamInfo::new(p::MOD_RATE, "Mod Rate", 0.05, 10.0, 0.5).unit("Hz").log().short("Rate"),
        ParamInfo::new(p::MOD_DEPTH, "Mod Depth", 0.0, 100.0, 0.0).unit("%").short("Depth"),
        ParamInfo::new(p::DUCK, "Ducking", 0.0, 100.0, 0.0).unit("%").short("Duck"),
        ParamInfo::new(p::WIDTH, "Width", 0.0, 200.0, 100.0).unit("%"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 30.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::DRIVE, "Loop Drive", 0.0, 24.0, 0.0).unit("dB").short("Drive"),
        ParamInfo::new(p::ENGINE, "Engine", 0.0, (ENGINES.len() - 1) as f32, 0.0).choices(ENGINES),
        ParamInfo::new(p::WOW, "Wow", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::FLUTTER, "Flutter", 0.0, 100.0, 0.0).unit("%").short("Flut"),
        ParamInfo::new(p::AGE, "Age", 0.0, 100.0, 25.0).unit("%"),
        ParamInfo::new(p::TAPS, "Taps", 0.0, (TAP_COUNTS.len() - 1) as f32, 0.0).choices(TAP_COUNTS),
        ParamInfo::new(p::DIFFUSION, "Diffusion", 0.0, 100.0, 0.0).unit("%").short("Diff"),
        ParamInfo::new(p::FREEZE, "Freeze", 0.0, 1.0, 0.0).choices(OFF_ON),
    ]
}

/// BBD filter cutoff for an Age of `age` (0..1): 9 kHz fresh, 2.5 kHz tired.
pub fn bbd_cutoff(age: f32) -> f32 {
    9_000.0 * (2_500.0f32 / 9_000.0).powf(age.clamp(0.0, 1.0))
}

/// Times (fractions of the main time) and gains of the multi-taps for a
/// tap count `n`: the main tap is last at 1.0.
pub fn tap_pattern(n: usize) -> impl Iterator<Item = (f32, f32)> {
    let n = n.clamp(1, 4);
    (1..n).map(move |k| {
        let at = k as f32 / n as f32;
        (at, 0.35 + 0.5 * at)
    })
}

/// Per-channel feedback-loop processing, plus the tape record head.
#[derive(Clone, Copy)]
struct Loop {
    hp: SvfState,
    lp: SvfState,
    clip: Adaa1,
    tape: TapeColor,
}

impl Loop {
    fn new() -> Self {
        Self { hp: SvfState::default(), lp: SvfState::default(), clip: Adaa1::new(ShaperKind::Tanh), tape: TapeColor::new() }
    }

    /// Feedback filters, then the loop clipper (tape saturates on the
    /// record head instead, see [`Loop::record`]).
    #[inline]
    fn process(&mut self, x: f32, hp: &SvfCoeffs, lp: &SvfCoeffs, drive: f32, clip: bool) -> f32 {
        let y = self.lp.process(lp, self.hp.process(hp, x));
        if clip { self.clip.process(y * drive) / drive } else { y }
    }

    /// What the tape record head writes: input and feedback together, so
    /// the first echo already carries the colour.
    #[inline]
    fn record(&mut self, x: f32, c: &TapeCoeffs, drive: f32) -> f32 {
        self.tape.process(x, c, drive)
    }

    fn reset(&mut self) {
        self.hp.reset();
        self.lp.reset();
        self.clip.reset();
        self.tape.reset();
    }
}

pub struct Delay {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    lines: [DelayLine; 2],
    bbd: [Bbd; 2],
    loops: [Loop; 2],
    diffusers: [[AllpassDiffuser; DIFFUSERS]; 2],
    transport: [Transport; 2],
    hp: SvfCoeffs,
    lp: SvfCoeffs,
    tape: TapeCoeffs,
    bump: [SvfState; 2],
    tap_lp: [OnePole; 2],
    time: [Smoother; 2],
    feedback: Smoother,
    wet: Smoother,
    dry: Smoother,
    width: Smoother,
    freeze: Smoother,
    duck_env: EnvelopeFollower,
    mod_phase: f64,
    mod_inc: f64,
    mod_depth: f32,
    wow_depth: f32,
    flutter_depth: f32,
    drive: f32,
    duck_amount: f32,
    diffusion: f32,
    engine: usize,
    taps: usize,
    peak: [f32; 2],
    duck_gain: f32,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Delay::new())
}

impl Default for Delay {
    fn default() -> Self {
        Self::new()
    }
}

fn diffusers(sr: f64) -> [[AllpassDiffuser; DIFFUSERS]; 2] {
    DIFF_MS.map(|side| side.map(|ms| AllpassDiffuser::new((ms * 0.001 * sr as f32) as usize + 1)))
}

impl Delay {
    pub fn new() -> Self {
        let params = params();
        let mut d = Self {
            info: PluginInfo {
                id: ID,
                name: "Delay",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["delay", "echo", "ping-pong", "tape", "bbd"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 0.0,
            lines: [DelayLine::new(16), DelayLine::new(16)],
            bbd: [Bbd::new(48_000.0), Bbd::new(48_000.0)],
            loops: [Loop::new(), Loop::new()],
            diffusers: diffusers(48_000.0),
            transport: [Transport::new(0x9E37, 0.0), Transport::new(0x7F4A, 0.31)],
            hp: SvfCoeffs::default(),
            lp: SvfCoeffs::default(),
            tape: TapeCoeffs::default(),
            bump: [SvfState::default(); 2],
            tap_lp: [OnePole::default(); 2],
            time: [Smoother::new(1000.0); 2],
            feedback: Smoother::new(0.0),
            wet: Smoother::new(0.0),
            dry: Smoother::new(1.0),
            width: Smoother::new(1.0),
            freeze: Smoother::new(0.0),
            duck_env: EnvelopeFollower::new(DetectorMode::Peak),
            mod_phase: 0.0,
            mod_inc: 0.0,
            mod_depth: 0.0,
            wow_depth: 0.0,
            flutter_depth: 0.0,
            drive: 1.0,
            duck_amount: 0.0,
            diffusion: 0.0,
            engine: 0,
            taps: 1,
            peak: [0.0; 2],
            duck_gain: 1.0,
            first: true,
        };
        d.prepare(48_000.0, 512);
        d
    }

    /// Delay time of one side in ms for the current params and tempo.
    pub fn time_ms(&self, right: bool, bpm: f64) -> f32 {
        let ms = if self.params.on(p::SYNC) {
            let div = self.params.choice(if right { p::DIV_R } else { p::DIV_L });
            (division_seconds(div, bpm) * 1000.0) as f32
        } else {
            self.params.plain(if right { p::TIME_R } else { p::TIME_L })
        };
        ms.clamp(1.0, MAX_TIME_MS)
    }

    fn update(&mut self) {
        let sr = self.sr;
        let engine = self.params.choice(p::ENGINE).min(ENGINES.len() - 1);
        if engine != self.engine {
            // The BBD holds its own buckets: start it clean.
            for b in &mut self.bbd {
                b.clear();
            }
            self.engine = engine;
        }
        self.hp = SvfCoeffs::new(SvfMode::Highpass, self.params.plain(p::LOW_CUT), 0.707, 0.0, sr);
        self.lp = SvfCoeffs::new(SvfMode::Lowpass, self.params.plain(p::HIGH_CUT), 0.707, 0.0, sr);
        let age = self.params.plain(p::AGE) / 100.0;
        self.tape = TapeCoeffs::new(age, sr);
        let cutoff = bbd_cutoff(age);
        for b in &mut self.bbd {
            b.set_cutoff(cutoff);
        }
        for t in &mut self.tap_lp {
            t.set_freq(cutoff, sr);
        }
        self.feedback.set_target(self.params.plain(p::FEEDBACK) / 100.0);
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target((2.0 * mix).min(1.0) * out);
        self.dry.set_target((2.0 * (1.0 - mix)).min(1.0) * out);
        self.width.set_target(self.params.plain(p::WIDTH) / 100.0);
        self.freeze.set_target(if self.params.on(p::FREEZE) { 1.0 } else { 0.0 });
        self.mod_inc = self.params.plain(p::MOD_RATE) as f64 / sr;
        let ms = |m: f32| m * 0.001 * sr as f32;
        self.mod_depth = ms(self.params.plain(p::MOD_DEPTH) / 100.0 * MAX_MOD_MS);
        self.wow_depth = ms(self.params.plain(p::WOW) / 100.0 * WOW_MS);
        self.flutter_depth = ms(self.params.plain(p::FLUTTER) / 100.0 * FLUTTER_MS);
        // Tape always wobbles a little; the knobs add to it.
        if engine == 1 {
            self.wow_depth += ms(0.08 * WOW_MS * age);
            self.flutter_depth += ms(0.15 * FLUTTER_MS * (0.3 + age));
        }
        self.drive = db_to_gain(self.params.plain(p::DRIVE) + if engine == 1 { 3.0 } else { 0.0 });
        self.duck_amount = self.params.plain(p::DUCK) / 100.0;
        self.diffusion = 0.7 * self.params.plain(p::DIFFUSION) / 100.0;
        self.taps = self.params.choice(p::TAPS) + 1;
        if self.first {
            for s in [&mut self.feedback, &mut self.wet, &mut self.dry, &mut self.width, &mut self.freeze] {
                s.snap_to_target();
            }
        }
    }
}

impl Segmented for Delay {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let bpm = ctx.transport.bpm;
        let mode = self.params.choice(p::MODE);
        let samples = |ms: f32| ms * 0.001 * self.sr as f32;
        let tl = samples(self.time_ms(false, bpm));
        let tr = samples(if mode == 1 { self.time_ms(false, bpm) } else { self.time_ms(true, bpm) });
        self.time[0].set_target(tl);
        self.time[1].set_target(tr);
        if self.first {
            self.time[0].snap_to_target();
            self.time[1].snap_to_target();
            self.first = false;
        }
        let (hp, lp, tape_c) = (self.hp, self.lp, self.tape);
        let engine = self.engine;
        let is_tape = engine == 1;
        let drive = self.drive;
        let (depth, wow_d, fl_d) = (self.mod_depth, self.wow_depth, self.flutter_depth);
        let wobble = wow_d > 0.0 || fl_d > 0.0;
        let duck_amount = self.duck_amount;
        let g_diff = self.diffusion;
        let taps = self.taps;
        let max_d = self.lines[0].max_delay() as f32 - 2.0;
        let decay = (-1.0 / (0.3 * self.sr as f32)).exp();
        let mut peak = self.peak;
        let mut duck_gain = self.duck_gain;
        let Self { lines, bbd, loops, diffusers, transport, bump, tap_lp, time, feedback, wet, dry, width, freeze, duck_env, mod_phase, mod_inc, .. } = self;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            // Modulation: quadrature sines so the sides move apart.
            *mod_phase += *mod_inc;
            if *mod_phase >= 1.0 {
                *mod_phase -= 1.0;
            }
            let ph = (*mod_phase * std::f64::consts::TAU) as f32;
            let (ms, mc) = ph.sin_cos();
            let (mut wl_mod, mut wr_mod) = (0.0, 0.0);
            if wobble {
                let (w0, f0) = transport[0].next();
                let (w1, f1) = transport[1].next();
                // Both heads share the capstan's wow; flutter differs a bit.
                wl_mod = wow_d * (0.5 + 0.5 * w0) + fl_d * (0.5 + 0.5 * f0);
                wr_mod = wow_d * (0.5 + 0.5 * (0.8 * w0 + 0.2 * w1)) + fl_d * (0.5 + 0.5 * f1);
            }
            let t_l = time[0].next();
            let t_r = time[1].next();
            let dl = (t_l + depth * (0.5 + 0.5 * ms) + wl_mod).clamp(1.0, max_d);
            let dr = (t_r + depth * (0.5 + 0.5 * mc) + wr_mod).clamp(1.0, max_d);
            let (tap_l, tap_r) = if engine == 2 {
                (bbd[0].read(dl), bbd[1].read(dr))
            } else {
                (lines[0].read_cubic(dl - 1.0), lines[1].read_cubic(dr - 1.0))
            };
            let fz = freeze.next();
            let fb = feedback.next();
            let fb = fb + (1.0 - fb) * fz;
            let gin = 1.0 - fz;
            let mono = 0.5 * (l + r);
            let mut back = |c: usize, x: f32| {
                let mut y = loops[c].process(x, &hp, &lp, drive, !is_tape);
                // Frozen, the loop holds what it has: no filters, no clip.
                y += (x - y) * fz;
                if g_diff > 0.0 {
                    for d in diffusers[c].iter_mut() {
                        y = d.process(y, g_diff);
                    }
                }
                fb * y
            };
            let (in_l, in_r) = match mode {
                // Ping-pong: input enters the left line, the lines feed
                // each other.
                1 => (mono * gin + back(0, tap_r), back(1, tap_l)),
                2 => (mono * gin + back(0, tap_l), 0.0),
                _ => (l * gin + back(0, tap_l), r * gin + back(1, tap_r)),
            };
            let (in_l, in_r) = if is_tape {
                // Frozen, the head stops colouring what goes round.
                let (a, b) = (loops[0].record(in_l, &tape_c, drive), loops[1].record(in_r, &tape_c, drive));
                (a + (in_l - a) * fz, b + (in_r - b) * fz)
            } else {
                (in_l, in_r)
            };
            lines[0].push(in_l);
            lines[1].push(in_r);
            if engine == 2 {
                bbd[0].push(in_l);
                bbd[1].push(in_r);
            }
            let (mut wl, mut wr) = if mode == 2 { (tap_l, tap_l) } else { (tap_l, tap_r) };
            // Multi-tap: earlier taps inside the main time, crossing sides.
            if taps > 1 {
                let (mut xl, mut xr) = (0.0, 0.0);
                for (k, (at, g)) in tap_pattern(taps).enumerate() {
                    let a = lines[0].read_cubic((dl * at).max(1.0)) * g;
                    let b = if mode == 2 { a } else { lines[1].read_cubic((dr * at).max(1.0)) * g };
                    if k % 2 == 0 {
                        xl += b;
                        xr += a;
                    } else {
                        xl += a;
                        xr += b;
                    }
                }
                if engine == 2 {
                    xl = tap_lp[0].lowpass(xl);
                    xr = tap_lp[1].lowpass(xr);
                }
                wl += xl;
                wr += xr;
            }
            if engine == 1 {
                wl = bump[0].process(&tape_c.bump, wl);
                wr = bump[1].process(&tape_c.bump, wr);
            }
            // Width on the wet signal.
            let w = width.next();
            let m = 0.5 * (wl + wr);
            let s = 0.5 * (wl - wr) * w;
            wl = m + s;
            wr = m - s;
            // Ducking: the dry input's envelope pushes the echoes down.
            let env = duck_env.process(mono);
            duck_gain = 1.0 - duck_amount * (env * 4.0).min(1.0);
            let wet = wet.next() * duck_gain;
            wl *= wet;
            wr *= wet;
            peak[0] = (peak[0] * decay).max(wl.abs());
            peak[1] = (peak[1] * decay).max(wr.abs());
            let dry = dry.next();
            (sanitize(l * dry + wl), sanitize(r * dry + wr))
        });
        self.peak = peak.map(|v| if v.is_finite() { v } else { 0.0 });
        self.duck_gain = duck_gain;
    }
}

impl Processor for Delay {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        if self.sr != sample_rate {
            self.sr = sample_rate;
            let n = ((MAX_TIME_MS + MAX_MOD_MS + WOW_MS + FLUTTER_MS + 10.0) * 0.001 * sample_rate as f32) as usize;
            self.lines = [DelayLine::new(n), DelayLine::new(n)];
            self.bbd = [Bbd::new(sample_rate), Bbd::new(sample_rate)];
            self.diffusers = diffusers(sample_rate);
        }
        for (s, ms) in [(&mut self.feedback, 20.0), (&mut self.wet, 20.0), (&mut self.dry, 20.0), (&mut self.width, 20.0), (&mut self.freeze, 30.0)] {
            s.set_time(ms, sample_rate);
        }
        // Time glides with a ~120 ms constant (pitch-bending repeats).
        for t in &mut self.time {
            t.set_time(120.0, sample_rate);
        }
        for t in &mut self.transport {
            t.set_rate(sample_rate as f32);
        }
        self.duck_env.set_times(5.0, 250.0, sample_rate);
        self.first = true;
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
        for b in &mut self.bbd {
            b.clear();
        }
        for l in &mut self.loops {
            l.reset();
        }
        for d in self.diffusers.iter_mut().flatten() {
            d.clear();
        }
        self.bump = [SvfState::default(); 2];
        for t in &mut self.tap_lp {
            t.reset();
        }
        self.duck_env.reset();
        self.mod_phase = 0.0;
        self.peak = [0.0; 2];
        self.duck_gain = 1.0;
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
        if self.params.on(p::FREEZE) {
            return (60.0 * self.sr) as u32;
        }
        let fb = (self.params.plain(p::FEEDBACK) / 100.0).clamp(0.0, 1.0);
        let t = self.time_ms(false, 120.0).max(self.time_ms(true, 120.0)) * 0.001;
        // Repeats until −60 dB, capped at 30 s (100 % feedback rings on).
        let repeats = if fb < 0.999 { (0.001f32.ln() / fb.max(1e-3).ln()).max(1.0) } else { 1e9 };
        ((t * (repeats + 1.0) + 0.05).min(30.0) * self.sr as f32) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.peak[0], self.peak[1], self.duck_gain, self.freeze.value()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, peak, process_stereo, process_stereo_prepared, rms, set_plain};
    use aitrack_dsp::Transport;

    fn dry_free(d: &mut Delay) {
        set_plain(d, p::MIX, 100.0);
        set_plain(d, p::LOW_CUT, 20.0);
        set_plain(d, p::HIGH_CUT, 20_000.0);
    }

    #[test]
    fn echo_lands_on_the_exact_sample() {
        let mut d = Delay::new();
        dry_free(&mut d);
        set_plain(&mut d, p::SYNC, 0.0);
        set_plain(&mut d, p::TIME_L, 10.0);
        set_plain(&mut d, p::TIME_R, 10.0);
        set_plain(&mut d, p::FEEDBACK, 0.0);
        let (l, _) = process_stereo(&mut d, |i| if i == 100 { (1.0, 1.0) } else { (0.0, 0.0) }, 2000);
        let (imax, _) = l.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap();
        assert_eq!(imax, 100 + 480);
    }

    #[test]
    fn sync_quarter_at_120_is_500_ms() {
        let mut d = Delay::new();
        set_plain(&mut d, p::SYNC, 1.0);
        set_plain(&mut d, p::DIV_L, 7.0); // 1/4
        assert!((d.time_ms(false, 120.0) - 500.0).abs() < 1e-3);
        dry_free(&mut d);
        set_plain(&mut d, p::FEEDBACK, 0.0);
        d.prepare(SR, 128);
        let tr = Transport { bpm: 120.0, ..Transport::default() };
        let (l, _) = process_stereo_prepared(&mut d, |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, 30_000, &tr);
        let (imax, _) = l.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap();
        assert_eq!(imax, 24_000);
    }

    #[test]
    fn ping_pong_alternates() {
        let mut d = Delay::new();
        dry_free(&mut d);
        set_plain(&mut d, p::MODE, 1.0);
        set_plain(&mut d, p::SYNC, 0.0);
        set_plain(&mut d, p::TIME_L, 100.0);
        set_plain(&mut d, p::FEEDBACK, 80.0);
        let (l, r) = process_stereo(&mut d, |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, 20_000);
        let at = |x: &[f32], n: usize| peak(&x[n * 4800 - 50..n * 4800 + 50]);
        assert!(at(&l, 1) > 0.3 && at(&r, 1) < 0.01, "first echo left");
        assert!(at(&r, 2) > 0.2 && at(&l, 2) < 0.01, "second echo right");
        assert!(at(&l, 3) > 0.1 && at(&r, 3) < 0.01, "third echo left");
    }

    #[test]
    fn full_feedback_is_bounded() {
        let mut d = Delay::new();
        dry_free(&mut d);
        set_plain(&mut d, p::FEEDBACK, 100.0);
        set_plain(&mut d, p::SYNC, 0.0);
        set_plain(&mut d, p::TIME_L, 5.0);
        set_plain(&mut d, p::TIME_R, 7.0);
        set_plain(&mut d, p::DRIVE, 24.0);
        let (l, r) = process_stereo(&mut d, |i| if i < 48_000 { (((i * 31) % 17) as f32 / 4.0 - 2.0, 1.0) } else { (0.0, 0.0) }, 96_000);
        assert!(peak(&l) < 4.0 && peak(&r) < 4.0, "{} {}", peak(&l), peak(&r));
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    }

    #[test]
    fn ducking_lowers_wet_while_input_plays() {
        let mut d = Delay::new();
        dry_free(&mut d);
        set_plain(&mut d, p::SYNC, 0.0);
        set_plain(&mut d, p::TIME_L, 50.0);
        set_plain(&mut d, p::TIME_R, 50.0);
        set_plain(&mut d, p::FEEDBACK, 90.0);
        set_plain(&mut d, p::DUCK, 100.0);
        let sig = |i: usize| {
            let v = if i < 24_000 { 0.5 * (i as f32 * 0.05).sin() } else { 0.0 };
            (v, v)
        };
        let (l, _) = process_stereo(&mut d, sig, 48_000);
        let during = peak(&l[12_000..24_000]);
        let after = peak(&l[30_000..36_000]);
        assert!(after > 2.0 * during, "during {during} after {after}");
    }

    fn impulse(i: usize) -> (f32, f32) {
        if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) }
    }

    fn free(d: &mut Delay, engine: f32, ms: f32, fb: f32) {
        dry_free(d);
        set_plain(d, p::ENGINE, engine);
        set_plain(d, p::SYNC, 0.0);
        set_plain(d, p::TIME_L, ms);
        set_plain(d, p::TIME_R, ms);
        set_plain(d, p::FEEDBACK, fb);
    }

    fn argmax(x: &[f32]) -> usize {
        x.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0
    }

    #[test]
    fn every_engine_puts_the_echo_on_time() {
        for engine in [0.0, 1.0, 2.0] {
            let mut d = Delay::new();
            free(&mut d, engine, 100.0, 0.0);
            set_plain(&mut d, p::AGE, 0.0);
            let (l, _) = process_stereo(&mut d, impulse, 9_600);
            let at = argmax(&l) as i32;
            assert!((at - 4_800).abs() < 24, "engine {engine}: echo at {at}");
            assert!(peak(&l[..4_700]) < 0.02, "engine {engine}: nothing before the echo");
        }
    }

    /// Level of a steady tone through the wet path, relative to its input.
    fn tone_gain(engine: f32, age: f32, freq: f32) -> f32 {
        let mut d = Delay::new();
        free(&mut d, engine, 50.0, 0.0);
        set_plain(&mut d, p::AGE, age);
        let sig = move |i: usize| {
            let v = 0.25 * (std::f32::consts::TAU * freq * i as f32 / SR as f32).sin();
            (v, v)
        };
        let (l, _) = process_stereo(&mut d, sig, 24_000);
        rms(&l[12_000..]) / (0.25 / 2f32.sqrt())
    }

    #[test]
    fn tape_and_bbd_darken_with_age_and_digital_does_not() {
        let digital = tone_gain(0.0, 100.0, 7_000.0);
        assert!((digital - 1.0).abs() < 0.05, "digital is flat: {digital}");
        for engine in [1.0, 2.0] {
            let mid = tone_gain(engine, 0.0, 500.0);
            assert!((mid - 1.0).abs() < 0.2, "engine {engine}: mids pass at {mid}");
            let (fresh, worn) = (tone_gain(engine, 0.0, 7_000.0), tone_gain(engine, 100.0, 7_000.0));
            assert!(worn < 0.25 * fresh, "engine {engine}: age darkens {fresh} → {worn}");
        }
    }

    #[test]
    fn wow_bends_the_pitch_of_a_held_tone() {
        // Zero crossings of the wet output: a steady delay keeps their
        // spacing, wow spreads it.
        let spread = |wow: f32| {
            let mut d = Delay::new();
            free(&mut d, 1.0, 300.0, 0.0);
            set_plain(&mut d, p::WOW, wow);
            set_plain(&mut d, p::AGE, 0.0);
            let sig = |i: usize| {
                let v = 0.2 * (std::f32::consts::TAU * 1_000.0 * i as f32 / SR as f32).sin();
                (v, v)
            };
            let (l, _) = process_stereo(&mut d, sig, 96_000);
            let mut periods = Vec::new();
            let mut last = None;
            for i in 20_000..96_000 {
                if l[i - 1] < 0.0 && l[i] >= 0.0 {
                    let t = i as f32 - l[i] / (l[i] - l[i - 1]);
                    if let Some(prev) = last {
                        periods.push(t - prev);
                    }
                    last = Some(t);
                }
            }
            let (lo, hi) = periods.iter().fold((f32::MAX, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
            hi - lo
        };
        let still = spread(0.0);
        let wobbly = spread(100.0);
        assert!(wobbly > 4.0 * still.max(0.01), "period spread {still} → {wobbly}");
    }

    #[test]
    fn multi_tap_adds_echoes_inside_the_main_time() {
        let mut d = Delay::new();
        free(&mut d, 0.0, 400.0, 0.0);
        set_plain(&mut d, p::TAPS, 3.0);
        set_plain(&mut d, p::WIDTH, 100.0);
        let (l, r) = process_stereo(&mut d, impulse, 24_000);
        for k in 1..=4 {
            let at = 19_200 * k / 4;
            let near = |x: &[f32]| peak(&x[at - 30..at + 30]);
            assert!(near(&l).max(near(&r)) > 0.2, "tap {k} of 4 at {at}");
        }
        // The first tap is quieter than the main one.
        assert!(peak(&l[4_700..4_900]).max(peak(&r[4_700..4_900])) < peak(&l[19_100..19_300]));
    }

    #[test]
    fn diffusion_smears_the_repeats() {
        let crest = |diff: f32| {
            let mut d = Delay::new();
            free(&mut d, 0.0, 100.0, 70.0);
            set_plain(&mut d, p::DIFFUSION, diff);
            let (l, _) = process_stereo(&mut d, impulse, 48_000);
            let w = &l[14_000..48_000];
            peak(w) / rms(w)
        };
        let (clean, diffused) = (crest(0.0), crest(100.0));
        assert!(diffused < 0.5 * clean, "crest {clean} → {diffused}");
    }

    #[test]
    fn freeze_holds_the_loop_and_mutes_the_input() {
        for engine in [0.0, 1.0, 2.0] {
            let mut d = Delay::new();
            free(&mut d, engine, 250.0, 0.0);
            set_plain(&mut d, p::FREEZE, 1.0);
            // Freeze first with the loop fed, then nothing more gets in.
            set_plain(&mut d, p::FREEZE, 0.0);
            set_plain(&mut d, p::FEEDBACK, 50.0);
            let mut src = |i: usize| {
                let v = if i < 12_000 { 0.3 * (i as f32 * 0.07).sin() } else if i > 30_000 { 0.5 } else { 0.0 };
                (v, v)
            };
            let tr = aitrack_dsp::Transport::default();
            d.prepare(SR, 128);
            let (a, _) = process_stereo_prepared(&mut d, &mut src, 12_000, &tr);
            set_plain(&mut d, p::FREEZE, 1.0);
            let (b, _) = process_stereo_prepared(&mut d, |i| src(i + 12_000), 96_000, &tr);
            let early = rms(&b[12_000..24_000]);
            let late = rms(&b[84_000..96_000]);
            assert!(a.iter().all(|v| v.is_finite()) && early > 0.02, "engine {engine}: frozen loop plays {early}");
            assert!(late > 0.5 * early && late < 1.5 * early, "engine {engine}: held {early} → {late}");
        }
    }

    #[test]
    fn every_engine_and_mode_is_bounded_at_full_feedback_and_drive() {
        for engine in [0.0, 1.0, 2.0] {
            for mode in [0.0, 1.0, 2.0] {
                let mut d = Delay::new();
                free(&mut d, engine, 30.0, 100.0);
                set_plain(&mut d, p::MODE, mode);
                set_plain(&mut d, p::DRIVE, 24.0);
                set_plain(&mut d, p::WOW, 100.0);
                set_plain(&mut d, p::FLUTTER, 100.0);
                set_plain(&mut d, p::DIFFUSION, 100.0);
                set_plain(&mut d, p::TAPS, 3.0);
                set_plain(&mut d, p::LOW_CUT, 20.0);
                let (l, r) = process_stereo(&mut d, |i| if i < 48_000 { (((i * 31) % 17) as f32 / 4.0 - 2.0, 1.0) } else { (0.0, 0.0) }, 96_000);
                assert!(l.iter().chain(&r).all(|v| v.is_finite()), "engine {engine} mode {mode}");
                assert!(peak(&l) < 8.0 && peak(&r) < 8.0, "engine {engine} mode {mode}: {} {}", peak(&l), peak(&r));
            }
        }
    }

    #[test]
    fn cpu_budget_per_instance() {
        // Worst case: BBD at its shortest clock period, every extra on.
        let mut d = Delay::new();
        free(&mut d, 2.0, 12.0, 60.0);
        set_plain(&mut d, p::TIME_R, 13.0);
        set_plain(&mut d, p::WOW, 50.0);
        set_plain(&mut d, p::FLUTTER, 50.0);
        set_plain(&mut d, p::TAPS, 3.0);
        set_plain(&mut d, p::DIFFUSION, 50.0);
        set_plain(&mut d, p::DUCK, 50.0);
        set_plain(&mut d, p::MOD_DEPTH, 50.0);
        let secs = 2.0;
        let t = std::time::Instant::now();
        let (l, _) = process_stereo(&mut d, |i| (((i * 37) % 23) as f32 / 23.0 - 0.5, 0.2), (secs * SR) as usize);
        let share = t.elapsed().as_secs_f64() / secs;
        assert!(l.iter().all(|v| v.is_finite()));
        let budget = if cfg!(debug_assertions) { 0.25 } else { 0.05 };
        assert!(share < budget, "delay uses {:.2} % of a core", share * 100.0);
    }
}
