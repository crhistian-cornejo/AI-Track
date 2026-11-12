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
        if index < points.len() {
            points.remove(index);
        }
        lane.points = points;
        lane.insert(AutomationPoint { tick: tick.max(0), value: value.clamp(0., 1.), curve: 0. });
        self.edit_coalesced("automation", "Edit automation", vec![Command::SetAutomation { track, target, lane: Some(lane) }])
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
