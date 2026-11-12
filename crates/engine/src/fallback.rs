//! Tiny built-in nodes for tests, demos and headless use.
//!
//! These are *not* the stock plugins (those live in `aitrack-stock`). They
//! exist so the engine can be tested end to end without any other crate:
//!
//! | plugin id      | node                                                |
//! |----------------|-----------------------------------------------------|
//! | `test.synth`   | 16-voice sine/saw synth with a linear ADSR          |
//! | `test.gain`    | gain effect (param 0: normalised 0..1 → 0..2 linear)|
//! | `test.latency` | pure delay that reports its latency (for PDC tests) |
//!
//! `test.latency` reads its delay in samples from `device.state` as a
//! little-endian `u32` (default 64).
//!
//! The graph compiler only falls back to these when the injected factory
//! returns `None` and the crate is built for its own tests or with the
//! `test-nodes` feature. [`FallbackFactory`] can also be injected directly.

use crate::factory::ProcessorFactory;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_model::{Device, DeviceKind};

pub const TEST_SYNTH: &str = "test.synth";
pub const TEST_GAIN: &str = "test.gain";
pub const TEST_LATENCY: &str = "test.latency";

/// Creates the built-in test nodes for `Stock { plugin_id: "test.*" }` devices.
pub struct FallbackFactory;

impl ProcessorFactory for FallbackFactory {
    fn create(&self, device: &Device, _sample_rate: f64, _max_frames: usize) -> Option<Box<dyn Processor>> {
        let DeviceKind::Stock { plugin_id } = &device.kind else { return None };
        match plugin_id.as_str() {
            TEST_SYNTH => Some(Box::new(TestSynth::new())),
            TEST_GAIN => Some(Box::new(TestGain::new())),
            TEST_LATENCY => {
                let latency = device
                    .state
                    .as_deref()
                    .and_then(|s| s.get(..4))
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .unwrap_or(64);
                Some(Box::new(TestLatency::new(latency)))
            }
            _ => None,
        }
    }
}

// ───────────────────────────── Synth ─────────────────────────────

const VOICES: usize = 16;

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    key: u8,
    note_id: i32,
    phase: f64,
    inc: f64,
    velocity: f32,
    /// Envelope level 0..1.
    env: f32,
    /// 0 attack, 1 decay, 2 sustain, 3 release.
    stage: u8,
    age: u64,
}

/// Polyphonic sine/saw synth with a linear ADSR.
///
/// Params: 0 waveform (0 = sine, 1 = saw), 1 attack (0..1 → 0..2 s),
/// 2 decay (0..1 → 0..2 s), 3 sustain level, 4 release (0..1 → 0..2 s),
/// 5 output level.
pub struct TestSynth {
    info: PluginInfo,
    sr: f64,
    voices: [Voice; VOICES],
    params: [f32; 6],
    clock: u64,
}

impl TestSynth {
    pub fn new() -> Self {
        Self {
            info: PluginInfo {
                id: TEST_SYNTH,
                name: "Test Synth",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["test"],
                params: vec![
                    ParamInfo::new(0, "Wave", 0.0, 1.0, 0.0).choices(&["Sine", "Saw"]),
                    ParamInfo::new(1, "Attack", 0.0, 2000.0, 0.0).unit("ms"),
                    ParamInfo::new(2, "Decay", 0.0, 2000.0, 0.0).unit("ms"),
                    ParamInfo::new(3, "Sustain", 0.0, 1.0, 1.0),
                    ParamInfo::new(4, "Release", 0.0, 2000.0, 0.0).unit("ms"),
                    ParamInfo::new(5, "Level", 0.0, 1.0, 0.5),
                ],
                inputs: 0,
                outputs: 2,
                has_sidechain: false,
            },
            sr: 48_000.0,
            voices: [Voice::default(); VOICES],
            params: [0.0, 0.0, 0.0, 1.0, 0.0, 0.5],
            clock: 0,
        }
    }

    fn rate(&self, param: usize) -> f32 {
        // Seconds → per-sample increment; 0 s means instantaneous.
        let secs = self.params[param] * 2.0;
        if secs <= 1e-5 { 1.0 } else { 1.0 / (secs * self.sr as f32) }
    }

    fn note_on(&mut self, key: u8, velocity: f32, note_id: i32) {
        self.clock += 1;
        let slot = self
            .voices
            .iter()
            .position(|v| !v.active)
            .unwrap_or_else(|| (0..VOICES).min_by_key(|&i| self.voices[i].age).unwrap_or(0));
        let freq = 440.0 * 2f64.powf((key as f64 - 69.0) / 12.0);
        self.voices[slot] = Voice {
            active: true,
            key,
            note_id,
            phase: 0.0,
            inc: freq / self.sr,
            velocity,
            env: 0.0,
            stage: 0,
            age: self.clock,
        };
    }

    fn note_off(&mut self, key: u8, note_id: i32) {
        for v in self.voices.iter_mut().filter(|v| v.active && v.key == key && v.stage < 3) {
            if note_id < 0 || v.note_id == note_id || v.note_id < 0 {
                v.stage = 3;
            }
        }
    }

    fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32], from: usize, to: usize) {
        let (a, d, r) = (self.rate(1), self.rate(2), self.rate(4));
        let sustain = self.params[3];
        let saw = self.params[0] >= 0.5;
        let level = self.params[5];
        for v in self.voices.iter_mut().filter(|v| v.active) {
            for i in from..to {
                match v.stage {
                    0 => {
                        v.env += a;
                        if v.env >= 1.0 {
                            v.env = 1.0;
                            v.stage = 1;
                        }
                    }
                    1 => {
                        v.env -= d;
                        if v.env <= sustain {
                            v.env = sustain;
                            v.stage = 2;
                        }
                    }
                    2 => v.env = sustain,
                    _ => {
                        v.env -= r;
                        if v.env <= 0.0 {
                            v.env = 0.0;
                            v.active = false;
                            break;
                        }
                    }
                }
                let osc = if saw { (2.0 * v.phase - 1.0) as f32 } else { (v.phase * std::f64::consts::TAU).sin() as f32 };
                let s = osc * v.env * v.velocity * level;
                out_l[i] += s;
                out_r[i] += s;
                v.phase += v.inc;
                if v.phase >= 1.0 {
                    v.phase -= 1.0;
                }
            }
        }
    }
}

impl Default for TestSynth {
    fn default() -> Self {
        Self::new()
    }
}

impl Processor for TestSynth {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
    }

    fn reset(&mut self) {
        self.voices = [Voice::default(); VOICES];
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.buffer.frames();
        ctx.buffer.clear();
        if ctx.buffer.channels() < 2 {
            return;
        }
        let (l, r) = ctx.buffer.stereo_mut();
        let mut cursor = 0usize;
        for ev in ctx.events {
            let at = (ev.offset as usize).min(frames);
            if at > cursor {
                self.render(l, r, cursor, at);
                cursor = at;
            }
            match ev.kind {
                EventKind::NoteOn { key, velocity, note_id, .. } => {
                    if velocity <= 0.0 {
                        self.note_off(key, note_id)
                    } else {
                        self.note_on(key, velocity, note_id)
                    }
                }
                EventKind::NoteOff { key, note_id, .. } => self.note_off(key, note_id),
                EventKind::AllNotesOff => {
                    for v in &mut self.voices {
                        v.stage = 3;
                    }
                }
                EventKind::Param { id, value } => self.set_param(id, value),
                _ => {}
            }
        }
        if cursor < frames {
            self.render(l, r, cursor, frames);
        }
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        if let Some(p) = self.params.get_mut(id as usize) {
            *p = normalized.clamp(0.0, 1.0);
        }
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id as usize).copied().unwrap_or(0.0)
    }
}

// ───────────────────────────── Gain ─────────────────────────────

/// Linear gain: param 0 normalised 0..1 → 0..2 (0.5 = unity).
pub struct TestGain {
    info: PluginInfo,
    gain: f32,
}

impl TestGain {
    pub fn new() -> Self {
        Self {
            info: PluginInfo {
                id: TEST_GAIN,
                name: "Test Gain",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["test"],
                params: vec![ParamInfo::new(0, "Gain", 0.0, 2.0, 1.0)],
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            gain: 0.5,
        }
    }
}

impl Default for TestGain {
    fn default() -> Self {
        Self::new()
    }
}

impl Processor for TestGain {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        ctx.buffer.apply_gain(self.gain * 2.0);
    }
    fn set_param(&mut self, id: u32, normalized: f32) {
        if id == 0 {
            self.gain = normalized.clamp(0.0, 1.0);
        }
    }
    fn get_param(&self, id: u32) -> f32 {
        if id == 0 { self.gain } else { 0.0 }
    }
    /// `[gain, 42]`, so tests can read display data through the engine.
    fn meter(&self, out: &mut [f32]) -> usize {
        let v = [self.gain, 42.0];
        let n = v.len().min(out.len());
        out[..n].copy_from_slice(&v[..n]);
        n
    }
}

// ───────────────────────────── Latency ─────────────────────────────

/// Delays its input by a fixed number of samples and reports it as latency.
pub struct TestLatency {
    info: PluginInfo,
    latency: u32,
    ring: Vec<[f32; 2]>,
    pos: usize,
}

impl TestLatency {
    pub fn new(latency: u32) -> Self {
        Self {
            info: PluginInfo {
                id: TEST_LATENCY,
                name: "Test Latency",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["test"],
                params: Vec::new(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            latency,
            ring: vec![[0.0; 2]; latency as usize + 1],
            pos: 0,
        }
    }
}

impl Processor for TestLatency {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {
        self.ring = vec![[0.0; 2]; self.latency as usize + 1];
        self.pos = 0;
    }
    fn reset(&mut self) {
        self.ring.fill([0.0; 2]);
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        if self.latency == 0 || ctx.buffer.channels() < 2 {
            return;
        }
        let n = self.ring.len();
        let (l, r) = ctx.buffer.stereo_mut();
        for (sl, sr) in l.iter_mut().zip(r.iter_mut()) {
            // The ring holds `latency + 1` slots: write the newest, read the
            // one written `latency` samples ago.
            self.ring[self.pos] = [*sl, *sr];
            let read = (self.pos + 1) % n;
            let [ol, or] = self.ring[read];
            *sl = ol;
            *sr = or;
            self.pos = read;
        }
    }
    fn set_param(&mut self, _: u32, _: f32) {}
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
    fn latency(&self) -> u32 {
        self.latency
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_dsp::{AudioBuffer, Event, Transport};

    #[test]
    fn latency_node_delays_exactly() {
        let mut node = TestLatency::new(5);
        node.prepare(48_000.0, 16);
        let mut buf = AudioBuffer::new(2, 16);
        buf.channel_mut(0)[0] = 1.0;
        let t = Transport::default();
        node.process(&mut ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events: &[], transport: &t });
        assert_eq!(buf.channel(0)[5], 1.0);
        assert_eq!(buf.channel(0).iter().filter(|s| **s != 0.0).count(), 1);
    }

    #[test]
    fn synth_starts_at_event_offset() {
        let mut s = TestSynth::new();
        s.prepare(48_000.0, 64);
        s.set_param(0, 1.0); // saw: non-zero right away
        let mut buf = AudioBuffer::new(2, 64);
        let ev = [Event { offset: 10, kind: EventKind::NoteOn { channel: 0, key: 60, velocity: 1.0, note_id: 1 } }];
        let t = Transport::default();
        s.process(&mut ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events: &ev, transport: &t });
        assert!(buf.channel(0)[..10].iter().all(|x| *x == 0.0));
        assert!(buf.channel(0)[10] != 0.0);
    }
}
