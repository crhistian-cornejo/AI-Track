//! Offline renders of one sound: what the level normalisation, the visual's
//! colour analysis, the sample export and the matching search listen to.
//! The same [`Voice`] as live playback, so a render sounds like the pad.

use super::genome::{Genome, g};
use super::voice::{Macros, Patch, Voice};

/// Peak level every sound is normalised to before its own Level gene.
pub const TARGET_PEAK: f32 = 0.8;
/// The level measurement: rate and length (short, so a scene measures in a
/// few milliseconds; the loudest part of a percussive sound is its start).
pub const MEASURE_RATE: f32 = 24_000.0;
pub const MEASURE_FRAMES: usize = 3_600;

/// The noise seed of a sound's hits (every hit of a sound is alike, so a
/// render, live playback and an export agree).
pub fn noise_seed(sound: usize, key: u8) -> u32 {
    (sound as u32).wrapping_mul(0x9E37_79B9) ^ u32::from(key).wrapping_mul(0x85EB_CA6B) ^ 0x2545_F491
}

/// Render `frames` of `genome` at `sr` into `left` / `right` (cleared first),
/// one hit at `velocity` with neutral macros and unit gain. No allocation.
pub fn render_into(genome: &Genome, sr: f32, velocity: f32, gain: f32, left: &mut [f32], right: &mut [f32]) {
    render_shaped(genome, sr, velocity, gain, 0.0, &Macros::default(), left, right);
}

/// [`render_into`] as the device plays it: `transpose` semitones (its Tune)
/// and its macro knobs. No allocation.
#[allow(clippy::too_many_arguments)]
pub fn render_shaped(genome: &Genome, sr: f32, velocity: f32, gain: f32, transpose: f32, macros: &Macros, left: &mut [f32], right: &mut [f32]) {
    let n = left.len().min(right.len());
    left[..n].fill(0.0);
    right[..n].fill(0.0);
    let patch = Patch::new(genome, sr, transpose, velocity, 1.0, macros, gain);
    let mut v = Voice::blank();
    v.start(patch, 0, 60, 0, noise_seed(0, 60));
    let mut send = [0f32; 64];
    let mut at = 0;
    while at < n {
        let end = (at + 64).min(n);
        send[..end - at].fill(0.0);
        if !v.render(&mut left[at..end], &mut right[at..end], &mut send[..end - at]) {
            break;
        }
        at = end;
    }
}

/// A mono render (the mean of both sides).
pub fn render_mono(genome: &Genome, sr: f32, frames: usize, velocity: f32) -> Vec<f32> {
    render_mono_shaped(genome, sr, frames, velocity, 0.0, &Macros::default())
}

/// [`render_mono`] with the device's Tune and macro knobs.
pub fn render_mono_shaped(genome: &Genome, sr: f32, frames: usize, velocity: f32, transpose: f32, macros: &Macros) -> Vec<f32> {
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    crate::instrument::vault::kit::init();
    render_shaped(genome, sr, velocity, 1.0, transpose, macros, &mut l, &mut r);
    l.iter().zip(&r).map(|(a, b)| (a + b) * 0.5).collect()
}

/// The gain that brings a sound's peak to [`TARGET_PEAK`] (its Level gene
/// not counted). `scratch` holds at least [`MEASURE_FRAMES`] × 2 samples; no
/// allocation, so the device can measure on the audio thread when a seed
/// automation grows a new scene there.
pub fn measure_gain(genome: &Genome, scratch: &mut [f32]) -> f32 {
    let mut flat = *genome;
    flat.set(g::LEVEL, 0.0);
    flat.set(g::PAN, 0.0);
    let n = MEASURE_FRAMES.min(scratch.len() / 2);
    let (l, r) = scratch.split_at_mut(n);
    render_into(&flat, MEASURE_RATE, 1.0, 1.0, l, &mut r[..n]);
    let peak = l[..n].iter().zip(&r[..n]).fold(0f32, |m, (a, b)| m.max(a.abs()).max(b.abs()));
    if peak.is_finite() && peak > 1e-6 { (TARGET_PEAK / peak).clamp(0.05, 40.0) } else { 1.0 }
}

/// A stereo render at `sr`, normalised like the device plays it.
pub fn render_normalised(genome: &Genome, sr: f32, frames: usize, velocity: f32) -> (Vec<f32>, Vec<f32>) {
    crate::instrument::vault::kit::init();
    let mut scratch = vec![0.0; MEASURE_FRAMES * 2];
    let gain = measure_gain(genome, &mut scratch);
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    render_into(genome, sr, velocity, gain, &mut l, &mut r);
    (l, r)
}

/// How many frames a sound needs to ring out at `sr` (at most `max`), from a
/// render: the last sample above -70 dB of the peak, plus a short margin.
pub fn tail_frames(left: &[f32], right: &[f32], sr: f32, max: usize) -> usize {
    let peak = left.iter().chain(right).fold(0f32, |m, v| m.max(v.abs()));
    let floor = peak * 3e-4;
    let last = left.iter().zip(right).rposition(|(a, b)| a.abs() > floor || b.abs() > floor).unwrap_or(0);
    (last + (sr * 0.02) as usize).min(max).min(left.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::bloom::grow::{GrowOpts, generate};

    #[test]
    fn every_sound_of_a_scene_measures_to_the_target() {
        let s = generate(77, GrowOpts::default());
        let mut scratch = vec![0.0; MEASURE_FRAMES * 2];
        for (i, g) in s.sounds.iter().enumerate() {
            crate::instrument::vault::kit::init();
            let gain = measure_gain(g, &mut scratch);
            assert!(gain.is_finite() && gain > 0.0, "{i}");
            let mut flat = *g;
            flat.set(g::LEVEL, 0.0);
            flat.set(g::PAN, 0.0);
            let (mut l, mut r) = (vec![0.0; MEASURE_FRAMES], vec![0.0; MEASURE_FRAMES]);
            render_into(&flat, MEASURE_RATE, 1.0, gain, &mut l, &mut r);
            let peak = l.iter().chain(&r).fold(0f32, |m, v| m.max(v.abs()));
            assert!(gain >= 39.9 || (peak - TARGET_PEAK).abs() < 0.05, "sound {i} ({:?}) peaks at {peak}", g.role);
        }
    }

    #[test]
    fn renders_are_repeatable() {
        let s = generate(78, GrowOpts::default());
        let a = render_mono(&s.sounds[3], 48_000.0, 9_600, 0.9);
        let b = render_mono(&s.sounds[3], 48_000.0, 9_600, 0.9);
        assert_eq!(a, b);
        assert!(a.iter().any(|v| v.abs() > 1e-3));
    }
}
