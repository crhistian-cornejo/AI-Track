//! Frequency responses of the filter structures the stock plugins run, so a
//! UI can draw an EQ or filter curve with the same maths as the audio path
//! (matched biquads, TPT SVFs, Butterworth cascades, first-order tilt).
//!
//! [`BandResponse::design`] mirrors how `fx.eq` designs one band;
//! [`BandResponse::magnitude_at`] is the curve in dB. Nothing here runs on
//! the audio thread.

use crate::biquad::{BiquadCoeffs, BiquadShape};
use crate::svf::{OnePole, SvfCoeffs, SvfMode, butterworth_q};

/// A complex value `(re, im)`.
pub type Complex = (f64, f64);

#[inline]
pub fn cmul(a: Complex, b: Complex) -> Complex {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

/// Power of a complex response in dB.
#[inline]
pub fn power_db(c: Complex) -> f64 {
    10.0 * (c.0 * c.0 + c.1 * c.1).max(1e-30).log10()
}

/// Shape of one EQ band (index = the `fx.eq` band type).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BandKind {
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
    BandPass,
    Tilt,
}

impl BandKind {
    pub const ALL: [BandKind; 8] =
        [Self::Bell, Self::LowShelf, Self::HighShelf, Self::LowCut, Self::HighCut, Self::Notch, Self::BandPass, Self::Tilt];

    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(Self::ALL.len() - 1)]
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0)
    }

    /// Whether the band's gain changes its shape.
    pub fn uses_gain(self) -> bool {
        matches!(self, Self::Bell | Self::LowShelf | Self::HighShelf | Self::Tilt)
    }

    /// Whether Q changes its shape (cuts are pure Butterworth, tilt is first order).
    pub fn uses_q(self) -> bool {
        !matches!(self, Self::LowCut | Self::HighCut | Self::Tilt)
    }

    pub fn uses_slope(self) -> bool {
        matches!(self, Self::LowCut | Self::HighCut)
    }
}

/// Butterworth Q of second-order section `section` of an order-`order`
/// cascade. Odd orders keep their real pole in a first-order section and
/// put the complex pairs at `θ = π (k + 1) / n`.
pub fn cascade_q(order: usize, section: usize) -> f32 {
    if order.is_multiple_of(2) {
        butterworth_q(order, section)
    } else {
        let theta = std::f64::consts::PI * (section + 1) as f64 / order as f64;
        (1.0 / (2.0 * theta.cos())) as f32
    }
}

#[derive(Clone, Copy, Debug)]
enum Stage {
    Biquad(BiquadCoeffs),
    Svf(SvfCoeffs),
    Cut { sections: [SvfCoeffs; 8], pairs: usize, first: Option<OnePole>, high_cut: bool },
    Tilt { pole: OnePole, low: f64, high: f64 },
}

/// One designed band and the sample rate it was designed for.
#[derive(Clone, Copy, Debug)]
pub struct BandResponse {
    stage: Stage,
    sample_rate: f64,
}

impl BandResponse {
    /// Design a band. `order` is the cut slope in poles (1..=16, 6 dB/oct
    /// each) and is ignored by the other shapes; `matched` picks the
    /// magnitude-matched biquad for bells, shelves and band-passes (as the
    /// EQ's Matched mode does for static bands) instead of the SVF.
    pub fn design(kind: BandKind, freq: f32, gain_db: f32, q: f32, order: usize, matched: bool, sample_rate: f64) -> Self {
        let shape = match kind {
            BandKind::Bell => Some((BiquadShape::Bell, SvfMode::Bell)),
            BandKind::LowShelf => Some((BiquadShape::LowShelf, SvfMode::LowShelf)),
            BandKind::HighShelf => Some((BiquadShape::HighShelf, SvfMode::HighShelf)),
            BandKind::BandPass => Some((BiquadShape::Bandpass, SvfMode::Bandpass)),
            _ => None,
        };
        let stage = match (kind, shape) {
            (_, Some((bs, sm))) => {
                if matched {
                    Stage::Biquad(BiquadCoeffs::matched(bs, freq as f64, q as f64, gain_db as f64, sample_rate))
                } else {
                    Stage::Svf(SvfCoeffs::new(sm, freq, q, gain_db, sample_rate))
                }
            }
            (BandKind::Notch, _) => Stage::Svf(SvfCoeffs::new(SvfMode::Notch, freq, q, 0.0, sample_rate)),
            (BandKind::LowCut | BandKind::HighCut, _) => {
                let high_cut = kind == BandKind::HighCut;
                let order = order.clamp(1, 16);
                let pairs = order / 2;
                let mode = if high_cut { SvfMode::Lowpass } else { SvfMode::Highpass };
                let mut sections = [SvfCoeffs::passthrough(); 8];
                for (k, s) in sections.iter_mut().enumerate().take(pairs) {
                    *s = SvfCoeffs::new(mode, freq, cascade_q(order, k), 0.0, sample_rate);
                }
                let first = (order % 2 == 1).then(|| OnePole::new(freq, sample_rate));
                Stage::Cut { sections, pairs, first, high_cut }
            }
            _ => Stage::Tilt {
                pole: OnePole::new(freq, sample_rate),
                low: crate::db_to_gain(-0.5 * gain_db) as f64,
                high: crate::db_to_gain(0.5 * gain_db) as f64,
            },
        };
        Self { stage, sample_rate }
    }

    /// Complex response at `freq`.
    pub fn response(&self, freq: f64) -> Complex {
        let sr = self.sample_rate;
        match &self.stage {
            Stage::Biquad(c) => c.response(freq, sr),
            Stage::Svf(c) => c.response(freq, sr),
            Stage::Cut { sections, pairs, first, high_cut } => {
                let mut acc = (1.0, 0.0);
                for s in &sections[..*pairs] {
                    acc = cmul(acc, s.response(freq, sr));
                }
                if let Some(pole) = first {
                    let lp = pole.lowpass_response(freq, sr);
                    acc = cmul(acc, if *high_cut { lp } else { (1.0 - lp.0, -lp.1) });
                }
                acc
            }
            Stage::Tilt { pole, low, high } => {
                let lp = pole.lowpass_response(freq, sr);
                (low * lp.0 + high * (1.0 - lp.0), low * lp.1 - high * lp.1)
            }
        }
    }

    /// Gain in dB at `freq`.
    pub fn magnitude_at(&self, freq: f64) -> f64 {
        power_db(self.response(freq))
    }
}

/// Gain in dB at `freq` of several bands in series.
pub fn magnitude_at(bands: &[BandResponse], freq: f64) -> f64 {
    power_db(bands.iter().fold((1.0, 0.0), |acc, b| cmul(acc, b.response(freq))))
}

/// Gain in dB at `freq` of SVF sections in series (multimode filters).
pub fn svf_cascade_db(sections: &[SvfCoeffs], freq: f64, sample_rate: f64) -> f64 {
    power_db(sections.iter().fold((1.0, 0.0), |acc, s| cmul(acc, s.response(freq, sample_rate))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biquad::{BiquadState, analog_magnitude_db};
    use crate::svf::SvfState;
    use crate::svf::tests::measure_gain_db;

    const SR: f64 = 48_000.0;

    #[test]
    fn matched_bell_is_the_matched_biquad() {
        let b = BandResponse::design(BandKind::Bell, 1000.0, 6.0, 1.2, 4, true, SR);
        let c = BiquadCoeffs::matched(BiquadShape::Bell, 1000.0, 1.2, 6.0, SR);
        for f in [50.0, 500.0, 1000.0, 2500.0, 12_000.0] {
            assert!((b.magnitude_at(f) - c.magnitude_db(f, SR)).abs() < 1e-5);
            let analog = analog_magnitude_db(BiquadShape::Bell, f, 1000.0, 1.2, 6.0);
            assert!((b.magnitude_at(f) - analog).abs() < 0.35, "{f} Hz: {} vs analogue {analog}", b.magnitude_at(f));
        }
        assert!((b.magnitude_at(1000.0) - 6.0).abs() < 0.05);
    }

    #[test]
    fn curve_matches_the_running_filters() {
        // A matched shelf through the biquad the audio path runs.
        let shelf = BandResponse::design(BandKind::HighShelf, 4000.0, -9.0, 0.707, 4, true, SR);
        let c: crate::biquad::Biquad32 = BiquadCoeffs::matched(BiquadShape::HighShelf, 4000.0, 0.707, -9.0, SR).into();
        // A bell on the SVF (what dynamic bands and Classic mode run).
        let bell = BandResponse::design(BandKind::Bell, 700.0, 8.0, 2.0, 4, false, SR);
        let sc = SvfCoeffs::new(SvfMode::Bell, 700.0, 2.0, 8.0, SR);
        for f in [100.0, 700.0, 2000.0, 4000.0, 9000.0, 16_000.0] {
            let mut st = BiquadState::default();
            let m = measure_gain_db(|x| st.process(&c, x), f, SR);
            assert!((shelf.magnitude_at(f) - m).abs() < 0.05, "shelf at {f} Hz: {} vs measured {m}", shelf.magnitude_at(f));
            let mut st = SvfState::default();
            let m = measure_gain_db(|x| st.process(&sc, x), f, SR);
            assert!((bell.magnitude_at(f) - m).abs() < 0.05, "bell at {f} Hz: {} vs measured {m}", bell.magnitude_at(f));
        }
    }

    #[test]
    fn odd_order_cut_matches_the_cascade() {
        // 18 dB/oct low cut: one SVF pair plus a first-order section.
        let f0 = 200.0;
        let cut = BandResponse::design(BandKind::LowCut, f0, 0.0, 0.707, 3, true, SR);
        let pair = SvfCoeffs::new(SvfMode::Highpass, f0, cascade_q(3, 0), 0.0, SR);
        for f in [50.0, 100.0, 200.0, 400.0, 3000.0] {
            let mut st = SvfState::default();
            let mut pole = OnePole::new(f0, SR);
            let m = measure_gain_db(|x| pole.highpass(st.process(&pair, x)), f, SR);
            assert!((cut.magnitude_at(f) - m).abs() < 0.05, "{f} Hz: {} vs measured {m}", cut.magnitude_at(f));
        }
        // Butterworth: −3 dB at the corner, 18 dB per octave below it.
        assert!((cut.magnitude_at(f0 as f64) + 3.01).abs() < 0.05);
        assert!((cut.magnitude_at(50.0) + 36.0).abs() < 0.5);
    }

    #[test]
    fn cuts_follow_their_slope() {
        for (order, per_oct) in [(1, 6.0), (2, 12.0), (4, 24.0), (8, 48.0), (16, 96.0)] {
            let cut = BandResponse::design(BandKind::HighCut, 1000.0, 0.0, 1.0, order, true, SR);
            assert!((cut.magnitude_at(1000.0) + 3.01).abs() < 0.06, "order {order} corner {}", cut.magnitude_at(1000.0));
            // Two octaves up (bilinear warping steepens it a little).
            let two = cut.magnitude_at(4000.0);
            assert!(two < -2.0 * per_oct + 1.0 && two > -2.0 * per_oct * 1.12 - 1.0, "order {order}: {two} dB at +2 oct");
            assert!(cut.magnitude_at(100.0).abs() < 0.05);
        }
    }

    #[test]
    fn tilt_pivots_and_series_bands_add() {
        let tilt = BandResponse::design(BandKind::Tilt, 1000.0, 6.0, 1.0, 4, true, SR);
        assert!((tilt.magnitude_at(20.0) + 3.0).abs() < 0.05);
        assert!((tilt.magnitude_at(20_000.0) - 3.0).abs() < 0.05);
        let bell = BandResponse::design(BandKind::Bell, 300.0, -4.0, 1.0, 4, true, SR);
        for f in [60.0, 300.0, 5000.0] {
            let sum = magnitude_at(&[tilt, bell], f);
            assert!((sum - tilt.magnitude_at(f) - bell.magnitude_at(f)).abs() < 1e-9);
        }
        assert_eq!(magnitude_at(&[], 1000.0), 0.0);
    }

    #[test]
    fn kinds_map_to_the_eq_types() {
        for (i, k) in BandKind::ALL.iter().enumerate() {
            assert_eq!(BandKind::from_index(i), *k);
            assert_eq!(k.index(), i);
        }
        assert!(BandKind::Bell.uses_gain() && BandKind::Bell.uses_q());
        assert!(BandKind::LowCut.uses_slope() && !BandKind::LowCut.uses_q());
        assert!(!BandKind::Notch.uses_gain());
    }

    #[test]
    fn svf_cascade_multiplies_sections() {
        let a = SvfCoeffs::new(SvfMode::Lowpass, 800.0, 0.54, 0.0, SR);
        let b = SvfCoeffs::new(SvfMode::Lowpass, 800.0, 1.31, 0.0, SR);
        let f = 1600.0;
        assert!((svf_cascade_db(&[a, b], f, SR) - a.magnitude_db(f, SR) - b.magnitude_db(f, SR)).abs() < 1e-9);
    }
}
