//! The arrangement commands beyond the clipboard: Loop Selection,
//! Consolidate, Insert Silence, Duplicate Time, Insert MIDI Clip, Rename,
//! clip loop and activation, and the track keys (arm, fold, activators).

use super::clipboard::Plan;
use super::{Area, GridView, TimeSel};
use crate::state::AppState;
use aitrack_model::*;

/// One track's share of a Consolidate: the range it fills and the clips
/// that go into it, earliest first.
#[derive(Clone, Debug, PartialEq)]
pub struct ConsolidateGroup {
    pub track: TrackId,
    pub start: Tick,
    pub end: Tick,
    pub parts: Vec<Placement>,
    /// Audio clips, which need a render before they can be joined.
    pub audio: bool,
}

/// What a Consolidate acts on, taken when it starts.
#[derive(Clone, Debug, PartialEq)]
pub enum ConsolidatePick {
    Time(TimeSel),
    Clips(Vec<(TrackId, Placement, Tick)>),
}

/// A Consolidate with audio in it, waiting for its files: render each audio
/// group of `project` (the song with the time selection's edges cut) off the
/// UI thread, then hand the files to [`AppState::finish_consolidate`].
#[derive(Clone, Debug)]
pub struct ConsolidateJob {
    pub pick: ConsolidatePick,
    pub project: Project,
    pub groups: Vec<ConsolidateGroup>,
}

/// How a Consolidate went.
#[derive(Debug)]
pub enum Consolidation {
    /// MIDI only: done, this many clips made.
    Done(usize),
    /// Audio is in it: render the job, then finish it.
    Render(Box<ConsolidateJob>),
}

/// What a group's clips are, to tell whether the song changed during a render.
fn content(p: &Project, group: &ConsolidateGroup) -> Vec<(Tick, Option<Clip>, Option<Source>)> {
    group
        .parts
        .iter()
        .map(|pl| {
            let clip = p.clips.get(&pl.clip).cloned();
            let source = clip.as_ref().and_then(|c| p.sources.get(&c.source).cloned());
            (pl.start, clip, source)
        })
        .collect()
}

impl AppState {
    /// Ctrl+L: put the loop brace on the time selection, else on the span
    /// of the selected clips. `false` when nothing is selected (the caller
    /// then toggles the loop).
    pub fn loop_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_span().filter(|(a, b)| b > a) else { return false };
        self.seal();
        let ok = self.edit("Set loop", vec![Command::SetLoop { region: Some((start, end)) }]);
        self.seal();
        ok
    }

    /// Ctrl+J: join the clips of the selection into one clip per track,
    /// spanning from the first to the last (the time selection when there is
    /// one), gaps included. MIDI is joined at once, with loops written out
    /// as notes. Audio has to be rendered to a new file first: the result
    /// is then a job for [`Self::finish_consolidate`].
    pub fn consolidate(&mut self) -> Result<Consolidation, &'static str> {
        let pick = match self.time_selection() {
            Some(sel) => ConsolidatePick::Time(sel.clone()),
            None => ConsolidatePick::Clips(self.clip_selection()),
        };
        let (plan, groups) = self.consolidate_plan(&pick)?;
        if groups.iter().any(|g| g.audio) {
            return Ok(Consolidation::Render(Box::new(ConsolidateJob { pick, project: plan.p, groups })));
        }
        self.apply_consolidate(plan, &groups, &[]).map(Consolidation::Done)
    }

    /// Finish a Consolidate with audio in it, once `files[i]` (for each
    /// audio group `i` of the job) holds what it rendered. Refused when the
    /// clips changed in the meantime.
    pub fn finish_consolidate(&mut self, job: &ConsolidateJob, files: &[(usize, AudioSource)]) -> Result<usize, &'static str> {
        const CHANGED: &str = "The clips changed while they were being rendered: consolidate again";
        let (plan, groups) = self.consolidate_plan(&job.pick).map_err(|_| CHANGED)?;
        let same = groups.len() == job.groups.len()
            && groups.iter().zip(&job.groups).all(|(a, b)| (a.track, a.start, a.end, a.audio) == (b.track, b.start, b.end, b.audio) && content(&plan.p, a) == content(&job.project, b));
        if !same {
            return Err(CHANGED);
        }
        self.apply_consolidate(plan, &groups, files)
    }

    /// Cut the time selection's edges (on a scratch copy) and gather what
    /// each track joins.
    fn consolidate_plan(&self, pick: &ConsolidatePick) -> Result<(Plan, Vec<ConsolidateGroup>), &'static str> {
        let mut plan = Plan::new(self);
        let mut groups: Vec<ConsolidateGroup> = Vec::new();
        match pick {
            ConsolidatePick::Time(sel) => {
                for track in &sel.tracks {
                    plan.split_edge(*track, sel.start);
                    plan.split_edge(*track, sel.end);
                    let parts = plan.placements(*track, |a, b| a >= sel.start && b <= sel.end);
                    if !parts.is_empty() {
                        groups.push(ConsolidateGroup { track: *track, start: sel.start, end: sel.end, parts, audio: false });
                    }
                }
            }
            ConsolidatePick::Clips(clips) => {
                for (track, pl, len) in clips {
                    if plan.p.track(*track).is_none_or(|t| !t.arrangement.iter().any(|x| x == pl)) {
                        return Err("The selected clips are gone");
                    }
                    match groups.iter_mut().find(|g| g.track == *track) {
                        Some(g) => {
                            g.start = g.start.min(pl.start);
                            g.end = g.end.max(pl.start + len);
                            g.parts.push(pl.clone());
                        }
                        None => groups.push(ConsolidateGroup { track: *track, start: pl.start, end: pl.start + len, parts: vec![pl.clone()], audio: false }),
                    }
                }
            }
        }
        if groups.is_empty() {
            return Err("Select the clips to consolidate");
        }
        for g in &mut groups {
            g.parts.sort_by_key(|pl| (pl.start, pl.id));
            let kinds: Vec<bool> = g.parts.iter().map(|pl| plan.p.clips.get(&pl.clip).is_some_and(|c| matches!(plan.p.sources.get(&c.source), Some(Source::Audio(_))))).collect();
            g.audio = kinds.first().copied().unwrap_or(false);
            if kinds.iter().any(|k| *k != g.audio) {
                return Err("A track mixes MIDI and audio clips: consolidate them apart");
            }
        }
        Ok((plan, groups))
    }

    /// Join every group in `plan` as one undo step: MIDI through the model's
    /// notes join, audio onto its rendered file. Selects what was made.
    fn apply_consolidate(&mut self, mut plan: Plan, groups: &[ConsolidateGroup], files: &[(usize, AudioSource)]) -> Result<usize, &'static str> {
        let mut made = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            let ids: Vec<PlacementId> = g.parts.iter().map(|pl| pl.id).collect();
            let done = if g.audio {
                let Some((_, file)) = files.iter().find(|(at, _)| *at == i) else { return Err("An audio clip was not rendered") };
                ops::consolidate_audio(&mut plan.p, g.track, &ids, g.start, g.end, file.clone())
            } else {
                ops::consolidate_notes(&mut plan.p, g.track, &ids, g.start, g.end)
            };
            let Some(done) = done else { continue };
            plan.run_all(done.commands);
            made.push(done.placement);
        }
        if made.is_empty() || !self.commit("Consolidate", plan) {
            return Err("Nothing to consolidate");
        }
        let count = made.len();
        let last = made.last().copied();
        self.select_clips(made, last);
        Ok(count)
    }

    /// Insert Silence: empty time the length of the time selection, on
    /// every track, at its start. What was there moves later.
    pub fn insert_silence(&mut self) -> bool {
        let Some((start, end)) = self.time_selection().map(|s| (s.start, s.end)) else { return false };
        let mut plan = Plan::new(self);
        plan.open_gap(start, end - start);
        self.commit("Insert silence", plan)
    }

    /// Ctrl+Shift+D: repeat the selected time on every track, pushing what
    /// follows later. The selection moves onto the copy.
    pub fn duplicate_time(&mut self) -> bool {
        let Some(sel) = self.time_selection().cloned() else { return false };
        let len = sel.end - sel.start;
        let mut plan = Plan::new(self);
        let tracks: Vec<TrackId> = plan.p.tracks.iter().filter(|t| t.kind.has_clips()).map(|t| t.id).collect();
        let lifted: Vec<_> = tracks.iter().map(|t| (*t, plan.lift(*t, sel.start, sel.end))).collect();
        plan.open_gap(sel.end, len);
        for (track, seeds) in &lifted {
            for s in seeds {
                plan.plant(s, *track, sel.end + s.at);
            }
        }
        if !self.commit("Duplicate time", plan) {
            return false;
        }
        self.ed.marker = sel.end;
        self.ed.time = Some(TimeSel { start: sel.end, end: sel.end + len, anchor: (sel.end, sel.anchor.1), ..sel });
        true
    }

    /// Ctrl+Shift+M: an empty MIDI clip on the selected instrument track,
    /// filling the time selection when it covers that track, else one bar
    /// at the insert marker. What was there is replaced.
    pub fn insert_midi_clip(&mut self) -> Option<ClipId> {
        let track = self.selected_track.and_then(|t| self.project.track(t)).filter(|t| t.kind == TrackKind::Instrument)?;
        let (track, color) = (track.id, track.color);
        let bar = self.grid_spec(GridView::Arrange).bar;
        let (start, end) = match self.time_selection().filter(|s| s.tracks.contains(&track)) {
            Some(sel) => (sel.start, sel.end),
            None => (self.ed.marker, self.ed.marker + bar),
        };
        let mut plan = Plan::new(self);
        plan.clear(track, start, end);
        let source = plan.p.alloc_source_id();
        let clip = plan.p.alloc_clip_id();
        let id = plan.p.alloc_placement_id();
        plan.run(Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource::default())) });
        plan.run(Command::AddClip { id: clip, clip: Box::new(Clip::new("MIDI Clip", color, source, end - start)) });
        plan.run(Command::PlaceClip { track, placement: Placement { id, clip, start, lane: 0 } });
        if !self.commit("Insert MIDI clip", plan) {
            return None;
        }
        self.ed.area = Area::Arrange;
        self.select_clips([id], Some(id));
        Some(clip)
    }

    pub fn rename_clip(&mut self, clip: ClipId, name: &str) -> bool {
        let name = name.trim();
        let Some(old) = self.project.clips.get(&clip).filter(|c| !name.is_empty() && c.name != name) else { return false };
        let new = Clip { name: name.to_owned(), ..old.clone() };
        self.seal();
        let ok = self.edit("Rename clip", vec![Command::UpdateClip { id: clip, clip: Box::new(new) }]);
        self.seal();
        ok
    }

    pub fn rename_track(&mut self, track: TrackId, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.project.track(track).is_none_or(|t| t.name == name) {
            return false;
        }
        self.seal();
        let ok = self.set_track(track, TrackProp::Name(name.to_owned()));
        self.seal();
        ok
    }

    /// The clips behind the selected placements, each once.
    fn selected_clips(&self) -> Vec<ClipId> {
        let mut ids: Vec<ClipId> = self.clip_selection().into_iter().map(|(_, pl, _)| pl.clip).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Switch the selected clips off, or back on when all of them are off.
    /// `None` when no clip is selected.
    pub fn toggle_clips_active(&mut self) -> Option<bool> {
        let ids = self.selected_clips();
        let muted = ids.iter().any(|id| self.project.clips.get(id).is_some_and(|c| !c.muted));
        let cmds: Vec<Command> = ids
            .iter()
            .filter_map(|id| {
                let c = self.project.clips.get(id).filter(|c| c.muted != muted)?;
                Some(Command::UpdateClip { id: *id, clip: Box::new(Clip { muted, ..c.clone() }) })
            })
            .collect();
        if cmds.is_empty() {
            return None;
        }
        self.seal();
        let ok = self.edit(if muted { "Deactivate clips" } else { "Activate clips" }, cmds);
        self.seal();
        ok.then_some(muted)
    }

    /// Loop the selected clips over what they show, or stop them looping
    /// (all of them follow the first). `None` when no clip is selected.
    pub fn toggle_clips_loop(&mut self) -> Option<bool> {
        let ids = self.selected_clips();
        let on = self.project.clips.get(ids.first()?)?.looping.is_none();
        let cmds: Vec<Command> = ids
            .iter()
            .filter(|id| self.project.clips.get(id).is_some_and(|c| c.looping.is_none() == on))
            .filter_map(|id| ops::toggle_loop(&self.project, *id))
            .collect();
        if cmds.is_empty() {
            return None;
        }
        self.seal();
        let ok = self.edit(if on { "Loop clip" } else { "Unloop clip" }, cmds);
        self.seal();
        ok.then_some(on)
    }

    // ── Track keys ──

    /// C: arm the selected track, or disarm it.
    pub fn toggle_arm(&mut self) -> Option<bool> {
        let t = self.selected_track.and_then(|t| self.project.track(t)).filter(|t| t.kind.has_clips())?;
        let (id, arm) = (t.id, !t.arm);
        self.seal();
        let ok = self.set_track(id, TrackProp::Arm(arm));
        self.seal();
        ok.then_some(arm)
    }

    /// S: fold the selected group, or the group the selected track is in.
    pub fn toggle_fold(&mut self) -> Option<bool> {
        let sel = self.selected_track.and_then(|t| self.project.track(t))?;
        let group = if sel.kind == TrackKind::Group { sel } else { self.project.track(sel.parent?)? };
        let (id, collapsed) = (group.id, !group.collapsed);
        self.seal();
        let ok = self.set_track(id, TrackProp::Collapsed(collapsed));
        self.seal();
        if ok {
            // A track hidden by the fold cannot stay selected.
            self.selected_track = Some(id);
        }
        ok.then_some(collapsed)
    }

    /// F1–F8: the activator of the `n`-th track on screen (0-based).
    /// Returns the track's name and whether it now plays.
    pub fn toggle_activator(&mut self, n: usize) -> Option<(String, bool)> {
        let id = *self.lane_tracks().get(n)?;
        let t = self.project.track(id)?;
        let (name, mute) = (t.name.clone(), !t.mute);
        self.seal();
        let ok = self.set_track(id, TrackProp::Mute(mute));
        self.seal();
        ok.then_some((name, !mute))
    }
}
