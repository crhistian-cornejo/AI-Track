//! Loudness measurement shared by the preset generator (which calibrates
//! each preset's volume) and the tests (which check it): a fixed phrase per
//! sound type, peak over everything and the loudest 400 ms RMS window.

use crate::test_util::{SR, render_notes};
use aitrack_dsp::Processor;

/// Where the factory presets sit (loudest-window RMS, dBFS).
pub const TARGET_RMS_DB: f32 = -17.0;
/// How far a preset may stray from the target.
pub const TOLERANCE_DB: f32 = 2.5;
/// Highest allowed sample peak (dBFS).
pub const PEAK_CEILING_DB: f32 = -1.0;

/// The test phrase of a sound type: `(keys, hold seconds)`.
pub fn phrase(kind: &str) -> (&'static [u8], f32) {
    match kind {
        "Bass" => (&[36], 1.2),
        "Lead" => (&[60], 1.2),
        "Pluck" | "Keys" | "Organ" => (&[60, 64, 67], 1.2),
        _ => (&[57, 60, 64], 2.0),
    }
}

/// Render the phrase of `kind` and return `(peak, rms dBFS)`.
pub fn measure(p: &mut dyn Processor, kind: &str) -> (f32, f32) {
    let (keys, hold) = phrase(kind);
    let on = (SR * hold as f64) as usize;
    let total = on + (SR * 1.0) as usize;
    let mut notes = Vec::new();
    for &k in keys {
        notes.push((0, k, 0.8, true));
        notes.push((on, k, 0.0, false));
    }
    let (l, r) = render_notes(p, &notes, total);
    let peak = l.iter().chain(&r).fold(0.0f32, |m, x| m.max(x.abs()));
    let win = (SR * 0.4) as usize;
    let hop = (SR * 0.05) as usize;
    let mut best = 0.0f64;
    let mut start = 0;
    while start + win <= l.len() {
        let e: f64 = (start..start + win).map(|i| 0.5 * ((l[i] as f64).powi(2) + (r[i] as f64).powi(2))).sum();
        best = best.max(e / win as f64);
        start += hop;
    }
    (peak, 10.0 * best.max(1e-20).log10() as f32)
}
