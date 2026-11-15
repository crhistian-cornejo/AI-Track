use super::*;
use crate::midi::fxtest::*;
use crate::test_util::set_plain;
use aitrack_dsp::EventKind;

fn rig() -> Rig {
    Rig::new(create())
}

fn set(r: &mut Rig, id: u32, plain: f32) {
    set_plain(r.p.as_mut(), id, plain);
}

/// Play one note per velocity (in 1…127) and return the velocities that came out, as 1…127.
fn through(r: &mut Rig, vels: &[u8]) -> Vec<u8> {
    let before = r.notes().iter().filter(|n| n.1).count();
    let input: Vec<_> = vels.iter().enumerate().map(|(i, v)| (i as i64 * 10, on(60, *v as f32 / 127.0))).collect();
    r.run(&input, vels.len() as i64 * 10 + 1, 64);
    r.notes().iter().filter(|n| n.1).skip(before).map(|n| (n.3 * 127.0).round() as u8).collect()
}

#[test]
fn the_defaults_change_nothing() {
    let mut r = rig();
    let all: Vec<u8> = (1..=127).collect();
    assert_eq!(through(&mut r, &all), all);
}

#[test]
fn fixed_mode_ignores_the_played_velocity() {
    let mut r = rig();
    set(&mut r, p::MODE, 2.0);
    set(&mut r, p::FIXED, 90.0);
    assert_eq!(through(&mut r, &[1, 40, 127]), vec![90, 90, 90]);
    set(&mut r, p::GAIN, 10.0);
    assert_eq!(through(&mut r, &[5]), vec![100], "gain still applies");
    set(&mut r, p::MAX, 96.0);
    assert_eq!(through(&mut r, &[5]), vec![96], "and the range clamps");
}

#[test]
fn map_squeezes_into_the_range_and_clip_cuts_to_it() {
    let mut r = rig();
    set(&mut r, p::MIN, 80.0);
    set(&mut r, p::MAX, 100.0);
    assert_eq!(through(&mut r, &[1, 64, 127]), vec![80, 90, 100]);
    set(&mut r, p::MODE, 1.0);
    assert_eq!(through(&mut r, &[1, 64, 90, 127]), vec![80, 80, 90, 100]);
    // Min above Max just swaps them.
    set(&mut r, p::MIN, 100.0);
    set(&mut r, p::MAX, 80.0);
    assert_eq!(through(&mut r, &[1, 127]), vec![80, 100]);
}

#[test]
fn curve_lifts_or_lowers_the_soft_notes_and_keeps_the_ends() {
    let mut r = rig();
    set(&mut r, p::CURVE, 60.0);
    let up = through(&mut r, &[1, 32, 64, 127]);
    assert!(up[1] > 32 && up[2] > 64 && up[3] == 127, "{up:?}");
    set(&mut r, p::CURVE, -60.0);
    let down = through(&mut r, &[1, 32, 64, 127]);
    assert!(down[1] < 32 && down[2] < 64 && down[3] == 127, "{down:?}");
    assert!(up.windows(2).all(|w| w[0] <= w[1]) && down.windows(2).all(|w| w[0] <= w[1]), "monotonic");
}

#[test]
fn compress_pulls_toward_the_pivot_and_expand_pushes_away() {
    let mut r = rig();
    set(&mut r, p::PIVOT, 64.0);
    set(&mut r, p::COMPAND, -100.0);
    let all_there = through(&mut r, &[10, 64, 120]);
    assert_eq!(all_there, vec![64, 64, 64], "full compression puts everything on the pivot");
    set(&mut r, p::COMPAND, -50.0);
    let half = through(&mut r, &[24, 104]);
    assert_eq!(half, vec![44, 84], "half the distance");
    set(&mut r, p::COMPAND, 50.0);
    let wide = through(&mut r, &[54, 74, 64]);
    assert_eq!(wide, vec![44, 84, 64], "twice the distance at +50%");
    set(&mut r, p::COMPAND, 100.0);
    assert_eq!(through(&mut r, &[100, 30]), vec![127, 1], "and clamped at the ends");
}

#[test]
fn gain_shifts_in_velocity_steps_and_clamps() {
    let mut r = rig();
    set(&mut r, p::GAIN, -30.0);
    assert_eq!(through(&mut r, &[100, 20, 127]), vec![70, 1, 97]);
    set(&mut r, p::GAIN, 64.0);
    assert_eq!(through(&mut r, &[100, 20]), vec![127, 84]);
}

#[test]
fn random_is_repeatable_with_the_seed_and_bounded_by_its_amount() {
    let vels = [80u8; 200];
    let run = |seed: f32, amount: f32| {
        let mut r = rig();
        set(&mut r, p::RANDOM, amount);
        set(&mut r, p::SEED, seed);
        through(&mut r, &vels)
    };
    let a = run(3.0, 50.0);
    assert_eq!(a, run(3.0, 50.0), "same seed, same sequence");
    assert_ne!(a, run(4.0, 50.0), "another seed, another one");
    // 50% is up to +-25% of the range: +-32 around 80.
    assert!(a.iter().all(|v| (80 - 32..=80 + 32).contains(&(*v as i32))), "{a:?}");
    assert!(a.iter().any(|v| *v < 70) && a.iter().any(|v| *v > 90), "it does spread");
    let mean = a.iter().map(|v| *v as f32).sum::<f32>() / a.len() as f32;
    assert!((mean - 80.0).abs() < 5.0, "centred: {mean}");
    assert!(run(3.0, 0.0).iter().all(|v| *v == 80));
}

#[test]
fn the_random_sequence_restarts_on_reset_and_ignores_block_size() {
    let input: Vec<_> = (0..100).map(|i| (i * 7, on(60, 0.6))).collect();
    let mut a = rig();
    set(&mut a, p::RANDOM, 80.0);
    set(&mut a, p::SEED, 11.0);
    a.run(&input, 700, 4096);
    for block in [1usize, 5, 64] {
        let mut b = rig();
        set(&mut b, p::RANDOM, 80.0);
        set(&mut b, p::SEED, 11.0);
        b.run(&input, 700, block);
        assert!(a.log == b.log, "block {block}");
    }
    let vels = |r: &Rig| -> Vec<u32> { r.notes().iter().map(|n| (n.3 * 127.0).round() as u32).collect() };
    let first = vels(&a);
    assert_eq!(first.len(), 100);
    a.log.clear();
    a.p.reset();
    a.run(&input, 700, 100);
    assert_eq!(vels(&a), first, "reset starts the sequence again");
}

#[test]
fn only_note_on_velocity_changes() {
    let mut r = rig();
    set(&mut r, p::MODE, 2.0);
    set(&mut r, p::FIXED, 33.0);
    let cc = EventKind::ControlChange { channel: 2, cc: 74, value: 0.2 };
    let release = EventKind::NoteOff { channel: 2, key: 61, velocity: 0.9, note_id: 8 };
    r.run(&[(0, EventKind::NoteOn { channel: 2, key: 61, velocity: 1.0, note_id: 8 }), (5, cc), (9, release)], 20, 20);
    assert_eq!(r.log[0].1, EventKind::NoteOn { channel: 2, key: 61, velocity: 33.0 / 127.0, note_id: 8 });
    assert_eq!(r.log[1], (5, cc));
    assert_eq!(r.log[2], (9, release));
}

#[test]
fn every_setting_stays_in_range() {
    let mut seed = 0xBEEF_1234u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..300 {
        let mut r = rig();
        for info in params() {
            set(&mut r, info.id, info.min + (info.max - info.min) * (rnd() % 1001) as f32 / 1000.0);
        }
        let v: Vec<u8> = (0..20).map(|_| 1 + (rnd() % 127) as u8).collect();
        let got = through(&mut r, &v);
        assert_eq!(got.len(), v.len());
        assert!(got.iter().all(|x| (1..=127).contains(x)));
    }
}
