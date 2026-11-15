use super::*;
use crate::midi::fxtest::*;
use crate::test_util::set_plain;
use aitrack_dsp::EventKind;

/// Beat length at 120 BPM / 48 kHz.
const BEAT: i64 = 24_000;
const S16: i64 = BEAT / 4;

fn rig() -> Rig {
    Rig::new(create())
}

fn set(r: &mut Rig, id: u32, plain: f32) {
    set_plain(r.p.as_mut(), id, plain);
}

fn press(keys: &[u8]) -> Vec<(i64, EventKind)> {
    keys.iter().map(|&k| (0, on(k, 0.8))).collect()
}

fn ons_keys(r: &Rig, n: usize) -> Vec<u8> {
    r.keys().into_iter().take(n).collect()
}

fn style_keys(style: f32, held: &[u8], n: usize) -> Vec<u8> {
    let mut r = rig();
    set(&mut r, p::STYLE, style);
    r.run(&press(held), S16 * n as i64, 480);
    ons_keys(&r, n)
}

#[test]
fn up_plays_the_held_notes_on_the_sixteenth_grid() {
    let mut r = rig();
    r.run(&press(&[67, 60, 64]), S16 * 7, 128);
    let ons = r.ons();
    assert_eq!(ons.iter().map(|n| n.1).collect::<Vec<_>>(), vec![60, 64, 67, 60, 64, 67, 60]);
    assert_eq!(ons.iter().map(|n| n.0).collect::<Vec<_>>(), (0..7).map(|i| i * S16).collect::<Vec<_>>(), "sample exact on the grid");
}

#[test]
fn every_style_walks_the_pool_in_its_own_order() {
    // Pressed out of order: 64 60 72 67 -> sorted 60 64 67 72.
    let held = [64, 60, 72, 67];
    let cases: [(&str, f32, Vec<u8>); 12] = [
        ("up", 0.0, vec![60, 64, 67, 72, 60, 64, 67, 72]),
        ("down", 1.0, vec![72, 67, 64, 60, 72, 67, 64, 60]),
        ("up-down", 2.0, vec![60, 64, 67, 72, 67, 64, 60, 64]),
        ("down-up", 3.0, vec![72, 67, 64, 60, 64, 67, 72, 67]),
        ("up&down", 4.0, vec![60, 64, 67, 72, 72, 67, 64, 60]),
        ("down&up", 5.0, vec![72, 67, 64, 60, 60, 64, 67, 72]),
        ("converge", 6.0, vec![60, 72, 64, 67, 60, 72, 64, 67]),
        ("diverge", 7.0, vec![67, 64, 72, 60, 67, 64, 72, 60]),
        ("low pedal", 8.0, vec![60, 64, 60, 67, 60, 72, 60, 64]),
        ("high pedal", 9.0, vec![60, 72, 64, 72, 67, 72, 60, 72]),
        ("play order", 10.0, vec![64, 60, 72, 67, 64, 60, 72, 67]),
        ("chord", 11.0, vec![60, 64, 67, 72]),
    ];
    for (name, style, want) in cases {
        let got = if name == "chord" {
            // Every step plays all four notes: two steps = eight note-ons.
            let mut r = rig();
            set(&mut r, p::STYLE, style);
            r.run(&press(&held), S16 * 2, 480);
            let ons = r.ons();
            assert_eq!(ons.len(), 8, "chord style: two steps of four notes");
            assert!(ons[..4].iter().all(|n| n.0 == 0) && ons[4..].iter().all(|n| n.0 == S16));
            let mut first: Vec<u8> = ons[..4].iter().map(|n| n.1).collect();
            first.sort();
            first
        } else {
            style_keys(style, &held, 8)
        };
        assert_eq!(got, want, "style {name}");
    }
}

#[test]
fn a_single_held_note_repeats_in_every_style() {
    for style in 0..=13 {
        assert_eq!(style_keys(style as f32, &[62], 4), vec![62, 62, 62, 62], "style {style}");
    }
}

#[test]
fn random_styles_stay_in_the_pool_and_other_never_repeats() {
    let held = [60, 63, 67, 70];
    let a = style_keys(12.0, &held, 64);
    assert!(a.iter().all(|k| held.contains(k)));
    assert!(a.windows(2).any(|w| w[0] != w[1]) && a.iter().collect::<std::collections::BTreeSet<_>>().len() >= 3, "it does wander: {a:?}");
    let b = style_keys(13.0, &held, 64);
    assert!(b.iter().all(|k| held.contains(k)));
    assert!(b.windows(2).all(|w| w[0] != w[1]), "random-other never plays the same note twice in a row: {b:?}");
}

#[test]
fn random_other_never_repeats_for_any_pool_size_and_ignores_where_it_starts() {
    for n in 1..=20usize {
        for seed in [0u64, 1, 7] {
            let seq: Vec<usize> = (-5..400).map(|s| random_other(s, seed, n)).collect();
            assert!(seq.iter().all(|i| *i < n));
            if n > 1 {
                assert!(seq.windows(2).all(|w| w[0] != w[1]), "n {n} seed {seed}");
            }
            if n > 3 {
                assert!(seq.iter().collect::<std::collections::BTreeSet<_>>().len() >= n.min(4), "n {n} uses several entries");
            }
        }
    }
}

#[test]
fn grid_covers_straight_dotted_and_triplet_rates() {
    // 1/2, 1/4, 1/4., 1/4T, 1/8, 1/8., 1/8T, 1/16, 1/16., 1/16T, 1/32, 1/32., 1/32T
    let want = [48_000, 24_000, 36_000, 16_000, 12_000, 18_000, 8_000, 6_000, 9_000, 4_000, 3_000, 4_500, 2_000];
    for (i, w) in want.iter().enumerate() {
        let mut r = rig();
        set(&mut r, p::RATE, i as f32);
        r.run(&press(&[60]), *w * 3 + 10, 512);
        let t: Vec<i64> = r.ons().iter().map(|n| n.0).collect();
        assert_eq!(t, vec![0, *w, 2 * *w, 3 * *w], "rate index {i}");
    }
}

#[test]
fn rate_follows_the_tempo() {
    let mut r = rig();
    r.bpm = 90.0;
    r.run(&press(&[60]), 40_000, 256);
    let t: Vec<i64> = r.ons().iter().map(|n| n.0).collect();
    // A sixteenth at 90 BPM is 8000 samples.
    assert_eq!(t, vec![0, 8000, 16_000, 24_000, 32_000]);
}

#[test]
fn gate_sets_the_note_off_as_a_share_of_the_step() {
    let mut r = rig();
    set(&mut r, p::GATE, 50.0);
    r.run(&press(&[60]), S16 * 3, 100);
    let notes = r.notes();
    assert_eq!((notes[0].0, notes[0].1), (0, true));
    assert_eq!((notes[1].0, notes[1].1), (3000, false), "50% of 6000");
    let mut r = rig();
    set(&mut r, p::GATE, 200.0);
    set(&mut r, p::STYLE, 0.0);
    r.run(&press(&[60, 64]), S16 * 4, 100);
    // Overlap: the second note starts before the first one ends.
    let n = r.notes();
    let first_off = n.iter().find(|x| !x.1 && x.2 == 60).unwrap().0;
    let second_on = n.iter().find(|x| x.1 && x.2 == 64).unwrap().0;
    assert!(second_on < first_off, "legato overlap at 200%: on {second_on} off {first_off}");
}

#[test]
fn an_overlapping_repeat_of_the_same_key_closes_the_old_note_first() {
    let mut r = rig();
    set(&mut r, p::GATE, 200.0);
    r.run(&press(&[60]), S16 * 4, 128);
    let n = r.notes();
    // on 0, then at 6000: off(60) before on(60), etc.
    assert_eq!((n[0].1, n[1].1, n[2].1), (true, false, true));
    assert_eq!(n[1].0, n[2].0, "closed at the very sample of the retrigger");
    r.run(&[(0, off(60))], S16 * 8, 128);
    r.assert_balanced();
}

#[test]
fn the_pattern_is_the_same_for_any_block_size_and_a_late_start() {
    let input = vec![(1000, on(60, 0.8)), (1000, on(64, 0.7)), (1001, on(67, 0.9)), (100_000, off(64)), (150_000, off(60)), (150_000, off(67))];
    let reference = {
        let mut r = rig();
        r.run(&input, 200_000, 4096);
        r.log_no_ids()
    };
    assert!(reference.len() > 20);
    for block in [1usize, 2, 7, 64, 100, 480, 1023, 4096] {
        let mut r = rig();
        r.run(&input, 200_000, block);
        assert_eq!(r.log_no_ids(), reference, "block size {block}");
    }
}

#[test]
fn retrigger_off_keeps_the_pattern_on_the_project_grid_wherever_the_transport_starts() {
    let mut a = rig();
    set(&mut a, p::RETRIGGER, 0.0);
    a.run(&press(&[60, 64, 67]), 8 * BEAT, 480);
    let full = a.keys();
    // Start 3.37 beats in: the first step is the one at beat 3.5 = step 14.
    let mut b = rig();
    set(&mut b, p::RETRIGGER, 0.0);
    b.start_at(3.37);
    b.run(&press(&[60, 64, 67]), BEAT, 333);
    let got = b.keys();
    assert!(!got.is_empty());
    assert_eq!(got, full[14..14 + got.len()].to_vec());
    let first = b.ons()[0].0;
    assert_eq!(first, ((3.5f64 - 3.37) * BEAT as f64).round() as i64, "first step lands on the beat grid");
}

#[test]
fn retrigger_note_restarts_at_the_next_step_after_each_new_note() {
    let mut r = rig();
    // First note mid-step; a second one two steps later.
    r.run(&[(1000, on(60, 0.8)), (2 * S16 + 100, on(64, 0.8))], 5 * S16, 64);
    let ons = r.ons();
    assert_eq!(ons[0], (S16, 60), "starts on the first step after the press");
    assert_eq!(ons[1], (2 * S16, 60));
    // The second press restarts at the start of the pattern: lowest first.
    assert_eq!(ons[2], (3 * S16, 60));
    assert_eq!(ons[3], (4 * S16, 64));
}

#[test]
fn retrigger_beat_and_bar_restart_the_pattern_on_their_boundaries() {
    for (mode, every) in [(2.0, BEAT), (3.0, 4 * BEAT)] {
        let mut r = rig();
        set(&mut r, p::RETRIGGER, mode);
        // Three notes on a sixteenth grid: 3 does not divide 4, so a restart shows.
        r.run(&press(&[60, 64, 67]), 2 * every, 500);
        let ons = r.ons();
        let at = |t: i64| ons.iter().find(|n| n.0 == t).map(|n| n.1);
        assert_eq!(at(0), Some(60));
        assert_eq!(at(every), Some(60), "mode {mode}: pattern restarts on the boundary");
        assert_eq!(at(every + S16), Some(64), "mode {mode}: and goes on from there");
        assert_eq!(at(every - S16), Some(60), "mode {mode}: the step before still ran the old cycle");
    }
}

#[test]
fn swing_delays_the_odd_steps() {
    let mut r = rig();
    set(&mut r, p::SWING, 100.0);
    r.run(&press(&[60]), S16 * 5, 100);
    let t: Vec<i64> = r.ons().iter().map(|n| n.0).collect();
    // 100% delays odd steps by half a step.
    assert_eq!(t, vec![0, S16 + S16 / 2, 2 * S16, 3 * S16 + S16 / 2, 4 * S16]);
    let mut r = rig();
    set(&mut r, p::SWING, 67.0);
    r.run(&press(&[60]), S16 * 3, 100);
    let t: Vec<i64> = r.ons().iter().map(|n| n.0).collect();
    assert_eq!(t[1] - S16, (0.335 * S16 as f64).round() as i64, "67% is the triplet feel");
}

#[test]
fn offset_shifts_the_whole_grid() {
    let mut r = rig();
    set(&mut r, p::OFFSET, 25.0);
    r.run(&press(&[60]), S16 * 3, 100);
    let t: Vec<i64> = r.ons().iter().map(|n| n.0).collect();
    assert_eq!(t, vec![S16 / 4, S16 + S16 / 4, 2 * S16 + S16 / 4]);
    // Early: the step before the press is gone, the next one is kept.
    let mut r = rig();
    set(&mut r, p::OFFSET, -25.0);
    r.run(&press(&[60]), S16 * 3, 100);
    let t: Vec<i64> = r.ons().iter().map(|n| n.0).collect();
    assert_eq!(t, vec![S16 - S16 / 4, 2 * S16 - S16 / 4, 3 * S16 - S16 / 4]);
}

#[test]
fn octaves_stack_the_pool_and_distance_sets_the_interval() {
    let mut r = rig();
    set(&mut r, p::OCTAVES, 2.0);
    r.run(&press(&[60, 64, 67]), S16 * 7, 128);
    assert_eq!(r.keys(), vec![60, 64, 67, 72, 76, 79, 60]);
    let mut r = rig();
    set(&mut r, p::OCTAVES, 3.0);
    set(&mut r, p::DISTANCE, 7.0);
    r.run(&press(&[60]), S16 * 3, 128);
    assert_eq!(r.keys(), vec![60, 67, 74]);
    // Down walks the stack from the top; negative distance goes down.
    let mut r = rig();
    set(&mut r, p::OCTAVES, 2.0);
    set(&mut r, p::DISTANCE, -12.0);
    r.run(&press(&[60, 64]), S16 * 4, 128);
    assert_eq!(r.keys(), vec![60, 64, 48, 52]);
}

#[test]
fn notes_pushed_out_of_the_midi_range_are_left_out() {
    let mut r = rig();
    set(&mut r, p::OCTAVES, 4.0);
    r.run(&press(&[100]), S16 * 4, 128);
    assert_eq!(r.keys(), vec![100, 112, 124], "136 would not exist: that step rests");
}

#[test]
fn chord_style_stacks_octaves_one_pass_per_step() {
    let mut r = rig();
    set(&mut r, p::STYLE, 11.0);
    set(&mut r, p::OCTAVES, 2.0);
    r.run(&press(&[60, 64]), S16 * 3, 128);
    let mut per_step: Vec<Vec<u8>> = vec![vec![]; 3];
    for (t, k) in r.ons() {
        per_step[(t / S16) as usize].push(k);
    }
    assert_eq!(per_step, vec![vec![60, 64], vec![72, 76], vec![60, 64]]);
}

#[test]
fn hold_keeps_the_notes_after_the_keys_are_released_until_a_new_press() {
    let mut r = rig();
    set(&mut r, p::HOLD, 1.0);
    let mut input = press(&[60, 64]);
    input.push((100, off(60)));
    input.push((100, off(64)));
    r.run(&input, S16 * 5, 100);
    assert_eq!(r.keys(), vec![60, 64, 60, 64, 60], "still playing after the release");
    // A new key after everything was released replaces the held set.
    r.run(&[(10, on(72, 0.8)), (400, off(72))], S16 * 4, 100);
    let late: Vec<u8> = r.ons().iter().filter(|n| n.0 >= 5 * S16 + S16).map(|n| n.1).collect();
    assert!(!late.is_empty() && late.iter().all(|k| *k == 72), "{late:?}");
    // Hold off again drops the released keys.
    set(&mut r, p::HOLD, 0.0);
    let before = r.ons().len();
    r.run(&[], S16 * 3, 100);
    assert_eq!(r.ons().len(), before, "nothing is held any more");
    r.assert_balanced();
}

#[test]
fn without_hold_releasing_the_keys_stops_the_arp_but_the_last_note_finishes() {
    let mut r = rig();
    set(&mut r, p::GATE, 100.0);
    let mut input = press(&[60]);
    input.push((S16 + 10, off(60)));
    r.run(&input, S16 * 6, 100);
    assert_eq!(r.keys(), vec![60, 60], "steps at 0 and S16, none after the release");
    r.assert_balanced();
}

#[test]
fn repeats_stop_the_pattern_after_that_many_cycles_until_the_next_press() {
    let mut r = rig();
    set(&mut r, p::REPEATS, 1.0); // 2 cycles
    r.run(&press(&[60, 64, 67]), S16 * 12, 128);
    assert_eq!(r.keys(), vec![60, 64, 67, 60, 64, 67]);
    r.run(&[(0, on(72, 0.5))], S16 * 6, 128);
    assert!(r.keys().len() > 6, "a new note starts it again");
}

#[test]
fn note_repeat_plays_each_note_several_steps_in_a_row() {
    let mut r = rig();
    set(&mut r, p::NOTE_REPEAT, 3.0); // 3 plays
    r.run(&press(&[60, 64]), S16 * 8, 128);
    assert_eq!(r.keys(), vec![60, 60, 60, 64, 64, 64, 60, 60]);
}

#[test]
fn velocity_follows_the_input_or_comes_from_the_step_table() {
    let mut r = rig();
    r.run(&[(0, on(60, 0.25)), (0, on(64, 1.0))], S16 * 2, 128);
    let v: Vec<f32> = r.notes().iter().filter(|n| n.1).map(|n| n.3).collect();
    assert_eq!(v, vec![0.25, 1.0], "the pressed velocity of each note");

    let mut r = rig();
    set(&mut r, p::VEL_MODE, 1.0);
    set(&mut r, p::VEL_FIXED, 100.0);
    r.run(&press(&[60]), S16 * 2, 128);
    assert!(r.notes().iter().filter(|n| n.1).all(|n| (n.3 - 100.0 / 127.0).abs() < 1e-6));

    let mut r = rig();
    set(&mut r, p::VEL_MODE, 2.0);
    set(&mut r, p::VEL_LEN, 3.0);
    for (i, v) in [127.0, 64.0, 32.0].iter().enumerate() {
        set(&mut r, p::VEL_STEP0 + i as u32, *v);
    }
    r.run(&press(&[60]), S16 * 7, 128);
    let v: Vec<i32> = r.notes().iter().filter(|n| n.1).map(|n| (n.3 * 127.0).round() as i32).collect();
    assert_eq!(v, vec![127, 64, 32, 127, 64, 32, 127], "one velocity per step, cycling");
}

#[test]
fn without_a_running_transport_it_plays_on_its_own_clock() {
    let mut r = rig();
    r.playing = false;
    r.run(&[(1000, on(60, 0.8)), (1000, on(64, 0.8))], 4 * S16, 77);
    let ons = r.ons();
    // Free beats start at 0: the next sixteenth after the press is at 6000.
    assert_eq!(ons[0], (S16, 60));
    assert_eq!(ons[1], (2 * S16, 64));
    // Let go: whatever sounds finishes.
    r.run(&[(0, off(60)), (0, off(64))], S16 * 6, 77);
    r.assert_balanced();
}

#[test]
fn a_position_jump_closes_what_sounds_and_restarts_on_the_grid() {
    let mut r = rig();
    set(&mut r, p::GATE, 200.0);
    r.run(&press(&[60, 64, 67]), S16 + 500, 100);
    // Loop wrap: back to the start of the loop.
    r.start_at(0.0);
    let at = r.clock;
    r.run(&[], S16 * 3, 100);
    let closed: std::collections::BTreeSet<u8> = r.notes().into_iter().filter(|n| !n.1 && n.0 == at).map(|n| n.2).collect();
    assert!(!closed.is_empty(), "the notes open at the jump are closed at the jump");
    // The restarted step retriggers key 60, so its note-off alone proves
    // nothing: what was sounding has to be closed too.
    assert!(closed.iter().any(|k| *k != 60), "the jump closes what was sounding, not just the key the pattern restarts on: {closed:?}");
    let after: Vec<_> = r.notes().into_iter().filter(|n| n.1 && n.0 >= at).collect();
    assert_eq!(after[0].0, at, "a step sits at the loop start");
    assert_eq!(after[0].2, 60, "and the pattern starts over");
    r.run(&[(0, off(60)), (0, off(64)), (0, off(67))], S16 * 10, 100);
    r.assert_balanced();
}

#[test]
fn other_events_pass_and_the_played_notes_do_not() {
    let mut r = rig();
    let cc = EventKind::ControlChange { channel: 0, cc: 1, value: 0.5 };
    let bend = EventKind::PitchBend { channel: 0, value: 0.25 };
    r.run(&[(10, cc), (20, bend), (30, on(60, 0.5))], 200, 64);
    assert!(r.log.contains(&(10, cc)) && r.log.contains(&(20, bend)));
    assert!(r.ons().is_empty(), "the pressed key itself never reaches the instrument");
}

#[test]
fn all_notes_off_clears_the_held_notes_and_closes_the_sounding_ones() {
    let mut r = rig();
    r.run(&press(&[60, 64]), S16 + 10, 64);
    r.run(&[(0, EventKind::AllNotesOff)], S16 * 4, 64);
    r.assert_balanced();
    let n = r.ons().len();
    assert_eq!(n, 2, "nothing plays after it");
}

#[test]
fn reset_forgets_the_held_notes_and_what_was_pending() {
    let mut r = rig();
    set(&mut r, p::GATE, 200.0);
    r.run(&press(&[60, 64]), S16 + 10, 64);
    r.p.reset();
    let n = r.ons().len();
    r.run(&[], S16 * 4, 64);
    assert_eq!(r.ons().len(), n, "no more notes");
    assert!(r.notes().iter().all(|x| x.1), "and no stray note-off from before the reset");
}

#[test]
fn no_note_stays_open_whatever_is_pressed_and_changed() {
    let mut seed = 0x1234_5678u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for round in 0..6 {
        let mut r = rig();
        r.p.reset();
        let total = 400_000i64;
        let mut input: Vec<(i64, EventKind)> = Vec::new();
        let mut down = [false; 12];
        let mut t = 0i64;
        while t < total {
            t += (rnd() % 9000) as i64;
            let k = (rnd() % 12) as usize;
            if down[k] {
                input.push((t, off(48 + k as u8)));
            } else {
                input.push((t, on(48 + k as u8, 0.2 + (rnd() % 80) as f32 / 100.0)));
            }
            down[k] = !down[k];
        }
        input.retain(|(t, _)| *t < total);
        // Which keys are still down once the last kept event has played.
        down = [false; 12];
        for (_, e) in &input {
            match *e {
                EventKind::NoteOn { key, .. } => down[key as usize - 48] = true,
                EventKind::NoteOff { key, .. } => down[key as usize - 48] = false,
                _ => {}
            }
        }
        // Param changes between slices.
        let slice = 20_000i64;
        let mut done = 0;
        while done < total {
            let part: Vec<(i64, EventKind)> = input.iter().filter(|(t, _)| *t >= done && *t < done + slice).map(|(t, k)| (t - done, *k)).collect();
            r.run(&part, slice, 1 + (rnd() % 700) as usize);
            done += slice;
            for (id, hi) in [(p::STYLE, 13), (p::RATE, 12), (p::OCTAVES, 4), (p::RETRIGGER, 3), (p::HOLD, 1), (p::REPEATS, 8), (p::NOTE_REPEAT, 4)] {
                if rnd() % 3 == 0 {
                    let lo = if id == p::OCTAVES || id == p::NOTE_REPEAT { 1 } else { 0 };
                    set(&mut r, id, (lo + rnd() % (hi - lo + 1)) as f32);
                }
            }
            if rnd() % 4 == 0 {
                set(&mut r, p::GATE, (1 + rnd() % 200) as f32);
            }
            if rnd() % 5 == 0 {
                set(&mut r, p::SWING, (rnd() % 101) as f32);
            }
            if rnd() % 7 == 0 {
                r.playing = !r.playing;
            }
            if rnd() % 9 == 0 {
                r.start_at((rnd() % 64) as f32 as f64 / 4.0);
            }
        }
        // Release everything and let it finish.
        let mut tail: Vec<(i64, EventKind)> = Vec::new();
        for (k, d) in down.iter().enumerate() {
            if *d {
                tail.push((0, off(48 + k as u8)));
            }
        }
        set(&mut r, p::HOLD, 0.0);
        r.run(&tail, 6 * BEAT, 256);
        r.assert_balanced();
        assert_eq!(r.dropped, 0, "round {round}");
        assert_eq!(r.p.dropped_events(), 0);
    }
}
