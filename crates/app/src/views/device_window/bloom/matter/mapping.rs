//! A sound's look as the matter shows it, from its timbre (pure functions).
//!
//! | measure | becomes |
//! |---|---|
//! | spectral centroid | thin-film shift and thickness (the hue of the sheen); the vapour's hue |
//! | brightness, noisiness, decay | the vapour's colours (pale and holographic, greyer for noise) and amount |
//! | transient speed, inharmonicity | spike sharpness and count |
//! | noisiness, FM | surface roughness, wobble frequency and depth |
//! | energy, decay | size and how far a hit swells it |

use super::super::look::{brightness, hue, oklch_to_linear};
use aitrack_stock::instrument::bloom::timbre::Timbre;

/// What one sound does to the matter (on top of the material).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundLook {
    /// Thin-film offset, µm (shifts the sheen's hues).
    pub film_shift: f32,
    /// Extra film spread, µm.
    pub film_range: f32,
    /// Vapour colours, linear sRGB.
    pub smoke: [[f32; 3]; 3],
    /// 0..1: how much vapour rises.
    pub smoke_amount: f32,
    /// Seconds-ish: how fast the vapour rises.
    pub smoke_rise: f32,
    pub turbulence: f32,
    /// Spike cone exponent (1.2 a cone .. 3.4 a needle) and count on the sphere.
    pub spike_sharp: f32,
    pub spike_count: f32,
    /// 0..1: how readily the surface spikes on a hit.
    pub spike_gain: f32,
    /// Added to the material's roughness.
    pub roughness: f32,
    pub wobble_freq: f32,
    pub wobble_amp: f32,
    /// Lumps and folds.
    pub lumps: f32,
    pub fold_freq: f32,
    /// Resting size (≈1) and how far a hit swells it.
    pub size: f32,
    pub swell: f32,
    /// How fast the surface churns.
    pub churn: f32,
    /// The sound's colour (linear), for the swatches and a hint in the light.
    pub accent: [f32; 3],
}

#[inline]
fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// How fast a sound's attack and decay are: 1 a click, 0 a long tone.
pub fn transient(t: &Timbre) -> f32 {
    let decay = 1.0 - clamp01(t.decay_ms / 180.0);
    let attack = 1.0 - clamp01(t.attack_ms / 40.0);
    clamp01(decay * (0.55 + 0.45 * attack))
}

/// The vapour's three colours: pale, holographic bands around the sound's
/// hue (−55°, 0, +70°). Noise greys them, brightness pales them. On light
/// panels they are deeper so they still read.
pub fn smoke_colours(t: &Timbre, light: bool) -> [[f32; 3]; 3] {
    let x = brightness(t.centroid);
    let h = hue(x);
    let tonal = clamp01(1.0 - t.noisiness);
    let c = (0.035 + 0.075 * tonal) * if light { 1.35 } else { 1.0 };
    let l = if light { 0.62 + 0.10 * x } else { 0.80 + 0.12 * x };
    [-55.0f32, 0.0, 70.0].map(|dh| oklch_to_linear(l + dh / 2000.0, c, (h + dh).rem_euclid(360.0)).0)
}

/// The look of a timbre. `light` picks the vapour's lightness for the theme.
pub fn sound_look(t: &Timbre, light: bool) -> SoundLook {
    let x = brightness(t.centroid);
    let noise = clamp01(t.noisiness);
    let energy = clamp01(t.energy);
    let fast = transient(t);
    let metal = clamp01(t.inharmonic);
    let fm = clamp01(t.wobble);
    let long = clamp01(t.decay_ms / 200.0);
    let accent = {
        let l = 0.68 + 0.12 * energy;
        let c = 0.05 + 0.12 * (1.0 - noise);
        oklch_to_linear(l, c, hue(x)).0
    };
    SoundLook {
        // Low sounds sit on thick, magenta–green films; bright ones on thin, gold–blue ones.
        film_shift: lerp(0.22, -0.12, x),
        film_range: 0.05 + 0.25 * noise,
        smoke: smoke_colours(t, light),
        smoke_amount: clamp01(0.18 + 0.38 * noise + 0.30 * long + 0.14 * x),
        smoke_rise: lerp(0.18, 0.42, clamp01(fast * 0.6 + x * 0.4)),
        turbulence: lerp(0.5, 1.6, clamp01(noise * 0.7 + metal * 0.3)),
        spike_sharp: lerp(1.2, 3.4, clamp01(fast * 0.65 + metal * 0.35)),
        spike_count: lerp(36.0, 150.0, clamp01(metal * 0.6 + fast * 0.25 + x * 0.15)),
        spike_gain: clamp01(0.35 + 0.45 * fast + 0.2 * metal),
        roughness: 0.18 * noise + 0.04 * fm,
        wobble_freq: lerp(1.8, 6.5, fm),
        wobble_amp: 0.004 + 0.045 * fm,
        lumps: lerp(0.10, 0.20, clamp01(noise * 0.5 + metal * 0.5)),
        fold_freq: lerp(1.0, 2.1, clamp01(x * 0.6 + noise * 0.4)),
        size: lerp(0.88, 1.04, energy),
        swell: lerp(0.35, 1.0, clamp01(energy * 0.5 + (1.0 - fast) * 0.5)),
        churn: lerp(0.06, 0.18, clamp01(noise * 0.6 + fm * 0.4)),
        accent,
    }
}

/// A blend of two looks (`t` = 0 is `a`).
pub fn blend(a: &SoundLook, b: &SoundLook, t: f32) -> SoundLook {
    let t = clamp01(t);
    if t >= 1.0 {
        return *b;
    }
    let l = |x: f32, y: f32| lerp(x, y, t);
    let c3 = |x: [f32; 3], y: [f32; 3]| [l(x[0], y[0]), l(x[1], y[1]), l(x[2], y[2])];
    SoundLook {
        film_shift: l(a.film_shift, b.film_shift),
        film_range: l(a.film_range, b.film_range),
        smoke: [c3(a.smoke[0], b.smoke[0]), c3(a.smoke[1], b.smoke[1]), c3(a.smoke[2], b.smoke[2])],
        smoke_amount: l(a.smoke_amount, b.smoke_amount),
        smoke_rise: l(a.smoke_rise, b.smoke_rise),
        turbulence: l(a.turbulence, b.turbulence),
        spike_sharp: l(a.spike_sharp, b.spike_sharp),
        spike_count: l(a.spike_count, b.spike_count),
        spike_gain: l(a.spike_gain, b.spike_gain),
        roughness: l(a.roughness, b.roughness),
        wobble_freq: l(a.wobble_freq, b.wobble_freq),
        wobble_amp: l(a.wobble_amp, b.wobble_amp),
        lumps: l(a.lumps, b.lumps),
        fold_freq: l(a.fold_freq, b.fold_freq),
        size: l(a.size, b.size),
        swell: l(a.swell, b.swell),
        churn: l(a.churn, b.churn),
        accent: c3(a.accent, b.accent),
    }
}

impl Default for SoundLook {
    fn default() -> Self {
        sound_look(&Timbre { centroid: 900.0, noisiness: 0.3, energy: 0.4, attack_ms: 2.0, decay_ms: 120.0, inharmonic: 0.1, wobble: 0.1 }, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timbre(centroid: f32, noisiness: f32, energy: f32, decay_ms: f32, inharmonic: f32, wobble: f32) -> Timbre {
        Timbre { centroid, noisiness, energy, attack_ms: 1.0, decay_ms, inharmonic, wobble }
    }

    fn all_bounded(l: &SoundLook) -> bool {
        let unit = [l.smoke_amount, l.spike_gain, l.swell];
        unit.iter().all(|v| (0.0..=1.0).contains(v))
            && (1.0..=6.0).contains(&l.spike_sharp)
            && (30.0..=160.0).contains(&l.spike_count)
            && (0.0..=0.25).contains(&l.roughness)
            && (0.0..=0.06).contains(&l.wobble_amp)
            && (0.8..=1.1).contains(&l.size)
            && l.film_shift.abs() <= 0.25
            && l.smoke.iter().chain(std::iter::once(&l.accent)).flatten().all(|c| (0.0..=1.0).contains(c))
    }

    #[test]
    fn every_timbre_maps_into_range() {
        for c in [20.0, 60.0, 400.0, 3_000.0, 12_000.0, 30_000.0] {
            for n in [0.0, 0.5, 1.0, 2.0] {
                for d in [0.0, 30.0, 200.0, 5_000.0] {
                    for x in [-1.0, 0.0, 1.0, 3.0] {
                        let l = sound_look(&timbre(c, n, x, d, x, x), c > 1_000.0);
                        assert!(all_bounded(&l), "{c} {n} {d} {x}: {l:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn brighter_sounds_shift_the_film_one_way() {
        let mut last = f32::INFINITY;
        for c in [60.0, 200.0, 800.0, 3_000.0, 12_000.0] {
            let l = sound_look(&timbre(c, 0.2, 0.5, 100.0, 0.1, 0.0), false);
            assert!(l.film_shift < last, "monotonic in the centroid");
            last = l.film_shift;
        }
    }

    #[test]
    fn clicks_spike_sharper_and_metal_spikes_denser() {
        let click = sound_look(&timbre(3_000.0, 0.3, 0.1, 8.0, 0.2, 0.0), false);
        let pad = sound_look(&timbre(400.0, 0.05, 0.7, 200.0, 0.0, 0.0), false);
        assert!(click.spike_sharp > pad.spike_sharp + 1.0);
        let metal = sound_look(&timbre(3_000.0, 0.3, 0.1, 8.0, 0.9, 0.0), false);
        assert!(metal.spike_count > click.spike_count);
        // Faster transients spike sharper, monotonically.
        let sharp: Vec<f32> = [200.0, 120.0, 60.0, 10.0].iter().map(|d| sound_look(&timbre(1_000.0, 0.3, 0.3, *d, 0.2, 0.0), false).spike_sharp).collect();
        assert!(sharp.windows(2).all(|w| w[1] > w[0]), "{sharp:?}");
    }

    #[test]
    fn noise_roughens_and_smokes_fm_wobbles_energy_grows() {
        let tone = sound_look(&timbre(800.0, 0.05, 0.5, 100.0, 0.0, 0.0), false);
        let noise = sound_look(&timbre(800.0, 0.95, 0.5, 100.0, 0.0, 0.0), false);
        assert!(noise.roughness > tone.roughness && noise.smoke_amount > tone.smoke_amount);
        let fm = sound_look(&timbre(800.0, 0.05, 0.5, 100.0, 0.0, 0.9), false);
        assert!(fm.wobble_amp > tone.wobble_amp * 4.0 && fm.wobble_freq > tone.wobble_freq);
        let loud = sound_look(&timbre(800.0, 0.05, 0.9, 100.0, 0.0, 0.0), false);
        assert!(loud.size > tone.size);
        // Long decays leave more vapour.
        let long = sound_look(&timbre(800.0, 0.3, 0.5, 200.0, 0.0, 0.0), false);
        let short = sound_look(&timbre(800.0, 0.3, 0.5, 10.0, 0.0, 0.0), false);
        assert!(long.smoke_amount > short.smoke_amount);
    }

    #[test]
    fn smoke_hues_follow_the_sound_and_light_panels_get_deeper_vapour() {
        let lum = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let low = smoke_colours(&timbre(80.0, 0.1, 0.5, 100.0, 0.0, 0.0), false);
        let high = smoke_colours(&timbre(9_000.0, 0.1, 0.5, 100.0, 0.0, 0.0), false);
        assert_ne!(low, high);
        let dark = smoke_colours(&timbre(800.0, 0.1, 0.5, 100.0, 0.0, 0.0), false);
        let light = smoke_colours(&timbre(800.0, 0.1, 0.5, 100.0, 0.0, 0.0), true);
        assert!(lum(light[1]) < lum(dark[1]));
    }

    #[test]
    fn blending_reaches_both_ends() {
        let a = sound_look(&timbre(100.0, 0.1, 0.8, 200.0, 0.0, 0.0), false);
        let b = sound_look(&timbre(9_000.0, 0.9, 0.1, 10.0, 0.8, 0.8), false);
        assert_eq!(blend(&a, &b, 0.0), a);
        assert_eq!(blend(&a, &b, 1.0), b);
        let m = blend(&a, &b, 0.5);
        assert!((m.spike_sharp - (a.spike_sharp + b.spike_sharp) * 0.5).abs() < 1e-5);
    }
}
