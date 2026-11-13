//! `fx.shaper` — a beat- or audio-triggered envelope shaper for **Volume**,
//! **Filter LP**, and **Pan**. Zero latency.
//!
//! The curve has six nodes: the first sits at the start of the cycle, the
//! last at its end, and four in between move freely. One `Tension` bends
//! every segment. In `Sync` trigger mode, transport playback follows song
//! position (free-running when stopped). In `Audio` mode, incoming stereo
//! peaks crossing `Threshold` trigger one cycle of the curve cleanly.
//!
//! Meter layout: `[phase 0..1, shaped 0..1]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "fx.shaper";

pub mod p {
    pub const RATE: u32 = 0;
    pub const DEPTH: u32 = 1;
    pub const SMOOTH: u32 = 2;
    pub const TENSION: u32 = 3;
    pub const PHASE: u32 = 4;
    /// Level at the start of the cycle.
    pub const Y0: u32 = 5;
    pub const X1: u32 = 6;
    pub const Y1: u32 = 7;
    pub const X2: u32 = 8;
    pub const Y2: u32 = 9;
    pub const X3: u32 = 10;
    pub const Y3: u32 = 11;
    pub const X4: u32 = 12;
    pub const Y4: u32 = 13;
    /// Level at the end of the cycle.
    pub const Y5: u32 = 14;
    pub const MODE: u32 = 15;
    pub const TRIGGER: u32 = 16;
    pub const THRESHOLD: u32 = 17;
    pub const CUTOFF: u32 = 18;
    pub const RESONANCE: u32 = 19;

    /// `(x, y)` parameter ids of the four free nodes.
    pub const NODES: [(u32, u32); 4] = [(X1, Y1), (X2, Y2), (X3, Y3), (X4, Y4)];
}

/// Cycle lengths, in quarter notes.
pub const RATE_BEATS: [f64; 7] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];
const RATES: &[&str] = &["1/16", "1/8", "1/4", "1/2", "1 bar", "2 bars", "4 bars"];
pub const MODES: &[&str] = &["Volume", "Filter LP", "Pan"];
pub const TRIGGERS: &[&str] = &["Sync", "Audio"];

pub fn params() -> Vec<ParamInfo> {
    let node = |id, name, short, default| ParamInfo::new(id, name, 0.0, 1.0, default).short(short);
    vec![
        ParamInfo::new(p::RATE, "Rate", 0.0, 6.0, 2.0).choices(RATES),
        ParamInfo::new(p::DEPTH, "Depth", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::SMOOTH, "Smooth", 0.0, 50.0, 4.0).unit("ms"),
        ParamInfo::new(p::TENSION, "Tension", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::PHASE, "Phase", 0.0, 100.0, 0.0).unit("%"),
        // The default curve is the pumping duck: down on the beat, back up by the off-beat.
        node(p::Y0, "Start Level", "Start", 0.0),
        node(p::X1, "Node 1 Time", "T1", 0.06),
        node(p::Y1, "Node 1 Level", "L1", 0.0),
        node(p::X2, "Node 2 Time", "T2", 0.3),
        node(p::Y2, "Node 2 Level", "L2", 0.7),
        node(p::X3, "Node 3 Time", "T3", 0.55),
        node(p::Y3, "Node 3 Level", "L3", 1.0),
        node(p::X4, "Node 4 Time", "T4", 0.9),
        node(p::Y4, "Node 4 Level", "L4", 1.0),
        node(p::Y5, "End Level", "End", 1.0),
        ParamInfo::new(p::MODE, "Target", 0.0, 2.0, 0.0).choices(MODES).short("Mode"),
        ParamInfo::new(p::TRIGGER, "Trigger", 0.0, 1.0, 0.0).choices(TRIGGERS).short("Trig"),
        ParamInfo::new(p::THRESHOLD, "Trigger Threshold", -48.0, 0.0, -18.0).unit("dB").short("Thresh"),
        ParamInfo::new(p::CUTOFF, "Filter Base Cutoff", 40.0, 18_000.0, 320.0).unit("Hz").log().short("Cut"),
        ParamInfo::new(p::RESONANCE, "Filter Resonance", 0.0, 100.0, 25.0).unit("%").short("Res"),
    ]
}

/// The curve's six points in time order, from plain parameter values.
pub fn curve_points(plain: impl Fn(u32) -> f32) -> [(f32, f32); 6] {
    let mut mid = p::NODES.map(|(x, y)| (plain(x).clamp(0.0, 1.0), plain(y).clamp(0.0, 1.0)));
    mid.sort_unstable_by(|a, b| a.0.total_cmp(&b.0)); // never allocates: this runs on the audio thread
    [(0.0, plain(p::Y0).clamp(0.0, 1.0)), mid[0], mid[1], mid[2], mid[3], (1.0, plain(p::Y5).clamp(0.0, 1.0))]
}

/// Level (0..=1) of the curve at `phase` (0..1). `tension` is −1..1: positive
/// holds each segment back and lets it arrive late, negative rushes it.
pub fn level_at(points: &[(f32, f32); 6], tension: f32, phase: f32) -> f32 {
    let x = phase.clamp(0.0, 1.0);
    let i = points.iter().rposition(|pt| pt.0 <= x).unwrap_or(0).min(4);
    let (a, b) = (points[i], points[i + 1]);
    let span = b.0 - a.0;
    if span <= 1e-6 {
        return b.1;
    }
    let t = ((x - a.0) / span).clamp(0.0, 1.0);
    let shaped = if tension.abs() < 1e-3 {
        t
    } else {
        let k = tension * 6.0;
        ((k * t).exp() - 1.0) / (k.exp() - 1.0)
    };
    a.1 + (b.1 - a.1) * shaped
}

pub struct Shaper {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    points: [(f32, f32); 6],
    tension: f32,
    depth: f32,
    cycle_beats: f64,
    offset: f64,
    smooth: f32,
    mode: usize,
    audio_trigger: bool,
    threshold: f32,
    cutoff_hz: f32,
    res_q: f32,
    /// Free-running position (in cycles) while the transport is stopped.
    free: f64,
    /// Current phase in Audio-trigger one-shot mode (1.0 = idle at end).
    trig_phase: f32,
    trig_holdoff: u32,
    prev_over: bool,
    filt: [SvfState; 2],
    gain: f32,
    phase: f32,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Shaper::new())
}

impl Default for Shaper {
    fn default() -> Self {
        Self::new()
    }
}

impl Shaper {
    pub fn new() -> Self {
        let params = params();
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Shaper",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["dynamics", "sidechain", "pump", "gate", "tremolo", "volume", "filter", "pan"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            points: [(0.0, 1.0); 6],
            tension: 0.0,
            depth: 1.0,
            cycle_beats: 1.0,
            offset: 0.0,
            smooth: 1.0,
            mode: 0,
            audio_trigger: false,
            threshold: 0.125,
            cutoff_hz: 320.0,
            res_q: 1.4,
            free: 0.0,
            trig_phase: 1.0,
            trig_holdoff: 0,
            prev_over: false,
            filt: [SvfState::default(); 2],
            gain: 1.0,
            phase: 0.0,
            first: true,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let params = &self.params;
        self.points = curve_points(|id| params.plain(id));
        self.tension = self.params.plain(p::TENSION) / 100.0;
        self.depth = self.params.plain(p::DEPTH) / 100.0;
        self.cycle_beats = RATE_BEATS[self.params.choice(p::RATE).min(RATE_BEATS.len() - 1)];
        self.offset = f64::from(self.params.plain(p::PHASE)) / 100.0;
        let ms = self.params.plain(p::SMOOTH);
        self.smooth = if ms <= 0.01 { 1.0 } else { 1.0 - (-1.0 / (ms * 0.001 * self.sr as f32)).exp() };
        self.mode = self.params.choice(p::MODE);
        self.audio_trigger = self.params.choice(p::TRIGGER) == 1;
        self.threshold = db_to_gain(self.params.plain(p::THRESHOLD));
        self.cutoff_hz = self.params.plain(p::CUTOFF);
        let r = (self.params.plain(p::RESONANCE) / 100.0).clamp(0.0, 1.0);
        self.res_q = std::f32::consts::FRAC_1_SQRT_2 * (12.0f32 / std::f32::consts::FRAC_1_SQRT_2).powf(r);
    }
}

impl Segmented for Shaper {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let tr = *ctx.transport;
        let cycles_per_sample = tr.bpm.max(1.0) / 60.0 / self.sr / self.cycle_beats;
        let playing = tr.playing;
        let (points, tension, depth, smooth, offset) = (self.points, self.tension, self.depth, self.smooth, self.offset);
        let (mode, audio_trig, thresh, base_cut, res_q, sr) = (self.mode, self.audio_trigger, self.threshold, self.cutoff_hz, self.res_q, self.sr);
        let holdoff_len = (0.04 * sr) as u32;
        let mut gain = self.gain;
        let mut free = self.free;
        let mut trig_phase = self.trig_phase;
        let mut trig_holdoff = self.trig_holdoff;
        let mut prev_over = self.prev_over;
        let mut filt = self.filt;
        let mut phase = self.phase;
        let mut first = self.first;
        for_frames(ctx.buffer, start, end, |i, l, r| {
            if audio_trig {
                let over = l.abs().max(r.abs()) >= thresh;
                if over && !prev_over && trig_holdoff == 0 {
                    trig_phase = 0.0;
                    trig_holdoff = holdoff_len;
                }
                prev_over = over;
                trig_holdoff = trig_holdoff.saturating_sub(1);
                phase = ((trig_phase as f64 + offset).clamp(0.0, 1.0)) as f32;
                trig_phase = (trig_phase + cycles_per_sample as f32).min(1.0);
            } else {
                let cycles = if playing { tr.position_beats / self.cycle_beats + i as f64 * cycles_per_sample } else { free };
                free += cycles_per_sample;
                phase = (cycles + offset).rem_euclid(1.0) as f32;
            }

            let raw = level_at(&points, tension, phase);
            let target = if mode == 2 {
                // Pan curves swing around 0.5 center (0 -> full L, 1 -> full R).
                0.5 + (raw - 0.5) * depth
            } else {
                1.0 - depth * (1.0 - raw)
            };
            if first {
                gain = target;
                first = false;
            } else {
                gain += (target - gain) * smooth;
            }
            match mode {
                1 => {
                    let fc = (base_cut * (19_000.0 / base_cut.max(20.0)).powf(gain.clamp(0.0, 1.0))).clamp(30.0, (0.45 * sr as f32).min(20_000.0));
                    let coeffs = SvfCoeffs::new(SvfMode::Lowpass, fc, res_q, 0.0, sr);
                    (sanitize(filt[0].process(&coeffs, l)), sanitize(filt[1].process(&coeffs, r)))
                }
                2 => {
                    let pan = (gain * 2.0 - 1.0).clamp(-1.0, 1.0);
                    let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
                    let gl = angle.cos() * std::f32::consts::SQRT_2;
                    let gr = angle.sin() * std::f32::consts::SQRT_2;
                    (sanitize(l * gl), sanitize(r * gr))
                }
                _ => (sanitize(l * gain), sanitize(r * gain)),
            }
        });
        // Stopped, keep the free-running clock small and in step.
        self.free = if playing { (tr.position_beats / self.cycle_beats + end as f64 * cycles_per_sample).rem_euclid(1.0) } else { free.rem_euclid(1.0) };
        self.gain = gain;
        self.trig_phase = trig_phase;
        self.trig_holdoff = trig_holdoff;
        self.prev_over = prev_over;
        self.filt = filt;
        self.phase = phase;
        self.first = first;
    }
}

impl Processor for Shaper {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        self.free = 0.0;
        self.trig_phase = 1.0;
        self.trig_holdoff = 0;
        self.prev_over = false;
        self.filt = [SvfState::default(); 2];
        self.gain = 1.0;
        self.phase = 0.0;
        self.first = true;
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
        write_meter(out, 0, &[self.phase, self.gain])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_dsp::{AudioBuffer, Transport};

    fn run(s: &mut Shaper, frames: usize, tr: &Transport) -> Vec<f32> {
        let mut buf = AudioBuffer::new(2, frames);
        for ch in 0..2 {
            buf.channel_mut(ch).fill(1.0);
        }
        let mut ctx = ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events: &[], transport: tr };
        s.process(&mut ctx);
        buf.channel(0).to_vec()
    }

    #[test]
    fn the_curve_passes_through_its_nodes_in_time_order() {
        let defaults = params();
        let pts = curve_points(|id| defaults.iter().find(|i| i.id == id).unwrap().default);
        assert_eq!(pts[0], (0.0, 0.0));
        assert_eq!(pts[5], (1.0, 1.0));
        assert!(pts.windows(2).all(|w| w[0].0 <= w[1].0));
        assert!((level_at(&pts, 0.0, 0.3) - 0.7).abs() < 1e-5);
        assert!((level_at(&pts, 0.0, 0.18) - 0.35).abs() < 1e-5, "half way between node 1 and node 2");
        // Positive tension arrives late, negative early; the ends do not move.
        assert!(level_at(&pts, 0.8, 0.18) < 0.2 && level_at(&pts, -0.8, 0.18) > 0.5);
        assert!((level_at(&pts, 0.8, 0.3) - 0.7).abs() < 1e-5);
        // Nodes given out of order are sorted, not crossed.
        let crossed = curve_points(|id| match id {
            p::X1 => 0.8,
            p::Y1 => 0.2,
            p::X2 => 0.1,
            p::Y2 => 0.9,
            _ => 0.5,
        });
        assert_eq!(crossed[1], (0.1, 0.9));
    }

    #[test]
    fn it_ducks_on_the_beat_and_recovers_before_the_next() {
        // 120 BPM, 1/4 cycle: one cycle is 24 000 samples.
        let mut s = Shaper::new();
        s.set_param(p::SMOOTH, 0.0);
        let tr = Transport { playing: true, bpm: 120.0, position_beats: 4.0, ..Transport::default() };
        let out = run(&mut s, 24_000, &tr);
        assert!(out[0] < 0.01 && out[1_000] < 0.01, "silent on the beat");
        assert!((out[7_200] - 0.7).abs() < 0.01, "node 2 at 30 % of the beat");
        assert!(out[14_000] > 0.99 && out[23_000] > 0.99, "fully back for the second half");
        // The cycle follows the song position, not when the plugin started.
        let tr = Transport { position_beats: 4.5, ..tr };
        assert!(run(&mut s, 64, &tr)[0] > 0.9, "half a beat in, the level is up");
    }

    #[test]
    fn depth_scales_the_cut_and_smoothing_removes_the_step() {
        let mut s = Shaper::new();
        s.set_param(p::SMOOTH, 0.0);
        s.set_param(p::DEPTH, 0.5);
        let tr = Transport { playing: true, bpm: 120.0, position_beats: 0.0, ..Transport::default() };
        assert!((run(&mut s, 64, &tr)[10] - 0.5).abs() < 0.01, "half depth cuts to half");
        // With smoothing, the jump from the end of one cycle (1) to the start of
        // the next (0) is a ramp, not a click.
        let mut s = Shaper::new();
        let before = Transport { playing: true, bpm: 120.0, position_beats: 0.99, ..Transport::default() };
        run(&mut s, 64, &before);
        let across = Transport { position_beats: 0.999, ..before };
        let out = run(&mut s, 512, &across);
        let biggest_step = out.windows(2).map(|w| (w[0] - w[1]).abs()).fold(0.0f32, f32::max);
        assert!(biggest_step < 0.02, "largest sample-to-sample change {biggest_step}");
        assert!(out[511] < 0.2, "and it does get down");
    }

    #[test]
    fn stopped_it_keeps_cycling_at_the_tempo() {
        let mut s = Shaper::new();
        s.set_param(p::SMOOTH, 0.0);
        let tr = Transport { playing: false, bpm: 120.0, ..Transport::default() };
        let out: Vec<f32> = (0..4).flat_map(|_| run(&mut s, 12_000, &tr)).collect();
        assert!(out[100] < 0.01 && out[24_100] < 0.01, "a new cycle every beat");
        assert!(out[20_000] > 0.99 && out[44_000] > 0.99);
    }

    #[test]
    fn filter_mode_attenuates_high_frequencies_when_curve_is_low() {
        let mut s = Shaper::new();
        crate::test_util::set_plain(&mut s, p::MODE, 1.0); // Filter LP
        crate::test_util::set_plain(&mut s, p::CUTOFF, 100.0);
        s.set_param(p::SMOOTH, 0.0);
        let tr = Transport { playing: true, bpm: 120.0, position_beats: 0.0, ..Transport::default() };
        let mut buf = AudioBuffer::new(2, 4_096);
        for i in 0..4_096 {
            let hi = (std::f32::consts::TAU * 6_000.0 * i as f32 / 48_000.0).sin();
            buf.channel_mut(0)[i] = hi;
            buf.channel_mut(1)[i] = hi;
        }
        let mut ctx = ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        s.process(&mut ctx);
        let early_rms = crate::test_util::rms(&buf.channel(0)[256..1_024]);
        assert!(early_rms < 0.15, "6kHz should be attenuated when LP curve starts near 100 Hz (rms={early_rms})");
    }

    #[test]
    fn pan_mode_steers_left_and_right_across_curve() {
        let mut s = Shaper::new();
        crate::test_util::set_plain(&mut s, p::MODE, 2.0); // Pan
        s.set_param(p::SMOOTH, 0.0);
        let tr = Transport { playing: true, bpm: 120.0, position_beats: 0.0, ..Transport::default() };
        let mut buf = AudioBuffer::new(2, 24_000);
        for ch in 0..2 {
            buf.channel_mut(ch).fill(0.5);
        }
        let mut ctx = ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        s.process(&mut ctx);
        // At start (curve ~0.0 -> full left), L > R; at end (curve ~1.0 -> full right), R > L.
        assert!(buf.channel(0)[10] > buf.channel(1)[10] + 0.3);
        assert!(buf.channel(1)[20_000] > buf.channel(0)[20_000] + 0.3);
    }

    #[test]
    fn audio_trigger_mode_fires_one_cycle_on_transient() {
        let mut s = Shaper::new();
        crate::test_util::set_plain(&mut s, p::TRIGGER, 1.0); // Audio
        crate::test_util::set_plain(&mut s, p::THRESHOLD, -12.0);
        s.set_param(p::SMOOTH, 0.0);
        let tr = Transport { playing: false, bpm: 120.0, ..Transport::default() };
        let mut buf = AudioBuffer::new(2, 24_000);
        // Quiet bed for first 4000 samples (must NOT trigger -> stays at end-of-cycle level 1.0).
        for ch in 0..2 {
            buf.channel_mut(ch)[0..4_000].fill(0.05);
            // Loud kick transient at sample 4000, followed by constant 0.5 signal to see ducking.
            buf.channel_mut(ch)[4_000..4_100].fill(0.9);
            buf.channel_mut(ch)[4_100..24_000].fill(0.5);
        }
        let mut ctx = ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        s.process(&mut ctx);
        assert!((buf.channel(0)[1_000] - 0.05).abs() < 1e-3, "idle before transient");
        assert!(buf.channel(0)[4_120].abs() < 0.02, "ducked right after transient: {}", buf.channel(0)[4_120]);
        assert!((buf.channel(0)[22_000] - 0.5).abs() < 0.02, "recovered to unity after cycle");
    }
}
