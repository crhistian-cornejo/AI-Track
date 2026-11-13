//! Algorithms and colours of `fx.reverb`: what each named space does to the
//! two tank engines (the FDN and the Dattorro plate), and how the vintage
//! colours band-limit, thin out and quantise the tank like the converters
//! and memory of early digital units did.
//!
//! An algorithm is only a voicing: line lengths, early-reflection pattern
//! span and level, diffusion, modulation and the shimmer floor. Every
//! voicing keeps the exact RT60 design of the engine it runs on, so
//! `Decay` means the same thing in all of them.

/// Which tank renders the late field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Fdn,
    Plate,
}

/// The shape of one algorithm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Voicing {
    pub engine: Engine,
    /// Forced FDN size (`None` follows the Density parameter).
    pub lines: Option<usize>,
    /// Longest FDN line in ms at Size 0 % and 100 %.
    pub longest: (f32, f32),
    /// Shortest line as a fraction of the longest.
    pub spread: f32,
    /// Early-reflection pattern span in ms at Size 0 % and 100 %.
    pub early_span: (f32, f32),
    /// Multiplier on the Early Reflections level.
    pub early_gain: f32,
    /// Multiplier on Diffusion (input all-pass gain).
    pub diffusion: f32,
    /// Multiplier on the modulation depth.
    pub mod_scale: f32,
    /// Multiplier on Damping (< 1 = a brighter tank).
    pub damping: f32,
    /// Late-field level trim.
    pub late: f32,
    /// Shimmer that is always present (the Shimmer amount scales the rest).
    pub shimmer_floor: f32,
}

const FDN: Voicing = Voicing {
    engine: Engine::Fdn,
    lines: None,
    longest: (45.0, 195.0),
    spread: 0.3,
    early_span: (10.0, 95.0),
    early_gain: 0.8,
    diffusion: 1.0,
    mod_scale: 1.0,
    damping: 1.0,
    late: 1.0,
    shimmer_floor: 0.0,
};

/// Voicings: `0..3` are the classic Hall / Room / Plate of the Mode
/// parameter; the rest follow [`ALGORITHMS`] from index 1.
pub const VOICES: [Voicing; 11] = [
    // Classic Hall.
    FDN,
    // Classic Room.
    Voicing { longest: (12.0, 70.0), early_span: (4.0, 40.0), early_gain: 1.4, mod_scale: 0.5, ..FDN },
    // Classic Plate.
    Voicing { engine: Engine::Plate, early_gain: 0.0, ..FDN },
    // Concert Hall: long, even, gently modulated, warm top.
    Voicing { lines: Some(16), longest: (60.0, 195.0), spread: 0.32, early_span: (14.0, 95.0), early_gain: 0.7, mod_scale: 0.8, damping: 1.15, ..FDN },
    // Bright Hall: the same hall with half the damping and more motion.
    Voicing { lines: Some(16), longest: (55.0, 190.0), spread: 0.3, early_span: (12.0, 90.0), early_gain: 0.6, mod_scale: 1.2, damping: 0.5, ..FDN },
    // Plate.
    Voicing { engine: Engine::Plate, early_gain: 0.0, ..FDN },
    // Room.
    Voicing { lines: Some(16), longest: (12.0, 70.0), early_span: (4.0, 40.0), early_gain: 1.4, mod_scale: 0.5, ..FDN },
    // Chamber: mid-size, dense walls, strong early field.
    Voicing { lines: Some(16), longest: (22.0, 100.0), spread: 0.45, early_span: (6.0, 55.0), early_gain: 1.2, diffusion: 1.07, mod_scale: 0.5, ..FDN },
    // Ambience: mostly early field, a short quiet tail.
    Voicing { lines: Some(8), longest: (9.0, 40.0), spread: 0.5, early_span: (2.0, 22.0), early_gain: 1.7, mod_scale: 0.3, late: 0.6, ..FDN },
    // Shimmer: big modulated hall with the pitch feedback always on.
    Voicing { lines: Some(16), longest: (70.0, 195.0), early_span: (20.0, 95.0), early_gain: 0.3, mod_scale: 1.6, damping: 0.8, shimmer_floor: 0.4, ..FDN },
    // Cloud: very long, heavily diffused and modulated wash.
    Voicing { lines: Some(16), longest: (90.0, 195.0), spread: 0.25, early_span: (20.0, 95.0), early_gain: 0.2, diffusion: 1.07, mod_scale: 2.5, ..FDN },
];

pub const ALGORITHMS: &[&str] = &["Classic", "Concert Hall", "Bright Hall", "Plate", "Room", "Chamber", "Ambience", "Shimmer", "Cloud"];

/// Index into [`VOICES`] for an Algorithm choice and the classic Mode.
pub fn voice_index(algorithm: usize, mode: usize) -> usize {
    if algorithm == 0 { mode.min(2) } else { (algorithm + 2).min(VOICES.len() - 1) }
}

pub const COLORS: &[&str] = &["Now", "80s", "70s"];

/// What a colour does to the tank.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    /// Tank input and output band limit in Hz (`None` = full band).
    pub bandwidth: Option<f32>,
    /// Wet-signal word length (`None` = float).
    pub bits: Option<u32>,
    /// Multiplier on the modulation depth.
    pub mod_scale: f32,
    /// Forced FDN size.
    pub lines: Option<usize>,
    /// Multiplier on Diffusion.
    pub diffusion: f32,
}

pub const COLOR_TABLE: [Color; 3] = [
    // Now: clean, full band.
    Color { bandwidth: None, bits: None, mod_scale: 1.0, lines: None, diffusion: 1.0 },
    // 80s: ~ 34 kHz converters (≈ 11 kHz usable band), 14-bit-ish words
    // and the chorused delay modulation those units were known for.
    Color { bandwidth: Some(11_000.0), bits: Some(14), mod_scale: 1.5, lines: None, diffusion: 1.0 },
    // 70s: ~ 20 kHz sampling (≈ 7 kHz band), 12-bit words, sparse tank,
    // hardly any modulation.
    Color { bandwidth: Some(7_000.0), bits: Some(12), mod_scale: 0.4, lines: Some(8), diffusion: 0.7 },
];

/// Quantise to a signed word of `bits` (full scale ±1), rounding.
#[inline]
pub fn quantise(x: f32, scale: f32) -> f32 {
    (x * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_follows_the_mode_and_algorithms_follow_their_list() {
        assert_eq!(voice_index(0, 0), 0);
        assert_eq!(voice_index(0, 2), 2);
        assert_eq!(VOICES[voice_index(3, 0)].engine, Engine::Plate, "Plate algorithm");
        assert_eq!(VOICES[voice_index(7, 0)].shimmer_floor, 0.4, "Shimmer algorithm");
        assert_eq!(voice_index(ALGORITHMS.len() - 1, 0), VOICES.len() - 1, "every algorithm has a voicing");
        for v in VOICES {
            assert!(v.longest.1 <= 195.0 && v.longest.0 > 0.0 && v.spread > 0.0 && v.spread < 1.0);
            assert!(v.early_span.1 <= 95.0, "early taps fit the pre-delay line");
            assert!(v.diffusion * 0.7 <= 0.75, "input all-passes stay well inside stability");
        }
    }

    #[test]
    fn quantise_rounds_to_the_word() {
        let s = 2f32.powi(11);
        assert_eq!(quantise(0.0, s), 0.0);
        assert_eq!(quantise(1e-5, s), 0.0, "below half an LSB is silence");
        assert!((quantise(0.3, s) - 0.3).abs() <= 0.5 / s);
    }
}
