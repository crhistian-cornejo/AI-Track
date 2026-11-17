//! Built-in grain sources, so a fresh `instrument.granular` (and every
//! factory preset) sounds without a file or a live input.
//!
//! Six mono sources, synthesised once per process at 44.1 kHz with band-
//! limited oscillators (additive saw tables), formant filters and simple
//! percussion models, then run through the mip chain like any other source.
//! All are tuned to C (play C4 / key 60 for the original pitch). Nothing here
//! runs on the audio thread.

use super::source::MipBuf;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use std::f64::consts::TAU;
use std::sync::OnceLock;

pub const SR: f64 = 44_100.0;
pub const NAMES: &[&str] = &["Chord Pad", "Voice Ah", "Noise Bed", "Bell Pluck", "Sub Bass", "Drum Loop"];
pub const COUNT: usize = 6;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / 8_388_608.0) - 1.0
    }
}

/// One cycle of a saw band-limited to `fmax` when played at `f0`.
fn saw_table(f0: f64, fmax: f64) -> Vec<f32> {
    const N: usize = 2048;
    let partials = ((fmax / f0) as usize).clamp(1, 300);
    let mut t = vec![0.0f32; N + 1];
    for (i, v) in t.iter_mut().enumerate().take(N) {
        let x = i as f64 / N as f64;
        let mut s = 0.0;
        for n in 1..=partials {
            s += (TAU * n as f64 * x).sin() / n as f64;
        }
        *v = (s * 2.0 / std::f64::consts::PI) as f32;
    }
    t[N] = t[0];
    t
}

#[inline]
fn table_osc(t: &[f32], phase: f64) -> f32 {
    let x = phase.rem_euclid(1.0) * 2048.0;
    let i = x as usize;
    let f = (x - i as f64) as f32;
    t[i] + (t[i + 1] - t[i]) * f
}

fn normalise(x: &mut [f32], peak: f32) {
    let m = x.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-9);
    let g = peak / m;
    let n = x.len();
    let fade = (SR * 0.02) as usize;
    for (i, v) in x.iter_mut().enumerate() {
        let e = (i.min(n - 1 - i) as f32 / fade as f32).min(1.0);
        *v *= g * e;
    }
}

fn chord_pad() -> Vec<f32> {
    let n = (SR * 3.0) as usize;
    let notes = [65.41, 130.81, 196.0, 311.13, 466.16, 587.33];
    let amps = [0.8, 1.0, 0.7, 0.6, 0.45, 0.35];
    let detune = [-0.0075, 0.0, 0.0075];
    let mut out = vec![0.0f32; n];
    for (k, &f) in notes.iter().enumerate() {
        let tab = saw_table(f * 1.01, 6000.0);
        for (d, &dt) in detune.iter().enumerate() {
            let inc = f * (1.0 + dt) / SR;
            let lfo_hz = 0.11 + 0.07 * (k * 3 + d) as f64;
            let lfo_ph = (k * 7 + d * 3) as f64 * 0.37;
            let mut ph = (k * 11 + d * 5) as f64 * 0.173;
            for (i, o) in out.iter_mut().enumerate() {
                let am = 0.65 + 0.35 * (TAU * (lfo_hz * i as f64 / SR + lfo_ph)).sin();
                *o += table_osc(&tab, ph) * amps[k] * am as f32;
                ph += inc;
            }
        }
    }
    // Soften the top.
    let mut lp = 0.0f32;
    for v in &mut out {
        lp += (*v - lp) * 0.45;
        *v = lp;
    }
    normalise(&mut out, 0.8);
    out
}

fn voice_ah() -> Vec<f32> {
    let n = (SR * 3.0) as usize;
    let tab = saw_table(150.0, 6000.0);
    let formants = [(700.0, 9.0, 1.0f32), (1220.0, 12.0, 0.55), (2600.0, 20.0, 0.3), (3500.0, 25.0, 0.15)];
    let mut st = [SvfState::default(); 4];
    let mut out = vec![0.0f32; n];
    let mut ph = 0.0;
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f64 / SR;
        let vib = 1.0 + 0.008 * (TAU * 5.2 * t).sin() + 0.002 * rng.next() as f64;
        ph += 130.81 * vib / SR;
        let src = table_osc(&tab, ph) + 0.03 * rng.next();
        let mut y = 0.0;
        for (k, &(f, q, g)) in formants.iter().enumerate() {
            let drift = 1.0 + 0.04 * (TAU * (0.13 + 0.05 * k as f64) * t).sin();
            let c = SvfCoeffs::new(SvfMode::Bandpass, (f * drift) as f32, q, 0.0, SR);
            y += st[k].process(&c, src) * g;
        }
        *o = y;
    }
    normalise(&mut out, 0.8);
    out
}

fn noise_bed() -> Vec<f32> {
    let n = (SR * 3.0) as usize;
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    let (mut b0, mut b1, mut b2) = (0.0f32, 0.0f32, 0.0f32);
    let mut bp = SvfState::default();
    let mut out = vec![0.0f32; n];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f64 / SR;
        let w = rng.next();
        b0 = 0.99765 * b0 + w * 0.099_046;
        b1 = 0.96300 * b1 + w * 0.296_516_4;
        b2 = 0.57000 * b2 + w * 1.052_691_3;
        let pink = (b0 + b1 + b2 + w * 0.1848) * 0.3;
        let fc = 900.0 * 2f64.powf(1.6 * (TAU * 0.17 * t).sin());
        let c = SvfCoeffs::new(SvfMode::Bandpass, fc as f32, 1.6, 0.0, SR);
        let band = bp.process(&c, pink);
        let swell = 0.7 + 0.3 * (TAU * 0.23 * t + 1.0).sin() as f32;
        *o = (pink * 0.45 + band * 0.9) * swell;
    }
    normalise(&mut out, 0.8);
    out
}

fn bell_pluck() -> Vec<f32> {
    let n = (SR * 3.0) as usize;
    let seq = [523.25, 783.99, 622.25, 932.33, 783.99, 523.25];
    let ratios = [1.0, 2.0, 2.76, 5.4, 8.93];
    let amps = [1.0, 0.6, 0.4, 0.25, 0.15];
    let taus = [1.2, 0.8, 0.5, 0.3, 0.2];
    let mut out = vec![0.0f32; n];
    for (k, &f) in seq.iter().enumerate() {
        let start = (k as f64 * 0.5 * SR) as usize;
        for i in start..n.min(start + (SR * 1.6) as usize) {
            let t = (i - start) as f64 / SR;
            let mut s = 0.0;
            for p in 0..5 {
                let fp = f * ratios[p];
                if fp < 12_000.0 {
                    s += (TAU * fp * t).sin() * amps[p] * (-t / taus[p]).exp();
                }
            }
            let atk = (t / 0.002).min(1.0);
            out[i] += (s * atk) as f32;
        }
    }
    normalise(&mut out, 0.8);
    out
}

fn sub_bass() -> Vec<f32> {
    let n = (SR * 3.0) as usize;
    let mut out = vec![0.0f32; n];
    let mut ph = 0.0f64;
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f64 / SR;
        ph += 65.41 * (1.0 + 0.0015 * (TAU * 0.4 * t).sin()) / SR;
        let s = (TAU * ph).sin() as f32;
        let drive = 1.2 + 0.6 * (TAU * 0.21 * t).sin() as f32;
        *o = (s * drive).tanh() * 0.7 + 0.15 * (TAU * 2.0 * ph).sin() as f32;
    }
    normalise(&mut out, 0.85);
    out
}

fn drum_loop() -> Vec<f32> {
    let n = (SR * 2.0) as usize;
    let mut out = vec![0.0f32; n];
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    // Kick on every beat (120 bpm).
    for beat in 0..4 {
        let start = (beat as f64 * 0.5 * SR) as usize;
        let mut ph = 0.0f64;
        for i in start..n.min(start + (SR * 0.4) as usize) {
            let t = (i - start) as f64 / SR;
            ph += (45.0 + 110.0 * (-t / 0.028).exp()) / SR;
            let a = (-t / 0.16).exp() * (t / 0.0015).min(1.0);
            out[i] += ((TAU * ph).sin() * a * 1.6).tanh() as f32 * 0.9;
        }
    }
    // Off-beat hats.
    for step in 0..8 {
        let start = ((step as f64 * 0.25 + 0.125) * SR) as usize;
        let mut prev = 0.0f32;
        for i in start..n.min(start + (SR * 0.1) as usize) {
            let t = (i - start) as f64 / SR;
            let w = rng.next();
            let hp = w - prev;
            prev = w;
            out[i] += hp * 0.22 * (-t / 0.025).exp() as f32;
        }
    }
    // Clap on 2 and 4.
    for beat in [1usize, 3] {
        let start = (beat as f64 * 0.5 * SR) as usize;
        let mut bp = SvfState::default();
        let c = SvfCoeffs::new(SvfMode::Bandpass, 1500.0, 1.2, 0.0, SR);
        for i in start..n.min(start + (SR * 0.25) as usize) {
            let t = (i - start) as f64 / SR;
            let env = [0.0, 0.012, 0.024].iter().map(|&d| if t >= d { (-(t - d) / 0.02).exp() } else { 0.0 }).sum::<f64>().min(1.2)
                + 0.5 * (-t / 0.09).exp();
            out[i] += bp.process(&c, rng.next()) * env as f32 * 0.9;
        }
    }
    normalise(&mut out, 0.9);
    out
}

/// The six sources (built on first use).
pub fn sources() -> &'static [MipBuf] {
    static S: OnceLock<Vec<MipBuf>> = OnceLock::new();
    S.get_or_init(|| {
        let gens: [fn() -> Vec<f32>; COUNT] = [chord_pad, voice_ah, noise_bed, bell_pluck, sub_bass, drum_loop];
        gens.iter()
            .map(|g| {
                let x = g();
                MipBuf::from_planar(&x, None, SR)
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_sources_are_finite_audible_and_bounded() {
        let s = sources();
        assert_eq!(s.len(), NAMES.len());
        for (i, m) in s.iter().enumerate() {
            let l = &m.levels[0].ch[0];
            assert!(l.iter().all(|v| v.is_finite()), "{}", NAMES[i]);
            let rms = (l.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / l.len() as f64).sqrt();
            assert!(rms > 0.05 && rms < 0.9, "{} rms {rms}", NAMES[i]);
            assert!(l.iter().all(|v| v.abs() <= 0.95), "{}", NAMES[i]);
        }
    }
}
