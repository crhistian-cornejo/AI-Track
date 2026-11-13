use super::*;
use crate::test_util::{BLOCK, SR, process_stereo, rms, set_plain, sine};
use aitrack_dsp::{AudioBuffer, Transport};
use aitrack_dsp::biquad::analog_magnitude_db;

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

#[test]
fn matched_bell_follows_analog() {
    for &(f0, g, q) in &[(1000.0f32, 6.0f32, 1.0f32), (16_000.0, 9.0, 1.2)] {
        let mut eq = Eq::new();
        band(&mut eq, 2, ty::BELL, f0, g, q);
        for f in [200.0, f0 * 0.7, f0, (f0 * 1.25).min(21_000.0), 21_000.0] {
            let m = gain_at(&mut eq, f);
            let a = analog_magnitude_db(BiquadShape::Bell, f as f64, f0 as f64, q as f64, g as f64) as f32;
            assert!((m - a).abs() < 1.0, "bell {f0}: at {f} measured {m} analog {a}");
        }
    }
}

#[test]
fn shelves_and_cuts() {
    let mut eq = Eq::new();
    band(&mut eq, 1, ty::LOW_SHELF, 200.0, -6.0, 0.707);
    assert!((gain_at(&mut eq, 30.0) + 6.0).abs() < 0.3);
    assert!(gain_at(&mut eq, 5000.0).abs() < 0.1);

    let mut eq = Eq::new();
    band(&mut eq, 0, ty::LOW_CUT, 100.0, 0.0, 0.707);
    set_plain(&mut eq, p::band(0, p::SLOPE), slope::DB12);
    assert!((gain_at(&mut eq, 100.0) + 3.01).abs() < 0.2);

    // 96 dB/oct: order-16 Butterworth.
    let mut eq = Eq::new();
    band(&mut eq, 7, ty::HIGH_CUT, 1000.0, 0.0, 0.707);
    set_plain(&mut eq, p::band(7, p::SLOPE), slope::DB96);
    assert!((gain_at(&mut eq, 1000.0) + 3.01).abs() < 0.3);
    let at = gain_at(&mut eq, 1400.0);
    let want = -10.0 * (1.0 + 1.4f32.powi(32)).log10();
    assert!((at - want).abs() < 1.5, "96 dB/oct at 1.4 kHz: {at} vs {want}");
    assert!(gain_at(&mut eq, 500.0).abs() < 0.1);

    // 18 dB/oct uses the first-order section too.
    let mut eq = Eq::new();
    band(&mut eq, 0, ty::LOW_CUT, 200.0, 0.0, 0.707);
    set_plain(&mut eq, p::band(0, p::SLOPE), slope::DB18);
    assert!((gain_at(&mut eq, 200.0) + 3.01).abs() < 0.3);
    let at = gain_at(&mut eq, 100.0);
    assert!((at + 18.1).abs() < 0.6, "{at}");
}

#[test]
fn curve_matches_measurement() {
    let mut eq = Eq::new();
    band(&mut eq, 3, ty::BELL, 800.0, -9.0, 2.0);
    band(&mut eq, 6, ty::TILT, 1000.0, 6.0, 0.7);
    process_stereo(&mut eq, |_| (0.0, 0.0), 4096);
    for i in [10usize, 50, 64, 100] {
        let f = Eq::curve_frequency(i);
        let c = eq.curve()[i];
        let m = gain_at(&mut eq, f);
        assert!((c - m).abs() < 0.2, "point {i} ({f} Hz): curve {c} measured {m}");
    }
}

#[test]
fn side_band_leaves_mono_untouched() {
    let mut eq = Eq::new();
    band(&mut eq, 3, ty::BELL, 1000.0, 12.0, 1.0);
    set_plain(&mut eq, p::band(3, p::CHANNEL), ch::SIDE);
    let (l, r) = process_stereo(&mut eq, sine(1000.0, 0.3), 9600);
    let x: Vec<f32> = (0..9600).map(|i| sine(1000.0, 0.3)(i).0).collect();
    for i in 4800..9600 {
        assert!((l[i] - x[i]).abs() < 1e-4 && (r[i] - x[i]).abs() < 1e-4);
    }
    // Anti-phase content is the side: it gets the boost.
    let (l, _) = process_stereo(&mut eq, |i| {
        let (a, _) = sine(1000.0, 0.1)(i);
        (a, -a)
    }, 9600);
    assert!(20.0 * (rms(&l[4800..]) / (0.1 / 2f32.sqrt())).log10() > 11.0);
}

#[test]
fn dynamic_band_compresses_loud_input() {
    let mut eq = Eq::new();
    band(&mut eq, 3, ty::BELL, 1000.0, 0.0, 1.0);
    set_plain(&mut eq, p::band(3, p::DYNAMIC), 1.0);
    set_plain(&mut eq, p::band(3, p::THRESHOLD), -30.0);
    set_plain(&mut eq, p::band(3, p::RATIO), 4.0);
    set_plain(&mut eq, p::band(3, p::ATTACK), 1.0);
    let loud = gain_at(&mut eq, 1000.0); // −12 dBFS sine: 18 dB over
    assert!(loud < -6.0, "loud {loud}");
    let mut eq2 = Eq::new();
    band(&mut eq2, 3, ty::BELL, 1000.0, 0.0, 1.0);
    set_plain(&mut eq2, p::band(3, p::DYNAMIC), 1.0);
    set_plain(&mut eq2, p::band(3, p::THRESHOLD), -6.0);
    assert!(gain_at(&mut eq2, 1000.0).abs() < 0.2);
    let mut m = vec![0.0; 400];
    eq.meter(&mut m);
    assert!(m[256 + 3] < -6.0);
}

#[test]
fn auto_gain_compensates() {
    let mut eq = Eq::new();
    band(&mut eq, 4, ty::BELL, 2000.0, 12.0, 0.7);
    set_plain(&mut eq, p::AUTO_GAIN, 1.0);
    process_stereo(&mut eq, |_| (0.0, 0.0), 1024);
    let mut m = vec![0.0; 400];
    assert_eq!(eq.meter(&mut m), meter::LEN);
    assert!(m[264] < -1.0, "auto gain {}", m[264]);
    // Curve peak is the boost.
    assert!((m[..128].iter().cloned().fold(f32::MIN, f32::max) - 12.0).abs() < 0.5);
}

#[test]
fn spectrum_shows_signal() {
    let mut eq = Eq::new();
    process_stereo(&mut eq, sine(1000.0, 0.5), 16_384);
    let mut m = vec![0.0; 400];
    eq.meter(&mut m);
    let spec = &m[128..256];
    let (i, _) = spec.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
    assert!((Eq::curve_frequency(i) / 1000.0 - 1.0).abs() < 0.1);
}

/// `gain_at` for the FIR modes: skip the latency before measuring.
fn gain_at_late(eq: &mut Eq, f: f32) -> f32 {
    let n = 36_000;
    let (l, _) = process_stereo(eq, sine(f, 0.25), n);
    let x: Vec<f32> = (0..n).map(|i| sine(f, 0.25)(i).0).collect();
    20.0 * (rms(&l[n / 2..]) / rms(&x[n / 2..])).log10()
}

fn impulse_response(eq: &mut Eq, len: usize) -> Vec<f32> {
    process_stereo(eq, |i| if i == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, len).0
}

#[test]
fn latency_follows_the_phase_mode() {
    let mut eq = Eq::new();
    assert_eq!(eq.latency(), 0);
    set_plain(&mut eq, p::PHASE, phase::NATURAL);
    assert_eq!(eq.latency(), 512);
    set_plain(&mut eq, p::PHASE, phase::LINEAR);
    assert_eq!(eq.latency(), 2304);
    eq.prepare(96_000.0, 512);
    assert_eq!(eq.latency(), 256 + 4096);
    assert_eq!(mode_latency(1, 96_000.0), 256 + 512);
    // A flat linear-phase EQ is a pure delay of exactly the reported latency.
    let ir = impulse_response(&mut eq, 4096);
    let (imax, v) = ir.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap();
    assert_eq!(imax, 2304);
    assert!((v - 1.0).abs() < 1e-3, "{v}");
    let rest: f32 = ir.iter().enumerate().filter(|(i, _)| *i != 2304).map(|(_, x)| x.abs()).sum();
    assert!(rest < 1e-3, "{rest}");
}

#[test]
fn linear_phase_is_symmetric_and_matches_the_analogue_magnitude() {
    let mut eq = Eq::new();
    set_plain(&mut eq, p::PHASE, phase::LINEAR);
    band(&mut eq, 2, ty::BELL, 1000.0, 9.0, 1.0);
    band(&mut eq, 6, ty::HIGH_SHELF, 12_000.0, -6.0, 0.707);
    let lat = eq.latency() as usize;
    let ir = impulse_response(&mut eq, 2 * lat + 256);
    let peak = ir.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    for k in 1..1500 {
        assert!((ir[lat + k] - ir[lat - k]).abs() < 1e-4 * peak.max(1.0), "asymmetric at ±{k}: {} vs {}", ir[lat + k], ir[lat - k]);
    }
    for f in [200.0f32, 700.0, 1000.0, 1400.0, 5000.0, 12_000.0, 20_000.0] {
        let m = gain_at_late(&mut eq, f);
        let a = (analog::band_db(0, f as f64, 1000.0, 9.0, 1.0, 2) + analog::band_db(2, f as f64, 12_000.0, -6.0, 0.707, 2)) as f32;
        assert!((m - a).abs() < 0.25, "at {f}: measured {m} analog {a}");
    }
}

#[test]
fn natural_phase_has_the_analogue_phase() {
    let mut eq = Eq::new();
    set_plain(&mut eq, p::PHASE, phase::NATURAL);
    band(&mut eq, 3, ty::BELL, 8000.0, 9.0, 2.0);
    band(&mut eq, 0, ty::LOW_CUT, 60.0, 0.0, 0.707);
    let lat = eq.latency() as f64;
    let ir = impulse_response(&mut eq, 8192);
    for f in [300.0f64, 2000.0, 6000.0, 8000.0, 12_000.0, 18_000.0] {
        // DFT of the IR at f with the bulk delay taken out.
        let w = std::f64::consts::TAU * f / SR;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (t, h) in ir.iter().enumerate() {
            let ph = -w * (t as f64 - lat);
            re += *h as f64 * ph.cos();
            im += *h as f64 * ph.sin();
        }
        let want = cmul(analog::band_response(0, f, 8000.0, 9.0, 2.0, 2), analog::band_response(3, f, 60.0, 0.0, 0.707, 4));
        let mag = |c: (f64, f64)| 10.0 * (c.0 * c.0 + c.1 * c.1).log10();
        let dphase = (im.atan2(re) - want.1.atan2(want.0)).to_degrees();
        let dphase = (dphase + 540.0) % 360.0 - 180.0;
        assert!((mag((re, im)) - mag(want)).abs() < 0.2, "at {f}: {} vs {}", mag((re, im)), mag(want));
        assert!(dphase.abs() < 2.0, "at {f}: phase off by {dphase}°");
    }
}

#[test]
fn fir_modes_route_mid_side_exactly() {
    for mode in [phase::NATURAL, phase::LINEAR] {
        let mut eq = Eq::new();
        set_plain(&mut eq, p::PHASE, mode);
        band(&mut eq, 3, ty::BELL, 1000.0, 12.0, 1.0);
        set_plain(&mut eq, p::band(3, p::CHANNEL), ch::SIDE);
        let lat = eq.latency() as usize;
        let n = 12_000;
        let (l, r) = process_stereo(&mut eq, sine(1000.0, 0.3), n);
        for i in lat + 4096..n {
            let x = sine(1000.0, 0.3)(i - lat).0;
            assert!((l[i] - x).abs() < 2e-3 && (r[i] - x).abs() < 2e-3, "mode {mode} at {i}: {} {} vs {x}", l[i], r[i]);
        }
        // Anti-phase content is the side: it gets the boost.
        let (l, _) = process_stereo(&mut eq, |i| {
            let (a, _) = sine(1000.0, 0.1)(i);
            (a, -a)
        }, n);
        assert!(20.0 * (rms(&l[n / 2..]) / (0.1 / 2f32.sqrt())).log10() > 11.5);
    }
}

#[test]
fn moving_a_band_in_linear_mode_does_not_click() {
    let mut eq = Eq::new();
    set_plain(&mut eq, p::PHASE, phase::LINEAR);
    band(&mut eq, 2, ty::BELL, 500.0, 0.0, 1.0);
    eq.prepare(SR, BLOCK);
    let mut buf = AudioBuffer::new(2, BLOCK);
    let tr = Transport::default();
    let mut src = sine(500.0, 0.25);
    let (mut prev, mut worst, mut pos) = (0.0f32, 0.0f32, 0usize);
    for blk in 0..400 {
        if (100..300).contains(&blk) {
            // A gain sweep, one step per block, like a mouse drag.
            set_plain(&mut eq, p::band(2, p::GAIN), (blk - 100) as f32 * 0.06);
        }
        for i in 0..BLOCK {
            let (a, b) = src(pos + i);
            buf.channel_mut(0)[i] = a;
            buf.channel_mut(1)[i] = b;
        }
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        eq.process(&mut ctx);
        for &y in buf.channel(0) {
            if pos > 3000 {
                worst = worst.max((y - prev).abs());
            }
            prev = y;
            pos += 1;
        }
        let _ = blk;
    }
    // A 500 Hz sine at up to +12 dB moves at most ≈ 0.26 per sample.
    assert!(worst < 0.3, "largest step {worst}");
    let mut m = vec![0.0; 400];
    eq.meter(&mut m);
    assert!((m[meter::CURVE..meter::SPECTRUM].iter().cloned().fold(f32::MIN, f32::max) - 11.94).abs() < 0.3);
}

#[test]
fn dynamic_directions() {
    // Cut above: classic downward compression of the band.
    assert!((dynamic_gain(0, -10.0, -30.0, 4.0, 0.0, 24.0) + 15.0).abs() < 1e-4);
    assert_eq!(dynamic_gain(0, -40.0, -30.0, 4.0, 0.0, 24.0), 0.0);
    // Boost above: the same amount, upwards.
    assert!((dynamic_gain(1, -10.0, -30.0, 4.0, 0.0, 24.0) - 15.0).abs() < 1e-4);
    // Below: acts on the distance under the threshold.
    assert!((dynamic_gain(2, -50.0, -30.0, 2.0, 0.0, 24.0) + 10.0).abs() < 1e-4);
    assert!((dynamic_gain(3, -50.0, -30.0, 2.0, 0.0, 24.0) - 10.0).abs() < 1e-4);
    assert_eq!(dynamic_gain(3, -20.0, -30.0, 2.0, 0.0, 24.0), 0.0);
    // Range caps it; silence below the threshold gets the full range.
    assert!((dynamic_gain(3, -160.0, -30.0, 4.0, 6.0, 6.0) - 6.0).abs() < 1e-4);
}

#[test]
fn boost_below_lifts_a_quiet_band_only() {
    let make = || {
        let mut eq = Eq::new();
        band(&mut eq, 3, ty::BELL, 1000.0, 0.0, 1.0);
        set_plain(&mut eq, p::band(3, p::DYNAMIC), 1.0);
        set_plain(&mut eq, p::band(3, p::THRESHOLD), -20.0);
        set_plain(&mut eq, p::band(3, p::RATIO), 4.0);
        set_plain(&mut eq, p::band(3, p::ATTACK), 1.0);
        set_plain(&mut eq, p::band(3, p::RELEASE), 20.0);
        set_plain(&mut eq, dynx::band(3, dynx::MODE), dir::BOOST_BELOW);
        set_plain(&mut eq, dynx::band(3, dynx::RANGE), 6.0);
        eq
    };
    let quiet = {
        let mut eq = make();
        let n = 24_000;
        let (l, _) = process_stereo(&mut eq, sine(1000.0, 0.01), n);
        20.0 * (rms(&l[n / 2..]) / (0.01 / 2f32.sqrt())).log10()
    };
    assert!((quiet - 6.0).abs() < 0.5, "quiet {quiet}");
    let mut eq = make();
    assert!(gain_at(&mut eq, 1000.0).abs() < 0.3, "loud passes");
    let mut m = vec![0.0; 400];
    eq.meter(&mut m);
    assert!(m[meter::DYNAMIC + 3].abs() < 0.3);
}

#[test]
fn an_external_sidechain_drives_a_band() {
    let mut eq = Eq::new();
    band(&mut eq, 3, ty::BELL, 1000.0, 0.0, 1.0);
    set_plain(&mut eq, p::band(3, p::DYNAMIC), 1.0);
    set_plain(&mut eq, p::band(3, p::THRESHOLD), -40.0);
    set_plain(&mut eq, p::band(3, p::RATIO), 8.0);
    set_plain(&mut eq, p::band(3, p::ATTACK), 1.0);
    set_plain(&mut eq, dynx::band(3, dynx::SOURCE), 1.0);
    set_plain(&mut eq, dynx::band(3, dynx::RANGE), 12.0);
    set_plain(&mut eq, p::SIDECHAIN, 1.0);
    for mode in [phase::MINIMUM, phase::LINEAR] {
        set_plain(&mut eq, p::PHASE, mode);
        eq.prepare(SR, BLOCK);
        let mut buf = AudioBuffer::new(2, BLOCK);
        let mut sc = AudioBuffer::new(2, BLOCK);
        let tr = Transport::default();
        let mut pos = 0usize;
        let mut run = |eq: &mut Eq, sc_amp: f32| {
            let mut acc = Vec::new();
            for blk in 0..300 {
                for i in 0..BLOCK {
                    let (a, _) = sine(1000.0, 0.05)(pos + i);
                    let (k, _) = sine(1000.0, sc_amp)(pos + i);
                    for c in 0..2 {
                        buf.channel_mut(c)[i] = a;
                        sc.channel_mut(c)[i] = k;
                    }
                }
                pos += BLOCK;
                let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: Some(&sc), events: &[], transport: &tr };
                eq.process(&mut ctx);
                if blk >= 200 {
                    acc.extend_from_slice(buf.channel(0));
                }
            }
            20.0 * (rms(&acc) / (0.05 / 2f32.sqrt())).log10()
        };
        let quiet = run(&mut eq, 0.0);
        assert!(quiet.abs() < 0.3, "mode {mode}: no key, no change ({quiet})");
        let keyed = run(&mut eq, 0.8);
        assert!(keyed < -9.0, "mode {mode}: keyed {keyed}");
        let mut m = vec![0.0; 400];
        assert_eq!(eq.meter(&mut m), meter::LEN);
        assert_eq!(m[meter::SC_ACTIVE], 1.0);
        let (i, _) = m[meter::SC_SPECTRUM..meter::LEN].iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
        assert!((Eq::sc_frequency(i) / 1000.0 - 1.0).abs() < 0.12, "sidechain spectrum peak at {}", Eq::sc_frequency(i));
    }
}

#[test]
fn no_nan_or_denormals_in_any_mode() {
    for mode in [phase::MINIMUM, phase::NATURAL, phase::LINEAR] {
        let mut eq = Eq::new();
        set_plain(&mut eq, p::PHASE, mode);
        for b in 0..BANDS {
            band(&mut eq, b, (b % 8) as f32, 30.0 * 2f32.powi(b as i32), 24.0, 18.0);
            set_plain(&mut eq, p::band(b, p::CHANNEL), (b % 5) as f32);
            set_plain(&mut eq, p::band(b, p::DYNAMIC), 1.0);
            set_plain(&mut eq, dynx::band(b, dynx::MODE), (b % 4) as f32);
        }
        let mut seed = 1u32;
        let (l, r) = process_stereo(&mut eq, |i| {
            if i < 12_000 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let v = (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5;
                (v, -v * 0.5)
            } else {
                (0.0, 0.0)
            }
        }, 60_000);
        for x in l.iter().chain(&r) {
            assert!(x.is_finite(), "mode {mode}: non-finite output");
            assert!(*x == 0.0 || x.abs() > 1e-30, "mode {mode}: denormal {x:e}");
        }
    }
}

/// Release builds only: one instance in its heaviest settings stays well
/// inside a core (`cargo test -p aitrack-stock --release eq::tests::cpu`).
#[test]
#[cfg_attr(debug_assertions, ignore)]
fn cpu_budget_per_instance() {
    for (mode, budget) in [(phase::MINIMUM, 0.03), (phase::NATURAL, 0.05), (phase::LINEAR, 0.05)] {
        let mut eq = Eq::new();
        set_plain(&mut eq, p::PHASE, mode);
        for b in 0..BANDS {
            band(&mut eq, b, ty::BELL, 60.0 * 2f32.powi(b as i32), 3.0, 1.0);
            set_plain(&mut eq, p::band(b, p::DYNAMIC), 1.0);
            set_plain(&mut eq, p::band(b, p::CHANNEL), ch::MID + (b % 2) as f32);
        }
        let seconds = 2.0;
        let n = (seconds * SR) as usize;
        let t = std::time::Instant::now();
        process_stereo(&mut eq, sine(440.0, 0.5), n);
        let share = t.elapsed().as_secs_f64() / seconds;
        assert!(share < budget, "mode {mode}: {:.2} % of a core", share * 100.0);
    }
}
