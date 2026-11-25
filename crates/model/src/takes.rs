//! Take lanes and comping.
//!
//! A track can hold take lanes under its main lane. Take lane `i` is where
//! the placements with `lane == i + 1` live; they are silent. The main lane
//! (`lane == 0`) is the comp, the only one the engine plays. Recording a loop
//! several times puts each pass in its own take lane, and comping copies
//! the best parts of the takes into the main lane.
//!
//! The lane commands (`AddTakeLane`, `RemoveTakeLane`, `MoveTakeLane`, …) are
//! applied by [`Command::apply`]; the helpers here do their bookkeeping.
//! [`comp_range`] is a builder: the commands that promote a stretch of a take
//! into the main lane, to be applied as one step.

use crate::command::{Command, CommandError};
use crate::ids::*;
use crate::ops;
use crate::project::*;
use crate::time::{Tick, PPQ};
use std::collections::BTreeSet;

/// The most take lanes a track can hold (lane numbers are `u16`).
pub const MAX_TAKE_LANES: usize = 64;

/// The length of the fades that join a promoted stretch to its neighbours.
pub const COMP_FADE: Tick = PPQ / 32;

/// A lane change: how it moves the lane numbers of placements and comp spans.
#[derive(Clone, Copy, Debug)]
pub(crate) enum LaneOp {
    /// A lane was inserted at `index` (0-based among the take lanes).
    Insert(usize),
    /// The lane at `index` was removed.
    Remove(usize),
    /// The lane at `from` moved to `to`.
    Move { from: usize, to: usize },
}

impl LaneOp {
    /// Where the placement lane `lane` (1-based; 0 = main) goes. `None` when
    /// the lane no longer exists.
    pub(crate) fn map(self, lane: u16) -> Option<u16> {
        if lane == 0 {
            return Some(0);
        }
        let i = usize::from(lane) - 1;
        let new = match self {
            Self::Insert(at) => i + usize::from(i >= at),
            Self::Remove(at) if i == at => return None,
            Self::Remove(at) => i - usize::from(i > at),
            Self::Move { from, to } if i == from => to,
            Self::Move { from, to } => {
                let rest = i - usize::from(i > from);
                rest + usize::from(rest >= to)
            }
        };
        Some(new as u16 + 1)
    }
}

/// Apply `op` to the lane numbers of `t`'s placements and comp spans. Placements
/// of a removed lane are returned (and taken out of the track).
pub(crate) fn renumber(t: &mut Track, op: LaneOp) -> Vec<Placement> {
    let mut gone = Vec::new();
    let mut kept = Vec::with_capacity(t.arrangement.len());
    for mut pl in std::mem::take(&mut t.arrangement) {
        match op.map(pl.lane) {
            Some(l) => {
                pl.lane = l;
                kept.push(pl);
            }
            None => gone.push(pl),
        }
    }
    t.arrangement = kept;
    t.comp = std::mem::take(&mut t.comp)
        .into_iter()
        .filter_map(|mut s| {
            s.lane = op.map(s.lane)?;
            Some(s)
        })
        .collect();
    gone
}

pub(crate) fn add_lane(p: &mut Project, track: TrackId, index: usize, name: String) -> Result<Command, CommandError> {
    let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
    if index > t.take_lanes.len() {
        return Err(CommandError::OutOfRange(index));
    }
    if t.take_lanes.len() >= MAX_TAKE_LANES {
        return Err(CommandError::Invalid(format!("a track holds at most {MAX_TAKE_LANES} take lanes")));
    }
    renumber(t, LaneOp::Insert(index));
    t.take_lanes.insert(index, TakeLane { name });
    Ok(Command::RemoveTakeLane { track, index })
}

pub(crate) fn remove_lane(p: &mut Project, track: TrackId, index: usize) -> Result<Command, CommandError> {
    let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
    if index >= t.take_lanes.len() {
        return Err(CommandError::OutOfRange(index));
    }
    let comp = t.comp.clone();
    let lane = t.take_lanes.remove(index);
    let gone = renumber(t, LaneOp::Remove(index));
    // Undo: the lane, its takes (back on its number once it is inserted), the comp marks.
    let mut restore = vec![Command::AddTakeLane { track, index, name: lane.name }];
    restore.extend(gone.into_iter().map(|placement| Command::PlaceClip { track, placement }));
    restore.push(Command::SetComp { track, spans: comp });
    Ok(Command::Batch { commands: restore })
}

pub(crate) fn move_lane(p: &mut Project, track: TrackId, from: usize, to: usize) -> Result<Command, CommandError> {
    let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
    let n = t.take_lanes.len();
    if from >= n {
        return Err(CommandError::OutOfRange(from));
    }
    if to >= n {
        return Err(CommandError::OutOfRange(to));
    }
    let lane = t.take_lanes.remove(from);
    t.take_lanes.insert(to, lane);
    renumber(t, LaneOp::Move { from, to });
    Ok(Command::MoveTakeLane { track, from: to, to: from })
}

pub(crate) fn rename_lane(p: &mut Project, track: TrackId, index: usize, name: String) -> Result<Command, CommandError> {
    let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
    let lane = t.take_lanes.get_mut(index).ok_or(CommandError::OutOfRange(index))?;
    let old = std::mem::replace(&mut lane.name, name);
    Ok(Command::RenameTakeLane { track, index, name: old })
}

/// Replace the comp spans (made sorted, non-overlapping, with no empty span).
pub(crate) fn set_comp(p: &mut Project, track: TrackId, spans: Vec<CompSpan>) -> Result<Command, CommandError> {
    let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
    if let Some(s) = spans.iter().find(|s| s.lane == 0 || usize::from(s.lane) > t.take_lanes.len()) {
        return Err(CommandError::Invalid(format!("comp span points at take lane {}", s.lane)));
    }
    Ok(Command::SetComp { track, spans: std::mem::replace(&mut t.comp, normalize(spans)) })
}

fn normalize(mut spans: Vec<CompSpan>) -> Vec<CompSpan> {
    spans.retain(|s| s.end > s.start);
    spans.sort_by_key(|s| (s.start, s.end));
    let mut out: Vec<CompSpan> = Vec::with_capacity(spans.len());
    for mut s in spans {
        if let Some(last) = out.last_mut() {
            if s.start < last.end {
                s.start = last.end;
                if s.end <= s.start {
                    continue;
                }
            }
            if last.lane == s.lane && last.end == s.start {
                last.end = s.end;
                continue;
            }
        }
        out.push(s);
    }
    out
}

/// `spans` with `a..b` made empty, then `new` added.
fn with_promoted(spans: &[CompSpan], a: Tick, b: Tick, new: &[CompSpan]) -> Vec<CompSpan> {
    let mut out = Vec::with_capacity(spans.len() + new.len() + 1);
    for s in spans {
        if s.end <= a || s.start >= b {
            out.push(*s);
            continue;
        }
        if s.start < a {
            out.push(CompSpan { end: a, ..*s });
        }
        if s.end > b {
            out.push(CompSpan { start: b, ..*s });
        }
    }
    out.extend_from_slice(new);
    normalize(out)
}

/// A name for a new take lane: "Take 1", "Take 2"… the first not in use.
pub fn next_lane_name(t: &Track) -> String {
    (1..).map(|n| format!("Take {n}")).find(|name| t.take_lanes.iter().all(|l| &l.name != name)).expect("a free name")
}

/// The placements of take lane `index` of `track`, in time order.
pub fn lane_placements(t: &Track, index: usize) -> impl Iterator<Item = &Placement> {
    let lane = index as u16 + 1;
    t.arrangement.iter().filter(move |pl| pl.lane == lane)
}

/// The stretches of take lane `index` that the comp plays (its comp spans on
/// that lane that the main lane still has a clip under), as `(start, end)`.
pub fn comp_used(p: &Project, track: TrackId, index: usize) -> Vec<(Tick, Tick)> {
    let Some(t) = p.track(track) else { return Vec::new() };
    let lane = index as u16 + 1;
    let main: Vec<(Tick, Tick)> = t.arrangement.iter().filter(|pl| pl.lane == 0).filter_map(|pl| Some((pl.start, pl.start + p.clips.get(&pl.clip)?.length))).collect();
    let mut out: Vec<(Tick, Tick)> = Vec::new();
    for s in t.comp.iter().filter(|s| s.lane == lane) {
        for &(a, b) in &main {
            let (x, y) = (s.start.max(a), s.end.min(b));
            if y > x {
                match out.last_mut() {
                    Some(last) if last.1 == x => last.1 = y,
                    _ => out.push((x, y)),
                }
            }
        }
    }
    out
}

/// A copy of the project the builders work on, applying each command as it
/// is made so the next one sees its effect.
struct Scratch {
    p: Project,
    cmds: Vec<Command>,
    freed: BTreeSet<ClipId>,
    failed: bool,
}

impl Scratch {
    fn run(&mut self, cmd: Command) {
        if self.failed {
            return;
        }
        match cmd.clone().apply(&mut self.p) {
            Ok(_) => self.cmds.push(cmd),
            Err(_) => self.failed = true,
        }
    }

    fn run_all(&mut self, cmds: impl IntoIterator<Item = Command>) {
        for c in cmds {
            self.run(c);
        }
    }

    fn remove(&mut self, track: TrackId, id: PlacementId) {
        if let Some(pl) = self.p.track(track).and_then(|t| t.arrangement.iter().find(|x| x.id == id)) {
            self.freed.insert(pl.clip);
        }
        self.run(Command::RemovePlacement { track, id });
    }

    /// The main-lane placements of `track` with their end, for which `keep(start, end)` holds.
    fn main(&self, track: TrackId, keep: impl Fn(Tick, Tick) -> bool) -> Vec<Placement> {
        let Some(t) = self.p.track(track) else { return Vec::new() };
        t.arrangement.iter().filter(|pl| pl.lane == 0 && self.p.clips.get(&pl.clip).is_some_and(|c| keep(pl.start, pl.start + c.length))).cloned().collect()
    }

    /// Empty `a..b` of the main lane: clips inside go, clips across an edge are cut there.
    fn clear_main(&mut self, track: TrackId, a: Tick, b: Tick) {
        for edge in [a, b] {
            for pl in self.main(track, |x, y| x < edge && edge < y) {
                if let Some(cmds) = ops::split_at(&mut self.p, track, pl.id, edge) {
                    self.run_all(cmds);
                }
            }
        }
        for pl in self.main(track, |x, y| x >= a && y <= b) {
            self.remove(track, pl.id);
        }
    }

    /// Remove the clips (and then the sources) nothing uses any more.
    fn sweep(&mut self) {
        let mut sources = BTreeSet::new();
        for id in std::mem::take(&mut self.freed) {
            let Some(clip) = self.p.clips.get(&id) else { continue };
            if !self.clip_in_use(id) {
                sources.insert(clip.source);
                self.run(Command::RemoveClip { id });
            }
        }
        for id in sources {
            if self.p.sources.contains_key(&id) && !self.p.clips.values().any(|c| c.source == id) {
                self.run(Command::RemoveSource { id });
            }
        }
    }

    fn clip_in_use(&self, id: ClipId) -> bool {
        self.p.tracks.iter().any(|t| t.arrangement.iter().any(|pl| pl.clip == id) || t.session_slots.values().any(|c| *c == id))
    }

    /// Fade the start (`start`) or the end of a clip by `COMP_FADE` (at most
    /// half the clip), unless it is audio shared with another placement.
    fn fade(&mut self, clip: ClipId, start: bool) {
        let Some(old) = self.p.clips.get(&clip).cloned() else { return };
        if !matches!(self.p.sources.get(&old.source), Some(Source::Audio(_))) {
            return;
        }
        let users = self.p.tracks.iter().flat_map(|t| t.arrangement.iter()).filter(|pl| pl.clip == clip).count();
        if users != 1 {
            return;
        }
        let length = COMP_FADE.min(old.length / 2);
        let mut new = old.clone();
        let (this, other) = if start { (&mut new.fade_in, new.fade_out.length) } else { (&mut new.fade_out, new.fade_in.length) };
        let length = length.min((old.length - other).max(0));
        if length <= 0 || this.length == length {
            return;
        }
        this.length = length;
        self.run(Command::UpdateClip { id: clip, clip: Box::new(new) });
    }

    /// An independent copy of the part of take placement `pl` inside `a..b`,
    /// placed on the main lane. `None` when the take has nothing there.
    fn promote(&mut self, track: TrackId, pl: &Placement, a: Tick, b: Tick) -> Option<(PlacementId, Tick, Tick)> {
        let len = self.p.clips.get(&pl.clip)?.length;
        let (s, e) = (pl.start.max(a), (pl.start + len).min(b));
        if e - s < ops::MIN_CLIP_LEN {
            return None;
        }
        let (cmds, id) = ops::duplicate_placement(&mut self.p, track, pl.id, pl.start)?;
        self.run_all(cmds);
        self.run(Command::MovePlacement { track, id, to_track: track, start: pl.start, lane: 0 });
        let mut cur = id;
        if pl.start < a {
            let split = ops::split_placement(&mut self.p, track, cur, a)?;
            self.run_all(split.commands);
            self.remove(track, cur);
            cur = split.right_placement;
        }
        if pl.start + len > b {
            let split = ops::split_placement(&mut self.p, track, cur, b)?;
            self.run_all(split.commands);
            self.remove(track, split.right_placement);
        }
        Some((cur, s, e))
    }
}

/// The commands that promote `from..to` of take lane `lane` (0-based) of
/// `track` into the main lane, to apply as one step: what the main lane holds
/// there is replaced (clips cut at the edges and the parts inside removed),
/// the takes' clips in that range are copied in (independent clips, so the
/// take lane stays as recorded), the cut edges get a short fade
/// ([`COMP_FADE`]) against their neighbours, and the comp marks are updated.
///
/// `None` when the range is too short, the lane does not exist or holds
/// nothing there.
pub fn comp_range(p: &mut Project, track: TrackId, lane: usize, from: Tick, to: Tick) -> Option<Vec<Command>> {
    let (a, b) = (from.min(to).max(0), from.max(to));
    if b - a < ops::MIN_CLIP_LEN {
        return None;
    }
    let t = p.track(track)?;
    if lane >= t.take_lanes.len() {
        return None;
    }
    let lane_no = lane as u16 + 1;
    let sources: Vec<Placement> = lane_placements(t, lane)
        .filter(|pl| p.clips.get(&pl.clip).is_some_and(|c| pl.start < b && pl.start + c.length > a))
        .cloned()
        .collect();
    if sources.is_empty() {
        return None;
    }
    let old_comp = t.comp.clone();
    let mut sc = Scratch { p: p.clone(), cmds: Vec::new(), freed: BTreeSet::new(), failed: false };
    sc.clear_main(track, a, b);
    let mut pieces = Vec::new();
    for pl in &sources {
        if let Some(piece) = sc.promote(track, pl, a, b) {
            pieces.push(piece);
        }
    }
    if pieces.is_empty() {
        return None;
    }
    // Fades at the edges of the promoted stretch, against what sits next to it.
    for &(id, s, e) in &pieces {
        let Some(clip) = sc.p.track(track).and_then(|t| t.arrangement.iter().find(|x| x.id == id)).map(|x| x.clip) else { continue };
        if s == a {
            sc.fade(clip, true);
        }
        if e == b {
            sc.fade(clip, false);
        }
    }
    let ends_at = |sc: &Scratch, x: Tick| sc.main(track, |_, y| y == x).into_iter().find(|pl| !pieces.iter().any(|q| q.0 == pl.id));
    let starts_at = |sc: &Scratch, x: Tick| sc.main(track, |y, _| y == x).into_iter().find(|pl| !pieces.iter().any(|q| q.0 == pl.id));
    if let Some(left) = ends_at(&sc, a) {
        sc.fade(left.clip, false);
    }
    if let Some(right) = starts_at(&sc, b) {
        sc.fade(right.clip, true);
    }
    sc.sweep();
    let marks: Vec<CompSpan> = pieces.iter().map(|&(_, start, end)| CompSpan { lane: lane_no, start, end }).collect();
    sc.run(Command::SetComp { track, spans: with_promoted(&old_comp, a, b, &marks) });
    if sc.failed {
        return None;
    }
    p.ids = sc.p.ids.clone();
    Some(sc.cmds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;

    struct Rig {
        p: Project,
        track: TrackId,
    }

    /// An audio track with the main lane holding one 8-beat clip of file A at
    /// 0, and `lanes` take lanes each holding one 8-beat clip of its own file.
    fn rig(lanes: usize) -> Rig {
        let mut p = Project::new("t");
        let track = p.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Audio, "Vox", Color(1))), index: 0 }.apply(&mut p).unwrap();
        let mut r = Rig { p, track };
        for i in 0..=lanes {
            let source = r.p.alloc_source_id();
            let file = AudioSource { path: format!("{i}.wav").into(), hash: None, sample_rate: 48_000, channels: 1, frames: 48_000 * 4, warp: Warp::default(), key: None };
            Command::AddSource { id: source, source: Box::new(Source::Audio(file)) }.apply(&mut r.p).unwrap();
            let (clip, pid) = (r.p.alloc_clip_id(), r.p.alloc_placement_id());
            Command::AddClip { id: clip, clip: Box::new(Clip::new(format!("c{i}"), Color(1), source, 8 * PPQ)) }.apply(&mut r.p).unwrap();
            if i > 0 {
                Command::AddTakeLane { track, index: i - 1, name: format!("Take {i}") }.apply(&mut r.p).unwrap();
            }
            Command::PlaceClip { track, placement: Placement { id: pid, clip, start: 0, lane: i as u16 } }.apply(&mut r.p).unwrap();
        }
        r
    }

    /// Whether two projects hold the same, ignoring the order of placements
    /// that start together and the id allocator.
    fn same(a: &Project, b: &Project) -> bool {
        let norm = |p: &Project| {
            let mut p = p.clone();
            p.ids = IdAllocator::default();
            for t in &mut p.tracks {
                t.arrangement.sort_by_key(|pl| (pl.start, pl.lane, pl.id));
            }
            p
        };
        norm(a) == norm(b)
    }

    fn main_lane(r: &Rig) -> Vec<(Tick, Tick, String)> {
        let t = r.p.track(r.track).unwrap();
        let mut v: Vec<_> = t.arrangement.iter().filter(|pl| pl.lane == 0).map(|pl| (pl.start, pl.start + r.p.clips[&pl.clip].length, r.p.clips[&pl.clip].name.clone())).collect();
        v.sort();
        v
    }

    fn file_of(r: &Rig, start: Tick) -> String {
        let t = r.p.track(r.track).unwrap();
        let pl = t.arrangement.iter().find(|pl| pl.lane == 0 && pl.start == start).unwrap();
        match &r.p.sources[&r.p.clips[&pl.clip].source] {
            Source::Audio(a) => a.path.to_string_lossy().into_owned(),
            Source::Notes(_) => "notes".into(),
        }
    }

    #[test]
    fn lanes_add_rename_move_and_remove_with_exact_undo() {
        let mut r = rig(3);
        let before = r.p.clone();
        let names = |p: &Project| p.track(r.track).unwrap().take_lanes.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&r.p), ["Take 1", "Take 2", "Take 3"]);
        // Lane numbers follow their lane through every change.
        let lane_of = |p: &Project, file: &str| {
            let t = p.track(r.track).unwrap();
            t.arrangement.iter().find(|pl| matches!(&p.sources[&p.clips[&pl.clip].source], Source::Audio(a) if a.path.to_str() == Some(file))).unwrap().lane
        };
        assert_eq!((lane_of(&r.p, "1.wav"), lane_of(&r.p, "3.wav")), (1, 3));
        let undo = Command::MoveTakeLane { track: r.track, from: 0, to: 2 }.apply(&mut r.p).unwrap();
        assert_eq!(names(&r.p), ["Take 2", "Take 3", "Take 1"]);
        assert_eq!((lane_of(&r.p, "1.wav"), lane_of(&r.p, "2.wav"), lane_of(&r.p, "3.wav")), (3, 1, 2));
        undo.apply(&mut r.p).unwrap();
        assert!(same(&r.p, &before));
        let undo = Command::AddTakeLane { track: r.track, index: 1, name: "New".into() }.apply(&mut r.p).unwrap();
        assert_eq!((lane_of(&r.p, "1.wav"), lane_of(&r.p, "2.wav"), lane_of(&r.p, "3.wav")), (1, 3, 4));
        undo.apply(&mut r.p).unwrap();
        assert!(same(&r.p, &before));
        let undo = Command::RenameTakeLane { track: r.track, index: 2, name: "Best".into() }.apply(&mut r.p).unwrap();
        assert_eq!(names(&r.p)[2], "Best");
        undo.apply(&mut r.p).unwrap();
        assert!(same(&r.p, &before));
        // Deleting a lane takes its clips with it; undo brings them all back.
        let undo = Command::RemoveTakeLane { track: r.track, index: 1 }.apply(&mut r.p).unwrap();
        assert_eq!(names(&r.p), ["Take 1", "Take 3"]);
        assert_eq!((lane_of(&r.p, "1.wav"), lane_of(&r.p, "3.wav")), (1, 2));
        assert!(r.p.track(r.track).unwrap().arrangement.len() == 3);
        undo.apply(&mut r.p).unwrap();
        assert!(same(&r.p, &before));
    }

    #[test]
    fn lane_commands_reject_bad_indexes() {
        let mut r = rig(1);
        assert!(Command::RemoveTakeLane { track: r.track, index: 1 }.apply(&mut r.p).is_err());
        assert!(Command::MoveTakeLane { track: r.track, from: 0, to: 1 }.apply(&mut r.p).is_err());
        assert!(Command::AddTakeLane { track: r.track, index: 2, name: "x".into() }.apply(&mut r.p).is_err());
        assert!(Command::SetComp { track: r.track, spans: vec![CompSpan { lane: 5, start: 0, end: 10 }] }.apply(&mut r.p).is_err());
        assert!(Command::RenameTakeLane { track: TrackId(999), index: 0, name: "x".into() }.apply(&mut r.p).is_err());
    }

    #[test]
    fn comping_a_range_replaces_the_main_lane_there_and_keeps_the_take() {
        let mut r = rig(2);
        let mut h = History::default();
        let before = r.p.clone();
        let cmds = comp_range(&mut r.p, r.track, 1, 2 * PPQ, 5 * PPQ).unwrap();
        // The main lane is cut around the promoted range.
        let mut q = before.clone();
        q.ids = r.p.ids.clone();
        h.apply(&mut q, "Comp", crate::history::Origin::User, cmds).unwrap();
        r.p = q;
        let main = main_lane(&r);
        assert_eq!(main.iter().map(|m| (m.0, m.1)).collect::<Vec<_>>(), [(0, 2 * PPQ), (2 * PPQ, 5 * PPQ), (5 * PPQ, 8 * PPQ)]);
        assert_eq!((file_of(&r, 0), file_of(&r, 2 * PPQ), file_of(&r, 5 * PPQ)), ("0.wav".into(), "2.wav".into(), "0.wav".into()));
        // The promoted piece plays the take from the same moment of its file.
        let t = r.p.track(r.track).unwrap();
        let piece = t.arrangement.iter().find(|pl| pl.lane == 0 && pl.start == 2 * PPQ).unwrap();
        assert_eq!(r.p.clips[&piece.clip].offset, 2 * PPQ);
        // Cut edges are joined with short fades.
        assert_eq!(r.p.clips[&piece.clip].fade_in.length, COMP_FADE);
        assert_eq!(r.p.clips[&piece.clip].fade_out.length, COMP_FADE);
        let left = t.arrangement.iter().find(|pl| pl.lane == 0 && pl.start == 0).unwrap();
        let right = t.arrangement.iter().find(|pl| pl.lane == 0 && pl.start == 5 * PPQ).unwrap();
        assert_eq!(r.p.clips[&left.clip].fade_out.length, COMP_FADE);
        assert_eq!(r.p.clips[&right.clip].fade_in.length, COMP_FADE);
        // The take lanes are untouched, and the comp remembers where it came from.
        assert_eq!(t.arrangement.iter().filter(|pl| pl.lane != 0).count(), 2);
        assert_eq!(t.comp, [CompSpan { lane: 2, start: 2 * PPQ, end: 5 * PPQ }]);
        assert_eq!(comp_used(&r.p, r.track, 1), [(2 * PPQ, 5 * PPQ)]);
        assert!(comp_used(&r.p, r.track, 0).is_empty());
        // One undo gives back the project exactly.
        h.undo(&mut r.p).unwrap();
        assert!(same(&r.p, &before));
        h.redo(&mut r.p).unwrap();
        assert_eq!(main_lane(&r).len(), 3);
    }

    #[test]
    fn a_second_promotion_overwrites_part_of_the_first() {
        let mut r = rig(2);
        for (lane, a, b) in [(0, PPQ, 5 * PPQ), (1, 3 * PPQ, 7 * PPQ)] {
            let cmds = comp_range(&mut r.p, r.track, lane, a, b).unwrap();
            Command::Batch { commands: cmds }.apply(&mut r.p).unwrap();
        }
        assert_eq!(main_lane(&r).iter().map(|m| (m.0, m.1)).collect::<Vec<_>>(), [(0, PPQ), (PPQ, 3 * PPQ), (3 * PPQ, 7 * PPQ), (7 * PPQ, 8 * PPQ)]);
        assert_eq!((file_of(&r, PPQ), file_of(&r, 3 * PPQ)), ("1.wav".into(), "2.wav".into()));
        let t = r.p.track(r.track).unwrap();
        assert_eq!(t.comp, [CompSpan { lane: 1, start: PPQ, end: 3 * PPQ }, CompSpan { lane: 2, start: 3 * PPQ, end: 7 * PPQ }]);
        // The clip that lost its place has no user: it and its file are gone.
        assert_eq!(r.p.clips.len(), 6, "the two takes, the main clip and the three new pieces; nothing is left unused");
    }

    #[test]
    fn comp_gives_nothing_for_an_empty_range_a_short_one_or_a_missing_lane() {
        let mut r = rig(1);
        assert!(comp_range(&mut r.p, r.track, 0, 9 * PPQ, 12 * PPQ).is_none(), "the take ends at beat 8");
        assert!(comp_range(&mut r.p, r.track, 0, 2 * PPQ, 2 * PPQ + 3).is_none());
        assert!(comp_range(&mut r.p, r.track, 4, 0, 4 * PPQ).is_none());
        assert!(comp_range(&mut r.p, TrackId(99), 0, 0, 4 * PPQ).is_none());
    }

    #[test]
    fn removing_a_lane_drops_its_comp_marks_and_undo_restores_them() {
        let mut r = rig(2);
        let cmds = comp_range(&mut r.p, r.track, 0, 0, 2 * PPQ).unwrap();
        Command::Batch { commands: cmds }.apply(&mut r.p).unwrap();
        let cmds = comp_range(&mut r.p, r.track, 1, 4 * PPQ, 6 * PPQ).unwrap();
        Command::Batch { commands: cmds }.apply(&mut r.p).unwrap();
        let before = r.p.clone();
        let undo = Command::RemoveTakeLane { track: r.track, index: 0 }.apply(&mut r.p).unwrap();
        assert_eq!(r.p.track(r.track).unwrap().comp, [CompSpan { lane: 1, start: 4 * PPQ, end: 6 * PPQ }]);
        undo.apply(&mut r.p).unwrap();
        assert!(same(&r.p, &before));
    }

    #[test]
    fn notes_takes_are_copied_with_their_own_notes() {
        let mut p = Project::new("t");
        let track = p.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Instrument, "Keys", Color(1))), index: 0 }.apply(&mut p).unwrap();
        Command::AddTakeLane { track, index: 0, name: "Take 1".into() }.apply(&mut p).unwrap();
        let source = p.alloc_source_id();
        let notes = (0..8).map(|i| Note::new(p.alloc_note_id(), 60 + i as u8, i * PPQ, PPQ, 0.8)).collect();
        Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource { notes, ..Default::default() })) }.apply(&mut p).unwrap();
        let (clip, pid) = (p.alloc_clip_id(), p.alloc_placement_id());
        Command::AddClip { id: clip, clip: Box::new(Clip::new("t", Color(1), source, 8 * PPQ)) }.apply(&mut p).unwrap();
        Command::PlaceClip { track, placement: Placement { id: pid, clip, start: 0, lane: 1 } }.apply(&mut p).unwrap();
        let cmds = comp_range(&mut p, track, 0, 2 * PPQ, 4 * PPQ).unwrap();
        Command::Batch { commands: cmds }.apply(&mut p).unwrap();
        let t = p.track(track).unwrap();
        let main: Vec<_> = t.arrangement.iter().filter(|pl| pl.lane == 0).collect();
        assert_eq!(main.len(), 1);
        let c = &p.clips[&main[0].clip];
        assert_eq!((main[0].start, c.length), (2 * PPQ, 2 * PPQ));
        let Some(Source::Notes(n)) = p.sources.get(&c.source) else { panic!("notes") };
        assert_eq!(n.notes.iter().map(|n| n.key).collect::<Vec<_>>(), [62, 63], "only the notes of the stretch, from tick 0");
        assert_eq!(n.notes[0].start, 0);
        assert_ne!(c.source, source, "the copy owns its notes");
        // Notes get no fades.
        assert_eq!((c.fade_in.length, c.fade_out.length), (0, 0));
    }

    #[test]
    fn take_lanes_are_saved_and_a_project_without_them_opens() {
        let mut r = rig(2);
        let cmds = comp_range(&mut r.p, r.track, 1, 0, 3 * PPQ).unwrap();
        Command::Batch { commands: cmds }.apply(&mut r.p).unwrap();
        let json = r.p.to_json().unwrap();
        let back = Project::open_json(&json).unwrap();
        assert_eq!(back.track(r.track).unwrap().take_lanes, r.p.track(r.track).unwrap().take_lanes);
        assert_eq!(back.track(r.track).unwrap().comp, r.p.track(r.track).unwrap().comp);
        // A track with none writes no field, and a file without the fields reads as none.
        let plain = Track::new(TrackId(1), TrackKind::Audio, "A", Color(0));
        let mut val = serde_json::to_value(&plain).unwrap();
        assert!(val.get("take_lanes").is_none() && val.get("comp").is_none());
        val.as_object_mut().unwrap().remove("comp");
        let parsed: Track = serde_json::from_value(val).unwrap();
        assert!(parsed.take_lanes.is_empty() && parsed.comp.is_empty());
    }

    #[test]
    fn next_lane_name_skips_the_names_in_use() {
        let mut t = Track::new(TrackId(1), TrackKind::Audio, "A", Color(0));
        assert_eq!(next_lane_name(&t), "Take 1");
        t.take_lanes = vec![TakeLane { name: "Take 1".into() }, TakeLane { name: "Take 3".into() }];
        assert_eq!(next_lane_name(&t), "Take 2");
    }
}
