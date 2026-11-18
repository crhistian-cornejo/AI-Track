//! Take lanes in the app (Live's comping): which tracks show them, the range
//! being picked on a take lane, the edits (insert, delete, rename, reorder,
//! promote a range into the main lane) and how a loop recording lands in them.
//!
//! The model owns the data and the rules (`aitrack_model::takes`); this is
//! the view state and the glue that turns gestures into one undo step each.

use crate::state::AppState;
use aitrack_model::*;
use std::collections::BTreeSet;

/// A range picked on a take lane, waiting to be promoted into the main lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TakeSel {
    pub track: TrackId,
    /// Index among the track's take lanes.
    pub lane: usize,
    pub start: Tick,
    pub end: Tick,
}

impl TakeSel {
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// What the arrangement shows of take lanes.
#[derive(Clone, Debug, Default)]
pub struct TakeView {
    /// Tracks whose take lanes are shown ("Show Take Lanes").
    pub shown: BTreeSet<TrackId>,
    pub sel: Option<TakeSel>,
}

/// Where a loop recording of audio puts its whole passes: `(loop start, loop
/// length, offset of each pass in the file)`. Only when the loop wrapped at
/// least once after starting inside it (at least two whole passes).
pub fn audio_passes(start: Tick, file: Tick, looped: Option<(Tick, Tick)>) -> Option<(Tick, Tick, Vec<Tick>)> {
    let (ls, le) = looped.filter(|(a, b)| b > a)?;
    if start < ls || start + file <= le {
        return None;
    }
    let (len, first) = (le - ls, le - start);
    let (done, from) = if first == len { (file / len, 0) } else { ((file - first) / len, first) };
    (done >= 2).then(|| (ls, len, (0..done).map(|k| from + k * len).collect()))
}

impl AppState {
    /// Take lanes shown under `track`: how many rows the arrangement draws.
    pub fn shown_takes(&self, track: TrackId) -> usize {
        if !self.takes.shown.contains(&track) {
            return 0;
        }
        self.project.track(track).map_or(0, |t| t.take_lanes.len())
    }

    pub fn take_lanes_shown(&self, track: TrackId) -> bool {
        self.takes.shown.contains(&track)
    }

    /// Show a track's take lanes at open: the ones that hold takes.
    pub(crate) fn show_saved_take_lanes(&mut self) {
        let with: Vec<TrackId> = self.project.tracks.iter().filter(|t| !t.take_lanes.is_empty()).map(|t| t.id).collect();
        self.takes.shown.extend(with);
    }

    pub(crate) fn prune_take_view(&mut self) {
        let p = &self.project;
        // A track shown with no lanes (undo took the only one away) has nothing to show.
        self.takes.shown.retain(|t| p.track(*t).is_some_and(|t| !t.take_lanes.is_empty()));
        if self.takes.sel.is_some_and(|s| p.track(s.track).is_none_or(|t| s.lane >= t.take_lanes.len())) {
            self.takes.sel = None;
        }
    }

    /// Show or hide the take lanes of `track`. Showing them on a track that
    /// has none makes the first, empty one, so there is something to record into.
    /// Returns whether they are shown now.
    pub fn toggle_take_lanes(&mut self, track: TrackId) -> bool {
        if self.takes.shown.remove(&track) {
            self.takes.sel = None;
            return false;
        }
        if !self.project.track(track).is_some_and(|t| t.kind.has_clips()) {
            return false;
        }
        self.takes.shown.insert(track);
        if self.project.track(track).is_some_and(|t| t.take_lanes.is_empty()) {
            self.insert_take_lane(track);
        }
        true
    }

    /// Show the take lanes of all `tracks` (when any is hidden) or hide them
    /// all. The first empty lanes the tracks need are one undo step.
    pub fn toggle_take_lanes_of(&mut self, tracks: &[TrackId]) {
        let tracks: Vec<TrackId> = tracks.iter().copied().filter(|t| self.project.track(*t).is_some_and(|t| t.kind.has_clips())).collect();
        let show = tracks.iter().any(|t| !self.take_lanes_shown(*t));
        let mut cmds = Vec::new();
        for t in tracks {
            if !show {
                self.takes.shown.remove(&t);
                self.takes.sel = None;
            } else if !self.take_lanes_shown(t) {
                self.takes.shown.insert(t);
                if let Some(tr) = self.project.track(t).filter(|tr| tr.take_lanes.is_empty()) {
                    cmds.push(Command::AddTakeLane { track: t, index: 0, name: takes::next_lane_name(tr) });
                }
            }
        }
        if !cmds.is_empty() {
            self.edit("Show take lanes", cmds);
            self.seal();
        }
    }

    /// "Insert Take Lane": a new empty lane after the last one. Shows the lanes.
    pub fn insert_take_lane(&mut self, track: TrackId) -> Option<usize> {
        let t = self.project.track(track).filter(|t| t.kind.has_clips())?;
        let index = t.take_lanes.len();
        let name = takes::next_lane_name(t);
        self.takes.shown.insert(track);
        self.edit("Add take lane", vec![Command::AddTakeLane { track, index, name }]).then_some(index)
    }

    pub fn delete_take_lane(&mut self, track: TrackId, lane: usize) -> bool {
        self.takes.sel = None;
        self.edit("Delete take lane", vec![Command::RemoveTakeLane { track, index: lane }])
    }

    pub fn rename_take_lane(&mut self, track: TrackId, lane: usize, name: String) -> bool {
        self.edit("Rename take lane", vec![Command::RenameTakeLane { track, index: lane, name }])
    }

    /// Move take lane `lane` one place up (`-1`) or down (`1`).
    pub fn shift_take_lane(&mut self, track: TrackId, lane: usize, by: i32) -> bool {
        let n = self.project.track(track).map_or(0, |t| t.take_lanes.len());
        let to = lane as i64 + i64::from(by);
        if lane >= n || to < 0 || to >= n as i64 {
            return false;
        }
        self.takes.sel = None;
        self.edit("Move take lane", vec![Command::MoveTakeLane { track, from: lane, to: to as usize }])
    }

    /// Pick the range `a..b` (either order) of take lane `lane`.
    pub fn select_take_range(&mut self, track: TrackId, lane: usize, a: Tick, b: Tick) {
        self.takes.sel = Some(TakeSel { track, lane, start: a.min(b).max(0), end: a.max(b).max(0) });
    }

    /// Promote `from..to` of take lane `lane` into the main lane as one undo
    /// step ("Comp"). `false` when the take holds nothing there.
    pub fn promote_take(&mut self, track: TrackId, lane: usize, from: Tick, to: Tick) -> bool {
        let Some(cmds) = takes::comp_range(&mut self.project, track, lane, from, to) else { return false };
        self.seal();
        let ok = self.edit("Comp", cmds);
        self.seal();
        ok
    }

    /// Promote the picked range (Enter, or the end of a drag). The pick goes.
    pub fn promote_take_selection(&mut self) -> bool {
        let Some(sel) = self.takes.sel.take() else { return false };
        !sel.is_empty() && self.promote_take(sel.track, sel.lane, sel.start, sel.end)
    }

    /// The whole clip of take lane `lane` under `tick`, as a range.
    pub fn take_clip_span(&self, track: TrackId, lane: usize, tick: Tick) -> Option<(Tick, Tick)> {
        let t = self.project.track(track)?;
        takes::lane_placements(t, lane).find_map(|pl| {
            let len = self.project.clips.get(&pl.clip)?.length;
            (tick >= pl.start && tick < pl.start + len).then_some((pl.start, pl.start + len))
        })
    }

    /// `AITRACK_DEV_VIEW=takes`: a MIDI track with its take lanes shown, three
    /// takes of a four-bar loop, and a comp built from parts of them.
    pub fn seed_demo_takes(&mut self) -> Option<TrackId> {
        let track = self.add_track(TrackKind::Instrument)?;
        self.rename_track(track, "Lead takes");
        self.set_track(track, TrackProp::Height(88.));
        let bar = 4 * PPQ;
        let at = bar;
        let color = self.project.track(track)?.color;
        let (mut cmds, mut place) = (Vec::new(), Vec::new());
        for take in 0..3_i64 {
            cmds.push(Command::AddTakeLane { track, index: take as usize, name: format!("Take {}", take + 1) });
            // Sixteen eighths: the same tune, each take a little different.
            let notes = (0..16_i64)
                .filter(|i| (i * 7 + take * 5) % 11 != 0)
                .map(|i| {
                    let key = [60, 62, 64, 67, 69, 67, 64, 62][((i + take) % 8) as usize] + if (i + take * 3) % 5 == 0 { 12 } else { 0 };
                    Note::new(self.project.alloc_note_id(), key, i * PPQ / 2 * 2 - (i % 4) * (take * 20), PPQ * 3 / 4 + take * 60, 0.6 + 0.1 * take as f32)
                })
                .collect();
            let (source, clip, placement) = (self.project.alloc_source_id(), self.project.alloc_clip_id(), self.project.alloc_placement_id());
            cmds.push(Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource { notes })) });
            cmds.push(Command::AddClip { id: clip, clip: Box::new(Clip::new(format!("Take {}", take + 1), color, source, 4 * bar)) });
            place.push(Command::PlaceClip { track, placement: Placement { id: placement, clip, start: at, lane: take as u16 + 1 } });
        }
        cmds.extend(place);
        self.takes.shown.insert(track);
        if !self.edit("Record takes", cmds) {
            return None;
        }
        self.seal();
        for (lane, a, b) in [(0, at, at + bar), (2, at + bar, at + 2 * bar + 2 * PPQ), (1, at + 2 * bar + 2 * PPQ, at + 4 * bar)] {
            self.promote_take(track, lane, a, b);
        }
        Some(track)
    }

    // ── Recording into take lanes ──

    /// The commands that keep what the main lane holds in `start..end` as a
    /// take lane "Previous" (its clips placed again there), to run before a
    /// recording replaces it. `lanes` is how many take lanes the track will
    /// have by then. Empty when the main lane holds nothing there.
    fn previous_into_lane(&mut self, track: TrackId, start: Tick, end: Tick, lanes: usize) -> Vec<Command> {
        let Some(t) = self.project.track(track) else { return Vec::new() };
        let old: Vec<Placement> = t.arrangement.iter().filter(|pl| pl.lane == 0 && self.project.clips.get(&pl.clip).is_some_and(|c| pl.start < end && pl.start + c.length > start)).cloned().collect();
        if old.is_empty() {
            return Vec::new();
        }
        let mut cmds = vec![Command::AddTakeLane { track, index: lanes, name: "Previous".into() }];
        for pl in old {
            let placement = Placement { id: self.project.alloc_placement_id(), lane: lanes as u16 + 1, ..pl };
            cmds.push(Command::PlaceClip { track, placement });
        }
        cmds
    }

    /// Names for `count` new take lanes after the track's: "Take 3", "Take 4"…
    fn pass_names(&self, track: TrackId, count: usize) -> Vec<String> {
        let mut t = self.project.track(track).cloned().unwrap_or_else(|| Track::new(track, TrackKind::Audio, "", Color(0)));
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let name = takes::next_lane_name(&t);
            t.take_lanes.push(TakeLane { name: name.clone() });
            out.push(name);
        }
        out
    }

    /// A loop recording of audio with the track's take lanes shown: every
    /// whole pass is a take lane (a clip over the loop, at its pass of the
    /// file) and what the main lane held there goes to a "Previous" lane. The
    /// last pass is also what the main lane plays, which the caller places.
    /// `source` is the recorded file's source, already added.
    pub(crate) fn audio_pass_lanes(&mut self, track: TrackId, source: SourceId, color: Color, start: Tick, length: Tick, offsets: &[Tick]) -> Vec<Command> {
        let base = self.project.track(track).map_or(0, |t| t.take_lanes.len());
        let mut cmds = self.previous_into_lane(track, start, start + length, base);
        let base = base + usize::from(!cmds.is_empty());
        for (i, (name, &offset)) in self.pass_names(track, offsets.len()).into_iter().zip(offsets).enumerate() {
            let (clip, placement) = (self.project.alloc_clip_id(), self.project.alloc_placement_id());
            let mut c = Clip::new(name.clone(), color, source, length);
            c.offset = offset;
            cmds.push(Command::AddTakeLane { track, index: base + i, name });
            cmds.push(Command::AddClip { id: clip, clip: Box::new(c) });
            cmds.push(Command::PlaceClip { track, placement: Placement { id: placement, clip, start, lane: (base + i) as u16 + 1 } });
        }
        cmds
    }

    /// The same for a MIDI loop recording: `passes` holds the notes of each
    /// pass (starting at tick 0 of the loop). Each is a take lane; the last
    /// is also placed on the main lane, replacing what was there.
    pub(crate) fn midi_pass_lanes(&mut self, track: TrackId, color: Color, start: Tick, length: Tick, passes: &[Vec<Note>]) -> Vec<Command> {
        let base = self.project.track(track).map_or(0, |t| t.take_lanes.len());
        let mut cmds = self.previous_into_lane(track, start, start + length, base);
        let base = base + usize::from(!cmds.is_empty());
        let names = self.pass_names(track, passes.len());
        let add = |this: &mut Self, name: String, notes: &[Note], lane: u16| {
            let (source, clip, placement) = (this.project.alloc_source_id(), this.project.alloc_clip_id(), this.project.alloc_placement_id());
            let notes = notes.iter().map(|n| Note { id: this.project.alloc_note_id(), ..n.clone() }).collect();
            vec![
                Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource { notes })) },
                Command::AddClip { id: clip, clip: Box::new(Clip::new(name, color, source, length)) },
                Command::PlaceClip { track, placement: Placement { id: placement, clip, start, lane } },
            ]
        };
        for (i, (name, notes)) in names.into_iter().zip(passes).enumerate() {
            cmds.push(Command::AddTakeLane { track, index: base + i, name: name.clone() });
            cmds.extend(add(self, name, notes, (base + i) as u16 + 1));
        }
        if let Some(last) = passes.last() {
            cmds.extend(add(self, "Recorded".into(), last, 0));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::PPQ;

    fn audio_state() -> (AppState, TrackId) {
        let mut s = AppState::new(Project::new("t"));
        let t = s.add_track(TrackKind::Audio).expect("track");
        (s, t)
    }

    #[test]
    fn passes_of_a_loop_recording_follow_the_file() {
        let (ls, le) = (4 * PPQ, 8 * PPQ);
        // Started at the loop start: three whole passes in twelve beats.
        assert_eq!(audio_passes(ls, 12 * PPQ, Some((ls, le))), Some((ls, 4 * PPQ, vec![0, 4 * PPQ, 8 * PPQ])));
        // Started a beat in: the partial first pass is dropped.
        assert_eq!(audio_passes(5 * PPQ, 3 * PPQ + 8 * PPQ, Some((ls, le))), Some((ls, 4 * PPQ, vec![3 * PPQ, 7 * PPQ])));
        // No loop, an intro before it, one pass or less: not takes.
        assert_eq!(audio_passes(ls, 12 * PPQ, None), None);
        assert_eq!(audio_passes(0, 20 * PPQ, Some((ls, le))), None);
        assert_eq!(audio_passes(ls, 6 * PPQ, Some((ls, le))), None);
        assert_eq!(audio_passes(ls, 4 * PPQ, Some((ls, le))), None);
    }

    #[test]
    fn showing_take_lanes_on_several_tracks_is_one_undo_step() {
        let (mut s, a) = audio_state();
        let b = s.project.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(b, TrackKind::Audio, "B", Color::palette(2))), index: 1 }.apply(&mut s.project).unwrap();
        let before = s.history.steps().count();
        s.toggle_take_lanes_of(&[a, b]);
        assert_eq!(s.history.steps().count(), before + 1);
        assert!(s.take_lanes_shown(a) && s.take_lanes_shown(b));
        assert_eq!((s.project.track(a).unwrap().take_lanes.len(), s.project.track(b).unwrap().take_lanes.len()), (1, 1));
        assert!(s.undo());
        assert_eq!((s.project.track(a).unwrap().take_lanes.len(), s.project.track(b).unwrap().take_lanes.len()), (0, 0));
        assert!(!s.take_lanes_shown(a) && !s.take_lanes_shown(b), "nothing is shown with no lanes");
    }

    #[test]
    fn showing_take_lanes_on_a_bare_track_makes_the_first_lane_and_hiding_keeps_them() {
        let (mut s, t) = audio_state();
        assert_eq!(s.shown_takes(t), 0);
        assert!(s.toggle_take_lanes(t));
        assert_eq!(s.shown_takes(t), 1);
        assert!(s.insert_take_lane(t).is_some());
        assert_eq!(s.shown_takes(t), 2);
        assert_eq!(s.project.track(t).unwrap().take_lanes.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Take 1", "Take 2"]);
        assert!(!s.toggle_take_lanes(t));
        assert_eq!((s.shown_takes(t), s.project.track(t).unwrap().take_lanes.len()), (0, 2));
        // Back on: no third lane, the saved ones return.
        assert!(s.toggle_take_lanes(t));
        assert_eq!(s.project.track(t).unwrap().take_lanes.len(), 2);
        // Undo takes the lane away again.
        assert!(s.undo());
        assert_eq!(s.project.track(t).unwrap().take_lanes.len(), 1);
    }

    #[test]
    fn a_loop_recording_with_take_lanes_shown_makes_a_lane_per_pass_and_keeps_the_old_main() {
        let (mut s, t) = audio_state();
        s.project.track_mut(t).unwrap().input = InputSource::Hardware { first: 0, stereo: false };
        s.project.loop_region = Some((4 * PPQ, 8 * PPQ));
        // An earlier clip sits on the loop.
        let (src, clip, pl) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
        let old = AudioSource { path: "old.wav".into(), hash: None, sample_rate: 48_000, channels: 1, frames: 96_000, warp: Warp::default(), key: None };
        assert!(s.edit(
            "old",
            vec![
                Command::AddSource { id: src, source: Box::new(Source::Audio(old)) },
                Command::AddClip { id: clip, clip: Box::new(Clip::new("old", Color(0), src, 4 * PPQ)) },
                Command::PlaceClip { track: t, placement: Placement { id: pl, clip, start: 4 * PPQ, lane: 0 } },
            ]
        ));
        s.toggle_take_lanes(t);
        // Three whole passes of the 4-beat loop at 120 BPM and 48 kHz: 6 s.
        let take = crate::bridge::AudioTake { track: t, path: "/rec/a.wav".into(), start_tick: 4 * PPQ, frames: 48_000 * 6, sample_rate: 48_000, channels: 1, error: None };
        let (clips, errors) = s.commit_audio_takes(&[take], s.project.loop_region);
        assert_eq!((clips, errors.len()), (1, 0));
        let tr = s.project.track(t).unwrap();
        // "Take 1" was the empty lane made by showing; then Previous and one lane per whole pass.
        assert_eq!(tr.take_lanes.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Take 1", "Previous", "Take 2", "Take 3", "Take 4"]);
        let main: Vec<_> = tr.arrangement.iter().filter(|p| p.lane == 0).collect();
        assert_eq!(main.len(), 1, "one clip plays: the last pass");
        let lane_clip = |lane: u16| tr.arrangement.iter().filter(|p| p.lane == lane).map(|p| (p.start, s.project.clips[&p.clip].offset, s.project.clips[&p.clip].length)).collect::<Vec<_>>();
        assert_eq!(lane_clip(1), []);
        assert_eq!(lane_clip(2), [(4 * PPQ, 0, 4 * PPQ)], "what the main lane held");
        // The recorded file is 12 beats: passes at 0, 4 and 8 beats of it.
        assert_eq!(lane_clip(3), [(4 * PPQ, 0, 4 * PPQ)]);
        assert_eq!(lane_clip(4), [(4 * PPQ, 4 * PPQ, 4 * PPQ)]);
        assert_eq!(lane_clip(5), [(4 * PPQ, 8 * PPQ, 4 * PPQ)]);
        assert_eq!(s.project.clips[&main[0].clip].offset, 8 * PPQ, "the main lane plays the last pass");
        // One undo gives all of it back.
        assert!(s.undo());
        let tr = s.project.track(t).unwrap();
        assert_eq!((tr.take_lanes.len(), tr.arrangement.len()), (1, 1));
    }

    #[test]
    fn hidden_take_lanes_keep_the_old_behaviour() {
        let (mut s, t) = audio_state();
        s.project.track_mut(t).unwrap().input = InputSource::Hardware { first: 0, stereo: false };
        s.project.loop_region = Some((4 * PPQ, 8 * PPQ));
        let take = crate::bridge::AudioTake { track: t, path: "/rec/a.wav".into(), start_tick: 4 * PPQ, frames: 48_000 * 6, sample_rate: 48_000, channels: 1, error: None };
        s.commit_audio_takes(&[take], s.project.loop_region);
        let tr = s.project.track(t).unwrap();
        assert!(tr.take_lanes.is_empty());
        assert_eq!(tr.arrangement.len(), 1);
    }

    #[test]
    fn promoting_a_range_is_one_undo_step() {
        let (mut s, t) = audio_state();
        s.toggle_take_lanes(t);
        let (src, clip, pl) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
        let file = AudioSource { path: "a.wav".into(), hash: None, sample_rate: 48_000, channels: 1, frames: 96_000, warp: Warp::default(), key: None };
        assert!(s.edit(
            "take",
            vec![
                Command::AddSource { id: src, source: Box::new(Source::Audio(file)) },
                Command::AddClip { id: clip, clip: Box::new(Clip::new("take", Color(0), src, 4 * PPQ)) },
                Command::PlaceClip { track: t, placement: Placement { id: pl, clip, start: 0, lane: 1 } },
            ]
        ));
        s.select_take_range(t, 0, 3 * PPQ, PPQ);
        assert_eq!(s.take_clip_span(t, 0, PPQ), Some((0, 4 * PPQ)));
        assert!(s.promote_take_selection());
        assert!(s.takes.sel.is_none());
        let tr = s.project.track(t).unwrap();
        assert_eq!(tr.arrangement.iter().filter(|p| p.lane == 0).map(|p| p.start).collect::<Vec<_>>(), [PPQ]);
        assert_eq!(tr.comp, [CompSpan { lane: 1, start: PPQ, end: 3 * PPQ }]);
        assert!(s.undo());
        let tr = s.project.track(t).unwrap();
        assert!(tr.arrangement.iter().all(|p| p.lane != 0) && tr.comp.is_empty());
        // Nothing to promote where the take has no audio.
        s.select_take_range(t, 0, 6 * PPQ, 7 * PPQ);
        assert!(!s.promote_take_selection());
    }

    #[test]
    fn the_dev_view_seeds_three_takes_and_a_comp() {
        let mut s = AppState::new(Project::new("t"));
        let t = s.seed_demo_takes().expect("seeded");
        assert_eq!(s.shown_takes(t), 3);
        let tr = s.project.track(t).unwrap();
        assert_eq!(tr.comp.iter().map(|c| c.lane).collect::<Vec<_>>(), [1, 3, 2]);
        assert_eq!(tr.arrangement.iter().filter(|p| p.lane == 0).count(), 3);
        // Every take lane still holds its whole take.
        assert!((1..=3).all(|l| tr.arrangement.iter().filter(|p| p.lane == l).count() == 1));
    }
}
