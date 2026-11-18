//! `mod.lfo` — LFO modulator. It passes audio through untouched; the engine
//! asks it for a control signal ([`Processor::modulate`]) and adds it, scaled
//! by each mapping's range, to the parameters it drives.
//!
//! Seven shapes, free (Hz) or tempo-synced rate, depth, offset, phase,
//! jitter, output smoothing and optional retrigger on note-on. Every value
//! derives from the song position (and the note-on that retriggered it), and
//! the random shapes hash the cycle number, so an offline export sounds like
//! live playback. While the transport is stopped the LFO free-runs.
//!
//! The output is `0.5 + 0.5·(offset + depth·wave)` clamped to 0..1.
//!
//! Meter layout: `[output (0..1), phase in the cycle (0..1)]`.

use crate::common::{ParamStore, load_state, save_state, write_meter};
use aitrack_dsp::lfo::{SYNC_DIVISIONS, division_beats};
use aitrack_dsp::{Category, EventKind, ModBlock, ParamInfo, PluginInfo, ProcessContext, Processor};

pub const ID: &str = "mod.lfo";

pub mod p {
    pub const WAVE: u32 = 0;
    pub const TIME_MODE: u32 = 1;
    pub const RATE: u32 = 2;
    pub const DIVISION: u32 = 3;
    pub const DEPTH: u32 = 4;
    pub const OFFSET: u32 = 5;
    pub const PHASE: u32 = 6;
    pub const JITTER: u32 = 7;
    pub const SMOOTH: u32 = 8;
    pub const RETRIG: u32 = 9;
}

pub const WAVES: &[&str] = &["Sine", "Triangle", "Saw Up", "Saw Down", "Square", "S&H", "Smooth"];
const TIME_MODES: &[&str] = &["Hz", "Sync"];
const OFF_ON: &[&str] = &["Off", "On"];

pub fn params() -> Vec<ParamInfo> {
    let n_div = (SYNC_DIVISIONS.len() - 1) as f32;
    vec![
        ParamInfo::new(p::WAVE, "Wave", 0.0, 6.0, 0.0).choices(WAVES),
        ParamInfo::new(p::TIME_MODE, "Time Mode", 0.0, 1.0, 1.0).choices(TIME_MODES).short("Time"),
        ParamInfo::new(p::RATE, "Rate", 0.01, 40.0, 1.0).unit("Hz").log(),
        ParamInfo::new(p::DIVISION, "Division", 0.0, n_div, 7.0).choices(SYNC_DIVISIONS).short("Div"),
        ParamInfo::new(p::DEPTH, "Depth", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OFFSET, "Offset", -100.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::PHASE, "Phase", 0.0, 360.0, 0.0).unit("°"),
        ParamInfo::new(p::JITTER, "Jitter", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::SMOOTH, "Smooth", 0.0, 1000.0, 0.0).unit("ms"),
        ParamInfo::new(p::RETRIG, "Retrigger", 0.0, 1.0, 0.0).choices(OFF_ON).short("Retrig"),
    ]
}

/// Deterministic noise in −1..1 for an integer (the random shapes and the
/// jitter hash the cycle number instead of keeping a generator).
#[inline]
fn hash(n: i64, seed: u64) -> f32 {
    let mut z = (n as u64).wrapping_add(seed).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    ((z >> 40) as f32 / (1u64 << 23) as f32) - 1.0
}

/// The bipolar (−1..1) value of shape `wave` (index into [`WAVES`]) after
/// `cycles` periods; the integer part picks the random value.
pub fn shape(wave: usize, cycles: f64) -> f32 {
    let idx = cycles.floor();
    let f = (cycles - idx) as f32;
    match wave {
        0 => (f * std::f32::consts::TAU).sin(),
        1 => {
            if f < 0.25 {
                4.0 * f
            } else if f < 0.75 {
                2.0 - 4.0 * f
            } else {
                4.0 * f - 4.0
            }
        }
        2 => 2.0 * f - 1.0,
        3 => 1.0 - 2.0 * f,
        4 => {
            if f < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        5 => hash(idx as i64, 0x51),
        _ => {
            let t = 0.5 - 0.5 * (f * std::f32::consts::PI).cos();
            let a = hash(idx as i64, 0x51);
            let b = hash(idx as i64 + 1, 0x51);
            a + (b - a) * t
        }
    }
}

pub struct Lfo {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    /// Where the cycle counts from: the song start, or the last note-on
    /// when retriggering (in the domain of `trig_playing`).
    trig_samples: i64,
    trig_beats: f64,
    trig_playing: bool,
    /// Frames the stopped transport has free-run.
    free_samples: i64,
    smoothed: f32,
    primed: bool,
    out: f32,
    phase: f32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Lfo::new())
}

impl Default for Lfo {
    fn default() -> Self {
        Self::new()
    }
}

impl Lfo {
    pub fn new() -> Self {
        let params = params();
        Self {
            info: PluginInfo {
                id: ID,
                name: "LFO",
                vendor: "AI-Track",
                category: Category::Utility,
                tags: &["modulation", "lfo", "modulator", "mapping"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            trig_samples: 0,
            trig_beats: 0.0,
            trig_playing: true,
            free_samples: 0,
            smoothed: 0.5,
            primed: false,
            out: 0.5,
            phase: 0.0,
        }
    }
}

impl Processor for Lfo {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.reset();
    }

    fn reset(&mut self) {
        self.trig_samples = 0;
        self.trig_beats = 0.0;
        self.trig_playing = true;
        self.free_samples = 0;
        self.primed = false;
    }

    fn process(&mut self, _ctx: &mut ProcessContext<'_>) {}

    fn modulate(&mut self, m: &ModBlock<'_>, out: &mut [f32]) -> bool {
        let wave = self.params.choice(p::WAVE);
        let sync = self.params.choice(p::TIME_MODE) == 1;
        let hz = self.params.plain(p::RATE) as f64;
        let div = division_beats(self.params.choice(p::DIVISION));
        let depth = self.params.plain(p::DEPTH) / 100.0;
        let offset = self.params.plain(p::OFFSET) / 100.0;
        let phase_off = self.params.plain(p::PHASE) as f64 / 360.0;
        let jitter = self.params.plain(p::JITTER) as f64 / 100.0;
        let smooth_ms = self.params.plain(p::SMOOTH);
        let retrig = self.params.on(p::RETRIG);
        let sr = m.sample_rate.max(1.0);
        let coeff = if smooth_ms > 0.5 { 1.0 - (-(m.step as f32 / sr as f32) / (smooth_ms * 0.001)).exp() } else { 1.0 };
        if m.playing != self.trig_playing {
            // The clock changed domain (play / stop): count from its start.
            self.trig_playing = m.playing;
            self.trig_samples = 0;
            self.trig_beats = 0.0;
        }
        let base = if m.playing { m.start_samples } else { self.free_samples };
        let mut ev = 0;
        let mut cycles = 0.0;
        for (k, o) in out.iter_mut().enumerate() {
            let off = k * m.step;
            let samples = base + off as i64;
            let beats = if m.playing { m.beats.get(k).copied().unwrap_or(0.0) } else { samples as f64 / sr * m.bpm / 60.0 };
            while let Some(e) = m.events.get(ev).filter(|e| (e.offset as usize) < off + m.step) {
                if retrig && matches!(e.kind, EventKind::NoteOn { .. }) {
                    self.trig_samples = samples;
                    self.trig_beats = beats;
                }
                ev += 1;
            }
            cycles = if sync { (beats - self.trig_beats) / div } else { (samples - self.trig_samples) as f64 / sr * hz };
            cycles += phase_off;
            if jitter > 0.0 {
                // A new random nudge every 5 ms, up to a quarter cycle.
                cycles += jitter * 0.25 * hash((samples as f64 / (sr * 0.005)).floor() as i64, 0x77) as f64;
            }
            let w = shape(wave, cycles);
            let v = (0.5 + 0.5 * (offset + depth * w)).clamp(0.0, 1.0);
            if !self.primed {
                self.smoothed = v;
                self.primed = true;
            }
            self.smoothed += (v - self.smoothed) * coeff;
            *o = self.smoothed;
        }
        if !m.playing {
            self.free_samples += m.frames as i64;
        }
        self.out = out.last().copied().unwrap_or(self.out);
        self.phase = (cycles - cycles.floor()) as f32;
        true
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
        write_meter(out, 0, &[self.out, self.phase])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, set_plain};
    use aitrack_dsp::Event;

    const STEP: usize = 32;

    /// `n` steps starting at song sample `start`, 120 bpm.
    fn run(p: &mut Lfo, start: i64, n: usize, playing: bool, events: &[Event]) -> Vec<f32> {
        let beats: Vec<f64> = (0..n).map(|k| (start + (k * STEP) as i64) as f64 / SR * 2.0).collect();
        let m = ModBlock { sample_rate: SR, playing, bpm: 120.0, step: STEP, frames: n * STEP, start_samples: start, beats: &beats, events };
        let mut out = vec![0.0; n];
        assert!(p.modulate(&m, &mut out));
        out
    }

    fn lfo() -> Lfo {
        let mut p = Lfo::new();
        p.prepare(SR, 128);
        p
    }

    #[test]
    fn synced_sine_completes_a_cycle_per_division() {
        let mut p = lfo();
        set_plain(&mut p, p::DIVISION, 7.0); // 1/4 = one beat = 24000 samples at 120 bpm
        let steps = 24_000 / STEP;
        let v = run(&mut p, 0, steps, true, &[]);
        assert!((v[0] - 0.5).abs() < 1e-4, "starts mid-scale");
        assert!((v[steps / 4] - 1.0).abs() < 1e-3, "peak at a quarter cycle");
        assert!(v[3 * steps / 4] < 1e-3, "trough at three quarters");
        assert!((v[steps - 1] - 0.5).abs() < 0.02, "back near the start");
    }

    #[test]
    fn output_depends_only_on_the_song_position() {
        // One long block and the same span in uneven pieces give the same values.
        let mut a = lfo();
        let mut b = lfo();
        for q in [&mut a, &mut b] {
            set_plain(q, p::WAVE, 6.0);
            set_plain(q, p::DIVISION, 13.0);
            set_plain(q, p::JITTER, 40.0);
        }
        let whole = run(&mut a, 0, 64, true, &[]);
        let mut parts = run(&mut b, 0, 24, true, &[]);
        parts.extend(run(&mut b, (24 * STEP) as i64, 40, true, &[]));
        assert_eq!(whole, parts);
        // And a fresh instance dropped at that position agrees (export from a cue point).
        let mut c = lfo();
        set_plain(&mut c, p::WAVE, 6.0);
        set_plain(&mut c, p::DIVISION, 13.0);
        set_plain(&mut c, p::JITTER, 40.0);
        assert_eq!(run(&mut c, (24 * STEP) as i64, 40, true, &[]), whole[24..]);
    }

    #[test]
    fn depth_offset_and_range() {
        let mut p = lfo();
        set_plain(&mut p, p::DEPTH, 50.0);
        set_plain(&mut p, p::OFFSET, 100.0);
        let v = run(&mut p, 0, 400, true, &[]);
        assert!(v.iter().all(|x| (0.0..=1.0).contains(x)));
        assert!(v.iter().cloned().fold(0.0, f32::max) > 0.99, "offset moves it to the top");
        set_plain(&mut p, p::DEPTH, 0.0);
        set_plain(&mut p, p::OFFSET, 0.0);
        assert!(run(&mut p, 0, 8, true, &[]).iter().all(|x| (x - 0.5).abs() < 1e-6), "no depth is a flat line");
    }

    #[test]
    fn retrigger_restarts_the_cycle_on_note_on() {
        let mut p = lfo();
        set_plain(&mut p, p::RETRIG, 1.0);
        set_plain(&mut p, p::WAVE, 2.0); // saw up starts at 0
        set_plain(&mut p, p::DIVISION, 7.0);
        let on = Event { offset: 100, kind: EventKind::NoteOn { channel: 0, key: 60, velocity: 1.0, note_id: 1 } };
        let start = 10_000;
        let v = run(&mut p, start, 16, true, &[on]);
        assert!(v[0] > 0.2 && v[2] > v[0], "running before the note");
        assert!(v[4] < 0.01, "restarts from zero after the note: {:?}", &v[..8]);
        // Without retrigger the note changes nothing.
        let mut q = lfo();
        set_plain(&mut q, p::WAVE, 2.0);
        set_plain(&mut q, p::DIVISION, 7.0);
        assert!(run(&mut q, start, 16, true, &[on])[4] > 0.2);
    }

    #[test]
    fn random_shapes_hold_or_glide_and_stay_in_range() {
        let mut p = lfo();
        set_plain(&mut p, p::WAVE, 5.0);
        set_plain(&mut p, p::DIVISION, 13.0); // 1/16: 6000 samples
        let v = run(&mut p, 0, 200, true, &[]);
        let distinct = v.windows(2).filter(|w| w[0] != w[1]).count();
        assert!((1..6).contains(&distinct), "steps, not noise: {distinct}");
        set_plain(&mut p, p::WAVE, 6.0);
        let g = run(&mut p, 0, 200, true, &[]);
        assert!(g.iter().all(|x| (0.0..=1.0).contains(x)));
        assert!(g.windows(2).filter(|w| w[0] != w[1]).count() > 100, "glides");
    }

    #[test]
    fn smoothing_slows_a_square() {
        let mut sharp = lfo();
        let mut soft = lfo();
        for q in [&mut sharp, &mut soft] {
            set_plain(q, p::WAVE, 4.0);
            set_plain(q, p::DIVISION, 7.0);
        }
        set_plain(&mut soft, p::SMOOTH, 200.0);
        // Half a cycle in, the square drops from 1 to 0: smoothed, it takes time.
        let (a, b) = (run(&mut sharp, 0, 800, true, &[]), run(&mut soft, 0, 800, true, &[]));
        let drop = 12_000 / STEP;
        assert!(a[drop + 1] < 0.01);
        assert!(b[drop + 1] > 0.8 && b[drop + 1] > b[drop + 100], "{} {}", b[drop + 1], b[drop + 100]);
    }

    #[test]
    fn free_runs_while_stopped() {
        let mut p = lfo();
        set_plain(&mut p, p::TIME_MODE, 0.0);
        set_plain(&mut p, p::RATE, 2.0);
        let a = run(&mut p, 0, 100, false, &[]);
        let b = run(&mut p, 0, 100, false, &[]);
        assert_ne!(a[0], b[0], "moves although the position stands still");
        assert!(a.iter().chain(&b).all(|x| (0.0..=1.0).contains(x)));
    }

    #[test]
    fn registry_and_state() {
        let p = crate::create(ID).unwrap();
        assert_eq!(p.info().category, Category::Utility);
        let mut q = lfo();
        set_plain(&mut q, p::DEPTH, 30.0);
        let bytes = q.save_state();
        let mut r = lfo();
        r.load_state(&bytes);
        assert!((crate::test_util::get_plain(&r, p::DEPTH) - 30.0).abs() < 1e-3);
    }
}
