use super::*;
use crate::test_util::{get_plain, peak, rms, set_plain};
use aitrack_dsp::{AudioBuffer, Event, Transport};

type Stereo = (Vec<f32>, Vec<f32>);

/// Render `frames` frames in blocks of `block`. `events` are
/// `(frame, kind)`; `side` is the sidechain signal by frame.
fn render(g: &mut Granular, sr: f64, frames: usize, block: usize, events: &[(usize, EventKind)], side: Option<&dyn Fn(usize) -> f32>, pos0: i64) -> Stereo {
    g.prepare(sr, block);
    render_prepared(g, sr, frames, block, events, side, pos0)
}

fn render_prepared(g: &mut Granular, sr: f64, frames: usize, block: usize, events: &[(usize, EventKind)], side: Option<&dyn Fn(usize) -> f32>, pos0: i64) -> Stereo {
    let mut buf = AudioBuffer::new(2, block);
    let mut sc = AudioBuffer::new(2, block);
    let mut tr = Transport { playing: true, bpm: 120.0, position_samples: pos0, ..Transport::default() };
    let (mut ol, mut or) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    let mut pos = 0;
    let mut evs: Vec<Event> = Vec::new();
    while pos < frames {
        let len = block.min(frames - pos);
        buf.set_frames(len);
        buf.clear();
        sc.set_frames(len);
        for i in 0..len {
            let v = side.map_or(0.0, |f| f(pos + i));
            sc.channel_mut(0)[i] = v;
            sc.channel_mut(1)[i] = v;
        }
        evs.clear();
        for &(f, kind) in events {
            if f >= pos && f < pos + len {
                evs.push(Event { offset: (f - pos) as u32, kind });
            }
        }
        let mut ctx = ProcessContext { sample_rate: sr, buffer: &mut buf, sidechain: side.map(|_| &sc), events: &evs, transport: &tr };
        g.process(&mut ctx);
        ol.extend_from_slice(buf.channel(0));
        or.extend_from_slice(buf.channel(1));
        pos += len;
        tr.position_samples += len as i64;
        tr.position_beats += len as f64 / sr * tr.bpm / 60.0;
    }
    (ol, or)
}

fn on(key: u8) -> EventKind {
    EventKind::NoteOn { channel: 0, key, velocity: 0.9, note_id: key as i32 }
}

fn off(key: u8) -> EventKind {
    EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: key as i32 }
}

fn param(g: &Granular, id: u32, plain: f32) -> EventKind {
    let n = g.info().params.iter().find(|i| i.id == id).unwrap().to_normalized(plain);
    EventKind::Param { id, value: n }
}

fn sine(f: f64, sr: f64, amp: f32, n: usize) -> Vec<f32> {
    (0..n).map(|i| amp * (std::f64::consts::TAU * f * i as f64 / sr).sin() as f32).collect()
}

fn sample_granular(x: &[f32], sr: f64) -> Granular {
    let mut g = Granular::new();
    g.set_sample(x, 1, sr);
    set_plain(&mut g, p::SOURCE, SOURCE_SAMPLE as f32);
    g
}

fn max_step(x: &[f32]) -> f32 {
    x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
}

/// Magnitude of the DFT of `x` at `f` (Goertzel-like, direct).
fn mag_at(x: &[f32], f: f64, sr: f64) -> f32 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &v) in x.iter().enumerate() {
        let a = std::f64::consts::TAU * f * i as f64 / sr;
        re += v as f64 * a.cos();
        im += v as f64 * a.sin();
    }
    (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
}

fn quiet_cloud(g: &mut Granular) {
    // Deterministic, coherent grains: no spray, jitter, pan or envelope shaping.
    set_plain(g, p::SPRAY, 0.0);
    set_plain(g, p::JITTER, 0.0);
    set_plain(g, p::PAN_SPREAD, 0.0);
    set_plain(g, p::ATTACK, 0.5);
    set_plain(g, p::RELEASE, 5.0);
}

#[test]
fn plays_out_of_the_box_and_is_silent_without_notes() {
    let mut g = Granular::new();
    let (l, _) = render(&mut g, 48_000.0, 24_000, 128, &[], None, 0);
    assert_eq!(rms(&l), 0.0);
    let (l, r) = render(&mut g, 48_000.0, 48_000, 128, &[(0, on(60))], None, 0);
    assert!(rms(&l[8000..]) > 0.01 && rms(&r[8000..]) > 0.01, "rms {}", rms(&l));
    assert!(peak(&l) <= 1.0 && l.iter().all(|v| v.is_finite()));
}

#[test]
fn identical_output_for_any_block_size() {
    let mut last = None;
    for sync in [0.0, 14.0] {
        let mut reference: Option<Stereo> = None;
        for block in [1usize, 37, 128, 512, 4096] {
            let mut g = Granular::new();
            set_plain(&mut g, p::SOURCE, SOURCE_BUILTIN as f32);
            set_plain(&mut g, p::BUILTIN, 2.0);
            set_plain(&mut g, p::SPRAY, 60.0);
            set_plain(&mut g, p::PITCH_SPREAD, 7.0);
            set_plain(&mut g, p::PAN_SPREAD, 100.0);
            set_plain(&mut g, p::SIZE_RAND, 50.0);
            set_plain(&mut g, p::DIRECTION, 3.0);
            set_plain(&mut g, p::DENSITY, 60.0);
            set_plain(&mut g, p::SCAN_SPEED, 40.0);
            set_plain(&mut g, p::SCAN_DEPTH, 30.0);
            set_plain(&mut g, p::FILTER_TYPE, 1.0);
            set_plain(&mut g, p::CUTOFF, 3000.0);
            set_plain(&mut g, p::SYNC, sync);
            set_plain(&mut g, p::SEED, 17.0);
            let ev = [(100, on(55)), (3000, on(62)), (9000, off(55)), (14_000, off(62))];
            let out = render(&mut g, 48_000.0, 24_000, block, &ev, None, 777_000);
            match &reference {
                None => reference = Some(out),
                Some(r) => {
                    let d = out.0.iter().zip(&r.0).chain(out.1.iter().zip(&r.1)).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                    assert!(d < 1e-4, "sync {sync} block {block}: differs by {d}");
                }
            }
        }
        last = reference;
    }
    assert!(rms(&last.unwrap().0) > 0.005);
}

#[test]
fn same_seed_and_position_repeat_and_other_seeds_differ() {
    let run = |seed: f32, pos: i64| {
        let mut g = Granular::new();
        set_plain(&mut g, p::SEED, seed);
        set_plain(&mut g, p::SPRAY, 80.0);
        set_plain(&mut g, p::PAN_SPREAD, 100.0);
        render(&mut g, 48_000.0, 12_000, 256, &[(0, on(60))], None, pos).0
    };
    let a = run(3.0, 1000);
    let b = run(3.0, 1000);
    assert_eq!(a, b, "bit-identical for the same seed and position");
    let c = run(4.0, 1000);
    let d = run(3.0, 2000);
    let diff = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(diff(&a, &c) > 0.01, "another seed gives another cloud");
    assert!(diff(&a, &d) > 0.01, "another transport position gives another cloud");
}

#[test]
fn grains_start_and_stop_without_clicks_for_every_window_and_shape() {
    // A constant source makes every discontinuity visible as a step.
    let dc = vec![0.5f32; 48_000];
    for win in 0..window::COUNT {
        for shape in [0.0, 50.0, 100.0] {
            for size in [3.0, 20.0, 200.0] {
                let mut g = sample_granular(&dc, 48_000.0);
                set_plain(&mut g, p::WINDOW, win as f32);
                set_plain(&mut g, p::WIN_SHAPE, shape);
                set_plain(&mut g, p::SIZE, size);
                set_plain(&mut g, p::DENSITY, 30.0);
                set_plain(&mut g, p::PAN_SPREAD, 0.0);
                set_plain(&mut g, p::ATTACK, 2.0);
                set_plain(&mut g, p::RELEASE, 20.0);
                let (l, _) = render(&mut g, 48_000.0, 30_000, 128, &[(0, on(60)), (20_000, off(60))], None, 0);
                assert!(rms(&l[5000..19_000]) > 0.01, "audible");
                let step = max_step(&l);
                // A hard grain edge of a 0.5 source would be a step of ~0.35.
                let bound = if size < 5.0 { 0.08 } else { 0.04 };
                assert!(step < bound, "{} shape {shape} size {size}: step {step}", window::NAMES[win]);
            }
        }
    }
}

#[test]
fn density_and_size_sweeps_do_not_crackle() {
    let dc = vec![0.5f32; 48_000];
    let mut g = sample_granular(&dc, 48_000.0);
    set_plain(&mut g, p::PAN_SPREAD, 0.0);
    set_plain(&mut g, p::ATTACK, 2.0);
    let mut ev = vec![(0, on(60))];
    // Sweep density 4..400 Hz and size 8..600 ms in 64-frame automation steps.
    let steps = 600;
    for k in 0..steps {
        let t = k as f32 / steps as f32;
        ev.push((k * 64, param(&g, p::DENSITY, 4.0 * 100f32.powf(t))));
        ev.push((k * 64, param(&g, p::SIZE, 8.0 * 75f32.powf(1.0 - t))));
    }
    let (l, _) = render(&mut g, 48_000.0, steps * 64, 128, &ev, None, 0);
    assert!(rms(&l[10_000..]) > 0.01);
    let step = max_step(&l[2000..]);
    assert!(step < 0.05, "swept step {step}");
}

#[test]
fn level_stays_within_6_db_across_density_and_size() {
    let mut levels = Vec::new();
    for (dens, size) in [(10.0, 100.0), (20.0, 100.0), (80.0, 100.0), (320.0, 100.0), (50.0, 800.0), (400.0, 800.0)] {
        let mut g = Granular::new();
        set_plain(&mut g, p::BUILTIN, 2.0);
        set_plain(&mut g, p::DENSITY, dens);
        set_plain(&mut g, p::SIZE, size);
        let (l, _) = render(&mut g, 48_000.0, 72_000, 128, &[(0, on(60))], None, 0);
        levels.push(rms(&l[24_000..]));
    }
    let (lo, hi) = levels.iter().fold((f32::MAX, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
    assert!(lo > 0.0 && hi / lo < 2.5, "levels {levels:?}");
}

#[test]
fn density_sets_the_grain_rate() {
    for dens in [10.0, 40.0, 160.0] {
        let mut g = Granular::new();
        set_plain(&mut g, p::DENSITY, dens);
        set_plain(&mut g, p::JITTER, 0.0);
        set_plain(&mut g, p::SIZE, 10.0);
        render(&mut g, 48_000.0, 96_000, 128, &[(0, on(60))], None, 0);
        let n = g.grains_started() as f32;
        assert!((n - 2.0 * dens).abs() <= 2.0 + 0.02 * dens, "density {dens}: {n} grains in 2 s");
    }
}

#[test]
fn sync_places_grains_on_the_transport_grid() {
    let dc = vec![0.5f32; 48_000];
    let mut g = sample_granular(&dc, 48_000.0);
    quiet_cloud(&mut g);
    set_plain(&mut g, p::SYNC, 11.0); // 1/8 -> 0.25 s -> 12 000 frames at 120 bpm
    set_plain(&mut g, p::SIZE, 10.0);
    set_plain(&mut g, p::PAN_SPREAD, 0.0);
    // The song position is 5 000 frames into the grid.
    let (l, _) = render(&mut g, 48_000.0, 40_000, 100, &[(0, on(60))], None, 5_000);
    let env: Vec<f32> = l.iter().map(|v| v.abs()).collect();
    let mut centres = Vec::new();
    for w in [6_900..7_700, 18_900..19_700, 30_900..31_700] {
        let (i, m) = env[w.clone()].iter().enumerate().fold((0, 0.0f32), |a, (i, &v)| if v > a.1 { (i, v) } else { a });
        assert!(m > 0.02, "a grain in {w:?}");
        centres.push(w.start + i);
    }
    // Grid points 12 000, 24 000, 36 000 minus the 5 000 offset; centre = +240.
    for (c, grid) in centres.iter().zip([7_000usize, 19_000, 31_000]) {
        assert!((*c as i64 - (grid + 240) as i64).abs() < 40, "grain centre {c}, grid {grid}");
    }
    // And nothing between the grid points.
    assert!(peak(&l[10_000..17_500]) < 0.01);
}

#[test]
fn pitch_follows_key_and_pitch_param() {
    let sr = 48_000.0;
    let x = sine(1000.0, sr, 0.8, 96_000);
    for (key, pitch, want) in [(60u8, 0.0f32, 1000.0f64), (72, 0.0, 2000.0), (60, 12.0, 2000.0), (48, 0.0, 500.0), (60, -12.0, 500.0)] {
        let mut g = sample_granular(&x, sr);
        quiet_cloud(&mut g);
        set_plain(&mut g, p::PITCH, pitch);
        let (l, _) = render(&mut g, sr, 24_000, 128, &[(0, on(key))], None, 0);
        let seg = &l[6000..22_000];
        let at = |f: f64| mag_at(seg, f, sr);
        assert!(at(want) > 0.05, "key {key} pitch {pitch}: no energy at {want}");
        for other in [want * 0.5, want * 2.0] {
            assert!(at(other) < at(want) * 0.25, "key {key} pitch {pitch}: {} at {other} vs {} at {want}", at(other), at(want));
        }
    }
}

#[test]
fn transposing_up_does_not_fold_back() {
    // 18 kHz played an octave up would alias to 12 kHz without band-limiting.
    let sr = 48_000.0;
    let x = sine(18_000.0, sr, 0.8, 96_000);
    let mut g = sample_granular(&x, sr);
    quiet_cloud(&mut g);
    set_plain(&mut g, p::PITCH, 12.0);
    let (l, _) = render(&mut g, sr, 24_000, 128, &[(0, on(60))], None, 0);
    let seg = &l[6000..22_000];
    assert!(rms(seg) < 0.02, "alias energy {}", rms(seg));
    // A tone that survives still comes through at full level.
    let x = sine(2000.0, sr, 0.8, 96_000);
    let mut g = sample_granular(&x, sr);
    quiet_cloud(&mut g);
    set_plain(&mut g, p::PITCH, 12.0);
    let (l, _) = render(&mut g, sr, 24_000, 128, &[(0, on(60))], None, 0);
    assert!(mag_at(&l[6000..22_000], 4000.0, sr) > 0.1);
}

#[test]
fn sample_rate_of_the_source_is_respected() {
    // 1 kHz recorded at 44.1 kHz must still play 1 kHz on a 48 kHz host.
    let x = sine(1000.0, 44_100.0, 0.8, 88_200);
    let mut g = sample_granular(&x, 44_100.0);
    quiet_cloud(&mut g);
    let (l, _) = render(&mut g, 48_000.0, 24_000, 128, &[(0, on(60))], None, 0);
    let seg = &l[6000..22_000];
    assert!(mag_at(seg, 1000.0, 48_000.0) > 0.1 && mag_at(seg, 919.0, 48_000.0) < 0.03);
}

#[test]
fn scale_quantizer_snaps_to_the_nearest_degree() {
    let c_major = scale_mask_ext(0, 0);
    assert_eq!(quantize(c_major, 61.0), 60.0, "ties go down");
    assert_eq!(quantize(c_major, 66.0), 65.0);
    assert_eq!(quantize(c_major, 64.4), 64.0);
    assert_eq!(quantize(c_major, 59.6), 60.0);
    let pent = scale_mask_ext(9, 0);
    assert_eq!(quantize(pent, 65.0), 64.0);
}

#[test]
fn quantized_pitch_spread_only_lands_on_scale_notes() {
    // Sine source, C major, wide spread: the energy must sit on scale pitches.
    let sr = 48_000.0;
    let x = sine(440.0, sr, 0.8, 96_000);
    let mut g = sample_granular(&x, sr);
    quiet_cloud(&mut g);
    set_plain(&mut g, p::ROOT_KEY, 69.0);
    set_plain(&mut g, p::PITCH_SPREAD, 5.0);
    set_plain(&mut g, p::QUANT, 1.0);
    set_plain(&mut g, p::SCALE, 0.0);
    set_plain(&mut g, p::ROOT, 0.0);
    set_plain(&mut g, p::DENSITY, 80.0);
    set_plain(&mut g, p::SIZE, 60.0);
    let (l, _) = render(&mut g, sr, 96_000, 128, &[(0, on(69))], None, 0);
    let seg = &l[8000..];
    // C major around A4: A, G (-2), F (-4), E (-5), B (+2), C (+3), D (+5).
    let note = |semi: f64| 440.0 * 2f64.powf(semi / 12.0);
    let on_scale: f32 = [-5.0, -4.0, -2.0, 0.0, 2.0, 3.0, 5.0].iter().map(|&s| mag_at(seg, note(s), sr)).sum();
    let off_scale: f32 = [-3.0, -1.0, 1.0, 4.0].iter().map(|&s| mag_at(seg, note(s), sr)).sum();
    assert!(on_scale > 0.05, "{on_scale}");
    assert!(off_scale < on_scale * 0.2, "off {off_scale} on {on_scale}");
}

#[test]
fn reverse_and_forward_read_opposite_ways() {
    // A rising ramp: a forward grain has its weight late, a reverse one early.
    let n = 9600;
    let x: Vec<f32> = (0..n).map(|i| 0.1 + 0.9 * i as f32 / n as f32).collect();
    let centroid = |dir: f32| {
        let mut g = sample_granular(&x, 48_000.0);
        quiet_cloud(&mut g);
        set_plain(&mut g, p::DIRECTION, dir);
        set_plain(&mut g, p::DENSITY, 0.5);
        set_plain(&mut g, p::SIZE, 80.0);
        set_plain(&mut g, p::POSITION, 50.0);
        let (l, _) = render(&mut g, 48_000.0, 3840, 128, &[(0, on(60))], None, 0);
        let (mut s, mut w) = (0.0f64, 0.0f64);
        for (i, v) in l.iter().enumerate() {
            s += i as f64 * v.abs() as f64;
            w += v.abs() as f64;
        }
        s / w
    };
    let (fwd, rev) = (centroid(0.0), centroid(1.0));
    assert!(fwd - rev > 60.0, "forward {fwd} reverse {rev}");
}

#[test]
fn position_and_spray_select_where_in_the_source_grains_come_from() {
    // First half 0.2 DC, second half 0.8 DC.
    let x: Vec<f32> = (0..96_000).map(|i| (if i < 48_000 { 0.2 } else { 0.8 }) * (std::f32::consts::TAU * 500.0 * i as f32 / 48_000.0).sin()).collect();
    let level = |pos: f32, spray: f32| {
        let mut g = sample_granular(&x, 48_000.0);
        quiet_cloud(&mut g);
        set_plain(&mut g, p::POSITION, pos);
        set_plain(&mut g, p::SPRAY, spray);
        set_plain(&mut g, p::SIZE, 50.0);
        set_plain(&mut g, p::DENSITY, 40.0);
        let (l, _) = render(&mut g, 48_000.0, 48_000, 128, &[(0, on(60))], None, 0);
        rms(&l[12_000..])
    };
    let (a, b) = (level(10.0, 0.0), level(90.0, 0.0));
    assert!(b > a * 2.5, "{a} {b}");
    let mid = level(50.0, 100.0);
    assert!(mid > a * 1.2 && mid < b * 0.95, "spray blends: {a} {mid} {b}");
}

#[test]
fn polyphony_voices_and_release() {
    let mut g = Granular::new();
    set_plain(&mut g, p::RELEASE, 30.0);
    set_plain(&mut g, p::POLYPHONY, 3.0);
    render(&mut g, 48_000.0, 4800, 128, &[(0, on(60)), (10, on(64)), (20, on(67)), (30, on(71))], None, 0);
    assert_eq!(g.active_voices(), 3, "the fourth note stole the oldest, which has faded out");
    let (l, _) = render_prepared(&mut g, 48_000.0, 24_000, 128, &[(0, off(60)), (0, off(64)), (0, off(67)), (0, off(71)), (0, EventKind::AllNotesOff)], None, 0);
    assert_eq!(g.active_voices(), 0);
    assert_eq!(g.active_grains(), 0, "grains of ended voices are freed");
    assert!(rms(&l[20_000..]) < 1e-6);
}

#[test]
fn grain_pool_is_bounded() {
    let mut g = Granular::new();
    set_plain(&mut g, p::DENSITY, 400.0);
    set_plain(&mut g, p::SIZE, 1000.0);
    set_plain(&mut g, p::POLYPHONY, 8.0);
    set_plain(&mut g, p::JITTER, 100.0);
    let mut max_grains = 0;
    let ev: Vec<_> = (0..8).map(|i| (i * 7, on(48 + i as u8 * 3))).collect();
    g.prepare(48_000.0, 128);
    for k in 0..40 {
        let e: &[(usize, EventKind)] = if k == 0 { &ev } else { &[] };
        let (l, _) = render_prepared(&mut g, 48_000.0, 4800, 128, e, None, k as i64 * 4800);
        assert!(l.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        max_grains = max_grains.max(g.active_grains());
    }
    assert!(max_grains <= MAX_GRAINS && max_grains > 32, "{max_grains}");
}

// ----- live input & freeze ---------------------------------------------------

#[test]
fn live_source_needs_the_sidechain_and_follows_it() {
    let sr = 48_000.0;
    let mut g = Granular::new();
    set_plain(&mut g, p::SOURCE, SOURCE_LIVE as f32);
    quiet_cloud(&mut g);
    set_plain(&mut g, p::POSITION, 100.0);
    set_plain(&mut g, p::SPRAY, 0.0);
    // No sidechain: nothing to hear.
    let (l, _) = render(&mut g, sr, 24_000, 128, &[(0, on(60))], None, 0);
    assert_eq!(rms(&l), 0.0);
    // With a 700 Hz input the cloud plays 700 Hz.
    let x = sine(700.0, sr, 0.7, 96_000);
    let f = |i: usize| x[i];
    let mut g2 = Granular::new();
    set_plain(&mut g2, p::SOURCE, SOURCE_LIVE as f32);
    quiet_cloud(&mut g2);
    set_plain(&mut g2, p::POSITION, 100.0);
    set_plain(&mut g2, p::SPRAY, 0.0);
    let (l, _) = render(&mut g2, sr, 48_000, 128, &[(12_000, on(60))], Some(&f), 0);
    let seg = &l[24_000..46_000];
    assert!(mag_at(seg, 700.0, sr) > 0.05, "live input heard");
}

#[test]
fn freeze_keeps_the_captured_sound_after_the_input_stops() {
    let sr = 48_000.0;
    // A 600 Hz tone for the first 0.6 s, then silence.
    let f = |i: usize| if i < 28_800 { 0.6 * (std::f64::consts::TAU * 600.0 * i as f64 / sr).sin() as f32 } else { 0.0 };
    let build = |freeze: bool| {
        let mut g = Granular::new();
        set_plain(&mut g, p::SOURCE, SOURCE_LIVE as f32);
        set_plain(&mut g, p::LIVE_LEN, 0.5);
        set_plain(&mut g, p::SIZE, 80.0);
        let mut ev = vec![(0, on(60))];
        if freeze {
            ev.push((28_800, param(&g, p::FREEZE, 1.0)));
        }
        render(&mut g, sr, 96_000, 128, &ev, Some(&f), 0).0
    };
    let frozen = build(true);
    let running = build(false);
    assert!(rms(&frozen[60_000..]) > 0.05, "frozen pad keeps sounding: {}", rms(&frozen[60_000..]));
    assert!(rms(&running[60_000..]) < 0.002, "unfrozen follows the silent input: {}", rms(&running[60_000..]));
}

#[test]
fn freezing_does_not_click() {
    let sr = 48_000.0;
    let x = sine(330.0, sr, 0.5, 200_000);
    let f = |i: usize| x[i];
    for freeze_at in [30_011usize, 30_999, 33_333] {
        let mut g = Granular::new();
        set_plain(&mut g, p::SOURCE, SOURCE_LIVE as f32);
        quiet_cloud(&mut g);
        set_plain(&mut g, p::POSITION, 100.0);
        set_plain(&mut g, p::SIZE, 150.0);
        set_plain(&mut g, p::DENSITY, 40.0);
        set_plain(&mut g, p::JITTER, 30.0);
        let ev = [(10_000, on(60)), (freeze_at, param(&g, p::FREEZE, 1.0))];
        let (l, _) = render(&mut g, sr, 70_000, 100, &ev, Some(&f), 0);
        let before = max_step(&l[freeze_at - 8000..freeze_at]);
        let after = max_step(&l[freeze_at..freeze_at + 8000]);
        assert!(after < before * 1.6 + 0.005, "freeze at {freeze_at}: step {before} -> {after}");
        assert!(rms(&l[60_000..]) > 0.02);
    }
}

#[test]
fn live_reads_stay_behind_the_write_head() {
    // A rising/falling sine whose every discontinuity would be a big step.
    let sr = 48_000.0;
    let x = sine(150.0, sr, 0.5, 200_000);
    let f = |i: usize| x[i];
    for (pitch, dir, size) in [(0.0, 0.0, 100.0), (12.0, 0.0, 400.0), (24.0, 0.0, 900.0), (-12.0, 1.0, 900.0), (7.0, 3.0, 500.0), (24.0, 1.0, 1000.0)] {
        let mut g = Granular::new();
        set_plain(&mut g, p::SOURCE, SOURCE_LIVE as f32);
        quiet_cloud(&mut g);
        set_plain(&mut g, p::POSITION, 100.0);
        set_plain(&mut g, p::PITCH, pitch);
        set_plain(&mut g, p::DIRECTION, dir);
        set_plain(&mut g, p::SIZE, size);
        set_plain(&mut g, p::LIVE_LEN, 1.0);
        let (l, _) = render(&mut g, sr, 120_000, 128, &[(2000, on(60))], Some(&f), 0);
        let step = max_step(&l[30_000..]);
        assert!(step < 0.35, "pitch {pitch} dir {dir} size {size}: step {step}");
        assert!(l.iter().all(|v| v.is_finite()));
    }
}

#[test]
fn frozen_capture_survives_a_state_round_trip() {
    let sr = 48_000.0;
    let x = sine(500.0, sr, 0.6, 48_000);
    let f = |i: usize| x[i];
    let mut g = Granular::new();
    set_plain(&mut g, p::SOURCE, SOURCE_LIVE as f32);
    set_plain(&mut g, p::LIVE_LEN, 0.5);
    let ev = [(30_000, param(&g, p::FREEZE, 1.0))];
    render(&mut g, sr, 32_000, 128, &ev, Some(&f), 0);
    let state = g.save_state();
    let mut h = Granular::new();
    h.load_state(&state);
    assert_eq!(get_plain(&h, p::FREEZE), 1.0);
    let (l, _) = render(&mut h, sr, 24_000, 128, &[(0, on(60))], None, 0);
    assert!(rms(&l[6000..]) > 0.03, "restored capture plays: {}", rms(&l[6000..]));
    assert!(mag_at(&l[6000..22_000], 500.0, sr) > 0.03);
    // An unfrozen state carries no audio.
    let mut u = Granular::new();
    set_plain(&mut u, p::SOURCE, SOURCE_LIVE as f32);
    render(&mut u, sr, 8000, 128, &[], Some(&f), 0);
    let s = u.save_state();
    assert!(s.len() < 4000, "no audio in an unfrozen state: {} bytes", s.len());
}

#[test]
fn sample_path_round_trip_and_missing_file() {
    let dir = std::env::temp_dir().join(format!("aitrack-granular-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tone.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for v in sine(900.0, 48_000.0, 0.7, 48_000) {
        w.write_sample((v * 30_000.0) as i16).unwrap();
    }
    w.finalize().unwrap();
    let mut g = Granular::new();
    g.load_wav(path.to_str().unwrap()).unwrap();
    assert_eq!(get_plain(&g, p::SOURCE), SOURCE_SAMPLE as f32);
    quiet_cloud(&mut g);
    let state = g.save_state();
    let mut h = Granular::new();
    h.load_state(&state);
    assert!(h.has_sample());
    let (l, _) = render(&mut h, 48_000.0, 24_000, 128, &[(0, on(60))], None, 0);
    assert!(mag_at(&l[6000..22_000], 900.0, 48_000.0) > 0.05);
    let _ = std::fs::remove_dir_all(&dir);
    // A path that no longer exists leaves the built-in source in charge.
    let mut m = Granular::new();
    m.load_state(&state);
    assert!(!m.has_sample());
    let (l, _) = render(&mut m, 48_000.0, 24_000, 128, &[(0, on(60))], None, 0);
    assert!(rms(&l[6000..]) > 0.01);
}

#[test]
fn base64_round_trips() {
    for n in 0..20 {
        let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
        assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
    }
    assert!(base64_decode("***").is_none());
}

#[test]
fn meter_reports_cloud_and_waveform() {
    let mut g = Granular::new();
    render(&mut g, 48_000.0, 24_000, 128, &[(0, on(60))], None, 0);
    let mut m = vec![0.0f32; 320];
    let n = g.meter(&mut m);
    assert_eq!(n, METER_LEN);
    assert_eq!(m[0], 1.0);
    assert!(m[1] >= 1.0 && m[2] > 0.0 && m[5] == 2.0 && m[6] > 1.0);
    assert!(m[8..72].iter().any(|v| *v > 0.1), "waveform peaks");
    assert!(m[72..264].chunks(3).any(|c| c[0] >= 0.0), "grains listed");
}

// ----- robustness ---------------------------------------------------------------

#[test]
fn extreme_settings_stay_finite_and_bounded() {
    let sample = sine(1000.0, 48_000.0, 1.0, 20_000);
    let noise = |i: usize| (((i as u32).wrapping_mul(2_654_435_761) >> 8) as f32 / 8_388_608.0 - 1.0) * 4.0;
    for &sr in &[22_050.0, 44_100.0, 96_000.0, 192_000.0] {
        for &block in &[1usize, 64, 4096] {
            for pass in 0..4 {
                let mut g = Granular::new();
                g.set_sample(&sample, 1, 48_000.0);
                g.prepare(sr, block);
                let ids: Vec<u32> = g.info().params.iter().map(|i| i.id).collect();
                for (j, id) in ids.iter().enumerate() {
                    let v = match pass {
                        0 => 0.0,
                        1 => 1.0,
                        2 => ((j * 7919 + 13) % 101) as f32 / 100.0,
                        _ => {
                            if j % 2 == 0 {
                                0.0
                            } else {
                                1.0
                            }
                        }
                    };
                    g.set_param(*id, v);
                }
                // Cycle the three sources through the pass.
                let source = pass % 3;
                set_plain(&mut g, p::SOURCE, source as f32);
                let frames = if block == 1 { 4800 } else { 24_000 };
                let ev = [(0, on(36)), (3, on(60)), (5, on(96)), (7, on(127)), (frames / 2, off(60)), (frames / 2, EventKind::PitchBend { channel: 0, value: 1.0 })];
                let (l, r) = render_prepared(&mut g, sr, frames, block, &ev, Some(&noise), 12_345);
                for (i, v) in l.iter().chain(r.iter()).enumerate() {
                    assert!(v.is_finite() && v.abs() <= 1.0 + 1e-6, "sr {sr} block {block} pass {pass}: sample {i} = {v}");
                }
                let mut m = vec![0.0f32; 320];
                g.meter(&mut m);
                assert!(m.iter().all(|v| v.is_finite()));
            }
        }
    }
}

/// `cargo test -p aitrack-stock --release --lib granular_cpu -- --ignored --nocapture`
#[test]
#[ignore]
fn granular_cpu() {
    let sr = 48_000.0;
    println!("\n{:<44} {:>8}", "scenario (1 s of 48 kHz audio, 128 frames)", "CPU %");
    for (name, voices, dens, size, aa) in [
        ("default: 1 voice, 20 Hz, 100 ms", 1, 20.0, 100.0, 0.0),
        ("4 voices, 60 Hz, 200 ms", 4, 60.0, 200.0, 0.0),
        ("8 voices, 400 Hz, 1 s (pool saturated)", 8, 400.0, 1000.0, 0.0),
        ("8 voices, 400 Hz, 1 s, +19 st (mip blend)", 8, 400.0, 1000.0, 19.0),
        ("1 voice, 400 Hz, 300 ms, live source", 1, 400.0, 300.0, 0.0),
    ] {
        let mut g = Granular::new();
        set_plain(&mut g, p::DENSITY, dens);
        set_plain(&mut g, p::SIZE, size);
        set_plain(&mut g, p::PITCH, aa);
        set_plain(&mut g, p::POLYPHONY, 8.0);
        set_plain(&mut g, p::PAN_SPREAD, 100.0);
        set_plain(&mut g, p::FILTER_TYPE, 1.0);
        set_plain(&mut g, p::CUTOFF, 4000.0);
        let live = name.contains("live");
        if live {
            set_plain(&mut g, p::SOURCE, SOURCE_LIVE as f32);
        }
        let noise = |i: usize| (((i as u32).wrapping_mul(2_654_435_761) >> 8) as f32 / 8_388_608.0 - 1.0) * 0.5;
        let ev: Vec<_> = (0..voices).map(|k| (k * 5, on(48 + k as u8 * 4))).collect();
        g.prepare(sr, 128);
        render_prepared(&mut g, sr, 48_000, 128, &ev, if live { Some(&noise) } else { None }, 0);
        let t = std::time::Instant::now();
        render_prepared(&mut g, sr, 48_000, 128, &[], if live { Some(&noise) } else { None }, 48_000);
        println!("{name:<44} {:>8.2}   grains {}", 100.0 * t.elapsed().as_secs_f64(), g.active_grains());
    }
}

#[test]
fn interval_quantize_keeps_chords_and_only_moves_the_offset() {
    // Octaves only: every grain is the played note shifted by whole octaves.
    let sr = 48_000.0;
    let x = sine(440.0, sr, 0.8, 96_000);
    let mut g = sample_granular(&x, sr);
    quiet_cloud(&mut g);
    set_plain(&mut g, p::ROOT_KEY, 69.0);
    set_plain(&mut g, p::PITCH_SPREAD, 12.0);
    set_plain(&mut g, p::QUANT, 2.0);
    set_plain(&mut g, p::SCALE, 13.0);
    set_plain(&mut g, p::DENSITY, 80.0);
    set_plain(&mut g, p::SIZE, 60.0);
    // Play E5 (a fifth above A4): the cloud must be E5 and its octaves only.
    let (l, _) = render(&mut g, sr, 96_000, 128, &[(0, on(76))], None, 0);
    let seg = &l[8000..];
    let e5 = 440.0 * 2f64.powf(7.0 / 12.0);
    let oct: f32 = [0.5, 1.0, 2.0].iter().map(|m| mag_at(seg, e5 * m, sr)).sum();
    let other: f32 = [440.0 * 2.0, 440.0 * 0.5, e5 * 1.5].iter().map(|&f| mag_at(seg, f, sr)).sum();
    assert!(oct > 0.03, "{oct}");
    assert!(other < oct * 0.2, "other {other} oct {oct}");
}

#[test]
fn factory_presets_are_audible_balanced_and_clean() {
    let list = crate::presets::list(ID);
    assert!(list.len() >= 15, "{} presets", list.len());
    let noise = |i: usize| (((i as u32).wrapping_mul(2_654_435_761) >> 8) as f32 / 8_388_608.0 - 1.0) * 0.3;
    let mut report = Vec::new();
    for r in &list {
        let preset = crate::presets::load(&r.path).unwrap();
        let mut g = Granular::new();
        g.prepare(48_000.0, 128);
        preset.apply(&mut g);
        let live = get_plain(&g, p::SOURCE) == SOURCE_LIVE as f32;
        let side: Option<&dyn Fn(usize) -> f32> = if live { Some(&noise) } else { None };
        let (l, rr) = render_prepared(&mut g, 48_000.0, 6 * 48_000, 128, &[(0, on(60))], side, 0);
        let tail = &l[4 * 48_000..];
        let level = rms(tail);
        report.push(format!("{:<26} rms {level:.3} peak {:.2}", r.name, peak(&l)));
        assert!(l.iter().chain(rr.iter()).all(|v| v.is_finite() && v.abs() <= 1.0), "{}", r.name);
        assert!(level > 0.004 && level < 0.4, "{}: rms {level}", r.name);
    }
    println!("{}", report.join("\n"));
}
