//! Building blocks shared by the Vault engines: a sine table, the voice
//! pool (allocation, stealing with a short fade, retrigger), a 4-pole
//! cascade filter in two flavours (transistor ladder / OTA), a multi-tap
//! chorus-ensemble and a slow random drift source. Everything allocates in
//! constructors only.

use aitrack_dsp::delay::DelayLine;
use std::sync::OnceLock;

const SINE_LEN: usize = 2048;

fn sine_table() -> &'static [f32] {
    static T: OnceLock<Box<[f32]>> = OnceLock::new();
    T.get_or_init(|| (0..=SINE_LEN).map(|i| (i as f64 / SINE_LEN as f64 * std::f64::consts::TAU).sin() as f32).collect())
}

/// Build the sine table (call off the audio thread before the first `sin01`).
pub fn init() {
    let _ = sine_table();
}

/// `sin(2π·phase)` for any phase (table + linear interpolation, ~1e-6 error).
#[inline]
pub fn sin01(phase: f32) -> f32 {
    let t = sine_table();
    let p = phase - phase.floor();
    let x = p * SINE_LEN as f32;
    let i = (x as usize).min(SINE_LEN - 1);
    let f = x - i as f32;
    t[i] + (t[i + 1] - t[i]) * f
}

/// Zero out values that would become denormal.
#[inline]
pub fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}

/// Bounded soft clip (cheap tanh-like).
#[inline]
pub fn soft(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

/// Per-block performance state the engines read.
#[derive(Clone, Copy, Debug, Default)]
pub struct Perf {
    /// Pitch bend in semitones (already scaled by the bend range).
    pub bend: f32,
    pub mod_wheel: f32,
    pub aftertouch: f32,
    /// Max voices of the polyphonic engines.
    pub polyphony: usize,
    pub sr: f32,
}

/// One synth voice driven by a [`Pool`].
pub trait Voice {
    type Settings;
    /// Start (or retrigger, when `retrigger`) a note.
    fn start(&mut self, key: u8, vel: f32, retrigger: bool, s: &Self::Settings, perf: &Perf);
    fn release(&mut self, s: &Self::Settings);
    fn active(&self) -> bool;
    /// Control-rate update (every [`CONTROL`] samples).
    fn control(&mut self, s: &Self::Settings, perf: &Perf);
    /// One stereo sample.
    fn tick(&mut self, s: &Self::Settings) -> (f32, f32);
    /// Hard stop.
    fn kill(&mut self);
}

/// Samples between control updates.
pub const CONTROL: usize = 16;
/// Steal fade: 1 → 0 in 64 samples (≈1.3 ms).
const STEAL_STEP: f32 = 1.0 / 64.0;

#[derive(Clone, Copy, Debug, Default)]
struct Meta {
    key: u8,
    gate: bool,
    age: u64,
    /// Fading out before `pending` starts.
    fading: bool,
    fade: f32,
    pending: Option<(u8, f32)>,
    /// Samples until the next control update.
    countdown: usize,
}

/// Polyphonic voice allocation.
pub struct Pool<V: Voice> {
    pub voices: Vec<V>,
    meta: Vec<Meta>,
    clock: u64,
}

impl<V: Voice> Pool<V> {
    pub fn new(voices: Vec<V>) -> Self {
        let n = voices.len();
        Self { voices, meta: vec![Meta::default(); n], clock: 0 }
    }

    pub fn capacity(&self) -> usize {
        self.voices.len()
    }

    pub fn active(&self) -> usize {
        self.voices.iter().filter(|v| v.active()).count()
    }

    /// Keys of the sounding voices (gate on), for tests and meters.
    pub fn gated_keys(&self) -> impl Iterator<Item = u8> + '_ {
        self.meta.iter().zip(&self.voices).filter(|(m, v)| m.gate && (v.active() || m.pending.is_some())).map(|(m, _)| if let Some((k, _)) = m.pending { k } else { m.key })
    }

    pub fn note_on(&mut self, key: u8, vel: f32, s: &V::Settings, perf: &Perf) {
        self.clock += 1;
        let limit = perf.polyphony.clamp(1, self.voices.len());
        // Same key still ringing: retrigger in place (a struck tine, a re-bowed string).
        if let Some(i) = (0..self.voices.len()).find(|&i| self.voices[i].active() && self.meta[i].key == key && !self.meta[i].fading) {
            self.meta[i].gate = true;
            self.meta[i].age = self.clock;
            self.voices[i].start(key, vel, true, s, perf);
            return;
        }
        let busy = |i: usize, m: &[Meta], v: &[V]| v[i].active() || m[i].pending.is_some();
        let used = (0..self.voices.len()).filter(|&i| busy(i, &self.meta, &self.voices)).count();
        let free = if used < limit { (0..self.voices.len()).find(|&i| !busy(i, &self.meta, &self.voices)) } else { None };
        if let Some(i) = free {
            self.meta[i] = Meta { key, gate: true, age: self.clock, fading: false, fade: 1.0, pending: None, countdown: 0 };
            self.voices[i].start(key, vel, false, s, perf);
            return;
        }
        // Steal: the oldest released voice, else the oldest.
        let pick = |released: bool| (0..self.voices.len()).filter(|&i| busy(i, &self.meta, &self.voices) && (!released || !self.meta[i].gate)).min_by_key(|&i| self.meta[i].age);
        let Some(i) = pick(true).or_else(|| pick(false)) else { return };
        let m = &mut self.meta[i];
        m.gate = true;
        m.age = self.clock;
        m.key = key;
        if self.voices[i].active() {
            m.fading = true;
            m.pending = Some((key, vel));
        } else {
            m.fading = false;
            m.pending = None;
            m.fade = 1.0;
            self.voices[i].start(key, vel, false, s, perf);
        }
    }

    pub fn note_off(&mut self, key: u8, s: &V::Settings) {
        for i in 0..self.voices.len() {
            let m = &mut self.meta[i];
            if m.gate && m.key == key {
                m.gate = false;
                if m.pending.is_some() {
                    // Released before it started: drop it.
                    m.pending = None;
                } else {
                    self.voices[i].release(s);
                }
            }
        }
    }

    pub fn release_all(&mut self, s: &V::Settings) {
        for i in 0..self.voices.len() {
            self.meta[i].gate = false;
            self.meta[i].pending = None;
            self.voices[i].release(s);
        }
    }

    pub fn kill_all(&mut self) {
        for (v, m) in self.voices.iter_mut().zip(&mut self.meta) {
            v.kill();
            *m = Meta::default();
        }
    }

    /// Force a control update on every voice at the next sample.
    pub fn refresh(&mut self) {
        for m in &mut self.meta {
            m.countdown = 0;
        }
    }

    /// Add every voice into `l`/`r`.
    pub fn render(&mut self, s: &V::Settings, perf: &Perf, l: &mut [f32], r: &mut [f32]) {
        for (v, m) in self.voices.iter_mut().zip(&mut self.meta) {
            if !v.active() && m.pending.is_none() {
                continue;
            }
            for i in 0..l.len() {
                if m.fading {
                    m.fade -= STEAL_STEP;
                    if m.fade <= 0.0 {
                        m.fading = false;
                        m.fade = 1.0;
                        v.kill();
                        if let Some((key, vel)) = m.pending.take() {
                            v.start(key, vel, false, s, perf);
                            m.countdown = 0;
                        }
                    }
                }
                if !v.active() {
                    if m.pending.is_none() {
                        break;
                    }
                    continue;
                }
                if m.countdown == 0 {
                    v.control(s, perf);
                    m.countdown = CONTROL;
                }
                m.countdown -= 1;
                let (a, b) = v.tick(s);
                let g = if m.fading { m.fade } else { 1.0 };
                l[i] += a * g;
                r[i] += b * g;
            }
        }
    }
}

/// Flavour of [`Cascade4`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterModel {
    /// Transistor ladder: saturation in every stage, bass thins out as the
    /// resonance rises, self-oscillates near 100 %.
    Ladder,
    /// OTA cascade: saturation at the input and in the feedback only, with
    /// the passband gain compensated; rounder, more "polite" resonance.
    Ota,
}

/// 4-pole low-pass made of four TPT one-poles with global feedback, run at
/// 2× oversampling for a stable, in-tune resonance up to Nyquist/2.
#[derive(Clone, Copy, Debug)]
pub struct Cascade4 {
    s: [f32; 4],
    last: f32,
    g: f32,
    k: f32,
    pub model: FilterModel,
    pub drive: f32,
}

impl Default for Cascade4 {
    fn default() -> Self {
        Self { s: [0.0; 4], last: 0.0, g: 0.0, k: 0.0, model: FilterModel::Ladder, drive: 1.0 }
    }
}

impl Cascade4 {
    pub fn new(model: FilterModel) -> Self {
        Self { model, ..Default::default() }
    }

    /// `cutoff` Hz, `reso` 0..1 (≈1 self-oscillates), at sample rate `sr`.
    #[inline]
    pub fn set(&mut self, cutoff: f32, reso: f32, sr: f32) {
        let fs = 2.0 * sr;
        let fc = cutoff.clamp(10.0, 0.45 * sr);
        let wd = (std::f32::consts::PI * fc / fs).tan();
        self.g = wd / (1.0 + wd);
        // The one-sample feedback delay detunes and damps the loop a little
        // at high cutoffs; a mild boost keeps self-oscillation reachable.
        self.k = 4.0 * reso.clamp(0.0, 1.05) * (1.0 + 0.5 * self.g);
    }

    pub fn reset(&mut self) {
        self.s = [0.0; 4];
        self.last = 0.0;
    }

    #[inline]
    fn step(&mut self, x: f32) -> f32 {
        let g = self.g;
        let mut u = match self.model {
            FilterModel::Ladder => soft(self.drive * x - self.k * self.last),
            // Passband compensation: an OTA cascade keeps its bass.
            FilterModel::Ota => soft(self.drive * x - soft(self.k * self.last)),
        };
        for i in 0..4 {
            let v = (u - self.s[i]) * g;
            let y = v + self.s[i];
            self.s[i] = flush(y + v);
            u = if self.model == FilterModel::Ladder && i < 3 { soft(y) } else { y };
        }
        self.last = u;
        u
    }

    /// One input sample (two internal steps).
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let a = self.step(x);
        let b = self.step(x);
        let mut y = 0.5 * (a + b);
        if self.model == FilterModel::Ota {
            // Passband compensation: an OTA cascade keeps its bass.
            y *= 1.0 + 0.8 * self.k;
        }
        if self.drive > 1.0 { y / self.drive.sqrt() } else { y }
    }
}

/// Multi-tap modulated delay: a stereo chorus (2 taps) or a string-machine
/// ensemble (3 taps 120° apart, slow + fast modulation).
pub struct Ensemble {
    lines: [DelayLine; 2],
    phase: f32,
    phase_fast: f32,
    sr: f32,
    pub taps: usize,
    /// Centre delay and modulation depths (ms).
    pub base_ms: f32,
    pub depth_ms: f32,
    pub fast_ms: f32,
    pub rate: f32,
    pub fast_rate: f32,
    pub mix: f32,
}

impl Ensemble {
    pub fn new(sr: f64) -> Self {
        let len = (0.05 * sr) as usize + 4;
        Self { lines: [DelayLine::new(len), DelayLine::new(len)], phase: 0.0, phase_fast: 0.25, sr: sr as f32, taps: 2, base_ms: 7.0, depth_ms: 2.0, fast_ms: 0.0, rate: 0.5, fast_rate: 6.0, mix: 0.0 }
    }

    pub fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        if self.mix <= 0.0 {
            return (l, r);
        }
        self.lines[0].push(l);
        self.lines[1].push(r);
        let ms = self.sr * 0.001;
        let max = self.lines[0].max_delay() as f32 - 3.0;
        let n = self.taps.clamp(1, 3);
        let mut out = [0.0f32; 2];
        for (c, o) in out.iter_mut().enumerate() {
            for t in 0..n {
                // Right channel taps sit between the left ones: wide image.
                let off = (t as f32 + 0.5 * c as f32) / n as f32;
                let slow = sin01(self.phase + off);
                let fast = sin01(self.phase_fast + off);
                let d = ((self.base_ms + self.depth_ms * slow + self.fast_ms * fast) * ms).clamp(1.0, max);
                *o += self.lines[c].read_linear(d);
            }
            *o /= n as f32;
        }
        self.phase += self.rate / self.sr;
        self.phase -= self.phase.floor();
        self.phase_fast += self.fast_rate / self.sr;
        self.phase_fast -= self.phase_fast.floor();
        let m = self.mix.clamp(0.0, 1.0);
        let dry = 1.0 - 0.5 * m;
        (l * dry + out[0] * m, r * dry + out[1] * m)
    }
}

/// xorshift noise.
#[derive(Clone, Copy, Debug)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / 8_388_608.0 - 1.0
    }
}

/// Slow random wander in −1..1 (analog drift), advanced at control rate.
#[derive(Clone, Copy, Debug)]
pub struct Drift {
    rng: Rng,
    target: f32,
    value: f32,
    count: u32,
}

impl Drift {
    pub fn new(seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let v = rng.next();
        Self { rng, target: v, value: v, count: 0 }
    }

    /// Advance one control step.
    #[inline]
    pub fn next(&mut self) -> f32 {
        if self.count == 0 {
            self.target = self.rng.next();
            self.count = 600;
        }
        self.count -= 1;
        self.value += (self.target - self.value) * 0.002;
        self.value
    }
}

/// MIDI key → Hz.
#[inline]
pub fn key_hz(key: f32) -> f32 {
    440.0 * 2f32.powf((key - 69.0) / 12.0)
}

/// Exponential one-pole approach coefficient for a time constant in ms.
#[inline]
pub fn coeff(ms: f32, sr: f32) -> f32 {
    1.0 - (-1.0 / (ms.max(0.01) * 0.001 * sr)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_table_is_accurate() {
        init();
        for i in 0..1000 {
            let p = i as f32 / 997.0 - 0.3;
            assert!((sin01(p) - (p * std::f32::consts::TAU).sin()).abs() < 1e-5);
        }
    }

    #[test]
    fn ladder_self_oscillates_at_cutoff_and_stays_bounded() {
        let sr = 48_000.0;
        let mut f = Cascade4::default();
        f.set(1000.0, 1.0, sr);
        let mut out = Vec::new();
        let mut x = 0.1;
        for _ in 0..48_000 {
            let y = f.process(x);
            x = 0.0;
            out.push(y);
        }
        let tail = &out[24_000..];
        let peak = tail.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.05 && peak < 3.0, "peak {peak}");
        let crossings = tail.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let hz = crossings as f32 / 0.5;
        assert!((hz - 1000.0).abs() < 60.0, "self-oscillation at {hz} Hz");
    }

    #[test]
    fn ota_keeps_passband_gain() {
        let sr = 48_000.0;
        let mut f = Cascade4::new(FilterModel::Ota);
        f.set(5000.0, 0.5, sr);
        let mut acc = 0.0;
        for i in 0..48_000 {
            let x = 0.1 * (i as f32 * std::f32::consts::TAU * 100.0 / sr).sin();
            let y = f.process(x);
            if i > 24_000 {
                acc += y * y;
            }
        }
        let rms = (acc / 24_000.0).sqrt();
        assert!(rms > 0.058, "OTA thinned the bass: rms {rms}");
    }
}
