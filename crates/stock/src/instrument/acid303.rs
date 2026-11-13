//! `instrument.acid303` — classic monophonic acid bassline synthesizer.
//!
//! Combines a band-limited sawtooth / hollow-square oscillator (`BlepOsc`)
//! and optional sub-octave with a 2x-oversampled 18 dB/oct non-linear
//! diode-ladder low-pass filter, exponential decay filter envelope, signature
//! accent punch, exponential portamento slide, warm overdrive, and an optional
//! 16-step transport-synced pattern sequencer.
//!
//! Can be played directly via MIDI notes (overlapping notes trigger slide;
//! velocity >= 0.78 triggers accent) or switched to `Pattern` mode to run
//! the built-in 16-step sequence (MIDI notes transpose the pattern live).
//!
//! Zero allocations, locks, or I/O on the audio thread.
//!
//! Meter layout: `[gate (0/1), active_step (-1 or 0..15), peak, env (0..1)]`.

use super::vault::kit::{flush, key_hz, soft};
use crate::common::{ParamStore, Segmented, drive, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::osc::{BlepOsc, VaShape};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "instrument.acid303";
pub const STEPS: usize = 16;
const STACK: usize = 16;

pub mod p {
    pub const WAVE: u32 = 0;
    pub const TUNING: u32 = 1;
    pub const CUTOFF: u32 = 2;
    pub const RESONANCE: u32 = 3;
    pub const ENV_MOD: u32 = 4;
    pub const DECAY: u32 = 5;
    pub const ACCENT: u32 = 6;
    pub const SLIDE: u32 = 7;
    pub const DRIVE: u32 = 8;
    pub const SUB: u32 = 9;
    pub const SEQ_MODE: u32 = 10;
    pub const VOLUME: u32 = 11;

    pub const STEP0: u32 = 32;
    pub const STEP_STRIDE: u32 = 4;
    pub const NOTE: u32 = 0;
    pub const GATE: u32 = 1;
    pub const S_ACCENT: u32 = 2;
    pub const S_SLIDE: u32 = 3;

    pub const fn step(index: usize, field: u32) -> u32 {
        STEP0 + index as u32 * STEP_STRIDE + field
    }
}

pub const WAVES: &[&str] = &["Saw", "Square"];
pub const SEQ_MODES: &[&str] = &["MIDI", "Pattern"];
const OFF_ON: &[&str] = &["Off", "On"];

// Default 16-step acid groove: (semitone offset from C2, gate, accent, slide).
const DEFAULT_PATTERN: [(f32, f32, f32, f32); STEPS] = [
    (0.0, 1.0, 1.0, 0.0),
    (0.0, 0.0, 0.0, 0.0),
    (12.0, 1.0, 0.0, 1.0),
    (0.0, 1.0, 0.0, 0.0),
    (3.0, 1.0, 1.0, 0.0),
    (0.0, 0.0, 0.0, 0.0),
    (7.0, 1.0, 0.0, 1.0),
    (10.0, 1.0, 0.0, 0.0),
    (0.0, 1.0, 1.0, 0.0),
    (12.0, 1.0, 0.0, 0.0),
    (15.0, 1.0, 1.0, 1.0),
    (12.0, 1.0, 0.0, 0.0),
    (7.0, 1.0, 0.0, 0.0),
    (3.0, 0.0, 0.0, 0.0),
    (5.0, 1.0, 1.0, 1.0),
    (0.0, 1.0, 0.0, 0.0),
];

const STEP_NAMES: [[&str; 4]; STEPS] = {
    const fn row(n: &'static str, g: &'static str, a: &'static str, s: &'static str) -> [&'static str; 4] {
        [n, g, a, s]
    }
    macro_rules! st {
        ($n:literal) => {
            row(
                concat!("Step ", $n, " Note"),
                concat!("Step ", $n, " Gate"),
                concat!("Step ", $n, " Accent"),
                concat!("Step ", $n, " Slide"),
            )
        };
    }
    [
        st!(1), st!(2), st!(3), st!(4), st!(5), st!(6), st!(7), st!(8),
        st!(9), st!(10), st!(11), st!(12), st!(13), st!(14), st!(15), st!(16),
    ]
};

pub fn params() -> Vec<ParamInfo> {
    let mut v = vec![
        ParamInfo::new(p::WAVE, "Waveform", 0.0, 1.0, 0.0).choices(WAVES).short("Wave"),
        ParamInfo::new(p::TUNING, "Tuning", -12.0, 12.0, 0.0).unit("st").short("Tune"),
        ParamInfo::new(p::CUTOFF, "Cutoff", 30.0, 18_000.0, 480.0).unit("Hz").log().short("Cut"),
        ParamInfo::new(p::RESONANCE, "Resonance", 0.0, 100.0, 62.0).unit("%").short("Res"),
        ParamInfo::new(p::ENV_MOD, "Env Mod", 0.0, 100.0, 58.0).unit("%").short("Env"),
        ParamInfo::new(p::DECAY, "Decay", 30.0, 2_500.0, 260.0).unit("ms").log().short("Dec"),
        ParamInfo::new(p::ACCENT, "Accent", 0.0, 100.0, 65.0).unit("%").short("Acc"),
        ParamInfo::new(p::SLIDE, "Slide Time", 10.0, 250.0, 65.0).unit("ms").log().short("Slide"),
        ParamInfo::new(p::DRIVE, "Overdrive", 0.0, 100.0, 25.0).unit("%").short("Drive"),
        ParamInfo::new(p::SUB, "Sub Octave", 0.0, 100.0, 0.0).unit("%").short("Sub"),
        ParamInfo::new(p::SEQ_MODE, "Play Mode", 0.0, 1.0, 0.0).choices(SEQ_MODES).short("Mode"),
        ParamInfo::new(p::VOLUME, "Volume", -48.0, 6.0, -3.0).unit("dB").short("Vol"),
    ];
    for i in 0..STEPS {
        let (note, gate, acc, slide) = DEFAULT_PATTERN[i];
        let base = p::STEP0 + i as u32 * p::STEP_STRIDE;
        let names = STEP_NAMES[i];
        v.extend([
            ParamInfo::new(base + p::NOTE, names[0], -12.0, 24.0, note).choices(&[]).unit("st"),
            ParamInfo::new(base + p::GATE, names[1], 0.0, 1.0, gate).choices(OFF_ON),
            ParamInfo::new(base + p::S_ACCENT, names[2], 0.0, 1.0, acc).choices(OFF_ON),
            ParamInfo::new(base + p::S_SLIDE, names[3], 0.0, 1.0, slide).choices(OFF_ON),
        ]);
    }
    v
}

/// 18 dB/oct (3-pole) non-linear diode-ladder filter, stepped at 2x oversample
/// to preserve high-frequency stability and acid squelch up to full resonance.
#[derive(Clone, Copy, Debug, Default)]
struct Diode18 {
    s1: f32,
    s2: f32,
    s3: f32,
}

impl Diode18 {
    fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.s3 = 0.0;
    }

    #[inline]
    fn step_half(&mut self, x: f32, g: f32, k: f32, sat: f32) -> f32 {
        // 3-pole cascade with diode feedback saturation and bass compensation.
        let fb = k * self.s3;
        let u = soft((x - fb) * sat);
        self.s1 = flush(self.s1 + g * (u - self.s1));
        self.s2 = flush(self.s2 + g * (soft(self.s1) - self.s2));
        self.s3 = flush(self.s3 + g * (self.s2 - self.s3));
        self.s3
    }

    #[inline]
    fn process(&mut self, x: f32, cutoff_hz: f32, res: f32, sr: f32, sat: f32) -> f32 {
        let sr2 = (sr * 2.0).max(16_000.0);
        let fc = cutoff_hz.clamp(25.0, sr2 * 0.22);
        let g = (std::f32::consts::PI * fc / sr2).tan().clamp(0.0005, 0.52);
        // 3-pole self-oscillation sits near k = 8.0 in this cascade; cap safely below runaway.
        let k = res.clamp(0.0, 1.0).powf(1.25) * 6.4;
        let _ = self.step_half(x, g, k, sat);
        let y = self.step_half(x, g, k, sat);
        // Slight make-up so high resonance doesn't lose too much body.
        y * (1.0 + k * 0.14)
    }
}

pub struct Acid303 {
    info: PluginInfo,
    params: ParamStore,
    sr: f32,
    osc: BlepOsc,
    sub_phase: f32,
    filter: Diode18,
    // Monophonic note stack for MIDI mode (and live pattern transpose).
    stack: [u8; STACK],
    vels: [f32; STACK],
    held: usize,
    pitch: f32,
    target_pitch: f32,
    gate: bool,
    accented: bool,
    sliding: bool,
    vca: f32,
    fenv: f32,
    accent_env: f32,
    last_seq_step: isize,
    transpose_base: f32,
    peak: f32,
    // Cached parameters
    wave: VaShape,
    tuning: f32,
    cutoff: f32,
    resonance: f32,
    env_mod: f32,
    decay_ms: f32,
    accent_amt: f32,
    slide_ms: f32,
    drive: f32,
    sub_amt: f32,
    seq_mode: bool,
    gain: f32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Acid303::new())
}

impl Default for Acid303 {
    fn default() -> Self {
        Self::new()
    }
}

impl Acid303 {
    pub fn new() -> Self {
        let params = params();
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Acid 303",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["acid", "303", "bass", "mono", "sequencer", "analog"],
                params: params.clone(),
                inputs: 0,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            osc: BlepOsc::default(),
            sub_phase: 0.0,
            filter: Diode18::default(),
            stack: [0; STACK],
            vels: [0.0; STACK],
            held: 0,
            pitch: 36.0,
            target_pitch: 36.0,
            gate: false,
            accented: false,
            sliding: false,
            vca: 0.0,
            fenv: 0.0,
            accent_env: 0.0,
            last_seq_step: -1,
            transpose_base: 0.0,
            peak: 0.0,
            wave: VaShape::Saw,
            tuning: 0.0,
            cutoff: 480.0,
            resonance: 0.62,
            env_mod: 0.58,
            decay_ms: 260.0,
            accent_amt: 0.65,
            slide_ms: 65.0,
            drive: 0.25,
            sub_amt: 0.0,
            seq_mode: false,
            gain: 1.0,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let pr = &self.params;
        self.wave = if pr.choice(p::WAVE) == 0 { VaShape::Saw } else { VaShape::Pulse };
        self.tuning = pr.plain(p::TUNING);
        self.cutoff = pr.plain(p::CUTOFF);
        self.resonance = pr.plain(p::RESONANCE) / 100.0;
        self.env_mod = pr.plain(p::ENV_MOD) / 100.0;
        self.decay_ms = pr.plain(p::DECAY);
        self.accent_amt = pr.plain(p::ACCENT) / 100.0;
        self.slide_ms = pr.plain(p::SLIDE);
        self.drive = pr.plain(p::DRIVE) / 100.0;
        self.sub_amt = pr.plain(p::SUB) / 100.0;
        self.seq_mode = pr.choice(p::SEQ_MODE) == 1;
        self.gain = db_to_gain(pr.plain(p::VOLUME));
    }

    fn trigger_step(&mut self, note: f32, gate: bool, accented: bool, slide: bool) {
        if !gate {
            self.gate = false;
            self.sliding = false;
            return;
        }
        let was_sliding = self.sliding && self.gate;
        self.target_pitch = note;
        if !was_sliding {
            if !self.gate {
                self.pitch = note;
            }
            self.fenv = 1.0;
            self.vca = 1.0;
            if accented {
                self.accent_env = (self.accent_env * 0.4 + 1.0).min(1.4);
            }
        } else if accented {
            self.accent_env = (self.accent_env + 0.7).min(1.4);
        }
        self.gate = true;
        self.accented = accented;
        self.sliding = slide;
    }

    fn midi_note_on(&mut self, key: u8, vel: f32) {
        if vel <= 0.0 {
            self.midi_note_off(key);
            return;
        }
        if let Some(i) = self.stack[..self.held].iter().position(|&k| k == key) {
            self.stack.copy_within(i + 1..self.held, i);
            self.vels.copy_within(i + 1..self.held, i);
            self.held -= 1;
        }
        if self.held == STACK {
            self.stack.copy_within(1..STACK, 0);
            self.vels.copy_within(1..STACK, 0);
            self.held -= 1;
        }
        let was_held = self.held > 0;
        self.stack[self.held] = key;
        self.vels[self.held] = vel;
        self.held += 1;
        if self.seq_mode {
            // In Pattern mode, held MIDI keys transpose the pattern relative to C2 (36).
            self.transpose_base = key as f32 - 36.0;
            return;
        }
        self.target_pitch = key as f32;
        if !was_held {
            self.pitch = self.target_pitch;
            self.fenv = 1.0;
            self.vca = 1.0;
            self.accented = vel >= 0.78;
            if self.accented {
                self.accent_env = 1.0;
            }
        } else {
            // Overlapping legato note slides smoothly without snapping the filter envelope.
            self.sliding = true;
            if vel >= 0.78 {
                self.accented = true;
                self.accent_env = (self.accent_env + 0.6).min(1.3);
            }
        }
        self.gate = true;
    }

    fn midi_note_off(&mut self, key: u8) {
        let Some(i) = self.stack[..self.held].iter().position(|&k| k == key) else { return };
        self.stack.copy_within(i + 1..self.held, i);
        self.vels.copy_within(i + 1..self.held, i);
        self.held -= 1;
        if self.seq_mode {
            self.transpose_base = if self.held > 0 { self.stack[self.held - 1] as f32 - 36.0 } else { 0.0 };
            return;
        }
        if self.held > 0 {
            self.target_pitch = self.stack[self.held - 1] as f32;
            self.sliding = true;
        } else {
            self.gate = false;
            self.sliding = false;
        }
    }
}

impl Segmented for Acid303 {
    fn event(&mut self, kind: &EventKind) {
        if self.params.take_dirty() {
            self.update();
        }
        match *kind {
            EventKind::Param { id, value } => self.params.set(id, value),
            EventKind::NoteOn { key, velocity, .. } => self.midi_note_on(key, velocity),
            EventKind::NoteOff { key, .. } => self.midi_note_off(key),
            EventKind::AllNotesOff | EventKind::ControlChange { cc: 120 | 123, .. } => {
                self.held = 0;
                self.gate = false;
                self.sliding = false;
            }
            _ => {}
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let buf = &mut *ctx.buffer;
        for ch in 0..buf.channels() {
            buf.channel_mut(ch)[start..end].fill(0.0);
        }

        let tr = *ctx.transport;
        let beats_per_sample = tr.bpm.max(20.0) / 60.0 / f64::from(self.sr);
        let slide_coeff = 1.0 - (-1.0 / (self.slide_ms.max(4.0) * 0.001 * self.sr)).exp();
        let atk_coeff = 1.0 - (-1.0 / (0.0025 * self.sr)).exp();
        let rel_coeff = (-1.0 / (0.008 * self.sr)).exp();
        let acc_decay = (-1.0 / (0.090 * self.sr)).exp();
        let mut peak = self.peak * (-((end - start) as f32) / (0.15 * self.sr)).exp();

        for i in start..end {
            if self.seq_mode {
                if tr.playing {
                    let step_pos = tr.position_beats + (i - start) as f64 * beats_per_sample;
                    let step_idx = (step_pos * 4.0).floor().rem_euclid(STEPS as f64) as usize;
                    let frac = (step_pos * 4.0).rem_euclid(1.0);
                    if step_idx as isize != self.last_seq_step {
                        self.last_seq_step = step_idx as isize;
                        let semi = self.params.plain(p::step(step_idx, p::NOTE)).round();
                        let gate = self.params.choice(p::step(step_idx, p::GATE)) == 1;
                        let acc = self.params.choice(p::step(step_idx, p::S_ACCENT)) == 1;
                        let slide = self.params.choice(p::step(step_idx, p::S_SLIDE)) == 1;
                        let note = 36.0 + self.transpose_base + semi;
                        self.trigger_step(note, gate, acc, slide);
                    } else if self.gate && !self.sliding && frac > 0.56 {
                        // Classic 303 ~55% gate length unless sliding into the next step.
                        self.gate = false;
                    }
                } else if self.last_seq_step >= 0 {
                    self.last_seq_step = -1;
                    self.gate = false;
                    self.sliding = false;
                }
            }

            // Portamento slide on pitch.
            self.pitch += (self.target_pitch - self.pitch) * slide_coeff;

            // VCA gate + release, filter exponential decay.
            let dec_ms = if self.accented { self.decay_ms * 0.58 + 30.0 } else { self.decay_ms };
            let f_dec = (-1.0 / (dec_ms * 0.001 * self.sr)).exp();
            self.fenv = flush(self.fenv * f_dec);
            self.accent_env = flush(self.accent_env * acc_decay);

            if self.gate {
                self.vca += (1.0 - self.vca) * atk_coeff;
            } else {
                self.vca = flush(self.vca * rel_coeff);
            }

            if self.vca < 1e-6 {
                continue;
            }

            let hz = key_hz(self.pitch + self.tuning).clamp(16.0, 12_000.0);
            self.osc.shape = self.wave;
            self.osc.set_freq(hz, self.sr);
            let mut osc = self.osc.next();
            if matches!(self.wave, VaShape::Pulse) {
                // Hollow the square slightly as the 303 pulse waveform does.
                osc = (osc * 0.85 + 0.12).clamp(-1.0, 1.0);
            }
            if self.sub_amt > 0.001 {
                self.sub_phase = (self.sub_phase + (hz * 0.5) / self.sr).fract();
                let sub = if self.sub_phase < 0.5 { 1.0 } else { -1.0 };
                osc += sub * self.sub_amt * 0.45;
            }

            let acc_boost = if self.accented { self.accent_amt } else { 0.0 };
            let oct_mod = (self.env_mod * 4.8 * self.fenv) + (acc_boost * 2.2 * self.accent_env);
            let fc = self.cutoff * 2.0f32.powf(oct_mod);
            let sat = 1.0 + self.drive * 2.4 + acc_boost * 0.8;
            let filtered = self.filter.process(osc * 0.7, fc, self.resonance, self.sr, sat);

            let vca_boost = 1.0 + acc_boost * 0.55 * self.accent_env;
            let driven = if self.drive > 0.01 {
                soft(filtered * (1.0 + self.drive * 3.2)) / (1.0 + self.drive * 0.45)
            } else {
                filtered
            };
            let out = sanitize(driven * self.vca * vca_boost * self.gain * 0.8);
            peak = peak.max(out.abs());

            buf.channel_mut(0)[i] = out;
            if buf.channels() >= 2 {
                buf.channel_mut(1)[i] = out;
            }
        }
        self.peak = peak;
    }
}

impl Processor for Acid303 {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate as f32;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.osc = BlepOsc::default();
        self.sub_phase = 0.0;
        self.filter.reset();
        self.held = 0;
        self.gate = false;
        self.accented = false;
        self.sliding = false;
        self.vca = 0.0;
        self.fenv = 0.0;
        self.accent_env = 0.0;
        self.last_seq_step = -1;
        self.transpose_base = 0.0;
        self.peak = 0.0;
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
        (0.05 * f64::from(self.sr)) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[if self.gate { 1.0 } else { 0.0 }, self.last_seq_step as f32, self.peak, self.fenv])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, render_notes, rms, set_plain};
    use aitrack_dsp::{AudioBuffer, Event, Transport};

    #[test]
    fn midi_note_produces_acid_bass_and_stops_on_release() {
        let mut s = Acid303::new();
        let (out, _) = render_notes(&mut s, &[(0, 36, 0.7, true), (24_000, 36, 0.0, false)], 32_000);
        assert!(rms(&out[..20_000]) > 0.05, "synth makes sound while held");
        assert!(rms(&out[28_000..]) < 1e-4, "silence after release");
    }

    #[test]
    fn accent_increases_peak_and_filter_brightness() {
        let mut quiet = Acid303::new();
        let mut loud = Acid303::new();
        let (out_q, _) = render_notes(&mut quiet, &[(0, 36, 0.5, true), (12_000, 36, 0.0, false)], 14_000);
        let (out_l, _) = render_notes(&mut loud, &[(0, 36, 1.0, true), (12_000, 36, 0.0, false)], 14_000);
        let peak_q = out_q.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let peak_l = out_l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak_l > peak_q * 1.1, "accent must boost output peak ({peak_l} vs {peak_q})");
    }

    #[test]
    fn high_resonance_and_drive_remain_finite_and_bounded() {
        let mut s = Acid303::new();
        set_plain(&mut s, p::CUTOFF, 1_200.0);
        set_plain(&mut s, p::RESONANCE, 100.0);
        set_plain(&mut s, p::DRIVE, 100.0);
        let (out, _) = render_notes(&mut s, &[(0, 40, 1.0, true)], 24_000);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 2.5));
        assert!(rms(&out) > 0.05);
    }

    #[test]
    fn internal_pattern_sequencer_plays_with_transport() {
        let mut s = Acid303::new();
        set_plain(&mut s, p::SEQ_MODE, 1.0);
        let mut buf = AudioBuffer::new(2, 24_000);
        let tr = Transport { playing: true, bpm: 120.0, position_beats: 0.0, ..Transport::default() };
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &[] as &[Event], transport: &tr };
        s.process(&mut ctx);
        assert!(rms(buf.channel(0)) > 0.04, "pattern mode plays sequence without external MIDI");
        let mut meter = [0.0; 4];
        assert_eq!(s.meter(&mut meter), 4);
        assert!(meter[1] >= 0.0, "step index advanced: {}", meter[1]);
    }

    #[test]
    fn state_round_trips_parameters() {
        let mut a = Acid303::new();
        set_plain(&mut a, p::CUTOFF, 2_200.0);
        set_plain(&mut a, p::RESONANCE, 85.0);
        set_plain(&mut a, p::step(4, p::NOTE), 7.0);
        let bytes = a.save_state();
        let mut b = Acid303::new();
        b.load_state(&bytes);
        let _ = render_notes(&mut b, &[], 64);
        assert!((a.get_param(p::CUTOFF) - b.get_param(p::CUTOFF)).abs() < 1e-5);
        assert!((a.get_param(p::step(4, p::NOTE)) - b.get_param(p::step(4, p::NOTE))).abs() < 1e-5);
    }
}
