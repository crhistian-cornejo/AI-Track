//! Measured quality of the delay: interpolation while the time moves,
//! pitch excursion of a time change, filters on the first echo, DC and
//! high-feedback behaviour.
//!
//! "Before" numbers in the comments are from the cubic-Hermite reads, the
//! exponential time glide and the feedback-only filters this delay had.

use super::*;
use crate::measure::*;
use crate::test_util::{SR, process_stereo, rms, set_plain};
use aitrack_dsp::delay::{AllpassReader, DelayLine};
use std::f64::consts::TAU;

/// Spurious power (dB re the tone, outside ±`excl` Hz of it) of a tone read
/// through `read` while the delay time swings sinusoidally by ±`a` samples.
fn swept_tone_db(freq: f64, a: f64, fm: f64, excl: f64, read: &mut dyn FnMut(&DelayLine, f32) -> f32) -> f32 {
    let mut line = DelayLine::new(8000);
    let mut out = Vec::new();
    for i in 0..96_000 {
        let t = i as f64 / SR;
        line.push((0.5 * (TAU * freq * t).sin()) as f32);
        let d = 3000.0 + a * (TAU * fm * t).sin();
        if i > 6000 {
            out.push(read(&line, d as f32));
        }
    }
    spurious_db(&out[..65_536], freq, excl)
}

#[test]
fn the_sinc_read_leaves_far_fewer_images_than_hermite() {
    // Delay-sized sweep (±200 samples at 0.5 Hz). Hermite before:
    // −51 dB at 5 kHz, −32 dB at 10 kHz, −22 dB at 15 kHz.
    let kernel = SincKernel::new();
    for (f, hermite_floor, sinc_ceiling) in [(5_000.0, -60.0, -65.0), (10_000.0, -40.0, -65.0), (15_000.0, -30.0, -35.0)] {
        let h = swept_tone_db(f, 200.0, 0.5, 400.0, &mut |l, d| l.read_cubic(d));
        let s = swept_tone_db(f, 200.0, 0.5, 400.0, &mut |l, d| kernel.read(l, d));
        assert!(h > hermite_floor, "{f} Hz: Hermite {h} dB is cleaner than the measurements say");
        assert!(s < sinc_ceiling, "{f} Hz: sinc {s} dB");
    }
}

#[test]
fn the_tank_lines_read_clean_while_they_modulate() {
    // The reverb's lines move ±24 samples at 0.6 Hz. The all-pass reader
    // they used before clicked each time its fraction wrapped: −52 dB at
    // 500 Hz, −40 dB at 2 kHz, −32 dB at 5 kHz.
    let kernel = SincKernel::with_cutoff(1.0);
    for f in [500.0, 2_000.0, 5_000.0, 10_000.0] {
        let mut ap = AllpassReader::default();
        let old = swept_tone_db(f, 24.0, 0.6, 400.0, &mut |l, d| ap.read(l, d));
        let new = swept_tone_db(f, 24.0, 0.6, 400.0, &mut |l, d| kernel.read(l, d));
        assert!(new < -78.0 && new < old - 25.0, "{f} Hz: sinc {new} dB, all-pass {old} dB");
    }
}

fn base(engine: f32, ms: f32, fb: f32) -> Delay {
    let mut d = Delay::new();
    set_plain(&mut d, p::MIX, 100.0);
    set_plain(&mut d, p::LOW_CUT, 20.0);
    set_plain(&mut d, p::HIGH_CUT, 20_000.0);
    set_plain(&mut d, p::ENGINE, engine);
    set_plain(&mut d, p::SYNC, 0.0);
    set_plain(&mut d, p::TIME_L, ms);
    set_plain(&mut d, p::TIME_R, ms);
    set_plain(&mut d, p::FEEDBACK, fb);
    d
}

#[test]
fn sweeping_the_time_of_the_plugin_adds_no_aliasing() {
    // Whole plugin, wet only, 100 % time modulation (±144 samples at
    // 0.5 Hz) on a 10 kHz tone: everything but the tone and its close-in
    // vibrato sidebands, in dB below the tone. Hermite reads: about −32.
    // (The tape engine adds its own saturation and wow, so only the clean
    // digital line is held to this.)
    let mut d = base(0.0, 150.0, 0.0);
    set_plain(&mut d, p::MOD_DEPTH, 100.0);
    set_plain(&mut d, p::MOD_RATE, 0.5);
    let freq = 10_000.0;
    let (l, _) = process_stereo(
        &mut d,
        |i| {
            let v = 0.3 * (TAU * freq * i as f64 / SR).sin() as f32;
            (v, v)
        },
        48_000 * 3,
    );
    let spur = spurious_db(&l[30_000..30_000 + 65_536], freq, 1_500.0);
    assert!(spur < -60.0, "{spur} dB");
}

#[test]
fn a_big_time_jump_bends_the_pitch_but_never_plays_backwards() {
    // The exponential glide started a 250 → 500 ms jump at a slope of
    // (12 000 samples × 1.7e-4 =) 2.08 samples per sample: the repeats ran
    // at −1.08× speed, backwards. The limited glide keeps every step inside
    // ±0.5 (±0.35 for tape) and never overshoots.
    for (ms, slope) in [(120.0, 0.5f32), (200.0, 0.35)] {
        for (from, to) in [(12_000.0f32, 24_000.0f32), (24_000.0, 12_000.0), (480.0, 96_000.0)] {
            let mut g = Glide::new(from);
            g.set_rate(ms, slope, SR);
            g.set_target(to);
            let (mut prev, mut steepest, mut samples) = (from, 0.0f32, 0usize);
            while (g.value() - to).abs() > 0.5 && samples < 48_000 * 60 {
                let v = g.next();
                steepest = steepest.max((v - prev).abs());
                assert!((v - to).signum() == (prev - to).signum() || (v - to).abs() < 1.0, "overshoot at {v}");
                prev = v;
                samples += 1;
            }
            assert!(steepest <= slope + 0.005, "{from} → {to}: slope {steepest}");
            assert!(steepest > 0.9 * slope.min(0.5) || (to - from).abs() < 2_000.0, "{from} → {to}: a big jump uses the allowed bend ({steepest})");
            assert!(samples < 48_000 * 8, "{from} → {to}: arrives in {} s", samples as f32 / 48_000.0);
        }
    }
}

#[test]
fn the_plugin_lands_on_the_new_time_after_a_jump() {
    for engine in [0.0, 1.0, 2.0] {
        let mut d = base(engine, 250.0, 0.0);
        set_plain(&mut d, p::AGE, 0.0);
        set_plain(&mut d, p::TIME_L, 250.0);
        d.prepare(SR, 128);
        let tr = aitrack_dsp::Transport::default();
        // Settle, then move to 500 ms and send an impulse once it arrived.
        let _ = crate::test_util::process_stereo_prepared(&mut d, |_| (0.0, 0.0), 24_000, &tr);
        set_plain(&mut d, p::TIME_L, 500.0);
        set_plain(&mut d, p::TIME_R, 500.0);
        let at = 48_000 * 2;
        let (l, _) = crate::test_util::process_stereo_prepared(&mut d, |i| if i == at { (1.0, 1.0) } else { (0.0, 0.0) }, at + 48_000, &tr);
        let hit = l.iter().enumerate().skip(at).max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
        assert!(((hit - at) as i32 - 24_000).abs() < 60, "engine {engine}: echo {} samples after the impulse", hit - at);
    }
}

#[test]
fn the_filters_shape_the_first_echo_not_only_the_repeats() {
    // Share of an echoed impulse's energy above 4 kHz. The filters used to
    // sit in the feedback path only, so the first echo came out flat
    // (≈ 0.58 of an impulse's energy is above 4 kHz).
    let share = |setup: &dyn Fn(&mut Delay)| {
        let mut d = base(0.0, 100.0, 0.0);
        setup(&mut d);
        let (l, _) = process_stereo(&mut d, |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, 9_600);
        let echo = &l[4_700..4_900];
        let hp = SvfCoeffs::new(SvfMode::Highpass, 4_000.0, 0.707, 0.0, SR);
        let mut st = SvfState::default();
        let hi: Vec<f32> = echo.iter().map(|&x| st.process(&hp, x)).collect();
        (rms(&hi) / rms(echo)).powi(2)
    };
    let open = share(&|_| {});
    let dark = share(&|d| set_plain(d, p::HIGH_CUT, 1_000.0));
    assert!(open > 0.4, "open filters leave the echo flat: {open}");
    assert!(dark < 0.02, "a 1 kHz high cut darkens the first echo: {dark}");
    // The low cut too.
    let low = |cut: f32| {
        let mut d = base(0.0, 100.0, 0.0);
        set_plain(&mut d, p::LOW_CUT, cut);
        let (l, _) = process_stereo(&mut d, |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, 9_600);
        let echo = &l[4_700..4_900];
        let lp = SvfCoeffs::new(SvfMode::Lowpass, 300.0, 0.707, 0.0, SR);
        let mut st = SvfState::default();
        let lo: Vec<f32> = echo.iter().map(|&x| st.process(&lp, x)).collect();
        (rms(&lo) / rms(echo)).powi(2)
    };
    assert!(low(1_500.0) < 0.25 * low(20.0), "a 1.5 kHz low cut thins the first echo");
}

#[test]
fn open_filters_leave_the_echo_untouched() {
    let mut d = base(0.0, 100.0, 0.0);
    let mut n = noise(3, 0.3);
    let mut src = Vec::new();
    let (l, _) = process_stereo(
        &mut d,
        |i| {
            let v = n(i);
            src.push(v.0);
            v
        },
        20_000,
    );
    // The windowed-sinc read at a whole-sample delay is a lowpass at 0.96
    // of Nyquist, so the match is close but not bit-exact.
    let err: f32 = (6_000..20_000).map(|i| (l[i] - src[i - 4_800]).powi(2)).sum::<f32>() / (14_000.0 * 0.3 * 0.3 / 3.0);
    assert!(err < 0.01, "relative error power {err}");
}

#[test]
fn the_tape_head_adds_no_offset() {
    // A loud sine at +12 dB drive left the tape echo with a mean of −0.024
    // against an rms of 0.18 (13 %): the asymmetric head rectifies.
    let mut d = base(1.0, 100.0, 40.0);
    set_plain(&mut d, p::DRIVE, 12.0);
    let (l, _) = process_stereo(
        &mut d,
        |i| {
            let v = 0.9 * (TAU * 200.0 * i as f64 / SR).sin() as f32;
            (v, v)
        },
        48_000 * 3,
    );
    let seg = &l[48_000..];
    let mean = seg.iter().sum::<f32>() / seg.len() as f32;
    assert!(mean.abs() < 0.01 * rms(seg), "offset {mean} against rms {}", rms(seg));
}

#[test]
fn a_dc_step_does_not_ring_on() {
    // DC for half a second, then silence, at 95 % feedback: the loop's
    // high-pass takes it out; nothing is left after 10 s.
    for engine in [0.0, 1.0, 2.0] {
        let mut d = base(engine, 100.0, 95.0);
        let (l, _) = process_stereo(&mut d, |i| if i < 24_000 { (0.5, 0.5) } else { (0.0, 0.0) }, 48_000 * 12);
        let tail = &l[48_000 * 10..];
        let mean = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(mean.abs() < 1e-4 && rms(tail) < 1e-3, "engine {engine}: mean {mean}, rms {}", rms(tail));
    }
}

#[test]
fn full_feedback_with_a_loud_input_neither_blows_up_nor_drifts() {
    // 100 % feedback self-sustains by design; it must stay bounded and
    // keep a steady level instead of creeping up or leaning on an offset.
    for engine in [0.0, 1.0, 2.0] {
        let mut d = base(engine, 80.0, 100.0);
        set_plain(&mut d, p::DRIVE, 12.0);
        let (l, _) = process_stereo(&mut d, noise_burst(0.9, 24_000), 48_000 * 20);
        assert!(l.iter().all(|v| v.is_finite()), "engine {engine}");
        let early = rms(&l[48_000 * 4..48_000 * 6]);
        let late = rms(&l[48_000 * 18..]);
        let mean = l[48_000 * 18..].iter().sum::<f32>() / (48_000.0 * 2.0);
        assert!(early < 1.5 && late < 1.5, "engine {engine}: levels {early} {late}");
        assert!(late < 1.5 * early + 0.01, "engine {engine}: creeping {early} → {late}");
        assert!(mean.abs() < 0.02 + 0.05 * late, "engine {engine}: offset {mean}");
    }
}

fn noise_burst(amp: f32, len: usize) -> impl FnMut(usize) -> (f32, f32) {
    let mut n = noise(11, amp);
    move |i| if i < len { n(i) } else { (0.0, 0.0) }
}
