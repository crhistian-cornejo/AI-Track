//! The arrangement commands beyond the clipboard: Loop Selection,
//! Consolidate, Insert Silence, Duplicate Time, Insert MIDI Clip, Rename,
//! clip loop and activation, and the track keys (arm, fold, activators).

use super::clipboard::Plan;
use super::{Area, GridView, TimeSel};
use crate::state::AppState;
use aitrack_model::*;

/// Notes of a clip as they are heard: `(ticks from the clip's start, length,
/// note)`, with the loop unrolled over the clip's length.
fn heard<'a>(clip: &Clip, notes: &'a [Note]) -> Vec<(Tick, Tick, &'a Note)> {
    let mut out = Vec::new();
    let length = clip.length.max(0);
    match clip.looping.filter(|l| l.end > l.start) {
        None => {
            for n in notes.iter().filter(|n| n.start >= clip.offset && n.start < clip.offset + length) {
                let at = n.start - clip.offset;
                out.push((at, n.length.min(length - at), n));
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
                        out.push((at, n.length.min(length - at).min(l.end - n.start), n));
                    }
                }
                base += l.end - from;
                from = l.start;
            }
        }
    }
    out
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

    /// Ctrl+J: join the MIDI clips of the selection into one clip per
    /// track, spanning from the first to the last (the time selection when
    /// there is one), with loops written out as notes. Returns how many
    /// clips were made.
    pub fn consolidate(&mut self) -> Result<usize, &'static str> {
        let time = self.time_selection().cloned();
        let mut plan = Plan::new(self);
        // Per track: the range to fill and the clips that go into it.
        let mut groups: Vec<(TrackId, Tick, Tick, Vec<Placement>)> = Vec::new();
        match &time {
            Some(sel) => {
                for track in &sel.tracks {
                    plan.split_edge(*track, sel.start);
                    plan.split_edge(*track, sel.end);
                    let inside = plan.placements(*track, |a, b| a >= sel.start && b <= sel.end);
                    if !inside.is_empty() {
                        groups.push((*track, sel.start, sel.end, inside));
                    }
                }
            }
            None => {
                for (track, pl, len) in self.clip_selection() {
                    match groups.iter_mut().find(|g| g.0 == track) {
                        Some(g) => {
                            g.1 = g.1.min(pl.start);
                            g.2 = g.2.max(pl.start + len);
                            g.3.push(pl);
                        }
                        None => groups.push((track, pl.start, pl.start + len, vec![pl])),
                    }
                }
            }
        }
        if groups.is_empty() {
            return Err("Select the MIDI clips to consolidate");
        }
        let is_notes = |p: &Project, pl: &Placement| p.clips.get(&pl.clip).is_some_and(|c| matches!(p.sources.get(&c.source), Some(Source::Notes(_))));
        if groups.iter().any(|g| g.3.iter().any(|pl| !is_notes(&plan.p, pl))) {
            return Err("Consolidate joins MIDI clips; audio clips need a render");
        }
        let mut made = Vec::new();
        for (track, start, end, parts) in &groups {
            let mut notes = Vec::new();
            let mut first: Option<Clip> = None;
            for pl in parts {
                let Some(clip) = plan.p.clips.get(&pl.clip).cloned() else { continue };
                let Some(Source::Notes(src)) = plan.p.sources.get(&clip.source).cloned() else { continue };
                for (at, len, n) in heard(&clip, &src.notes) {
                    notes.push(Note { id: plan.p.alloc_note_id(), start: pl.start - start + at, length: len, ..n.clone() });
                }
                first.get_or_insert(clip);
                plan.remove(*track, pl);
            }
            let Some(first) = first else { continue };
            let source = plan.p.alloc_source_id();
            let clip = plan.p.alloc_clip_id();
            let id = plan.p.alloc_placement_id();
            let joined = Clip { scale: first.scale, gain_db: first.gain_db, muted: first.muted, ..Clip::new(first.name.clone(), first.color, source, end - start) };
            plan.run(Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource { notes })) });
            plan.run(Command::AddClip { id: clip, clip: Box::new(joined) });
            plan.run(Command::PlaceClip { track: *track, placement: Placement { id, clip, start: *start, lane: 0 } });
            made.push(id);
        }
        if !self.commit("Consolidate", plan) {
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
