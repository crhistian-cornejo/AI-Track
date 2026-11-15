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

/// Play each key for 10 samples and return the keys that came out.
fn through(r: &mut Rig, keys: &[u8]) -> Vec<u8> {
    let before = r.ons().len();
    let mut input = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        input.push((i as i64 * 20, on(*k, 0.7)));
        input.push((i as i64 * 20 + 10, off(*k)));
    }
    r.run(&input, keys.len() as i64 * 20, 64);
    r.keys()[before..].to_vec()
}

fn c_major(r: &mut Rig) {
    set(r, p::ROOT, 0.0);
    set(r, p::SCALE, 0.0);
}

#[test]
fn nearest_corrects_the_notes_outside_the_scale_and_leaves_the_rest() {
    let mut r = rig();
    c_major(&mut r);
    // C C# D D# E F F# G G# A A# B
    let got = through(&mut r, &[60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71]);
    assert_eq!(got, vec![60, 60, 62, 62, 64, 65, 65, 67, 67, 69, 69, 71]);
}

#[test]
fn up_and_down_take_the_first_scale_note_that_way() {
    let mut r = rig();
    c_major(&mut r);
    set(&mut r, p::MODE, 1.0);
    assert_eq!(through(&mut r, &[61, 63, 66, 68, 70, 60]), vec![62, 64, 67, 69, 71, 60]);
    set(&mut r, p::MODE, 2.0);
    assert_eq!(through(&mut r, &[61, 63, 66, 68, 70, 60]), vec![60, 62, 65, 67, 69, 60]);
}

#[test]
fn pass_off_makes_up_and_down_a_diatonic_step() {
    let mut r = rig();
    c_major(&mut r);
    set(&mut r, p::PASS, 0.0);
    set(&mut r, p::MODE, 1.0);
    assert_eq!(through(&mut r, &[60, 61, 64, 71]), vec![62, 62, 65, 72]);
    set(&mut r, p::MODE, 2.0);
    assert_eq!(through(&mut r, &[60, 61, 65, 62]), vec![59, 60, 64, 60]);
    // Nearest has nothing to step: inside stays, outside snaps.
    set(&mut r, p::MODE, 0.0);
    assert_eq!(through(&mut r, &[60, 61]), vec![60, 60]);
}

#[test]
fn a_scale_other_than_the_default_and_a_different_root() {
    let mut r = rig();
    set(&mut r, p::ROOT, 9.0); // A
    set(&mut r, p::SCALE, 10.0); // minor pentatonic: A C D E G
    assert_eq!(through(&mut r, &[69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79]), vec![69, 69, 72, 72, 72, 74, 74, 76, 76, 79, 79]);
}

#[test]
fn chromatic_leaves_everything_alone() {
    let mut r = rig();
    set(&mut r, p::SCALE, 12.0);
    let keys: Vec<u8> = (30..90).collect();
    assert_eq!(through(&mut r, &keys), keys);
}

#[test]
fn custom_scale_uses_the_twelve_switches() {
    let mut r = rig();
    set(&mut r, p::SCALE, 13.0);
    for i in 0..12 {
        set(&mut r, p::CUSTOM0 + i, if [0, 4, 7].contains(&i) { 1.0 } else { 0.0 });
    }
    // C E G only. D ties C and E and goes down; A# is 3 from G, 2 from the next C.
    assert_eq!(through(&mut r, &[60, 61, 62, 63, 65, 66, 69, 70, 71]), vec![60, 60, 60, 64, 64, 67, 67, 72, 72]);
    // An empty custom scale is a bypass.
    for i in 0..12 {
        set(&mut r, p::CUSTOM0 + i, 0.0);
    }
    assert_eq!(through(&mut r, &[61, 62]), vec![61, 62]);
}

#[test]
fn transpose_comes_before_the_correction() {
    let mut r = rig();
    c_major(&mut r);
    set(&mut r, p::TRANSPOSE, 1.0);
    // C -> C# -> C; D -> D# -> D; E -> F (inside).
    assert_eq!(through(&mut r, &[60, 62, 64]), vec![60, 62, 65]);
    set(&mut r, p::TRANSPOSE, -36.0);
    assert_eq!(through(&mut r, &[40, 20]), vec![4, 0], "E0, and the floor of the range");
}

#[test]
fn keys_outside_lowest_and_range_go_through_untouched() {
    let mut r = rig();
    c_major(&mut r);
    set(&mut r, p::LOWEST, 60.0);
    set(&mut r, p::RANGE, 12.0);
    set(&mut r, p::TRANSPOSE, 2.0);
    // 59 and 73 are outside [60, 72]: neither corrected nor transposed.
    // 61 + 2 = 63 is corrected to 62; 72 + 2 = 74 is in the scale.
    assert_eq!(through(&mut r, &[59, 61, 72, 73, 49]), vec![59, 62, 74, 73, 49]);
}

#[test]
fn the_note_off_ends_the_note_that_was_started_even_if_the_scale_changed() {
    let mut r = rig();
    c_major(&mut r);
    r.run(&[(0, on(61, 0.7))], 100, 100);
    set(&mut r, p::SCALE, 1.0);
    set(&mut r, p::TRANSPOSE, 5.0);
    r.run(&[(0, off(61))], 100, 100);
    let n = r.notes();
    assert_eq!((n[0].1, n[0].2), (true, 60));
    assert_eq!((n[1].1, n[1].2), (false, 60), "the key that was started");
    r.assert_balanced();
}

#[test]
fn velocity_and_note_id_are_kept() {
    let mut r = rig();
    c_major(&mut r);
    r.run(&[(0, EventKind::NoteOn { channel: 3, key: 61, velocity: 0.33, note_id: 77 })], 10, 10);
    assert_eq!(r.log[0].1, EventKind::NoteOn { channel: 3, key: 60, velocity: 0.33, note_id: 77 });
}

#[test]
fn two_keys_on_the_same_note_share_it() {
    let mut r = rig();
    c_major(&mut r);
    // 61 and 60 both become 60.
    r.run(&[(0, on(60, 0.7)), (10, on(61, 0.7)), (20, off(60)), (30, off(61))], 50, 50);
    let of60: Vec<(i64, bool)> = r.notes().iter().filter(|n| n.2 == 60).map(|n| (n.0, n.1)).collect();
    assert_eq!(of60, vec![(0, true), (10, false), (10, true), (30, false)]);
    r.assert_balanced();
}

#[test]
fn other_events_pass_and_expressions_follow_their_note() {
    let mut r = rig();
    c_major(&mut r);
    let cc = EventKind::ControlChange { channel: 0, cc: 1, value: 0.5 };
    let ex = |key| EventKind::NoteExpression { note_id: 5, key, expr: aitrack_dsp::event::Expression::Tuning, value: 0.5 };
    r.run(&[(0, on(61, 0.7)), (5, cc), (6, ex(61)), (7, ex(99))], 20, 20);
    assert!(r.log.contains(&(5, cc)));
    assert!(r.log.contains(&(6, ex(60))), "the expression follows the corrected key");
    assert!(!r.log.iter().any(|e| e.1 == ex(99)), "and one for a key that is not sounding is dropped");
}

#[test]
fn reset_and_all_notes_off_forget_the_keys() {
    let mut r = rig();
    c_major(&mut r);
    r.run(&[(0, on(61, 0.7))], 10, 10);
    r.p.reset();
    let n = r.log.len();
    r.run(&[(0, off(61))], 10, 10);
    assert_eq!(r.log.len(), n);
}

#[test]
fn block_size_does_not_matter() {
    let input: Vec<_> = (0..50).flat_map(|i| [(i * 40, on(55 + (i % 13) as u8, 0.5)), (i * 40 + 25, off(55 + (i % 13) as u8))]).collect();
    let mut a = rig();
    a.run(&input, 2100, 4096);
    for block in [1usize, 7, 128] {
        let mut b = rig();
        b.run(&input, 2100, block);
        assert_eq!(a.log, b.log);
    }
}

#[test]
fn every_note_on_gets_its_note_off_under_random_play() {
    let mut seed = 0xC0FF_EE11u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut r = rig();
    let mut down = [false; 40];
    for slice in 0..80 {
        let mut input = Vec::new();
        for t in (0..2000).step_by(37) {
            if rnd() % 2 == 0 {
                let k = (rnd() % 40) as usize;
                input.push((t as i64, if down[k] { off(40 + k as u8) } else { on(40 + k as u8, 0.5) }));
                down[k] = !down[k];
            }
        }
        r.run(&input, 2000, 1 + (rnd() % 300) as usize);
        if slice % 3 == 0 {
            set(&mut r, p::SCALE, (rnd() % 14) as f32);
            set(&mut r, p::ROOT, (rnd() % 12) as f32);
            set(&mut r, p::MODE, (rnd() % 3) as f32);
            set(&mut r, p::PASS, (rnd() % 2) as f32);
            set(&mut r, p::TRANSPOSE, (rnd() % 73) as f32 - 36.0);
            set(&mut r, p::LOWEST, (rnd() % 60) as f32);
            set(&mut r, p::RANGE, (rnd() % 128) as f32);
        }
    }
    let tail: Vec<_> = down.iter().enumerate().filter(|(_, d)| **d).map(|(k, _)| (0, off(40 + k as u8))).collect();
    r.run(&tail, 10, 10);
    r.assert_balanced();
}
