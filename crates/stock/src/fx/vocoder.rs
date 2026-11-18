//! `fx.vocoder` — filter-bank channel vocoder with 4..40 bands.
//!
//! The input (the *modulator*, usually a voice) goes through a bank of
//! band-pass filters spaced evenly in log frequency; an envelope follower per
//! band measures how much of the signal lives there. The *carrier* goes
//! through an identical bank and each carrier band is scaled by the matching
//! envelope, so the carrier takes on the modulator's spectral shape:
//! intelligible speech from a synth. The carrier is the sidechain input, or
//! one of the internal sources: white noise, a saw chord ("buzz") or a saw
//! that follows the pitch of the modulator. With "Sidechain" selected and
//! nothing connected the saw is used, so the device always sounds.
//!
//! * **Formant** moves the synthesis bands against the analysis bands: the
//!   voice sounds smaller or larger without changing the carrier pitch.
//! * **Unvoiced** adds high-passed noise scaled by the modulator's
//!   sibilance (energy above 5 kHz) so "s", "t" and "f" stay intelligible
//!   when the carrier is a pitched buzz.
//! * **Band range** sets the lowest and highest centre frequency.
//!
//! Zero latency. Meter layout:
//! `[bands, tracked f0 Hz (0 = none), voiced (0/1), sibilance 0..1, then one 0..1 level per band]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, key_to_hz, load_state, sanitize, save_state, sidechain_frame, write_meter};
use aitrack_dsp::biquad::{Biquad32, BiquadCoeffs, BiquadShape, BiquadState};
use aitrack_dsp::osc::{BlepOsc, Noise};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::{AudioBuffer, Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "fx.vocoder";

pub mod p {
    pub const CARRIER: u32 = 0;
    pub const BANDS: u32 = 1;
    pub const LOW: u32 = 2;
    pub const HIGH: u32 = 3;
    pub const FORMANT: u32 = 4;
    pub const BANDWIDTH: u32 = 5;
    pub const ATTACK: u32 = 6;
    pub const RELEASE: u32 = 7;
    pub const UNVOICED: u32 = 8;
    pub const PITCH: u32 = 9;
    pub const CHORD: u32 = 10;
    pub const DETUNE: u32 = 11;
    pub const TRANSPOSE: u32 = 12;
    pub const MIX: u32 = 13;
    pub const OUTPUT: u32 = 14;
}

pub const MAX_BANDS: usize = 40;
const CARRIERS: &[&str] = &["Sidechain", "Noise", "Saw", "Tracked"];
const CHORDS: &[&str] = &["Single", "Octave", "Fifth", "Minor", "Major", "Minor 7th"];
/// Semitone offsets of each chord (at most four voices).
const CHORD_NOTES: [&[f32]; 6] = [&[0.0], &[0.0, 12.0], &[0.0, 7.0, 12.0], &[0.0, 3.0, 7.0], &[0.0, 4.0, 7.0], &[0.0, 3.0, 7.0, 10.0]];
const MAX_VOICES: usize = 4;
/// Scale from (carrier band × mean-rectified envelope) to output level,
/// found with the level test below.
const MAKEUP: f32 = 6.0;
/// Corner above which the modulator counts as sibilance.
const SIBILANCE_HZ: f64 = 5000.0;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::CARRIER, "Carrier", 0.0, 3.0, 2.0).choices(CARRIERS),
        ParamInfo::new(p::BANDS, "Bands", 4.0, MAX_BANDS as f32, 16.0).choices(&[]),
        ParamInfo::new(p::LOW, "Low", 60.0, 500.0, 120.0).unit("Hz").log(),
        ParamInfo::new(p::HIGH, "High", 2000.0, 16_000.0, 8000.0).unit("Hz").log(),
        ParamInfo::new(p::FORMANT, "Formant", -12.0, 12.0, 0.0).unit("st"),
        ParamInfo::new(p::BANDWIDTH, "Bandwidth", 50.0, 200.0, 100.0).unit("%").short("BW"),
        ParamInfo::new(p::ATTACK, "Attack", 0.5, 50.0, 3.0).unit("ms").log(),
        ParamInfo::new(p::RELEASE, "Release", 5.0, 500.0, 40.0).unit("ms").log(),
        ParamInfo::new(p::UNVOICED, "Unvoiced", 0.0, 100.0, 50.0).unit("%").short("Noise"),
        ParamInfo::new(p::PITCH, "Pitch", 24.0, 84.0, 48.0).choices(&[]).unit("key"),
        ParamInfo::new(p::CHORD, "Chord", 0.0, 5.0, 0.0).choices(CHORDS),
        ParamInfo::new(p::DETUNE, "Detune", 0.0, 50.0, 8.0).unit("ct"),
        ParamInfo::new(p::TRANSPOSE, "Transpose", -24.0, 24.0, 0.0).choices(&[]).unit("st").short("Trans"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
    ]
}

/// Monophonic pitch follower for the modulator: low-pass and decimate to
/// ~12 kHz, then every ~5 ms a normalised autocorrelation over 21 ms, taking
/// the first strong peak (which avoids octave-down errors).
struct PitchTracker {
    hist: Vec<f32>,
    nsdf: Vec<f32>,
    w: usize,
    lp: [f32; 2],
    lp_a: f32,
    dec: usize,
    phase: usize,
    since: usize,
    fs: f32,
    lag_min: usize,
    lag_max: usize,
    f0: f32,
    voiced: bool,
}

const HIST: usize = 512;
const WINDOW: usize = 256;
const HOP: usize = 64;

impl PitchTracker {
    fn new() -> Self {
        Self { hist: vec![0.0; HIST], nsdf: vec![0.0; 256], w: 0, lp: [0.0; 2], lp_a: 0.1, dec: 4, phase: 0, since: 0, fs: 12_000.0, lag_min: 24, lag_max: 192, f0: 0.0, voiced: false }
    }

    fn prepare(&mut self, sr: f64) {
        self.dec = ((sr / 12_000.0).round() as usize).max(1);
        self.fs = (sr / self.dec as f64) as f32;
        self.lp_a = 1.0 - (-std::f64::consts::TAU * 900.0 / sr).exp() as f32;
        self.lag_min = ((self.fs / 500.0) as usize).max(4);
        self.lag_max = ((self.fs / 62.0) as usize).min(HIST - WINDOW - 2).min(self.nsdf.len() - 1);
        self.reset();
    }

    fn reset(&mut self) {
        self.hist.fill(0.0);
        self.nsdf.fill(0.0);
        self.lp = [0.0; 2];
        self.phase = 0;
        self.since = 0;
        self.f0 = 0.0;
        self.voiced = false;
    }

    #[inline]
    fn push(&mut self, x: f32) {
        self.lp[0] += self.lp_a * (x - self.lp[0]);
        self.lp[1] += self.lp_a * (self.lp[0] - self.lp[1]);
        self.phase += 1;
        if self.phase < self.dec {
            return;
        }
        self.phase = 0;
        self.w = (self.w + 1) % HIST;
        self.hist[self.w] = self.lp[1];
        self.since += 1;
        if self.since >= HOP {
            self.since = 0;
            self.analyse();
        }
    }

    #[inline]
    fn at(&self, back: usize) -> f32 {
        self.hist[(self.w + HIST - back) % HIST]
    }

    fn analyse(&mut self) {
        // B = newest WINDOW samples, A = the same window `lag` earlier.
        let mut e_b = 0.0f32;
        for n in 0..WINDOW {
            let v = self.at(n);
            e_b += v * v;
        }
        if e_b / (WINDOW as f32) < 1e-6 {
            self.voiced = false;
            return;
        }
        let mut max_v = 0.0f32;
        let mut e_a = 0.0f32;
        for n in 0..WINDOW {
            let v = self.at(n + self.lag_min - 1);
            e_a += v * v;
        }
        for lag in self.lag_min..=self.lag_max {
            // Slide A one sample further back.
            let add = self.at(lag + WINDOW - 1);
            let drop = self.at(lag - 1);
            e_a += add * add - drop * drop;
            let mut r = 0.0f32;
            for n in 0..WINDOW {
                r += self.at(n) * self.at(n + lag);
            }
            let v = r / (e_a.max(0.0) * e_b).sqrt().max(1e-9);
            self.nsdf[lag] = v;
            max_v = max_v.max(v);
        }
        if max_v < 0.7 {
            self.voiced = false;
            return;
        }
        let mut pick = 0;
        for lag in self.lag_min + 1..self.lag_max {
            let (a, b, c) = (self.nsdf[lag - 1], self.nsdf[lag], self.nsdf[lag + 1]);
            if b >= 0.9 * max_v && b >= a && b > c {
                pick = lag;
                break;
            }
        }
        if pick == 0 {
            self.voiced = false;
            return;
        }
        let (a, b, c) = (self.nsdf[pick - 1], self.nsdf[pick], self.nsdf[pick + 1]);
        let denom = a - 2.0 * b + c;
        let shift = if denom.abs() > 1e-9 { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) } else { 0.0 };
        self.f0 = self.fs / (pick as f32 + shift);
        self.voiced = true;
    }
}

pub struct Vocoder {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    bands: usize,
    coeffs: [Biquad32; MAX_BANDS],
    analysis: [[BiquadState; 2]; MAX_BANDS],
    synth: [[[BiquadState; 2]; MAX_BANDS]; 2],
    env: [f32; MAX_BANDS],
    gains: [f32; MAX_BANDS],
    /// Lower analysis band and weight for each synthesis band (`-1` = none).
    map_lo: [i32; MAX_BANDS],
    map_w: [f32; MAX_BANDS],
    att: f32,
    rel: f32,
    hp_coeff: Biquad32,
    hp_mod: [BiquadState; 2],
    hp_noise: [[BiquadState; 2]; 2],
    sib: f32,
    noise: [Noise; 2],
    osc: [BlepOsc; MAX_VOICES],
    mult: [f32; MAX_VOICES],
    voices: usize,
    tracker: PitchTracker,
    pitch_hz: Smoother,
    wet: Smoother,
    dry: Smoother,
    uv: Smoother,
    first: bool,
    meter_env: [f32; MAX_BANDS],
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Vocoder::new())
}

impl Default for Vocoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Vocoder {
    pub fn new() -> Self {
        let params = params();
        let mut v = Self {
            info: PluginInfo {
                id: ID,
                name: "Vocoder",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["vocoder", "voice", "robot", "talkbox", "sidechain", "formant", "pitch"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: true,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            bands: 16,
            coeffs: [Biquad32::default(); MAX_BANDS],
            analysis: [[BiquadState::default(); 2]; MAX_BANDS],
            synth: [[[BiquadState::default(); 2]; MAX_BANDS]; 2],
            env: [0.0; MAX_BANDS],
            gains: [0.0; MAX_BANDS],
            map_lo: [-1; MAX_BANDS],
            map_w: [0.0; MAX_BANDS],
            att: 0.1,
            rel: 0.01,
            hp_coeff: Biquad32::default(),
            hp_mod: [BiquadState::default(); 2],
            hp_noise: [[BiquadState::default(); 2]; 2],
            sib: 0.0,
            noise: [Noise::new(11), Noise::new(29)],
            osc: [BlepOsc::default(); MAX_VOICES],
            mult: [1.0; MAX_VOICES],
            voices: 1,
            tracker: PitchTracker::new(),
            pitch_hz: Smoother::new(130.0),
            wet: Smoother::new(1.0),
            dry: Smoother::new(0.0),
            uv: Smoother::new(0.5),
            first: true,
            meter_env: [0.0; MAX_BANDS],
        };
        v.prepare(48_000.0, 512);
        v
    }

    /// Band centres, filter coefficients, envelope times and the formant map.
    fn update(&mut self) {
        let n = (self.params.plain(p::BANDS).round() as usize).clamp(4, MAX_BANDS);
        self.bands = n;
        let lo = self.params.plain(p::LOW) as f64;
        let hi = (self.params.plain(p::HIGH) as f64).max(lo * 2.0).min(self.sr * 0.45);
        let ratio = (hi / lo).powf(1.0 / (n - 1) as f64);
        let q = 0.8 / (ratio.sqrt() - 1.0 / ratio.sqrt()) / (self.params.plain(p::BANDWIDTH) as f64 / 100.0);
        for k in 0..n {
            let f = lo * ratio.powi(k as i32);
            self.coeffs[k] = BiquadCoeffs::rbj(BiquadShape::Bandpass, f, q, 0.0, self.sr).into();
        }
        let sr = self.sr;
        self.att = 1.0 - (-1.0 / (self.params.plain(p::ATTACK) as f64 * 0.001 * sr)).exp() as f32;
        self.rel = 1.0 - (-1.0 / (self.params.plain(p::RELEASE) as f64 * 0.001 * sr)).exp() as f32;
        // Synthesis band k listens to analysis band k − shift (in bands).
        let per_octave = (n - 1) as f32 / (hi / lo).log2() as f32;
        let shift = self.params.plain(p::FORMANT) / 12.0 * per_octave;
        for k in 0..n {
            let pos = k as f32 - shift;
            let lo_i = pos.floor();
            if lo_i < -1.0 || lo_i >= n as f32 {
                self.map_lo[k] = -2;
                self.map_w[k] = 0.0;
            } else {
                self.map_lo[k] = lo_i as i32;
                self.map_w[k] = pos - lo_i;
            }
        }
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry.set_target((1.0 - mix) * out);
        self.uv.set_target(self.params.plain(p::UNVOICED) / 100.0);
        let chord = CHORD_NOTES[self.params.choice(p::CHORD).min(CHORD_NOTES.len() - 1)];
        self.voices = chord.len();
        let detune = self.params.plain(p::DETUNE) / 100.0;
        for v in 0..MAX_VOICES {
            let note = chord.get(v).copied().unwrap_or(0.0);
            let spread = if v % 2 == 0 { detune } else { -detune };
            self.mult[v] = ((note + spread) / 12.0).exp2();
        }
        if self.first {
            for s in [&mut self.wet, &mut self.dry, &mut self.uv] {
                s.snap_to_target();
            }
            self.first = false;
        }
    }

    /// Fundamental the tracker currently follows, 0 when it has none yet.
    pub fn tracked_hz(&self) -> f32 {
        if self.tracker.voiced { self.tracker.f0 } else { 0.0 }
    }

    pub fn band_count(&self) -> usize {
        self.bands
    }
}

impl Segmented for Vocoder {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let sc: Option<&AudioBuffer> = ctx.sidechain;
        let mut mode = self.params.choice(p::CARRIER);
        if mode == 0 && sc.is_none() {
            mode = 2;
        }
        let stereo_carrier = mode == 0;
        let n = self.bands;
        let base_hz = key_to_hz(self.params.plain(p::PITCH).round());
        let transpose = (self.params.plain(p::TRANSPOSE).round() / 12.0).exp2();
        let sr32 = self.sr as f32;
        let voice_norm = 1.0 / (self.voices as f32).sqrt();
        for_frames(ctx.buffer, start, end, |i, l, r| {
            let m = 0.5 * (l + r);
            if mode == 3 {
                self.tracker.push(m);
            }
            // Analysis: envelope of every band.
            for k in 0..n {
                let c = &self.coeffs[k];
                let a = self.analysis[k][0].process(c, m);
                let x = self.analysis[k][1].process(c, a).abs();
                let e = &mut self.env[k];
                *e += (x - *e) * if x > *e { self.att } else { self.rel };
            }
            // Sibilance: energy of the modulator above 5 kHz.
            let h = self.hp_mod[0].process(&self.hp_coeff, m);
            let hf = self.hp_mod[1].process(&self.hp_coeff, h).abs();
            self.sib += (hf - self.sib) * if hf > self.sib { self.att } else { self.rel };
            // Envelope seen by each synthesis band (formant shift).
            for k in 0..n {
                let lo = self.map_lo[k];
                let at = |j: i32| if j >= 0 && (j as usize) < n { self.env[j as usize] } else { 0.0 };
                self.gains[k] = if lo == -2 { 0.0 } else { at(lo) * (1.0 - self.map_w[k]) + at(lo + 1) * self.map_w[k] };
            }
            // The carrier.
            let car: [f32; 2] = match mode {
                0 => {
                    let (a, b) = sidechain_frame(sc, i);
                    [a, b]
                }
                1 => [self.noise[0].next() * 5.0, self.noise[1].next() * 5.0],
                _ => {
                    let f = if mode == 3 {
                        if self.tracker.voiced {
                            self.pitch_hz.set_target(self.tracker.f0 * transpose);
                        } else if self.tracker.f0 <= 0.0 {
                            self.pitch_hz.set_target(base_hz);
                        }
                        self.pitch_hz.next()
                    } else {
                        base_hz
                    };
                    let mut s = 0.0;
                    for v in 0..self.voices {
                        self.osc[v].set_freq(f * self.mult[v], sr32);
                        s += self.osc[v].next();
                    }
                    s *= voice_norm * 0.52;
                    [s, s]
                }
            };
            // Synthesis.
            let channels = if stereo_carrier { 2 } else { 1 };
            let mut acc = [0.0f32; 2];
            for ch in 0..channels {
                let mut sum = 0.0;
                for k in 0..n {
                    let c = &self.coeffs[k];
                    let s0 = self.synth[ch][k][0].process(c, car[ch]);
                    let y = self.synth[ch][k][1].process(c, s0);
                    sum += y * self.gains[k];
                }
                acc[ch] = sum * MAKEUP;
            }
            if !stereo_carrier {
                acc[1] = acc[0];
            }
            // Unvoiced: noise above the sibilance corner, scaled by it.
            let uv = self.uv.next();
            let mut out = [0.0f32; 2];
            for ch in 0..2 {
                let nz = self.noise[ch].next();
                let h0 = self.hp_noise[ch][0].process(&self.hp_coeff, nz);
                let hp = self.hp_noise[ch][1].process(&self.hp_coeff, h0);
                out[ch] = sanitize(acc[ch] + hp * self.sib * uv * 6.0);
            }
            let (w, d) = (self.wet.next(), self.dry.next());
            (sanitize(l * d + out[0] * w).clamp(-8.0, 8.0), sanitize(r * d + out[1] * w).clamp(-8.0, 8.0))
        });
        for k in 0..n {
            self.meter_env[k] = self.env[k];
        }
    }
}

impl Processor for Vocoder {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.hp_coeff = BiquadCoeffs::rbj(BiquadShape::Highpass, SIBILANCE_HZ.min(sample_rate * 0.4), 0.707, 0.0, sample_rate).into();
        self.tracker.prepare(sample_rate);
        for s in [&mut self.wet, &mut self.dry, &mut self.uv] {
            s.set_time(20.0, sample_rate);
        }
        self.pitch_hz.set_time(15.0, sample_rate);
        self.first = true;
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        self.analysis = [[BiquadState::default(); 2]; MAX_BANDS];
        self.synth = [[[BiquadState::default(); 2]; MAX_BANDS]; 2];
        self.env = [0.0; MAX_BANDS];
        self.gains = [0.0; MAX_BANDS];
        self.meter_env = [0.0; MAX_BANDS];
        self.hp_mod = [BiquadState::default(); 2];
        self.hp_noise = [[BiquadState::default(); 2]; 2];
        self.sib = 0.0;
        self.tracker.reset();
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
        (self.sr * (self.params.plain(p::RELEASE) as f64 * 0.001 * 4.0 + 0.05)) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let head = [self.bands as f32, self.tracked_hz(), if self.tracker.voiced { 1.0 } else { 0.0 }, (self.sib * 4.0).min(1.0)];
        let mut at = write_meter(out, 0, &head);
        let mut levels = [0.0f32; MAX_BANDS];
        for k in 0..self.bands {
            // −60..0 dB onto 0..1 (the mean-rectified envelope sits 4 dB under the peak).
            levels[k] = ((20.0 * self.meter_env[k].max(1e-6).log10() + 56.0) / 56.0).clamp(0.0, 1.0);
        }
        at = write_meter(out, at, &levels[..self.bands]);
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{BLOCK, SR, peak, rms, set_plain};
    use aitrack_dsp::Transport;

    /// Run `n` frames; `modulator(i)` feeds the main input, `carrier(i)` the sidechain when given.
    fn run(v: &mut Vocoder, mut modulator: impl FnMut(usize) -> f32, mut carrier: Option<&mut dyn FnMut(usize) -> f32>, n: usize) -> Vec<f32> {
        v.prepare(SR, BLOCK);
        let mut buf = AudioBuffer::new(2, BLOCK);
        let mut sc = AudioBuffer::new(2, BLOCK);
        let tr = Transport::default();
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < n {
            let len = BLOCK.min(n - pos);
            buf.set_frames(len);
            sc.set_frames(len);
            for i in 0..len {
                let x = modulator(pos + i);
                buf.channel_mut(0)[i] = x;
                buf.channel_mut(1)[i] = x;
                if let Some(c) = carrier.as_mut() {
                    let y = c(pos + i);
                    sc.channel_mut(0)[i] = y;
                    sc.channel_mut(1)[i] = y;
                }
            }
            let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: carrier.as_ref().map(|_| &sc), events: &[], transport: &tr };
            v.process(&mut ctx);
            out.extend_from_slice(buf.channel(0));
            pos += len;
        }
        out
    }

    fn voice_like(i: usize) -> f32 {
        // A 120 Hz pulse train through a crude vowel-ish resonance, 0.3 peak.
        let t = i as f32 / SR as f32;
        0.3 * ((std::f32::consts::TAU * 120.0 * t).sin() + 0.5 * (std::f32::consts::TAU * 600.0 * t).sin() + 0.4 * (std::f32::consts::TAU * 1200.0 * t).sin()) / 1.9
    }

    #[test]
    fn output_follows_the_modulator_envelope() {
        // Modulator: noise gated on/off every 250 ms; carrier: a buzz.
        let mut v = Vocoder::new();
        set_plain(&mut v, p::CARRIER, 0.0);
        let mut seed = Noise::new(3);
        let mut buzz = BlepOsc::default();
        buzz.set_freq(110.0, SR as f32);
        let mut c = move |_| buzz.next() * 0.5;
        let out = run(&mut v, move |i| if (i / 12_000) % 2 == 0 { seed.next() * 0.3 } else { 0.0 }, Some(&mut c), 96_000);
        let on = rms(&out[4_000..11_000]);
        let off = rms(&out[20_000..23_000]);
        assert!(on > 0.02, "on {on}");
        assert!(off < 0.02 * on, "off {off} on {on}");
    }

    #[test]
    fn band_energy_follows_the_modulator_spectrum() {
        // A 1 kHz modulator against a noise carrier: the output's energy sits near 1 kHz.
        let mut v = Vocoder::new();
        set_plain(&mut v, p::CARRIER, 1.0);
        set_plain(&mut v, p::UNVOICED, 0.0);
        set_plain(&mut v, p::BANDS, 24.0);
        let out = run(&mut v, |i| 0.3 * (std::f32::consts::TAU * 1000.0 * i as f32 / SR as f32).sin(), None, 48_000);
        let tail = &out[12_000..];
        let band = |lo: f32, hi: f32| -> f32 {
            let mut e = 0.0;
            let mut f = lo;
            while f < hi {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for (i, &x) in tail.iter().enumerate() {
                    let a = std::f64::consts::TAU * f as f64 * i as f64 / SR;
                    re += x as f64 * a.cos();
                    im += x as f64 * a.sin();
                }
                e += (re * re + im * im) as f32;
                f += 100.0;
            }
            e
        };
        assert!(band(800.0, 1300.0) > 20.0 * band(3000.0, 3500.0), "{} vs {}", band(800.0, 1300.0), band(3000.0, 3500.0));
    }

    #[test]
    fn formant_shift_moves_the_spectral_shape() {
        let centroid = |formant: f32| {
            let mut v = Vocoder::new();
            set_plain(&mut v, p::CARRIER, 1.0);
            set_plain(&mut v, p::UNVOICED, 0.0);
            set_plain(&mut v, p::BANDS, 24.0);
            set_plain(&mut v, p::FORMANT, formant);
            let out = run(&mut v, |i| 0.3 * (std::f32::consts::TAU * 800.0 * i as f32 / SR as f32).sin(), None, 36_000);
            // Crude centroid from the zero-crossing rate of the steady part.
            let tail = &out[12_000..];
            let crossings = tail.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
            crossings as f32 / (tail.len() as f32 / SR as f32)
        };
        assert!(centroid(12.0) > 1.3 * centroid(0.0), "up {} flat {}", centroid(12.0), centroid(0.0));
        assert!(centroid(-12.0) < 0.8 * centroid(0.0));
    }

    #[test]
    fn level_is_in_the_neighbourhood_of_the_input() {
        for mode in [1.0, 2.0] {
            let mut v = Vocoder::new();
            set_plain(&mut v, p::CARRIER, mode);
            let out = run(&mut v, voice_like, None, 48_000);
            let (i, o) = (rms(&(0..48_000).map(voice_like).collect::<Vec<_>>()[12_000..]), rms(&out[12_000..]));
            let db = 20.0 * (o / i).log10();
            println!("carrier {mode}: in {i:.4} out {o:.4} ({db:+.1} dB)");
            assert!(db.abs() < 12.0, "carrier {mode}: {db:+.1} dB");
        }
    }

    #[test]
    fn tracker_finds_the_pitch_of_a_voice() {
        for f0 in [110.0f32, 196.0, 330.0] {
            let mut v = Vocoder::new();
            set_plain(&mut v, p::CARRIER, 3.0);
            let mut osc = BlepOsc::default();
            osc.set_freq(f0, SR as f32);
            run(&mut v, move |_| osc.next() * 0.3, None, 24_000);
            let got = v.tracked_hz();
            assert!((got / f0 - 1.0).abs() < 0.02, "{f0}: tracked {got}");
        }
    }

    #[test]
    fn tracked_carrier_follows_the_voice_pitch() {
        let mut v = Vocoder::new();
        set_plain(&mut v, p::CARRIER, 3.0);
        set_plain(&mut v, p::UNVOICED, 0.0);
        let mut osc = BlepOsc::default();
        osc.set_freq(150.0, SR as f32);
        let out = run(&mut v, move |_| osc.next() * 0.3, None, 48_000);
        let tail = &out[24_000..];
        let crossings = tail.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() as f32;
        // The vocoded saw is 150 Hz with a spectrum shaped by the same saw: its fundamental stays at 150 Hz.
        let est = crossings / (tail.len() as f32 / SR as f32);
        assert!(peak(tail) > 0.01);
        assert!(est > 100.0 && est < 600.0, "est {est}");
    }

    #[test]
    fn sidechain_selected_without_a_sidechain_uses_the_saw() {
        let mut v = Vocoder::new();
        set_plain(&mut v, p::CARRIER, 0.0);
        let out = run(&mut v, voice_like, None, 24_000);
        assert!(rms(&out[8_000..]) > 0.01);
    }

    #[test]
    fn meter_fits_and_band_count_follows() {
        let mut v = Vocoder::new();
        set_plain(&mut v, p::BANDS, 40.0);
        run(&mut v, voice_like, None, 6_000);
        let mut m = [0.0f32; 320];
        let n = v.meter(&mut m);
        assert_eq!(n, 4 + 40);
        assert_eq!(m[0], 40.0);
        assert!(m[4..44].iter().all(|x| (0.0..=1.0).contains(x)));
    }
}
