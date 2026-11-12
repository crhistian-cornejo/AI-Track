//! UI state over the project. [`AppState`] owns the [`Project`] and its
//! [`History`]; every edit the views make goes through one of the methods
//! here, which build [`Command`]s and hand them to `History::apply` (or
//! `apply_coalesced` for drags, sealed on mouse-up). Nothing here touches
//! GPUI, so it is tested headlessly.

use aitrack_model::command::DeviceSlot;
use aitrack_model::*;
use std::collections::BTreeSet;

pub const BAR: Tick = 4 * PPQ;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainView {
    Session,
    Arrangement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BottomTab {
    Devices,
    PianoRoll,
    Steps,
    Mixer,
}

impl BottomTab {
    pub const ALL: [BottomTab; 4] = [Self::Devices, Self::PianoRoll, Self::Steps, Self::Mixer];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserTab {
    Library,
    Plugins,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Draw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    Off,
    Bar,
    Beat,
    Eighth,
    Sixteenth,
    ThirtySecond,
    TripletSixteenth,
}

impl Snap {
    pub const ALL: [Snap; 7] =
        [Self::Off, Self::Bar, Self::Beat, Self::Eighth, Self::Sixteenth, Self::ThirtySecond, Self::TripletSixteenth];

    pub fn ticks(self) -> Tick {
        match self {
            Self::Off => 1,
            Self::Bar => BAR,
            Self::Beat => PPQ,
            Self::Eighth => PPQ / 2,
            Self::Sixteenth => PPQ / 4,
            Self::ThirtySecond => PPQ / 8,
            Self::TripletSixteenth => PPQ / 6,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Bar => "1 Bar",
            Self::Beat => "1/4",
            Self::Eighth => "1/8",
            Self::Sixteenth => "1/16",
            Self::ThirtySecond => "1/32",
            Self::TripletSixteenth => "1/16 T",
        }
    }

    pub fn floor(self, t: Tick) -> Tick {
        let g = self.ticks();
        t.div_euclid(g) * g
    }

    pub fn round(self, t: Tick) -> Tick {
        let g = self.ticks();
        (t + g / 2).div_euclid(g) * g
    }
}

/// Zoom and scroll of the arrangement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArrangeView {
    pub px_per_beat: f32,
    pub scroll_x: f32,
    pub scroll_y: f32,
}

/// Zoom and scroll of the piano roll.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PianoView {
    pub px_per_beat: f32,
    pub row_h: f32,
    pub scroll_x: f32,
    pub scroll_y: f32,
}

pub struct AppState {
    pub project: Project,
    pub history: History,
    pub view: MainView,
    pub bottom_tab: BottomTab,
    pub browser_tab: BrowserTab,
    pub tool: Tool,
    pub snap: Snap,
    pub selected_track: Option<TrackId>,
    pub selected_clip: Option<ClipId>,
    pub selected_placement: Option<(TrackId, PlacementId)>,
    pub selected_notes: BTreeSet<NoteId>,
    pub automation_shown: BTreeSet<TrackId>,
    pub arrange: ArrangeView,
    pub piano: PianoView,
    /// Length new notes get (the last drawn or resized length, FL-style).
    pub note_length: Tick,
    pub last_error: Option<String>,
}

impl AppState {
    pub fn new(project: Project) -> Self {
        let first_track = project.content_tracks().next().map(|t| t.id);
        let mut s = Self {
            project,
            history: History::default(),
            view: MainView::Arrangement,
            bottom_tab: BottomTab::PianoRoll,
            browser_tab: BrowserTab::Library,
            tool: Tool::Draw,
            snap: Snap::Sixteenth,
            selected_track: first_track,
            selected_clip: None,
            selected_placement: None,
            selected_notes: BTreeSet::new(),
            automation_shown: BTreeSet::new(),
            arrange: ArrangeView { px_per_beat: 7., scroll_x: 0., scroll_y: 0. },
            piano: PianoView { px_per_beat: 64., row_h: 12., scroll_x: 0., scroll_y: 0. },
            note_length: PPQ / 2,
            last_error: None,
        };
        // Start on a melodic clip so the piano roll has something to show.
        let busiest = s
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Instrument && !is_drum_track(t))
            .filter_map(|t| {
                let clip = *t.session_slots.values().next()?;
                Some((s.clip_notes(clip).map_or(0, |(_, n)| n.notes.len()), t.id, clip))
            })
            .max_by_key(|(n, ..)| *n);
        if let Some((_, track, clip)) = busiest {
            s.selected_track = Some(track);
            s.selected_clip = Some(clip);
        }
        for t in &s.project.tracks {
            if !t.automation.is_empty() {
                s.automation_shown.insert(t.id);
            }
        }
        // Centre the piano roll on the notes (or middle C).
        let mid = s.selected_clip.and_then(|c| s.clip_notes(c)).map_or(60., |(_, n)| {
            if n.notes.is_empty() { 60. } else { n.notes.iter().map(|n| n.key as f32).sum::<f32>() / n.notes.len() as f32 }
        });
        s.piano.scroll_y = ((127. - mid) * s.piano.row_h - 120.).max(0.);
        s
    }

    pub fn revision(&self) -> u64 {
        self.history.revision()
    }

    // ── Applying edits ──

    /// One undo step. Returns whether it applied.
    pub fn edit(&mut self, label: &str, commands: Vec<Command>) -> bool {
        self.edit_as(label, Origin::User, commands)
    }

    pub fn edit_as(&mut self, label: &str, origin: Origin, commands: Vec<Command>) -> bool {
        match self.history.apply(&mut self.project, label, origin, commands) {
            Ok(()) => true,
            Err(e) => {
                self.last_error = Some(e.to_string());
                false
            }
        }
    }

    /// Part of a continuous gesture: merges with the previous step of the same `key`.
    pub fn edit_coalesced(&mut self, key: &str, label: &str, commands: Vec<Command>) -> bool {
        match self.history.apply_coalesced(&mut self.project, key, label, commands) {
            Ok(()) => true,
            Err(e) => {
                self.last_error = Some(e.to_string());
                false
            }
        }
    }

    /// End of a gesture: the next drag starts a fresh undo step.
    pub fn seal(&mut self) {
        self.history.seal();
    }

    pub fn undo(&mut self) -> bool {
        let done = self.history.undo(&mut self.project).unwrap_or(false);
        self.prune_selection();
        done
    }

    pub fn redo(&mut self) -> bool {
        let done = self.history.redo(&mut self.project).unwrap_or(false);
        self.prune_selection();
        done
    }

    fn prune_selection(&mut self) {
        if let Some(c) = self.selected_clip
            && !self.project.clips.contains_key(&c) {
                self.selected_clip = None;
            }
        let live: BTreeSet<NoteId> = self
            .selected_clip
            .and_then(|c| self.clip_notes(c))
            .map(|(_, n)| n.notes.iter().map(|n| n.id).collect())
            .unwrap_or_default();
        self.selected_notes.retain(|id| live.contains(id));
        if let Some((t, pl)) = self.selected_placement {
            let exists = self.project.track(t).is_some_and(|t| t.arrangement.iter().any(|p| p.id == pl));
            if !exists {
                self.selected_placement = None;
            }
        }
    }

    // ── Lookups ──

    pub fn clip_notes(&self, clip: ClipId) -> Option<(SourceId, &NoteSource)> {
        let c = self.project.clips.get(&clip)?;
        match self.project.sources.get(&c.source)? {
            Source::Notes(n) => Some((c.source, n)),
            Source::Audio(_) => None,
        }
    }

    /// The tick range of a clip that note tools edit (its loop, or its length).
    pub fn clip_edit_len(&self, clip: ClipId) -> Tick {
        self.project.clips.get(&clip).map_or(BAR, |c| c.looping.map_or(c.length, |l| l.end - l.start)).max(PPQ)
    }

    pub fn clip_track(&self, clip: ClipId) -> Option<TrackId> {
        self.project
            .tracks
            .iter()
            .find(|t| t.session_slots.values().any(|c| *c == clip) || t.arrangement.iter().any(|p| p.clip == clip))
            .map(|t| t.id)
    }

    pub fn clip_scene(&self, clip: ClipId) -> Option<SceneId> {
        self.project.tracks.iter().find_map(|t| t.session_slots.iter().find(|(_, c)| **c == clip).map(|(s, _)| *s))
    }

    /// The scale tools and highlighting use for a clip: its own, else the project's.
    pub fn clip_scale(&self, clip: Option<ClipId>) -> Option<Scale> {
        clip.and_then(|c| self.project.clips.get(&c)).and_then(|c| c.scale).or(self.project.scale)
    }

    /// FL-style ghost notes: the other MIDI clips in the same scene.
    pub fn ghost_notes(&self, clip: ClipId) -> Vec<(Color, Note)> {
        let Some(scene) = self.clip_scene(clip) else { return Vec::new() };
        let own = self.clip_track(clip);
        let mut out = Vec::new();
        for t in &self.project.tracks {
            if Some(t.id) == own || is_drum_track(t) {
                continue;
            }
            let Some(other) = t.session_slots.get(&scene) else { continue };
            if *other == clip {
                continue;
            }
            if let Some((_, n)) = self.clip_notes(*other) {
                out.extend(n.notes.iter().map(|n| (t.color, n.clone())));
            }
        }
        out
    }

    /// The clip the step sequencer edits: the selected clip on a drum-rack
    /// track, else the first clip of the first drum-rack track.
    pub fn step_clip(&self) -> Option<(TrackId, ClipId)> {
        if let (Some(clip), Some(track)) = (self.selected_clip, self.selected_clip.and_then(|c| self.clip_track(c)))
            && self.project.track(track).is_some_and(is_drum_track) {
                return Some((track, clip));
            }
        let t = self.project.tracks.iter().find(|t| is_drum_track(t))?;
        let clip = t.session_slots.values().next().copied().or_else(|| t.arrangement.first().map(|p| p.clip))?;
        Some((t.id, clip))
    }

    pub fn select_clip(&mut self, clip: Option<ClipId>) {
        if self.selected_clip != clip {
            self.selected_notes.clear();
        }
        self.selected_clip = clip;
        if let Some(t) = clip.and_then(|c| self.clip_track(c)) {
            self.selected_track = Some(t);
        }
    }

    // ── Notes ──

    /// Draw a note at `tick` (floored to the snap grid) and select it.
    pub fn draw_note(&mut self, clip: ClipId, key: u8, tick: Tick) -> Option<NoteId> {
        let (source, _) = self.clip_notes(clip)?;
        let start = self.snap.floor(tick.max(0));
        let id = self.project.alloc_note_id();
        let note = Note::new(id, key.min(127), start, self.note_length.max(1), 0.8);
        if self.edit("Add note", vec![Command::AddNotes { source, notes: vec![note] }]) {
            self.selected_notes.clear();
            self.selected_notes.insert(id);
            Some(id)
        } else {
            None
        }
    }

    pub fn delete_notes(&mut self, clip: ClipId, ids: Vec<NoteId>) -> bool {
        let Some((source, _)) = self.clip_notes(clip) else { return false };
        if ids.is_empty() {
            return false;
        }
        for id in &ids {
            self.selected_notes.remove(id);
        }
        self.edit("Delete notes", vec![Command::RemoveNotes { source, ids }])
    }

    pub fn delete_selected_notes(&mut self) -> bool {
        let Some(clip) = self.selected_clip else { return false };
        let ids: Vec<NoteId> = self.selected_notes.iter().copied().collect();
        self.delete_notes(clip, ids)
    }

    /// Move `originals` by `dt` ticks and `dk` semitones (one coalesced drag).
    pub fn move_notes(&mut self, clip: ClipId, originals: &[Note], dt: Tick, dk: i32) -> bool {
        let Some((source, _)) = self.clip_notes(clip) else { return false };
        let min_start = originals.iter().map(|n| n.start).min().unwrap_or(0);
        let dt = dt.max(-min_start);
        let notes = originals
            .iter()
            .map(|n| Note { start: n.start + dt, key: (n.key as i32 + dk).clamp(0, 127) as u8, ..n.clone() })
            .collect();
        self.edit_coalesced("notes-move", "Move notes", vec![Command::UpdateNotes { source, notes }])
    }

    /// Change the length of `originals` by `dl` ticks (one coalesced drag).
    pub fn resize_notes(&mut self, clip: ClipId, originals: &[Note], dl: Tick) -> bool {
        let Some((source, _)) = self.clip_notes(clip) else { return false };
        let min_len = self.snap.ticks().clamp(PPQ / 16, PPQ / 4);
        let notes: Vec<Note> =
            originals.iter().map(|n| Note { length: (n.length + dl).max(min_len), ..n.clone() }).collect();
        if let Some(n) = notes.first() {
            self.note_length = n.length;
        }
        self.edit_coalesced("notes-resize", "Resize notes", vec![Command::UpdateNotes { source, notes }])
    }

    /// Set velocity of `originals` offset by `dv` (one coalesced drag).
    pub fn set_velocity(&mut self, clip: ClipId, originals: &[Note], dv: f32) -> bool {
        let Some((source, _)) = self.clip_notes(clip) else { return false };
        if originals.is_empty() { return false }
        let dv = velocity_delta(originals, dv);
        let notes = originals.iter().map(|n| Note { velocity: (n.velocity + dv).clamp(0.01, 1.), ..n.clone() }).collect();
        self.edit_coalesced("notes-velocity", "Change velocity", vec![Command::UpdateNotes { source, notes }])
    }

    /// The notes a tool acts on: the selection, or the whole clip.
    fn tool_targets(&self, clip: ClipId) -> Option<(SourceId, Vec<Note>)> {
        let (source, n) = self.clip_notes(clip)?;
        let sel: Vec<Note> = n.notes.iter().filter(|n| self.selected_notes.contains(&n.id)).cloned().collect();
        Some((source, if sel.is_empty() { n.notes.clone() } else { sel }))
    }

    /// Quantise note starts to the snap grid (`strength` 0..=1). Returns how many moved.
    pub fn quantize(&mut self, clip: ClipId, strength: f32) -> usize {
        let Some((source, notes)) = self.tool_targets(clip) else { return 0 };
        let grid = if self.snap == Snap::Off { Snap::Sixteenth } else { self.snap };
        let s = strength.clamp(0., 1.);
        let changed: Vec<Note> = notes
            .into_iter()
            .filter_map(|n| {
                let target = grid.round(n.start);
                let start = n.start + ((target - n.start) as f32 * s).round() as Tick;
                (start != n.start).then_some(Note { start, ..n })
            })
            .collect();
        let count = changed.len();
        if count > 0 {
            self.edit("Quantize", vec![Command::UpdateNotes { source, notes: changed }]);
        }
        count
    }

    /// Move out-of-scale notes to the nearest note of the clip's scale. Returns how many moved.
    pub fn scale_snap(&mut self, clip: ClipId) -> usize {
        let Some(scale) = self.clip_scale(Some(clip)) else { return 0 };
        let Some((source, notes)) = self.tool_targets(clip) else { return 0 };
        let changed: Vec<Note> = notes
            .into_iter()
            .filter_map(|n| {
                let key = scale.snap(n.key);
                (key != n.key).then_some(Note { key, ..n })
            })
            .collect();
        let count = changed.len();
        if count > 0 {
            self.edit("Snap to scale", vec![Command::UpdateNotes { source, notes: changed }]);
        }
        count
    }

    // ── Keyboard editing (the Live key commands) ──

    fn grid(&self) -> Tick {
        if self.snap == Snap::Off { PPQ / 4 } else { self.snap.ticks().max(1) }
    }

    /// The selected notes of the open clip, when the piano roll has the keys.
    fn key_notes(&self) -> Option<(ClipId, SourceId, Vec<Note>)> {
        let clip = self.selected_clip?;
        let (source, n) = self.clip_notes(clip)?;
        let sel: Vec<Note> = n.notes.iter().filter(|n| self.selected_notes.contains(&n.id)).cloned().collect();
        (!sel.is_empty()).then_some((clip, source, sel))
    }

    /// Arrow-key editing of the selection, one undo step per press.
    ///
    /// Notes: ↑/↓ move by one step of the clip's scale (a semitone when there
    /// is no scale; Alt forces a semitone), Shift+↑/↓ by an octave, ←/→ by
    /// the grid, Shift+←/→ change the length, Ctrl+↑/↓ the velocity.
    /// With no notes selected, ←/→ nudge the selected arrangement clip.
    pub fn key_edit(&mut self, e: KeyEdit) -> bool {
        let grid = self.grid();
        if let Some((clip, source, sel)) = self.key_notes() {
            let scale = self.clip_scale(Some(clip));
            let first = sel.iter().map(|n| n.start).min().unwrap_or(0);
            let notes: Vec<Note> = sel
                .iter()
                .map(|n| {
                    let mut n = n.clone();
                    match e {
                        KeyEdit::Up | KeyEdit::Down => {
                            let dir = if e == KeyEdit::Up { 1 } else { -1 };
                            n.key = match scale {
                                Some(s) => step_in_scale(s, n.key, dir),
                                None => (n.key as i32 + dir).clamp(0, 127) as u8,
                            };
                        }
                        KeyEdit::SemitoneUp => n.key = (n.key + 1).min(127),
                        KeyEdit::SemitoneDown => n.key = n.key.saturating_sub(1),
                        KeyEdit::OctaveUp => n.key = if n.key <= 115 { n.key + 12 } else { n.key },
                        KeyEdit::OctaveDown => n.key = if n.key >= 12 { n.key - 12 } else { n.key },
                        KeyEdit::Left => n.start -= grid.min(first),
                        KeyEdit::Right => n.start += grid,
                        KeyEdit::Longer => n.length += grid,
                        KeyEdit::Shorter => n.length = (n.length - grid).max(grid.min(PPQ / 4)),
                        KeyEdit::VelocityUp => n.velocity = (n.velocity + 0.08).min(1.),
                        KeyEdit::VelocityDown => n.velocity = (n.velocity - 0.08).max(0.01),
                    }
                    n
                })
                .collect();
            if notes == sel {
                return false;
            }
            self.seal();
            let label = match e {
                KeyEdit::Left | KeyEdit::Right => "Move notes",
                KeyEdit::Longer | KeyEdit::Shorter => "Resize notes",
                KeyEdit::VelocityUp | KeyEdit::VelocityDown => "Change velocity",
                _ => "Transpose notes",
            };
            let ok = self.edit(label, vec![Command::UpdateNotes { source, notes }]);
            self.seal();
            return ok;
        }
        if let (Some((track, id)), KeyEdit::Left | KeyEdit::Right) = (self.selected_placement, e) {
            let Some(start) = self.project.track(track).and_then(|t| t.arrangement.iter().find(|p| p.id == id)).map(|p| p.start) else {
                return false;
            };
            let to = if e == KeyEdit::Left { (start - grid).max(0) } else { start + grid };
            if to == start {
                return false;
            }
            self.seal();
            let ok = self.move_placement(track, id, track, to);
            self.seal();
            return ok;
        }
        false
    }

    /// `0` in Live: deactivate or reactivate the selection. Notes when notes
    /// are selected, otherwise the selected clip, otherwise the selected track.
    pub fn toggle_active(&mut self) -> Option<&'static str> {
        if let Some((_, source, sel)) = self.key_notes() {
            let mute = sel.iter().any(|n| !n.muted);
            let notes = sel.into_iter().map(|n| Note { muted: mute, ..n }).collect();
            self.seal();
            self.edit(if mute { "Deactivate notes" } else { "Activate notes" }, vec![Command::UpdateNotes { source, notes }]);
            self.seal();
            return Some(if mute { "Notes deactivated" } else { "Notes activated" });
        }
        let placed = self.selected_placement.and_then(|(t, id)| self.project.track(t)?.arrangement.iter().find(|p| p.id == id).map(|p| p.clip));
        if let Some(id) = placed.or(self.selected_clip.filter(|_| self.bottom_tab == BottomTab::PianoRoll && self.selected_track.is_none()))
            && let Some(clip) = self.project.clips.get(&id).cloned()
        {
            let muted = !clip.muted;
            self.seal();
            self.edit(if muted { "Deactivate clip" } else { "Activate clip" }, vec![Command::UpdateClip { id, clip: Box::new(Clip { muted, ..clip }) }]);
            self.seal();
            return Some(if muted { "Clip deactivated" } else { "Clip activated" });
        }
        let track = self.selected_track?;
        let mute = !self.project.track(track)?.mute;
        self.seal();
        self.set_track(track, TrackProp::Mute(mute));
        self.seal();
        Some(if mute { "Track deactivated" } else { "Track activated" })
    }

    /// Ctrl+D: duplicate the selected notes right after themselves, or the
    /// selected arrangement clip right after itself (as an independent copy).
    pub fn duplicate_selection(&mut self) -> bool {
        let grid = self.grid();
        if let Some((_, source, sel)) = self.key_notes() {
            let (first, last) = (sel.iter().map(|n| n.start).min().unwrap_or(0), sel.iter().map(Note::end).max().unwrap_or(0));
            let span = ((last - first + grid - 1) / grid * grid).max(grid);
            let copies: Vec<Note> = sel.iter().map(|n| Note { id: self.project.alloc_note_id(), start: n.start + span, ..n.clone() }).collect();
            let ids: Vec<NoteId> = copies.iter().map(|n| n.id).collect();
            self.seal();
            let ok = self.edit("Duplicate notes", vec![Command::AddNotes { source, notes: copies }]);
            self.seal();
            if ok {
                self.selected_notes = ids.into_iter().collect();
            }
            return ok;
        }
        let Some((track, id)) = self.selected_placement else { return false };
        let Some(place) = self.project.track(track).and_then(|t| t.arrangement.iter().find(|p| p.id == id)).cloned() else { return false };
        let Some(clip) = self.project.clips.get(&place.clip).cloned() else { return false };
        let mut cmds = Vec::new();
        let mut copy = clip.clone();
        // MIDI clips get their own notes so editing the copy leaves the original alone.
        if let Some(Source::Notes(n)) = self.project.sources.get(&clip.source).cloned() {
            let source = self.project.alloc_source_id();
            let notes = n.notes.iter().map(|x| Note { id: self.project.alloc_note_id(), ..x.clone() }).collect();
            cmds.push(Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource { notes })) });
            copy.source = source;
        }
        let new_clip = self.project.alloc_clip_id();
        let new_place = self.project.alloc_placement_id();
        cmds.push(Command::AddClip { id: new_clip, clip: Box::new(copy) });
        cmds.push(Command::PlaceClip { track, placement: Placement { id: new_place, clip: new_clip, start: place.start + clip.length, lane: place.lane } });
        self.seal();
        let ok = self.edit("Duplicate clip", cmds);
        self.seal();
        if ok {
            self.selected_placement = Some((track, new_place));
        }
        ok
    }

    // ── Clips and tracks (the Live key commands) ──

    /// Ctrl+E: split at `tick`. The selected arrangement clip when the cut
    /// falls inside it, otherwise every clip of the selected track under it.
    /// Returns how many clips were split.
    pub fn split_at(&mut self, tick: Tick) -> usize {
        let tick = self.snap.round(tick);
        let spans = |p: &Project, pl: &Placement| p.clips.get(&pl.clip).is_some_and(|c| tick > pl.start && tick < pl.start + c.length);
        let mut targets: Vec<(TrackId, Placement)> = Vec::new();
        if let Some((track, id)) = self.selected_placement
            && let Some(pl) = self.project.track(track).and_then(|t| t.arrangement.iter().find(|p| p.id == id))
            && spans(&self.project, pl)
        {
            targets.push((track, pl.clone()));
        }
        if targets.is_empty()
            && let Some(track) = self.selected_track
            && let Some(t) = self.project.track(track)
        {
            targets.extend(t.arrangement.iter().filter(|pl| spans(&self.project, pl)).map(|pl| (track, pl.clone())));
        }
        let mut cmds = Vec::new();
        let mut last = None;
        for (track, pl) in &targets {
            let Some(clip) = self.project.clips.get(&pl.clip).cloned() else { continue };
            let head = tick - pl.start;
            let right = Clip { offset: clip.offset + head, length: clip.length - head, ..clip.clone() };
            let (right_id, place_id) = (self.project.alloc_clip_id(), self.project.alloc_placement_id());
            cmds.push(Command::UpdateClip { id: pl.clip, clip: Box::new(Clip { length: head, ..clip }) });
            cmds.push(Command::AddClip { id: right_id, clip: Box::new(right) });
            cmds.push(Command::PlaceClip { track: *track, placement: Placement { id: place_id, clip: right_id, start: tick, lane: pl.lane } });
            last = Some((*track, place_id));
        }
        if cmds.is_empty() {
            return 0;
        }
        self.seal();
        let ok = self.edit("Split clip", cmds);
        self.seal();
        if ok {
            self.selected_placement = last;
            targets.len()
        } else {
            0
        }
    }

    /// Where a new track goes: right after the selected one, else before the buses.
    fn new_track_index(&self) -> usize {
        let buses = self.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(self.project.tracks.len());
        self.selected_track
            .and_then(|id| self.project.track_index(id))
            .filter(|i| *i < buses)
            .map_or(buses, |i| {
                // After a group, skip the tracks inside it.
                let id = self.project.tracks[i].id;
                let inside = self.project.tracks[i + 1..buses].iter().take_while(|t| t.parent == Some(id)).count();
                i + 1 + inside
            })
    }

    /// Ctrl+T / Ctrl+Shift+T / Ctrl+Alt+T: a new audio, MIDI or return track.
    pub fn add_track(&mut self, kind: TrackKind) -> Option<TrackId> {
        let id = self.project.alloc_track_id();
        let count = self.project.tracks.iter().filter(|t| t.kind == kind).count();
        let mut cmds = Vec::new();
        let (name, index) = match kind {
            TrackKind::Return => {
                let letter = (b'A' + (count as u8).min(25)) as char;
                // Every track gets a send to the new return, turned down.
                for t in self.project.tracks.iter().filter(|t| t.kind.has_clips() || t.kind == TrackKind::Group) {
                    let mut sends = t.sends.clone();
                    sends.push(Send { target: id, level_db: -70., pre_fader: false });
                    cmds.push(Command::SetTrack { id: t.id, change: TrackProp::Sends(sends) });
                }
                let main = self.project.track_index(self.project.main_track().id).unwrap_or(self.project.tracks.len());
                let after_returns = self.project.tracks.iter().rposition(|t| t.kind == TrackKind::Return).map_or(main, |i| i + 1);
                (format!("{letter} Return"), after_returns.min(main))
            }
            TrackKind::Audio => (format!("Audio {}", count + 1), self.new_track_index()),
            _ => (format!("MIDI {}", count + 1), self.new_track_index()),
        };
        let mut track = Track::new(id, kind, name, Color::palette(self.project.tracks.len()));
        // A track created inside a group joins it.
        if kind != TrackKind::Return
            && let Some(parent) = self.selected_track.and_then(|s| self.project.track(s)).and_then(|s| if s.kind == TrackKind::Group { Some(s.id) } else { s.parent })
            && let Some(g) = self.project.track(parent)
        {
            track.parent = Some(parent);
            track.output = Output::Track(parent);
            track.color = g.color;
        }
        cmds.insert(0, Command::AddTrack { track: Box::new(track), index });
        self.seal();
        let ok = self.edit("Add track", cmds);
        self.seal();
        ok.then(|| {
            self.selected_track = Some(id);
            id
        })
    }

    /// Ctrl+G: put the selected track in a new group (which takes its colour
    /// and its place in the list). A track that is already in a group is
    /// grouped inside it.
    pub fn group_selected(&mut self) -> Option<TrackId> {
        let track = self.selected_track.and_then(|id| self.project.track(id)).filter(|t| t.kind.has_clips() || t.kind == TrackKind::Group)?.clone();
        let index = self.project.track_index(track.id)?;
        let id = self.project.alloc_track_id();
        let mut group = Track::new(id, TrackKind::Group, "Group", track.color);
        group.parent = track.parent;
        group.output = track.output;
        group.sends = track.sends.clone();
        let cmds = vec![
            Command::AddTrack { track: Box::new(group), index },
            Command::SetTrack { id: track.id, change: TrackProp::Parent(Some(id)) },
            Command::SetTrack { id: track.id, change: TrackProp::Output(Output::Track(id)) },
        ];
        self.seal();
        let ok = self.edit("Group track", cmds);
        self.seal();
        ok.then_some(id)
    }

    /// Ctrl+Shift+G: dissolve the selected group (or the group the selected
    /// track is in); its tracks go back to where the group was routed.
    pub fn ungroup_selected(&mut self) -> bool {
        let Some(sel) = self.selected_track.and_then(|id| self.project.track(id)) else { return false };
        let Some(group) = (if sel.kind == TrackKind::Group { Some(sel.clone()) } else { sel.parent.and_then(|p| self.project.track(p)).cloned() }) else {
            return false;
        };
        let mut cmds = Vec::new();
        let mut first = None;
        for t in self.project.tracks.iter().filter(|t| t.parent == Some(group.id)) {
            first.get_or_insert(t.id);
            cmds.push(Command::SetTrack { id: t.id, change: TrackProp::Parent(group.parent) });
            cmds.push(Command::SetTrack { id: t.id, change: TrackProp::Output(group.output) });
        }
        cmds.push(Command::RemoveTrack { id: group.id });
        self.seal();
        let ok = self.edit("Ungroup tracks", cmds);
        self.seal();
        if ok {
            self.selected_track = first.or(self.project.content_tracks().next().map(|t| t.id));
        }
        ok
    }

    /// Step sequencer: toggle a note of `key` in the step starting at `start`.
    pub fn toggle_step(&mut self, clip: ClipId, key: u8, start: Tick, len: Tick) -> bool {
        let Some((source, n)) = self.clip_notes(clip) else { return false };
        let hits: Vec<NoteId> =
            n.notes.iter().filter(|n| n.key == key && n.start >= start && n.start < start + len).map(|n| n.id).collect();
        if hits.is_empty() {
            let id = self.project.alloc_note_id();
            self.edit("Add step", vec![Command::AddNotes { source, notes: vec![Note::new(id, key, start, len, 0.8)] }])
        } else {
            self.edit("Remove step", vec![Command::RemoveNotes { source, ids: hits }])
        }
    }

    // ── Tracks, devices, mixer ──

    pub fn set_track(&mut self, id: TrackId, change: TrackProp) -> bool {
        let label = Command::SetTrack { id, change: change.clone() }.label();
        self.edit(label, vec![Command::SetTrack { id, change }])
    }

    /// Fader, pan and send drags.
    pub fn set_track_coalesced(&mut self, key: &str, id: TrackId, change: TrackProp) -> bool {
        let label = Command::SetTrack { id, change: change.clone() }.label();
        self.edit_coalesced(key, label, vec![Command::SetTrack { id, change }])
    }

    pub fn set_send(&mut self, track: TrackId, index: usize, level_db: f32) -> bool {
        let Some(t) = self.project.track(track) else { return false };
        let mut sends = t.sends.clone();
        let Some(s) = sends.get_mut(index) else { return false };
        s.level_db = level_db.clamp(-70., 6.);
        self.edit_coalesced(&format!("send-{}-{index}", track.0), "Change sends", vec![Command::SetTrack {
            id: track,
            change: TrackProp::Sends(sends),
        }])
    }

    pub fn set_param(&mut self, device: DeviceId, param: u32, value: f32) -> bool {
        self.edit_coalesced(&format!("param-{}-{param}", device.0), "Change parameter", vec![Command::SetParam {
            device,
            param,
            value: Some(value.clamp(0., 1.)),
        }])
    }

    pub fn set_macro(&mut self, rack: DeviceId, index: usize, value: f32) -> bool {
        self.edit_coalesced(&format!("macro-{}-{index}", rack.0), "Change macro", vec![Command::SetMacro {
            rack,
            index,
            value,
        }])
    }

    pub fn toggle_device(&mut self, device: DeviceId) -> bool {
        let Some(enabled) = self.project.device(device).map(|d| d.enabled) else { return false };
        self.edit("Toggle device", vec![Command::SetDeviceEnabled { device, enabled: !enabled }])
    }

    pub fn add_stock_device(&mut self, track: TrackId, plugin_id: &str, name: &str) -> Option<DeviceId> {
        let index = self.project.track(track)?.devices.len();
        let id = self.project.alloc_device_id();
        let mut device = Device::stock(id, plugin_id, name);
        if let Some(kind) = match plugin_id {
            "rack.drum" => Some(RackKind::Drum),
            "rack.instrument" => Some(RackKind::Instrument),
            "rack.audio" => Some(RackKind::Audio),
            _ => None,
        } {
            device.kind = DeviceKind::Rack(Rack::new(kind));
        }
        let slot = DeviceSlot::Track { track, index };
        self.edit("Add device", vec![Command::AddDevice { slot, device: Box::new(device) }]).then_some(id)
    }

    /// Add a third-party VST3/CLAP plugin to the end of a track's chain.
    pub fn add_plugin_device(&mut self, track: TrackId, plugin: &crate::bridge::ExternalPlugin) -> Option<DeviceId> {
        let index = self.project.track(track)?.devices.len();
        let id = self.project.alloc_device_id();
        let mut device = Device::stock(id, "", plugin.name.clone());
        device.kind = DeviceKind::Plugin {
            format: plugin.format,
            uid: plugin.uid.clone(),
            path: plugin.path.clone(),
            vendor: plugin.vendor.clone(),
            is_instrument: plugin.is_instrument,
        };
        let slot = DeviceSlot::Track { track, index };
        self.edit("Add device", vec![Command::AddDevice { slot, device: Box::new(device) }]).then_some(id)
    }

    /// Apply a stock preset: every parameter it stores, plus its name for
    /// display, as one undo step.
    pub fn load_preset(&mut self, device: DeviceId, name: &str, params: &std::collections::BTreeMap<u32, f32>) -> bool {
        let mut cmds: Vec<Command> = params.iter().map(|(&param, &v)| Command::SetParam { device, param, value: Some(v) }).collect();
        // Factory presets are parameters only; the device keeps its state (samples…).
        let state = self.project.device(device).and_then(|d| d.state.clone());
        cmds.push(Command::SetDeviceState { device, state, preset: Some(name.to_owned()) });
        self.seal();
        let ok = self.edit(&format!("Load preset {name}"), cmds);
        self.seal();
        ok
    }

    pub fn remove_device(&mut self, id: DeviceId) -> bool {
        self.edit("Delete device", vec![Command::RemoveDevice { id }])
    }

    // ── Clips, arrangement, transport settings ──

    pub fn move_placement(&mut self, track: TrackId, id: PlacementId, to_track: TrackId, start: Tick) -> bool {
        let ok = self.edit_coalesced("clip-move", "Move clip", vec![Command::MovePlacement {
            track,
            id,
            to_track,
            start: start.max(0),
            lane: 0,
        }]);
        if ok {
            self.selected_placement = Some((to_track, id));
        }
        ok
    }

    pub fn resize_clip(&mut self, clip: ClipId, length: Tick) -> bool {
        let Some(c) = self.project.clips.get(&clip) else { return false };
        let clip_box = Box::new(Clip { length: length.max(PPQ / 4), ..c.clone() });
        self.edit_coalesced("clip-resize", "Resize clip", vec![Command::UpdateClip { id: clip, clip: clip_box }])
    }

    pub fn remove_placement(&mut self, track: TrackId, id: PlacementId) -> bool {
        self.selected_placement = None;
        self.edit("Delete clip", vec![Command::RemovePlacement { track, id }])
    }

    pub fn set_tempo(&mut self, bpm: f64) -> bool {
        self.edit_coalesced("tempo", "Change tempo", vec![Command::SetTempo { bpm: (bpm * 100.).round() / 100. }])
    }

    pub fn set_time_signature(&mut self, sig: TimeSignature) -> bool {
        let mut map = self.project.tempo.clone();
        match map.signatures.first_mut() {
            Some(first) => first.1 = sig,
            None => map.signatures.push((0, sig)),
        }
        self.edit("Change time signature", vec![Command::SetTempoMap { map }])
    }

    pub fn set_scale(&mut self, scale: Option<Scale>) -> bool {
        self.edit("Change scale", vec![Command::SetScale { scale }])
    }

    pub fn set_loop(&mut self, region: Option<(Tick, Tick)>) -> bool {
        self.edit_coalesced("loop", "Set loop", vec![Command::SetLoop { region }])
    }

    /// Insert or replace an automation point (normalised value).
    pub fn set_automation_point(&mut self, track: TrackId, target: AutomationTarget, tick: Tick, value: f32, drag: bool) -> bool {
        let Some(t) = self.project.track(track) else { return false };
        let mut lane = t.automation.iter().find(|l| l.target == target).cloned().unwrap_or_else(|| AutomationLane::new(target));
        lane.insert(AutomationPoint { tick: tick.max(0), value: value.clamp(0., 1.), curve: 0. });
        let cmd = Command::SetAutomation { track, target, lane: Some(lane) };
        if drag { self.edit_coalesced("automation", "Edit automation", vec![cmd]) } else { self.edit("Edit automation", vec![cmd]) }
    }

    /// Replace the lane's points during a point drag (removes the old point first).
    pub fn move_automation_point(&mut self, track: TrackId, target: AutomationTarget, original: Vec<AutomationPoint>, index: usize, tick: Tick, value: f32) -> bool {
        let mut lane = self
            .project
            .track(track)
            .and_then(|t| t.automation.iter().find(|l| l.target == target).cloned())
            .unwrap_or_else(|| AutomationLane::new(target));
        let mut points = original;
        // The point keeps the curve of the segment that starts at it.
        let curve = points.get(index).map_or(0., |p| p.curve);
        if index < points.len() {
            points.remove(index);
        }
        lane.points = points;
        lane.insert(AutomationPoint { tick: tick.max(0), value: value.clamp(0., 1.), curve });
        self.edit_coalesced("automation", "Edit automation", vec![Command::SetAutomation { track, target, lane: Some(lane) }])
    }

    /// Remove one breakpoint of an automation lane (double-click or right-click).
    pub fn delete_automation_point(&mut self, track: TrackId, target: AutomationTarget, index: usize) -> bool {
        let Some(mut lane) = self.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target).cloned()) else { return false };
        if index >= lane.points.len() {
            return false;
        }
        lane.points.remove(index);
        self.seal();
        let ok = self.edit("Delete automation point", vec![Command::SetAutomation { track, target, lane: Some(lane) }]);
        self.seal();
        ok
    }

    /// Bend the segment that starts at point `index` (−1..1, 0 = straight). One coalesced drag.
    pub fn set_automation_curve(&mut self, track: TrackId, target: AutomationTarget, index: usize, curve: f32) -> bool {
        let Some(mut lane) = self.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target).cloned()) else { return false };
        let Some(p) = lane.points.get_mut(index) else { return false };
        p.curve = curve.clamp(-1., 1.);
        self.edit_coalesced("automation-curve", "Curve automation", vec![Command::SetAutomation { track, target, lane: Some(lane) }])
    }

    /// Delete whatever is selected: notes in the piano roll, else the selected arrangement clip.
    pub fn delete_selection(&mut self) -> bool {
        if !self.selected_notes.is_empty() && self.bottom_tab == BottomTab::PianoRoll {
            return self.delete_selected_notes();
        }
        if let Some((t, pl)) = self.selected_placement {
            return self.remove_placement(t, pl);
        }
        false
    }

    // ── Proposals (AI) ──

    pub fn propose(&mut self, label: &str, origin: Origin, commands: Vec<Command>) -> Option<u64> {
        match self.history.propose(&self.project, label, origin, commands) {
            Ok(id) => Some(id),
            Err(e) => {
                self.last_error = Some(e.to_string());
                None
            }
        }
    }

    pub fn accept(&mut self, id: u64) -> bool {
        self.history.accept(&mut self.project, id).unwrap_or_else(|e| {
            self.last_error = Some(e.to_string());
            false
        })
    }

    pub fn discard(&mut self, id: u64) -> bool {
        self.history.discard(id)
    }

    /// Commands that add a new MIDI track holding `notes` in the first free scene.
    pub fn midi_track_commands(&mut self, name: &str, color: Color, notes: &[(u8, Tick, Tick, f32)]) -> Vec<Command> {
        let track = self.project.alloc_track_id();
        let src = self.project.alloc_source_id();
        let clip = self.project.alloc_clip_id();
        let notes: Vec<Note> =
            notes.iter().map(|&(k, s, l, v)| Note::new(self.project.alloc_note_id(), k, s, l, v)).collect();
        let len = notes.iter().map(|n| n.end()).max().unwrap_or(BAR);
        let length = (len + BAR - 1) / BAR * BAR;
        let index = self.project.tracks.iter().position(|t| matches!(t.kind, TrackKind::Return | TrackKind::Vca | TrackKind::Main)).unwrap_or(self.project.tracks.len() - 1);
        let mut t = Track::new(track, TrackKind::Instrument, name, color);
        let dev = self.project.alloc_device_id();
        t.devices.push(Device::stock(dev, "instrument.wavetable", "Wavetable Synth"));
        let mut commands = vec![
            Command::AddTrack { track: Box::new(t), index },
            Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes })) },
            Command::AddClip {
                id: clip,
                clip: Box::new(Clip {
                    name: name.into(),
                    color,
                    source: src,
                    offset: 0,
                    length,
                    looping: None,
                    gain_db: 0.,
                    envelopes: Vec::new(),
                    launch: LaunchSettings::default(),
                    muted: false,
                    scale: None,
                }),
            },
        ];
        if let Some(scene) = self.project.scenes.first() {
            commands.push(Command::SetSessionSlot { track, scene: scene.id, clip: Some(clip) });
        }
        commands
    }
}

pub fn velocity_delta(originals: &[Note], delta: f32) -> f32 {
    if originals.is_empty() { return 0. }
    let lower = originals.iter().map(|n| 0.01 - n.velocity).fold(f32::NEG_INFINITY, f32::max);
    let upper = originals.iter().map(|n| 1. - n.velocity).fold(f32::INFINITY, f32::min);
    delta.clamp(lower, upper)
}

pub fn is_drum_track(t: &Track) -> bool {
    t.devices.iter().any(|d| matches!(&d.kind, DeviceKind::Rack(r) if r.kind == RackKind::Drum))
}

/// The drum rack's pads (name, colour, note) on a track.
pub fn drum_pads(t: &Track) -> Vec<(String, Color, u8)> {
    t.devices
        .iter()
        .find_map(|d| match &d.kind {
            DeviceKind::Rack(r) if r.kind == RackKind::Drum => {
                Some(r.chains.iter().filter_map(|c| c.note.map(|n| (c.name.clone(), c.color, n))).collect())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// dB ↔ fader position (0..=1), with 0 dB at 0.75 like most consoles.
pub fn db_to_fader(db: f32) -> f32 {
    if db <= -70. {
        return 0.;
    }
    if db >= 0. {
        return (0.75 + db / 6. * 0.25).min(1.);
    }
    0.75 * ((db + 70.) / 70.).powf(2.2)
}

pub fn fader_to_db(pos: f32) -> f32 {
    let pos = pos.clamp(0., 1.);
    if pos <= 0. {
        return -150.;
    }
    if pos >= 0.75 {
        return (pos - 0.75) / 0.25 * 6.;
    }
    (pos / 0.75).powf(1. / 2.2) * 70. - 70.
}

pub fn format_db(db: f32) -> String {
    if db <= -69.9 { "-inf".into() } else { format!("{db:+.1}") }
}


/// Commands that give every track inside `group`, and the clips on them,
/// the group's colour, so a group reads as one block (as grouped tracks do
/// in Live). Empty when nothing would change.
pub fn group_color_commands(p: &Project, group: TrackId) -> Vec<Command> {
    let Some(color) = p.track(group).map(|g| g.color) else { return Vec::new() };
    let mut out = Vec::new();
    for t in p.tracks.iter().filter(|t| t.parent == Some(group)) {
        if t.color != color {
            out.push(Command::SetTrack { id: t.id, change: TrackProp::Color(color) });
        }
        let clips: BTreeSet<ClipId> = t.session_slots.values().copied().chain(t.arrangement.iter().map(|pl| pl.clip)).collect();
        for id in clips {
            if let Some(c) = p.clips.get(&id).filter(|c| c.color != color) {
                out.push(Command::UpdateClip { id, clip: Box::new(Clip { color, ..c.clone() }) });
            }
        }
        // Groups inside the group follow too.
        if t.kind == TrackKind::Group {
            let mut inner = p.clone();
            if let Some(g) = inner.track_mut(t.id) {
                g.color = color;
            }
            out.extend(group_color_commands(&inner, t.id));
        }
    }
    out
}

/// A keyboard edit of the selection (see [`AppState::key_edit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEdit {
    Up,
    Down,
    SemitoneUp,
    SemitoneDown,
    OctaveUp,
    OctaveDown,
    Left,
    Right,
    Longer,
    Shorter,
    VelocityUp,
    VelocityDown,
}

/// The next note of `scale` above (`dir` = 1) or below (`dir` = -1) `key`.
/// A note outside the scale lands on the nearest scale note in that direction.
pub fn step_in_scale(scale: Scale, key: u8, dir: i32) -> u8 {
    let mut k = key as i32;
    for _ in 0..12 {
        k += dir;
        if !(0..=127).contains(&k) {
            return key;
        }
        if scale.contains(k as u8) {
            return k as u8;
        }
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState::new(crate::demo::project())
    }

    fn lead_clip(s: &AppState) -> ClipId {
        let lead = s.project.tracks.iter().find(|t| t.name == "Lead").unwrap();
        *lead.session_slots.values().next().unwrap()
    }

    #[test]
    fn drawing_a_note_is_one_undoable_command() {
        let mut s = state();
        let clip = lead_clip(&s);
        let before = s.clip_notes(clip).unwrap().1.notes.len();
        let id = s.draw_note(clip, 60, 5 * PPQ / 4 + 13).expect("note drawn");
        let (_, notes) = s.clip_notes(clip).unwrap();
        assert_eq!(notes.notes.len(), before + 1);
        let n = notes.notes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(n.start, 5 * PPQ / 4, "floored to the 1/16 grid");
        assert_eq!(s.history.undo_label(), Some("Add note"));
        assert!(s.undo());
        assert_eq!(s.clip_notes(clip).unwrap().1.notes.len(), before);
        assert!(s.selected_notes.is_empty(), "selection follows undo");
        assert!(s.redo());
        assert_eq!(s.clip_notes(clip).unwrap().1.notes.len(), before + 1);
    }

    #[test]
    fn arrow_keys_edit_the_selected_notes_like_live() {
        let mut s = state();
        let clip = lead_clip(&s);
        s.selected_clip = Some(clip);
        // The demo is in A minor: A4 (69) is in the scale, A#4 (70) is not.
        let id = s.draw_note(clip, 69, 0).unwrap();
        let note = |s: &AppState| s.clip_notes(clip).unwrap().1.notes.iter().find(|n| n.id == id).unwrap().clone();
        assert!(s.key_edit(KeyEdit::Up));
        assert_eq!(note(&s).key, 71, "up moves to the next note of the scale (B), skipping A#");
        assert!(s.key_edit(KeyEdit::SemitoneDown));
        assert_eq!(note(&s).key, 70, "Alt moves a semitone");
        assert!(s.key_edit(KeyEdit::Down));
        assert_eq!(note(&s).key, 69, "an out-of-scale note lands on the scale");
        assert!(s.key_edit(KeyEdit::OctaveUp));
        assert_eq!(note(&s).key, 81);
        let grid = s.snap.ticks();
        assert!(s.key_edit(KeyEdit::Right));
        assert_eq!(note(&s).start, grid);
        assert!(s.key_edit(KeyEdit::Left) && s.key_edit(KeyEdit::Longer));
        assert_eq!((note(&s).start, note(&s).length), (0, s.note_length + grid));
        assert!(!s.key_edit(KeyEdit::Left), "cannot move before the clip start");
        let v = note(&s).velocity;
        assert!(s.key_edit(KeyEdit::VelocityDown));
        assert!(note(&s).velocity < v);
        // Each press is its own undo step.
        assert_eq!(s.history.undo_label(), Some("Change velocity"));
        assert!(s.undo());
        assert_eq!(note(&s).velocity, v);
    }

    #[test]
    fn zero_deactivates_and_ctrl_d_duplicates() {
        let mut s = state();
        let clip = lead_clip(&s);
        s.selected_clip = Some(clip);
        let id = s.draw_note(clip, 60, 0).unwrap();
        let count = s.clip_notes(clip).unwrap().1.notes.len();
        assert_eq!(s.toggle_active(), Some("Notes deactivated"));
        assert!(s.clip_notes(clip).unwrap().1.notes.iter().find(|n| n.id == id).unwrap().muted);
        assert_eq!(s.toggle_active(), Some("Notes activated"));
        assert!(s.duplicate_selection());
        let notes = &s.clip_notes(clip).unwrap().1.notes;
        assert_eq!(notes.len(), count + 1);
        let copy = notes.iter().find(|n| s.selected_notes.contains(&n.id)).unwrap();
        assert_eq!((copy.key, copy.start), (60, s.note_length), "the copy follows the original and is selected");
        // With no notes selected, 0 acts on the selected track.
        s.selected_notes.clear();
        let track = s.project.tracks.iter().find(|t| t.name == "Lead").unwrap().id;
        s.selected_track = Some(track);
        s.selected_placement = None;
        assert_eq!(s.toggle_active(), Some("Track deactivated"));
        assert!(s.project.track(track).unwrap().mute);
    }

    #[test]
    fn automation_points_can_be_deleted_and_segments_curved() {
        let mut s = state();
        let track = s.project.tracks.iter().find(|t| t.name == "Lead").unwrap().id;
        let target = AutomationTarget::Pan;
        assert!(s.set_automation_point(track, target, 0, 0.2, false));
        assert!(s.set_automation_point(track, target, 4 * PPQ, 0.8, false));
        assert!(s.set_automation_point(track, target, 8 * PPQ, 0.5, false));
        let lane = |s: &AppState| s.project.track(track).unwrap().automation.iter().find(|l| l.target == target).unwrap().clone();
        assert_eq!(lane(&s).value_at(2 * PPQ), Some(0.5), "straight segment");
        s.seal();
        assert!(s.set_automation_curve(track, target, 0, 0.6));
        assert!(s.set_automation_curve(track, target, 0, 0.9));
        s.seal();
        let mid = lane(&s).value_at(2 * PPQ).unwrap();
        assert!(mid < 0.4, "a positive curve holds low and rises late: {mid}");
        // Moving the point keeps its curve.
        let original = lane(&s).points;
        assert!(s.move_automation_point(track, target, original, 0, 0, 0.3));
        s.seal();
        assert!((lane(&s).points[0].curve - 0.9).abs() < 1e-6);
        // The whole curve drag is one undo step.
        assert_eq!(s.history.undo_label(), Some("Edit automation"));
        assert!(s.delete_automation_point(track, target, 1));
        assert_eq!(lane(&s).points.len(), 2);
        assert!(s.undo());
        assert_eq!(lane(&s).points.len(), 3);
        assert!(!s.delete_automation_point(track, target, 9));
    }

    #[test]
    fn split_cuts_the_clip_under_the_playhead() {
        let mut s = state();
        let (track, place) = s
            .project
            .tracks
            .iter()
            .find_map(|t| t.arrangement.first().filter(|_| t.kind == TrackKind::Instrument).map(|p| (t.id, p.clone())))
            .unwrap();
        let clip = s.project.clips[&place.clip].clone();
        s.selected_track = Some(track);
        s.selected_placement = None;
        let before = s.project.track(track).unwrap().arrangement.len();
        assert_eq!(s.split_at(place.start), 0, "the clip's own edge is not a cut");
        let cut = place.start + BAR;
        assert_eq!(s.split_at(cut + 7), 1, "the cut snaps to the grid");
        let t = s.project.track(track).unwrap();
        assert_eq!(t.arrangement.len(), before + 1);
        let left = &s.project.clips[&place.clip];
        let right_place = t.arrangement.iter().find(|p| p.start == cut).expect("a clip starts at the cut");
        let right = &s.project.clips[&right_place.clip];
        assert_eq!(left.length, BAR);
        assert_eq!((right.offset, right.length), (clip.offset + BAR, clip.length - BAR));
        assert_eq!(s.selected_placement, Some((track, right_place.id)), "the right half is selected");
        assert!(s.undo());
        assert_eq!(s.project.clips[&place.clip].length, clip.length);
        assert_eq!(s.project.track(track).unwrap().arrangement.len(), before);
    }

    #[test]
    fn tracks_are_added_grouped_and_ungrouped() {
        let mut s = state();
        let bass = s.project.tracks.iter().find(|t| t.name == "Bass").unwrap().id;
        s.selected_track = Some(bass);
        let audio = s.add_track(TrackKind::Audio).unwrap();
        assert_eq!(s.project.track_index(audio), s.project.track_index(bass).map(|i| i + 1), "after the selected track");
        // A new track made while inside a group joins it and takes its colour.
        let lead = s.project.tracks.iter().find(|t| t.name == "Lead").unwrap().clone();
        s.selected_track = Some(lead.id);
        let midi = s.add_track(TrackKind::Instrument).unwrap();
        let t = s.project.track(midi).unwrap();
        assert_eq!((t.parent, t.color), (lead.parent, lead.color));
        // A return goes after the other returns and every track gets a send to it.
        let ret = s.add_track(TrackKind::Return).unwrap();
        let ri = s.project.track_index(ret).unwrap();
        assert_eq!(s.project.tracks[ri - 1].kind, TrackKind::Return);
        assert_eq!(s.project.tracks[ri].name, "C Return");
        assert!(s.project.track(bass).unwrap().sends.iter().any(|x| x.target == ret));
        // Group, then ungroup, the bass.
        s.selected_track = Some(bass);
        let group = s.group_selected().unwrap();
        let b = s.project.track(bass).unwrap();
        assert_eq!((b.parent, b.output), (Some(group), Output::Track(group)));
        assert_eq!(s.project.track_index(group).map(|i| i + 1), s.project.track_index(bass), "the group sits right above");
        assert!(s.ungroup_selected());
        let b = s.project.track(bass).unwrap();
        assert_eq!((b.parent, b.output), (None, Output::Main));
        assert!(s.project.track(group).is_none());
    }

    #[test]
    fn duplicating_a_clip_makes_an_independent_copy() {
        let mut s = state();
        let (track, place) = s
            .project
            .tracks
            .iter()
            .find_map(|t| t.arrangement.first().filter(|_| t.kind == TrackKind::Instrument).map(|p| (t.id, p.clone())))
            .expect("the demo has a MIDI clip in the arrangement");
        let length = s.project.clips[&place.clip].length;
        s.selected_notes.clear();
        s.selected_placement = Some((track, place.id));
        assert!(s.duplicate_selection());
        let (_, new_id) = s.selected_placement.unwrap();
        let t = s.project.track(track).unwrap();
        let copy = t.arrangement.iter().find(|p| p.id == new_id).unwrap();
        assert_eq!(copy.start, place.start + length);
        assert_ne!(s.project.clips[&copy.clip].source, s.project.clips[&place.clip].source, "its notes are its own");
        assert!(s.undo());
        assert!(s.project.track(track).unwrap().arrangement.iter().all(|p| p.id != new_id));
    }

    #[test]
    fn quantize_moves_notes_to_the_grid_and_undoes() {
        let mut s = state();
        let clip = lead_clip(&s);
        let id = s.draw_note(clip, 64, 0).unwrap();
        // Nudge it off the grid through a drag.
        let original = s.clip_notes(clip).unwrap().1.notes.iter().find(|n| n.id == id).unwrap().clone();
        s.move_notes(clip, std::slice::from_ref(&original), 50, 0);
        s.seal();
        s.selected_notes = [id].into();
        s.snap = Snap::Sixteenth;
        assert_eq!(s.quantize(clip, 1.), 1);
        let n = s.clip_notes(clip).unwrap().1.notes.iter().find(|n| n.id == id).unwrap().clone();
        assert_eq!(n.start % (PPQ / 4), 0);
        assert_eq!(n.start, 0);
        s.undo();
        let n = s.clip_notes(clip).unwrap().1.notes.iter().find(|n| n.id == id).unwrap().clone();
        assert_eq!(n.start, 50);
    }

    #[test]
    fn scale_snap_uses_the_project_scale() {
        let mut s = state();
        let clip = lead_clip(&s);
        let id = s.draw_note(clip, 61, 0).unwrap(); // C#, not in A minor
        s.selected_notes = [id].into();
        assert_eq!(s.scale_snap(clip), 1);
        let n = s.clip_notes(clip).unwrap().1.notes.iter().find(|n| n.id == id).unwrap().clone();
        assert_eq!(n.key, 60);
        assert_eq!(s.history.undo_label(), Some("Snap to scale"));
    }

    #[test]
    fn a_note_drag_is_one_undo_step() {
        let mut s = state();
        let clip = lead_clip(&s);
        let id = s.draw_note(clip, 60, 0).unwrap();
        let original = vec![s.clip_notes(clip).unwrap().1.notes.iter().find(|n| n.id == id).unwrap().clone()];
        for dt in [10, 40, 120] {
            s.move_notes(clip, &original, dt, 2);
        }
        s.seal();
        let n = s.clip_notes(clip).unwrap().1.notes.iter().find(|n| n.id == id).unwrap().clone();
        assert_eq!((n.start, n.key), (120, 62));
        s.undo();
        let n = s.clip_notes(clip).unwrap().1.notes.iter().find(|n| n.id == id).unwrap().clone();
        assert_eq!((n.start, n.key), (0, 60));
        assert_eq!(s.history.undo_label(), Some("Add note"));
    }

    #[test]
    fn velocity_drag_preserves_relative_values_at_limits_and_undoes_once() {
        let mut s = state();
        let clip = lead_clip(&s);
        let a = s.draw_note(clip, 60, 0).unwrap();
        let b = s.draw_note(clip, 64, PPQ).unwrap();
        let originals: Vec<_> = s.clip_notes(clip).unwrap().1.notes.iter()
            .filter(|n| n.id == a || n.id == b)
            .cloned()
            .map(|mut n| { n.velocity = if n.id == a { 0.4 } else { 0.8 }; n })
            .collect();
        s.set_velocity(clip, &originals, 0.0);
        s.seal();
        s.set_velocity(clip, &originals, 0.1);
        s.set_velocity(clip, &originals, 0.5);
        s.seal();
        let notes = &s.clip_notes(clip).unwrap().1.notes;
        let a_velocity = notes.iter().find(|n| n.id == a).unwrap().velocity;
        let b_velocity = notes.iter().find(|n| n.id == b).unwrap().velocity;
        assert!((a_velocity - 0.6).abs() < 1e-5);
        assert!((b_velocity - 1.0).abs() < 1e-5);
        assert!(s.undo());
        let notes = &s.clip_notes(clip).unwrap().1.notes;
        assert!((notes.iter().find(|n| n.id == a).unwrap().velocity - 0.4).abs() < 1e-5);
        assert!((notes.iter().find(|n| n.id == b).unwrap().velocity - 0.8).abs() < 1e-5);
    }

    #[test]
    fn steps_toggle_notes() {
        let mut s = state();
        let (_, clip) = s.step_clip().expect("demo has a drum clip");
        let count = |s: &AppState| s.clip_notes(clip).unwrap().1.notes.len();
        let before = count(&s);
        // Step 2 of the Ride row is empty in the demo.
        assert!(s.toggle_step(clip, 51, PPQ / 4, PPQ / 4));
        assert_eq!(count(&s), before + 1);
        assert!(s.toggle_step(clip, 51, PPQ / 4, PPQ / 4));
        assert_eq!(count(&s), before);
    }

    #[test]
    fn accepted_proposal_is_one_step() {
        let mut s = state();
        let tracks = s.project.tracks.len();
        let cmds = s.midi_track_commands("Sub", Color::palette(4), &[(33, 0, BAR, 0.8)]);
        let id = s.propose("Add sub", Origin::Agent("test".into()), cmds).unwrap();
        assert_eq!(s.project.tracks.len(), tracks, "a proposal leaves the project alone");
        assert!(s.accept(id));
        assert_eq!(s.project.tracks.len(), tracks + 1);
        s.undo();
        assert_eq!(s.project.tracks.len(), tracks);
    }

    #[test]
    fn fader_law_round_trips() {
        for db in [-60., -24., -6., 0., 3., 6.] {
            assert!((fader_to_db(db_to_fader(db)) - db).abs() < 0.01, "{db}");
        }
        assert_eq!(db_to_fader(0.), 0.75);
    }

    #[test]
    fn parameter_drag_coalesces() {
        let mut s = state();
        let dev = s.project.tracks.iter().flat_map(|t| &t.devices).find(|d| d.name == "EQ").unwrap().id;
        let steps_before = s.history.steps().count();
        for v in [0.1, 0.2, 0.3] {
            s.set_param(dev, 0, v);
        }
        s.seal();
        assert_eq!(s.history.steps().count(), steps_before + 1);
        assert_eq!(s.project.device(dev).unwrap().params[&0], 0.3);
    }
}
