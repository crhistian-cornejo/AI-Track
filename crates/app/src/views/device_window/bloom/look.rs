//! A sound's colour and shape, from its timbre (pure functions).
//!
//! | measure | becomes |
//! |---|---|
//! | spectral centroid (log, 60 Hz .. 12 kHz) | hue: deep violet → red → amber → green → cyan, near-white at the top |
//! | tonality (1 − noisiness) | chroma: pure tones saturated, noise greyish |
//! | energy (RMS over peak) | lightness (OkLCH L, so it reads alike on dark and light themes) |
//! | short decay + inharmonic partials | spikes: sharp lobes for clicks and metal |
//! | long tonal sounds | round soft blobs |
//! | FM movement | wobble of the outline |
//!
//! Colours are computed in OkLCH (perceptual lightness and chroma) and
//! mapped into the sRGB gamut by lowering the chroma, so two cells of equal
//! lightness look equally light whatever their hue.

use aitrack_stock::instrument::bloom::demo_note;
use aitrack_stock::instrument::bloom::genome::{Genome, g};
#[cfg(test)]
use aitrack_stock::instrument::bloom::genome::SOUNDS;
use aitrack_stock::instrument::bloom::patch::{Kind, Patch};
use aitrack_stock::instrument::bloom::timbre::Timbre;
use aitrack_stock::instrument::bloom::tonal;
use aitrack_stock::instrument::wavetable::p as w;

/// How a cell looks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// OkLCH lightness 0..1, chroma, hue in degrees.
    pub l: f32,
    pub c: f32,
    pub h: f32,
    /// Linear sRGB 0..1 (what the field blends in).
    pub rgb: [f32; 3],
    /// Number of lobes of the outline (3..10).
    pub lobes: u32,
    /// How far the lobes reach out (0 = round).
    pub spikes: f32,
    /// How pointed the lobes are (exponent, 1.5 soft .. 7.5 needle).
    pub sharp: f32,
    /// Outline movement from FM (0..1).
    pub wobble: f32,
    /// How much the cell glows when it plays (0..1).
    pub glow: f32,
}

#[inline]
fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The centroid on a 0..1 brightness scale (60 Hz .. 12 kHz, log).
pub fn brightness(centroid: f32) -> f32 {
    ((centroid.max(1.0) / 60.0).log2() / 200f32.log2()).clamp(0.0, 1.0)
}

/// The hue of a brightness: violet (300°) through red, amber and green to cyan (200°).
pub fn hue(brightness: f32) -> f32 {
    (300.0 + 260.0 * brightness.clamp(0.0, 1.0)) % 360.0
}

/// OkLab → linear sRGB (B. Ottosson's matrices).
pub fn oklab_to_linear(l: f32, a: f32, b: f32) -> [f32; 3] {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    ]
}

/// OkLCH → linear sRGB, chroma lowered until the colour fits the gamut.
pub fn oklch_to_linear(l: f32, c: f32, h_deg: f32) -> ([f32; 3], f32) {
    let (s, co) = h_deg.to_radians().sin_cos();
    let at = |c: f32| oklab_to_linear(l, c * co, c * s);
    let fits = |v: [f32; 3]| v.iter().all(|x| (-1e-4..=1.0001).contains(x));
    let mut c_ok = c.max(0.0);
    if !fits(at(c_ok)) {
        let (mut lo, mut hi) = (0.0, c_ok);
        for _ in 0..18 {
            let mid = (lo + hi) * 0.5;
            if fits(at(mid)) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        c_ok = lo;
    }
    (at(c_ok).map(|x| x.clamp(0.0, 1.0)), c_ok)
}

/// Linear → sRGB-encoded (0..1).
pub fn encode(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

/// The look of a timbre. `dark` picks the lightness range of the theme.
pub fn look(t: &Timbre, dark: bool) -> Look {
    let x = brightness(t.centroid);
    let tonal = (1.0 - t.noisiness).clamp(0.0, 1.0);
    let white = smooth(0.82, 1.0, x);
    let energy = t.energy.clamp(0.0, 1.0);
    let l = if dark { 0.5 + 0.26 * energy + 0.14 * white } else { 0.46 + 0.18 * energy + 0.08 * white };
    let c = (0.035 + 0.14 * tonal.powf(0.8)) * (1.0 - 0.6 * white);
    let h = hue(x);
    let (rgb, c) = oklch_to_linear(l, c, h);
    let transient = 1.0 - (t.decay_ms / 180.0).clamp(0.0, 1.0);
    let metal = t.inharmonic.clamp(0.0, 1.0);
    let fast = if t.attack_ms < 8.0 { 1.0 } else { 0.6 };
    let spikes = ((0.55 * transient * (0.4 + 0.6 * metal) + 0.45 * metal) * fast).clamp(0.0, 0.9);
    Look {
        l,
        c,
        h,
        rgb,
        lobes: (3.0 + metal * 5.0 + transient * 2.0).round().clamp(3.0, 10.0) as u32,
        spikes,
        sharp: 1.5 + 6.0 * transient,
        wobble: t.wobble.clamp(0.0, 1.0),
        glow: 0.4 + 0.6 * energy,
    }
}

/// A first guess of a genome's timbre from its genes alone, shown until the
/// render-based analysis arrives (so a new cell never flashes grey).
pub fn estimate(genome: &Genome) -> Timbre {
    let gn = genome.folded();
    let p = |k: usize| gn.plain(k);
    let pitch_hz = 440.0 * 2f32.powf((p(g::PITCH) - 69.0) / 12.0);
    let tone = p(g::TONE_LEVEL);
    let noise = p(g::NOISE_LEVEL);
    let total = (tone + noise).max(1e-3);
    let tone_c = pitch_hz * (1.0 + p(g::FM_INDEX) * 0.4) * if gn.index(g::OSC1) >= 2 { 3.0 } else { 1.3 };
    let centroid = (tone_c * tone + p(g::NOISE_FREQ) * noise) / total;
    let decay = p(g::DECAY).max(if p(g::RES_MIX) > 0.3 { p(g::RES_DECAY) } else { 0.0 });
    let fm = (p(g::FM_INDEX) / 6.0).min(1.0) * (p(g::FM_DECAY) / 250.0).min(1.0);
    Timbre {
        centroid: centroid.min(p(g::CUTOFF) * 1.5),
        noisiness: (noise / total).clamp(0.0, 1.0),
        energy: (decay / 600.0).clamp(0.05, 0.8),
        attack_ms: p(g::ATTACK),
        decay_ms: decay.min(200.0),
        inharmonic: (p(g::RES_MIX) * (p(g::RES_SHAPE) / 4.0) + fm * 0.5).clamp(0.0, 1.0),
        wobble: fm,
    }
}

/// A tonal timbre as the looks read it. A patch is measured over 750 ms of
/// a held note: when it is still rising or sounding at the end of that
/// window (a pad's slow attack, a held lead), its "decay" is the window
/// running out, not the sound falling, so it counts as still ringing (as a
/// drum sound's 200 ms do) instead of as a click.
pub fn ringing(t: &Timbre) -> Timbre {
    let mut o = *t;
    if t.attack_ms + t.decay_ms >= 600.0 {
        o.decay_ms = o.decay_ms.max(750.0 - t.attack_ms).max(200.0);
    }
    o
}

/// `l` as a sound of `kind` wears it: the hue stays the timbre's, the kind
/// sets how deep and vivid it is (identity for drums). A bass deep and
/// saturated, a lead vivid, a pad an airy pastel, a pluck bright, FX
/// greyer, a texture a grey fog; the CPU field's spikes follow the kind too.
pub fn with_kind(l: &Look, kind: Kind, dark: bool) -> Look {
    let (dl, kc, ks): (f32, f32, f32) = match kind {
        Kind::Drums => return *l,
        Kind::Bass => (-0.05, 1.2, 0.5),
        Kind::Lead => (0.04, 1.25, 1.1),
        Kind::Pad => (0.08, 0.6, 0.15),
        Kind::Pluck => (0.03, 1.15, 1.6),
        Kind::Keys => (0.0, 0.9, 1.0),
        Kind::Fx => (0.0, 0.7, 1.2),
        Kind::Texture => (0.03, 0.45, 0.6),
    };
    let lightness = (l.l + if dark { dl } else { dl * 0.6 }).clamp(0.3, 0.9);
    let (rgb, c) = oklch_to_linear(lightness, l.c * kc, l.h);
    Look { l: lightness, c, rgb, spikes: (l.spikes * ks + if kind == Kind::Pluck { 0.25 } else { 0.0 }).clamp(0.0, 0.9), ..*l }
}

/// A first guess of a tonal patch's timbre from its synth parameters (the
/// kind's demo note through the oscillators, the low-pass and the amp
/// envelope), shown until the render-based analysis arrives.
pub fn estimate_patch(patch: &Patch) -> Timbre {
    let x = patch.folded();
    let (key, hold, _) = demo_note(patch.kind);
    let f0 = 440.0 * 2f32.powf((f32::from(key) - 69.0) / 12.0);
    // Brightness of the sources: a saw-rich table ~6 harmonics up, a sine ~1.
    let lit: Vec<(f32, f32)> = (0..3u32).map(|o| (x.plain(w::osc(o, w::LEVEL)), x.plain(w::osc(o, w::POS)) / 100.0)).filter(|(l, _)| *l > 5.0).collect();
    let level: f32 = lit.iter().map(|(l, _)| l).sum::<f32>().max(1.0);
    let rich = lit.iter().map(|(l, pos)| l * (3.0 + 4.0 * pos)).sum::<f32>() / level;
    let mut centroid = f0 * if lit.is_empty() { 2.0 } else { rich };
    if x.choice(w::filt(0, w::F_ENABLE)) == 1 {
        let cutoff = x.plain(w::CUTOFF) * 2f32.powf(x.plain(w::FILTER_ENV) / 100.0 * 2.0);
        centroid = match x.choice(w::FILTER_TYPE) {
            0 => centroid.min(f0 + cutoff * 0.6),
            1 => centroid.max(cutoff),
            _ => (centroid * cutoff).sqrt(),
        };
    }
    let noise = tonal::noise_share(&x);
    centroid = centroid * (1.0 - noise) + 5_000.0 * noise;
    let sustain = x.plain(w::AMP_SUSTAIN) / 100.0;
    let decay = x.plain(w::AMP_DECAY);
    let held = (hold * 1000.0).min(750.0);
    let decay_ms = if sustain > 0.3 { held } else { (x.plain(w::AMP_ATTACK) + decay).min(750.0) };
    Timbre {
        centroid,
        noisiness: (noise * 0.9).clamp(0.0, 1.0),
        energy: (0.15 + 0.55 * sustain + 0.2 * (decay / 1_500.0).min(1.0)).clamp(0.05, 0.85),
        attack_ms: x.plain(w::AMP_ATTACK).min(750.0),
        decay_ms,
        inharmonic: if (0..3u32).any(|o| x.choice(w::osc(o, w::WARP)) >= 8) { 0.4 } else { 0.05 },
        wobble: tonal::motion(&x),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_patch_estimate_tells_the_kinds_apart() {
        let mean = |k: Kind, f: &dyn Fn(&Timbre) -> f32| {
            let s = tonal::generate(1_337, k);
            s.patches.iter().map(|p| f(&estimate_patch(p))).sum::<f32>() / SOUNDS as f32
        };
        // A bass sits low, a lead high; a pad swells slowly, a pluck is short.
        assert!(mean(Kind::Bass, &|t| t.centroid) < mean(Kind::Lead, &|t| t.centroid));
        assert!(mean(Kind::Pad, &|t| t.attack_ms) > 5.0 * mean(Kind::Pluck, &|t| t.attack_ms).max(1.0));
        assert!(mean(Kind::Pluck, &|t| t.decay_ms) < mean(Kind::Pad, &|t| t.decay_ms));
        assert!(mean(Kind::Texture, &|t| t.noisiness) > mean(Kind::Keys, &|t| t.noisiness));
        for k in Kind::TONAL {
            for p in &tonal::generate(7, k).patches {
                let t = estimate_patch(p);
                assert!(t.centroid.is_finite() && t.centroid > 0.0, "{k:?} {t:?}");
                assert!((0.0..=1.0).contains(&t.noisiness) && (0.0..=1.0).contains(&t.energy) && (0.0..=1.0).contains(&t.wobble), "{k:?} {t:?}");
            }
        }
    }

    #[test]
    fn a_note_still_sounding_at_the_window_end_is_ringing() {
        // A pad peaking at the end of the 750 ms window, a held lead, a pluck.
        let pad = Timbre { attack_ms: 744.0, decay_ms: 6.0, ..Timbre::default() };
        let lead = Timbre { attack_ms: 3.0, decay_ms: 747.0, ..Timbre::default() };
        let pluck = Timbre { attack_ms: 2.0, decay_ms: 97.0, ..Timbre::default() };
        assert!(ringing(&pad).decay_ms >= 200.0 && ringing(&lead).decay_ms >= 200.0);
        assert_eq!(ringing(&pluck), pluck);
        assert!(look(&ringing(&pad), true).spikes < 0.1, "a pad is round, not a click");
    }

    #[test]
    fn kinds_wear_their_colour_differently() {
        let base = look(&timbre(600.0, 0.1, 0.4, 200.0, 0.0), true);
        assert_eq!(with_kind(&base, Kind::Drums, true), base);
        let (pad, lead, bass, tex) = (with_kind(&base, Kind::Pad, true), with_kind(&base, Kind::Lead, true), with_kind(&base, Kind::Bass, true), with_kind(&base, Kind::Texture, true));
        assert!(pad.l > base.l && pad.c < base.c, "an airy pastel");
        assert!(lead.c >= base.c - 1e-4, "vivid");
        assert!(bass.l < base.l, "deep");
        assert!(tex.c < pad.c, "a grey fog");
        assert!(with_kind(&base, Kind::Pluck, true).spikes > pad.spikes);
        for k in Kind::ALL {
            for dark in [true, false] {
                let l = with_kind(&base, k, dark);
                assert!(l.rgb.iter().all(|v| (0.0..=1.0).contains(v)) && l.h == base.h, "{k:?}");
            }
        }
    }

    fn timbre(centroid: f32, noisiness: f32, energy: f32, decay_ms: f32, inharmonic: f32) -> Timbre {
        Timbre { centroid, noisiness, energy, attack_ms: 1.0, decay_ms, inharmonic, wobble: 0.0 }
    }

    #[test]
    fn white_and_black_come_out_right() {
        let w = oklab_to_linear(1.0, 0.0, 0.0);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-3), "{w:?}");
        let k = oklab_to_linear(0.0, 0.0, 0.0);
        assert!(k.iter().all(|v| v.abs() < 1e-6));
        assert!((encode(1.0) - 1.0).abs() < 1e-6 && encode(0.0) == 0.0);
        // A very saturated request is brought into the gamut.
        let (rgb, c) = oklch_to_linear(0.7, 0.4, 150.0);
        assert!(c < 0.4 && rgb.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn low_sounds_are_violet_red_and_high_ones_cyan() {
        let kick = look(&timbre(110.0, 0.1, 0.7, 180.0, 0.0), true);
        let hat = look(&timbre(9_000.0, 0.6, 0.1, 20.0, 0.1), true);
        // Violet/red sit near 300..360 and 0..40 degrees; cyan near 200.
        assert!(kick.h > 290.0 || kick.h < 40.0, "{}", kick.h);
        assert!((hat.h - 190.0).abs() < 25.0, "{}", hat.h);
        assert!(hat.c < kick.c, "the top of the range goes white-ish");
        assert_eq!(hue(0.0), 300.0);
        assert!((hue(1.0) - 200.0).abs() < 1e-3);
    }

    #[test]
    fn noise_is_greyer_and_energy_is_lighter() {
        let tone = look(&timbre(800.0, 0.05, 0.5, 200.0, 0.0), true);
        let noise = look(&timbre(800.0, 0.95, 0.5, 200.0, 0.0), true);
        assert!(noise.c < tone.c * 0.5, "{} vs {}", noise.c, tone.c);
        let quiet = look(&timbre(800.0, 0.2, 0.05, 200.0, 0.0), true);
        let full = look(&timbre(800.0, 0.2, 0.8, 200.0, 0.0), true);
        assert!(full.l > quiet.l + 0.15);
        // Light themes get darker cells so they still stand out.
        assert!(look(&timbre(800.0, 0.2, 0.8, 200.0, 0.0), false).l < full.l);
    }

    #[test]
    fn clicks_and_metal_are_spiky_and_long_tones_are_round() {
        let click = look(&timbre(3_000.0, 0.3, 0.05, 10.0, 0.7), true);
        let pad = look(&timbre(400.0, 0.05, 0.7, 200.0, 0.0), true);
        assert!(click.spikes > 0.5 && pad.spikes < 0.05, "{} {}", click.spikes, pad.spikes);
        assert!(click.sharp > pad.sharp);
        assert!(click.lobes > pad.lobes);
        let mut fm = timbre(400.0, 0.05, 0.7, 200.0, 0.0);
        fm.wobble = 0.8;
        assert_eq!(look(&fm, true).wobble, 0.8);
    }

    #[test]
    fn the_estimate_orders_sounds_like_the_analysis() {
        use aitrack_stock::instrument::bloom::grow::{GrowOpts, generate};
        let s = generate(4, GrowOpts::default());
        let (kick, hat) = (estimate(&s.cores[0]), estimate(&s.cores[6]));
        assert!(hat.centroid > kick.centroid * 3.0);
        assert!(hat.noisiness > kick.noisiness);
    }
}
