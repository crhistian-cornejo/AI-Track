//! What a sound sounds like, in a handful of numbers: the visual derives a
//! cell's colour and shape from these (`docs/bloom.md`). Measured on a short
//! offline render of the genome, off the audio and UI threads.

use super::genome::{Genome, g};
use super::render::render_mono_shaped;
use super::voice::Macros;
use realfft::RealFftPlanner;

/// Rate and length of the analysis render.
pub const RATE: f32 = 32_000.0;
pub const FRAMES: usize = 6_400; // 200 ms
const N_FFT: usize = 2_048;

/// A sound's timbre.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Timbre {
    /// Energy-weighted spectral centroid, Hz.
    pub centroid: f32,
    /// 0 (pure tones) .. 1 (white noise): spectral flatness.
    pub noisiness: f32,
    /// 0..1: RMS over the peak across the 200 ms (a click is near 0, a
    /// sustained tone near 0.7).
    pub energy: f32,
    /// Time to the peak, ms.
    pub attack_ms: f32,
    /// From the peak to 20 dB under it, ms (200 = still ringing).
    pub decay_ms: f32,
    /// 0 (harmonic partials) .. 1 (partials off any harmonic series).
    pub inharmonic: f32,
    /// 0..1: how much the FM moves the sound.
    pub wobble: f32,
}

/// Analyse `genome` (renders 200 ms at [`RATE`]).
pub fn analyze(genome: &Genome) -> Timbre {
    analyze_shaped(genome, 0.0, &Macros::default())
}

/// The timbre of `genome` as the device plays it: `transpose` semitones (its
/// Tune) and its macro knobs, so turning a knob changes what is measured.
pub fn analyze_shaped(genome: &Genome, transpose: f32, macros: &Macros) -> Timbre {
    let x = render_mono_shaped(genome, RATE, FRAMES, 1.0, transpose, macros);
    let mut t = analyze_signal(&x, RATE);
    let gn = genome.folded();
    let fm = (gn.plain(g::FM_INDEX) / 6.0).min(1.0) * (gn.plain(g::FM_DECAY) / 250.0).min(1.0);
    let ratio = gn.plain(g::FM_RATIO);
    let off_grid = (ratio - ratio.round()).abs() * 2.0;
    t.wobble = (fm * (0.5 + 0.5 * off_grid)).clamp(0.0, 1.0);
    t
}

/// The measures of a mono signal (no FM wobble: that comes from the genome).
pub fn analyze_signal(x: &[f32], sr: f32) -> Timbre {
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    if x.len() < 64 || peak < 1e-7 || !peak.is_finite() {
        return Timbre::default();
    }
    let rms = (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();

    // Envelope in 1 ms steps.
    let hop = (sr * 0.001).max(1.0) as usize;
    let env: Vec<f32> = x.chunks(hop).map(|c| c.iter().fold(0f32, |m, v| m.max(v.abs()))).collect();
    let peak_at = env.iter().enumerate().fold((0, 0f32), |b, (i, v)| if *v > b.1 { (i, *v) } else { b }).0;
    let decay_at = env[peak_at..].iter().position(|v| *v < peak * 0.1).unwrap_or(env.len() - peak_at);

    // Spectrum: frames over the sounding part, weighted by their energy.
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let mut input = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let bins = N_FFT / 2 + 1;
    let mut avg = vec![0f32; bins];
    let mut start = 0;
    while start < x.len() {
        let mut e = 0.0;
        for (i, v) in input.iter_mut().enumerate() {
            let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N_FFT as f32).cos();
            let s = x.get(start + i).copied().unwrap_or(0.0);
            e += s * s;
            *v = s * w;
        }
        if e > 1e-10 && fft.process(&mut input, &mut spec).is_ok() {
            for (a, c) in avg.iter_mut().zip(&spec) {
                *a += c.norm_sqr();
            }
        }
        start += N_FFT / 2;
    }
    let hz = |k: usize| k as f32 * sr / N_FFT as f32;
    let total: f32 = avg[1..].iter().sum::<f32>().max(1e-20);
    let centroid = (1..bins).map(|k| hz(k) * avg[k]).sum::<f32>() / total;
    // Flatness of the magnitude spectrum over 60 Hz .. 0.45 sr.
    let (lo, hi) = (((60.0 / sr) * N_FFT as f32) as usize, ((0.45 * N_FFT as f32) as usize).min(bins - 1));
    let mags: Vec<f32> = avg[lo..hi].iter().map(|p| p.sqrt() + 1e-12).collect();
    let geo = (mags.iter().map(|m| m.ln()).sum::<f32>() / mags.len() as f32).exp();
    let arith = mags.iter().sum::<f32>() / mags.len() as f32;
    let flatness = (geo / arith.max(1e-12)).clamp(0.0, 1.0);

    // Partials: local maxima at least 1/30 of the strongest.
    let max = avg[lo..hi].iter().fold(0f32, |m, v| m.max(*v));
    let mut peaks: Vec<(f32, f32)> = (lo.max(1)..hi - 1).filter(|&k| avg[k] > avg[k - 1] && avg[k] >= avg[k + 1] && avg[k] > max / 30.0).map(|k| (hz(k), avg[k])).collect();
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    peaks.truncate(10);
    let inharmonic = if peaks.len() < 2 {
        0.0
    } else {
        let f0 = peaks.iter().filter(|p| p.1 > max * 0.2).map(|p| p.0).fold(f32::MAX, f32::min).max(20.0);
        let (mut on, mut all) = (0.0, 0.0);
        for (f, a) in &peaks {
            let n = (f / f0).round().max(1.0);
            let off = (f / (f0 * n) - 1.0).abs();
            all += a;
            if off < 0.03 {
                on += a;
            }
        }
        (1.0 - on / all.max(1e-20)).clamp(0.0, 1.0)
    };
    Timbre {
        centroid: if centroid.is_finite() { centroid } else { 0.0 },
        // Flatness of real noise sits near 0.5..0.8, of tones under 0.1.
        noisiness: (flatness * 1.6).clamp(0.0, 1.0),
        energy: (rms / peak).clamp(0.0, 1.0),
        attack_ms: peak_at as f32,
        decay_ms: decay_at as f32,
        inharmonic: inharmonic * (1.0 - (flatness * 1.6).clamp(0.0, 1.0) * 0.7),
        wobble: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::bloom::genome::Role;
    use crate::instrument::bloom::grow::{GrowOpts, generate};

    #[test]
    fn hats_are_brighter_and_noisier_than_kicks() {
        let s = generate(4, GrowOpts::default());
        let kick = analyze(&s.sounds[0]);
        let hat = analyze(&s.sounds[6]);
        assert_eq!(s.sounds[6].role, Role::HatClosed);
        assert!(hat.centroid > kick.centroid * 3.0, "{} vs {}", hat.centroid, kick.centroid);
        assert!(hat.noisiness > kick.noisiness, "{} vs {}", hat.noisiness, kick.noisiness);
        assert!(hat.decay_ms < 200.0);
    }

    #[test]
    fn the_knobs_change_what_is_measured() {
        let s = generate(4, GrowOpts::default());
        let hat = &s.sounds[6];
        let plain = analyze(hat);
        assert_eq!(analyze_shaped(hat, 0.0, &Macros::default()), plain);
        let dark = analyze_shaped(hat, 0.0, &Macros { tone: -1.0, ..Macros::default() });
        let bright = analyze_shaped(hat, 0.0, &Macros { tone: 1.0, ..Macros::default() });
        assert!(bright.centroid > dark.centroid, "{} vs {}", bright.centroid, dark.centroid);
        let kick = &s.sounds[0];
        let up = analyze_shaped(kick, 12.0, &Macros::default());
        assert!(up.centroid > analyze(kick).centroid, "{} vs {}", up.centroid, analyze(kick).centroid);
    }

    #[test]
    fn a_harmonic_tone_is_harmonic_and_a_bar_is_not() {
        let tone: Vec<f32> = (0..FRAMES).map(|i| {
            let t = i as f32 / RATE;
            (std::f32::consts::TAU * 220.0 * t).sin() + 0.5 * (std::f32::consts::TAU * 440.0 * t).sin() + 0.3 * (std::f32::consts::TAU * 660.0 * t).sin()
        }).collect();
        let bar: Vec<f32> = (0..FRAMES).map(|i| {
            let t = i as f32 / RATE;
            (std::f32::consts::TAU * 220.0 * t).sin() + 0.7 * (std::f32::consts::TAU * 606.0 * t).sin() + 0.5 * (std::f32::consts::TAU * 1188.0 * t).sin()
        }).collect();
        let (a, b) = (analyze_signal(&tone, RATE), analyze_signal(&bar, RATE));
        assert!(a.inharmonic < 0.1, "{}", a.inharmonic);
        assert!(b.inharmonic > 0.3, "{}", b.inharmonic);
        assert!(a.energy > 0.4, "a steady tone is all energy");
        assert_eq!(analyze_signal(&[0.0; 100], RATE), Timbre::default());
    }
}

/// `cargo test -p aitrack-stock --lib bloom_table -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn bloom_table() {
    use crate::instrument::bloom::genome::Species;
    use crate::instrument::bloom::grow::{GrowOpts, generate};
    use crate::instrument::bloom::render::render_normalised;
    for species in Species::ALL {
        let s = generate(1337, GrowOpts { species, spread: 1.0 });
        println!("== {species:?}");
        for (i, g) in s.sounds.iter().enumerate() {
            let t = analyze(g);
            let (l, r) = render_normalised(g, 48_000.0, 96_000, 1.0);
            let peak = l.iter().chain(&r).fold(0f32, |m, v| m.max(v.abs()));
            let len = crate::instrument::bloom::render::tail_frames(&l, &r, 48_000.0, 96_000) as f32 / 48.0;
            println!("{i:2} {:<10} d={:.2} cen={:6.0} noise={:.2} en={:.2} att={:3.0} dec={:3.0} inh={:.2} wob={:.2} peak={peak:.2} len={len:5.0}ms", g.role.label(), s.distances[i], t.centroid, t.noisiness, t.energy, t.attack_ms, t.decay_ms, t.inharmonic, t.wobble);
        }
    }
}
