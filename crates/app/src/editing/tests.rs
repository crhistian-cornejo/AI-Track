use super::*;
use crate::state::{AppState, KeyEdit, Snap};

const BEAT: Tick = PPQ;

/// An empty song with two MIDI tracks and an audio track, snapping off.
struct Song {
    s: AppState,
    midi: TrackId,
    midi2: TrackId,
    audio: TrackId,
}

fn song() -> Song {
    let mut p = Project::new("Test");
    let ids = [p.alloc_track_id(), p.alloc_track_id(), p.alloc_track_id()];
    for (i, (id, kind, name)) in [(ids[0], TrackKind::Instrument, "Keys"), (ids[1], TrackKind::Instrument, "Bass"), (ids[2], TrackKind::Audio, "Drums")].into_iter().enumerate() {
        Command::AddTrack { track: Box::new(Track::new(id, kind, name, Color::palette(i))), index: i }.apply(&mut p).unwrap();
    }
    let mut s = AppState::new(p);
    s.snap = Snap::Off;
    Song { s, midi: ids[0], midi2: ids[1], audio: ids[2] }
}

/// A MIDI clip with `notes` (key, start, length) at `start`.
fn midi_clip(s: &mut AppState, track: TrackId, start: Tick, length: Tick, notes: &[(u8, Tick, Tick)]) -> PlacementId {
    let (source, clip, id) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
    let notes = notes.iter().map(|&(k, at, len)| Note::new(s.project.alloc_note_id(), k, at, len, 0.8)).collect();
    assert!(s.edit("Add clip", vec![
        Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource { notes })) },
        Command::AddClip { id: clip, clip: Box::new(Clip::new("Riff", Color::palette(1), source, length)) },
        Command::PlaceClip { track, placement: Placement { id, clip, start, lane: 0 } },
    ]));
    id
}

/// An audio clip of a one-second file (two beats at 120 BPM) at `start`.
fn audio_clip(s: &mut AppState, track: TrackId, start: Tick, length: Tick) -> PlacementId {
    let (source, clip, id) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
    let file = AudioSource { path: "Samples/Kick.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames: 48_000, warp: Warp::default(), key: None };
    assert!(s.edit("Add clip", vec![
        Command::AddSource { id: source, source: Box::new(Source::Audio(file)) },
        Command::AddClip { id: clip, clip: Box::new(Clip::new("Kick", Color::palette(2), source, length)) },
        Command::PlaceClip { track, placement: Placement { id, clip, start, lane: 0 } },
    ]));
    id
}

/// The clips of a track as (start, length), in time order.
fn spans(s: &AppState, track: TrackId) -> Vec<(Tick, Tick)> {
    let mut out: Vec<_> = s.project.track(track).unwrap().arrangement.iter().map(|pl| (pl.start, s.project.clips[&pl.clip].length)).collect();
    out.sort();
    out
}

fn clip_of(s: &AppState, id: PlacementId) -> Clip {
    s.project.clips[&s.placement(id).unwrap().1.clip].clone()
}

fn steps(s: &AppState) -> usize {
    s.history.steps().count()
}

#[test]
fn pasting_twice_lays_two_copies_end_to_end_and_moves_the_marker() {
    let Song { mut s, audio, .. } = song();
    // An odd length: the copies must touch, wherever the grid is.
    let kick = audio_clip(&mut s, audio, 0, 1000);
    s.click_clip(kick, false);
    assert!(s.copy());
    assert_eq!(s.ed.marker, 1000, "the marker waits right after what was copied");
    let before = steps(&s);
    assert!(s.paste());
    assert_eq!((spans(&s, audio), s.ed.marker), (vec![(0, 1000), (1000, 1000)], 2000));
    assert!(s.paste());
    assert_eq!((spans(&s, audio), s.ed.marker), (vec![(0, 1000), (1000, 1000), (2000, 1000)], 3000));
    assert_eq!(steps(&s), before + 2, "each paste is one undo step");
    // The last copy is the selection; all three play the same file.
    let pasted = s.clip_selection();
    assert_eq!(pasted.iter().map(|(_, pl, _)| pl.start).collect::<Vec<_>>(), [2000]);
    assert_eq!(s.project.sources.len(), 1);
    assert!(s.undo() && s.undo());
    assert_eq!(spans(&s, audio), [(0, 1000)]);
    assert!(s.clip_selection().is_empty(), "undo prunes the selection");
}

#[test]
fn a_pasted_audio_clip_keeps_its_source_fades_and_loop() {
    let Song { mut s, audio, .. } = song();
    let kick = audio_clip(&mut s, audio, 0, 2 * BEAT);
    let id = s.placement(kick).unwrap().1.clip;
    let shaped = Clip {
        fade_in: Fade { length: 120, curve: 0.5 },
        fade_out: Fade { length: 300, curve: -0.25 },
        looping: Some(LoopRegion { start: 0, end: BEAT }),
        gain_db: -3.,
        envelopes: vec![AutomationLane { points: vec![AutomationPoint { tick: 0, value: 1., curve: 0. }, AutomationPoint { tick: BEAT, value: 0.5, curve: 0. }], ..AutomationLane::new(AutomationTarget::Volume) }],
        ..s.project.clips[&id].clone()
    };
    assert!(s.edit("Shape", vec![Command::UpdateClip { id, clip: Box::new(shaped.clone()) }]));
    s.click_clip(kick, false);
    assert!(s.copy());
    s.set_marker(16 * BEAT, None);
    assert!(s.paste());
    let (_, pl, _) = s.clip_selection()[0].clone();
    assert_eq!(pl.start, 16 * BEAT);
    assert_ne!(pl.clip, id, "a clip of its own");
    assert_eq!(s.project.clips[&pl.clip], shaped, "same source, fades, loop, gain and envelope");
    assert_eq!(s.project.sources.len(), 1, "the file is shared, not copied");
    // Cut takes the original away with its source; the clipboard still pastes it.
    s.click_clip(kick, false);
    assert!(s.cut());
    assert_eq!(s.ed.marker, 0, "the marker stays where the cut began");
    s.select_clips(s.project.track(audio).unwrap().arrangement.iter().map(|p| p.id).collect::<Vec<_>>(), None);
    assert_eq!(s.delete_arrangement(), Some(true));
    assert!(s.project.sources.is_empty() && s.project.clips.is_empty(), "nothing is left behind");
    assert!(s.paste());
    let back = clip_of(&s, s.clip_selection()[0].1.id);
    assert_eq!(Clip { source: shaped.source, ..back.clone() }, shaped);
    assert!(matches!(&s.project.sources[&back.source], Source::Audio(a) if a.path.ends_with("Kick.wav")));
}

#[test]
fn duplicating_a_multi_selection_tiles_it() {
    let Song { mut s, midi, audio, .. } = song();
    let riff = midi_clip(&mut s, midi, 0, 2 * BEAT, &[(60, 0, BEAT)]);
    let kick = audio_clip(&mut s, audio, BEAT, 2 * BEAT);
    s.click_clip(riff, false);
    s.click_clip(kick, true);
    assert_eq!(s.selection_span(), Some((0, 3 * BEAT)));
    let before = steps(&s);
    assert!(s.duplicate_selection());
    assert_eq!(spans(&s, midi), [(0, 2 * BEAT), (3 * BEAT, 2 * BEAT)]);
    assert_eq!(spans(&s, audio), [(BEAT, 2 * BEAT), (4 * BEAT, 2 * BEAT)]);
    // The copies are the selection, so the next one lands after them.
    assert_eq!(s.selection_span(), Some((3 * BEAT, 6 * BEAT)));
    assert!(s.duplicate_selection());
    assert_eq!(spans(&s, midi), [(0, 2 * BEAT), (3 * BEAT, 2 * BEAT), (6 * BEAT, 2 * BEAT)]);
    assert_eq!(spans(&s, audio), [(BEAT, 2 * BEAT), (4 * BEAT, 2 * BEAT), (7 * BEAT, 2 * BEAT)]);
    assert_eq!(steps(&s), before + 2);
    // MIDI copies own their notes; audio copies share the file.
    let sources: BTreeSet<SourceId> = s.project.track(midi).unwrap().arrangement.iter().map(|pl| s.project.clips[&pl.clip].source).collect();
    assert_eq!(sources.len(), 3);
    assert_eq!(s.project.sources.values().filter(|x| matches!(x, Source::Audio(_))).count(), 1);
    assert!(s.undo());
    assert_eq!(spans(&s, midi).len(), 2);
}

#[test]
fn duplicating_a_time_selection_repeats_the_part_inside_it() {
    let Song { mut s, midi, .. } = song();
    midi_clip(&mut s, midi, 0, 8 * BEAT, &[(60, 0, BEAT)]);
    s.set_time_selection((2 * BEAT, midi), (4 * BEAT, midi));
    assert!(s.duplicate_selection());
    // The two beats land right after the selection, replacing what was there.
    assert_eq!(spans(&s, midi), [(0, 4 * BEAT), (4 * BEAT, 2 * BEAT), (6 * BEAT, 2 * BEAT)]);
    let sel = s.time_selection().unwrap();
    assert_eq!((sel.start, sel.end), (4 * BEAT, 6 * BEAT), "the selection moved onto the copy");
    let copy = s.project.track(midi).unwrap().arrangement.iter().find(|pl| pl.start == 4 * BEAT).unwrap();
    assert_eq!(s.project.clips[&copy.clip].offset, 2 * BEAT, "it shows what the selection covered");
}

#[test]
fn deleting_a_time_selection_cuts_the_clips_crossing_its_edges() {
    let Song { mut s, midi, audio, .. } = song();
    let long = midi_clip(&mut s, midi, 0, 8 * BEAT, &[(60, 0, BEAT), (64, 5 * BEAT, BEAT)]);
    midi_clip(&mut s, midi, 9 * BEAT, BEAT, &[]);
    audio_clip(&mut s, audio, 2 * BEAT, 2 * BEAT);
    let before = steps(&s);
    s.set_time_selection((2 * BEAT, midi), (4 * BEAT, midi));
    assert!(s.delete_selection());
    assert_eq!(spans(&s, midi), [(0, 2 * BEAT), (4 * BEAT, 4 * BEAT), (9 * BEAT, BEAT)]);
    assert_eq!(clip_of(&s, long).length, 2 * BEAT, "the left part is the original clip");
    let right = s.project.track(midi).unwrap().arrangement.iter().find(|pl| pl.start == 4 * BEAT).unwrap();
    assert_eq!(s.project.clips[&right.clip].offset, 4 * BEAT, "the right part plays on from the cut");
    assert_eq!(spans(&s, audio), [(2 * BEAT, 2 * BEAT)], "tracks outside the selection are left alone");
    assert_eq!(steps(&s), before + 1);
    // Across both tracks a clip wholly inside goes.
    s.set_time_selection((BEAT, midi), (5 * BEAT, audio));
    assert_eq!(s.time_selection().unwrap().tracks.len(), 3, "the selection covers the lanes in between");
    assert!(s.delete_selection());
    assert_eq!(spans(&s, midi), [(0, BEAT), (5 * BEAT, 3 * BEAT), (9 * BEAT, BEAT)]);
    assert!(spans(&s, audio).is_empty());
    assert!(s.undo() && s.undo());
    assert_eq!(spans(&s, midi), [(0, 8 * BEAT), (9 * BEAT, BEAT)]);
}

#[test]
fn copying_a_time_selection_takes_the_parts_inside_it() {
    let Song { mut s, midi, midi2, .. } = song();
    midi_clip(&mut s, midi, 0, 8 * BEAT, &[(60, 0, BEAT)]);
    let project = s.project.clone();
    s.set_time_selection((2 * BEAT, midi), (5 * BEAT, midi));
    assert!(s.copy());
    assert_eq!(s.project, project, "copying changes nothing");
    let Some(Clipboard::Clips { span, rows }) = &s.ed.clipboard else { panic!("clips on the clipboard") };
    assert_eq!((*span, rows.len(), rows[0].seeds.len()), (3 * BEAT, 1, 1));
    assert_eq!((rows[0].seeds[0].at, rows[0].seeds[0].clip.offset, rows[0].seeds[0].clip.length), (0, 2 * BEAT, 3 * BEAT));
    // Paste on another MIDI track, at the marker.
    s.set_marker(BEAT, Some(midi2));
    assert!(s.paste());
    assert_eq!(spans(&s, midi2), [(BEAT, 3 * BEAT)]);
    assert_eq!(spans(&s, midi), [(0, 8 * BEAT)]);
    assert_eq!(s.ed.marker, 4 * BEAT);
}

#[test]
fn clips_only_paste_onto_tracks_of_their_kind() {
    let Song { mut s, midi, midi2, audio } = song();
    let riff = midi_clip(&mut s, midi, 0, 2 * BEAT, &[(60, 0, BEAT)]);
    s.click_clip(riff, false);
    assert!(s.copy());
    // An audio track cannot take it: it goes back to the track it came from.
    s.set_marker(4 * BEAT, Some(audio));
    assert!(s.paste());
    assert!(spans(&s, audio).is_empty());
    assert_eq!(spans(&s, midi), [(0, 2 * BEAT), (4 * BEAT, 2 * BEAT)]);
    // Another MIDI track does.
    s.set_marker(0, Some(midi2));
    assert!(s.paste());
    assert_eq!(spans(&s, midi2), [(0, 2 * BEAT)]);
    // Two rows keep their distance: from the audio track down there is no room for the MIDI row.
    let kick = audio_clip(&mut s, audio, 0, BEAT);
    s.select_clips([riff, kick], None);
    assert!(s.copy());
    let Some(Clipboard::Clips { rows, .. }) = &s.ed.clipboard else { panic!() };
    assert_eq!(rows.iter().map(|r| r.kind).collect::<Vec<_>>(), [TrackKind::Instrument, TrackKind::Audio]);
    s.set_marker(8 * BEAT, Some(midi2));
    assert!(s.paste());
    assert_eq!(spans(&s, midi2), [(0, 2 * BEAT), (8 * BEAT, 2 * BEAT)], "the MIDI row moved one track down");
    assert_eq!(spans(&s, audio), [(0, BEAT)], "the audio row had no audio track below");
}

#[test]
fn pasting_replaces_what_is_under_the_clip() {
    let Song { mut s, audio, .. } = song();
    let kick = audio_clip(&mut s, audio, 0, 2 * BEAT);
    audio_clip(&mut s, audio, 4 * BEAT, 4 * BEAT);
    s.click_clip(kick, false);
    assert!(s.copy());
    s.set_marker(5 * BEAT, None);
    assert!(s.paste());
    assert_eq!(spans(&s, audio), [(0, 2 * BEAT), (4 * BEAT, BEAT), (5 * BEAT, 2 * BEAT), (7 * BEAT, BEAT)]);
}

#[test]
fn a_multi_clip_drag_is_one_undo_step() {
    let Song { mut s, midi, midi2, audio } = song();
    let riff = midi_clip(&mut s, midi, 0, 2 * BEAT, &[]);
    let kick = audio_clip(&mut s, audio, BEAT, BEAT);
    s.select_clips([riff, kick], Some(riff));
    let origin: Vec<_> = s.clip_selection().into_iter().map(|(t, pl, _)| (pl.id, t, pl.start)).collect();
    let before = steps(&s);
    s.seal();
    for dt in [90, 300, 480] {
        assert!(s.drag_clips(&origin, dt, 0));
    }
    assert!(!s.drag_clips(&origin, 480, 0), "no move, no edit");
    s.seal();
    assert_eq!((spans(&s, midi), spans(&s, audio)), (vec![(480, 2 * BEAT)], vec![(BEAT + 480, BEAT)]));
    assert_eq!(steps(&s), before + 1);
    assert_eq!(s.history.undo_label(), Some("Move clips"));
    assert!(s.undo());
    assert_eq!((spans(&s, midi), spans(&s, audio)), (vec![(0, 2 * BEAT)], vec![(BEAT, BEAT)]));
    // The group stops at the start of the song together.
    assert!(s.drag_clips(&origin, -5 * BEAT, 0) || spans(&s, midi) == [(0, 2 * BEAT)]);
    assert_eq!((spans(&s, midi), spans(&s, audio)), (vec![(0, 2 * BEAT)], vec![(BEAT, BEAT)]));
    // One lane down the audio clip would leave the lanes: nobody changes track.
    assert!(s.shift_tracks(&origin, 1).is_none());
    // A MIDI clip alone moves to the MIDI track below, not onto the audio track.
    let one = [(riff, midi, 0)];
    assert_eq!(s.shift_tracks(&one, 1), Some(vec![midi2]));
    assert!(s.shift_tracks(&one, 2).is_none());
    assert!(s.drag_clips(&one, 0, 1));
    assert_eq!(s.placement(riff).unwrap().0, midi2);
    assert_eq!(s.selected_placement, Some((midi2, riff)), "the last clicked clip is still the one");
}

#[test]
fn the_adaptive_grid_gets_finer_with_zoom_and_the_keys_shift_it() {
    let Song { mut s, .. } = song();
    s.snap = Snap::Adaptive;
    let mut last = Tick::MAX;
    let mut steps_seen = BTreeSet::new();
    let mut zoom = 2.;
    while zoom <= crate::nav::MAX_PX_PER_BEAT {
        s.arrange.px_per_beat = zoom;
        let step = s.snap_step(GridView::Arrange).unwrap();
        assert!(step <= last, "zooming in never coarsens the grid");
        last = step;
        steps_seen.insert(step);
        zoom *= 1.3;
    }
    assert!(steps_seen.contains(&(4 * BEAT)) && steps_seen.contains(&(BEAT / 8)) && last <= BEAT / 32, "bars down to 1/128: {steps_seen:?}");
    // The piano roll has its own zoom.
    s.arrange.px_per_beat = 56.;
    s.piano.px_per_beat = 120.;
    assert_eq!((s.snap_step(GridView::Arrange), s.snap_step(GridView::Piano)), (Some(BEAT / 4), Some(BEAT / 8)));
    assert_eq!(s.snap_label(GridView::Arrange), "Auto 1/16");
    // Ctrl+1 narrows, Ctrl+2 widens.
    assert!(s.step_grid(-1));
    assert_eq!(s.snap_step(GridView::Arrange), Some(BEAT / 8));
    assert!(s.step_grid(1) && s.step_grid(1));
    assert_eq!(s.snap_step(GridView::Arrange), Some(BEAT / 2));
    assert!(s.step_grid(1) && !s.step_grid(1), "it stops at the widest");
    s.ed.density = 0;
    // Ctrl+3: triplets.
    assert!(s.toggle_triplet());
    assert_eq!(s.snap_step(GridView::Arrange), Some(BEAT / 3));
    assert!(!s.toggle_triplet());
    // Snapping: to the nearest line, unless Alt is held or Ctrl+4 switched it off.
    assert_eq!(s.snap_tick(GridView::Arrange, BEAT / 4 + 100, false), BEAT / 4);
    assert_eq!(s.snap_tick(GridView::Arrange, BEAT / 4 + 130, false), BEAT / 2);
    assert_eq!(s.snap_tick(GridView::Arrange, BEAT / 4 + 100, true), BEAT / 4 + 100);
    assert!(!s.toggle_snap());
    assert_eq!((s.snap, s.snap_step(GridView::Arrange)), (Snap::Off, None));
    assert_eq!(s.snap_tick(GridView::Arrange, 333, false), 333);
    assert!(s.toggle_snap());
    assert_eq!(s.snap, Snap::Adaptive);
    // A fixed value from the Snap menu still works, and the keys step through the values.
    s.snap = Snap::Eighth;
    assert_eq!((s.snap_step(GridView::Arrange), s.snap_label(GridView::Arrange)), (Some(BEAT / 2), "1/8".to_owned()));
    assert!(s.step_grid(-1));
    assert_eq!(s.snap, Snap::Sixteenth);
    // In 3/4 a fixed bar is three beats.
    s.snap = Snap::Bar;
    s.set_time_signature(TimeSignature { numerator: 3, denominator: 4 });
    assert_eq!(s.snap_step(GridView::Arrange), Some(3 * BEAT));
}

#[test]
fn splitting_keeps_the_fades_on_the_outer_edges() {
    let Song { mut s, audio, .. } = song();
    let kick = audio_clip(&mut s, audio, 4 * BEAT, 4 * BEAT);
    let id = s.placement(kick).unwrap().1.clip;
    let (fade_in, fade_out) = (Fade { length: 200, curve: 0.3 }, Fade { length: 300, curve: -0.3 });
    assert!(s.edit("Fade", vec![Command::UpdateClip { id, clip: Box::new(Clip { fade_in, fade_out, ..s.project.clips[&id].clone() }) }]));
    s.click_clip(kick, false);
    assert_eq!(s.split_at(6 * BEAT), 1);
    assert_eq!((s.selected_placement, s.selected_clip), (Some((audio, kick)), Some(id)), "the part before the cut stays selected");
    let right = s.project.track(audio).unwrap().arrangement.iter().find(|pl| pl.start == 6 * BEAT).unwrap().clone();
    let (left, right) = (clip_of(&s, kick), s.project.clips[&right.clip].clone());
    assert_eq!((left.fade_in, left.fade_out), (fade_in, Fade::default()), "no fade at the cut");
    assert_eq!((right.fade_in, right.fade_out), (Fade::default(), fade_out));
    assert_eq!((left.length, right.offset, right.length, right.source), (2 * BEAT, 2 * BEAT, 2 * BEAT, left.source));
    // With a time selection Ctrl+E cuts at both of its edges.
    s.set_time_selection((4 * BEAT + 100, audio), (4 * BEAT + 400, audio));
    assert_eq!(s.split_at(0), 2);
    assert_eq!(spans(&s, audio), [(4 * BEAT, 100), (4 * BEAT + 100, 300), (4 * BEAT + 400, 2 * BEAT - 400), (6 * BEAT, 2 * BEAT)]);
}

#[test]
fn dragging_an_audio_clip_longer_than_its_file_loops_it() {
    let Song { mut s, audio, .. } = song();
    let kick = audio_clip(&mut s, audio, 0, 2 * BEAT);
    let id = s.placement(kick).unwrap().1.clip;
    assert_eq!(ops::source_len(&s.project, id), Some(2 * BEAT), "one second at 120 BPM");
    s.seal();
    assert!(s.resize_clip(id, 3 * BEAT) && s.resize_clip(id, 7 * BEAT));
    s.seal();
    let clip = s.project.clips[&id].clone();
    assert_eq!((clip.length, clip.looping), (7 * BEAT, Some(LoopRegion { start: 0, end: 2 * BEAT })));
    assert_eq!(overlay::loop_repeats(&clip, 1.), [(2 * BEAT) as f32, (4 * BEAT) as f32, (6 * BEAT) as f32]);
    // Back inside the file the loop it got goes away; the whole drag is one step.
    s.seal();
    assert!(s.resize_clip(id, BEAT));
    assert_eq!(s.project.clips[&id].looping, None);
    assert!(s.undo() && s.undo());
    assert_eq!(s.project.clips[&id].length, 2 * BEAT);
}

#[test]
fn notes_copy_cut_and_paste_at_the_piano_marker() {
    let Song { mut s, midi, .. } = song();
    s.snap = Snap::Adaptive; // 64 px per beat in the piano roll: sixteenths
    let riff = midi_clip(&mut s, midi, 0, 8 * BEAT, &[(60, 0, BEAT / 2), (64, BEAT / 2, 400)]);
    let clip = s.placement(riff).unwrap().1.clip;
    s.select_clip(Some(clip));
    let notes = |s: &AppState| {
        let mut n: Vec<_> = s.clip_notes(clip).unwrap().1.notes.iter().map(|n| (n.start, n.key)).collect();
        n.sort();
        n
    };
    s.selected_notes = s.clip_notes(clip).unwrap().1.notes.iter().map(|n| n.id).collect();
    // The arrangement has the keys: Copy takes the clip, not the notes.
    s.click_clip(riff, false);
    assert!(s.copy());
    assert!(matches!(s.ed.clipboard, Some(Clipboard::Clips { .. })));
    s.ed.area = Area::Piano;
    assert!(s.copy());
    assert!(matches!(&s.ed.clipboard, Some(Clipboard::Notes { span, notes }) if *span == BEAT && notes.len() == 2), "the span is rounded up to the grid");
    assert_eq!(s.ed.piano_marker, BEAT);
    assert!(s.paste() && s.paste());
    assert_eq!(notes(&s), [(0, 60), (BEAT / 2, 64), (BEAT, 60), (BEAT + BEAT / 2, 64), (2 * BEAT, 60), (2 * BEAT + BEAT / 2, 64)]);
    assert_eq!((s.ed.piano_marker, s.selected_notes.len()), (3 * BEAT, 2), "the pasted notes are selected");
    assert!(s.cut());
    assert_eq!(notes(&s).len(), 4);
    assert_eq!(s.ed.piano_marker, 2 * BEAT, "cut leaves the marker where the notes were");
    assert!(s.paste());
    assert_eq!(notes(&s).len(), 6);
    assert_eq!(spans(&s, midi), [(0, 8 * BEAT)], "the clip itself was never touched");
}

#[test]
fn consolidate_joins_midi_clips_and_writes_out_loops() {
    let Song { mut s, midi, audio, .. } = song();
    let a = midi_clip(&mut s, midi, 0, 2 * BEAT, &[(60, 0, BEAT)]);
    let b = midi_clip(&mut s, midi, 4 * BEAT, 3 * BEAT, &[(64, 0, BEAT / 2), (67, BEAT, BEAT)]);
    let looped = s.placement(b).unwrap().1.clip;
    let with_loop = Clip { looping: Some(LoopRegion { start: 0, end: BEAT }), ..s.project.clips[&looped].clone() };
    assert!(s.edit("Loop", vec![Command::UpdateClip { id: looped, clip: Box::new(with_loop) }]));
    s.select_clips([a, b], None);
    assert_eq!(s.consolidate(), Ok(1));
    assert_eq!(spans(&s, midi), [(0, 7 * BEAT)]);
    let (_, joined, _) = s.clip_selection()[0].clone();
    let mut notes: Vec<_> = s.clip_notes(joined.clip).unwrap().1.notes.iter().map(|n| (n.start, n.key, n.length)).collect();
    notes.sort();
    // The one-beat loop of the second clip plays three times; its note outside the loop never did.
    assert_eq!(notes, [(0, 60, BEAT), (4 * BEAT, 64, BEAT / 2), (5 * BEAT, 64, BEAT / 2), (6 * BEAT, 64, BEAT / 2)]);
    assert_eq!((s.project.clips.len(), s.project.sources.len()), (1, 1), "the old clips are gone");
    assert!(s.undo());
    assert_eq!(spans(&s, midi), [(0, 2 * BEAT), (4 * BEAT, 3 * BEAT)]);
    // Audio is refused.
    let kick = audio_clip(&mut s, audio, 0, BEAT);
    s.select_clips([kick], None);
    assert!(s.consolidate().is_err());
    assert_eq!(spans(&s, audio), [(0, BEAT)]);
}

#[test]
fn insert_silence_and_duplicate_time_move_every_track() {
    let Song { mut s, midi, audio, .. } = song();
    midi_clip(&mut s, midi, 0, 4 * BEAT, &[(60, 0, BEAT)]);
    audio_clip(&mut s, audio, 2 * BEAT, 2 * BEAT);
    assert!(s.set_automation_point(midi, AutomationTarget::Pan, 3 * BEAT, 0.25, false));
    assert!(!s.insert_silence(), "it needs a time selection");
    // Two beats of silence at beat 1, selected on one track only: every track moves.
    s.set_time_selection((BEAT, midi), (3 * BEAT, midi));
    assert!(s.insert_silence());
    assert_eq!(spans(&s, midi), [(0, BEAT), (3 * BEAT, 3 * BEAT)]);
    assert_eq!(spans(&s, audio), [(4 * BEAT, 2 * BEAT)]);
    assert_eq!(s.project.track(midi).unwrap().automation[0].points[0].tick, 5 * BEAT, "automation moves with the clips");
    assert!(s.undo());
    assert_eq!((spans(&s, midi), spans(&s, audio)), (vec![(0, 4 * BEAT)], vec![(2 * BEAT, 2 * BEAT)]));
    // Duplicate Time repeats beats 2..4 and pushes the rest later.
    s.set_time_selection((2 * BEAT, audio), (4 * BEAT, audio));
    assert!(s.duplicate_time());
    assert_eq!(spans(&s, midi), [(0, 2 * BEAT), (2 * BEAT, 2 * BEAT), (4 * BEAT, 2 * BEAT)]);
    assert_eq!(spans(&s, audio), [(2 * BEAT, 2 * BEAT), (4 * BEAT, 2 * BEAT)]);
    let sel = s.time_selection().unwrap();
    assert_eq!((sel.start, sel.end), (4 * BEAT, 6 * BEAT));
    assert_eq!(s.history.undo_label(), Some("Duplicate time"));
}

#[test]
fn loop_selection_select_all_and_the_arrow_keys() {
    let Song { mut s, midi, midi2, audio } = song();
    let riff = midi_clip(&mut s, midi, BEAT, 2 * BEAT, &[]);
    let kick = audio_clip(&mut s, audio, 4 * BEAT, BEAT);
    assert!(!s.loop_selection(), "nothing selected: the caller toggles the loop");
    assert!(s.select_all_clips());
    assert_eq!(s.selected_clip_ids(), [riff, kick].into());
    assert!(s.loop_selection());
    assert_eq!(s.project.loop_region, Some((BEAT, 5 * BEAT)));
    s.set_time_selection((0, midi), (2 * BEAT, midi));
    assert!(s.clip_selection().is_empty(), "a time selection replaces the clip selection");
    assert!(s.loop_selection());
    assert_eq!(s.project.loop_region, Some((0, 2 * BEAT)));
    // Shift+click stretches the selection from where it began.
    s.extend_time_selection(6 * BEAT, audio);
    let sel = s.time_selection().unwrap();
    assert_eq!((sel.start, sel.end, sel.tracks.clone()), (0, 6 * BEAT, vec![midi, midi2, audio]));
    // Arrows: nudge the selected clips by the grid step, each press one undo step.
    s.snap = Snap::Sixteenth;
    s.select_clips([riff, kick], None);
    assert!(s.key_edit(KeyEdit::Right) && s.key_edit(KeyEdit::Right));
    assert_eq!((spans(&s, midi), spans(&s, audio)), (vec![(BEAT + BEAT / 2, 2 * BEAT)], vec![(4 * BEAT + BEAT / 2, BEAT)]));
    assert!(s.undo());
    assert_eq!(spans(&s, midi), [(BEAT + BEAT / 4, 2 * BEAT)]);
    // With no clip selected they move the insert marker and step through the tracks.
    s.set_marker(BEAT, Some(midi));
    assert!(s.key_edit(KeyEdit::Left));
    assert_eq!(s.ed.marker, BEAT - BEAT / 4);
    assert!(s.key_edit(KeyEdit::Down));
    assert_eq!(s.selected_track, Some(midi2));
    assert!(s.key_edit(KeyEdit::Up) && !s.key_edit(KeyEdit::Up));
    // The piano roll has the keys: Ctrl+A is not ours.
    s.ed.area = Area::Piano;
    assert!(!s.select_all_clips());
}

#[test]
fn clicks_build_the_selection_and_old_code_can_still_set_one_clip() {
    let Song { mut s, midi, audio, .. } = song();
    let a = midi_clip(&mut s, midi, 0, BEAT, &[]);
    let b = midi_clip(&mut s, midi, 2 * BEAT, BEAT, &[]);
    let c = audio_clip(&mut s, audio, 0, BEAT);
    s.click_clip(a, false);
    s.click_clip(c, true);
    assert_eq!((s.selected_clip_ids(), s.selected_placement), ([a, c].into(), Some((audio, c))));
    // A plain click on a selected clip keeps the group (to drag it); on another clip it starts over.
    s.click_clip(a, false);
    assert_eq!((s.selected_clip_ids(), s.selected_placement), ([a, c].into(), Some((midi, a))));
    s.click_clip(c, true);
    assert_eq!((s.selected_clip_ids(), s.selected_placement), ([a].into(), Some((midi, a))));
    s.click_clip(b, false);
    assert_eq!(s.selected_clip_ids(), [b].into());
    // Code that only knows `selected_placement` selects exactly that clip.
    s.select_clips([a, b], None);
    s.selected_placement = Some((audio, c));
    assert_eq!(s.selected_clip_ids(), [c].into());
    // Deleting through it prunes the rest.
    assert!(s.remove_placement(audio, c));
    s.select_clips([a, b], None);
    assert!(s.delete_selection());
    assert!(s.selected_clip_ids().is_empty() && spans(&s, midi).is_empty());
    assert!(s.undo());
    assert_eq!(spans(&s, midi).len(), 2);
    // While the piano roll has the keys they never reach the clips behind it.
    s.select_clips([a], None);
    s.ed.area = Area::Piano;
    assert!(!s.delete_selection() && !s.duplicate_selection() && !s.copy() && !s.key_edit(KeyEdit::Right));
    assert_eq!(spans(&s, midi), [(0, BEAT), (2 * BEAT, BEAT)]);
}

#[test]
fn insert_midi_clip_rename_loop_and_deactivate() {
    let Song { mut s, midi, audio, .. } = song();
    s.set_marker(4 * BEAT, Some(audio));
    assert_eq!(s.insert_midi_clip(), None, "not on an audio track");
    s.set_marker(4 * BEAT, Some(midi));
    let clip = s.insert_midi_clip().expect("one bar at the marker");
    assert_eq!(spans(&s, midi), [(4 * BEAT, 4 * BEAT)]);
    assert_eq!(s.selected_clip, Some(clip), "it opens in the piano roll");
    // In the time selection it fills the selection.
    s.set_time_selection((0, midi), (2 * BEAT, midi));
    assert!(s.insert_midi_clip().is_some());
    assert_eq!(spans(&s, midi), [(0, 2 * BEAT), (4 * BEAT, 4 * BEAT)]);
    assert!(s.rename_clip(clip, "  Verse  ") && !s.rename_clip(clip, "Verse") && !s.rename_clip(clip, " "));
    assert_eq!(s.project.clips[&clip].name, "Verse");
    assert!(s.rename_track(midi, "Lead") && !s.rename_track(midi, "Lead"));
    assert_eq!(s.project.track(midi).unwrap().name, "Lead");
    // Loop and Deactivate act on every selected clip.
    assert!(s.select_all_clips());
    assert_eq!(s.toggle_clips_loop(), Some(true));
    assert!(s.project.clips.values().all(|c| c.looping == Some(LoopRegion { start: 0, end: c.length })));
    assert_eq!(s.toggle_clips_loop(), Some(false));
    assert_eq!(s.toggle_clips_active(), Some(true));
    assert!(s.project.clips.values().all(|c| c.muted));
    assert_eq!(s.toggle_clips_active(), Some(false));
    // Track keys.
    s.selected_track = Some(midi);
    assert_eq!(s.toggle_arm(), Some(true));
    assert_eq!(s.toggle_activator(0), Some(("Lead".to_owned(), false)));
    assert!(s.project.track(midi).unwrap().mute);
    assert_eq!(s.toggle_fold(), None, "only groups fold");
    let group = s.group_tracks().unwrap();
    assert_eq!(s.toggle_fold(), Some(true));
    assert_eq!(s.selected_track, Some(group));
    assert!(!s.lane_tracks().contains(&midi), "a folded group hides its tracks");
}

// ───────────── Seams: lanes, sample editor, clipboard and engine ─────────────

use crate::views::arrange_clips::edit::{self as lane_edit, Edit, Origin};
use crate::views::arrange_clips::geom::WaveMap;
use crate::views::sample_editor::edit::{self as sample_edit, Gesture};
use crate::views::sample_editor::geom::unroll;

/// Samples per tick at 120 BPM and 48 kHz.
const SPT: usize = 25;

/// An audio clip over a one-second ramp (two beats at 120 BPM) at beat 4.
/// The value of a sample tells its frame.
fn ramp_clip(s: &mut AppState, track: TrackId) -> (PlacementId, ClipId, Vec<f32>) {
    let data: Vec<f32> = (0..48_000).map(|i| (i + 1) as f32 / 48_000.).collect();
    let (source, clip, id) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
    let file = AudioSource { path: "mem://ramp.wav".into(), hash: None, sample_rate: 48_000, channels: 1, frames: 48_000, warp: Warp::default(), key: None };
    assert!(s.edit("Add clip", vec![
        Command::AddSource { id: source, source: Box::new(Source::Audio(file)) },
        Command::AddClip { id: clip, clip: Box::new(Clip::new("Ramp", Color::palette(2), source, 2 * BEAT)) },
        Command::PlaceClip { track, placement: Placement { id, clip, start: 4 * BEAT, lane: 0 } },
    ]));
    s.seal();
    s.click_clip(id, false);
    (id, clip, data)
}

/// The left edge pulled half a beat in and the right edge out to five beats, in the lanes.
fn trim_and_stretch_in_the_lane(s: &mut AppState, placement: PlacementId, clip: ClipId) {
    s.seal();
    let origin = Origin::take(s, placement).unwrap();
    assert!(lane_edit::apply(s, &origin, Edit::Start(4 * BEAT + BEAT / 2)));
    s.seal();
    assert!(s.resize_clip(clip, 5 * BEAT));
    s.seal();
}

#[test]
fn the_lane_edges_and_the_sample_editor_markers_make_the_same_looping_clip() {
    let Song { mut s, audio, .. } = song();
    let (placement, clip, _) = ramp_clip(&mut s, audio);
    trim_and_stretch_in_the_lane(&mut s, placement, clip);
    let lane = (s.project.clips[&clip].clone(), s.placement(placement).unwrap().1.start);

    // The same two moves with the start and end markers of the sample editor
    // (positions on its axis are source ticks).
    let Song { mut s, audio, .. } = song();
    let (placement, clip, _) = ramp_clip(&mut s, audio);
    assert!(sample_edit::apply_now(&mut s, clip, Gesture::Start(BEAT / 2)));
    assert!(sample_edit::apply_now(&mut s, clip, Gesture::End(BEAT / 2 + 5 * BEAT)));
    let editor = (s.project.clips[&clip].clone(), s.placement(placement).unwrap().1.start);

    assert_eq!(lane, editor);
    assert_eq!((lane.0.offset, lane.0.length, lane.1), (BEAT / 2, 5 * BEAT, 4 * BEAT + BEAT / 2));
    assert_eq!(lane.0.looping, Some(LoopRegion { start: BEAT / 2, end: 2 * BEAT }), "past the file it loops what it was showing");
}

#[test]
fn ctrl_e_the_clip_menu_and_the_sample_editor_split_alike() {
    // The halves of a faded clip with a volume envelope, cut 1.5 beats in.
    let halves = |split: fn(&mut AppState, ClipId)| {
        let Song { mut s, audio, .. } = song();
        let (placement, clip, _) = ramp_clip(&mut s, audio);
        let shaped = Clip {
            fade_in: Fade { length: 200, curve: 0.3 },
            fade_out: Fade { length: 300, curve: -0.3 },
            envelopes: vec![AutomationLane { points: vec![AutomationPoint { tick: 0, value: 1., curve: 0.6 }, AutomationPoint { tick: 2 * BEAT, value: 0.25, curve: 0. }], ..AutomationLane::new(AutomationTarget::Volume) }],
            ..s.project.clips[&clip].clone()
        };
        assert!(s.edit("Shape", vec![Command::UpdateClip { id: clip, clip: Box::new(shaped) }]));
        split(&mut s, clip);
        let lane = &s.project.track(audio).unwrap().arrangement;
        assert_eq!(lane.len(), 2);
        let right = lane.iter().find(|pl| pl.id != placement).unwrap();
        assert_eq!(right.start, 4 * BEAT + 3 * BEAT / 2);
        // The part before the cut is what stays selected, in the lane and in the Clip tab.
        assert_eq!((s.selected_placement, s.selected_clip, s.selected_clip_ids()), (Some((audio, placement)), Some(clip), [placement].into()));
        (s.project.clips[&clip].clone(), s.project.clips[&right.clip].clone())
    };
    // Ctrl+E and the clip menu are one command: at the playhead while
    // playing, at the insert marker when stopped.
    let keys = halves(|s, _| assert_eq!(s.split(Some(4 * BEAT + 3 * BEAT / 2)), 1));
    let menu = halves(|s, _| {
        s.ed.marker = 4 * BEAT + 3 * BEAT / 2;
        assert_eq!(s.split(None), 1);
    });
    // The sample editor's cursor is in source ticks.
    let editor = halves(|s, clip| assert_eq!(sample_edit::split(s, clip, 3 * BEAT / 2), Ok(())));
    assert_eq!(keys, menu);
    assert_eq!(keys, editor);
    let (left, right) = keys;
    assert_eq!((left.fade_in.length, left.fade_out.length, right.fade_in.length, right.fade_out.length), (200, 0, 0, 300), "fades stay on the outer edges");
    assert_eq!((left.source, right.offset), (right.source, 3 * BEAT / 2));
}

/// The ramp clip of [`ramp_clip`] with a second placement of the same clip at beat 12.
fn linked_ramp(s: &mut AppState, track: TrackId) -> (PlacementId, PlacementId, ClipId) {
    let (first, clip, _) = ramp_clip(s, track);
    let twin = s.project.alloc_placement_id();
    assert!(s.edit("Place again", vec![Command::PlaceClip { track, placement: Placement { id: twin, clip, start: 12 * BEAT, lane: 0 } }]));
    s.seal();
    (first, twin, clip)
}

#[test]
fn cutting_one_placement_of_a_shared_clip_leaves_the_other_alone_from_every_entry_point() {
    let cut = 4 * BEAT + BEAT / 2;
    type Cut = fn(&mut AppState, TrackId, ClipId);
    let cuts: [(&str, Cut); 4] = [
        ("Ctrl+E at the marker", |s, _, _| {
            s.ed.marker = 4 * BEAT + BEAT / 2;
            assert_eq!(s.split(None), 1);
        }),
        ("the sample editor", |s, _, clip| assert_eq!(sample_edit::split(s, clip, BEAT / 2), Ok(()))),
        ("Ctrl+E on a time selection", |s, track, _| {
            s.set_time_selection((4 * BEAT + BEAT / 2, track), (5 * BEAT, track));
            assert_eq!(s.split_at(0), 2);
        }),
        ("Delete on a time selection", |s, track, _| {
            s.set_time_selection((4 * BEAT + BEAT / 2, track), (5 * BEAT, track));
            assert!(s.delete_selection());
        }),
    ];
    for (what, run) in cuts {
        let Song { mut s, audio, .. } = song();
        let (first, twin, clip) = linked_ramp(&mut s, audio);
        let before = s.project.clone();
        let whole = s.project.clips[&clip].clone();
        run(&mut s, audio, clip);
        // The other placement plays what it played: same clip, length and offset.
        let (_, other, len) = s.placement(twin).unwrap();
        assert_eq!((other.clip, other.start, len), (clip, 12 * BEAT, 2 * BEAT), "{what}");
        assert_eq!(s.project.clips[&clip], whole, "{what}");
        // The placement that was cut ends at the cut, on a clip of its own.
        let (_, left, len) = s.placement(first).unwrap();
        assert_ne!(left.clip, clip, "{what}");
        assert_eq!((left.start, len, s.project.clips[&left.clip].offset), (4 * BEAT, BEAT / 2, 0), "{what}");
        assert!(s.project.track(audio).unwrap().arrangement.iter().any(|pl| pl.start == cut && pl.id != first) || what.starts_with("Delete"), "{what}");
        // One undo puts everything back exactly.
        assert!(s.undo());
        let mut back = s.project.clone();
        back.ids = before.ids.clone();
        assert_eq!(back, before, "{what}");
    }
}

#[test]
fn after_a_split_the_lane_and_the_clip_tab_show_the_part_before_the_cut() {
    // A clip of its own keeps its id; a shared one gets a new clip for the cut placement.
    for shared in [false, true] {
        let Song { mut s, audio, .. } = song();
        let (first, clip) = if shared {
            let (first, _, clip) = linked_ramp(&mut s, audio);
            (first, clip)
        } else {
            let (first, clip, _) = ramp_clip(&mut s, audio);
            (first, clip)
        };
        s.click_clip(first, false);
        s.ed.marker = 5 * BEAT;
        assert_eq!(s.split(None), 1);
        let left = s.placement(first).unwrap().1.clip;
        assert_eq!(left == clip, !shared);
        assert_eq!((s.selected_placement, s.selected_clip, s.selected_clip_ids()), (Some((audio, first)), Some(left), [first].into()), "shared: {shared}");
        // The sample editor works on the same placement the lane shows selected.
        assert_eq!(sample_edit::placement_of(&s, left).map(|pl| pl.placement), Some(first));
        // With no clip selected the clips under the marker on the selected track are cut, and their left parts selected.
        s.set_marker(4 * BEAT + BEAT / 2, Some(audio));
        assert!(s.clip_selection().is_empty());
        assert_eq!(s.split(None), 1);
        let left = s.placement(first).unwrap().1.clip;
        assert_eq!((s.selected_placement, s.selected_clip, s.selected_clip_ids()), (Some((audio, first)), Some(left), [first].into()));
        assert_eq!(s.project.clips[&left].length, BEAT / 2);
    }
}

#[test]
fn a_click_on_a_clip_puts_the_marker_there_and_split_cuts_at_it() {
    let Song { mut s, audio, midi, .. } = song();
    let (ramp, clip, _) = ramp_clip(&mut s, audio);
    s.snap = Snap::Sixteenth;
    s.set_marker(0, Some(midi));
    // Pressed and released without dragging: the marker lands on the grid
    // line nearest the click, and the clip is selected as before.
    s.press_clip(ramp, 5 * BEAT + 7, false);
    assert_eq!(s.ed.marker, 0, "not before the press ends");
    assert_eq!(s.release_clip(false), Some(5 * BEAT));
    assert_eq!((s.ed.marker, s.selected_placement, s.selected_clip), (5 * BEAT, Some((audio, ramp)), Some(clip)));
    // A press that dragged the clip leaves the marker alone; Alt places it off the grid.
    s.press_clip(ramp, 4 * BEAT + 7, false);
    assert_eq!((s.release_clip(true), s.ed.marker), (None, 5 * BEAT));
    assert_eq!(s.release_clip(false), None, "a press ends once");
    s.press_clip(ramp, 5 * BEAT + 7, true);
    assert_eq!(s.release_clip(false), Some(5 * BEAT + 7));
    // Stopped, Split cuts at the marker, exactly where it is drawn.
    assert_eq!(s.split(None), 1);
    assert_eq!(spans(&s, audio), [(4 * BEAT, BEAT + 7), (5 * BEAT + 7, BEAT - 7)]);
    // Playing, it cuts at the playhead (on the grid) and the marker stays.
    assert_eq!(s.split(Some(4 * BEAT + BEAT / 2 + 3)), 1);
    assert_eq!(spans(&s, audio), [(4 * BEAT, BEAT / 2), (4 * BEAT + BEAT / 2, BEAT / 2 + 7), (5 * BEAT + 7, BEAT - 7)]);
    assert_eq!(s.ed.marker, 5 * BEAT + 7);
    // The marker outside the selected clip: the clips under it on the selected track are cut.
    assert_eq!(s.selected_placement, Some((audio, ramp)));
    s.ed.marker = 5 * BEAT + BEAT / 2;
    assert_eq!(s.split(None), 1);
    assert_eq!(spans(&s, audio).len(), 4);
    // Nothing under the marker: nothing is cut, and no undo step is made.
    let before = steps(&s);
    s.ed.marker = 20 * BEAT;
    assert_eq!((s.split(None), steps(&s)), (0, before));
    s.ed.marker = 4 * BEAT;
    assert_eq!(s.split(None), 0, "a clip's own edge is not a cut");
}

#[test]
fn copies_of_a_clip_are_clips_of_their_own() {
    let Song { mut s, audio, .. } = song();
    let (ramp, clip, _) = ramp_clip(&mut s, audio);
    // Ctrl+D, Ctrl+C / Ctrl+V and Ctrl+drag.
    s.click_clip(ramp, false);
    assert!(s.duplicate_selection());
    let duplicated = s.selected_placement.unwrap().1;
    s.click_clip(ramp, false);
    assert!(s.copy());
    s.ed.marker = 10 * BEAT;
    assert!(s.paste());
    let pasted = s.selected_placement.unwrap().1;
    let copies = lane_edit::Copies::make(&mut s, ramp).unwrap();
    assert!(copies.move_to(&mut s, 20 * BEAT, 0));
    let dragged = copies.origin[0].0;
    assert!(copies.finish(&mut s));
    let ids: BTreeSet<ClipId> = [ramp, pasted, duplicated, dragged].iter().map(|id| s.placement(*id).unwrap().1.clip).collect();
    assert_eq!(ids.len(), 4, "four placements, four clips");
    assert_eq!(s.project.sources.len(), 1, "over one file");
    // So each can be cut, resized and faded without the others following.
    let original = s.project.clips[&clip].clone();
    s.click_clip(pasted, false);
    s.ed.marker = s.placement(pasted).unwrap().1.start + BEAT;
    assert_eq!(s.split(None), 1);
    let copy = s.placement(dragged).unwrap().1.clip;
    let fades = ops::set_fades(&s.project, copy, Fade { length: 240, curve: 0. }, Fade::default()).unwrap();
    assert!(s.edit("Fade", vec![fades]));
    assert!(s.resize_clip(s.placement(duplicated).unwrap().1.clip, BEAT));
    assert_eq!(s.project.clips[&clip], original);
    assert_eq!(clip_of(&s, pasted).length, BEAT);
    assert_eq!((clip_of(&s, dragged).length, clip_of(&s, dragged).fade_in.length), (2 * BEAT, 240));
}

#[test]
fn a_right_edge_drag_has_room_up_to_the_next_clip() {
    let Song { mut s, audio, midi, .. } = song();
    let kick = audio_clip(&mut s, audio, 0, 2 * BEAT);
    assert_eq!(s.room_after(kick, 2 * BEAT), None, "nothing follows it");
    let next = audio_clip(&mut s, audio, 5 * BEAT, BEAT);
    midi_clip(&mut s, midi, 3 * BEAT, BEAT, &[]);
    assert_eq!(s.room_after(kick, 2 * BEAT), Some(5 * BEAT), "up to the next clip of its own track");
    assert_eq!(s.room_after(next, BEAT), None);
    // A clip that already lies over the next one is not cut back by touching its edge.
    let over = audio_clip(&mut s, audio, 4 * BEAT, 3 * BEAT);
    assert_eq!(s.room_after(over, 3 * BEAT), None);
    assert_eq!(s.room_after(kick, 2 * BEAT), Some(4 * BEAT));
}

#[test]
fn a_stretched_shaped_clip_pastes_whole_and_the_engine_plays_the_copies_like_the_original() {
    let Song { mut s, audio, .. } = song();
    let (placement, id, data) = ramp_clip(&mut s, audio);
    trim_and_stretch_in_the_lane(&mut s, placement, id);
    // Fades and a volume envelope on top of the loop.
    let points = vec![AutomationPoint { tick: 0, value: 1., curve: 0. }, AutomationPoint { tick: 2 * BEAT, value: 0.5, curve: 0.4 }, AutomationPoint { tick: 5 * BEAT, value: 0.8, curve: 0. }];
    let fades = ops::set_fades(&s.project, id, Fade { length: 240, curve: 0.5 }, Fade { length: 480, curve: -0.3 }).unwrap();
    assert!(s.edit("Fades", vec![fades]));
    let envelope = ops::set_volume_envelope(&s.project, id, points).unwrap();
    assert!(s.edit("Envelope", vec![envelope]));
    let original = s.project.clips[&id].clone();
    assert!(original.looping.is_some() && original.volume_envelope().is_some() && original.fade_out.length == 480);

    // Copy → paste → paste: two more clips end to end, each with everything, all on one file.
    s.click_clip(placement, false);
    assert!(s.copy() && s.paste() && s.paste());
    let start = 4 * BEAT + BEAT / 2;
    let lane = s.project.track(audio).unwrap().arrangement.clone();
    let mut starts: Vec<Tick> = lane.iter().map(|pl| pl.start).collect();
    starts.sort();
    assert_eq!(starts, [start, start + 5 * BEAT, start + 10 * BEAT]);
    let ids: BTreeSet<ClipId> = lane.iter().map(|pl| pl.clip).collect();
    assert_eq!(ids.len(), 3, "each copy is a clip of its own");
    assert!(lane.iter().all(|pl| s.project.clips[&pl.clip] == original), "fades, envelope and loop came along");
    assert_eq!(s.project.sources.len(), 1, "the file is shared");

    // The engine plays every copy exactly like the original.
    let samples = aitrack_engine::SampleCache::blocking();
    samples.insert("mem://ramp.wav", std::sync::Arc::new(aitrack_engine::DecodedAudio::from_planar(vec![data.clone()], 48_000)));
    let out = aitrack_engine::render_offline_with(&s.project, &aitrack_engine::NullFactory, (0, 20 * BEAT), 48_000., 256, samples).expect("render");
    let ch = out.channel(0);
    let at = |tick: Tick| tick as usize * SPT;
    let played = &ch[at(start)..at(start + 5 * BEAT)];
    assert!(played.iter().fold(0f32, |m, v| m.max(v.abs())) > 0.5, "the clip is heard");
    for copy in 1..3 {
        let from = at(start + copy * 5 * BEAT);
        let worst = played.iter().zip(&ch[from..]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(worst < 1e-5, "copy {copy} differs from the original by {worst}");
    }

    // The lane's notches, its waveform, the sample editor's axis and the
    // engine agree on where the loop starts over.
    let repeats: Vec<Tick> = overlay::loop_repeats(&original, 1.).into_iter().map(|x| x as Tick).collect();
    assert_eq!(repeats, [3 * BEAT / 2, 3 * BEAT, 9 * BEAT / 2]);
    let loop_start = BEAT / 2;
    let map = WaveMap::of(&original, SPT as f64, 1.);
    for r in repeats {
        assert_eq!(map.frames(r as f64).0, (loop_start as usize * SPT) as f64, "lane waveform at {r}");
        assert_eq!(unroll(original.looping, (original.offset + r) as f64), loop_start as f64, "sample editor at {r}");
        for n in [0, 100, 2000] {
            let (got, want) = (played[at(r) + n], data[at(loop_start) + n] * original.gain_at(r + (n / SPT) as Tick));
            assert!((got - want).abs() < 2e-3, "engine {n} samples after the repeat at {r}: {got} vs {want}");
        }
    }
}
