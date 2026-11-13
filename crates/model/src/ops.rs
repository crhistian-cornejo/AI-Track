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

/// Source ticks an audio clip plays per tick of clip time: its transpose and
/// detune as a speed when unwarped or in Re-Pitch, or 1 for pitch-preserving
/// warp modes (`Beats`, `Tones`, `Texture`, `Complex`, `ComplexPro`).
pub fn clip_rate(p: &Project, clip: ClipId) -> f64 {
    match p.clips.get(&clip).and_then(|c| p.sources.get(&c.source)) {
        Some(Source::Audio(a)) => a.warp.speed_ratio().max(1e-6),
        _ => 1.0,
    }
}

/// How much clip time an audio clip has from its offset to the end of its
/// file (playback speed included); `None` for a notes clip or a file of
/// unknown length. A one-shot clip never grows past this.
pub fn file_room(p: &Project, clip: ClipId) -> Option<Tick> {
    let src = source_len(p, clip)?;
    let offset = p.clips.get(&clip)?.offset;
    Some((((src - offset) as f64) / clip_rate(p, clip)).floor().max(0.0) as Tick)
}

/// The right-edge drag, as in Live. A clip that loops repeats its loop for
/// as long as it is made; a one-shot audio clip (loop off) only shortens or
/// restores its tail, up to the end of its file: it never grows past the
/// file and never starts looping by itself. The fades stay at their edges
/// (the fade-out shapes the cut tail) and only shrink when the clip gets too
/// short for them. A notes clip only changes length.
pub fn resize_end(p: &Project, clip: ClipId, new_length: Tick) -> Option<Command> {
    let mut new = p.clips.get(&clip)?.clone();
    let mut length = new_length.max(MIN_CLIP_LEN);
    if new.looping.is_none_or(|l| l.end <= l.start)
        && let Some(room) = file_room(p, clip)
    {
        length = length.min(room.max(MIN_CLIP_LEN));
    }
    new.length = length;
    clamp_fades(&mut new, Edge::End);
    update(p, clip, new)
}

/// The left-edge drag: move the placement's start to `new_start` and trim or
/// reveal the same amount of content, so what is left stays where it was on
/// the timeline (volume envelope included). Without a loop an audio clip
/// cannot start before its file does; a notes clip can (empty time before
/// its notes); inside a loop the offset wraps around. A notes clip is left
/// with an offset: [`rebase_notes`] settles it when the drag ends.
pub fn trim_start(p: &Project, track: TrackId, placement: PlacementId, new_start: Tick) -> Option<Vec<Command>> {
    let place = find_placement(p, track, placement)?;
    let old = p.clips.get(&place.clip)?;
    let mut delta = (new_start.max(0) - place.start).min(old.length - MIN_CLIP_LEN);
    // A notes clip can grow to the left past its first note (the offset goes
    // negative until [`rebase_notes`] moves the notes instead); audio cannot
    // start before its file.
    let notes = matches!(p.sources.get(&old.source), Some(Source::Notes(_)));
    let rate = clip_rate(p, place.clip);
    if !in_loop(old) && !notes {
        delta = delta.max(-((old.offset as f64 / rate).floor() as Tick));
    }
    if delta == 0 {
        return None;
    }
    let mut new = old.clone();
    new.offset = advance(old, delta, rate).max(if in_loop(old) || notes { Tick::MIN } else { 0 });
    new.length -= delta;
    shift_envelope(&mut new, -delta);
    clamp_fades(&mut new, Edge::Start);
    Some(vec![
        Command::UpdateClip { id: place.clip, clip: Box::new(new) },
        Command::MovePlacement { track, id: placement, to_track: track, start: place.start + delta, lane: place.lane },
    ])
}

/// `clip` stretched in time to `new_length` ticks, Re-Pitch style (FL
/// Studio's stretch, Live's Re-Pitch warp): the audio is resampled to fit
/// and its pitch follows. `warp` is its file's warp; `bpm` the project
/// tempo. The file is warped at an original tempo scaled by the stretch, so
/// it plays slower when longer; offset, loop, fades and envelopes (in source
/// or clip ticks) scale with it, so the clip plays exactly the same audio,
/// slower or faster. Transpose and detune stay on top. `None` for an empty
/// clip or a length below [`MIN_CLIP_LEN`].
pub fn stretched(clip: &Clip, warp: &Warp, bpm: f64, new_length: Tick) -> Option<(Clip, Warp)> {
    if clip.length <= 0 || new_length < MIN_CLIP_LEN || bpm.is_nan() || bpm <= 0.0 {
        return None;
    }
    let k = new_length as f64 / clip.length as f64;
    let scale = |t: Tick| (t as f64 * k).round() as Tick;
    let mut c = clip.clone();
    c.offset = scale(clip.offset);
    c.length = new_length;
    c.looping = clip.looping.map(|l| LoopRegion { start: scale(l.start), end: scale(l.end) });
    c.fade_in.length = scale(clip.fade_in.length);
    c.fade_out.length = scale(clip.fade_out.length);
    for lane in &mut c.envelopes {
        for pt in &mut lane.points {
            pt.tick = scale(pt.tick);
        }
    }
    clamp_fades(&mut c, Edge::End);
    let original = match warp.original_bpm {
        Some(b) if warp.enabled && b > 0.0 => b,
        _ => bpm,
    };
    let mut w = warp.clone();
    if !w.enabled {
        w.mode = WarpMode::Repitch;
    }
    w.enabled = true;
    w.original_bpm = Some(original * k);
    for m in &mut w.markers {
        m.1 = scale(m.1);
    }
    Some((c, w))
}

/// How much a clip is stretched: its playback speed from warping relative to
/// the file as recorded at the project tempo (1 = not stretched, 0.5 = half
/// speed, twice as long). Transpose is not included.
pub fn stretch_factor(warp: &Warp, bpm: f64) -> f64 {
    match warp.original_bpm {
        Some(b) if warp.enabled && b > 0.0 && bpm > 0.0 => b / bpm,
        _ => 1.0,
    }
}

/// The commands that take clip `id` to `origin` (with its file's warp
/// `origin_warp`) stretched to `new_length` (see [`stretched`]). Only this
/// clip changes: a file it shares with other clips is copied for it first,
/// as a transpose is. With `keep_end` (the left edge) every placement of the
/// clip keeps its end where it was and moves its start instead. `origin` is
/// the clip as the drag found it, so every move of a drag starts from the
/// same state; back at the original length the clip and warp are restored.
pub fn stretch(p: &mut Project, id: ClipId, origin: &Clip, origin_warp: &Warp, new_length: Tick, keep_end: Option<&[(TrackId, PlacementId, Tick)]>) -> Option<Vec<Command>> {
    let now = p.clips.get(&id)?.clone();
    let Some(Source::Audio(file)) = p.sources.get(&now.source).cloned() else { return None };
    let (mut clip, warp) = if new_length == origin.length {
        (origin.clone(), origin_warp.clone())
    } else {
        stretched(origin, origin_warp, p.tempo.initial_bpm(), new_length)?
    };
    let mut cmds = Vec::new();
    let shared = p.clips.iter().any(|(other, c)| *other != id && c.source == now.source);
    if warp == file.warp {
        clip.source = now.source;
    } else if shared {
        let source = p.alloc_source_id();
        cmds.push(Command::AddSource { id: source, source: Box::new(Source::Audio(AudioSource { warp, ..file })) });
        clip.source = source;
    } else {
        cmds.push(Command::SetWarp { source: now.source, warp: Box::new(warp) });
        clip.source = now.source;
    }
    if clip != now {
        cmds.push(Command::UpdateClip { id, clip: Box::new(clip) });
    }
    for &(track, placement, start) in keep_end.unwrap_or(&[]) {
        let Some(pl) = find_placement(p, track, placement) else { continue };
        let to = (start + origin.length - new_length).max(0);
        if pl.start != to {
            cmds.push(Command::MovePlacement { track, id: placement, to_track: track, start: to, lane: pl.lane });
        }
    }
    (!cmds.is_empty()).then_some(cmds)
}

/// Keep only clip time `from..to` (the sample editor's "crop to selection").
pub fn crop(p: &Project, clip: ClipId, from: Tick, to: Tick) -> Option<Command> {
    let old = p.clips.get(&clip)?;
    let (from, to) = (from.min(to).clamp(0, old.length), from.max(to).clamp(0, old.length));
    if to - from < MIN_CLIP_LEN {
        return None;
    }
    let mut new = old.clone();
    new.offset = advance(old, from, clip_rate(p, clip));
    new.length = to - from;
    shift_envelope(&mut new, -from);
    clamp_fades(&mut new, Edge::End);
    update(p, clip, new)
}

/// A cut worked out by [`split_placement`].
#[derive(Clone, Debug, PartialEq)]
pub struct Split {
    pub commands: Vec<Command>,
    /// The clip of the part before the cut, which keeps the placement id:
    /// the clip that was cut, or a new one when that clip has other users.
    pub left: ClipId,
    /// The new clip and placement of the part after the cut.
    pub right: ClipId,
    pub right_placement: PlacementId,
}

/// Cut a placed clip at the absolute tick `at` (strictly inside it) into two
/// independent clips. The left half keeps the clip id; the right half is a
/// new clip and placement on the same lane. The cut edges get no fade, the
/// volume envelope is divided with a point on each side of the cut and audio
/// halves share the file with offsets of their own.
///
/// Each notes half gets notes of its own holding only what it plays, from
/// tick 0 (see [`own_trimmed_notes`]): a note that crosses the cut is
/// shortened to end at it in the left half and is not in the right half,
/// which never played it. Opened in the piano roll, each half shows and
/// edits just its own notes.
///
/// A clip that is also placed somewhere else, or sits in a session slot, is
/// left as it is for its other users: the left half becomes a new clip too,
/// and the placement that was cut (same id) points at it.
pub fn split_at(p: &mut Project, track: TrackId, placement: PlacementId, at: Tick) -> Option<Vec<Command>> {
    split_placement(p, track, placement, at).map(|split| split.commands)
}

/// [`split_at`], with the ids of the two halves.
pub fn split_placement(p: &mut Project, track: TrackId, placement: PlacementId, at: Tick) -> Option<Split> {
    let place = find_placement(p, track, placement)?.clone();
    let old = p.clips.get(&place.clip)?.clone();
    let cut = at - place.start;
    if cut <= 0 || cut >= old.length {
        return None;
    }
    let mut left = Clip { length: cut, fade_out: Fade::default(), ..old.clone() };
    let mut right = Clip { offset: advance(&old, cut, clip_rate(p, place.clip)), length: old.length - cut, fade_in: Fade::default(), ..old.clone() };
    for (i, lane) in old.envelopes.iter().enumerate() {
        if lane.target == AutomationTarget::Volume {
            let (before, after) = split_points(lane, cut);
            left.envelopes[i].points = before;
            right.envelopes[i].points = after;
        }
    }
    clamp_fades(&mut left, Edge::End);
    clamp_fades(&mut right, Edge::Start);
    let mut cmds = Vec::with_capacity(9);
    let shared = p.tracks.iter().any(|t| t.arrangement.iter().any(|pl| pl.clip == place.clip && pl.id != placement) || t.session_slots.values().any(|c| *c == place.clip));
    own_trimmed_notes(p, &mut left, &mut cmds);
    let left_id = if shared {
        let id = p.alloc_clip_id();
        cmds.push(Command::AddClip { id, clip: Box::new(left) });
        cmds.push(Command::RemovePlacement { track, id: placement });
        cmds.push(Command::PlaceClip { track, placement: Placement { clip: id, ..place.clone() } });
        id
    } else {
        let notes_replaced = left.source != old.source;
        cmds.push(Command::UpdateClip { id: place.clip, clip: Box::new(left) });
        // The left half has notes of its own now; nothing else plays the old ones.
        if notes_replaced && !p.clips.iter().any(|(id, c)| *id != place.clip && c.source == old.source) {
            cmds.push(Command::RemoveSource { id: old.source });
        }
        place.clip
    };
    own_trimmed_notes(p, &mut right, &mut cmds);
    let right_id = p.alloc_clip_id();
    let right_place = Placement { id: p.alloc_placement_id(), clip: right_id, start: at, lane: place.lane };
    let right_placement = right_place.id;
    cmds.push(Command::AddClip { id: right_id, clip: Box::new(right) });
    cmds.push(Command::PlaceClip { track, placement: right_place });
    Some(Split { commands: cmds, left: left_id, right: right_id, right_placement })
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

/// Notes of a clip as they are heard: copies with their start in ticks from
/// the clip's start and their length cut where they stop sounding (the end
/// of the clip, and of the loop pass they start in), with the loop unrolled
/// over the clip's length. Notes outside what the clip plays are left out;
/// ids are kept.
pub fn heard_notes(clip: &Clip, notes: &[Note]) -> Vec<Note> {
    let mut out = Vec::new();
    let length = clip.length.max(0);
    match clip.looping.filter(|l| l.end > l.start) {
        None => {
            for n in notes.iter().filter(|n| n.start >= clip.offset && n.start < clip.offset + length) {
                let at = n.start - clip.offset;
                out.push(Note { start: at, length: n.length.min(length - at), ..n.clone() });
            }
        }
        Some(l) => {
            // The first pass runs from the offset to the loop's end, the rest loop.
            let mut from = if clip.offset >= l.end { l.start + (clip.offset - l.start) % (l.end - l.start) } else { clip.offset };
            let mut base = 0;
            while base < length {
                for n in notes.iter().filter(|n| n.start >= from && n.start < l.end) {
                    let at = base + n.start - from;
                    if at < length {
                        out.push(Note { start: at, length: n.length.min(length - at).min(l.end - n.start), ..n.clone() });
                    }
                }
                base += l.end - from;
                from = l.start;
            }
        }
    }
    out.sort_by_key(|n| (n.start, n.key));
    out
}

/// Settle a notes clip that does not loop onto offset 0: its notes move by
/// the offset so they keep their place on the timeline. Notes that start
/// before the clip (`start < 0`) or past its end are kept (neither plays while
/// outside `0..length`), so dragging the left or right edge back out restores
/// them. The clip gets notes of its own; the old ones are removed
/// when nothing else uses them. `None` when there is nothing to settle (a
/// looping or audio clip, or offset 0 already). This is what the left-edge
/// trim ends with, so the piano roll, which shows a clip's notes from tick
/// 0, displays and edits the active region non-destructively.
pub fn rebase_notes(p: &mut Project, clip: ClipId) -> Option<Vec<Command>> {
    let old = p.clips.get(&clip)?.clone();
    if old.offset == 0 || old.looping.is_some_and(|l| l.end > l.start) {
        return None;
    }
    let Some(Source::Notes(src)) = p.sources.get(&old.source) else { return None };
    let kept: Vec<Note> = src.notes.iter().map(|n| Note { start: n.start - old.offset, ..n.clone() }).collect();
    let source = p.alloc_source_id();
    let notes = kept.into_iter().map(|n| Note { id: p.alloc_note_id(), ..n }).collect();
    let looping = old.looping.map(|l| LoopRegion { start: l.start - old.offset, end: l.end - old.offset });
    let mut cmds = vec![
        Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource { notes })) },
        Command::UpdateClip { id: clip, clip: Box::new(Clip { source, offset: 0, looping, ..old.clone() }) },
    ];
    if !p.clips.iter().any(|(id, c)| *id != clip && c.source == old.source) {
        cmds.push(Command::RemoveSource { id: old.source });
    }
    Some(cmds)
}

/// [`rebase_notes`] for every notes clip of the project that needs it (a
/// project saved before clips were settled this way).
pub fn rebase_all_notes(p: &mut Project) -> Vec<Command> {
    let clips: Vec<ClipId> = p.clips.keys().copied().collect();
    let mut cmds = Vec::new();
    for id in clips {
        if let Some(mut c) = rebase_notes(p, id) {
            cmds.append(&mut c);
        }
    }
    cmds
}

/// What [`consolidate_notes`] and [`consolidate_audio`] made.
#[derive(Clone, Debug, PartialEq)]
pub struct Consolidated {
    pub commands: Vec<Command>,
    pub clip: ClipId,
    pub placement: PlacementId,
}

/// Ctrl+J on MIDI clips: replace the placements `parts` of `track` with one
/// notes clip over `start..end` holding the notes they play, each where it
/// was heard (loops written out, gaps kept as silence). The clip takes the
/// name, color, scale, gain and activation of the earliest part. Clips and
/// notes nothing uses any more are removed. `None` when a part is missing,
/// is not a notes clip, or the range is empty.
pub fn consolidate_notes(p: &mut Project, track: TrackId, parts: &[PlacementId], start: Tick, end: Tick) -> Option<Consolidated> {
    let placed = gather(p, track, parts, start, end)?;
    let mut notes = Vec::new();
    for (pl, clip) in &placed {
        let Some(Source::Notes(src)) = p.sources.get(&clip.source) else { return None };
        notes.extend(heard_notes(clip, &src.notes).into_iter().map(|n| Note { start: pl.start - start + n.start, ..n }));
    }
    notes.sort_by_key(|n| (n.start, n.key));
    for n in &mut notes {
        n.id = p.alloc_note_id();
    }
    let first = &placed[0].1;
    let source = p.alloc_source_id();
    let joined = Clip { scale: first.scale, gain_db: first.gain_db, muted: first.muted, ..Clip::new(first.name.clone(), first.color, source, end - start) };
    Some(replace(p, track, &placed, Source::Notes(NoteSource { notes }), source, joined, start))
}

/// Ctrl+J on audio clips, once their sound is rendered to `file` (from
/// `start` to `end`, gain, fades, envelopes and warp applied): replace the
/// placements `parts` of `track` with one plain clip that plays it. The
/// clip takes the name and color of the earliest part. Clips and sources
/// nothing uses any more are removed (the files stay on disk). `None` when
/// a part is missing, is not an audio clip, or the range is empty.
pub fn consolidate_audio(p: &mut Project, track: TrackId, parts: &[PlacementId], start: Tick, end: Tick, file: AudioSource) -> Option<Consolidated> {
    let placed = gather(p, track, parts, start, end)?;
    if placed.iter().any(|(_, c)| !matches!(p.sources.get(&c.source), Some(Source::Audio(_)))) {
        return None;
    }
    let first = &placed[0].1;
    let source = p.alloc_source_id();
    let joined = Clip::new(first.name.clone(), first.color, source, end - start);
    Some(replace(p, track, &placed, Source::Audio(file), source, joined, start))
}

/// The placements `parts` of `track` with their clips, earliest first.
fn gather(p: &Project, track: TrackId, parts: &[PlacementId], start: Tick, end: Tick) -> Option<Vec<(Placement, Clip)>> {
    if parts.is_empty() || end <= start {
        return None;
    }
    let mut placed = Vec::with_capacity(parts.len());
    for id in parts {
        let pl = find_placement(p, track, *id)?.clone();
        let clip = p.clips.get(&pl.clip)?.clone();
        placed.push((pl, clip));
    }
    placed.sort_by_key(|(pl, _)| pl.start);
    placed.dedup_by_key(|(pl, _)| pl.id);
    Some(placed)
}

/// Remove `placed` (and the clips and sources only they used), then place
/// `clip` over the new `content` at `start`.
fn replace(p: &mut Project, track: TrackId, placed: &[(Placement, Clip)], content: Source, source: SourceId, clip: Clip, start: Tick) -> Consolidated {
    let mut cmds = Vec::with_capacity(placed.len() * 3 + 3);
    let gone: Vec<PlacementId> = placed.iter().map(|(pl, _)| pl.id).collect();
    for id in &gone {
        cmds.push(Command::RemovePlacement { track, id: *id });
    }
    let used = |clip: ClipId| {
        p.tracks.iter().any(|t| t.arrangement.iter().any(|pl| pl.clip == clip && !(t.id == track && gone.contains(&pl.id))) || t.session_slots.values().any(|c| *c == clip))
    };
    let mut dropped: Vec<ClipId> = placed.iter().map(|(pl, _)| pl.clip).filter(|c| !used(*c)).collect();
    dropped.sort();
    dropped.dedup();
    for id in &dropped {
        cmds.push(Command::RemoveClip { id: *id });
    }
    let mut sources: Vec<SourceId> = dropped.iter().filter_map(|id| p.clips.get(id).map(|c| c.source)).collect();
    sources.sort();
    sources.dedup();
    for id in sources {
        if !p.clips.iter().any(|(cid, c)| c.source == id && !dropped.contains(cid)) {
            cmds.push(Command::RemoveSource { id });
        }
    }
    let clip_id = p.alloc_clip_id();
    let placement = p.alloc_placement_id();
    let lane = placed[0].0.lane;
    cmds.push(Command::AddSource { id: source, source: Box::new(content) });
    cmds.push(Command::AddClip { id: clip_id, clip: Box::new(clip) });
    cmds.push(Command::PlaceClip { track, placement: Placement { id: placement, clip: clip_id, start, lane } });
    Consolidated { commands: cmds, clip: clip_id, placement }
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
/// (before it when negative) at `rate` source ticks per clip tick, wrapped
/// inside the loop as playback does.
fn advance(c: &Clip, delta: Tick, rate: f64) -> Tick {
    let pos = c.offset + (delta as f64 * rate).round() as Tick;
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

/// Give a notes clip notes of its own (new source and note ids) that hold
/// only what it plays, so the piano roll shows exactly that from tick 0:
/// without a loop, the heard notes ([`heard_notes`]) and offset 0; with a
/// loop, the notes from the offset (or the loop start, when earlier) to the
/// loop end, moved so that point is tick 0, and the offset and loop moved
/// with them. The clip plays the same either way. Audio is left alone.
fn own_trimmed_notes(p: &mut Project, clip: &mut Clip, cmds: &mut Vec<Command>) {
    let Some(Source::Notes(src)) = p.sources.get(&clip.source) else { return };
    let notes: Vec<Note> = match clip.looping.filter(|l| l.end > l.start) {
        None => {
            let heard = heard_notes(clip, &src.notes);
            clip.offset = 0;
            heard
        }
        Some(l) => {
            let base = clip.offset.min(l.start).max(0);
            let kept = src
                .notes
                .iter()
                .filter(|n| n.start >= base && n.start < l.end)
                .map(|n| Note { start: n.start - base, length: n.length.min(l.end - n.start), ..n.clone() })
                .collect();
            clip.offset -= base;
            clip.looping = Some(LoopRegion { start: l.start - base, end: l.end - base });
            kept
        }
    };
    let id = p.alloc_source_id();
    let notes = notes.into_iter().map(|n| Note { id: p.alloc_note_id(), ..n }).collect();
    cmds.push(Command::AddSource { id, source: Box::new(Source::Notes(NoteSource { notes })) });
    clip.source = id;
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
    use std::collections::BTreeSet;

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
    fn a_one_shot_never_grows_past_its_file_or_loops_by_itself() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let clip = f.clip;
        edit(&mut f, |c| {
            c.offset = 2 * BEAT;
            c.fade_out.length = BEAT;
        });
        assert_eq!(file_room(&f.p, clip), Some(6 * BEAT));
        // Inside the file: only the length changes.
        let cmd = resize_end(&f.p, clip, 5 * BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!((p.clips[&clip].length, p.clips[&clip].looping), (5 * BEAT, None));
        });
        // Past the file: it stops at the end of the file and does not loop.
        let cmd = resize_end(&f.p, clip, 20 * BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!((p.clips[&clip].length, p.clips[&clip].looping), (6 * BEAT, None));
            assert_eq!(p.clips[&clip].fade_out.length, BEAT, "the fade-out stays at the end");
        });
        // Shortening cuts the tail: the fade-out shapes the cut, the fade-in the attack.
        edit(&mut f, |c| c.fade_in.length = BEAT);
        let cmd = resize_end(&f.p, clip, 3 * BEAT / 2).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            let c = &p.clips[&clip];
            assert_eq!((c.length, c.fade_in.length, c.fade_out.length), (3 * BEAT / 2, BEAT, BEAT / 2), "the fade at the moved edge gives way");
            assert_eq!(c.gain_at(3 * BEAT / 2), 0.0, "the cut tail fades to silence");
            assert_eq!(c.gain_at(0), 0.0, "the attack still fades in");
        });
        // Never shorter than the minimum, and fades shrink with the clip.
        let cmd = resize_end(&f.p, clip, 0).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!(p.clips[&clip].length, MIN_CLIP_LEN);
            assert_eq!(p.clips[&clip].fade_in.length + p.clips[&clip].fade_out.length, MIN_CLIP_LEN);
        });
        // At the end of the file already: nothing to do.
        edit(&mut f, |c| c.length = 6 * BEAT);
        assert_eq!(resize_end(&f.p, clip, 20 * BEAT), None);
        assert_eq!(resize_end(&f.p, ClipId(9999), BEAT), None);
        // Transposed up an octave the file plays twice as fast: half the room.
        let src = f.p.clips[&clip].source;
        if let Some(Source::Audio(a)) = f.p.sources.get_mut(&src) {
            a.warp.transpose = 12.0;
        }
        edit(&mut f, |c| c.length = BEAT);
        assert_eq!(file_room(&f.p, clip), Some(3 * BEAT));
        let cmd = resize_end(&f.p, clip, 20 * BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| assert_eq!(p.clips[&clip].length, 3 * BEAT));
    }

    #[test]
    fn a_looping_clip_repeats_its_loop_when_made_longer() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let clip = f.clip;
        let own = LoopRegion { start: 2 * BEAT, end: 4 * BEAT };
        edit(&mut f, |c| {
            c.looping = Some(own);
            c.fade_out.length = BEAT;
        });
        let cmd = resize_end(&f.p, clip, 30 * BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| assert_eq!((p.clips[&clip].length, p.clips[&clip].looping), (30 * BEAT, Some(own))));
        let cmd = resize_end(&f.p, clip, BEAT).unwrap();
        round_trip(&mut f.p, vec![cmd], |p| {
            assert_eq!(p.clips[&clip].looping, Some(own));
            assert_eq!(p.clips[&clip].fade_out.length, BEAT, "the fade fits the shorter clip");
        });
    }

    #[test]
    fn stretching_resamples_the_clip_to_its_new_length() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let clip = f.clip;
        edit(&mut f, |c| {
            c.offset = BEAT;
            c.fade_in.length = BEAT / 2;
            c.fade_out.length = BEAT;
            c.envelopes.push(envelope(&[(0, 1.0), (2 * BEAT, 0.5)]));
        });
        let origin = f.p.clips[&clip].clone();
        let warp = Warp { transpose: 3.0, ..Warp::default() };
        // Twice as long: the file runs at an original tempo of 240 at 120.
        let (c, w) = stretched(&origin, &warp, 120.0, 8 * BEAT).unwrap();
        assert_eq!((c.offset, c.length, c.fade_in.length, c.fade_out.length), (2 * BEAT, 8 * BEAT, BEAT, 2 * BEAT));
        assert_eq!(c.envelopes[0].points.iter().map(|pt| pt.tick).collect::<Vec<_>>(), vec![0, 4 * BEAT]);
        assert!(w.enabled && w.mode == WarpMode::Repitch);
        assert_eq!((w.original_bpm, w.transpose), (Some(240.0), 3.0), "transpose stays on top");
        assert_eq!(stretch_factor(&w, 120.0), 2.0);
        // It still plays the same audio: offset and end in seconds of the file are unchanged.
        let file = |c: &Clip, w: &Warp| {
            let mut a = match &f.p.sources[&c.source] { Source::Audio(a) => a.clone(), _ => unreachable!() };
            a.warp = w.clone();
            let len = a.length_ticks(120.0) as f64;
            (c.offset as f64 / len, (c.offset as f64 + c.length as f64) / len)
        };
        assert_eq!(file(&c, &w), file(&origin, &Warp::default()));
        // Stretched again from a stretched one: the factors multiply.
        let (_, w2) = stretched(&c, &w, 120.0, 4 * BEAT).unwrap();
        assert_eq!(w2.original_bpm, Some(120.0));
        assert_eq!(stretched(&origin, &warp, 120.0, 0), None);
        assert_eq!(stretch_factor(&Warp::default(), 120.0), 1.0);
    }

    #[test]
    fn a_stretch_copies_a_shared_file_and_undoes_in_one_go() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let (clip, track, placement) = (f.clip, f.track, f.placement);
        let src = f.p.clips[&clip].source;
        // A second clip on the same file.
        let twin = f.p.alloc_clip_id();
        let twin_clip = Clip::new("twin", Color(0), src, 4 * BEAT);
        f.p.clips.insert(twin, twin_clip.clone());
        let origin = f.p.clips[&clip].clone();
        let cmds = stretch(&mut f.p, clip, &origin, &Warp::default(), 2 * BEAT, None).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let c = &p.clips[&clip];
            assert_ne!(c.source, src, "the stretched clip got its own copy of the file");
            assert_eq!(c.length, 2 * BEAT);
            assert!(matches!(&p.sources[&c.source], Source::Audio(a) if a.warp.original_bpm == Some(60.0)));
            assert!(matches!(&p.sources[&src], Source::Audio(a) if a.warp == Warp::default()), "the other clip is untouched");
            assert_eq!(p.clips[&twin], twin_clip);
        });
        // Alone on its file, from the left edge: the warp is set in place and the clip keeps its end.
        f.p.clips.remove(&twin);
        let cmds = stretch(&mut f.p, clip, &origin, &Warp::default(), 6 * BEAT, Some(&[(track, placement, 4 * BEAT)])).unwrap();
        assert!(cmds.iter().any(|c| matches!(c, Command::SetWarp { .. })));
        round_trip(&mut f.p, cmds, |p| {
            assert_eq!(p.clips[&clip].source, src);
            assert_eq!(p.track(track).unwrap().arrangement[0].start, 2 * BEAT, "the end stayed at beat 8");
        });
        // Back at the original length nothing changes.
        assert_eq!(stretch(&mut f.p, clip, &origin, &Warp::default(), 4 * BEAT, None), None);
    }

    #[test]
    fn edges_of_a_transposed_clip_keep_the_audio_in_place() {
        let mut f = fixture(Some(8), 2 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let src = f.p.clips[&clip].source;
        if let Some(Source::Audio(a)) = f.p.sources.get_mut(&src) {
            a.warp.transpose = 12.0;
        }
        edit(&mut f, |c| c.offset = 2 * BEAT);
        // One beat of clip time is two beats of the file an octave up.
        let cmds = trim_start(&f.p, track, placement, 5 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| assert_eq!((p.clips[&clip].offset, p.clips[&clip].length), (4 * BEAT, BEAT)));
        // Revealed to the start of the file: one beat of clip time back.
        let cmds = trim_start(&f.p, track, placement, 0).unwrap();
        round_trip(&mut f.p, cmds, |p| assert_eq!((p.clips[&clip].offset, p.clips[&clip].length), (0, 3 * BEAT)));
        // A split halfway: the right half starts two beats into the file later.
        let split = split_placement(&mut f.p.clone(), track, placement, 5 * BEAT).unwrap();
        let right = split.commands.iter().find_map(|c| match c { Command::AddClip { clip, .. } => Some(clip.offset), _ => None });
        assert_eq!(right, Some(4 * BEAT));
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

    /// `(start, key, length)` of the notes a clip's source holds, in order.
    fn notes_of(p: &Project, clip: ClipId) -> Vec<(Tick, u8, Tick)> {
        let Some(Source::Notes(src)) = p.sources.get(&p.clips[&clip].source) else { panic!("a notes clip") };
        let mut v: Vec<_> = src.notes.iter().map(|n| (n.start, n.key, n.length)).collect();
        v.sort();
        v
    }

    fn right_half(p: &Project, track: TrackId, placement: PlacementId) -> ClipId {
        p.track(track).unwrap().arrangement.iter().find(|pl| pl.id != placement).unwrap().clip
    }

    #[test]
    fn split_notes_gives_each_half_only_its_own_notes() {
        let mut f = fixture(None, 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let original = f.p.clips[&clip].source;
        // The second note crosses the cut at beat 2 of the clip.
        let Some(Source::Notes(src)) = f.p.sources.get_mut(&original) else { unreachable!() };
        src.notes[1].length = BEAT + BEAT / 2;
        let cmds = split_at(&mut f.p, track, placement, 6 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let right = right_half(p, track, placement);
            let (l, r) = (&p.clips[&clip], &p.clips[&right]);
            assert_eq!((l.offset, l.length, r.offset, r.length), (0, 2 * BEAT, 0, 2 * BEAT), "both halves start at their own tick 0");
            assert!(l.source != r.source && l.source != original);
            // The crossing note ends at the cut on the left and is not on the right.
            assert_eq!(notes_of(p, clip), [(0, 60, BEAT / 2), (BEAT, 61, BEAT)]);
            assert_eq!(notes_of(p, right), [(0, 62, BEAT / 2), (BEAT, 63, BEAT / 2)]);
            assert!(!p.sources.contains_key(&original), "nothing plays the old notes any more");
            let ids: BTreeSet<NoteId> = [l.source, r.source].iter().flat_map(|s| match &p.sources[s] { Source::Notes(n) => n.notes.iter().map(|n| n.id).collect::<Vec<_>>(), _ => vec![] }).collect();
            assert_eq!(ids.len(), 4, "every note has an id of its own");
        });
    }

    #[test]
    fn editing_one_half_leaves_the_other_alone() {
        let mut f = fixture(None, 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let cmds = split_at(&mut f.p, track, placement, 6 * BEAT).unwrap();
        Command::Batch { commands: cmds }.apply(&mut f.p).unwrap();
        let right = right_half(&f.p, track, placement);
        let before = notes_of(&f.p, right);
        let source = f.p.clips[&clip].source;
        let Source::Notes(src) = &f.p.sources[&source] else { unreachable!() };
        let moved: Vec<Note> = src.notes.iter().map(|n| Note { key: n.key + 12, ..n.clone() }).collect();
        Command::UpdateNotes { source, notes: moved }.apply(&mut f.p).unwrap();
        assert_eq!(notes_of(&f.p, clip), [(0, 72, BEAT / 2), (BEAT, 73, BEAT / 2)]);
        assert_eq!(notes_of(&f.p, right), before);
    }

    #[test]
    fn split_of_a_trimmed_notes_clip_rebases_both_halves() {
        let mut f = fixture(None, 3 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        edit(&mut f, |c| c.offset = BEAT);
        let cmds = split_at(&mut f.p, track, placement, 5 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let right = right_half(p, track, placement);
            assert_eq!(p.clips[&clip].offset, 0);
            assert_eq!(notes_of(p, clip), [(0, 61, BEAT / 2)]);
            assert_eq!(notes_of(p, right), [(0, 62, BEAT / 2), (BEAT, 63, BEAT / 2)]);
        });
    }

    #[test]
    fn split_of_a_looping_notes_clip_keeps_the_loop() {
        let mut f = fixture(None, 8 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        // A two-beat loop over beats 2..4 of the notes, played from beat 1.
        edit(&mut f, |c| {
            c.offset = BEAT;
            c.looping = Some(LoopRegion { start: 2 * BEAT, end: 4 * BEAT });
        });
        let heard = |p: &Project, id: ClipId| {
            let Source::Notes(src) = &p.sources[&p.clips[&id].source] else { unreachable!() };
            heard_notes(&p.clips[&id], &src.notes).iter().map(|n| (n.start, n.key, n.length)).collect::<Vec<_>>()
        };
        let whole = heard(&f.p, clip);
        let cmds = split_at(&mut f.p, track, placement, 8 * BEAT).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let right = right_half(p, track, placement);
            let (l, r) = (&p.clips[&clip], &p.clips[&right]);
            assert_eq!((l.offset, l.looping), (0, Some(LoopRegion { start: BEAT, end: 3 * BEAT })));
            assert_eq!((r.offset, r.looping), (BEAT, Some(LoopRegion { start: 0, end: 2 * BEAT })));
            assert_eq!(notes_of(p, clip), [(0, 61, BEAT / 2), (BEAT, 62, BEAT / 2), (2 * BEAT, 63, BEAT / 2)]);
            assert_eq!(notes_of(p, right), [(0, 62, BEAT / 2), (BEAT, 63, BEAT / 2)]);
            // Together the halves still play what the clip played.
            let mut both = heard(p, clip);
            both.extend(heard(p, right).into_iter().map(|(t, k, len)| (t + 4 * BEAT, k, len)));
            assert_eq!(both, whole);
        });
    }

    #[test]
    fn splitting_one_placement_of_a_shared_clip_leaves_the_other_alone() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        edit(&mut f, |c| {
            c.offset = BEAT;
            c.fade_out = Fade { length: BEAT, curve: 0.2 };
        });
        // The same clip again, later in the song.
        let twin = f.p.alloc_placement_id();
        f.p.track_mut(track).unwrap().arrangement.push(Placement { id: twin, clip, start: 16 * BEAT, lane: 0 });
        let whole = f.p.clips[&clip].clone();
        let split = split_placement(&mut f.p, track, placement, 6 * BEAT).unwrap();
        let (left_id, right_id, right_place) = (split.left, split.right, split.right_placement);
        assert!(left_id != clip && right_id != clip && left_id != right_id, "both halves are new clips");
        round_trip(&mut f.p, split.commands, |p| {
            let find = |id: PlacementId| p.track(track).unwrap().arrangement.iter().find(|pl| pl.id == id).unwrap().clone();
            // The other placement plays what it played: same clip, length and offset.
            assert_eq!((find(twin).clip, find(twin).start), (clip, 16 * BEAT));
            assert_eq!(p.clips[&clip], whole);
            // The placement that was cut keeps its id and place, on a clip of its own.
            assert_eq!((find(placement).clip, find(placement).start), (left_id, 4 * BEAT));
            assert_eq!((find(right_place).clip, find(right_place).start), (right_id, 6 * BEAT));
            let (left, right) = (&p.clips[&left_id], &p.clips[&right_id]);
            assert_eq!((left.offset, left.length, left.fade_out), (BEAT, 2 * BEAT, Fade::default()));
            assert_eq!((right.offset, right.length, right.fade_out), (3 * BEAT, 2 * BEAT, whole.fade_out));
            assert_eq!(p.sources.len(), 1, "all of them play the same file");
        });
        // A clip that also sits in a session slot is left alone the same way.
        let mut f = fixture(None, 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let scene = f.p.alloc_scene_id();
        f.p.track_mut(track).unwrap().session_slots.insert(scene, clip);
        let whole = f.p.clips[&clip].clone();
        let split = split_placement(&mut f.p, track, placement, 5 * BEAT).unwrap();
        let (left_id, right_id) = (split.left, split.right);
        assert_ne!(left_id, clip);
        round_trip(&mut f.p, split.commands, |p| {
            assert_eq!((p.clips[&clip].clone(), p.track(track).unwrap().session_slots[&scene]), (whole.clone(), clip));
            assert_eq!((p.clips[&left_id].length, p.clips[&right_id].offset), (BEAT, 0));
            assert_eq!(notes_of(p, right_id).len(), 3, "the right half has the three notes after the cut");
            // Each half has notes of its own; the slot's clip keeps the original ones.
            let sources: std::collections::BTreeSet<SourceId> = [clip, left_id, right_id].iter().map(|id| p.clips[id].source).collect();
            assert_eq!((sources.len(), p.sources.len()), (3, 3));
        });
    }

    #[test]
    fn splitting_the_only_placement_keeps_the_clip_id() {
        let mut f = fixture(Some(8), 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let clips = f.p.clips.len();
        let split = split_placement(&mut f.p, track, placement, 5 * BEAT).unwrap();
        assert_eq!(split.left, clip);
        assert!(matches!(&split.commands[..], [Command::UpdateClip { id, .. }, Command::AddClip { .. }, Command::PlaceClip { .. }] if *id == clip));
        let again = fixture(Some(8), 4 * BEAT);
        let mut p = again.p;
        assert_eq!(split_at(&mut p, track, placement, 5 * BEAT), Some(split.commands.clone()), "`split_at` is the same cut");
        round_trip(&mut f.p, split.commands, |p| {
            assert_eq!(p.clips.len(), clips + 1);
            assert_eq!((p.clips[&clip].length, p.clips[&split.right].length), (BEAT, 3 * BEAT));
            assert!(p.track(track).unwrap().arrangement.iter().any(|pl| pl.id == split.right_placement && pl.clip == split.right));
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

    #[test]
    fn consolidating_notes_joins_them_with_the_gaps() {
        let mut f = fixture(None, 2 * BEAT);
        let (track, first) = (f.track, f.placement);
        // A second clip two beats after the first ends, trimmed to start at its second note.
        let (cmds, second) = duplicate_placement(&mut f.p, track, first, 8 * BEAT).unwrap();
        Command::Batch { commands: cmds }.apply(&mut f.p).unwrap();
        let second_clip = f.p.track(track).unwrap().arrangement.iter().find(|pl| pl.id == second).unwrap().clip;
        f.p.clips.get_mut(&second_clip).unwrap().offset = BEAT;
        // One more clip on the same track that is not selected.
        let (cmds, other) = duplicate_placement(&mut f.p, track, first, 20 * BEAT).unwrap();
        Command::Batch { commands: cmds }.apply(&mut f.p).unwrap();
        let (clips, sources) = (f.p.clips.len(), f.p.sources.len());
        let done = consolidate_notes(&mut f.p, track, &[second, first], 4 * BEAT, 10 * BEAT).unwrap();
        assert!(Command::Batch { commands: done.commands.clone() }.is_destructive());
        round_trip(&mut f.p, done.commands, |p| {
            let arrangement = &p.track(track).unwrap().arrangement;
            assert_eq!(arrangement.iter().map(|pl| (pl.id, pl.start)).collect::<Vec<_>>(), [(done.placement, 4 * BEAT), (other, 20 * BEAT)]);
            assert_eq!(p.clips[&done.clip].length, 6 * BEAT);
            assert_eq!((p.clips[&done.clip].offset, p.clips[&done.clip].looping), (0, None));
            // First clip: notes at 0 and 1; second (from its beat 1): notes at 4 + 0 and 4 + 1.
            assert_eq!(notes_of(p, done.clip), [(0, 60, BEAT / 2), (BEAT, 61, BEAT / 2), (4 * BEAT, 61, BEAT / 2), (5 * BEAT, 62, BEAT / 2)]);
            assert_eq!((p.clips.len(), p.sources.len()), (clips - 1, sources - 1), "the two joined clips and their notes went");
        });
        assert!(consolidate_notes(&mut f.p, track, &[first], 4 * BEAT, 4 * BEAT).is_none(), "empty range");
        assert!(consolidate_notes(&mut f.p, track, &[PlacementId(9999)], 0, BEAT).is_none());
    }

    #[test]
    fn consolidating_keeps_a_clip_that_is_used_elsewhere() {
        let mut f = fixture(None, 2 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let scene = f.p.alloc_scene_id();
        f.p.track_mut(track).unwrap().session_slots.insert(scene, clip);
        let done = consolidate_notes(&mut f.p, track, &[placement], 4 * BEAT, 6 * BEAT).unwrap();
        round_trip(&mut f.p, done.commands, |p| {
            assert!(p.clips.contains_key(&clip), "the slot still plays it");
            assert_eq!(p.sources.len(), 2);
        });
    }

    #[test]
    fn consolidating_audio_puts_the_rendered_file_in_their_place() {
        let mut f = fixture(Some(8), 2 * BEAT);
        let (track, placement) = (f.track, f.placement);
        let (cmds, second) = duplicate_placement(&mut f.p, track, placement, 8 * BEAT).unwrap();
        Command::Batch { commands: cmds }.apply(&mut f.p).unwrap();
        let file = AudioSource { path: "joined.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames: 6 * 24_000, warp: Warp::default(), key: None };
        assert!(consolidate_notes(&mut f.p, track, &[placement, second], 4 * BEAT, 10 * BEAT).is_none(), "audio is not notes");
        let done = consolidate_audio(&mut f.p, track, &[placement, second], 4 * BEAT, 10 * BEAT, file.clone()).unwrap();
        round_trip(&mut f.p, done.commands, |p| {
            let arrangement = &p.track(track).unwrap().arrangement;
            assert_eq!(arrangement.iter().map(|pl| (pl.id, pl.start)).collect::<Vec<_>>(), [(done.placement, 4 * BEAT)]);
            let c = &p.clips[&done.clip];
            assert_eq!((c.offset, c.length, c.gain_db, c.fade_in, c.envelopes.len()), (0, 6 * BEAT, 0.0, Fade::default(), 0));
            assert_eq!(p.sources[&c.source], Source::Audio(file.clone()));
            assert_eq!((p.clips.len(), p.sources.len()), (1, 1), "the old file is no longer used by the project");
        });
        let mut notes = fixture(None, 2 * BEAT);
        let (t, pl) = (notes.track, notes.placement);
        assert!(consolidate_audio(&mut notes.p, t, &[pl], 4 * BEAT, 6 * BEAT, file).is_none(), "notes are not audio");
    }

    #[test]
    fn a_left_trim_of_notes_settles_on_offset_zero() {
        let mut f = fixture(None, 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let original = f.p.clips[&clip].source;
        // The second note (beat 1) crosses the new start at beat 1 + 1/4.
        let Some(Source::Notes(src)) = f.p.sources.get_mut(&original) else { unreachable!() };
        src.notes[1].length = BEAT;
        // Past the clip's end, revealed by a longer clip later.
        src.notes.push(Note::new(NoteId(9000), 70, 6 * BEAT, BEAT, 0.8));
        let cmds = trim_start(&f.p, track, placement, 5 * BEAT + BEAT / 4).unwrap();
        Command::Batch { commands: cmds }.apply(&mut f.p).unwrap();
        assert_eq!(f.p.clips[&clip].offset, BEAT + BEAT / 4);
        let cmds = rebase_notes(&mut f.p, clip).unwrap();
        round_trip(&mut f.p, cmds, |p| {
            let c = &p.clips[&clip];
            assert_eq!((c.offset, c.length), (0, 3 * BEAT - BEAT / 4));
            let q = BEAT / 4;
            assert_eq!(
                notes_of(p, clip),
                [(-BEAT - q, 60, BEAT / 2), (-q, 61, BEAT), (BEAT - q, 62, BEAT / 2), (2 * BEAT - q, 63, BEAT / 2), (5 * BEAT - q, 70, BEAT)],
                "earlier notes shift before 0 so dragging left back out restores them"
            );
            let Some(Source::Notes(src)) = p.sources.get(&c.source) else { unreachable!() };
            assert_eq!(heard_notes(c, &src.notes).len(), 2, "only notes inside 0..length play");
            assert!(!p.sources.contains_key(&original));
        });
        // `round_trip` leaves the project before `rebase_notes`; settle it, then trim back left.
        let settle_trimmed = rebase_notes(&mut f.p, clip).unwrap();
        Command::Batch { commands: settle_trimmed }.apply(&mut f.p).unwrap();
        let restore = trim_start(&f.p, track, placement, 4 * BEAT).unwrap();
        Command::Batch { commands: restore }.apply(&mut f.p).unwrap();
        let settle_back = rebase_notes(&mut f.p, clip).unwrap();
        Command::Batch { commands: settle_back }.apply(&mut f.p).unwrap();
        let c = &f.p.clips[&clip];
        let Some(Source::Notes(src)) = f.p.sources.get(&c.source) else { unreachable!() };
        assert_eq!(heard_notes(c, &src.notes).len(), 4, "earlier notes return when trimmed left again");
        assert_eq!(rebase_notes(&mut f.p, ClipId(9999)), None);
    }

    #[test]
    fn a_notes_clip_grows_to_the_left_into_empty_time() {
        let mut f = fixture(None, 4 * BEAT);
        let (track, clip, placement) = (f.track, f.clip, f.placement);
        let audio = fixture(Some(8), 4 * BEAT);
        assert_eq!(trim_start(&audio.p, audio.track, audio.placement, BEAT), None, "audio cannot start before its file");
        let cmds = trim_start(&f.p, track, placement, 2 * BEAT).unwrap();
        Command::Batch { commands: cmds }.apply(&mut f.p).unwrap();
        assert_eq!((f.p.clips[&clip].offset, f.p.clips[&clip].length), (-2 * BEAT, 6 * BEAT));
        let cmds = rebase_notes(&mut f.p, clip).unwrap();
        Command::Batch { commands: cmds }.apply(&mut f.p).unwrap();
        assert_eq!(f.p.clips[&clip].offset, 0);
        assert_eq!(notes_of(&f.p, clip).iter().map(|n| n.0).collect::<Vec<_>>(), [2 * BEAT, 3 * BEAT, 4 * BEAT, 5 * BEAT], "the notes stay where they were on the timeline");
    }

    #[test]
    fn rebase_leaves_loops_audio_and_settled_clips_alone() {
        let mut f = fixture(None, 4 * BEAT);
        assert_eq!(rebase_notes(&mut f.p, f.clip), None, "offset 0 already");
        edit(&mut f, |c| {
            c.offset = BEAT;
            c.looping = Some(LoopRegion { start: 0, end: 2 * BEAT });
        });
        assert_eq!(rebase_notes(&mut f.p, f.clip), None);
        let mut a = fixture(Some(8), 4 * BEAT);
        edit(&mut a, |c| c.offset = BEAT);
        assert_eq!(rebase_notes(&mut a.p, a.clip), None);
        // A whole old project: only the clip that needs it.
        let mut old = fixture(None, 4 * BEAT);
        edit(&mut old, |c| c.offset = BEAT);
        let cmds = rebase_all_notes(&mut old.p);
        Command::Batch { commands: cmds }.apply(&mut old.p).unwrap();
        assert_eq!(notes_of(&old.p, old.clip).len(), 4);
        let c = &old.p.clips[&old.clip];
        let Some(Source::Notes(src)) = old.p.sources.get(&c.source) else { unreachable!() };
        assert_eq!(heard_notes(c, &src.notes).len(), 3);
        assert!(rebase_all_notes(&mut old.p).is_empty());
    }
}
