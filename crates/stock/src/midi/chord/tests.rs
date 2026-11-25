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
        assert_eq!(a.log_no_ids(), b.log_no_ids(), "block {block}");
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

// ───────────────────────────── Chance ─────────────────────────────

/// Samples per millisecond at the rig's rate.
const MS: i64 = 48;

/// Press `key` at every time of `times` (released 40 samples later) and
/// return, per press, the sorted keys that sounded at that very sample.
fn presses(r: &mut Rig, key: u8, times: &[i64], frames: i64) -> Vec<Vec<u8>> {
    let mut input = Vec::new();
    for &t in times {
        input.push((t, on(key, 0.8)));
        input.push((t + 40, off(key)));
    }
    let start = r.clock;
    r.run(&input, frames, 256);
    times
        .iter()
        .map(|&t| {
            let mut k: Vec<u8> = r.ons().iter().filter(|n| n.0 == start + t).map(|n| n.1).collect();
            k.sort();
            k
        })
        .collect()
}

#[test]
fn a_voice_at_zero_chance_never_sounds_and_the_others_always_do() {
    let mut r = rig();
    set(&mut r, p::chance(1), 0.0);
    let times: Vec<i64> = (0..40).map(|i| i * 500).collect();
    for got in presses(&mut r, 60, &times, 21_000) {
        assert_eq!(got, vec![60, 67], "the second voice (64) is out, the first and the third stay");
    }
    r.assert_balanced();
}

#[test]
fn a_chance_follows_its_percentage_over_many_presses() {
    let mut r = rig();
    set(&mut r, p::chance(2), 50.0);
    let times: Vec<i64> = (0..400).map(|i| i * 300).collect();
    let got = presses(&mut r, 60, &times, 400 * 300 + 100);
    let fifth = got.iter().filter(|g| g.contains(&67)).count();
    assert!((140..=260).contains(&fifth), "{fifth} of 400 at 50 %");
    assert!(got.iter().all(|g| g.contains(&60) && g.contains(&64)), "the other voices are not touched");
    r.assert_balanced();
}

#[test]
fn the_same_song_position_and_seed_give_the_same_chances_and_another_seed_others() {
    let times: Vec<i64> = (0..60).map(|i| i * 400).collect();
    let run = |seed: f32| {
        let mut r = rig();
        set(&mut r, p::chance(1), 50.0);
        set(&mut r, p::chance(2), 50.0);
        set(&mut r, p::SEED, seed);
        presses(&mut r, 60, &times, 25_000)
    };
    assert_eq!(run(1.0), run(1.0));
    assert_ne!(run(1.0), run(2.0));
}

#[test]
fn one_chance_never_moves_the_draws_of_the_other_voices() {
    let times: Vec<i64> = (0..80).map(|i| i * 400).collect();
    let second = |third: f32| {
        let mut r = rig();
        set(&mut r, p::chance(1), 50.0);
        set(&mut r, p::chance(2), third);
        presses(&mut r, 60, &times, 33_000).iter().map(|g| g.contains(&64)).collect::<Vec<_>>()
    };
    assert_eq!(second(100.0), second(30.0));
}

#[test]
fn a_pattern_loop_repeats_the_chances_every_bar_count() {
    // 120 bpm, 4/4: a bar is 96 000 samples.
    const BAR: i64 = 96_000;
    let slots: Vec<i64> = (0..24).map(|i| i * 3_000).collect();
    let mut times = slots.clone();
    times.extend(slots.iter().map(|t| t + BAR));
    times.extend(slots.iter().map(|t| t + 3 * BAR));
    let run = |loop_choice: f32| {
        let mut r = rig();
        set(&mut r, p::chance(1), 50.0);
        set(&mut r, p::chance(2), 50.0);
        set(&mut r, p::LOOP, loop_choice);
        presses(&mut r, 60, &times, 4 * BAR)
    };
    let looped = run(1.0); // 1 bar
    assert_eq!(looped[..24], looped[24..48]);
    assert_eq!(looped[..24], looped[48..72]);
    let free = run(0.0);
    assert_ne!(free[..24], free[24..48], "without a loop every bar draws its own");
}

#[test]
fn a_press_whose_voices_all_fail_is_silent_and_so_is_its_release() {
    let mut r = rig();
    for v in 0..VOICES {
        set(&mut r, p::chance(v), 0.0);
    }
    r.run(&[(0, on(60, 0.8)), (100, off(60)), (150, on(62, 0.8)), (170, off(62))], 300, 64);
    assert!(r.log.is_empty(), "{:?}", r.log);
}

// ───────────────────────────── Strum ─────────────────────────────

#[test]
fn a_strum_spreads_the_notes_upward_over_its_time_and_the_key_ends_them_together() {
    let mut r = rig();
    set(&mut r, p::STRUM, 60.0);
    r.run(&[(100, on(60, 0.8)), (6_000, off(60))], 8_000, 256);
    assert_eq!(r.ons(), vec![(100, 60), (100 + 30 * MS, 64), (100 + 60 * MS, 67)]);
    assert!(r.notes().iter().filter(|n| !n.1).all(|n| n.0 == 6_000), "every note ends with the key");
    assert_eq!(r.notes().iter().filter(|n| !n.1).count(), 3);
    r.assert_balanced();
}

#[test]
fn a_strum_down_starts_from_the_highest_note() {
    let mut r = rig();
    set(&mut r, p::STRUM, 60.0);
    set(&mut r, p::STRUM_DIR, dir::DOWN);
    r.run(&[(100, on(60, 0.8)), (6_000, off(60))], 8_000, 256);
    assert_eq!(r.ons(), vec![(100, 67), (100 + 30 * MS, 64), (100 + 60 * MS, 60)]);
}

#[test]
fn down_up_goes_down_on_the_beat_and_up_between_the_beats() {
    let first_key = |beats: f64| {
        let mut r = rig();
        set(&mut r, p::STRUM, 40.0);
        set(&mut r, p::STRUM_DIR, dir::DOWN_UP);
        r.start_at(beats);
        r.run(&[(0, on(60, 0.8)), (4_000, off(60))], 4_000, 256);
        r.ons()[0].1
    };
    assert_eq!(first_key(0.0), 67, "on the beat: down");
    assert_eq!(first_key(0.5), 60, "the off-beat eighth: up");
    assert_eq!(first_key(1.0), 67);
    assert_eq!(first_key(1.5), 60);
    assert_eq!(first_key(0.45), 60, "the nearest eighth decides");
}

#[test]
fn a_random_strum_order_is_decided_by_the_song_position_and_uses_each_note_once() {
    let orders = |r: &mut Rig| -> Vec<Vec<u8>> {
        let mut input = Vec::new();
        for i in 0..40i64 {
            input.push((i * 6_000, on(60, 0.8)));
            input.push((i * 6_000 + 3_000, off(60)));
        }
        r.run(&input, 40 * 6_000, 512);
        (0..40i64)
            .map(|i| {
                let mut v: Vec<(i64, u8)> = r.ons().into_iter().filter(|o| o.0 >= i * 6_000 && o.0 < i * 6_000 + 3_000).collect();
                v.sort();
                v.into_iter().map(|o| o.1).collect()
            })
            .collect()
    };
    let mk = || {
        let mut r = rig();
        set(&mut r, p::STRUM, 40.0);
        set(&mut r, p::STRUM_DIR, dir::RANDOM);
        r
    };
    let (a, b) = (orders(&mut mk()), orders(&mut mk()));
    assert_eq!(a, b, "the same positions give the same orders");
    for o in &a {
        let mut sorted = o.clone();
        sorted.sort();
        assert_eq!(sorted, vec![60, 64, 67], "each note once: {o:?}");
    }
    let distinct: std::collections::BTreeSet<_> = a.iter().cloned().collect();
    assert!(distinct.len() >= 3, "{} distinct orders in 40 presses", distinct.len());
}

#[test]
fn the_tension_bends_the_gaps_of_the_strum_but_never_its_ends() {
    let delays = |tension: f32| -> Vec<i64> {
        let mut r = rig();
        for v in 0..VOICES {
            set(&mut r, p::on(v), 1.0);
        }
        set(&mut r, p::STRUM, 100.0);
        set(&mut r, p::STRUM_TENSION, tension);
        r.run(&[(0, on(60, 0.8)), (8_000, off(60))], 9_000, 256);
        r.ons().iter().map(|o| o.0).collect()
    };
    let (lin, late, early) = (delays(0.0), delays(100.0), delays(-100.0));
    assert_eq!(lin, vec![0, 960, 1_920, 2_880, 3_840, 4_800], "linear: a fifth of the time apart");
    for d in [&late, &early] {
        assert_eq!((d[0], d[5]), (0, 4_800), "the first note is at once and the last at the full time");
    }
    assert!(late[1] < lin[1] && late[4] < lin[4], "positive: the notes huddle at the start");
    assert!(early[1] > lin[1] && early[4] > lin[4], "negative: they huddle at the end");
    let gaps = |d: &[i64]| d.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>();
    assert!(gaps(&late).windows(2).all(|g| g[0] <= g[1]), "positive: the gaps widen {:?}", gaps(&late));
    assert!(gaps(&early).windows(2).all(|g| g[0] >= g[1]), "negative: the gaps narrow {:?}", gaps(&early));
}

#[test]
fn the_strum_velocity_tilts_the_notes_along_the_strum_even_without_time() {
    let velocities = |ramp: f32, direction: f32, strum: f32| -> Vec<(i64, u8, f32)> {
        let mut r = rig();
        set(&mut r, p::STRUM, strum);
        set(&mut r, p::STRUM_VEL, ramp);
        set(&mut r, p::STRUM_DIR, direction);
        r.run(&[(0, on(60, 0.4)), (4_000, off(60))], 5_000, 256);
        let mut v: Vec<(i64, u8, f32)> = r.notes().into_iter().filter(|n| n.1).map(|n| (n.0, n.2, n.3)).collect();
        v.sort_by_key(|n| (n.0, n.1));
        v
    };
    let close = |got: &[(i64, u8, f32)], want: [f32; 3]| {
        for (g, w) in got.iter().zip(want) {
            assert!((g.2 - w).abs() < 1e-5, "{got:?} wanted {want:?}");
        }
    };
    close(&velocities(100.0, dir::UP, 30.0), [0.2, 0.4, 0.6]);
    close(&velocities(-100.0, dir::UP, 30.0), [0.6, 0.4, 0.2]);
    let down = velocities(100.0, dir::DOWN, 30.0);
    assert_eq!(down.iter().map(|n| n.1).collect::<Vec<_>>(), vec![67, 64, 60]);
    close(&down, [0.2, 0.4, 0.6]);
    // No time: all at once, still tilted from the lowest note up.
    let tilt = velocities(100.0, dir::UP, 0.0);
    assert!(tilt.iter().all(|n| n.0 == 0));
    close(&tilt, [0.2, 0.4, 0.6]);
    assert_eq!(tilt.iter().map(|n| n.1).collect::<Vec<_>>(), vec![60, 64, 67]);
}

#[test]
fn the_positions_and_gains_of_a_strum_are_shared_with_the_editor() {
    assert_eq!(strum_position(0, 1, 0.7), 0.0, "a lone note starts at once");
    let linear: Vec<f64> = (0..5).map(|i| strum_position(i, 5, 0.0)).collect();
    assert_eq!(linear, vec![0.0, 0.25, 0.5, 0.75, 1.0]);
    assert!(strum_position(1, 5, 1.0) < 0.25 && strum_position(1, 5, -1.0) > 0.25);
    assert_eq!((strum_position(0, 5, 1.0), strum_position(4, 5, 1.0), strum_position(4, 5, -1.0)), (0.0, 1.0, 1.0));
    assert_eq!(strum_gain(0, 1, 1.0), 1.0);
    assert_eq!((strum_gain(0, 3, 1.0), strum_gain(1, 3, 1.0), strum_gain(2, 3, 1.0)), (0.5, 1.0, 1.5));
    assert_eq!((strum_gain(0, 3, -1.0), strum_gain(2, 3, -1.0)), (1.5, 0.5));
}

#[test]
fn a_key_released_before_a_note_has_started_cancels_it() {
    let mut r = rig();
    set(&mut r, p::STRUM, 100.0); // 60 at 0, 64 at 2400, 67 at 4800
    r.run(&[(0, on(60, 0.8)), (3_000, off(60))], 12_000, 256);
    assert_eq!(r.ons(), vec![(0, 60), (2_400, 64)]);
    let offs: Vec<(i64, u8)> = r.notes().into_iter().filter(|n| !n.1).map(|n| (n.0, n.2)).collect();
    assert_eq!(offs.len(), 2);
    assert!(offs.iter().all(|o| o.0 == 3_000), "{offs:?}");
    r.assert_balanced();
}

#[test]
fn a_note_due_at_the_very_moment_of_the_release_does_not_start() {
    let mut r = rig();
    set(&mut r, p::STRUM, 100.0);
    r.run(&[(0, on(60, 0.8)), (2_400, off(60))], 12_000, 256);
    assert_eq!(r.ons(), vec![(0, 60)]);
    r.assert_balanced();
}

#[test]
fn pressing_a_strummed_key_again_drops_what_the_first_press_had_not_played() {
    let mut r = rig();
    set(&mut r, p::STRUM, 100.0);
    r.run(&[(0, on(60, 0.8)), (1_000, on(60, 0.8)), (9_000, off(60))], 12_000, 256);
    // The first press played only 60; the second starts over at 1000.
    assert_eq!(r.ons(), vec![(0, 60), (1_000, 60), (3_400, 64), (5_800, 67)]);
    r.assert_balanced();
}

#[test]
fn all_notes_off_and_reset_drop_the_pending_notes() {
    let mut r = rig();
    set(&mut r, p::STRUM, 100.0);
    r.run(&[(0, on(60, 0.8)), (500, EventKind::AllNotesOff)], 12_000, 256);
    assert_eq!(r.ons(), vec![(0, 60)]);
    r.assert_balanced();
    let mut r = rig();
    set(&mut r, p::STRUM, 100.0);
    r.run(&[(0, on(60, 0.8))], 500, 256);
    r.p.reset();
    r.run(&[], 12_000, 256);
    assert_eq!(r.ons(), vec![(0, 60)], "a reset forgets what was still to play");
}

#[test]
fn a_note_two_strummed_chords_share_is_retriggered_when_the_second_starts_and_ends_with_the_last_key() {
    let mut r = rig();
    set(&mut r, p::STRUM, 100.0);
    // 60 → 60 at 0, 64 at 2400, 67 at 4800; 64 → 64 at 100, 68 at 2500, 71 at 4900.
    r.run(&[(0, on(60, 0.8)), (100, on(64, 0.8)), (6_000, off(60)), (7_000, off(64))], 12_000, 256);
    let of64: Vec<(i64, bool)> = r.notes().into_iter().filter(|n| n.2 == 64).map(|n| (n.0, n.1)).collect();
    assert_eq!(of64, vec![(100, true), (2_400, false), (2_400, true), (7_000, false)], "off+on when the first chord's 64 arrives, one off at the very end");
    r.assert_balanced();
}

#[test]
fn the_strum_is_counted_in_samples_of_the_current_rate() {
    let mut proc = create();
    proc.prepare(96_000.0, 4096);
    set_plain(proc.as_mut(), p::STRUM, 60.0);
    let mut out = Vec::with_capacity(64);
    let mut dropped = 0;
    let mut times = Vec::new();
    let tr = aitrack_dsp::Transport::default();
    for block in 0..8u32 {
        let events: Vec<aitrack_dsp::Event> = if block == 0 { vec![aitrack_dsp::Event { offset: 10, kind: on(60, 0.8) }] } else { Vec::new() };
        out.clear();
        let ctx = aitrack_dsp::MidiContext { sample_rate: 96_000.0, frames: 1_000, events: &events, transport: &tr };
        proc.process_midi(&ctx, &mut aitrack_dsp::EventSink::new(&mut out, &mut dropped));
        times.extend(out.iter().filter(|e| matches!(e.kind, EventKind::NoteOn { .. })).map(|e| block as i64 * 1_000 + e.offset as i64));
    }
    // 60 ms at 96 kHz = 5760 samples between the first and the last note.
    assert_eq!(times, vec![10, 10 + 2_880, 10 + 5_760]);
}

#[test]
fn the_output_is_the_same_for_any_block_size_with_a_strum_chances_and_a_random_direction() {
    let input = vec![
        (10, on(60, 0.8)),
        (20, on(64, 0.6)),
        (1_300, off(60)),
        (2_900, off(64)),
        (3_000, on(48, 1.0)),
        (3_050, on(72, 0.7)),
        (7_000, off(48)),
        (9_999, off(72)),
    ];
    let mk = || {
        let mut r = rig();
        set(&mut r, p::STRUM, 90.0);
        set(&mut r, p::STRUM_DIR, dir::RANDOM);
        set(&mut r, p::STRUM_TENSION, 40.0);
        set(&mut r, p::STRUM_VEL, 30.0);
        set(&mut r, p::chance(1), 60.0);
        set(&mut r, p::chance(2), 60.0);
        for v in 3..VOICES {
            set(&mut r, p::on(v), 1.0);
            set(&mut r, p::chance(v), 70.0);
        }
        r
    };
    let mut a = mk();
    a.run(&input, 14_000, 4_096);
    assert!(a.ons().len() > 4, "something to compare");
    for block in [1usize, 3, 100, 777, 1_000] {
        let mut b = mk();
        b.run(&input, 14_000, block);
        assert_eq!(a.log_no_ids(), b.log_no_ids(), "block {block}");
    }
}

#[test]
fn no_note_stays_open_under_random_play_with_strum_chance_learn_and_changing_parameters() {
    let mut seed = 0xC0FF_EE11u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..5 {
        let mut r = rig();
        let mut down = [false; 24];
        for _slice in 0..60 {
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
                if rnd() % 3 == 0 {
                    set(&mut r, p::on(v), (rnd() % 2) as f32);
                    set(&mut r, p::interval(v), (rnd() % 73) as f32 - 36.0);
                    set(&mut r, p::chance(v), (rnd() % 101) as f32);
                }
            }
            set(&mut r, p::STRUM, if rnd() % 4 == 0 { 0.0 } else { (rnd() % 501) as f32 });
            set(&mut r, p::STRUM_DIR, (rnd() % 4) as f32);
            set(&mut r, p::STRUM_TENSION, (rnd() % 201) as f32 - 100.0);
            set(&mut r, p::STRUM_VEL, (rnd() % 201) as f32 - 100.0);
            set(&mut r, p::LEARN, if rnd() % 6 == 0 { 1.0 } else { 0.0 });
            set(&mut r, p::FIT, (rnd() % 2) as f32);
        }
        let tail: Vec<_> = down.iter().enumerate().filter(|(_, d)| **d).map(|(k, _)| (0, off(40 + k as u8 * 3))).collect();
        r.run(&tail, 40_000, 100);
        r.assert_balanced();
        assert_eq!(r.p.dropped_events(), 0);
    }
}

#[test]
fn the_older_presets_do_not_notice_the_new_parameters_and_the_new_ones_use_them() {
    let defaults: Vec<(u32, f32)> = params().iter().filter(|i| i.id >= p::CHANCE0).map(|i| (i.id, i.default_normalized())).collect();
    let (mut older, mut strum, mut chance) = (0, 0, 0);
    for r in crate::presets::list(ID) {
        let preset = crate::presets::load(&r.path).unwrap();
        let mut c = Chord::new();
        preset.apply(&mut c);
        let untouched = defaults.iter().all(|(id, d)| (c.get_param(*id) - d).abs() < 1e-6);
        match r.category.as_str() {
            "Strum" => {
                assert!(c.get_param(p::STRUM) > 0.0, "{} has no strum", r.name);
                strum += 1;
            }
            "Chance" => {
                assert!((0..VOICES).any(|v| c.get_param(p::chance(v)) < 1.0), "{} has no chance", r.name);
                chance += 1;
            }
            _ => {
                assert!(untouched, "{} sets a new parameter: it would sound different", r.name);
                older += 1;
            }
        }
    }
    assert_eq!((older, strum, chance), (11, 3, 2));
}

// ───────────────────────────── Learn ─────────────────────────────

fn meter_of(r: &Rig) -> Vec<f32> {
    let mut m = [0.0f32; 32];
    let n = r.p.meter(&mut m);
    m[..n].to_vec()
}

#[test]
fn while_learning_the_played_notes_pass_through_alone_and_unchanged() {
    let mut r = rig();
    set(&mut r, p::LEARN, 1.0);
    r.run(&[(10, on(60, 0.8)), (500, off(60))], 1_000, 64);
    assert_eq!(r.notes().len(), 2, "one note on and its off, not the chord: {:?}", r.notes());
    assert_eq!((r.notes()[0].2, r.notes()[0].3), (60, 0.8));
    r.assert_balanced();
    set(&mut r, p::LEARN, 0.0);
    r.run(&[(0, on(60, 0.8)), (100, off(60))], 200, 64);
    assert_eq!(r.ons().len(), 4, "with Learn off the chord is back");
}

#[test]
fn a_learned_gesture_is_published_once_the_last_key_is_released() {
    let mut r = rig();
    set(&mut r, p::LEARN, 1.0);
    r.run(&[(0, on(60, 0.8)), (10, on(67, 0.8)), (20, on(64, 0.8)), (300, off(67))], 400, 64);
    let m = meter_of(&r);
    assert_eq!(m.len(), meter::LEN);
    assert_eq!((m[meter::SERIAL], m[meter::LIVE]), (0.0, 3.0), "still held: nothing published, three keys live");
    r.run(&[(0, off(60)), (10, off(64))], 100, 64);
    let m = meter_of(&r);
    assert_eq!((m[meter::SERIAL], m[meter::COUNT], m[meter::LIVE]), (1.0, 3.0, 0.0));
    assert_eq!(&m[meter::KEYS..meter::KEYS + 3], &[60.0, 64.0, 67.0], "sorted, lowest first");
}

#[test]
fn the_capture_is_everything_pressed_in_the_gesture_and_a_key_pressed_twice_counts_once() {
    let mut r = rig();
    set(&mut r, p::LEARN, 1.0);
    // 60 and 64 are released before 67 is pressed, but the hand never left the keys.
    r.run(&[(0, on(60, 0.8)), (10, on(64, 0.8)), (20, off(60)), (30, on(60, 0.8)), (40, on(67, 0.8)), (50, off(64)), (60, off(60)), (70, off(67))], 200, 64);
    let m = meter_of(&r);
    assert_eq!((m[meter::SERIAL], m[meter::COUNT]), (1.0, 3.0));
    assert_eq!(&m[meter::KEYS..meter::KEYS + 3], &[60.0, 64.0, 67.0]);
    r.assert_balanced();
}

#[test]
fn only_the_lowest_six_keys_of_a_big_gesture_are_kept() {
    let mut r = rig();
    set(&mut r, p::LEARN, 1.0);
    let mut input = Vec::new();
    for (i, k) in (50u8..58).rev().enumerate() {
        input.push((i as i64, on(k, 0.8)));
    }
    for (i, k) in (50u8..58).enumerate() {
        input.push((100 + i as i64, off(k)));
    }
    r.run(&input, 200, 64);
    let m = meter_of(&r);
    assert_eq!(m[meter::COUNT], 6.0);
    assert_eq!(&m[meter::KEYS..meter::KEYS + 6], &[50.0, 51.0, 52.0, 53.0, 54.0, 55.0]);
}

#[test]
fn every_gesture_is_a_new_chord_with_a_new_serial_and_nothing_is_learned_with_learn_off() {
    let mut r = rig();
    r.run(&[(0, on(60, 0.8)), (50, off(60))], 100, 64);
    assert_eq!(meter_of(&r)[meter::SERIAL], 0.0, "Learn off: nothing");
    set(&mut r, p::LEARN, 1.0);
    r.run(&[(0, on(60, 0.8)), (10, on(64, 0.8)), (50, off(60)), (60, off(64))], 100, 64);
    r.run(&[(0, on(55, 0.8)), (10, on(58, 0.8)), (50, off(55)), (60, off(58))], 100, 64);
    let m = meter_of(&r);
    assert_eq!((m[meter::SERIAL], m[meter::COUNT]), (2.0, 2.0));
    assert_eq!(&m[meter::KEYS..meter::KEYS + 2], &[55.0, 58.0], "the last gesture wins");
}

#[test]
fn keys_already_down_when_learn_goes_on_are_not_part_of_the_capture() {
    let mut r = rig();
    r.run(&[(0, on(48, 0.8))], 50, 64);
    set(&mut r, p::LEARN, 1.0);
    r.run(&[(0, on(60, 0.8)), (10, on(64, 0.8)), (40, off(60)), (50, off(64)), (60, off(48))], 100, 64);
    let m = meter_of(&r);
    assert_eq!(m[meter::COUNT], 2.0);
    assert_eq!(&m[meter::KEYS..meter::KEYS + 2], &[60.0, 64.0]);
    r.assert_balanced();
}

#[test]
fn the_meter_fits_any_buffer() {
    let r = rig();
    let mut small = [9.0f32; 4];
    assert_eq!(r.p.meter(&mut small), 4);
    assert_eq!(meter_of(&r).len(), meter::LEN);
    const { assert!(meter::LEN <= 320, "a meter holds at most 320 values") };
}

#[test]
fn a_learned_chord_becomes_voices_above_its_lowest_key_and_switches_learn_off() {
    let edits = learn_edits(&[67, 60, 63, 70, 63]);
    let mut want = Vec::new();
    for (v, interval) in [0.0, 3.0, 7.0, 10.0].into_iter().enumerate() {
        want.extend([(p::on(v), 1.0), (p::interval(v), interval), (p::level(v), 100.0)]);
    }
    want.extend([(p::on(4), 0.0), (p::on(5), 0.0), (p::LEARN, 0.0)]);
    assert_eq!(edits, want, "sorted, without the repeated 63, the rest switched off");
    // Playing a key with these voices gives the chord on that key.
    let mut r = rig();
    for (id, v) in &edits {
        set(&mut r, *id, *v);
    }
    r.run(&[(0, on(62, 0.8)), (100, off(62))], 200, 64);
    assert_eq!(keys_at(&r, 0), vec![62, 65, 69, 72]);
}

#[test]
fn learned_edits_keep_the_lowest_six_leave_out_what_is_out_of_reach_and_ignore_nothing() {
    let many = learn_edits(&[46, 40, 41, 42, 43, 44, 45]);
    let intervals: Vec<f32> = (0..VOICES).map(|v| many.iter().find(|e| e.0 == p::interval(v)).unwrap().1).collect();
    assert_eq!(intervals, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    let far = learn_edits(&[36, 40, 80]);
    let on_voices = (0..VOICES).filter(|v| far.contains(&(p::on(*v), 1.0))).count();
    assert_eq!(on_voices, 2, "40 is four semitones up, 80 is 44 up: out of reach (±36)");
    assert!(learn_edits(&[]).is_empty());
}
