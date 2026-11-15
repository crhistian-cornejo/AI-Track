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

fn keys_at(r: &Rig, t: i64) -> Vec<u8> {
    let mut k: Vec<u8> = r.ons().iter().filter(|n| n.0 == t).map(|n| n.1).collect();
    k.sort();
    k
}

#[test]
fn the_default_is_a_major_triad_that_ends_with_the_key() {
    let mut r = rig();
    r.run(&[(10, on(60, 0.8)), (500, off(60))], 1000, 64);
    assert_eq!(keys_at(&r, 10), vec![60, 64, 67]);
    let offs: Vec<_> = r.notes().into_iter().filter(|n| !n.1).collect();
    assert_eq!(offs.len(), 3);
    assert!(offs.iter().all(|n| n.0 == 500));
    r.assert_balanced();
}

#[test]
fn six_voices_with_their_own_interval_and_level() {
    let mut r = rig();
    for v in 0..VOICES {
        set(&mut r, p::on(v), 1.0);
        set(&mut r, p::interval(v), [0.0, -12.0, 3.0, 7.0, 10.0, 24.0][v]);
        set(&mut r, p::level(v), [100.0, 50.0, 100.0, 25.0, 200.0, 100.0][v]);
    }
    r.run(&[(0, on(60, 0.5))], 100, 100);
    let mut got: Vec<(u8, f32)> = r.notes().iter().filter(|n| n.1).map(|n| (n.2, n.3)).collect();
    got.sort_by_key(|n| n.0);
    let keys: Vec<u8> = got.iter().map(|n| n.0).collect();
    assert_eq!(keys, vec![48, 60, 63, 67, 70, 84]);
    let vel = |k: u8| got.iter().find(|n| n.0 == k).unwrap().1;
    assert!((vel(60) - 0.5).abs() < 1e-6 && (vel(48) - 0.25).abs() < 1e-6 && (vel(67) - 0.125).abs() < 1e-6);
    assert_eq!(vel(70), 1.0, "200% of 0.5, clamped to full");
}

#[test]
fn a_zero_level_or_a_switched_off_voice_is_silent_and_out_of_range_voices_are_dropped() {
    let mut r = rig();
    set(&mut r, p::level(1), 0.0);
    set(&mut r, p::on(2), 0.0);
    r.run(&[(0, on(60, 0.8))], 10, 10);
    assert_eq!(keys_at(&r, 0), vec![60]);
    let mut r = rig();
    set(&mut r, p::interval(1), 36.0);
    r.run(&[(0, on(120, 0.8))], 10, 10);
    assert_eq!(keys_at(&r, 0), vec![120, 127], "120+36 and 120+7 fall outside only for the first");
}

#[test]
fn two_voices_on_the_same_interval_make_one_note() {
    let mut r = rig();
    set(&mut r, p::interval(1), 0.0);
    r.run(&[(0, on(60, 0.8))], 10, 10);
    assert_eq!(keys_at(&r, 0), vec![60, 67]);
}

#[test]
fn fit_to_scale_moves_the_added_voices_but_never_the_played_key() {
    let mut r = rig();
    set(&mut r, p::FIT, 1.0);
    set(&mut r, p::ROOT, 0.0);
    set(&mut r, p::SCALE, 1.0); // C minor
    // C: E is out of C minor, it goes down to Eb; G stays.
    r.run(&[(0, on(60, 0.8)), (100, on(61, 0.8))], 200, 100);
    assert_eq!(keys_at(&r, 0), vec![60, 63, 67]);
    // C# is not in the scale but it is what was played; E+1 (F) is in.
    let c_sharp = keys_at(&r, 100);
    assert!(c_sharp.contains(&61), "{c_sharp:?}");
    assert!(c_sharp.iter().filter(|k| **k != 61).all(|k| crate::midi::theory::in_mask(scale_mask(1, 0), *k as i32)));
}

#[test]
fn changing_the_voices_while_a_key_is_held_does_not_strand_its_notes() {
    let mut r = rig();
    r.run(&[(0, on(60, 0.8))], 100, 100);
    set(&mut r, p::interval(1), 5.0);
    set(&mut r, p::on(2), 0.0);
    set(&mut r, p::FIT, 1.0);
    r.run(&[(0, off(60))], 100, 100);
    r.assert_balanced();
    let offs: Vec<u8> = {
        let mut o: Vec<u8> = r.notes().iter().filter(|n| !n.1).map(|n| n.2).collect();
        o.sort();
        o
    };
    assert_eq!(offs, vec![60, 64, 67], "the keys that were started are the ones ended");
}

#[test]
fn chords_sharing_a_note_retrigger_it_and_end_it_with_the_last_key() {
    let mut r = rig();
    // 60 -> 60 64 67; 64 -> 64 68 71: key 64 is in both.
    r.run(&[(0, on(60, 0.8)), (100, on(64, 0.8)), (200, off(60)), (300, off(64))], 400, 64);
    let n = r.notes();
    let of64: Vec<(i64, bool)> = n.iter().filter(|x| x.2 == 64).map(|x| (x.0, x.1)).collect();
    assert_eq!(of64, vec![(0, true), (100, false), (100, true), (300, false)], "off+on at the second press, one off at the very end");
    r.assert_balanced();
}

#[test]
fn pressing_a_key_twice_restarts_its_chord() {
    let mut r = rig();
    r.run(&[(0, on(60, 0.8)), (50, on(60, 0.8)), (100, off(60))], 200, 64);
    r.assert_balanced();
    assert_eq!(r.ons().len(), 6);
}

#[test]
fn all_notes_off_ends_everything_and_other_events_pass() {
    let mut r = rig();
    let cc = EventKind::ControlChange { channel: 0, cc: 7, value: 0.3 };
    r.run(&[(0, on(60, 0.8)), (0, on(72, 0.8)), (10, cc), (20, EventKind::AllNotesOff)], 100, 100);
    r.assert_balanced();
    assert!(r.log.contains(&(10, cc)));
    // A stray note-off for a key never pressed (after a reset) is ignored.
    let n = r.log.len();
    r.run(&[(0, off(90))], 10, 10);
    assert_eq!(r.log.len(), n);
}

#[test]
fn reset_forgets_the_keys() {
    let mut r = rig();
    r.run(&[(0, on(60, 0.8))], 10, 10);
    r.p.reset();
    let n = r.log.len();
    r.run(&[(0, off(60))], 10, 10);
    assert_eq!(r.log.len(), n, "the engine closed them; a late note-off is not repeated");
    r.run(&[(0, on(62, 0.8)), (5, off(62))], 10, 10);
    assert_eq!(r.log.len(), n + 6);
}

#[test]
fn output_is_the_same_for_any_block_size() {
    let input = vec![(10, on(60, 0.8)), (20, on(64, 0.6)), (700, off(60)), (900, off(64)), (950, on(48, 1.0)), (1999, off(48))];
    let mut a = rig();
    a.run(&input, 2000, 4096);
    for block in [1usize, 3, 100, 1000] {
        let mut b = rig();
        b.run(&input, 2000, block);
        assert_eq!(a.log, b.log, "block {block}");
    }
}

#[test]
fn no_note_stays_open_under_random_play_and_parameter_changes() {
    let mut seed = 0x9E37_79B9u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..5 {
        let mut r = rig();
        let mut down = [false; 24];
        for slice in 0..60 {
            let mut input = Vec::new();
            let mut t = 0;
            while t < 3000 {
                t += (rnd() % 400) as i64;
                if t >= 3000 {
                    break;
                }
                let k = (rnd() % 24) as usize;
                if down[k] {
                    input.push((t, off(40 + k as u8 * 3)));
                } else {
                    input.push((t, on(40 + k as u8 * 3, 0.1 + (rnd() % 90) as f32 / 100.0)));
                }
                down[k] = !down[k];
            }
            r.run(&input, 3000, 1 + (rnd() % 500) as usize);
            for v in 0..VOICES {
                if rnd() % 4 == 0 {
                    set(&mut r, p::on(v), (rnd() % 2) as f32);
                    set(&mut r, p::interval(v), (rnd() % 73) as f32 - 36.0);
                    set(&mut r, p::level(v), (rnd() % 201) as f32);
                }
            }
            if slice % 7 == 0 {
                set(&mut r, p::FIT, (rnd() % 2) as f32);
                set(&mut r, p::SCALE, (rnd() % 13) as f32);
                set(&mut r, p::ROOT, (rnd() % 12) as f32);
            }
        }
        let tail: Vec<_> = down.iter().enumerate().filter(|(_, d)| **d).map(|(k, _)| (0, off(40 + k as u8 * 3))).collect();
        r.run(&tail, 100, 100);
        r.assert_balanced();
        assert_eq!(r.p.dropped_events(), 0);
    }
}
