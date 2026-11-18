//! Matched processing (no cramping near Nyquist) for every band type, the
//! display helper, and automation robustness.

use super::*;
use crate::test_util::{BLOCK, SR, process_stereo, rms, set_plain, sine};
use aitrack_dsp::{AudioBuffer, Transport};

fn band(eq: &mut Eq, b: usize, t: f32, f: f32, g: f32, q: f32) {
    set_plain(eq, p::band(b, p::ON), 1.0);
    set_plain(eq, p::band(b, p::TYPE), t);
    set_plain(eq, p::band(b, p::FREQ), f);
    set_plain(eq, p::band(b, p::GAIN), g);
    set_plain(eq, p::band(b, p::Q), q);
}

fn gain_at(eq: &mut Eq, f: f32) -> f32 {
    let n = 24_000;
    let (l, _) = process_stereo(eq, sine(f, 0.25), n);
    let x: Vec<f32> = (0..n).map(|i| sine(f, 0.25)(i).0).collect();
    20.0 * (rms(&l[n / 2..]) / rms(&x[n / 2..])).log10()
}

/// Analogue magnitude (dB) of band type `t` at `f`, from the prototypes the
/// FIR modes use.
fn analog_db(t: f32, f: f64, f0: f64, g: f64, q: f64, order: usize) -> f64 {
    analog::band_db(t as usize, f, f0, g, q, order)
}

/// The EQ's curve against the analogue prototype at every meter point where
/// the analogue gain is above -40 dB.
fn curve_error(eq: &Eq, t: f32, f0: f64, g: f64, q: f64, order: usize) -> f64 {
    let mut worst = 0.0f64;
    for i in 0..CURVE_POINTS {
        let f = curve_freq(i);
        let a = analog_db(t, f, f0, g, q, order);
        if a > -40.0 {
            worst = worst.max((eq.curve()[i] as f64 - a).abs());
        }
    }
    worst
}

/// Report 24/01 section 2.2: the dynamic bell, the cuts and the tilt fell back
/// on the bilinear SVF (4.5 dB off for a bell at 12 kHz, 13.5 dB for a
/// 12 dB/oct low-pass at 20 kHz). In Matched processing they follow the
/// analogue curve like the static bells always did, and the audio agrees.
#[test]
fn matched_mode_follows_the_analogue_curve_for_every_band_type() {
    // (type, f0, gain, q, slope, poles, allowed error in dB)
    let cases = [
        (ty::BELL, 12_000.0, 12.0, 1.0, slope::DB24, 4, 0.8),
        (ty::BELL, 8_000.0, 12.0, 1.0, slope::DB24, 4, 0.55),
        (ty::LOW_SHELF, 8_000.0, 9.0, 0.707, slope::DB24, 4, 0.5),
        (ty::HIGH_SHELF, 12_000.0, 6.0, 0.707, slope::DB24, 4, 0.3),
        (ty::HIGH_CUT, 12_000.0, 0.0, 0.707, slope::DB12, 2, 0.9),
        (ty::HIGH_CUT, 16_000.0, 0.0, 0.707, slope::DB12, 2, 0.6),
        (ty::HIGH_CUT, 12_000.0, 0.0, 0.707, slope::DB18, 3, 0.9),
        (ty::LOW_CUT, 12_000.0, 0.0, 0.707, slope::DB24, 4, 0.2),
        (ty::NOTCH, 10_000.0, 0.0, 2.0, slope::DB24, 4, 0.3),
        (ty::TILT, 12_000.0, 6.0, 0.7, slope::DB24, 4, 0.5),
    ];
    for dynamic in [false, true] {
        for &(t, f0, g, q, sl, order, max) in &cases {
            // Only bells and shelves can be dynamic; a silent input leaves
            // them at their static curve.
            if dynamic && !(t == ty::BELL || t == ty::LOW_SHELF || t == ty::HIGH_SHELF) {
                continue;
            }
            let mut eq = Eq::new();
            band(&mut eq, 3, t, f0 as f32, g as f32, q as f32);
            set_plain(&mut eq, p::band(3, p::SLOPE), sl);
            if dynamic {
                set_plain(&mut eq, p::band(3, p::DYNAMIC), 1.0);
                set_plain(&mut eq, p::band(3, p::THRESHOLD), -6.0);
            }
            process_stereo(&mut eq, |_| (0.0, 0.0), 4096);
            let err = curve_error(&eq, t, f0, g, q, order);
            assert!(err < max, "type {t} at {f0} Hz {g} dB (dynamic {dynamic}): curve is {err:.2} dB off the analogue");
            // The audio follows the curve.
            let f = if t == ty::LOW_CUT { 14_000.0 } else { 18_000.0 };
            let a = analog_db(t, f, f0, g, q, order);
            if a > -30.0 {
                let m = gain_at(&mut eq, f as f32) as f64;
                assert!((m - a).abs() < max + 0.15, "type {t} at {f0} Hz: measured {m:.2} dB at {f} Hz, analogue {a:.2}");
            }
        }
    }
}

/// Classic mode is still the plain bilinear SVF: the 12 kHz bell is cramped
/// by several dB (the choice stays available).
#[test]
fn classic_mode_keeps_the_bilinear_curve() {
    let mut eq = Eq::new();
    set_plain(&mut eq, p::MODE, 1.0);
    band(&mut eq, 3, ty::BELL, 12_000.0, 12.0, 1.0);
    process_stereo(&mut eq, |_| (0.0, 0.0), 4096);
    assert!(curve_error(&eq, ty::BELL, 12_000.0, 12.0, 1.0, 4) > 3.0);
}

/// The display (the app draws it with `BandResponse`) is the filter that runs.
#[test]
fn response_helper_matches_the_running_bands() {
    use aitrack_dsp::response::{BandKind, BandResponse, magnitude_at};
    let mut eq = Eq::new();
    band(&mut eq, 1, ty::LOW_CUT, 90.0, 0.0, 0.707);
    set_plain(&mut eq, p::band(1, p::SLOPE), slope::DB18);
    band(&mut eq, 2, ty::BELL, 3_300.0, -7.0, 2.5);
    band(&mut eq, 4, ty::HIGH_SHELF, 11_000.0, 8.0, 0.9);
    band(&mut eq, 5, ty::TILT, 900.0, 4.0, 0.7);
    band(&mut eq, 6, ty::HIGH_CUT, 15_000.0, 0.0, 0.707);
    set_plain(&mut eq, p::band(6, p::SLOPE), slope::DB12);
    process_stereo(&mut eq, |_| (0.0, 0.0), 4096);
    let bands = [
        BandResponse::design(BandKind::LowCut, 90.0, 0.0, 0.707, 3, true, SR),
        BandResponse::design(BandKind::Bell, 3_300.0, -7.0, 2.5, 4, true, SR),
        BandResponse::design(BandKind::HighShelf, 11_000.0, 8.0, 0.9, 4, true, SR),
        BandResponse::design(BandKind::Tilt, 900.0, 4.0, 0.7, 4, true, SR),
        BandResponse::design(BandKind::HighCut, 15_000.0, 0.0, 0.707, 2, true, SR),
    ];
    for i in (0..CURVE_POINTS).step_by(7) {
        let f = curve_freq(i);
        let helper = magnitude_at(&bands, f);
        assert!((helper - eq.curve()[i] as f64).abs() < 0.05, "{f} Hz: helper {helper} curve {}", eq.curve()[i]);
    }
}

/// Fast automation: every block (128 samples, eight coefficient refreshes)
/// the frequency jumps along a sweep and the gain swings across +-12 dB.
/// Every band type, both processing modes: finite output, no step beyond what
/// the signal itself can do.
#[test]
fn automation_sweeps_do_not_zipper_or_blow_up() {
    for mode in [0.0f32, 1.0] {
        for t in [ty::BELL, ty::LOW_SHELF, ty::HIGH_SHELF, ty::LOW_CUT, ty::HIGH_CUT, ty::NOTCH, ty::BAND_PASS, ty::TILT] {
            let mut eq = Eq::new();
            set_plain(&mut eq, p::MODE, mode);
            band(&mut eq, 2, t, 200.0, 0.0, 4.0);
            set_plain(&mut eq, p::band(2, p::SLOPE), slope::DB48);
            eq.prepare(SR, BLOCK);
            let mut buf = AudioBuffer::new(2, BLOCK);
            let tr = Transport::default();
            let (mut prev, mut worst, mut peak, mut pos) = (0.0f32, 0.0f32, 0.0f32, 0usize);
            for blk in 0..600 {
                let f = 60.0 * 250f32.powf(0.5 + 0.5 * (blk as f32 * 0.21).sin());
                let g = 12.0 * (blk as f32 * 0.37).sin();
                set_plain(&mut eq, p::band(2, p::FREQ), f);
                set_plain(&mut eq, p::band(2, p::GAIN), g);
                for i in 0..BLOCK {
                    let v = 0.3 * (std::f64::consts::TAU * 100.0 * (pos + i) as f64 / SR).sin() as f32;
                    buf.channel_mut(0)[i] = v;
                    buf.channel_mut(1)[i] = v;
                }
                let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
                eq.process(&mut ctx);
                for &y in buf.channel(0) {
                    assert!(y.is_finite(), "type {t} mode {mode}: non-finite at {pos}");
                    if pos > 4000 {
                        worst = worst.max((y - prev).abs());
                    }
                    peak = peak.max(y.abs());
                    prev = y;
                    pos += 1;
                }
            }
            // A 100 Hz sine of 0.3 at up to +12 dB (x4, or the band-pass at
            // unity) moves 0.3 * 4 * 2 pi 100 / 48000 = 0.016 per sample at
            // most; a zipper from a bad coefficient switch is a spike far above.
            assert!(peak < 3.0, "type {t} mode {mode}: peak {peak}");
            assert!(worst < 0.06, "type {t} mode {mode}: largest step {worst}");
        }
    }
}
