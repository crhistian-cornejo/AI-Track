//! Clip-editing operations as pure command builders.
//!
//! Each function works out the [`Command`]s for one edit (fades, gain, loop,
//! resize, trim, crop, split, duplicate) so the UI, scripts and agents share
//! the same rules. Nothing here changes the project's content: the builders
//! that create clips take `&mut Project` only to allocate ids. They return
//! `None` when the ids do not exist or the edit would change nothing.

use crate::command::Command;
use crate::ids::*;
use crate::project::*;
use crate::time::{Tick, PPQ};

/// Shortest clip the edge drags leave behind (a 1/64 note).
pub const MIN_CLIP_LEN: Tick = PPQ / 16;

/// Natural length of an audio clip's file in source ticks at the project
/// tempo; `None` for a notes clip or a file of unknown length.
pub fn source_len(p: &Project, clip: ClipId) -> Option<Tick> {
    match p.sources.get(&p.clips.get(&clip)?.source)? {
        Source::Audio(a) => Some(a.length_ticks(p.tempo.initial_bpm())).filter(|len| *len > 0),
        Source::Notes(_) => None,
    }
}

/// Set both fades. Lengths are kept inside the clip without overlapping (the
/// fade that changed gives way to the other) and curves inside -1..=1.
pub fn set_fades(p: &Project, clip: ClipId, fade_in: Fade, fade_out: Fade) -> Option<Command> {
    let old = p.clips.get(&clip)?;
    let len = old.length.max(0);
    let tidy = |f: Fade| Fade { length: f.length.clamp(0, len), curve: tidy_curve(f.curve) };
    let (mut fade_in, mut fade_out) = (tidy(fade_in), tidy(fade_out));
    if fade_in.length + fade_out.length > len {
        if fade_in.length != old.fade_in.length {
            fade_in.length = len - fade_out.length;
        } else {
            fade_out.length = len - fade_in.length;
        }
    }
    update(p, clip, Clip { fade_in, fade_out, ..old.clone() })
}

/// Set the clip gain, -70..=24 dB.
pub fn set_gain(p: &Project, clip: ClipId, gain_db: f32) -> Option<Command> {
    let old = p.clips.get(&clip)?;
    if !gain_db.is_finite() {
        return None;
    }
    update(p, clip, Clip { gain_db: gain_db.clamp(-70.0, 24.0), ..old.clone() })
}

/// Replace the volume envelope (see [`Clip::envelopes`]); no points removes it.
/// Points are sorted by tick (the last one wins on a shared tick) and values
/// clamped to 0..=1.
pub fn set_volume_envelope(p: &Project, clip: ClipId, mut points: Vec<AutomationPoint>) -> Option<Command> {
    let mut new = p.clips.get(&clip)?.clone();
    points.sort_by_key(|pt| pt.tick);
    points.dedup_by(|later, kept| {
        let same = later.tick == kept.tick;
        if same {
            *kept = *later;
        }
        same
    });
    for pt in &mut points {
        pt.value = if pt.value.is_finite() { pt.value.clamp(0.0, 1.0) } else { 1.0 };
        pt.curve = tidy_curve(pt.curve);
    }
    let at = new.envelopes.iter().position(|l| l.target == AutomationTarget::Volume);
    match (at, points.is_empty()) {
        (Some(i), true) => {
            new.envelopes.remove(i);
        }
        (Some(i), false) => {
            new.envelopes[i].points = points;
            new.envelopes[i].enabled = true;
        }
        (None, true) => return None,
        (None, false) => new.envelopes.push(AutomationLane { points, ..AutomationLane::new(AutomationTarget::Volume) }),
    }
    update(p, clip, new)
}

/// Loop the clip over `region` of its source, or stop looping with `None`.
/// The region is kept inside the file for audio; a clip that starts past the
/// region's end starts at the region instead.
pub fn set_loop(p: &Project, clip: ClipId, region: Option<LoopRegion>) -> Option<Command> {
    let mut new = p.clips.get(&clip)?.clone();
    new.looping = match region {
        None => None,
        Some(r) => {
            let start = r.start.max(0);
            let end = source_len(p, clip).map_or(r.end, |src| r.end.min(src));
            if end <= start {
                return None;
            }
            if new.offset >= end {
                new.offset = start;
            }
            Some(LoopRegion { start, end })
        }
    };
    update(p, clip, new)
}

/// Switch looping on over what the clip shows now (`offset .. offset +
/// length`, at most to the end of the file for audio), or off.
pub fn toggle_loop(p: &Project, clip: ClipId) -> Option<Command> {
    let c = p.clips.get(&clip)?;
    match c.looping {
        Some(_) => set_loop(p, clip, None),
        None => set_loop(p, clip, Some(LoopRegion { start: c.offset, end: c.offset + c.length })),
    }
}

/// The right-edge drag. An audio clip stretched past the end of its file
/// starts looping from its offset to the file's end; pulled back inside the
/// file, that automatic loop goes away again. A clip that already loops, and
/// any notes clip, only changes length.
pub fn resize_end(p: &Project, clip: ClipId, new_length: Tick) -> Option<Command> {
    let mut new = p.clips.get(&clip)?.clone();
    new.length = new_length.max(MIN_CLIP_LEN);
    if let Some(src) = source_len(p, clip) {
        let auto = LoopRegion { start: new.offset, end: src };
        let past_file = new.offset + new.length > src;
        match new.looping {
            None if past_file && src - new.offset >= MIN_CLIP_LEN => new.looping = Some(auto),
            Some(l) if l == auto && !past_file => new.looping = None,
            _ => {}
        }
    }
    clamp_fades(&mut new, Edge::End);
    update(p, clip, new)
}

/// The left-edge drag: move the placement's start to `new_start` and trim or
/// reveal the same amount of content, so what is left stays where it was on
/// the timeline (volume envelope included). Without a loop the clip cannot
/// start before its source does; inside a loop the offset wraps around.
pub fn trim_start(p: &Project, track: TrackId, placement: PlacementId, new_start: Tick) -> Option<Vec<Command>> {
    let place = find_placement(p, track, placement)?;
    let old = p.clips.get(&place.clip)?;
    let mut delta = (new_start.max(0) - place.start).min(old.length - MIN_CLIP_LEN);
    if !in_loop(old) {
        delta = delta.max(-old.offset);
    }
    if delta == 0 {
        return None;
    }
    let mut new = old.clone();
    new.offset = advance(old, delta);
    new.length -= delta;
    shift_envelope(&mut new, -delta);
    clamp_fades(&mut new, Edge::Start);
    Some(vec![
        Command::UpdateClip { id: place.clip, clip: Box::new(new) },
        Command::MovePlacement { track, id: placement, to_track: track, start: place.start + delta, lane: place.lane },
    ])
}

/// Keep only clip time `from..to` (the sample editor's "crop to selection").
pub fn crop(p: &Project, clip: ClipId, from: Tick, to: Tick) -> Option<Command> {
    let old = p.clips.get(&clip)?;
    let (from, to) = (from.min(to).clamp(0, old.length), from.max(to).clamp(0, old.length));
    if to - from < MIN_CLIP_LEN {
        return None;
    }
    let mut new = old.clone();
    new.offset = advance(old, from);
    new.length = to - from;
    shift_envelope(&mut new, -from);
    clamp_fades(&mut new, Edge::End);
    update(p, clip, new)
}

/// Cut a placed clip at the absolute tick `at` (strictly inside it) into two
/// independent clips. The left half keeps the clip id; the right half is a
/// new clip and placement on the same lane. The cut edges get no fade, the
/// volume envelope is divided with a point on each side of the cut, audio
/// halves share the file and a notes half gets its own copy of the notes.
pub fn split_at(p: &mut Project, track: TrackId, placement: PlacementId, at: Tick) -> Option<Vec<Command>> {
    let place = find_placement(p, track, placement)?.clone();
    let old = p.clips.get(&place.clip)?.clone();
    let cut = at - place.start;
    if cut <= 0 || cut >= old.length {
        return None;
    }
    let mut left = Clip { length: cut, fade_out: Fade::default(), ..old.clone() };
    let mut right = Clip { offset: advance(&old, cut), length: old.length - cut, fade_in: Fade::default(), ..old.clone() };
    for (i, lane) in old.envelopes.iter().enumerate() {
        if lane.target == AutomationTarget::Volume {
            let (before, after) = split_points(lane, cut);
            left.envelopes[i].points = before;
            right.envelopes[i].points = after;
        }
    }
    clamp_fades(&mut left, Edge::End);
    clamp_fades(&mut right, Edge::Start);
    let mut cmds = Vec::with_capacity(4);
    own_notes(p, &mut right, &mut cmds);
    let right_id = p.alloc_clip_id();
    let right_place = Placement { id: p.alloc_placement_id(), clip: right_id, start: at, lane: place.lane };
    cmds.push(Command::UpdateClip { id: place.clip, clip: Box::new(left) });
    cmds.push(Command::AddClip { id: right_id, clip: Box::new(right) });
    cmds.push(Command::PlaceClip { track, placement: right_place });
    Some(cmds)
}

/// An independent copy of a placed clip at `start` on the same track and
/// lane. Audio shares the file; a notes clip gets its own copy of the notes.
/// Returns the commands and the id of the new placement.
pub fn duplicate_placement(p: &mut Project, track: TrackId, placement: PlacementId, start: Tick) -> Option<(Vec<Command>, PlacementId)> {
    let place = find_placement(p, track, placement)?.clone();
    let mut copy = p.clips.get(&place.clip)?.clone();
    let mut cmds = Vec::with_capacity(3);
    own_notes(p, &mut copy, &mut cmds);
    let clip = p.alloc_clip_id();
    let id = p.alloc_placement_id();
    cmds.push(Command::AddClip { id: clip, clip: Box::new(copy) });
    cmds.push(Command::PlaceClip { track, placement: Placement { id, clip, start: start.max(0), lane: place.lane } });
    Some((cmds, id))
}

// ───────────────────────────── Helpers ─────────────────────────────

fn update(p: &Project, id: ClipId, new: Clip) -> Option<Command> {
    (p.clips.get(&id)? != &new).then(|| Command::UpdateClip { id, clip: Box::new(new) })
}

fn find_placement(p: &Project, track: TrackId, id: PlacementId) -> Option<&Placement> {
    p.track(track)?.arrangement.iter().find(|pl| pl.id == id)
}

fn tidy_curve(curve: f32) -> f32 {
    if curve.is_finite() { curve.clamp(-1.0, 1.0) } else { 0.0 }
}

/// Keep both fades inside the clip without overlapping. When the clip got too
/// short for both, the fade at the edge that moved gives way first.
fn clamp_fades(c: &mut Clip, moved: Edge) {
    let len = c.length.max(0);
    let (keep, give) = match moved {
        Edge::Start => (&mut c.fade_out, &mut c.fade_in),
        Edge::End => (&mut c.fade_in, &mut c.fade_out),
    };
    keep.length = keep.length.clamp(0, len);
    give.length = give.length.clamp(0, len - keep.length);
}

#[derive(Clone, Copy)]
enum Edge {
    Start,
    End,
}

/// Whether the clip starts inside (or past) its loop, where its offset wraps.
fn in_loop(c: &Clip) -> bool {
    c.looping.is_some_and(|l| l.end > l.start && c.offset >= l.start)
}

/// The source position `delta` ticks of clip time after the clip's offset
/// (before it when negative), wrapped inside the loop as playback does.
fn advance(c: &Clip, delta: Tick) -> Tick {
    let pos = c.offset + delta;
    match c.looping.filter(|l| l.end > l.start) {
        Some(l) if pos >= l.end || (delta < 0 && c.offset >= l.start) => l.start + (pos - l.start).rem_euclid(l.end - l.start),
        _ => pos,
    }
}

/// Move the volume envelope by `by` ticks of clip time.
fn shift_envelope(c: &mut Clip, by: Tick) {
    for lane in c.envelopes.iter_mut().filter(|l| l.target == AutomationTarget::Volume) {
        for pt in &mut lane.points {
            pt.tick += by;
        }
    }
}

/// Divide a volume envelope at clip time `cut`: the points before it plus one
/// at the cut, and one at 0 plus the points after it in the right half's clip
/// time. A curved segment that is cut keeps its exact shape on both sides.
fn split_points(lane: &AutomationLane, cut: Tick) -> (Vec<AutomationPoint>, Vec<AutomationPoint>) {
    let pts = &lane.points;
    let Some(value) = lane.value_at(cut) else { return (Vec::new(), Vec::new()) };
    let i = pts.partition_point(|pt| pt.tick < cut);
    let mut left = pts[..i].to_vec();
    let mut rest = i;
    let mut curve = 0.0;
    match pts.get(i) {
        Some(on_cut) if on_cut.tick == cut => {
            curve = on_cut.curve;
            rest = i + 1;
        }
        Some(next) if i > 0 => {
            // An exponential segment cut at fraction `f` is two exponential
            // segments with the curvature shared out in the same proportion.
            let prev = pts[i - 1];
            let f = (cut - prev.tick) as f32 / (next.tick - prev.tick).max(1) as f32;
            left[i - 1].curve = prev.curve * f;
            curve = prev.curve * (1.0 - f);
        }
        _ => {}
    }
    left.push(AutomationPoint { tick: cut, value, curve: 0.0 });
    let mut right = vec![AutomationPoint { tick: 0, value, curve }];
    right.extend(pts[rest..].iter().map(|pt| AutomationPoint { tick: pt.tick - cut, ..*pt }));
    (left, right)
}

/// Give a notes clip its own copy of its notes (new source and note ids).
fn own_notes(p: &mut Project, clip: &mut Clip, cmds: &mut Vec<Command>) {
    let Some(Source::Notes(src)) = p.sources.get(&clip.source).cloned() else { return };
    let id = p.alloc_source_id();
    let notes = src.notes.into_iter().map(|n| Note { id: p.alloc_note_id(), ..n }).collect();
    cmds.push(Command::AddSource { id, source: Box::new(Source::Notes(NoteSource { notes })) });
    clip.source = id;
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEAT: Tick = PPQ;

    struct Fixture {
        p: Project,
        track: TrackId,
        clip: ClipId,
        placement: PlacementId,
    }

    /// A track with one clip of `length` placed at beat 4. `audio_beats`
    /// makes it an audio clip over a file that long, else a notes clip.
    fn fixture(audio_beats: Option<i64>, length: Tick) -> Fixture {
        let mut p = Project::new("t");
        let track = p.alloc_track_id();
        let kind = if audio_beats.is_some() { TrackKind::Audio } else { TrackKind::Instrument };
        p.tracks.push(Track::new(track, kind, "T", Color(0)));
        let source = p.alloc_source_id();
        let content = match audio_beats {
            // 120 BPM: one beat is half a second.
            Some(beats) => Source::Audio(AudioSource {
                path: "a.wav".into(),
                hash: None,
                sample_rate: 48_000,
                channels: 2,
                frames: beats as u64 * 24_000,
                warp: Warp::default(),
                key: None,
            }),
            None => {
                let notes = (0..4).map(|i| Note::new(p.alloc_note_id(), 60 + i as u8, i * BEAT, BEAT / 2, 0.8)).collect();
                Source::Notes(NoteSource { notes })
            }
        };
        p.sources.insert(source, content);
        let clip = p.alloc_clip_id();
        p.clips.insert(clip, Clip::new("c", Color(0), source, length));
        let placement = p.alloc_placement_id();
        p.track_mut(track).unwrap().arrangement.push(Placement { id: placement, clip, start: 4 * BEAT, lane: 0 });
        Fixture { p, track, clip, placement }
    }

    /// Apply `cmds` as one edit, run `check` on the result, then undo and
    /// make sure the project is back where it started.
    fn round_trip(p: &mut Project, cmds: Vec<Command>, check: impl FnOnce(&Project)) {
        let mut before = p.clone();
        let inverse = Command::Batch { commands: cmds }.apply(p).expect("commands apply");
        check(p);
        inverse.apply(p).expect("inverse applies");
        before.ids = p.ids.clone();
        assert_eq!(*p, before, "undo restores the project");
    }

    fn edit(f: &mut Fixture, change: impl FnOnce(&mut Clip)) {
        change(f.p.clips.get_mut(&f.clip).unwrap());
    }

    fn envelope(points: &[(Tick, f32)]) -> AutomationLane {
        let points = points.iter().map(|&(tick, value)| AutomationPoint { tick, value, curve: 0.0 }).collect();
        AutomationLane { points, ..AutomationLane::new(AutomationTarget::Volume) }
    }

    #[test]
    fn source_len_only_for_audio_of_known_length() {
        let audio = fixture(Some(8), 4 * BEAT);
        assert_eq!(source_len(&audio.p, audio.clip), Some(8 * BEAT));
        let notes = fixture(None, 4 * BEAT);
        assert_eq!(source_len(&notes.p, notes.clip), None);
        let empty = fixture(Some(0), 4 * BEAT);
        assert_eq!(source_len(&empty.p, empty.clip), None);
        assert_eq!(source_len(&audio.p, ClipId(9999)), None);
    }

    #[test]
    fn fades_are_clamped_and_do_not_overlap() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let (clip, len) = (f.clip, 4 * BEAT);
        let cmd = set_fades(&f.p, clip, Fade { length: BEAT, curve: 3.0 }, Fade { length: -5, curve: -0.5 }).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!(p.clips[&clip].fade_in, Fade { length: BEAT, curve: 1.0 });
            assert_eq!(p.clips[&clip].fade_out, Fade { length: 0, curve: -0.5 });
        });
        // The fade being dragged stops at the other one.
        edit(&mut f, |c| c.fade_in.length = BEAT);
        let cmd = set_fades(&f.p, clip, Fade { length: BEAT, curve: 0.0 }, Fade { length: 10 * BEAT, curve: 0.0 }).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| assert_eq!(p.clips[&clip].fade_out.length, len - BEAT));
        edit(&mut f, |c| c.fade_out.length = BEAT);
        let cmd = set_fades(&f.p, clip, Fade { length: 10 * BEAT, curve: 0.0 }, Fade { length: BEAT, curve: 0.0 }).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| assert_eq!(p.clips[&clip].fade_in.length, len - BEAT));
        // Nothing to do.
        let c = f.p.clips[&clip].clone();
        assert_eq!(set_fades(&f.p, clip, c.fade_in, c.fade_out), None);
        assert_eq!(set_fades(&f.p, ClipId(9999), c.fade_in, c.fade_out), None);
    }

    #[test]
    fn gain_is_clamped() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let clip = f.clip;
        let cmd = set_gain(&f.p, clip, 40.0).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| assert_eq!(p.clips[&clip].gain_db, 24.0));
        let cmd = set_gain(&f.p, clip, -200.0).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| assert_eq!(p.clips[&clip].gain_db, -70.0));
        assert_eq!(set_gain(&f.p, clip, 0.0), None);
        assert_eq!(set_gain(&f.p, clip, f32::NAN), None);
    }

    #[test]
    fn volume_envelope_is_set_replaced_and_removed() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let clip = f.clip;
        let pt = |tick, value| AutomationPoint { tick, value, curve: 0.0 };
        assert_eq!(set_volume_envelope(&f.p, clip, Vec::new()), None);
        let cmd = set_volume_envelope(&f.p, clip, vec![pt(BEAT, 2.0), pt(0, -1.0), pt(BEAT, 0.5)]).unwrap();
        round_trip(&mut f.p, vec![cmd.clone()], |p| {
            let lane = p.clips[&clip].volume_envelope().expect("volume lane");
            assert_eq!(lane.points, vec![pt(0, 0.0), pt(BEAT, 0.5)]);
        });
        cmd.apply(&mut f.p).unwrap();
        let cmd = set_volume_envelope(&f.p, clip, vec![pt(0, 1.0)]).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!(p.clips[&clip].envelopes.len(), 1);
            assert_eq!(p.clips[&clip].gain_at(BEAT), 1.0);
        });
        let cmd = set_volume_envelope(&f.p, clip, Vec::new()).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| assert!(p.clips[&clip].envelopes.is_empty()));
    }

    #[test]
    fn loop_toggles_over_the_visible_content() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let clip = f.clip;
        edit(&mut f, |c| c.offset = BEAT);
        let cmd = toggle_loop(&f.p, clip).unwrap();
        round_trip(&mut f.p, vec![cmd.clone()], |p| assert_eq!(p.clips[&clip].looping, Some(LoopRegion { start: BEAT, end: 5 * BEAT })));
        cmd.apply(&mut f.p).unwrap();
        let cmd = toggle_loop(&f.p, clip).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| assert_eq!(p.clips[&clip].looping, None));
        // An audio loop ends with the file at the latest.
        let mut long = fixture(Some(8), 12 * BEAT);
        let clip = long.clip;
        let cmd = toggle_loop(&long.p, clip).unwrap();
        round_trip(&mut long.p, vec![cmd], |p| assert_eq!(p.clips[&clip].looping, Some(LoopRegion { start: 0, end: 8 * BEAT })));
        assert_eq!(set_loop(&long.p, clip, Some(LoopRegion { start: BEAT, end: BEAT })), None);
        assert_eq!(set_loop(&long.p, clip, None), None);
        // Notes have no file to end at.
        let mut notes = fixture(None, 12 * BEAT);
        let clip = notes.clip;
        let cmd = toggle_loop(&notes.p, clip).unwrap();
        round_trip(&mut notes.p, vec![cmd], |p| assert_eq!(p.clips[&clip].looping, Some(LoopRegion { start: 0, end: 12 * BEAT })));
    }

    #[test]
    fn stretching_audio_past_its_file_loops_it() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let clip = f.clip;
        edit(&mut f, |c| {
            c.offset = 2 * BEAT;
            c.fade_out.length = BEAT;
        });
        // Inside the file: only the length changes.
        let cmd = resize_end(&f.p, clip, 6 * BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!((p.clips[&clip].length, p.clips[&clip].looping), (6 * BEAT, None));
        });
        // Past the file: it loops what it was showing.
        let auto = LoopRegion { start: 2 * BEAT, end: 8 * BEAT };
        let cmd = resize_end(&f.p, clip, 20 * BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd.clone()], |p| {
            assert_eq!((p.clips[&clip].length, p.clips[&clip].looping), (20 * BEAT, Some(auto)));
        });
        cmd.apply(&mut f.p).unwrap();
        // Longer still keeps the loop; back inside the file removes it.
        let cmd = resize_end(&f.p, clip, 30 * BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| assert_eq!(p.clips[&clip].looping, Some(auto)));
        let cmd = resize_end(&f.p, clip, 3 * BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!((p.clips[&clip].length, p.clips[&clip].looping), (3 * BEAT, None));
        });
        // A loop the user set is left alone.
        let own = LoopRegion { start: 2 * BEAT, end: 4 * BEAT };
        edit(&mut f, |c| c.looping = Some(own));
        let cmd = resize_end(&f.p, clip, BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!(p.clips[&clip].looping, Some(own));
            assert_eq!(p.clips[&clip].fade_out.length, BEAT, "the fade fits the shorter clip");
        });
        // Never shorter than the minimum, and fades shrink with the clip.
        let cmd = resize_end(&f.p, clip, 0).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!(p.clips[&clip].length, MIN_CLIP_LEN);
            assert_eq!(p.clips[&clip].fade_out.length, MIN_CLIP_LEN);
        });
        assert_eq!(resize_end(&f.p, clip, 20 * BEAT), None, "already that long");
        assert_eq!(resize_end(&f.p, ClipId(9999), BEAT), None);
    }

    #[test]
    fn resizing_notes_only_changes_the_length() {
        let mut f = fixture(None, 4 * BEAT);
        let clip = f.clip;
        let cmd = resize_end(&f.p, clip, 40 * BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!((p.clips[&clip].length, p.clips[&clip].looping), (40 * BEAT, None));
        });
    }

    #[test]
    fn trimming_the_start_keeps_content_in_place() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let start_of = |p: &Project| p.track(track).unwrap().arrangement.iter().find(|pl| pl.id == placement).unwrap().start;
        edit(&mut f, |c| {
            c.offset = BEAT;
            c.fade_in.length = 3 * BEAT;
            c.fade_out.length = BEAT;
            c.envelopes.push(envelope(&[(0, 1.0), (2 * BEAT, 0.5)]));
        });
        // Trim one beat off the front.
        let cmds = trim_start(&f.p, track, placement, 5 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let c = &p.clips[&clip];
            assert_eq!((start_of(p), c.offset, c.length), (5 * BEAT, 2 * BEAT, 3 * BEAT));
            assert_eq!(c.envelopes[0].points.iter().map(|pt| pt.tick).collect::<Vec<_>>(), vec![-BEAT, BEAT]);
            // The clip is too short for both fades now: the one at the trimmed edge gives way.
            assert_eq!((c.fade_in.length, c.fade_out.length), (2 * BEAT, BEAT));
            assert_eq!(c.gain_at(3 * BEAT), 0.0, "the fade-out still ends the clip");
        });
        // Reveal: the source starts one beat before the clip, no further.
        let cmds = trim_start(&f.p, track, placement, 0).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let c = &p.clips[&clip];
            assert_eq!((start_of(p), c.offset, c.length), (3 * BEAT, 0, 5 * BEAT));
        });
        // At least the minimum length stays.
        let cmds = trim_start(&f.p, track, placement, 100 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            assert_eq!(p.clips[&clip].length, MIN_CLIP_LEN);
            assert_eq!(start_of(p), 8 * BEAT - MIN_CLIP_LEN);
        });
        assert_eq!(trim_start(&f.p, track, placement, 4 * BEAT), None);
        assert_eq!(trim_start(&f.p, track, PlacementId(9999), 0), None);
    }

    #[test]
    fn trimming_a_looping_clip_wraps_its_offset() {
        let mut f = fixture(Some(8), 8 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        edit(&mut f, |c| {
            c.offset = 3 * BEAT;
            c.looping = Some(LoopRegion { start: 2 * BEAT, end: 4 * BEAT });
        });
        // Forward over the loop end.
        let cmds = trim_start(&f.p, track, placement, 6 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| assert_eq!((p.clips[&clip].offset, p.clips[&clip].length), (3 * BEAT, 6 * BEAT)));
        let cmds = trim_start(&f.p, track, placement, 5 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| assert_eq!(p.clips[&clip].offset, 2 * BEAT));
        // Backward wraps to the loop end instead of leaving the loop.
        let cmds = trim_start(&f.p, track, placement, BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| assert_eq!((p.clips[&clip].offset, p.clips[&clip].length), (2 * BEAT, 11 * BEAT)));
        let cmds = trim_start(&f.p, track, placement, 2 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| assert_eq!(p.clips[&clip].offset, 3 * BEAT));
    }

    #[test]
    fn crop_keeps_the_selected_part() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let clip = f.clip;
        edit(&mut f, |c| {
            c.offset = BEAT;
            c.fade_in.length = 2 * BEAT;
            c.fade_out.length = 2 * BEAT;
            c.envelopes.push(envelope(&[(0, 0.0), (4 * BEAT, 1.0)]));
        });
        let before = f.p.clips[&clip].volume_envelope().unwrap().value_at(2 * BEAT);
        let cmd = crop(&f.p, clip, 3 * BEAT, BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            let c = &p.clips[&clip];
            assert_eq!((c.offset, c.length), (2 * BEAT, 2 * BEAT));
            assert_eq!(c.volume_envelope().unwrap().value_at(BEAT), before, "the envelope moved with the audio");
            assert_eq!((c.fade_in.length, c.fade_out.length), (2 * BEAT, 0));
        });
        assert_eq!(crop(&f.p, clip, 0, 4 * BEAT), None);
        assert_eq!(crop(&f.p, clip, BEAT, BEAT + 1), None);
    }

    #[test]
    fn split_makes_two_independent_audio_clips() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        edit(&mut f, |c| {
            c.offset = BEAT;
            c.fade_in = Fade { length: BEAT, curve: 0.3 };
            c.fade_out = Fade { length: BEAT, curve: -0.3 };
            let mut lane = envelope(&[(0, 0.2), (2 * BEAT, 1.0), (4 * BEAT, 0.4)]);
            lane.points[0].curve = 0.8;
            c.envelopes.push(lane);
        });
        let whole = f.p.clips[&clip].clone();
        let cut = BEAT + BEAT / 2;
        let cmds = split_at(&mut f.p, track, placement, 4 * BEAT + cut).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let arrangement = &p.track(track).unwrap().arrangement;
            assert_eq!(arrangement.len(), 2);
            let right_place = arrangement.iter().find(|pl| pl.id != placement).unwrap();
            assert_eq!((right_place.start, right_place.lane), (4 * BEAT + cut, 0));
            assert_ne!(right_place.clip, clip);
            let (left, right) = (&p.clips[&clip], &p.clips[&right_place.clip]);
            assert_eq!((left.offset, left.length), (BEAT, cut));
            assert_eq!((right.offset, right.length), (BEAT + cut, 4 * BEAT - cut));
            assert_eq!(left.source, right.source, "audio halves share the file");
            assert_eq!((left.fade_in, left.fade_out), (whole.fade_in, Fade::default()));
            assert_eq!((right.fade_in, right.fade_out), (Fade::default(), whole.fade_out));
            // The envelope reads the same on both sides as it did before the cut.
            let (was, l, r) = (whole.volume_envelope().unwrap(), left.volume_envelope().unwrap(), right.volume_envelope().unwrap());
            assert_eq!(l.points.last().unwrap().tick, cut);
            assert_eq!(l.value_at(cut), r.value_at(0));
            for t in (0..cut).step_by(60) {
                assert!((l.value_at(t).unwrap() - was.value_at(t).unwrap()).abs() < 1e-4, "left at {t}");
            }
            for t in (0..4 * BEAT - cut).step_by(60) {
                assert!((r.value_at(t).unwrap() - was.value_at(t + cut).unwrap()).abs() < 1e-4, "right at {t}");
            }
        });
        // Only strictly inside the clip.
        assert_eq!(split_at(&mut f.p, track, placement, 4 * BEAT), None);
        assert_eq!(split_at(&mut f.p, track, placement, 8 * BEAT), None);
        assert_eq!(split_at(&mut f.p, track, PlacementId(9999), 5 * BEAT), None);
    }

    #[test]
    fn split_wraps_a_looping_right_half() {
        let mut f = fixture(Some(8), 8 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        edit(&mut f, |c| c.looping = Some(LoopRegion { start: 0, end: 2 * BEAT }));
        let cmds = split_at(&mut f.p, track, placement, 9 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let right = p.clips.iter().find(|(id, _)| **id != clip).unwrap().1;
            assert_eq!((right.offset, right.length), (BEAT, 3 * BEAT));
            assert_eq!(right.looping, Some(LoopRegion { start: 0, end: 2 * BEAT }));
        });
    }

    #[test]
    fn split_notes_gives_the_right_half_its_own_notes() {
        let mut f = fixture(None, 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let cmds = split_at(&mut f.p, track, placement, 6 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let (left, right) = (&p.clips[&clip], p.clips.iter().find(|(id, _)| **id != clip).unwrap().1);
            assert_eq!((right.offset, right.length), (2 * BEAT, 2 * BEAT));
            assert_ne!(left.source, right.source);
            let (Source::Notes(a), Source::Notes(b)) = (&p.sources[&left.source], &p.sources[&right.source]) else { panic!("notes") };
            assert_eq!(a.notes.len(), b.notes.len());
            for (x, y) in a.notes.iter().zip(&b.notes) {
                assert_ne!(x.id, y.id);
                assert_eq!(Note { id: y.id, ..x.clone() }, *y);
            }
        });
    }

    #[test]
    fn duplicate_copies_notes_and_shares_audio() {
        let mut f = fixture(None, 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let (cmds, new_place) = duplicate_placement(&mut f.p, track, placement, 8 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let copy = p.track(track).unwrap().arrangement.iter().find(|pl| pl.id == new_place).unwrap();
            assert_eq!(copy.start, 8 * BEAT);
            assert_ne!(copy.clip, clip);
            assert_ne!(p.clips[&copy.clip].source, p.clips[&clip].source);
            assert_eq!(Clip { source: p.clips[&clip].source, ..p.clips[&copy.clip].clone() }, p.clips[&clip]);
        });
        let mut f = fixture(Some(8), 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let (cmds, new_place) = duplicate_placement(&mut f.p, track, placement, 8 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let copy = p.track(track).unwrap().arrangement.iter().find(|pl| pl.id == new_place).unwrap();
            assert_eq!(p.clips[&copy.clip], p.clips[&clip]);
            assert_eq!(p.sources.len(), 1);
        });
        assert!(duplicate_placement(&mut f.p, track, PlacementId(9999), 0).is_none());
    }
}
