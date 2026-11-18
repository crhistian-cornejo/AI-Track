//! Fast automation editing: shapes over a range, the transform handles,
//! simplify, the arrow keys and the captures; each one undo step.

use super::*;
use crate::state::{AutoXform, KeyEdit};
use aitrack_model::automation::Shape;

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

fn lane_points(s: &AppState, track: TrackId, target: AutomationTarget) -> Vec<AutomationPoint> {
    s.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target).map(|l| l.points.clone())).unwrap_or_default()
}

#[test]
fn a_shape_over_the_selected_range_follows_the_grid_in_one_undo_step() {
    let Song { mut s, midi, .. } = song();
    pan_ramp(&mut s, midi);
    let before = lane_points(&s, midi, AutomationTarget::Pan);
    assert!(!s.insert_auto_shape(Shape::Sine), "no range selected: nothing happens");
    s.set_auto_range(midi, AutomationTarget::Pan, 2 * BEAT, 6 * BEAT);
    s.snap = Snap::Beat;
    let base = steps(&s);
    assert!(s.insert_auto_shape(Shape::Sine));
    assert_eq!(steps(&s), base + 1);
    // One cycle per beat at the 1/4 grid: the top a quarter beat in, the bottom three quarters.
    assert!(close(pan_at(&s, midi, 2 * BEAT + BEAT / 4), 1.0), "{}", pan_at(&s, midi, 2 * BEAT + BEAT / 4));
    assert!(close(pan_at(&s, midi, 2 * BEAT + 3 * BEAT / 4), 0.0));
    assert!(close(pan_at(&s, midi, 5 * BEAT + BEAT / 4), 1.0), "four cycles over four beats");
    assert!(close(pan_at(&s, midi, BEAT), 0.25) && close(pan_at(&s, midi, 7 * BEAT), 0.625), "outside is untouched");
    s.undo();
    assert_eq!(lane_points(&s, midi, AutomationTarget::Pan), before);
}

#[test]
fn every_shape_can_be_inserted_and_stays_in_range() {
    for shape in Shape::ALL {
        let Song { mut s, midi, .. } = song();
        pan_ramp(&mut s, midi);
        s.set_auto_range(midi, AutomationTarget::Pan, BEAT, 5 * BEAT);
        assert!(s.insert_auto_shape(shape), "{shape:?}");
        let pts = lane_points(&s, midi, AutomationTarget::Pan);
        assert!(pts.iter().all(|p| (0.0..=1.0).contains(&p.value)), "{shape:?}");
        assert!(pts.windows(2).all(|w| w[0].tick <= w[1].tick), "{shape:?} stays sorted");
    }
}

#[test]
fn a_handle_drag_scales_skews_or_stretches_in_one_undo_step() {
    let Song { mut s, midi, .. } = song();
    pan_ramp(&mut s, midi);
    s.set_auto_range(midi, AutomationTarget::Pan, 0, 4 * BEAT);
    let range = s.auto_sel.unwrap();
    let original = lane_points(&s, midi, AutomationTarget::Pan);
    let base = steps(&s);
    // Scale the 0..1 ramp to half its height about the bottom, in three frames.
    for f in [0.9, 0.7, 0.5] {
        assert!(s.xform_auto_range(range, &original, AutoXform::Scale { anchor: 0., factor: f }));
    }
    s.seal();
    assert_eq!(steps(&s), base + 1, "one drag, one step");
    assert!(close(pan_at(&s, midi, 4 * BEAT - 1), 0.5), "{}", pan_at(&s, midi, 4 * BEAT - 1));
    assert!(close(pan_at(&s, midi, 6 * BEAT), 0.75), "outside is untouched");
    s.undo();
    assert_eq!(lane_points(&s, midi, AutomationTarget::Pan), original);
    // A skew tilts it: the right edge gains 0.2.
    s.xform_auto_range(range, &original, AutoXform::Skew { left: 0., right: -0.2 });
    s.seal();
    assert!(close(pan_at(&s, midi, 2 * BEAT), 0.4), "{}", pan_at(&s, midi, 2 * BEAT));
    s.undo();
    // A time stretch to twice as long moves the selection with it.
    assert!(s.xform_auto_range(range, &original, AutoXform::Stretch { start: 0, end: 8 * BEAT }));
    s.seal();
    assert_eq!(s.auto_sel.map(|r| (r.start, r.end)), Some((0, 8 * BEAT)));
    assert!(close(pan_at(&s, midi, 4 * BEAT), 0.5), "the ramp now takes eight beats");
}

#[test]
fn simplify_envelope_drops_breakpoints_without_changing_the_line() {
    let Song { mut s, midi, .. } = song();
    let points: Vec<AutomationPoint> = (0..200).map(|i| AutomationPoint { tick: i * 40, value: 0.5 + 0.4 * (i as f32 * 0.07).sin(), curve: 0. }).collect();
    let lane = AutomationLane { points: points.clone(), ..AutomationLane::new(AutomationTarget::Pan) };
    assert!(s.edit("Automate", vec![Command::SetAutomation { track: midi, target: AutomationTarget::Pan, lane: Some(lane) }]));
    s.seal();
    let base = steps(&s);
    let removed = s.simplify_auto(midi, AutomationTarget::Pan).unwrap();
    assert!(removed > 100, "{removed} breakpoints went");
    assert_eq!(steps(&s), base + 1);
    let worst = (0..8000).map(|t| (pan_at(&s, midi, t) - AutomationLane { points: points.clone(), ..AutomationLane::new(AutomationTarget::Pan) }.value_at(t).unwrap()).abs()).fold(0., f32::max);
    assert!(worst <= 0.0045, "off by {worst}");
    assert_eq!(s.simplify_auto(midi, AutomationTarget::Volume), None, "no lane, nothing to do");
}

#[test]
fn arrow_keys_move_the_selected_range_and_shift_takes_fine_steps() {
    let Song { mut s, midi, .. } = song();
    pan_ramp(&mut s, midi);
    s.snap = Snap::Beat;
    assert_eq!(s.key_edit_auto_range(KeyEdit::Up), None, "no range: the arrow selects tracks as before");
    s.set_auto_range(midi, AutomationTarget::Pan, BEAT, 3 * BEAT);
    let base = steps(&s);
    // Up: 5 % higher; the fine key (shift) 1 %.
    let v = pan_at(&s, midi, 2 * BEAT);
    assert_eq!(s.key_edit_auto_range(KeyEdit::Up), Some(true));
    assert!(close(pan_at(&s, midi, 2 * BEAT), v + 0.05), "{v} -> {}", pan_at(&s, midi, 2 * BEAT));
    assert_eq!(s.key_edit_auto_range(KeyEdit::OctaveDown), Some(true));
    assert!(close(pan_at(&s, midi, 2 * BEAT), v + 0.04));
    assert_eq!(steps(&s), base + 2, "one step per key press");
    // Right: a grid step later, the selection follows; shift-right a fine step.
    assert_eq!(s.key_edit_auto_range(KeyEdit::Right), Some(true));
    assert_eq!(s.auto_sel.map(|r| (r.start, r.end)), Some((2 * BEAT, 4 * BEAT)));
    assert_eq!(s.key_edit_auto_range(KeyEdit::Longer), Some(true));
    assert_eq!(s.auto_sel.map(|r| r.start), Some(2 * BEAT + BEAT / 8));
    // Not before the start of the song.
    s.set_auto_range(midi, AutomationTarget::Pan, 0, BEAT);
    assert_eq!(s.key_edit_auto_range(KeyEdit::Left), Some(false));
    assert_eq!(s.key_edit_auto_range(KeyEdit::SemitoneUp), None, "not an arrow of this editor");
    // Delete is the range's own.
    s.set_auto_range(midi, AutomationTarget::Pan, BEAT, 3 * BEAT);
    assert_eq!(s.delete_auto_range(), Some(true));
}

#[test]
fn capture_automation_writes_the_recent_moves_where_they_happened() {
    let Song { mut s, midi, .. } = song();
    let device = s.add_stock_device(midi, "eq8", "EQ").unwrap();
    s.seal();
    // Not rolling: nothing is kept.
    s.set_param(device, 3, 0.1);
    s.seal();
    assert_eq!(s.capture_automation(), 0);
    // Rolling: a sweep of the knob over four beats, moved every sixteenth.
    for i in 0..=16 {
        s.capture.set_clock(Some(i * BEAT / 4));
        s.set_param(device, 3, i as f32 / 16.);
    }
    s.seal();
    let base = steps(&s);
    assert_eq!(s.capture_automation(), 1);
    assert_eq!(steps(&s), base + 1, "one undo step");
    let target = AutomationTarget::DeviceParam { device, param: 3 };
    let pts = lane_points(&s, midi, target);
    assert!(pts.len() < 17, "a straight sweep is thinned: {} points", pts.len());
    assert_eq!((pts.first().map(|p| p.tick), pts.last().map(|p| p.tick)), (Some(0), Some(4 * BEAT)));
    assert!(close(pts.last().unwrap().value, 1.0));
    assert!(s.shown_lanes(midi).contains(&target), "its lane is shown");
    assert_eq!(s.capture_automation(), 0, "captured once");
    s.undo();
    assert!(lane_points(&s, midi, target).is_empty());
}

#[test]
fn capture_midi_makes_a_clip_where_the_notes_were_played() {
    use crate::bridge::RecNote;
    let Song { mut s, midi, .. } = song();
    assert_eq!(s.capture_midi(), 0);
    let now = std::time::Instant::now();
    let note = |key, start, len| RecNote { track: midi, key, velocity: 0.7, start_tick: start, length_ticks: len, pass: 0 };
    s.capture.push_notes(now, [note(60, 8 * BEAT + 100, BEAT / 2), note(64, 9 * BEAT, BEAT)]);
    let base = steps(&s);
    assert_eq!(s.capture_midi(), 1);
    assert_eq!(steps(&s), base + 1, "one undo step");
    let t = s.project.track(midi).unwrap();
    let pl = &t.arrangement[0];
    assert_eq!(pl.start, 8 * BEAT, "on the beat of the first note");
    let Some(Source::Notes(src)) = s.project.clips.get(&pl.clip).and_then(|c| s.project.sources.get(&c.source)) else { panic!("a notes clip") };
    assert_eq!(src.notes.len(), 2);
    assert_eq!(src.notes.iter().map(|n| n.start).min(), Some(100), "notes start from the clip's tick 0");
    assert_eq!(s.capture_midi(), 0, "captured once");
    s.undo();
    assert!(s.project.track(midi).unwrap().arrangement.is_empty());
}
