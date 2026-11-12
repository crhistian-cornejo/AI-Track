//! Cut, Copy, Paste, Duplicate and Delete for arrangement clips and notes.
//!
//! Clips travel as [`Seed`]s: a copy of the clip (fades, loop, envelopes and
//! gain included) with a snapshot of its source, so a paste still works after
//! the original was deleted. Edits that take several steps (cut a range, then
//! place clips in it) are worked out on a scratch copy of the project in a
//! [`Plan`] and applied as one undo step.

use super::{Area, GridView, ceil_to};
use crate::state::AppState;
use aitrack_model::*;
use std::collections::BTreeSet;

/// A clip lifted out of the project.
#[derive(Clone, Debug, PartialEq)]
pub struct Seed {
    pub clip: Clip,
    /// The source as it was when copied.
    pub source: Source,
    /// The id `source` had: pasted audio shares it while it still exists.
    pub source_id: SourceId,
    /// Start, in ticks from the start of the copied range.
    pub at: Tick,
}

/// The clips copied from one track.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub track: TrackId,
    pub kind: TrackKind,
    /// The track's place among the tracks that hold clips, to keep the
    /// rows the same distance apart when pasting elsewhere.
    pub lane: usize,
    pub seeds: Vec<Seed>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Clipboard {
    /// Notes with starts relative to the earliest one.
    Notes { span: Tick, notes: Vec<Note> },
    /// Clips of a time range `span` ticks long, one row per track.
    Clips { span: Tick, rows: Vec<Row> },
}

/// A multi-step edit worked out on a scratch copy of the project.
pub(super) struct Plan {
    pub p: Project,
    pub cmds: Vec<Command>,
    /// Clips whose placements were removed; dropped at the end when nothing uses them.
    freed: BTreeSet<ClipId>,
    failed: bool,
}

impl Plan {
    pub fn new(s: &AppState) -> Self {
        Self { p: s.project.clone(), cmds: Vec::new(), freed: BTreeSet::new(), failed: false }
    }

    pub fn run(&mut self, cmd: Command) {
        match cmd.clone().apply(&mut self.p) {
            Ok(_) => self.cmds.push(cmd),
            Err(_) => self.failed = true,
        }
    }

    pub fn run_all(&mut self, cmds: impl IntoIterator<Item = Command>) {
        for c in cmds {
            self.run(c);
        }
    }

    /// The placements of `track` that satisfy `keep` (start, end).
    pub fn placements(&self, track: TrackId, keep: impl Fn(Tick, Tick) -> bool) -> Vec<Placement> {
        let Some(t) = self.p.track(track) else { return Vec::new() };
        t.arrangement
            .iter()
            .filter(|pl| self.p.clips.get(&pl.clip).is_some_and(|c| keep(pl.start, pl.start + c.length)))
            .cloned()
            .collect()
    }

    /// Cut every clip of `track` that crosses `edge`. Returns how many were cut.
    pub fn split_edge(&mut self, track: TrackId, edge: Tick) -> usize {
        let mut cut = 0;
        for pl in self.placements(track, |a, b| a < edge && edge < b) {
            if let Some(cmds) = ops::split_at(&mut self.p, track, pl.id, edge) {
                self.run_all(cmds);
                cut += 1;
            }
        }
        cut
    }

    pub fn remove(&mut self, track: TrackId, pl: &Placement) {
        self.freed.insert(pl.clip);
        self.run(Command::RemovePlacement { track, id: pl.id });
    }

    /// Empty `a..b` on `track`: clips inside go, clips crossing an edge are cut there.
    pub fn clear(&mut self, track: TrackId, a: Tick, b: Tick) {
        if b <= a {
            return;
        }
        self.split_edge(track, a);
        self.split_edge(track, b);
        for pl in self.placements(track, |x, y| x >= a && y <= b) {
            self.remove(track, &pl);
        }
    }

    /// The clips inside `a..b` on `track` (cut at its edges first), as seeds.
    pub fn lift(&mut self, track: TrackId, a: Tick, b: Tick) -> Vec<Seed> {
        self.split_edge(track, a);
        self.split_edge(track, b);
        self.placements(track, |x, y| x >= a && y <= b).iter().filter_map(|pl| seed(&self.p, pl, a)).collect()
    }

    /// Put a seed on `track` at `start`. Notes get a source of their own;
    /// audio shares the file's source while the project still has it.
    pub fn plant(&mut self, seed: &Seed, track: TrackId, start: Tick) -> PlacementId {
        let mut clip = seed.clip.clone();
        match &seed.source {
            Source::Notes(n) => {
                let id = self.p.alloc_source_id();
                let notes = n.notes.iter().map(|x| Note { id: self.p.alloc_note_id(), ..x.clone() }).collect();
                self.run(Command::AddSource { id, source: Box::new(Source::Notes(NoteSource { notes })) });
                clip.source = id;
            }
            Source::Audio(a) => {
                let shared = matches!(self.p.sources.get(&seed.source_id), Some(Source::Audio(b)) if b.path == a.path);
                if !shared {
                    let id = self.p.alloc_source_id();
                    self.run(Command::AddSource { id, source: Box::new(seed.source.clone()) });
                    clip.source = id;
                }
            }
        }
        let clip_id = self.p.alloc_clip_id();
        let id = self.p.alloc_placement_id();
        self.run(Command::AddClip { id: clip_id, clip: Box::new(clip) });
        self.run(Command::PlaceClip { track, placement: Placement { id, clip: clip_id, start: start.max(0), lane: 0 } });
        id
    }

    /// Push everything from `at` on `len` ticks later on every track that
    /// holds clips (clips crossing `at` are cut there); track automation
    /// from `at` on moves with it.
    pub fn open_gap(&mut self, at: Tick, len: Tick) {
        let tracks: Vec<TrackId> = self.p.tracks.iter().filter(|t| t.kind.has_clips()).map(|t| t.id).collect();
        for track in tracks {
            self.split_edge(track, at);
            for pl in self.placements(track, |x, _| x >= at) {
                self.run(Command::MovePlacement { track, id: pl.id, to_track: track, start: pl.start + len, lane: pl.lane });
            }
        }
        let lanes: Vec<(TrackId, AutomationLane)> = self
            .p
            .tracks
            .iter()
            .flat_map(|t| t.automation.iter().filter(|l| l.points.iter().any(|pt| pt.tick >= at)).map(|l| (t.id, l.clone())))
            .collect();
        for (track, mut lane) in lanes {
            for pt in lane.points.iter_mut().filter(|pt| pt.tick >= at) {
                pt.tick += len;
            }
            self.run(Command::SetAutomation { track, target: lane.target, lane: Some(lane) });
        }
    }

    /// Remove the clips (and then the sources) the plan left without a user.
    fn sweep(&mut self) {
        let mut sources = BTreeSet::new();
        for id in std::mem::take(&mut self.freed) {
            let Some(clip) = self.p.clips.get(&id) else { continue };
            let used = self.p.tracks.iter().any(|t| t.arrangement.iter().any(|pl| pl.clip == id) || t.session_slots.values().any(|c| *c == id));
            if !used {
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
}

/// A placed clip as a seed, `at` relative to `origin`.
fn seed(p: &Project, pl: &Placement, origin: Tick) -> Option<Seed> {
    let clip = p.clips.get(&pl.clip)?.clone();
    let source = p.sources.get(&clip.source)?.clone();
    Some(Seed { source_id: clip.source, clip, source, at: pl.start - origin })
}

/// The kind of track a source can sit on.
fn kind_of(source: &Source) -> TrackKind {
    match source {
        Source::Notes(_) => TrackKind::Instrument,
        Source::Audio(_) => TrackKind::Audio,
    }
}

impl AppState {
    /// Apply a plan as one undo step. `false` when it has nothing to do or
    /// one of its steps did not apply.
    pub(super) fn commit(&mut self, label: &str, mut plan: Plan) -> bool {
        plan.sweep();
        if plan.failed || plan.cmds.is_empty() {
            return false;
        }
        // The ids the plan handed out are taken.
        self.project.ids = plan.p.ids.clone();
        self.seal();
        let ok = self.edit(label, plan.cmds);
        self.seal();
        self.prune_selection();
        ok
    }

    /// The tracks that hold clips, in project order.
    fn clip_tracks(&self) -> Vec<TrackId> {
        self.project.tracks.iter().filter(|t| t.kind.has_clips()).map(|t| t.id).collect()
    }

    fn row(&self, track: TrackId, seeds: Vec<Seed>) -> Option<Row> {
        let kind = self.project.track(track)?.kind;
        let lane = self.clip_tracks().iter().position(|t| *t == track)?;
        Some(Row { track, kind, lane, seeds })
    }

    /// What Copy would take from the arrangement: the parts of the clips
    /// inside the time selection, else the selected clips.
    fn lift_selection(&self) -> Option<(Tick, Tick, Vec<Row>)> {
        if let Some(sel) = self.time_selection() {
            let mut scratch = Plan::new(self);
            let rows = sel.tracks.iter().filter_map(|t| self.row(*t, scratch.lift(*t, sel.start, sel.end))).collect();
            return Some((sel.start, sel.end, rows));
        }
        let (start, end) = self.selection_span()?;
        let mut rows: Vec<Row> = Vec::new();
        for (track, pl, _) in self.clip_selection() {
            let Some(s) = seed(&self.project, &pl, start) else { continue };
            match rows.iter_mut().find(|r| r.track == track) {
                Some(row) => row.seeds.push(s),
                None => rows.extend(self.row(track, vec![s])),
            }
        }
        Some((start, end, rows))
    }

    /// The selected notes of the open clip when the piano roll has the keys.
    fn note_selection(&self) -> Option<(ClipId, SourceId, Vec<Note>)> {
        let clip = self.selected_clip.filter(|_| self.ed.area == Area::Piano)?;
        let (source, n) = self.clip_notes(clip)?;
        let sel: Vec<Note> = n.notes.iter().filter(|n| self.selected_notes.contains(&n.id)).cloned().collect();
        (!sel.is_empty()).then_some((clip, source, sel))
    }

    /// Ctrl+C. Notes when the piano roll has the keys and notes are
    /// selected, otherwise the time selection or the selected clips. The
    /// insert marker moves to the end of what was copied, so Paste lays the
    /// copy right after it.
    pub fn copy(&mut self) -> bool {
        if let Some((_, _, sel)) = self.note_selection() {
            let first = sel.iter().map(|n| n.start).min().unwrap_or(0);
            let last = sel.iter().map(Note::end).max().unwrap_or(first);
            let step = self.snap_step(GridView::Piano).unwrap_or(1);
            let span = ceil_to(last - first, step).max(step);
            self.ed.piano_marker = first + span;
            let notes = sel.into_iter().map(|n| Note { start: n.start - first, ..n }).collect();
            self.ed.clipboard = Some(Clipboard::Notes { span, notes });
            return true;
        }
        if self.ed.area != Area::Arrange {
            return false;
        }
        let Some((start, end, rows)) = self.lift_selection() else { return false };
        if rows.iter().all(|r| r.seeds.is_empty()) {
            return false;
        }
        self.ed.clipboard = Some(Clipboard::Clips { span: end - start, rows });
        self.ed.marker = end;
        true
    }

    /// Ctrl+X: Copy, then Delete. The marker stays where the cut began.
    pub fn cut(&mut self) -> bool {
        let notes = self.note_selection().map(|(_, _, sel)| sel.iter().map(|n| n.start).min().unwrap_or(0));
        let start = self.selection_span().map(|s| s.0);
        if !self.copy() {
            return false;
        }
        match notes {
            Some(first) => {
                self.ed.piano_marker = first;
                self.delete_selected_notes()
            }
            None => {
                self.ed.marker = start.unwrap_or(self.ed.marker);
                self.delete_arrangement().unwrap_or(false)
            }
        }
    }

    /// Where each clipboard row goes: the track it came from, or, when the
    /// selected track is another one that can hold the first row, the rows
    /// laid out from that track down. `None` for a row with no fitting track.
    fn paste_tracks(&self, rows: &[Row]) -> Vec<Option<TrackId>> {
        let lanes = self.clip_tracks();
        let kind = |t: TrackId| self.project.track(t).map(|t| t.kind);
        let Some(first) = rows.iter().find(|r| !r.seeds.is_empty()) else { return vec![None; rows.len()] };
        let base = self
            .selected_track
            .filter(|t| *t != first.track && kind(*t) == Some(first.kind))
            .and_then(|t| lanes.iter().position(|l| *l == t));
        rows.iter()
            .map(|r| match base {
                Some(base) => {
                    let lane = (base + r.lane).checked_sub(first.lane)?;
                    lanes.get(lane).copied().filter(|t| kind(*t) == Some(r.kind))
                }
                None => Some(r.track).filter(|t| kind(*t) == Some(r.kind)),
            })
            .collect()
    }

    /// Put clipboard rows at `at`, replacing what is under each clip.
    /// Returns the new placements.
    fn plant_rows(&self, plan: &mut Plan, rows: &[Row], at: Tick) -> Vec<PlacementId> {
        let mut placed = Vec::new();
        for (row, track) in rows.iter().zip(self.paste_tracks(rows)) {
            let Some(track) = track else { continue };
            for s in &row.seeds {
                // An audio clip only sits on an audio track, notes on an instrument track.
                if kind_of(&s.source) != row.kind {
                    continue;
                }
                plan.clear(track, at + s.at, at + s.at + s.clip.length);
                placed.push(plan.plant(s, track, at + s.at));
            }
        }
        placed
    }

    /// Ctrl+V. Clips land on the insert marker, on the tracks they came
    /// from (or from the selected track down when it is another track of the
    /// right kind), replacing what was there; the marker moves to the end
    /// of the pasted range, so pasting again lays the next copy after it.
    /// Notes land on the piano roll's marker in the open clip.
    pub fn paste(&mut self) -> bool {
        match self.ed.clipboard.clone() {
            None => false,
            Some(Clipboard::Notes { span, notes }) => {
                let Some((source, _)) = self.selected_clip.and_then(|c| self.clip_notes(c)) else { return false };
                let at = self.ed.piano_marker.max(0);
                let copies: Vec<Note> = notes.iter().map(|n| Note { id: self.project.alloc_note_id(), start: at + n.start, ..n.clone() }).collect();
                let ids: BTreeSet<NoteId> = copies.iter().map(|n| n.id).collect();
                self.seal();
                let ok = self.edit("Paste notes", vec![Command::AddNotes { source, notes: copies }]);
                self.seal();
                if ok {
                    self.ed.area = Area::Piano;
                    self.selected_notes = ids;
                    self.ed.piano_marker = at + span;
                }
                ok
            }
            Some(Clipboard::Clips { span, rows }) => {
                let at = self.ed.marker.max(0);
                let mut plan = Plan::new(self);
                let placed = self.plant_rows(&mut plan, &rows, at);
                if placed.is_empty() || !self.commit("Paste", plan) {
                    return false;
                }
                self.ed.area = Area::Arrange;
                let last = placed.last().copied();
                self.select_clips(placed, last);
                self.ed.marker = at + span;
                true
            }
        }
    }

    /// Ctrl+D in the arrangement: the time selection or the selected clips
    /// again, right after themselves; the copies become the selection, so
    /// repeating it tiles them. `None` leaves it to the caller: the piano
    /// roll has the keys and notes are selected, or nothing is selected here.
    pub(crate) fn duplicate_arrangement(&mut self) -> Option<bool> {
        if self.ed.area != Area::Arrange {
            // The piano roll has the keys: its notes or nothing, never the clips behind it.
            return self.note_selection().is_none().then_some(false);
        }
        let time = self.time_selection().cloned();
        let (start, end, rows) = self.lift_selection()?;
        let mut plan = Plan::new(self);
        let mut placed = Vec::new();
        for row in &rows {
            for s in &row.seeds {
                let to = end + s.at;
                plan.clear(row.track, to, to + s.clip.length);
                placed.push(plan.plant(s, row.track, to));
            }
        }
        if let Some(sel) = &time {
            // The copy of an empty stretch is an empty stretch.
            for track in &sel.tracks {
                if !rows.iter().any(|r| r.track == *track && !r.seeds.is_empty()) {
                    plan.clear(*track, end, end + (end - start));
                }
            }
        }
        if !self.commit(if time.is_some() { "Duplicate time selection" } else if placed.len() == 1 { "Duplicate clip" } else { "Duplicate clips" }, plan) {
            return Some(false);
        }
        self.ed.area = Area::Arrange;
        match time {
            Some(sel) => {
                self.ed.clips.clear();
                self.selected_placement = None;
                self.ed.marker = end;
                self.ed.time = Some(super::TimeSel { start: end, end: end + (end - start), anchor: (end, sel.anchor.1), ..sel });
            }
            None => {
                let last = placed.last().copied();
                self.select_clips(placed, last);
            }
        }
        Some(true)
    }

    /// Delete in the arrangement: the time selection is emptied (clips
    /// inside go, clips crossing an edge are cut there), else the selected
    /// clips go. `None` leaves it to the caller: the piano roll has the keys
    /// and notes are selected, or nothing is selected here.
    pub(crate) fn delete_arrangement(&mut self) -> Option<bool> {
        if self.ed.area != Area::Arrange {
            return self.note_selection().is_none().then_some(false);
        }
        let mut plan = Plan::new(self);
        let label = match self.time_selection().cloned() {
            Some(sel) => {
                for track in &sel.tracks {
                    plan.clear(*track, sel.start, sel.end);
                }
                "Delete time selection"
            }
            None => {
                let clips = self.clip_selection();
                if clips.is_empty() {
                    return None;
                }
                for (track, pl, _) in &clips {
                    plan.remove(*track, pl);
                }
                if clips.len() == 1 { "Delete clip" } else { "Delete clips" }
            }
        };
        let ok = self.commit(label, plan);
        if ok {
            self.ed.clips.clear();
            self.selected_placement = None;
        }
        Some(ok)
    }

    /// Ctrl+E with a time selection: cut at both of its edges on its
    /// tracks. Returns how many clips were cut, `None` without a selection.
    pub(crate) fn split_time_selection(&mut self) -> Option<usize> {
        let sel = self.time_selection().filter(|_| self.ed.area == Area::Arrange)?.clone();
        let mut plan = Plan::new(self);
        let cut: usize = sel.tracks.iter().map(|t| plan.split_edge(*t, sel.start) + plan.split_edge(*t, sel.end)).sum();
        Some(if self.commit("Split clips", plan) { cut } else { 0 })
    }
}
