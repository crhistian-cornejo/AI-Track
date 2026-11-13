//! `instrument.kick808` — dedicated TR-808 kick and tuned 808 bass synthesizer.
//!
//! Combines a bridged-T inspired phase-continuous sine core, fast exponential
//! pitch sweep for punch, shaped transient click beater, asymmetric tube/diode
//! saturation for 2nd/3rd harmonics on small speakers, post-drive low-pass tone
//! control, fixed or chromatic key-tracked pitching with portamento glide, and
//! One-Shot or Gated release behaviour.
//!
//! Zero allocations, locks, or I/O on the audio thread.
//!
//! Meter layout: `[active (0/1), norm_pitch (0..1), peak, amp_env (0..1)]`.

use super::vault::kit::{flush, key_hz, soft};
use crate::common::{ParamStore, Segmented, drive, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "instrument.kick808";
const STACK: usize = 8;

pub mod p {
    pub const TUNE: u32 = 0;
    pub const SWEEP: u32 = 1;
    pub const SWEEP_DECAY: u32 = 2;
    pub const DECAY: u32 = 3;
    pub const CLICK: u32 = 4;
    pub const DRIVE: u32 = 5;
    pub const TONE: u32 = 6;
    pub const KEY_TRACK: u32 = 7;
    pub const GLIDE: u32 = 8;
    pub const HOLD: u32 = 9;
    pub const VOLUME: u32 = 10;
}

pub const KEY_TRACK_MODES: &[&str] = &["Fixed", "Chromatic"];
pub const HOLD_MODES: &[&str] = &["One-Shot", "Gate"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::TUNE, "Fundamental", 24.0, 110.0, 48.0).unit("Hz").log().short("Tune"),
        ParamInfo::new(p::SWEEP, "Sweep Depth", 0.0, 36.0, 16.0).unit("st").short("Sweep"),
        ParamInfo::new(p::SWEEP_DECAY, "Sweep Time", 4.0, 180.0, 26.0).unit("ms").log().short("Swp T"),
        ParamInfo::new(p::DECAY, "Decay", 60.0, 4_000.0, 680.0).unit("ms").log().short("Decay"),
        ParamInfo::new(p::CLICK, "Click", 0.0, 100.0, 32.0).unit("%").short("Click"),
        ParamInfo::new(p::DRIVE, "Drive", 0.0, 100.0, 22.0).unit("%").short("Drive"),
        ParamInfo::new(p::TONE, "Tone LP", 120.0, 12_000.0, 4_200.0).unit("Hz").log().short("Tone"),
        ParamInfo::new(p::KEY_TRACK, "Key Tracking", 0.0, 1.0, 0.0).choices(KEY_TRACK_MODES).short("Track"),
        ParamInfo::new(p::GLIDE, "Glide", 0.0, 250.0, 0.0).unit("ms").short("Glide"),
        ParamInfo::new(p::HOLD, "Env Mode", 0.0, 1.0, 0.0).choices(HOLD_MODES).short("Mode"),
        ParamInfo::new(p::VOLUME, "Volume", -48.0, 6.0, 0.0).unit("dB").short("Vol"),
    ]
}

pub struct Kick808 {
    info: PluginInfo,
    params: ParamStore,
    sr: f32,
    phase: f32,
    click_phase: f32,
    amp_env: f32,
    pitch_env: f32,
    click_env: f32,
    lp_state: f32,
    cur_key: f32,
    target_key: f32,
    vel_gain: f32,
    gate: bool,
    released: bool,
    bend: f32,
    stack: [u8; STACK],
    held: usize,
    last_hz: f32,
    peak: f32,
    // Cached parameters
    tune_hz: f32,
    sweep_st: f32,
    sweep_ms: f32,
    decay_ms: f32,
    click: f32,
    drive: f32,
    tone_hz: f32,
    chromatic: bool,
    glide_ms: f32,
    gate_mode: bool,
    gain: f32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Kick808::new())
}

impl Default for Kick808 {
    fn default() -> Self {
        Self::new()
    }
}

impl Kick808 {
    pub fn new() -> Self {
        let params = params();
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Kick 808",
                vendor: "AI-Track",
                category: Category::Instrument,
                tags: &["808", "kick", "sub", "bass", "drums", "trap"],
                params: params.clone(),
                inputs: 0,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            phase: 0.0,
            click_phase: 0.0,
            amp_env: 0.0,
            pitch_env: 0.0,
            click_env: 0.0,
            lp_state: 0.0,
            cur_key: 36.0,
            target_key: 36.0,
            vel_gain: 1.0,
            gate: false,
            released: false,
            bend: 0.0,
            stack: [0; STACK],
            held: 0,
            last_hz: 48.0,
            peak: 0.0,
            tune_hz: 48.0,
            sweep_st: 16.0,
            sweep_ms: 26.0,
            decay_ms: 680.0,
            click: 0.32,
            drive: 0.22,
            tone_hz: 4_200.0,
            chromatic: false,
            glide_ms: 0.0,
            gate_mode: false,
            gain: 1.0,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let pr = &self.params;
        self.tune_hz = pr.plain(p::TUNE);
        self.sweep_st = pr.plain(p::SWEEP);
        self.sweep_ms = pr.plain(p::SWEEP_DECAY);
        self.decay_ms = pr.plain(p::DECAY);
        self.click = pr.plain(p::CLICK) / 100.0;
        self.drive = pr.plain(p::DRIVE) / 100.0;
        self.tone_hz = pr.plain(p::TONE);
        self.chromatic = pr.choice(p::KEY_TRACK) == 1;
        self.glide_ms = pr.plain(p::GLIDE);
        self.gate_mode = pr.choice(p::HOLD) == 1;
        self.gain = db_to_gain(pr.plain(p::VOLUME));
    }

    fn note_on(&mut self, key: u8, vel: f32) {
        if vel <= 0.0 {
            self.note_off(key);
            return;
        }
        if let Some(i) = self.stack[..self.held].iter().position(|&k| k == key) {
            self.stack.copy_within(i + 1..self.held, i);
            self.held -= 1;
        }
        if self.held == STACK {
            self.stack.copy_within(1..STACK, 0);
            self.held -= 1;
        }
        let legato = self.held > 0 && self.chromatic && self.glide_ms > 1.0;
        self.stack[self.held] = key;
        self.held += 1;
        self.target_key = key as f32;
        if !legato {
            self.cur_key = self.target_key;
            // Reset phase slightly off zero crossing with subtle click consistency for repeatable low-end punch.
            self.phase = 0.0;
            self.click_phase = 0.0;
            self.amp_env = 1.0;
            self.pitch_env = 1.0;
            self.click_env = 1.0;
        }
        self.vel_gain = 0.35 + 0.65 * vel.clamp(0.0, 1.0);
        self.gate = true;
        self.released = false;
    }

    fn note_off(&mut self, key: u8) {
        let Some(i) = self.stack[..self.held].iter().position(|&k| k == key) else { return };
        self.stack.copy_within(i + 1..self.held, i);
        self.held -= 1;
        if self.held > 0 && self.chromatic {
            self.target_key = self.stack[self.held - 1] as f32;
        } else {
            self.gate = false;
            if self.gate_mode {
                self.released = true;
            }
        }
    }
}

impl Segmented for Kick808 {
    fn event(&mut self, kind: &EventKind) {
        if self.params.take_dirty() {
            self.update();
        }
        match *kind {
            EventKind::Param { id, value } => self.params.set(id, value),
            EventKind::NoteOn { key, velocity, .. } => self.note_on(key, velocity),
            EventKind::NoteOff { key, .. } => self.note_off(key),
            EventKind::PitchBend { value, .. } => self.bend = value.clamp(-1.0, 1.0),
            EventKind::AllNotesOff | EventKind::ControlChange { cc: 120 | 123, .. } => {
                self.held = 0;
                self.gate = false;
                self.released = true;
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

        let sweep_dec = (-1.0 / (self.sweep_ms.max(1.0) * 0.001 * self.sr)).exp();
        let body_dec = (-1.0 / (self.decay_ms.max(10.0) * 0.001 * self.sr)).exp();
        let rel_dec = (-1.0 / (0.032 * self.sr)).exp();
        let click_dec = (-1.0 / (0.0028 * self.sr)).exp();
        let glide_c = if self.glide_ms <= 0.1 {
            1.0
        } else {
            1.0 - (-1.0 / (self.glide_ms * 0.001 * self.sr)).exp()
        };
        let tone_c = 1.0 - (-std::f32::consts::TAU * self.tone_hz.clamp(80.0, self.sr * 0.45) / self.sr).exp();
        let mut peak = self.peak * (-((end - start) as f32) / (0.15 * self.sr)).exp();

        for i in start..end {
            if self.amp_env < 1e-6 && self.click_env < 1e-6 {
                self.amp_env = 0.0;
                self.pitch_env = 0.0;
                self.click_env = 0.0;
                continue;
            }

            self.cur_key += (self.target_key - self.cur_key) * glide_c;
            let base_hz = if self.chromatic {
                // Chromatic scaling relative to C1 (MIDI 24..36 centered around 36=C2 shifted by TUNE/48).
                key_hz(self.cur_key + self.bend * 2.0) * (self.tune_hz / 65.406)
            } else {
                self.tune_hz * 2.0f32.powf(self.bend * 2.0 / 12.0)
            };

            let inst_hz = (base_hz * 2.0f32.powf((self.sweep_st * self.pitch_env) / 12.0)).clamp(18.0, self.sr * 0.4);
            self.last_hz = inst_hz;
            self.phase = (self.phase + inst_hz / self.sr).fract();

            // Pure sine core + gentle 808 beater click burst.
            let body = (self.phase * std::f32::consts::TAU).sin();
            self.click_phase = (self.click_phase + 980.0 / self.sr).fract();
            let beater = ((self.click_phase * std::f32::consts::TAU).sin() * 0.7 + (self.click_phase * 2.7 * std::f32::consts::TAU).sin() * 0.3)
                * self.click_env
                * self.click;

            let raw = body * self.amp_env + beater;
            // Asymmetric warmth + soft clip creates even+odd harmonics for drive > 0.
            let driven = if self.drive > 0.005 {
                let g = 1.0 + self.drive * 4.5;
                let asym = raw * g + 0.14 * self.drive * raw * raw;
                soft(asym) / (1.0 + self.drive * 0.35)
            } else {
                raw
            };

            self.lp_state = flush(self.lp_state + tone_c * (driven - self.lp_state));
            let out = sanitize(self.lp_state * self.vel_gain * self.gain * 0.9);
            peak = peak.max(out.abs());

            buf.channel_mut(0)[i] = out;
            if buf.channels() >= 2 {
                buf.channel_mut(1)[i] = out;
            }

            let env_dec = if self.gate_mode && self.released { rel_dec } else { body_dec };
            self.amp_env = flush(self.amp_env * env_dec);
            self.pitch_env = flush(self.pitch_env * sweep_dec);
            self.click_env = flush(self.click_env * click_dec);
        }
        self.peak = peak;
    }
}

impl Processor for Kick808 {
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
        self.phase = 0.0;
        self.click_phase = 0.0;
        self.amp_env = 0.0;
        self.pitch_env = 0.0;
        self.click_env = 0.0;
        self.lp_state = 0.0;
        self.held = 0;
        self.gate = false;
        self.released = false;
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
        ((self.params.plain(p::DECAY) / 1000.0 * 1.1) as f64 * f64::from(self.sr)) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let norm_pitch = ((self.last_hz.clamp(24.0, 250.0) - 24.0) / (250.0 - 24.0)).clamp(0.0, 1.0);
        write_meter(out, 0, &[if self.amp_env > 1e-3 { 1.0 } else { 0.0 }, norm_pitch, self.peak, self.amp_env])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, render_notes, rms, set_plain};

    fn zero_crossing_hz(x: &[f32]) -> f32 {
        let c: Vec<f32> = (1..x.len()).filter(|&i| x[i - 1] <= 0.0 && x[i] > 0.0).map(|i| i as f32).collect();
        if c.len() < 2 {
            return 0.0;
        }
        (c.len() - 1) as f32 / (c[c.len() - 1] - c[0]) * SR as f32
    }

    #[test]
    fn kick_starts_high_and_settles_at_fundamental() {
        let mut k = Kick808::new();
        set_plain(&mut k, p::TUNE, 50.0);
        set_plain(&mut k, p::SWEEP, 24.0);
        set_plain(&mut k, p::SWEEP_DECAY, 20.0);
        set_plain(&mut k, p::CLICK, 0.0);
        set_plain(&mut k, p::DRIVE, 0.0);
        let (out, _) = render_notes(&mut k, &[(0, 36, 1.0, true)], 24_000);
        let early_hz = zero_crossing_hz(&out[100..2_000]);
        let late_hz = zero_crossing_hz(&out[12_000..22_000]);
        assert!(early_hz > late_hz * 1.4, "early pitch punch ({early_hz} Hz) > settled pitch ({late_hz} Hz)");
        assert!((late_hz - 50.0).abs() < 3.0, "settles near 50 Hz, got {late_hz}");
    }

    #[test]
    fn chromatic_mode_follows_midi_note_frequency() {
        let mut k = Kick808::new();
        set_plain(&mut k, p::TUNE, 65.406);
        set_plain(&mut k, p::SWEEP, 0.0);
        set_plain(&mut k, p::CLICK, 0.0);
        set_plain(&mut k, p::DRIVE, 0.0);
        set_plain(&mut k, p::KEY_TRACK, 1.0);
        let (out_c2, _) = render_notes(&mut k, &[(0, 36, 1.0, true)], 20_000);
        let hz_c2 = zero_crossing_hz(&out_c2[2_000..18_000]);
        k.reset();
        let (out_c3, _) = render_notes(&mut k, &[(0, 48, 1.0, true)], 20_000);
        let hz_c3 = zero_crossing_hz(&out_c3[2_000..18_000]);
        assert!((hz_c3 / hz_c2 - 2.0).abs() < 0.08, "octave ratio expected 2.0, got {hz_c3}/{hz_c2}");
    }

    #[test]
    fn gate_mode_stops_shortly_after_note_off_while_oneshot_rings() {
        let mut oneshot = Kick808::new();
        set_plain(&mut oneshot, p::DECAY, 2_000.0);
        let (out_os, _) = render_notes(&mut oneshot, &[(0, 36, 1.0, true), (2_000, 36, 0.0, false)], 24_000);
        let mut gated = Kick808::new();
        set_plain(&mut gated, p::DECAY, 2_000.0);
        set_plain(&mut gated, p::HOLD, 1.0);
        let (out_gt, _) = render_notes(&mut gated, &[(0, 36, 1.0, true), (2_000, 36, 0.0, false)], 24_000);
        assert!(rms(&out_os[14_000..22_000]) > 0.1, "one-shot rings out");
        assert!(rms(&out_gt[14_000..22_000]) < 1e-3, "gated mode damps after note off");
    }

    #[test]
    fn drive_saturates_cleanly_and_state_round_trips() {
        let mut a = Kick808::new();
        set_plain(&mut a, p::DRIVE, 100.0);
        set_plain(&mut a, p::TUNE, 55.0);
        let (out, _) = render_notes(&mut a, &[(0, 36, 1.0, true)], 12_000);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.5));
        let bytes = a.save_state();
        let mut b = Kick808::new();
        b.load_state(&bytes);
        let _ = render_notes(&mut b, &[], 64);
        assert!((a.get_param(p::TUNE) - b.get_param(p::TUNE)).abs() < 1e-5);
        assert!((a.get_param(p::DRIVE) - b.get_param(p::DRIVE)).abs() < 1e-5);
    }
}
