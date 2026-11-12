//! What the sample editor's gestures do to the clip, as commands.
//!
//! A drag starts by taking a [`Snapshot`] of the clip. Every mouse move turns
//! the pointer into a [`Gesture`] and asks the snapshot for the commands that
//! bring the project to "the clip as it was, with this gesture applied", so
//! nothing accumulates along the way: dragging a marker in and back out
//! leaves the fades and the envelope exactly as they were. The rules are the
//! ones in `aitrack_model::ops`; the snapshot keeps a small private project
//! with only this clip in it for the builders to read.
//!
//! The moves of one drag are coalesced into one undo step ([`apply`]); the
//! one-shot edits (split, crop, loop, reset…) are sealed steps of their own.

use super::geom::{curve_through, end_u};
use crate::state::AppState;
use aitrack_model::ops::{self, MIN_CLIP_LEN};
use aitrack_model::*;

/// Where an unplaced clip's stand-in placement starts in the snapshot's
/// private project: far enough from 0 that revealing audio never hits it.
const FAR: Tick = 1 << 40;

/// A placement of the edited clip in the arrangement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub track: TrackId,
    pub placement: PlacementId,
    pub start: Tick,
    pub lane: u16,
}

/// The placement the editor works on: the selected one when it holds this
/// clip, else the clip's first placement. `None` for a session-only clip.
pub fn placement_of(s: &AppState, clip: ClipId) -> Option<Placed> {
    let of = |t: &Track, pl: &Placement| Placed { track: t.id, placement: pl.id, start: pl.start, lane: pl.lane };
    if let Some((track, id)) = s.selected_placement
        && let Some(t) = s.project.track(track)
        && let Some(pl) = t.arrangement.iter().find(|pl| pl.id == id && pl.clip == clip)
    {
        return Some(of(t, pl));
    }
    s.project.tracks.iter().find_map(|t| t.arrangement.iter().find(|pl| pl.clip == clip).map(|pl| of(t, pl)))
}

/// One state a drag asks for. Positions are unrolled source ticks (see
/// `geom`), already snapped; gains are linear 0..=1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    /// The start marker: trims or reveals audio, which stays where it is on the timeline.
    Start(Tick),
    /// The end marker; past the end of the file the clip starts looping.
    End(Tick),
    /// Where the fade-in ends.
    FadeIn(Tick),
    /// Where the fade-out begins.
    FadeOut(Tick),
    /// The gain halfway along the fade's slope.
    FadeInCurve(f32),
    FadeOutCurve(f32),
    /// Envelope breakpoint `index` moved; it stays between its neighbours.
    Point { index: usize, u: Tick, value: f32 },
    /// The envelope's value halfway between breakpoint `index` and the next.
    Bend { index: usize, mid: f32 },
    LoopStart(Tick),
    LoopEnd(Tick),
    /// The whole loop brace moved so that it starts here.
    LoopMove(Tick),
    /// Clip gain in dB.
    Gain(f32),
    /// Clip length in ticks (the end marker by number).
    Length(Tick),
}

impl Gesture {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Start(_) => "Trim clip start",
            Self::End(_) | Self::Length(_) => "Resize clip",
            Self::FadeIn(_) | Self::FadeInCurve(_) => "Fade in",
            Self::FadeOut(_) | Self::FadeOutCurve(_) => "Fade out",
            Self::Point { .. } | Self::Bend { .. } => "Edit clip volume",
            Self::LoopStart(_) | Self::LoopEnd(_) | Self::LoopMove(_) => "Set clip loop",
            Self::Gain(_) => "Change clip gain",
        }
    }
}

/// The clip as it was when a drag began.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub id: ClipId,
    pub clip: Clip,
    pub place: Option<Placed>,
    /// Only this clip, its file and its placement, for the `ops` builders.
    scratch: Project,
    track: TrackId,
    placement: PlacementId,
    start: Tick,
}

impl Snapshot {
    pub fn take(s: &AppState, id: ClipId) -> Option<Self> {
        let clip = s.project.clips.get(&id)?.clone();
        let source = s.project.sources.get(&clip.source)?.clone();
        let place = placement_of(s, id);
        let (track, placement, start, lane) = match place {
            Some(pl) => (pl.track, pl.placement, pl.start, pl.lane),
            None => (TrackId(u64::MAX - 1), PlacementId(u64::MAX - 1), FAR, 0),
        };
        let mut scratch = Project::new("");
        scratch.tempo = s.project.tempo.clone();
        scratch.sources.insert(clip.source, source);
        scratch.clips.insert(id, clip.clone());
        let mut lane_track = Track::new(track, TrackKind::Audio, "", Color(0));
        lane_track.arrangement.push(Placement { id: placement, clip: id, start, lane });
        scratch.tracks = vec![lane_track];
        Some(Self { id, clip, place, scratch, track, placement, start })
    }

    /// Length of the file in source ticks, when it is known.
    pub fn source_len(&self) -> Option<Tick> {
        ops::source_len(&self.scratch, self.id)
    }

    /// The commands that take `now` to the snapshot with `g` applied; none
    /// when it is there already.
    pub fn commands(&self, now: &Project, g: Gesture) -> Vec<Command> {
        let mut cmds = self.wanted(g).unwrap_or_else(|| self.restore());
        if self.place.is_none() {
            cmds.retain(|c| !matches!(c, Command::MovePlacement { .. }));
        }
        cmds.retain(|c| match c {
            Command::UpdateClip { id, clip } => now.clips.get(id) != Some(&**clip),
            Command::MovePlacement { track, id, start, .. } => {
                now.track(*track).and_then(|t| t.arrangement.iter().find(|pl| pl.id == *id)).is_none_or(|pl| pl.start != *start)
            }
            _ => true,
        });
        cmds
    }

    /// Back to how the clip was.
    fn restore(&self) -> Vec<Command> {
        let mut cmds = vec![Command::UpdateClip { id: self.id, clip: Box::new(self.clip.clone()) }];
        if let Some(pl) = self.place {
            cmds.push(Command::MovePlacement { track: pl.track, id: pl.placement, to_track: pl.track, start: pl.start, lane: pl.lane });
        }
        cmds
    }

    /// `g` applied to the snapshot; `None` when that changes nothing.
    fn wanted(&self, g: Gesture) -> Option<Vec<Command>> {
        let (p, id, c) = (&self.scratch, self.id, &self.clip);
        let one = |cmd: Option<Command>| cmd.map(|cmd| vec![cmd]);
        match g {
            Gesture::Start(u) => {
                // Inside a loop the model wraps the offset around; the marker
                // stops at the loop's ends instead, so it never jumps.
                let u = match c.looping.filter(|l| l.end > l.start) {
                    Some(l) => u.clamp(if c.offset >= l.start { l.start } else { 0 }, (l.end - MIN_CLIP_LEN).max(0)),
                    None => u.max(0),
                };
                let mut cmds = ops::trim_start(p, self.track, self.placement, self.start + (u - c.offset))?;
                // The stand-in placement of a session clip is ours alone.
                if self.place.is_none() {
                    cmds.retain(|cmd| matches!(cmd, Command::UpdateClip { .. }));
                }
                Some(cmds)
            }
            Gesture::End(u) => one(ops::resize_end(p, id, u - c.offset)),
            Gesture::Length(len) => one(ops::resize_end(p, id, len)),
            Gesture::FadeIn(u) => one(ops::set_fades(p, id, Fade { length: (u - c.offset).max(0), ..c.fade_in }, c.fade_out)),
            Gesture::FadeOut(u) => one(ops::set_fades(p, id, c.fade_in, Fade { length: (end_u(c) - u).max(0), ..c.fade_out })),
            Gesture::FadeInCurve(mid) => one(ops::set_fades(p, id, Fade { curve: curve_through(mid), ..c.fade_in }, c.fade_out)),
            Gesture::FadeOutCurve(mid) => one(ops::set_fades(p, id, c.fade_in, Fade { curve: curve_through(mid), ..c.fade_out })),
            Gesture::Point { index, u, value } => {
                let mut points = c.volume_envelope()?.points.clone();
                let old = *points.get(index)?;
                let after = index.checked_sub(1).and_then(|i| points.get(i)).map_or(0, |prev| prev.tick + 1).max(0);
                let before = points.get(index + 1).map_or(c.length, |next| next.tick - 1).min(c.length);
                let tick = if after <= before { (u - c.offset).clamp(after, before) } else { old.tick };
                points[index] = AutomationPoint { tick, value: value.clamp(0., 1.), curve: old.curve };
                one(ops::set_volume_envelope(p, id, points))
            }
            Gesture::Bend { index, mid } => {
                let mut points = c.volume_envelope()?.points.clone();
                let (a, b) = (*points.get(index)?, *points.get(index + 1)?);
                let rise = b.value - a.value;
                if rise.abs() < 1e-3 {
                    return None;
                }
                points[index].curve = curve_through((mid - a.value) / rise);
                one(ops::set_volume_envelope(p, id, points))
            }
            Gesture::LoopStart(u) => {
                let l = c.looping?;
                one(ops::set_loop(p, id, Some(LoopRegion { start: u.clamp(0, l.end - MIN_CLIP_LEN), end: l.end })))
            }
            Gesture::LoopEnd(u) => {
                let l = c.looping?;
                one(ops::set_loop(p, id, Some(LoopRegion { start: l.start, end: u.max(l.start + MIN_CLIP_LEN) })))
            }
            Gesture::LoopMove(start) => {
                let l = c.looping?;
                let len = l.end - l.start;
                let last = self.source_len().map_or(Tick::MAX - len, |src| (src - len).max(0));
                let start = start.clamp(0, last);
                one(ops::set_loop(p, id, Some(LoopRegion { start, end: start + len })))
            }
            Gesture::Gain(db) => one(ops::set_gain(p, id, db)),
        }
    }
}

/// One move of a drag: coalesced with the drag's other moves into a single
/// undo step. Returns whether the project changed. The caller seals the
/// history when the drag starts and when it ends.
pub fn apply(s: &mut AppState, snapshot: &Snapshot, g: Gesture) -> bool {
    let cmds = snapshot.commands(&s.project, g);
    !cmds.is_empty() && s.edit_coalesced(&drag_key(snapshot.id), g.label(), cmds)
}

fn drag_key(clip: ClipId) -> String {
    format!("sample-{}", clip.0)
}

/// A whole gesture in one go (buttons, double-clicks, keys): one undo step.
pub fn apply_now(s: &mut AppState, clip: ClipId, g: Gesture) -> bool {
    let Some(snapshot) = Snapshot::take(s, clip) else { return false };
    let cmds = snapshot.commands(&s.project, g);
    !cmds.is_empty() && sealed(s, g.label(), cmds)
}

fn sealed(s: &mut AppState, label: &str, cmds: Vec<Command>) -> bool {
    s.seal();
    let ok = s.edit(label, cmds);
    s.seal();
    ok
}

/// Add a breakpoint to the volume envelope at unrolled position `u` and
/// return its index. Like a drag move it coalesces, so drawing a point and
/// dragging it on in the same press is one undo step.
pub fn add_point(s: &mut AppState, clip: ClipId, u: Tick, value: f32) -> Option<usize> {
    let c = s.project.clips.get(&clip)?;
    let tick = (u - c.offset).clamp(0, c.length);
    let mut points = c.volume_envelope().map(|lane| lane.points.clone()).unwrap_or_default();
    points.push(AutomationPoint { tick, value: value.clamp(0., 1.), curve: 0. });
    let cmd = ops::set_volume_envelope(&s.project, clip, points)?;
    if !s.edit_coalesced(&drag_key(clip), "Add volume breakpoint", vec![cmd]) {
        return None;
    }
    s.project.clips.get(&clip)?.volume_envelope()?.points.iter().position(|pt| pt.tick == tick)
}

/// Remove breakpoint `index`; the last one takes the envelope with it.
pub fn delete_point(s: &mut AppState, clip: ClipId, index: usize) -> bool {
    let Some(mut points) = s.project.clips.get(&clip).and_then(|c| c.volume_envelope()).map(|lane| lane.points.clone()) else { return false };
    if index >= points.len() {
        return false;
    }
    points.remove(index);
    match ops::set_volume_envelope(&s.project, clip, points) {
        Some(cmd) => sealed(s, "Delete volume breakpoint", vec![cmd]),
        None => false,
    }
}

pub fn toggle_loop(s: &mut AppState, clip: ClipId) -> bool {
    match ops::toggle_loop(&s.project, clip) {
        Some(cmd) => sealed(s, "Loop clip", vec![cmd]),
        None => false,
    }
}

/// Remove the fades and the volume envelope.
pub fn reset_shaping(s: &mut AppState, clip: ClipId) -> bool {
    let Some(old) = s.project.clips.get(&clip) else { return false };
    let mut new = Clip { fade_in: Fade::default(), fade_out: Fade::default(), ..old.clone() };
    new.envelopes.retain(|lane| lane.target != AutomationTarget::Volume);
    *old != new && sealed(s, "Reset fades and volume", vec![Command::UpdateClip { id: clip, clip: Box::new(new) }])
}

/// Cut the clip in two at unrolled position `u`. The left part keeps the
/// clip's id and stays selected. Needs the clip to be in the arrangement.
pub fn split(s: &mut AppState, clip: ClipId, u: Tick) -> Result<(), &'static str> {
    let offset = s.project.clips.get(&clip).ok_or("There is no clip to split")?.offset;
    let place = placement_of(s, clip).ok_or("Place the clip in the arrangement to split it")?;
    let at = place.start + (u - offset);
    let cmds = ops::split_at(&mut s.project, place.track, place.placement, at).ok_or("Put the cursor inside the clip to split it")?;
    if !sealed(s, "Split clip", cmds) {
        return Err("The clip could not be split");
    }
    s.selected_clip = Some(clip);
    s.selected_placement = Some((place.track, place.placement));
    Ok(())
}

/// Keep only the unrolled range `a..b` of the clip. What is kept stays
/// where it was on the timeline, in every placement of the clip.
pub fn crop(s: &mut AppState, clip: ClipId, a: Tick, b: Tick) -> bool {
    let Some(c) = s.project.clips.get(&clip) else { return false };
    let from = (a.min(b) - c.offset).clamp(0, c.length);
    let to = (a.max(b) - c.offset).clamp(0, c.length);
    let Some(cmd) = ops::crop(&s.project, clip, from, to) else { return false };
    let mut cmds = vec![cmd];
    if from > 0 {
        for t in &s.project.tracks {
            for pl in t.arrangement.iter().filter(|pl| pl.clip == clip) {
                cmds.push(Command::MovePlacement { track: t.id, id: pl.id, to_track: t.id, start: pl.start + from, lane: pl.lane });
            }
        }
    }
    sealed(s, "Crop clip", cmds)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEAT: Tick = PPQ;

    struct Fixture {
        s: AppState,
        clip: ClipId,
        track: TrackId,
        placement: Option<PlacementId>,
    }

    /// The demo project plus an audio clip of 4 beats over an 8-beat file
    /// (the demo runs at 128 BPM), placed at beat 16 or left in no track.
    fn fixture(placed: bool) -> Fixture {
        let mut s = AppState::new(crate::demo::project());
        let bpm = s.project.tempo.initial_bpm();
        let frames = (8. * 60. / bpm * 48_000.).round() as u64;
        let (source, clip, track) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_track_id());
        let audio = AudioSource { path: "loop.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames, warp: Warp::default(), key: None };
        let index = s.project.tracks.len() - 1;
        let mut cmds = vec![
            Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Audio, "Loop", Color::palette(3))), index },
            Command::AddSource { id: source, source: Box::new(Source::Audio(audio)) },
            Command::AddClip { id: clip, clip: Box::new(Clip::new("Loop", Color::palette(3), source, 4 * BEAT)) },
        ];
        let placement = placed.then(|| s.project.alloc_placement_id());
        if let Some(id) = placement {
            cmds.push(Command::PlaceClip { track, placement: Placement { id, clip, start: 16 * BEAT, lane: 0 } });
        }
        assert!(s.edit("Add loop", cmds));
        s.seal();
        s.select_clip(Some(clip));
        assert_eq!(ops::source_len(&s.project, clip), Some(8 * BEAT));
        Fixture { s, clip, track, placement }
    }

    impl Fixture {
        fn clip(&self) -> &Clip {
            &self.s.project.clips[&self.clip]
        }

        fn start(&self) -> Tick {
            let id = self.placement.expect("placed");
            self.s.project.track(self.track).unwrap().arrangement.iter().find(|pl| pl.id == id).unwrap().start
        }

        fn steps(&self) -> usize {
            self.s.history.steps().count()
        }

        /// A whole drag: mouse-down, the moves, mouse-up.
        fn drag(&mut self, moves: &[Gesture]) -> usize {
            let before = self.steps();
            self.s.seal();
            let snapshot = Snapshot::take(&self.s, self.clip).expect("snapshot");
            for g in moves {
                apply(&mut self.s, &snapshot, *g);
            }
            self.s.seal();
            self.steps() - before
        }

        fn change(&mut self, f: impl FnOnce(&mut Clip)) {
            let mut c = self.clip().clone();
            f(&mut c);
            assert!(self.s.edit("Set up", vec![Command::UpdateClip { id: self.clip, clip: Box::new(c) }]));
            self.s.seal();
        }
    }

    fn points(c: &Clip) -> Vec<(Tick, f32)> {
        c.volume_envelope().map(|lane| lane.points.iter().map(|pt| (pt.tick, pt.value)).collect()).unwrap_or_default()
    }

    fn pt(tick: Tick, value: f32) -> AutomationPoint {
        AutomationPoint { tick, value, curve: 0. }
    }

    #[test]
    fn a_fade_handle_drag_is_one_undo_step() {
        let mut f = fixture(true);
        let before = f.clip().clone();
        // Pull the fade-in out to a beat, in small moves.
        let moves: Vec<Gesture> = (1..=8).map(|i| Gesture::FadeIn(i * BEAT / 8)).collect();
        assert_eq!(f.drag(&moves), 1);
        assert_eq!(f.clip().fade_in, Fade { length: BEAT, curve: 0. });
        assert_eq!(f.clip().gain_at(BEAT / 2), 0.5);
        assert_eq!(f.s.history.undo_label(), Some("Fade in"));
        // Bend it: lifting the middle of the slope to 0.8.
        assert_eq!(f.drag(&[Gesture::FadeInCurve(0.6), Gesture::FadeInCurve(0.8)]), 1);
        assert!(f.clip().fade_in.curve < 0.);
        assert!((f.clip().gain_at(BEAT / 2) - 0.8).abs() < 1e-3);
        // The fade-out stops at the fade-in.
        assert_eq!(f.drag(&[Gesture::FadeOut(3 * BEAT), Gesture::FadeOut(-5 * BEAT)]), 1);
        assert_eq!(f.clip().fade_out.length, 3 * BEAT);
        assert_eq!(f.drag(&[Gesture::FadeOutCurve(0.2)]), 1);
        assert!(f.clip().fade_out.curve > 0.);
        // Undo walks back one drag at a time, to exactly the clip we started with.
        assert!(f.s.undo());
        assert_eq!(f.clip().fade_out, Fade { length: 3 * BEAT, curve: 0. });
        assert!(f.s.undo() && f.s.undo());
        assert_eq!(f.clip().fade_in, Fade { length: BEAT, curve: 0. });
        assert!(f.s.undo());
        assert_eq!(*f.clip(), before);
    }

    #[test]
    fn an_envelope_point_drag_is_one_undo_step() {
        let mut f = fixture(true);
        let clip = f.clip;
        // Two breakpoints: the attack and where it reaches full level.
        f.s.seal();
        assert_eq!(add_point(&mut f.s, clip, 0, 0.2), Some(0));
        f.s.seal();
        assert_eq!(add_point(&mut f.s, clip, BEAT, 1.), Some(1));
        f.s.seal();
        assert_eq!(points(f.clip()), vec![(0, 0.2), (BEAT, 1.)]);
        let before = f.clip().clone();
        // Drag the second one later and lower.
        let moves: Vec<Gesture> = (1..=6).map(|i| Gesture::Point { index: 1, u: BEAT + i * BEAT / 4, value: 1. - 0.05 * i as f32 }).collect();
        assert_eq!(f.drag(&moves), 1);
        assert_eq!(points(f.clip()), vec![(0, 0.2), (5 * BEAT / 2, 0.7)]);
        assert_eq!(f.s.history.undo_label(), Some("Edit clip volume"));
        assert!(f.s.undo());
        assert_eq!(*f.clip(), before, "undo restores exactly");
        assert!(f.s.redo());
        // A breakpoint stays between its neighbours and inside the clip.
        assert_eq!(f.drag(&[Gesture::Point { index: 0, u: 9 * BEAT, value: 2. }]), 1);
        assert_eq!(points(f.clip()), vec![(5 * BEAT / 2 - 1, 1.), (5 * BEAT / 2, 0.7)]);
        assert_eq!(f.drag(&[Gesture::Point { index: 1, u: 99 * BEAT, value: -1. }]), 1);
        assert_eq!(points(f.clip())[1], (4 * BEAT, 0.));
        // Bending the segment between them: the line passes through `mid` halfway.
        assert_eq!(f.drag(&[Gesture::Bend { index: 0, mid: 0.9 }, Gesture::Bend { index: 0, mid: 0.25 }]), 1);
        let lane = f.clip().volume_envelope().unwrap();
        let half = (lane.points[0].tick + lane.points[1].tick) / 2;
        assert!((lane.value_at(half).unwrap() - 0.25).abs() < 0.01);
        assert_eq!(f.s.history.undo_label(), Some("Edit clip volume"));
        // Drawing a breakpoint and dragging it on in the same press is one step too.
        let steps = f.steps();
        f.s.seal();
        let index = add_point(&mut f.s, clip, 2 * BEAT, 0.5).expect("added");
        let snapshot = Snapshot::take(&f.s, clip).unwrap();
        assert!(apply(&mut f.s, &snapshot, Gesture::Point { index, u: 2 * BEAT + 240, value: 0.4 }));
        f.s.seal();
        assert_eq!(f.steps(), steps + 1);
        assert!(points(f.clip()).contains(&(2 * BEAT + 240, 0.4)));
        // Deleting: by index, and the last one removes the envelope.
        assert!(delete_point(&mut f.s, clip, index));
        assert_eq!(points(f.clip()).len(), 2);
        assert!(delete_point(&mut f.s, clip, 0) && delete_point(&mut f.s, clip, 0));
        assert!(f.clip().envelopes.is_empty());
        assert!(!delete_point(&mut f.s, clip, 0));
    }

    #[test]
    fn a_marker_drag_is_one_undo_step() {
        let mut f = fixture(true);
        f.change(|c| {
            c.fade_in.length = 3 * BEAT;
            c.fade_out.length = BEAT;
            c.envelopes.push(AutomationLane { points: vec![pt(0, 1.), pt(2 * BEAT, 0.5)], ..AutomationLane::new(AutomationTarget::Volume) });
        });
        let before = f.clip().clone();
        // The start marker, dragged in past where both fades fit and back out a little.
        let moves = [Gesture::Start(BEAT / 2), Gesture::Start(BEAT), Gesture::Start(3 * BEAT), Gesture::Start(2 * BEAT), Gesture::Start(BEAT)];
        assert_eq!(f.drag(&moves), 1);
        // One beat trimmed: the audio stayed where it was on the timeline.
        assert_eq!((f.clip().offset, f.clip().length, f.start()), (BEAT, 3 * BEAT, 17 * BEAT));
        assert_eq!(points(f.clip()), vec![(-BEAT, 1.), (BEAT, 0.5)], "the envelope moved with the audio");
        // The fade gave way by exactly as much as this final position needs,
        // not by what the furthest point of the drag needed.
        assert_eq!((f.clip().fade_in.length, f.clip().fade_out.length), (2 * BEAT, BEAT));
        assert_eq!(f.s.history.undo_label(), Some("Trim clip start"));
        assert!(f.s.undo());
        assert_eq!((f.clip(), f.start()), (&before, 16 * BEAT), "undo restores exactly");
        // Dragged away and back to where it started: nothing is left changed.
        f.drag(&[Gesture::Start(2 * BEAT), Gesture::Start(0)]);
        assert_eq!((f.clip(), f.start()), (&before, 16 * BEAT));
        // The start cannot go before the file's.
        f.drag(&[Gesture::Start(-3 * BEAT)]);
        assert_eq!((f.clip().offset, f.start()), (0, 16 * BEAT));

        // The end marker: shorter, then out past the end of the file, which loops it.
        let moves = [Gesture::End(2 * BEAT), Gesture::End(6 * BEAT), Gesture::End(20 * BEAT)];
        assert_eq!(f.drag(&moves), 1);
        assert_eq!(f.clip().length, 20 * BEAT);
        assert_eq!(f.clip().looping, Some(LoopRegion { start: 0, end: 8 * BEAT }));
        assert_eq!(f.clip().fade_out.length, BEAT, "the fade survived the short stretch of the drag");
        assert_eq!(f.s.history.undo_label(), Some("Resize clip"));
        // Back inside the file the automatic loop goes away again.
        assert_eq!(f.drag(&[Gesture::End(5 * BEAT)]), 1);
        assert_eq!((f.clip().length, f.clip().looping), (5 * BEAT, None));
        assert!(f.s.undo() && f.s.undo());
        assert_eq!((f.clip(), f.start()), (&before, 16 * BEAT));
    }

    #[test]
    fn a_click_without_movement_changes_nothing() {
        let mut f = fixture(true);
        f.change(|c| c.fade_in.length = BEAT);
        let (before, steps) = (f.clip().clone(), f.steps());
        // A drag whose every move lands where the handle already is.
        let still = [Gesture::Start(0), Gesture::End(4 * BEAT), Gesture::FadeIn(BEAT), Gesture::FadeOut(4 * BEAT), Gesture::Gain(0.), Gesture::Length(4 * BEAT)];
        assert_eq!(f.drag(&still), 0);
        assert_eq!(f.drag(&[]), 0);
        // Gestures on things the clip does not have.
        assert_eq!(f.drag(&[Gesture::Point { index: 3, u: BEAT, value: 0.5 }, Gesture::Bend { index: 0, mid: 0.5 }, Gesture::LoopEnd(BEAT)]), 0);
        assert_eq!((f.clip(), f.steps()), (&before, steps));
    }

    #[test]
    fn a_session_clip_is_edited_without_a_placement() {
        let mut f = fixture(false);
        assert!(placement_of(&f.s, f.clip).is_none());
        f.change(|c| c.envelopes.push(AutomationLane { points: vec![pt(BEAT, 0.5)], ..AutomationLane::new(AutomationTarget::Volume) }));
        let before = f.clip().clone();
        assert_eq!(f.drag(&[Gesture::Start(BEAT), Gesture::Start(3 * BEAT / 2)]), 1);
        assert_eq!((f.clip().offset, f.clip().length), (3 * BEAT / 2, 5 * BEAT / 2));
        assert_eq!(points(f.clip()), vec![(-BEAT / 2, 0.5)]);
        assert_eq!(f.drag(&[Gesture::End(3 * BEAT)]), 1);
        assert_eq!(f.clip().length, 3 * BEAT / 2);
        assert_eq!(f.drag(&[Gesture::FadeOut(5 * BEAT / 2)]), 1);
        assert_eq!(f.clip().fade_out.length, BEAT / 2);
        assert!(f.s.undo() && f.s.undo() && f.s.undo());
        assert_eq!(*f.clip(), before);
        // Everything except splitting works.
        assert!(split(&mut f.s, f.clip, 2 * BEAT).is_err());
        assert_eq!(*f.clip(), before);
        assert!(crop(&mut f.s, f.clip, BEAT, 3 * BEAT));
        assert_eq!((f.clip().offset, f.clip().length), (BEAT, 2 * BEAT));
    }

    #[test]
    fn the_loop_brace_drags_and_the_clip_repeats() {
        let mut f = fixture(true);
        let clip = f.clip;
        assert!(toggle_loop(&mut f.s, clip));
        assert_eq!(f.clip().looping, Some(LoopRegion { start: 0, end: 4 * BEAT }));
        assert_eq!(f.s.history.undo_label(), Some("Loop clip"));
        // Shorten the loop from both ends.
        assert_eq!(f.drag(&[Gesture::LoopEnd(3 * BEAT), Gesture::LoopEnd(2 * BEAT)]), 1);
        assert_eq!(f.drag(&[Gesture::LoopStart(BEAT / 2), Gesture::LoopStart(BEAT)]), 1);
        assert_eq!(f.clip().looping, Some(LoopRegion { start: BEAT, end: 2 * BEAT }));
        assert_eq!(f.s.history.undo_label(), Some("Set clip loop"));
        // The ends cannot cross, and the loop stays inside the file.
        f.drag(&[Gesture::LoopStart(50 * BEAT)]);
        assert_eq!(f.clip().looping, Some(LoopRegion { start: 2 * BEAT - MIN_CLIP_LEN, end: 2 * BEAT }));
        assert!(f.s.undo());
        f.drag(&[Gesture::LoopEnd(50 * BEAT)]);
        assert_eq!(f.clip().looping, Some(LoopRegion { start: BEAT, end: 8 * BEAT }));
        assert!(f.s.undo());
        // Moving the brace keeps its length.
        assert_eq!(f.drag(&[Gesture::LoopMove(3 * BEAT), Gesture::LoopMove(50 * BEAT)]), 1);
        assert_eq!(f.clip().looping, Some(LoopRegion { start: 7 * BEAT, end: 8 * BEAT }));
        assert!(f.s.undo());
        // The clip made longer by number: it repeats the loop.
        assert!(apply_now(&mut f.s, clip, Gesture::Length(16 * BEAT)));
        assert_eq!((f.clip().length, f.clip().looping), (16 * BEAT, Some(LoopRegion { start: BEAT, end: 2 * BEAT })));
        // Inside a loop the start marker stops at the loop's ends.
        f.change(|c| c.offset = 3 * BEAT / 2);
        f.drag(&[Gesture::Start(0)]);
        assert_eq!(f.clip().offset, BEAT);
        f.drag(&[Gesture::Start(7 * BEAT)]);
        assert_eq!(f.clip().offset, 2 * BEAT - MIN_CLIP_LEN);
        // Off again.
        assert!(toggle_loop(&mut f.s, clip));
        assert_eq!(f.clip().looping, None);
    }

    #[test]
    fn gain_and_reset_are_single_steps() {
        let mut f = fixture(true);
        let clip = f.clip;
        let moves: Vec<Gesture> = (1..=10).map(|i| Gesture::Gain(-0.6 * i as f32)).collect();
        assert_eq!(f.drag(&moves), 1);
        assert!((f.clip().gain_db + 6.).abs() < 1e-4);
        assert_eq!(f.drag(&[Gesture::Gain(90.)]), 1);
        assert_eq!(f.clip().gain_db, 24.);
        assert!(apply_now(&mut f.s, clip, Gesture::Gain(0.)));
        assert!(!apply_now(&mut f.s, clip, Gesture::Gain(0.)), "already there");
        // Reset takes the fades and the envelope away and leaves the rest.
        f.change(|c| {
            c.fade_in = Fade { length: BEAT, curve: 0.5 };
            c.fade_out.length = BEAT;
            c.gain_db = -3.;
            c.envelopes.push(AutomationLane { points: vec![pt(0, 0.5)], ..AutomationLane::new(AutomationTarget::Volume) });
            c.envelopes.push(AutomationLane::new(AutomationTarget::Pan));
        });
        let steps = f.steps();
        assert!(reset_shaping(&mut f.s, clip));
        assert_eq!(f.steps(), steps + 1);
        let c = f.clip();
        assert_eq!((c.fade_in, c.fade_out, c.gain_db), (Fade::default(), Fade::default(), -3.));
        assert_eq!(c.envelopes.iter().map(|lane| lane.target).collect::<Vec<_>>(), vec![AutomationTarget::Pan]);
        assert_eq!(c.gain_at(0), 1.);
        assert!(!reset_shaping(&mut f.s, clip), "nothing left to reset");
    }

    #[test]
    fn split_at_the_cursor_yields_two_clips() {
        let mut f = fixture(true);
        let (clip, track, placement) = (f.clip, f.track, f.placement.unwrap());
        f.change(|c| {
            c.offset = BEAT;
            c.fade_out.length = BEAT;
        });
        let (clips, steps) = (f.s.project.clips.len(), f.steps());
        // The cursor is in source time: 2.5 beats into the file is 1.5 beats into the clip.
        assert_eq!(split(&mut f.s, clip, 5 * BEAT / 2), Ok(()));
        assert_eq!(f.steps(), steps + 1);
        assert_eq!(f.s.history.undo_label(), Some("Split clip"));
        assert_eq!(f.s.project.clips.len(), clips + 1);
        let lane = &f.s.project.track(track).unwrap().arrangement;
        assert_eq!(lane.len(), 2);
        let right_place = lane.iter().find(|pl| pl.id != placement).unwrap();
        let (left, right) = (f.clip(), &f.s.project.clips[&right_place.clip]);
        assert_eq!(left.length + right.length, 4 * BEAT, "the lengths add up");
        assert_eq!((left.offset, left.length), (BEAT, 3 * BEAT / 2));
        assert_eq!((right.offset, right.length), (5 * BEAT / 2, 5 * BEAT / 2), "the right half starts where the left one ends");
        assert_eq!((f.start(), right_place.start), (16 * BEAT, 16 * BEAT + 3 * BEAT / 2));
        assert_eq!((left.fade_out.length, right.fade_out.length), (0, BEAT));
        // The left part stays selected.
        assert_eq!((f.s.selected_clip, f.s.selected_placement), (Some(clip), Some((track, placement))));
        // Outside the clip there is nothing to cut.
        assert!(split(&mut f.s, clip, BEAT).is_err());
        assert!(split(&mut f.s, clip, 7 * BEAT).is_err());
        assert_eq!(f.steps(), steps + 1);
        assert!(f.s.undo());
        assert_eq!((f.s.project.clips.len(), f.clip().length), (clips, 4 * BEAT));
        assert_eq!(f.s.selected_clip, Some(clip));
    }

    #[test]
    fn crop_keeps_the_selection_where_it_was_on_the_timeline() {
        let mut f = fixture(true);
        let clip = f.clip;
        f.change(|c| c.offset = BEAT);
        let steps = f.steps();
        // Source beats 2..4 are clip beats 1..3; the selection may be given either way round.
        assert!(crop(&mut f.s, clip, 4 * BEAT, 2 * BEAT));
        assert_eq!(f.steps(), steps + 1);
        assert_eq!((f.clip().offset, f.clip().length, f.start()), (2 * BEAT, 2 * BEAT, 17 * BEAT));
        // A selection that reaches outside the clip is cut to it; one that keeps everything does nothing.
        assert!(!crop(&mut f.s, clip, 0, 50 * BEAT));
        assert!(crop(&mut f.s, clip, -BEAT, 3 * BEAT));
        assert_eq!((f.clip().offset, f.clip().length, f.start()), (2 * BEAT, BEAT, 17 * BEAT));
        assert!(f.s.undo() && f.s.undo());
        assert_eq!((f.clip().offset, f.clip().length, f.start()), (BEAT, 4 * BEAT, 16 * BEAT));
    }

    #[test]
    fn the_selected_placement_is_the_one_edited() {
        let mut f = fixture(true);
        let (clip, track) = (f.clip, f.track);
        // The same clip a second time, later in the song.
        let second = f.s.project.alloc_placement_id();
        assert!(f.s.edit("Place", vec![Command::PlaceClip { track, placement: Placement { id: second, clip, start: 40 * BEAT, lane: 0 } }]));
        assert_eq!(placement_of(&f.s, clip).map(|pl| pl.start), Some(16 * BEAT));
        f.s.selected_placement = Some((track, second));
        assert_eq!(placement_of(&f.s, clip).map(|pl| (pl.placement, pl.start)), Some((second, 40 * BEAT)));
        // A selection that is some other clip's does not count.
        let other = f.s.project.tracks.iter().find_map(|t| t.arrangement.iter().find(|pl| pl.clip != clip).map(|pl| (t.id, pl.id)));
        if let Some(other) = other {
            f.s.selected_placement = Some(other);
            assert_eq!(placement_of(&f.s, clip).map(|pl| pl.start), Some(16 * BEAT));
        }
    }
}
