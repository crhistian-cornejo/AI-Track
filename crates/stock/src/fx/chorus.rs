//! `fx.chorus` — chorus (2–6 modulated voices), flanger (short modulated
//! delay with ± feedback) and phaser (2–12 first-order TPT all-pass stages
//! with feedback), free-running or tempo-synced, with stereo LFO spread.
//! Zero latency.
//!
//! Meter layout: `[LFO phase (0..1)]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::lfo::{SYNC_DIVISIONS, division_beats, division_hz};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use std::f32::consts::TAU;

pub const ID: &str = "fx.chorus";

pub mod p {
    pub const MODE: u32 = 0;
    pub const RATE: u32 = 1;
    pub const SYNC: u32 = 2;
    pub const DIVISION: u32 = 3;
    pub const DEPTH: u32 = 4;
    pub const FEEDBACK: u32 = 5;
    pub const VOICES: u32 = 6;
    pub const STAGES: u32 = 7;
    pub const DELAY: u32 = 8;
    pub const CENTER: u32 = 9;
    pub const SPREAD: u32 = 10;
    pub const MIX: u32 = 11;
    pub const OUTPUT: u32 = 12;
}

const MODES: &[&str] = &["Chorus", "Flanger", "Phaser"];
const OFF_ON: &[&str] = &["Off", "On"];
const VOICES: &[&str] = &["2", "3", "4", "5", "6"];
const STAGES: &[&str] = &["2", "4", "6", "8", "10", "12"];
const MAX_VOICES: usize = 6;
const MAX_STAGES: usize = 12;
/// Longest delay a chorus voice reaches (base + sweep).
const MAX_DELAY_MS: f32 = 50.0;

pub fn params() -> Vec<ParamInfo> {
    let n_div = (SYNC_DIVISIONS.len() - 1) as f32;
    vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 2.0, 0.0).choices(MODES),
        ParamInfo::new(p::RATE, "Rate", 0.01, 10.0, 0.8).unit("Hz").log(),
        ParamInfo::new(p::SYNC, "Sync", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::DIVISION, "Division", 0.0, n_div, 3.0).choices(SYNC_DIVISIONS).short("Div"),
        ParamInfo::new(p::DEPTH, "Depth", 0.0, 100.0, 50.0).unit("%"),
        ParamInfo::new(p::FEEDBACK, "Feedback", -95.0, 95.0, 0.0).unit("%").short("FB"),
        ParamInfo::new(p::VOICES, "Voices", 2.0, 6.0, 3.0).choices(VOICES),
        ParamInfo::new(p::STAGES, "Stages", 0.0, 5.0, 2.0).choices(STAGES),
        ParamInfo::new(p::DELAY, "Delay", 0.1, 30.0, 8.0).unit("ms").log(),
        ParamInfo::new(p::CENTER, "Phaser Center", 100.0, 8000.0, 800.0).unit("Hz").log().short("Center"),
        ParamInfo::new(p::SPREAD, "Stereo Spread", 0.0, 100.0, 50.0).unit("%").short("Spread"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 50.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
    ]
}

/// Chain of first-order TPT all-passes sharing one coefficient.
#[derive(Clone, Copy, Default)]
struct AllpassChain {
    s: [f32; MAX_STAGES],
}

impl AllpassChain {
    #[inline]
    fn process(&mut self, mut x: f32, g: f32, stages: usize) -> f32 {
        for s in self.s.iter_mut().take(stages) {
            let v = (x - *s) * g;
            let lp = v + *s;
            *s = lp + v;
            if s.abs() < 1e-20 {
                *s = 0.0;
            }
            x = 2.0 * lp - x;
        }
        x
    }
}

pub struct Chorus {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    lines: [DelayLine; 2],
    ap: [AllpassChain; 2],
    fb_state: [f32; 2],
    phase: f64,
    inc: f64,
    wet: Smoother,
    dry: Smoother,
    depth: Smoother,
    first: bool,
    counter: usize,
    ap_g: [f32; 2],
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Chorus::new())
}

impl Default for Chorus {
    fn default() -> Self {
        Self::new()
    }
}

impl Chorus {
    pub fn new() -> Self {
        let params = params();
        let mut c = Self {
            info: PluginInfo {
                id: ID,
                name: "Chorus",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["modulation", "chorus", "flanger", "phaser"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 0.0,
            lines: [DelayLine::new(16), DelayLine::new(16)],
            ap: [AllpassChain::default(); 2],
            fb_state: [0.0; 2],
            phase: 0.0,
            inc: 0.0,
            wet: Smoother::new(0.5),
            dry: Smoother::new(0.5),
            depth: Smoother::new(0.5),
            first: true,
            counter: 0,
            ap_g: [0.0; 2],
        };
        c.prepare(48_000.0, 512);
        c
    }

    fn update(&mut self) {
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry.set_target((1.0 - mix) * out);
        self.depth.set_target(self.params.plain(p::DEPTH) / 100.0);
        if self.first {
            self.wet.snap_to_target();
            self.dry.snap_to_target();
            self.depth.snap_to_target();
            self.first = false;
        }
    }

    pub fn lfo_phase(&self) -> f64 {
        self.phase
    }
}

impl Segmented for Chorus {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
            self.counter = 0;
        }
        let tr = ctx.transport;
        let sync = self.params.on(p::SYNC);
        let div = self.params.choice(p::DIVISION);
        let rate = if sync { division_hz(div, tr.bpm) as f32 } else { self.params.plain(p::RATE) };
        self.inc = rate as f64 / self.sr;
        if sync && tr.playing {
            let beats = tr.position_beats + start as f64 / self.sr * tr.bpm / 60.0;
            self.phase = (beats / division_beats(div)).rem_euclid(1.0);
        }
        let mode = self.params.choice(p::MODE);
        let voices = (self.params.plain(p::VOICES).round() as usize).clamp(2, MAX_VOICES);
        let stages = 2 * (self.params.choice(p::STAGES) + 1);
        let base_ms = self.params.plain(p::DELAY);
        let fb = self.params.plain(p::FEEDBACK) / 100.0;
        let spread = self.params.plain(p::SPREAD) / 100.0 * 0.5;
        let center = self.params.plain(p::CENTER);
        let ms = 0.001 * self.sr as f32;
        let max_d = self.lines[0].max_delay() as f32 - 2.0;
        let voice_norm = 1.0 / (voices as f32).sqrt();
        let nyq = self.sr as f32 * 0.45;
        let sr = self.sr;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            self.phase += self.inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            let ph = self.phase as f32;
            let depth = self.depth.next();
            let input = [l, r];
            let mut out = [0.0f32; 2];
            match mode {
                0 => {
                    // Chorus: voices spread over the LFO cycle, the right
                    // side offset by the stereo spread.
                    let base = base_ms.clamp(5.0, 30.0) * ms;
                    let sweep = depth * (0.25 * base + 2.0 * ms);
                    for c in 0..2 {
                        self.lines[c].push(input[c] + fb * 0.5 * self.fb_state[c]);
                        let mut acc = 0.0;
                        let off = if c == 1 { spread } else { 0.0 };
                        for v in 0..voices {
                            let p = ph + v as f32 / voices as f32 + off;
                            let lfo = 0.5 + 0.5 * (TAU * p).sin();
                            let d = (base * (1.0 + 0.13 * v as f32) + sweep * lfo).clamp(1.0, max_d);
                            acc += self.lines[c].read_cubic(d);
                        }
                        self.fb_state[c] = sanitize(acc * voice_norm);
                        out[c] = acc * voice_norm;
                    }
                }
                1 => {
                    // Flanger: one short sweeping tap with feedback.
                    let base = base_ms.clamp(0.1, 10.0) * ms;
                    for c in 0..2 {
                        let off = if c == 1 { spread } else { 0.0 };
                        let lfo = 0.5 + 0.5 * (TAU * (ph + off)).sin();
                        let d = (0.1 * ms + base * depth.max(0.02) * lfo + 1.0).clamp(1.0, max_d);
                        self.lines[c].push(input[c] + fb * self.fb_state[c]);
                        let y = self.lines[c].read_cubic(d);
                        self.fb_state[c] = sanitize(y.clamp(-4.0, 4.0));
                        out[c] = y;
                    }
                }
                _ => {
                    // Phaser: all-pass corner swept ±2 octaves × depth.
                    if self.counter == 0 {
                        for c in 0..2 {
                            let off = if c == 1 { spread } else { 0.0 };
                            let lfo = (TAU * (ph + off)).sin();
                            let f = (center * 2f32.powf(2.0 * depth * lfo)).clamp(20.0, nyq);
                            let w = (std::f64::consts::PI * f as f64 / sr).tan();
                            self.ap_g[c] = (w / (1.0 + w)) as f32;
                        }
                        self.counter = 4;
                    }
                    self.counter -= 1;
                    for c in 0..2 {
                        let x = input[c] + fb * self.fb_state[c];
                        let y = self.ap[c].process(x, self.ap_g[c], stages);
                        self.fb_state[c] = sanitize(y.clamp(-4.0, 4.0));
                        out[c] = y;
                    }
                }
            }
            let (w, d) = (self.wet.next(), self.dry.next());
            (sanitize(l * d + out[0] * w), sanitize(r * d + out[1] * w))
        });
    }
}

impl Processor for Chorus {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        if self.sr != sample_rate {
            self.sr = sample_rate;
            let n = (MAX_DELAY_MS * 0.001 * sample_rate as f32) as usize + 8;
            self.lines = [DelayLine::new(n), DelayLine::new(n)];
        }
        for s in [&mut self.wet, &mut self.dry, &mut self.depth] {
            s.set_time(20.0, sample_rate);
        }
        self.first = true;
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
        self.ap = [AllpassChain::default(); 2];
        self.fb_state = [0.0; 2];
        self.phase = 0.0;
        self.counter = 0;
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
        (self.sr * 0.1) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.phase as f32])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, peak, process_stereo, process_stereo_prepared, rms, set_plain, sine};
    use aitrack_dsp::Transport;

    #[test]
    fn mix_zero_is_dry() {
        for mode in 0..3 {
            let mut c = Chorus::new();
            set_plain(&mut c, p::MODE, mode as f32);
            set_plain(&mut c, p::MIX, 0.0);
            let (l, _) = process_stereo(&mut c, sine(440.0, 0.5), 4000);
            let (want, _) = sine(440.0, 0.5)(3000);
            assert!((l[3000] - want).abs() < 1e-5);
        }
    }

    #[test]
    fn phaser_notch_moves() {
        // The level of a fixed tone through a 50 % phaser varies as the
        // notches sweep past it.
        let mut c = Chorus::new();
        set_plain(&mut c, p::MODE, 2.0);
        set_plain(&mut c, p::RATE, 1.0);
        set_plain(&mut c, p::DEPTH, 100.0);
        set_plain(&mut c, p::MIX, 50.0);
        let (l, _) = process_stereo(&mut c, sine(800.0, 0.5), 96_000);
        let levels: Vec<f32> = l.chunks(2400).skip(2).map(rms).collect();
        let (lo, hi) = levels.iter().fold((f32::MAX, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
        assert!(hi > 4.0 * lo, "lo {lo} hi {hi}");
    }

    #[test]
    fn flanger_feedback_is_bounded() {
        let mut c = Chorus::new();
        set_plain(&mut c, p::MODE, 1.0);
        set_plain(&mut c, p::FEEDBACK, 95.0);
        set_plain(&mut c, p::DEPTH, 100.0);
        set_plain(&mut c, p::MIX, 100.0);
        let (l, r) = process_stereo(&mut c, |i| if i % 997 == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, 96_000);
        assert!(peak(&l) < 25.0 && peak(&r) < 25.0);
        let mut c = Chorus::new();
        set_plain(&mut c, p::MODE, 1.0);
        set_plain(&mut c, p::FEEDBACK, -95.0);
        let (l, _) = process_stereo(&mut c, sine(3000.0, 1.0), 48_000);
        assert!(l.iter().all(|v| v.is_finite()) && peak(&l) < 25.0);
    }

    #[test]
    fn chorus_widens() {
        let mut c = Chorus::new();
        set_plain(&mut c, p::MIX, 100.0);
        set_plain(&mut c, p::SPREAD, 100.0);
        let (l, r) = process_stereo(&mut c, sine(440.0, 0.5), 48_000);
        let diff: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
        assert!(rms(&diff[24_000..]) > 0.05);
    }

    #[test]
    fn sync_locks_phase() {
        let mut c = Chorus::new();
        set_plain(&mut c, p::SYNC, 1.0);
        set_plain(&mut c, p::DIVISION, 7.0); // 1/4
        c.prepare(SR, 128);
        let tr = Transport { playing: true, bpm: 120.0, position_beats: 10.25, ..Transport::default() };
        process_stereo_prepared(&mut c, sine(440.0, 0.1), 1, &tr);
        assert!((c.lfo_phase() - 0.25).abs() < 0.01, "{}", c.lfo_phase());
    }
}
