//! `fx.delay` — stereo / ping-pong / mono delay with tempo sync, filtered
//! and soft-clipped feedback, modulation, ducking and width. Zero latency.
//!
//! The feedback path runs high-pass → low-pass → ADAA tanh, so even 100 %
//! feedback stays bounded (the clipper caps the loop). Time changes glide
//! (tape-style pitch bend) instead of jumping.
//!
//! Meter layout: `[wet peak L, wet peak R, duck gain (0..1)]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower};
use aitrack_dsp::lfo::{SYNC_DIVISIONS, division_seconds};
use aitrack_dsp::shaper::{Adaa1, ShaperKind};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

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
}

const MODES: &[&str] = &["Stereo", "Ping-Pong", "Mono"];
const OFF_ON: &[&str] = &["Off", "On"];
/// Longest time a tap can reach (sync divisions are clamped to it).
pub const MAX_TIME_MS: f32 = 4000.0;
/// Peak modulation excursion at 100 % depth.
const MAX_MOD_MS: f32 = 6.0;

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
    ]
}

/// Per-channel feedback-loop processing.
#[derive(Clone, Copy)]
struct Loop {
    hp: SvfState,
    lp: SvfState,
    clip: Adaa1,
}

impl Loop {
    fn new() -> Self {
        Self { hp: SvfState::default(), lp: SvfState::default(), clip: Adaa1::new(ShaperKind::Tanh) }
    }

    #[inline]
    fn process(&mut self, x: f32, hp: &SvfCoeffs, lp: &SvfCoeffs, drive: f32) -> f32 {
        let y = self.lp.process(lp, self.hp.process(hp, x));
        self.clip.process(y * drive) / drive
    }

    fn reset(&mut self) {
        self.hp.reset();
        self.lp.reset();
        self.clip.reset();
    }
}

pub struct Delay {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    lines: [DelayLine; 2],
    loops: [Loop; 2],
    hp: SvfCoeffs,
    lp: SvfCoeffs,
    time: [Smoother; 2],
    feedback: Smoother,
    wet: Smoother,
    dry: Smoother,
    width: Smoother,
    duck_env: EnvelopeFollower,
    mod_phase: f64,
    mod_inc: f64,
    mod_depth: f32,
    drive: f32,
    duck_amount: f32,
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

impl Delay {
    pub fn new() -> Self {
        let params = params();
        let mut d = Self {
            info: PluginInfo {
                id: ID,
                name: "Delay",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["delay", "echo", "ping-pong"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 0.0,
            lines: [DelayLine::new(16), DelayLine::new(16)],
            loops: [Loop::new(), Loop::new()],
            hp: SvfCoeffs::default(),
            lp: SvfCoeffs::default(),
            time: [Smoother::new(1000.0); 2],
            feedback: Smoother::new(0.0),
            wet: Smoother::new(0.0),
            dry: Smoother::new(1.0),
            width: Smoother::new(1.0),
            duck_env: EnvelopeFollower::new(DetectorMode::Peak),
            mod_phase: 0.0,
            mod_inc: 0.0,
            mod_depth: 0.0,
            drive: 1.0,
            duck_amount: 0.0,
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
        self.hp = SvfCoeffs::new(SvfMode::Highpass, self.params.plain(p::LOW_CUT), 0.707, 0.0, sr);
        self.lp = SvfCoeffs::new(SvfMode::Lowpass, self.params.plain(p::HIGH_CUT), 0.707, 0.0, sr);
        self.feedback.set_target(self.params.plain(p::FEEDBACK) / 100.0);
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target((2.0 * mix).min(1.0) * out);
        self.dry.set_target((2.0 * (1.0 - mix)).min(1.0) * out);
        self.width.set_target(self.params.plain(p::WIDTH) / 100.0);
        self.mod_inc = self.params.plain(p::MOD_RATE) as f64 / sr;
        self.mod_depth = self.params.plain(p::MOD_DEPTH) / 100.0 * MAX_MOD_MS * 0.001 * sr as f32;
        self.drive = db_to_gain(self.params.plain(p::DRIVE));
        self.duck_amount = self.params.plain(p::DUCK) / 100.0;
        if self.first {
            for s in [&mut self.feedback, &mut self.wet, &mut self.dry, &mut self.width] {
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
        let (hp, lp) = (self.hp, self.lp);
        let drive = self.drive;
        let depth = self.mod_depth;
        let duck_amount = self.duck_amount;
        let max_d = self.lines[0].max_delay() as f32 - 2.0;
        let decay = (-1.0 / (0.3 * self.sr as f32)).exp();
        let mut peak = self.peak;
        let mut duck_gain = self.duck_gain;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            // Modulation: quadrature sines so the sides move apart.
            self.mod_phase += self.mod_inc;
            if self.mod_phase >= 1.0 {
                self.mod_phase -= 1.0;
            }
            let ph = (self.mod_phase * std::f64::consts::TAU) as f32;
            let (ms, mc) = ph.sin_cos();
            let dl = (self.time[0].next() + depth * (0.5 + 0.5 * ms)).clamp(1.0, max_d);
            let dr = (self.time[1].next() + depth * (0.5 + 0.5 * mc)).clamp(1.0, max_d);
            let tap_l = self.lines[0].read_cubic(dl - 1.0);
            let tap_r = self.lines[1].read_cubic(dr - 1.0);
            let fb = self.feedback.next();
            let mono = 0.5 * (l + r);
            match mode {
                1 => {
                    // Ping-pong: input enters the left line, the lines feed
                    // each other.
                    let back_l = self.loops[0].process(tap_r, &hp, &lp, drive);
                    let back_r = self.loops[1].process(tap_l, &hp, &lp, drive);
                    self.lines[0].push(mono + fb * back_l);
                    self.lines[1].push(fb * back_r);
                }
                2 => {
                    let back = self.loops[0].process(tap_l, &hp, &lp, drive);
                    self.lines[0].push(mono + fb * back);
                    self.lines[1].push(0.0);
                }
                _ => {
                    let back_l = self.loops[0].process(tap_l, &hp, &lp, drive);
                    let back_r = self.loops[1].process(tap_r, &hp, &lp, drive);
                    self.lines[0].push(l + fb * back_l);
                    self.lines[1].push(r + fb * back_r);
                }
            }
            let (mut wl, mut wr) = if mode == 2 { (tap_l, tap_l) } else { (tap_l, tap_r) };
            // Width on the wet signal.
            let w = self.width.next();
            let m = 0.5 * (wl + wr);
            let s = 0.5 * (wl - wr) * w;
            wl = m + s;
            wr = m - s;
            // Ducking: the dry input's envelope pushes the echoes down.
            let env = self.duck_env.process(mono);
            duck_gain = 1.0 - duck_amount * (env * 4.0).min(1.0);
            let wet = self.wet.next() * duck_gain;
            wl *= wet;
            wr *= wet;
            peak[0] = (peak[0] * decay).max(wl.abs());
            peak[1] = (peak[1] * decay).max(wr.abs());
            let dry = self.dry.next();
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
            let n = ((MAX_TIME_MS + MAX_MOD_MS + 10.0) * 0.001 * sample_rate as f32) as usize;
            self.lines = [DelayLine::new(n), DelayLine::new(n)];
        }
        for (s, ms) in [(&mut self.feedback, 20.0), (&mut self.wet, 20.0), (&mut self.dry, 20.0), (&mut self.width, 20.0)] {
            s.set_time(ms, sample_rate);
        }
        // Time glides with a ~120 ms constant (pitch-bending repeats).
        for t in &mut self.time {
            t.set_time(120.0, sample_rate);
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
        for l in &mut self.loops {
            l.reset();
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
        let fb = (self.params.plain(p::FEEDBACK) / 100.0).clamp(0.0, 1.0);
        let t = self.time_ms(false, 120.0).max(self.time_ms(true, 120.0)) * 0.001;
        // Repeats until −60 dB, capped at 30 s (100 % feedback rings on).
        let repeats = if fb < 0.999 { (0.001f32.ln() / fb.max(1e-3).ln()).max(1.0) } else { 1e9 };
        ((t * (repeats + 1.0)).min(30.0) * self.sr as f32) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.peak[0], self.peak[1], self.duck_gain])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, peak, process_stereo, process_stereo_prepared, set_plain};
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
}
