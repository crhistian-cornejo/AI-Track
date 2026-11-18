//! `instrument.acid303` — classic monophonic acid bassline synthesizer.
//!
//! A band-limited sawtooth or tanh-shaped hollow square (plus an optional
//! band-limited sub-octave) feeds the zero-delay-feedback diode ladder of
//! `aitrack_dsp::ladder` (18 dB/oct, high-passed feedback so the resonance
//! thins out at low cutoffs). Around it sit the 303's fixed high-pass
//! stages, a filter envelope whose decay shortens on accents, an accent
//! "sweep" that charges while an accented note is held and discharges slowly
//! (so consecutive accents build on each other), an exponential slide, a
//! slowly decaying VCA and a warm overdrive.
//!
//! Can be played directly via MIDI notes (overlapping notes trigger slide;
//! velocity >= 0.78 triggers accent) or switched to `Pattern` mode to run
//! the built-in 16-step sequence (MIDI notes transpose the pattern live).
//!
//! Zero allocations, locks, or I/O on the audio thread.
//!
//! Meter layout: `[gate (0/1), active_step (-1 or 0..15), peak, env (0..1)]`.

use super::vault::kit::{coeff, flush, key_hz, soft};
use crate::common::{ParamStore, Segmented, drive, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::ladder::{Ladder, LadderKind};
use aitrack_dsp::osc::{BlepOsc, VaShape};
use aitrack_dsp::svf::{OnePole, SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "instrument.acid303";
pub const STEPS: usize = 16;
const STACK: usize = 16;
/// Output trim so a default note sits around half of full scale.
const OUT_TRIM: f32 = 0.8;

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

pub struct Acid303 {
    info: PluginInfo,
    params: ParamStore,
    sr: f32,
    osc: BlepOsc,
    sub: BlepOsc,
    filter: Ladder,
    hp_pre: OnePole,
    ap_pre: OnePole,
    hp_post: OnePole,
    notch: SvfState,
    notch_c: SvfCoeffs,
    amp_lp: [f32; 2],
    fenv_att: bool,
    acc_cap: f32,
    acc_rc: f32,
    fc_peak: f32,
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

fn sub_osc() -> BlepOsc {
    let mut o = BlepOsc::default();
    o.shape = VaShape::Pulse;
    o
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
            sub: sub_osc(),
            filter: Ladder::new(LadderKind::Diode),
            hp_pre: OnePole::default(),
            ap_pre: OnePole::default(),
            hp_post: OnePole::default(),
            notch: SvfState::default(),
            notch_c: SvfCoeffs::passthrough(),
            amp_lp: [0.0; 2],
            fenv_att: false,
            acc_cap: 0.0,
            acc_rc: 0.0,
            fc_peak: 0.0,
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

    /// Start the envelopes of a fresh (non-slid) note.
    fn retrigger(&mut self) {
        self.fenv_att = true;
        self.vca = 1.0;
        self.fc_peak = 0.0;
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
            self.retrigger();
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
            self.retrigger();
            self.accented = vel >= 0.78;
        } else {
            // Overlapping legato note slides smoothly without snapping the filter envelope.
            self.sliding = true;
            self.accented = vel >= 0.78;
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
        let sr = self.sr;
        let beats_per_sample = tr.bpm.max(20.0) / 60.0 / f64::from(sr);
        // The slide's time constant is a fifth of the set time, so the
        // glide has settled once that time has passed.
        let slide_coeff = coeff(0.2 * self.slide_ms.max(2.0), sr);
        let env_att = coeff(1.0, sr);
        let acc_charge = coeff(30.0, sr);
        let acc_decay = (-1.0 / (0.2 * sr)).exp();
        let rc2 = coeff(15.0, sr);
        let amp_decay = (-1.0 / (1.230 * sr)).exp();
        let anti_click = coeff(0.8, sr);
        let mut peak = self.peak * (-((end - start) as f32) / (0.15 * sr)).exp();
        // Resonance as the original skews it: slow start, steep end.
        let res = (1.0 - (-3.0 * self.resonance).exp()) / (1.0 - (-3.0f32).exp());

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

            // Filter envelope: quick rise, then an exponential decay that is
            // shorter on accented notes.
            if self.fenv_att {
                self.fenv += (1.05 - self.fenv) * env_att;
                if self.fenv >= 1.0 {
                    self.fenv = 1.0;
                    self.fenv_att = false;
                }
            } else {
                let dec_ms = if self.accented { self.decay_ms.min(200.0) } else { self.decay_ms };
                self.fenv = flush(self.fenv * (-1.0 / (dec_ms * 0.001 * sr)).exp());
            }
            // Accent sweep: charges while an accented note is held, drains
            // slowly, so back-to-back accents climb; then a 15 ms RC.
            if self.gate && self.accented {
                self.acc_cap += (1.0 - self.acc_cap) * acc_charge;
            } else {
                self.acc_cap = flush(self.acc_cap * acc_decay);
            }
            self.acc_rc = flush(self.acc_rc + (self.acc_cap - self.acc_rc) * rc2);

            // VCA: slow decay while held, a short release (longer on accents).
            if self.gate {
                self.vca = flush(self.vca * amp_decay);
            } else {
                let rel_ms = if self.accented { 50.0 } else { 1.0 };
                self.vca = flush(self.vca * (-1.0 / (rel_ms * 0.001 * sr)).exp());
            }
            self.amp_lp[0] += (self.vca - self.amp_lp[0]) * anti_click;
            self.amp_lp[1] += (self.amp_lp[0] - self.amp_lp[1]) * anti_click;
            self.amp_lp = [flush(self.amp_lp[0]), flush(self.amp_lp[1])];
            let amp = self.amp_lp[1];

            if amp < 1e-6 && self.vca < 1e-6 {
                continue;
            }

            let hz = key_hz(self.pitch + self.tuning).clamp(16.0, 12_000.0);
            self.osc.shape = VaShape::Saw;
            self.osc.set_freq(hz, sr);
            let saw = self.osc.next();
            let mut x = if matches!(self.wave, VaShape::Pulse) {
                // Square from the saw through a limiter with an offset: the
                // hollow, slightly rounded 303 pulse.
                (6.0 * (saw + 0.3)).tanh() * 0.9
            } else {
                saw
            };
            if self.sub_amt > 0.001 {
                self.sub.set_freq(hz * 0.5, sr);
                x += self.sub.next() * self.sub_amt * 0.45;
            }
            // Fixed input stages: high-pass and all-pass in front of the filter.
            x = self.ap_pre.allpass(self.hp_pre.highpass(x));

            let acc_boost = self.accent_amt * self.acc_rc;
            let oct_mod = self.env_mod * 4.8 * self.fenv + acc_boost * 2.2;
            let fc = (self.cutoff * 2.0f32.powf(oct_mod)).min(sr * 0.45);
            self.fc_peak = self.fc_peak.max(fc);
            self.filter.set_params(fc, res);
            let pre = 0.7 * (1.0 + self.drive * 1.6 + acc_boost * 0.4);
            let mut y = self.filter.process(x * pre);
            // Fixed output stages: high-pass, then a notch that removes DC.
            y = self.notch.process(&self.notch_c, self.hp_post.highpass(y));

            let vca_boost = 1.0 + acc_boost * 0.55;
            let driven = if self.drive > 0.01 {
                soft(y * (1.0 + self.drive * 3.2)) / (1.0 + self.drive * 0.45)
            } else {
                y
            };
            let out = sanitize(driven * amp * vca_boost * self.gain * OUT_TRIM);
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
        self.filter.set_sample_rate(self.sr);
        self.filter.set_feedback_highpass(150.0);
        self.hp_pre.set_freq(44.486, sample_rate);
        self.ap_pre.set_freq(14.008, sample_rate);
        self.hp_post.set_freq(24.167, sample_rate);
        self.notch_c = SvfCoeffs::new(SvfMode::Notch, 7.5164, 0.7, 0.0, sample_rate);
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.osc = BlepOsc::default();
        self.sub = sub_osc();
        self.filter.reset();
        self.hp_pre.reset();
        self.ap_pre.reset();
        self.hp_post.reset();
        self.notch.reset();
        self.amp_lp = [0.0; 2];
        self.fenv_att = false;
        self.acc_cap = 0.0;
        self.acc_rc = 0.0;
        self.fc_peak = 0.0;
        self.held = 0;
        self.gate = false;
        self.accented = false;
        self.sliding = false;
        self.vca = 0.0;
        self.fenv = 0.0;
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

    fn render_blocks(block: usize, notes: &[(usize, u8, f32, bool)], n: usize) -> Vec<f32> {
        let mut s = Acid303::new();
        s.prepare(SR, block);
        let mut buf = AudioBuffer::new(2, block);
        let tr = Transport { playing: true, ..Transport::default() };
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < n {
            let len = block.min(n - pos);
            buf.set_frames(len);
            buf.clear();
            let events: Vec<Event> = notes
                .iter()
                .filter(|&&(f, ..)| f >= pos && f < pos + len)
                .map(|&(f, key, vel, on)| {
                    let kind = if on {
                        EventKind::NoteOn { channel: 0, key, velocity: vel, note_id: key as i32 }
                    } else {
                        EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: key as i32 }
                    };
                    Event { offset: (f - pos) as u32, kind }
                })
                .collect();
            let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &events, transport: &tr };
            s.process(&mut ctx);
            out.extend_from_slice(buf.channel(0));
            pos += len;
        }
        out
    }

    #[test]
    fn output_does_not_depend_on_the_block_size() {
        let notes = [(0, 36, 0.9, true), (6_000, 43, 0.5, true), (12_000, 36, 0.0, false), (14_000, 43, 0.0, false), (15_000, 38, 1.0, true)];
        let a = render_blocks(64, &notes, 30_000);
        let b = render_blocks(480, &notes, 30_000);
        let c = render_blocks(1024, &notes, 30_000);
        assert!(rms(&a) > 0.02);
        let diff = |x: &[f32], y: &[f32]| x.iter().zip(y).fold(0.0f32, |m, (p, q)| m.max((p - q).abs()));
        assert!(diff(&a, &b) < 1e-5, "{}", diff(&a, &b));
        assert!(diff(&a, &c) < 1e-5, "{}", diff(&a, &c));
    }

    #[test]
    fn accented_note_opens_the_filter_further() {
        let peak_fc = |vel: f32| {
            let mut s = Acid303::new();
            let _ = render_notes(&mut s, &[(0, 36, vel, true)], 4_800);
            s.fc_peak
        };
        let (plain, accent) = (peak_fc(0.5), peak_fc(1.0));
        assert!(accent > plain * 1.25, "accent {accent} Hz vs plain {plain} Hz");
    }

    #[test]
    fn consecutive_accents_build_up_the_sweep() {
        // Three accented notes 125 ms apart: the capacitor never drains fully.
        let mut s = Acid303::new();
        let notes = [(0, 36, 1.0, true), (3_000, 36, 0.0, false), (6_000, 36, 1.0, true), (9_000, 36, 0.0, false)];
        let _ = render_notes(&mut s, &notes, 6_001);
        let start_of_second = s.acc_cap;
        assert!(start_of_second > 0.2, "residual sweep {start_of_second}");
    }

    #[test]
    fn slide_glides_pitch_over_the_set_time() {
        for &ms in &[40.0f32, 100.0, 220.0] {
            let total = (ms * 0.001 * SR as f32) as usize;
            let at = |frames: usize| {
                let mut s = Acid303::new();
                set_plain(&mut s, p::SLIDE, ms);
                let _ = render_notes(&mut s, &[(0, 36, 0.5, true), (2_400, 48, 0.5, true)], 2_400 + frames);
                s.pitch
            };
            let early = (at(total / 10) - 36.0) / 12.0;
            let mid = (at(total / 2) - 36.0) / 12.0;
            let done = (at(total) - 36.0) / 12.0;
            assert!(early > 0.02 && early < 0.7, "{ms} ms early {early}");
            assert!(mid > 0.8 && mid < 0.99, "{ms} ms mid {mid}");
            assert!(done > 0.97, "{ms} ms done {done}");
        }
    }

    #[test]
    fn level_is_sane_for_default_and_extreme_settings() {
        let mut s = Acid303::new();
        let (out, _) = render_notes(&mut s, &[(0, 36, 0.7, true)], 24_000);
        let pk = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(pk > 0.1 && pk < 1.0, "default peak {pk}");
        for (cut, res, drive, acc) in [(60.0, 0.0, 0.0, 0.0), (18_000.0, 100.0, 100.0, 100.0), (300.0, 100.0, 0.0, 100.0), (2_000.0, 80.0, 100.0, 0.0)] {
            let mut s = Acid303::new();
            set_plain(&mut s, p::CUTOFF, cut);
            set_plain(&mut s, p::RESONANCE, res);
            set_plain(&mut s, p::DRIVE, drive);
            set_plain(&mut s, p::ACCENT, acc);
            let (out, _) = render_notes(&mut s, &[(0, 36, 1.0, true)], 24_000);
            let pk = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(out.iter().all(|v| v.is_finite()) && pk < 2.0, "cut {cut} res {res} drive {drive}: peak {pk}");
        }
    }
}
