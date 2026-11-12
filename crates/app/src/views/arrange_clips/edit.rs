//! What the lane gestures do to the project, as commands; no window needed.
//!
//! A fade or trim drag starts by taking an [`Origin`]: the clip and its
//! placement as they were at mouse-down. Every mouse move asks it for the
//! commands that bring the project to "the clip as it was, with this
//! [`Edit`] applied", so nothing accumulates along the way: dragging a fade
//! in and back out, or an edge over a fade and back, leaves the clip exactly
//! as it was. The rules are those of `aitrack_model::ops`; the origin keeps
//! a small private project with only this clip in it for the builders to
//! read. (The sample editor's `Snapshot` does the same for its own markers;
//! it stops the start marker at the loop's ends, where a lane edge wraps
//! around the loop as the model does.)
//!
//! A copy-drag makes its [`Copies`] once, on top of their originals, and
//! then moves them like any group of clips. All of it coalesces into one
//! undo step.

use crate::state::AppState;
use aitrack_model::*;

/// What a drag asks for, relative to the clip as it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edit {
    /// Length of the fade in, in ticks.
    FadeIn(Tick),
    /// Length of the fade out.
    FadeOut(Tick),
    /// Curvature of a fade, -1..=1.
    FadeInCurve(f32),
    FadeOutCurve(f32),
    /// Where the placement starts on the timeline: trims or reveals the
    /// start of the clip, whose content stays put.
    Start(Tick),
}

impl Edit {
    pub fn label(&self) -> &'static str {
        match self {
            Self::FadeIn(_) | Self::FadeInCurve(_) => "Fade in",
            Self::FadeOut(_) | Self::FadeOutCurve(_) => "Fade out",
            Self::Start(_) => "Trim clip start",
        }
    }
}

/// A placed clip as it was when a drag began.
#[derive(Clone, Debug)]
pub struct Origin {
    pub id: ClipId,
    pub clip: Clip,
    pub track: TrackId,
    pub placement: PlacementId,
    pub start: Tick,
    lane: u16,
    /// The clip's other placements (track, placement, start, lane): a trim
    /// moves them too, so its content stays put wherever the clip is used.
    others: Vec<(TrackId, PlacementId, Tick, u16)>,
    /// Only this clip, its file and its placement, for the `ops` builders.
    scratch: Project,
}

impl Origin {
    pub fn take(s: &AppState, placement: PlacementId) -> Option<Self> {
        let (track, place, _) = s.placement(placement)?;
        let clip = s.project.clips.get(&place.clip)?.clone();
        let mut scratch = Project::new("");
        scratch.tempo = s.project.tempo.clone();
        // The builders only ask a file for its length, and whether the clip
        // holds notes (an empty stand-in: the notes settle at mouse-up).
        match s.project.sources.get(&clip.source) {
            Some(Source::Audio(file)) => {
                scratch.sources.insert(clip.source, Source::Audio(file.clone()));
            }
            Some(Source::Notes(_)) => {
                scratch.sources.insert(clip.source, Source::Notes(NoteSource::default()));
            }
            None => {}
        }
        scratch.clips.insert(place.clip, clip.clone());
        let mut lane = Track::new(track, TrackKind::Audio, "", Color(0));
        lane.arrangement.push(place.clone());
        scratch.tracks = vec![lane];
        let others = other_placements(&s.project, place.clip, placement);
        Some(Self { id: place.clip, clip, track, placement, start: place.start, lane: place.lane, others, scratch })
    }

    /// Where the clip ended on the timeline.
    pub fn end(&self) -> Tick {
        self.start + self.clip.length
    }

    /// The commands that take `now` to the origin with `edit` applied; none
    /// when it is there already.
    pub fn commands(&self, now: &Project, edit: Edit) -> Vec<Command> {
        let mut cmds = self.wanted(edit).unwrap_or_default();
        // What the edit leaves alone goes back to how it was.
        if !cmds.iter().any(|c| matches!(c, Command::UpdateClip { .. })) {
            cmds.push(Command::UpdateClip { id: self.id, clip: Box::new(self.clip.clone()) });
        }
        if !cmds.iter().any(|c| matches!(c, Command::MovePlacement { .. })) {
            cmds.push(Command::MovePlacement { track: self.track, id: self.placement, to_track: self.track, start: self.start, lane: self.lane });
        }
        move_others(&mut cmds, self.start, &self.others);
        cmds.retain(|c| match c {
            Command::UpdateClip { id, clip } => now.clips.get(id) != Some(&**clip),
            Command::MovePlacement { track, id, start, .. } => {
                now.track(*track).and_then(|t| t.arrangement.iter().find(|pl| pl.id == *id)).is_none_or(|pl| pl.start != *start)
            }
            _ => true,
        });
        cmds
    }

    /// `edit` applied to the origin; `None` when that changes nothing.
    fn wanted(&self, edit: Edit) -> Option<Vec<Command>> {
        let (p, id, c) = (&self.scratch, self.id, &self.clip);
        let one = |cmd: Option<Command>| cmd.map(|cmd| vec![cmd]);
        match edit {
            Edit::FadeIn(length) => one(ops::set_fades(p, id, Fade { length: length.max(0), ..c.fade_in }, c.fade_out)),
            Edit::FadeOut(length) => one(ops::set_fades(p, id, c.fade_in, Fade { length: length.max(0), ..c.fade_out })),
            Edit::FadeInCurve(curve) => one(ops::set_fades(p, id, Fade { curve, ..c.fade_in }, c.fade_out)),
            Edit::FadeOutCurve(curve) => one(ops::set_fades(p, id, c.fade_in, Fade { curve, ..c.fade_out })),
            Edit::Start(start) => ops::trim_start(p, self.track, self.placement, start),
        }
    }
}

/// The placements of `clip` other than `edited`: (track, placement, start, lane).
pub fn other_placements(p: &Project, clip: ClipId, edited: PlacementId) -> Vec<(TrackId, PlacementId, Tick, u16)> {
    p.tracks.iter().flat_map(|t| t.arrangement.iter().filter(|pl| pl.clip == clip && pl.id != edited).map(|pl| (t.id, pl.id, pl.start, pl.lane))).collect()
}

/// A trim moves the placement being edited (`cmds` has its move, from
/// `home`); the clip's other placements start as much later or earlier, so
/// its content stays put wherever the clip is used. The lanes and the sample
/// editor both trim through here.
pub fn move_others(cmds: &mut Vec<Command>, home: Tick, others: &[(TrackId, PlacementId, Tick, u16)]) {
    let moved = cmds.iter().find_map(|c| if let Command::MovePlacement { start, .. } = c { Some(*start - home) } else { None }).unwrap_or(0);
    for (track, id, start, lane) in others {
        cmds.push(Command::MovePlacement { track: *track, id: *id, to_track: *track, start: (start + moved).max(0), lane: *lane });
    }
}

/// One move of a fade or trim drag: coalesced with the drag's other moves
/// into a single undo step. Returns whether the project changed. The caller
/// seals the history when the drag starts and when it ends.
pub fn apply(s: &mut AppState, origin: &Origin, edit: Edit) -> bool {
    let cmds = origin.commands(&s.project, edit);
    !cmds.is_empty() && s.edit_coalesced(&format!("clip-shape-{}", origin.placement.0), edit.label(), cmds)
}

/// Mouse-up of a left-edge trim: a notes clip settles on offset 0 with its
/// notes moved to match (see [`ops::rebase_notes`]), in the drag's undo step,
/// so the piano roll shows and edits just what the clip plays. Returns
/// whether anything changed.
pub fn finish_trim(s: &mut AppState, origin: &Origin) -> bool {
    let Some(cmds) = ops::rebase_notes(&mut s.project, origin.id) else { return false };
    s.edit_coalesced(&format!("clip-shape-{}", origin.placement.0), Edit::Start(0).label(), cmds)
}

/// Double-click on a fade handle: that fade is gone (`curve_only` keeps its
/// length and only straightens it). One undo step.
pub fn reset_fade(s: &mut AppState, clip: ClipId, fade_in: bool, curve_only: bool) -> bool {
    let Some(c) = s.project.clips.get(&clip) else { return false };
    let reset = |f: Fade| if curve_only { Fade { curve: 0., ..f } } else { Fade::default() };
    let (new_in, new_out) = if fade_in { (reset(c.fade_in), c.fade_out) } else { (c.fade_in, reset(c.fade_out)) };
    let Some(cmd) = ops::set_fades(&s.project, clip, new_in, new_out) else { return false };
    s.seal();
    let ok = s.edit(if fade_in { "Reset fade in" } else { "Reset fade out" }, vec![cmd]);
    s.seal();
    ok
}

/// The copies of a copy-drag.
#[derive(Clone, Debug)]
pub struct Copies {
    /// Each copy with the track and start of its original: where
    /// `AppState::drag_clips` moves it from.
    pub origin: Vec<(PlacementId, TrackId, Tick)>,
    /// What was selected before, to go back to when the drag is given up.
    selection: Vec<PlacementId>,
    last: Option<PlacementId>,
}

impl Copies {
    /// Copy the clip `under` the pointer, or the whole clip selection when
    /// it is part of it. The copies start on top of their originals and are
    /// selected. Audio copies share their file; notes copies get their own
    /// notes.
    pub fn make(s: &mut AppState, under: PlacementId) -> Option<Self> {
        let selection: Vec<PlacementId> = s.selected_clip_ids().into_iter().collect();
        let last = s.selected_placement.map(|(_, id)| id);
        let originals: Vec<(PlacementId, TrackId, Tick)> = if selection.contains(&under) {
            s.clip_selection().into_iter().map(|(track, pl, _)| (pl.id, track, pl.start)).collect()
        } else {
            let (track, pl, _) = s.placement(under)?;
            vec![(pl.id, track, pl.start)]
        };
        let mut cmds = Vec::new();
        let mut origin = Vec::with_capacity(originals.len());
        let mut grabbed = None;
        for (id, track, start) in &originals {
            let (mut copy, new) = ops::duplicate_placement(&mut s.project, *track, *id, *start)?;
            cmds.append(&mut copy);
            origin.push((new, *track, *start));
            if *id == under {
                grabbed = Some(new);
            }
        }
        // The same key as a clip move, so the moves that follow join this step.
        s.seal();
        if !s.edit_coalesced("clip-move", if origin.len() == 1 { "Copy clip" } else { "Copy clips" }, cmds) {
            return None;
        }
        s.select_clips(origin.iter().map(|o| o.0), grabbed);
        Some(Self { origin, selection, last })
    }

    /// Put the copies `dt` ticks and `rows` lanes from their originals.
    pub fn move_to(&self, s: &mut AppState, dt: Tick, rows: i32) -> bool {
        s.drag_clips(&self.origin, dt, rows)
    }

    /// Whether every copy still lies on top of its original.
    pub fn at_home(&self, s: &AppState) -> bool {
        self.origin.iter().all(|(id, track, start)| s.placement(*id).is_some_and(|(t, pl, _)| t == *track && pl.start == *start))
    }

    /// Mouse-up: the copies stay where they are and are the selection.
    /// Released where the drag began, nothing is created. Returns whether
    /// the copies were kept.
    pub fn finish(self, s: &mut AppState) -> bool {
        if self.at_home(s) {
            self.cancel(s);
            return false;
        }
        s.seal();
        true
    }

    /// Escape: take the copies away again and select what was selected.
    pub fn cancel(self, s: &mut AppState) {
        s.seal();
        s.undo();
        s.select_clips(self.selection, self.last);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEAT: Tick = PPQ;

    struct Fixture {
        s: AppState,
        clip: ClipId,
        track: TrackId,
        placement: PlacementId,
    }

    /// The demo project plus an audio clip of 4 beats over an 8-beat file
    /// (the demo runs at 128 BPM), placed at beat 16 on a track of its own.
    fn fixture() -> Fixture {
        let mut s = AppState::new(crate::demo::project());
        let bpm = s.project.tempo.initial_bpm();
        let frames = (8. * 60. / bpm * 48_000.).round() as u64;
        let (source, clip, track, placement) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_track_id(), s.project.alloc_placement_id());
        let audio = AudioSource { path: "loop.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames, warp: Warp::default(), key: None };
        let index = s.project.tracks.len() - 1;
        assert!(s.edit("Add loop", vec![
            Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Audio, "Loop", Color::palette(3))), index },
            Command::AddSource { id: source, source: Box::new(Source::Audio(audio)) },
            Command::AddClip { id: clip, clip: Box::new(Clip::new("Loop", Color::palette(3), source, 4 * BEAT)) },
            Command::PlaceClip { track, placement: Placement { id: placement, clip, start: 16 * BEAT, lane: 0 } },
        ]));
        s.seal();
        s.click_clip(placement, false);
        assert_eq!(ops::source_len(&s.project, clip), Some(8 * BEAT));
        Fixture { s, clip, track, placement }
    }

    impl Fixture {
        fn clip(&self) -> &Clip {
            &self.s.project.clips[&self.clip]
        }

        fn start(&self) -> Tick {
            self.s.placement(self.placement).unwrap().1.start
        }

        fn steps(&self) -> usize {
            self.s.history.steps().count()
        }

        /// A whole drag: mouse-down, the moves, mouse-up. Returns the undo steps it made.
        fn drag(&mut self, moves: &[Edit]) -> usize {
            let before = self.steps();
            self.s.seal();
            let origin = Origin::take(&self.s, self.placement).unwrap();
            for edit in moves {
                apply(&mut self.s, &origin, *edit);
            }
            self.s.seal();
            self.steps() - before
        }

        /// The placements of the fixture's track as (start, clip), in time order.
        fn lane(&self) -> Vec<(Tick, ClipId)> {
            let mut out: Vec<_> = self.s.project.track(self.track).unwrap().arrangement.iter().map(|pl| (pl.start, pl.clip)).collect();
            out.sort();
            out
        }
    }

    #[test]
    fn a_fade_drag_is_one_undo_step() {
        let mut f = fixture();
        let before = f.s.project.clone();
        assert_eq!(f.drag(&[Edit::FadeIn(100), Edit::FadeIn(BEAT / 2), Edit::FadeIn(BEAT)]), 1);
        assert_eq!(f.clip().fade_in, Fade { length: BEAT, curve: 0. });
        assert_eq!(f.s.history.undo_label(), Some("Fade in"));
        // The slope, then the other fade: a step each.
        assert_eq!(f.drag(&[Edit::FadeInCurve(0.3), Edit::FadeInCurve(-0.6), Edit::FadeInCurve(-3.)]), 1);
        assert_eq!(f.clip().fade_in, Fade { length: BEAT, curve: -1. }, "the curve stays inside -1..=1");
        assert_eq!(f.drag(&[Edit::FadeOut(BEAT), Edit::FadeOut(2 * BEAT)]), 1);
        assert_eq!(f.drag(&[Edit::FadeOutCurve(0.2), Edit::FadeOutCurve(0.5)]), 1);
        assert_eq!(f.clip().fade_out, Fade { length: 2 * BEAT, curve: 0.5 });
        // A fade never goes below nothing, and stops at the other fade.
        f.drag(&[Edit::FadeOut(-300)]);
        assert_eq!(f.clip().fade_out.length, 0);
        f.drag(&[Edit::FadeOut(40 * BEAT)]);
        assert_eq!(f.clip().fade_out.length, 3 * BEAT);
        // Everything undoes back to the clip as it was.
        while f.s.project != before {
            assert!(f.s.undo());
        }
    }

    #[test]
    fn dragging_a_fade_in_and_back_out_leaves_the_clip_unchanged() {
        let mut f = fixture();
        f.drag(&[Edit::FadeIn(BEAT)]);
        f.drag(&[Edit::FadeOut(BEAT)]);
        let before = f.clip().clone();
        assert_eq!((before.fade_in.length, before.fade_out.length), (BEAT, BEAT));
        // Over the other fade and back: its length is not eaten on the way.
        f.drag(&[Edit::FadeIn(2 * BEAT), Edit::FadeIn(4 * BEAT), Edit::FadeIn(BEAT)]);
        assert_eq!(*f.clip(), before);
        // Trimming past the fades and back does not shorten them either.
        f.drag(&[Edit::Start(19 * BEAT), Edit::Start(19 * BEAT + 900), Edit::Start(16 * BEAT)]);
        assert_eq!((f.clip().clone(), f.start()), (before, 16 * BEAT));
        // A drag that never left makes no undo step at all.
        assert_eq!(f.drag(&[Edit::FadeIn(BEAT), Edit::Start(16 * BEAT)]), 0);
    }

    #[test]
    fn a_trim_drag_is_one_undo_step_and_keeps_the_content_in_place() {
        let mut f = fixture();
        // Start the clip a beat into its file, so there is audio to reveal.
        assert!(f.s.edit("Offset", vec![Command::UpdateClip { id: f.clip, clip: Box::new(Clip { offset: BEAT, ..f.clip().clone() }) }]));
        let before = f.s.project.clone();
        // Trim: a beat later on the timeline, a beat further into the file.
        assert_eq!(f.drag(&[Edit::Start(16 * BEAT + 300), Edit::Start(17 * BEAT)]), 1);
        assert_eq!((f.start(), f.clip().offset, f.clip().length), (17 * BEAT, 2 * BEAT, 3 * BEAT));
        assert_eq!(f.s.history.undo_label(), Some("Trim clip start"));
        assert!(f.s.undo());
        assert_eq!(f.s.project, before, "one undo puts the clip and its placement back");
        // Reveal: only as far as the file goes.
        assert_eq!(f.drag(&[Edit::Start(15 * BEAT + 100), Edit::Start(10 * BEAT)]), 1);
        assert_eq!((f.start(), f.clip().offset, f.clip().length), (15 * BEAT, 0, 5 * BEAT));
        // The clip never gets shorter than the model's minimum.
        f.drag(&[Edit::Start(99 * BEAT)]);
        assert_eq!((f.clip().length, f.start() + f.clip().length), (ops::MIN_CLIP_LEN, 20 * BEAT), "the end stays where it was");
    }

    #[test]
    fn the_left_edge_of_a_looping_clip_wraps_around_its_loop() {
        let mut f = fixture();
        let looped = Clip { looping: Some(LoopRegion { start: 0, end: 2 * BEAT }), ..f.clip().clone() };
        assert!(f.s.edit("Loop", vec![Command::UpdateClip { id: f.clip, clip: Box::new(looped) }]));
        // Pulled three beats to the left: three more beats of the loop before it.
        assert_eq!(f.drag(&[Edit::Start(13 * BEAT)]), 1);
        assert_eq!((f.start(), f.clip().length, f.clip().offset), (13 * BEAT, 7 * BEAT, BEAT));
        // And back in.
        f.drag(&[Edit::Start(16 * BEAT)]);
        assert_eq!((f.start(), f.clip().length, f.clip().offset), (16 * BEAT, 4 * BEAT, 0));
    }

    #[test]
    fn trimming_a_clip_placed_twice_keeps_its_content_in_place_in_both() {
        let mut f = fixture();
        // The same clip again, 8 beats later.
        let twin = f.s.project.alloc_placement_id();
        assert!(f.s.edit("Place again", vec![Command::PlaceClip { track: f.track, placement: Placement { id: twin, clip: f.clip, start: 24 * BEAT, lane: 0 } }]));
        let before = f.s.project.clone();
        assert_eq!(f.drag(&[Edit::Start(16 * BEAT + 200), Edit::Start(17 * BEAT)]), 1);
        assert_eq!(f.lane(), [(17 * BEAT, f.clip), (25 * BEAT, f.clip)], "both start a beat later, a beat further into the file");
        assert_eq!((f.clip().offset, f.clip().length), (BEAT, 3 * BEAT));
        // A fade leaves the other placement where it is.
        f.drag(&[Edit::FadeIn(BEAT)]);
        assert_eq!(f.lane(), [(17 * BEAT, f.clip), (25 * BEAT, f.clip)]);
        assert!(f.s.undo() && f.s.undo());
        assert_eq!(f.s.project, before);
    }

    #[test]
    fn a_notes_clip_trims_from_the_left_too() {
        let mut f = fixture();
        let (track, place) = f.s.project.tracks.iter().find_map(|t| {
            t.arrangement.iter().find(|pl| matches!(f.s.project.sources.get(&f.s.project.clips[&pl.clip].source), Some(Source::Notes(_)))).map(|pl| (t.id, pl.clone()))
        }).expect("the demo has a notes clip");
        // Without a loop (inside one the offset is the start marker in the loop).
        let unlooped = Clip { looping: None, ..f.s.project.clips[&place.clip].clone() };
        assert!(f.s.edit("Unloop", vec![Command::UpdateClip { id: place.clip, clip: Box::new(unlooped) }]));
        let old = f.s.project.clips[&place.clip].clone();
        f.s.click_clip(place.id, false);
        let before = f.steps();
        f.s.seal();
        let origin = Origin::take(&f.s, place.id).unwrap();
        assert_eq!(origin.track, track);
        assert!(apply(&mut f.s, &origin, Edit::Start(place.start + BEAT / 2)));
        assert!(apply(&mut f.s, &origin, Edit::Start(place.start + BEAT)));
        let new = &f.s.project.clips[&place.clip];
        assert_eq!((new.offset, new.length, f.s.placement(place.id).unwrap().1.start), (old.offset + BEAT, old.length - BEAT, place.start + BEAT));
        // Mouse-up: the notes move instead of the offset, in the same undo step.
        let heard = |s: &AppState| {
            let c = &s.project.clips[&place.clip];
            let Some(Source::Notes(n)) = s.project.sources.get(&c.source) else { panic!("notes") };
            ops::heard_notes(c, &n.notes).into_iter().map(|n| (n.start, n.key, n.length)).collect::<Vec<_>>()
        };
        let playing = heard(&f.s);
        assert!(finish_trim(&mut f.s, &origin));
        f.s.seal();
        assert_eq!(f.steps(), before + 1);
        assert_eq!(f.s.project.clips[&place.clip].offset, 0);
        assert_eq!(heard(&f.s), playing, "it plays the same");
        let source = f.s.project.clips[&place.clip].source;
        let Some(Source::Notes(n)) = f.s.project.sources.get(&source) else { panic!("notes") };
        assert!(n.notes.iter().all(|n| n.start >= 0), "no note starts before the clip");
        assert!(!finish_trim(&mut f.s, &origin), "settled already");
        assert!(f.s.undo());
        assert_eq!(f.s.project.clips[&place.clip], old);
    }

    #[test]
    fn a_notes_clip_drags_out_to_the_left_and_keeps_its_notes_in_place() {
        let mut f = fixture();
        let (_, place) = f.s.project.tracks.iter().find_map(|t| {
            t.arrangement.iter().find(|pl| pl.start >= 2 * BEAT && matches!(f.s.project.sources.get(&f.s.project.clips[&pl.clip].source), Some(Source::Notes(_)))).map(|pl| (t.id, pl.clone()))
        }).expect("the demo has a notes clip after beat 2");
        let unlooped = Clip { looping: None, ..f.s.project.clips[&place.clip].clone() };
        assert!(f.s.edit("Unloop", vec![Command::UpdateClip { id: place.clip, clip: Box::new(unlooped) }]));
        let starts = |s: &AppState| {
            let c = &s.project.clips[&place.clip];
            let at = s.placement(place.id).unwrap().1.start;
            let Some(Source::Notes(n)) = s.project.sources.get(&c.source) else { panic!("notes") };
            let mut v: Vec<Tick> = ops::heard_notes(c, &n.notes).iter().map(|n| at + n.start).collect();
            v.sort();
            v
        };
        let before = starts(&f.s);
        f.s.seal();
        let origin = Origin::take(&f.s, place.id).unwrap();
        assert!(apply(&mut f.s, &origin, Edit::Start(place.start - 2 * BEAT)));
        finish_trim(&mut f.s, &origin);
        f.s.seal();
        let c = &f.s.project.clips[&place.clip];
        assert_eq!((c.offset, f.s.placement(place.id).unwrap().1.start), (0, place.start - 2 * BEAT));
        assert_eq!(starts(&f.s), before, "two empty beats in front, the notes where they were");
    }

    #[test]
    fn double_click_resets_one_fade() {
        let mut f = fixture();
        f.drag(&[Edit::FadeIn(BEAT)]);
        f.drag(&[Edit::FadeInCurve(0.4)]);
        f.drag(&[Edit::FadeOut(BEAT)]);
        let before = f.steps();
        assert!(reset_fade(&mut f.s, f.clip, true, true));
        assert_eq!(f.clip().fade_in, Fade { length: BEAT, curve: 0. }, "the slope handle only straightens");
        assert!(reset_fade(&mut f.s, f.clip, true, false));
        assert_eq!((f.clip().fade_in, f.clip().fade_out.length), (Fade::default(), BEAT));
        assert_eq!(f.steps(), before + 2);
        assert!(!reset_fade(&mut f.s, f.clip, true, false), "nothing left to reset");
    }

    #[test]
    fn a_copy_drag_is_one_undo_step_and_leaves_the_original() {
        let mut f = fixture();
        let before = f.s.project.clone();
        let steps = f.steps();
        let copies = Copies::make(&mut f.s, f.placement).unwrap();
        assert!(copies.at_home(&f.s));
        assert!(copies.move_to(&mut f.s, BEAT, 0));
        assert!(copies.move_to(&mut f.s, 8 * BEAT, 0));
        let copy = copies.origin[0].0;
        assert!(copies.finish(&mut f.s));
        assert_eq!(f.steps(), steps + 1, "making the copy and moving it are one step");
        assert_eq!(f.s.history.undo_label(), Some("Copy clip"));
        // The original stays, the copy lands where the drag ended and is the selection.
        let new_clip = f.s.placement(copy).unwrap().1.clip;
        assert_eq!(f.lane(), [(16 * BEAT, f.clip), (24 * BEAT, new_clip)]);
        assert_eq!(f.s.selected_clip_ids().into_iter().collect::<Vec<_>>(), [copy]);
        assert_eq!(f.s.selected_clip, Some(new_clip));
        // A clip of its own over the same file.
        assert_ne!(new_clip, f.clip);
        assert_eq!(f.s.project.clips[&new_clip], *f.clip());
        assert_eq!(f.s.project.sources.len(), before.sources.len(), "the file is shared");
        assert!(f.s.undo());
        let mut back = f.s.project.clone();
        back.ids = before.ids.clone();
        assert_eq!(back, before, "one undo takes the copy away");
    }

    #[test]
    fn a_copy_drag_given_up_or_released_where_it_began_creates_nothing() {
        let mut f = fixture();
        let lane = f.lane();
        let clips = f.s.project.clips.len();
        // Escape after dragging away.
        let copies = Copies::make(&mut f.s, f.placement).unwrap();
        copies.move_to(&mut f.s, 4 * BEAT, 0);
        copies.cancel(&mut f.s);
        assert_eq!((f.lane(), f.s.project.clips.len()), (lane.clone(), clips));
        assert_eq!(f.s.selected_clip_ids().into_iter().collect::<Vec<_>>(), [f.placement], "the original is selected again");
        // Dragged away and back, then released.
        let copies = Copies::make(&mut f.s, f.placement).unwrap();
        copies.move_to(&mut f.s, 4 * BEAT, 0);
        copies.move_to(&mut f.s, 0, 0);
        assert!(!copies.finish(&mut f.s));
        assert_eq!((f.lane(), f.s.project.clips.len()), (lane, clips));
        assert_eq!(f.s.selected_clip, Some(f.clip));
    }

    #[test]
    fn a_copy_drag_on_a_selection_copies_all_of_it_and_notes_get_their_own() {
        let mut f = fixture();
        let (midi_track, midi) = f.s.project.tracks.iter().find_map(|t| {
            t.arrangement.iter().find(|pl| matches!(f.s.project.sources.get(&f.s.project.clips[&pl.clip].source), Some(Source::Notes(_)))).map(|pl| (t.id, pl.clone()))
        }).expect("the demo has a notes clip");
        f.s.select_clips([f.placement, midi.id], Some(midi.id));
        let (sources, steps) = (f.s.project.sources.len(), f.steps());
        let copies = Copies::make(&mut f.s, midi.id).unwrap();
        assert_eq!(copies.origin.len(), 2);
        assert!(copies.move_to(&mut f.s, 32 * BEAT, 0));
        let ids: Vec<PlacementId> = copies.origin.iter().map(|o| o.0).collect();
        assert!(copies.finish(&mut f.s));
        assert_eq!(f.steps(), steps + 1);
        assert_eq!(f.s.history.undo_label(), Some("Copy clips"));
        assert_eq!(f.s.selected_clip_ids(), ids.iter().copied().collect());
        // Each copy is on its original's track, 32 beats later.
        let midi_copy = ids.iter().copied().find(|id| f.s.placement(*id).unwrap().0 == midi_track).expect("a copy on the notes track");
        let audio_copy = ids.iter().copied().find(|id| *id != midi_copy).unwrap();
        assert_eq!(f.s.placement(midi_copy).unwrap().1.start, midi.start + 32 * BEAT);
        assert_eq!((f.s.placement(audio_copy).unwrap().0, f.s.placement(audio_copy).unwrap().1.start), (f.track, 48 * BEAT));
        // The notes copy has notes of its own; the audio copy shares the file.
        assert_eq!(f.s.project.sources.len(), sources + 1);
        assert_ne!(f.s.project.clips[&f.s.placement(midi_copy).unwrap().1.clip].source, f.s.project.clips[&midi.clip].source);
        // A clip that is not part of the selection is copied alone.
        f.s.select_clips([midi.id], Some(midi.id));
        let alone = Copies::make(&mut f.s, f.placement).unwrap();
        assert_eq!(alone.origin.len(), 1);
        alone.cancel(&mut f.s);
        assert_eq!(f.s.selected_clip_ids().into_iter().collect::<Vec<_>>(), [midi.id]);
    }
}
