//! UI state over the project. [`AppState`] owns the [`Project`] and its
//! [`History`]; every edit the views make goes through one of the methods
//! here, which build [`Command`]s and hand them to `History::apply` (or
//! `apply_coalesced` for drags, sealed on mouse-up). Nothing here touches
//! GPUI, so it is tested headlessly.

use aitrack_model::command::DeviceSlot;
use aitrack_model::*;
use std::collections::{BTreeMap, BTreeSet};

pub const BAR: Tick = 4 * PPQ;

/// What [`AppState::sync_engine`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Synced {
    Nothing,
    /// The project went to the engine.
    Full,
    /// Only parameter values went, by the fast path.
    Fast,
}

/// A take being captured: the tracks it records and its first tick.
#[derive(Clone, Debug, PartialEq)]
pub struct RecView {
    pub start: Tick,
    pub tracks: Vec<TrackId>,
}

/// The audio files of a take that has ended and are still arriving (the
/// engine writes them on a background thread).
#[derive(Debug)]
struct PendingAudio {
    expected: usize,
    got: Vec<crate::bridge::AudioTake>,
    /// When the take was seen to be over, to give up waiting for a file.
    over_since: Option<std::time::Instant>,
    /// The loop the transport was looping over while it recorded.
    looped: Option<(Tick, Tick)>,
}

/// How long to wait for the files of a take once it is over.
const AUDIO_FILES_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// What [`AppState::poll_recording`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecPoll {
    /// Tracks that got a clip (or notes) from a take that just ended.
    pub committed: usize,
    /// Audio clips made from the files of a take.
    pub audio_clips: usize,
    /// Takes that failed (a file that could not be written, one that never arrived).
    pub errors: Vec<String>,
    /// The view of the take in progress changed: repaint.
    pub changed: bool,
}

/// Where the clip of an audio take goes, as `(timeline start, offset into the
/// file, length)`, all in ticks. Recorded in a loop, the file holds every pass
/// one after the other, but the clip fills only the loop, as in Live: it shows
/// the last whole pass (the offset points at it) and the other passes stay in
/// the file. A take that began before the loop runs from where it began to the
/// loop's end; one that never reached the end of the loop is one plain clip.
/// `file` is the length of the whole recording.
fn audio_take_span(start: Tick, file: Tick, looped: Option<(Tick, Tick)>) -> (Tick, Tick, Tick) {
    let Some((ls, le)) = looped.filter(|(a, b)| b > a) else { return (start, 0, file) };
    if start + file <= le {
        return (start, 0, file);
    }
    if start < ls {
        return (start, 0, le - start);
    }
    let (len, first) = (le - ls, le - start);
    // Whole passes in the file after the first (partial, unless it began at the loop start).
    let (done, from) = if first == len { (file / len, 0) } else { ((file - first) / len, first) };
    if done >= 1 { (ls, from + (done - 1) * len, len) } else { (start, 0, le - start) }
}

/// The folder audio takes are written to: `Recordings` next to the saved
/// project, else a folder under the settings until it has a file (Collect All
/// and Save gathers them into the project then).
pub fn recordings_dir(project_file: Option<&std::path::Path>) -> std::path::PathBuf {
    match project_file.and_then(crate::project_io::project_folder) {
        Some(folder) => folder.join("Recordings"),
        None => crate::appearance::config_dir().join("Recordings"),
    }
}

/// One move of a knob, fader, pan or send, as the engine's fast path takes it.
#[derive(Clone, Debug, PartialEq)]
pub enum FastEdit {
    Param { device: DeviceId, param: u32, value: f32 },
    Volume { track: TrackId, db: f32 },
    Pan { track: TrackId, pan: f32 },
    Send { track: TrackId, index: usize, db: f32 },
    /// A Drum/Instrument Rack's chain selector (0..=127).
    Selector { rack: DeviceId, value: u8 },
}

impl FastEdit {
    /// The fast edits `commands` make, if every one of them is such a move.
    /// `project` is the state before them (to see which send changed).
    fn of_all(project: &Project, commands: &[Command]) -> Option<Vec<FastEdit>> {
        let mut out = Vec::new();
        let mut macro_move = false;
        for c in commands {
            match c {
                // A macro moves the parameters mapped to it, by the same rule the
                // engine compiles (`min + (max - min) * value`): live, no recompile.
                Command::SetMacro { rack, index, value } => {
                    let Some(DeviceKind::Rack(r)) = project.device(*rack).map(|d| &d.kind) else { return None };
                    let m = r.macros.get(*index)?;
                    let v = value.clamp(0., 1.);
                    out.extend(m.mappings.iter().map(|x| FastEdit::Param { device: x.device, param: x.param, value: x.min + (x.max - x.min) * v }));
                    macro_move = true;
                }
                Command::SetRackSelector { rack, value } => out.push(FastEdit::Selector { rack: *rack, value: *value }),
                Command::SetParam { device, param, value: Some(v) } => out.push(FastEdit::Param { device: *device, param: *param, value: *v }),
                Command::SetTrack { id, change: TrackProp::VolumeDb(db) } => out.push(FastEdit::Volume { track: *id, db: *db }),
                Command::SetTrack { id, change: TrackProp::Pan(pan) } => out.push(FastEdit::Pan { track: *id, pan: *pan }),
                Command::SetTrack { id, change: TrackProp::Sends(new) } => {
                    let old = &project.track(*id)?.sends;
                    // Only level changes: same sends, same order, same routing.
                    if old.len() != new.len() || old.iter().zip(new).any(|(a, b)| a.target != b.target || a.pre_fader != b.pre_fader) {
                        return None;
                    }
                    out.extend(old.iter().zip(new).enumerate().filter(|(_, (a, b))| a.level_db != b.level_db).map(|(index, (_, b))| FastEdit::Send { track: *id, index, db: b.level_db }));
                }
                _ => return None,
            }
        }
        // A macro nothing is mapped to has nothing to tell the engine, but the move still counts.
        (!out.is_empty() || macro_move).then_some(out)
    }

    fn send(&self, engine: &mut dyn crate::bridge::EngineBridge) -> bool {
        match *self {
            FastEdit::Param { device, param, value } => engine.fast_param(device, param, value),
            FastEdit::Volume { track, db } => engine.fast_track_volume(track, db),
            FastEdit::Pan { track, pan } => engine.fast_track_pan(track, pan),
            FastEdit::Send { track, index, db } => engine.fast_send_level(track, index, db),
            FastEdit::Selector { rack, value } => engine.fast_rack_selector(rack, value),
        }
    }

    /// What the edit writes to: a later edit of the same thing replaces it.
    fn target(&self) -> (u8, u64, u64) {
        match *self {
            FastEdit::Param { device, param, .. } => (0, device.0, u64::from(param)),
            FastEdit::Volume { track, .. } => (1, track.0, 0),
            FastEdit::Pan { track, .. } => (2, track.0, 0),
            FastEdit::Send { track, index, .. } => (3, track.0, index as u64),
            FastEdit::Selector { rack, .. } => (4, rack.0, 0),
        }
    }
}

/// The fast edits since the engine was last told, and the revisions they span.
#[derive(Debug, Default)]
struct FastLog {
    from: u64,
    upto: u64,
    edits: Vec<FastEdit>,
    /// At least one fast move was recorded (it may have had nothing to send).
    noted: bool,
}

impl FastLog {
    /// Record an edit that took the project from revision `before` to `after`.
    /// `None` (not a fast move) empties the log: the engine needs the project.
    fn note(&mut self, before: u64, after: u64, fast: Option<Vec<FastEdit>>) {
        let Some(fast) = fast else {
            *self = Self::default();
            return;
        };
        if !self.noted || self.upto != before {
            *self = FastLog { from: before, upto: after, edits: Vec::new(), noted: true };
        }
        self.upto = after;
        for e in fast {
            self.edits.retain(|x| x.target() != e.target());
            self.edits.push(e);
        }
    }

    /// The edits, when they explain every revision from `synced` to `now`.
    fn since(&self, synced: Option<u64>, now: u64) -> Option<Vec<FastEdit>> {
        (self.noted && synced == Some(self.from) && self.upto == now).then(|| self.edits.clone())
    }
}

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

/// Which per-note lane is shown under the piano roll grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoteLane {
    #[default]
    Velocity,
    Chance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Draw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    Off,
    /// The grid follows the zoom of the view (see [`AppState::snap_step`]).
    Adaptive,
    Bar,
    Beat,
    Eighth,
    Sixteenth,
    ThirtySecond,
    TripletSixteenth,
}

impl Snap {
    pub const ALL: [Snap; 8] =
        [Self::Off, Self::Adaptive, Self::Bar, Self::Beat, Self::Eighth, Self::Sixteenth, Self::ThirtySecond, Self::TripletSixteenth];

    pub fn ticks(self) -> Tick {
        match self {
            Self::Off => 1,
            // Without a view to ask, the adaptive grid counts as sixteenths.
            Self::Adaptive => PPQ / 4,
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
            Self::Adaptive => "Adaptive",
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

/// A time range in one automation lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoRange {
    pub track: TrackId,
    pub target: AutomationTarget,
    pub start: Tick,
    pub end: Tick,
}

/// What the transform handles of a selected automation range do to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AutoXform {
    /// Vertical stretch about the value `anchor`.
    Scale { anchor: f32, factor: f32 },
    /// A tilt: the offset at the left and right edge (normalised).
    Skew { left: f32, right: f32 },
    /// The range becomes `start..end` (an edge dragged).
    Stretch { start: Tick, end: Tick },
}

impl AutoRange {
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

pub struct AppState {
    pub project: Project,
    pub history: History,
    /// The project before the first frame of a tempo drag, the revision after
    /// its last frame and the clips it has changed: every frame scales the
    /// samples from the start, so rounding does not pile up. See [`AppState::set_tempo`].
    tempo_base: Option<(Project, u64, BTreeSet<ClipId>)>,
    /// The knob, fader, pan and send moves of a drag the engine has not been
    /// told about, and whether it is a revision behind the project because of them.
    fast: FastLog,
    engine_behind: bool,
    /// Notes of the take being played that are complete so far, and where the
    /// take began and on which tracks (the arrangement draws the clip growing).
    pub rec_notes: Vec<crate::bridge::RecNote>,
    pub rec_view: Option<RecView>,
    /// Audio files of the take being finished: how many are expected and what has come in.
    pending_audio: Option<PendingAudio>,
    /// Audio files this session's takes wrote: the ones a project that is never
    /// saved leaves behind in the settings folder (see `project_io::remove_orphan_takes`).
    pub session_takes: BTreeSet<std::path::PathBuf>,
    pub view: MainView,
    pub bottom_tab: BottomTab,
    pub tool: Tool,
    pub snap: Snap,
    pub selected_track: Option<TrackId>,
    pub selected_clip: Option<ClipId>,
    pub selected_placement: Option<(TrackId, PlacementId)>,
    pub selected_notes: BTreeSet<NoteId>,
    /// Tracks whose automation lanes are shown (the header's automation toggle).
    pub automation_shown: BTreeSet<TrackId>,
    /// The lanes each track shows, top to bottom, while its automation is
    /// shown (kept while hidden, as Live remembers them). See [`AppState::shown_lanes`].
    pub auto_lanes: BTreeMap<TrackId, Vec<AutomationTarget>>,
    /// Which tracks show their take lanes, and the range picked on one.
    pub takes: crate::take_lanes::TakeView,
    /// Live's Automation Arm: while recording, moving a control in a
    /// plugin's window writes its automation.
    pub automation_arm: bool,
    /// The colour options of Preferences > Appearance that decide how new tracks and clips are coloured.
    pub color_prefs: ColorPrefs,
    /// Gestures being written as automation: per (device, param), the last tick written.
    pub auto_writes: BTreeMap<(DeviceId, u32), Tick>,
    pub arrange: ArrangeView,
    pub piano: PianoView,
    /// Selection, insert marker, clipboard and grid of the editing commands.
    pub ed: crate::editing::EditState,
    /// The time range dragged out in an automation lane (Live's automation
    /// selection). Cut, Copy, Paste, Duplicate and Delete act on it while it
    /// is set; `start == end` is a caret, where Paste lands.
    pub auto_sel: Option<AutoRange>,
    /// What Copy took from an automation range: breakpoints from tick 0 and the span.
    pub auto_clip: Option<(Vec<AutomationPoint>, Tick)>,
    /// The last minutes of knob moves and played notes, for Capture Automation / Capture MIDI.
    pub capture: crate::capture::Capture,
    /// Length new notes get (the last drawn or resized length, FL-style).
    pub note_length: Tick,
    /// Ableton-style Scale Mode in the piano roll: fold vertically to show only keys in the active scale.
    pub piano_scale_fold: bool,
    /// Per-note lane under the piano roll (Velocity or Chance probability).
    pub note_lane: NoteLane,
    pub last_error: Option<String>,
}

/// How new tracks and clips are coloured (from the Appearance settings).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorPrefs {
    /// New tracks take the next palette colour (off: a neutral grey).
    pub auto_track: bool,
    /// New clips take the colour of the track they land on.
    pub clips_take_track: bool,
}

impl ColorPrefs {
    /// The neutral grey tracks get when colours are not auto-assigned.
    pub const DEFAULT_TRACK: Color = Color(0x9AA0A6);
}

impl Default for ColorPrefs {
    fn default() -> Self {
        Self { auto_track: true, clips_take_track: true }
    }
}

impl AppState {
    pub fn new(project: Project) -> Self {
        let first_track = project.content_tracks().next().map(|t| t.id);
        let mut s = Self {
            project,
            history: History::default(),
            view: MainView::Arrangement,
            bottom_tab: BottomTab::PianoRoll,
            tool: Tool::Draw,
            snap: Snap::Adaptive,
            selected_track: first_track,
            selected_clip: None,
            selected_placement: None,
            selected_notes: BTreeSet::new(),
            automation_shown: BTreeSet::new(),
            auto_lanes: BTreeMap::new(),
            takes: Default::default(),
            tempo_base: None,
            fast: FastLog::default(),
            engine_behind: false,
            rec_notes: Vec::new(),
            rec_view: None,
            pending_audio: None,
            session_takes: BTreeSet::new(),
            automation_arm: false,
            color_prefs: ColorPrefs::default(),
            auto_writes: BTreeMap::new(),
            arrange: ArrangeView { px_per_beat: 7., scroll_x: 0., scroll_y: 0. },
            piano: PianoView { px_per_beat: 64., row_h: 12., scroll_x: 0., scroll_y: 0. },
            ed: Default::default(),
            auto_sel: None,
            auto_clip: None,
            capture: Default::default(),
            note_length: PPQ / 2,
            piano_scale_fold: true,
            note_lane: NoteLane::Velocity,
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
                s.auto_lanes.insert(t.id, t.automation.iter().map(|l| l.target).collect());
            }
        }
        s.show_saved_take_lanes();
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
        // The clips this edit lands replace what they cover, in this step. A
        // drag in flight is left alone: it settles when it ends, not when
        // some other edit (an analysis that finished, say) happens by.
        let commands = self.overwriting(commands);
        match self.history.apply(&mut self.project, label, origin, commands) {
            Ok(()) => true,
            Err(e) => {
                self.last_error = Some(e.to_string());
                false
            }
        }
    }

    /// Collect what the engine recorded: while a take is captured keep its
    /// finished notes (the arrangement shows the clip growing); when it ends,
    /// turn it into clips ([`commit_take`](Self::commit_take)).
    pub fn poll_recording(&mut self, engine: &mut dyn crate::bridge::EngineBridge) -> RecPoll {
        self.poll_recording_at(engine, std::time::Instant::now())
    }

    pub fn poll_recording_at(&mut self, engine: &mut dyn crate::bridge::EngineBridge, now: std::time::Instant) -> RecPoll {
        let state = engine.recording_state();
        let mut poll = RecPoll::default();
        if state.capturing {
            self.rec_notes.extend(engine.take_recorded_notes());
            let view = Some(RecView { start: state.start_tick, tracks: state.tracks.clone() });
            poll.changed = self.rec_view != view || !self.rec_notes.is_empty();
            self.rec_view = view;
            if !state.audio_tracks.is_empty() {
                let pending = self.pending_audio.get_or_insert(PendingAudio { expected: 0, got: Vec::new(), over_since: None, looped: None });
                pending.expected = pending.expected.max(state.audio_tracks.len());
                pending.over_since = None;
                pending.looped = self.project.loop_region.filter(|_| engine.transport().looping);
            }
        } else if self.rec_view.take().is_some() {
            poll.changed = true;
        }
        if let Some(mut take) = engine.finish_midi_recording() {
            let mut all = std::mem::take(&mut self.rec_notes);
            all.append(&mut take.notes);
            take.notes = all;
            self.rec_view = None;
            poll.committed = self.commit_take(&take);
            poll.changed = true;
        }
        // The audio files arrive a moment after the take ends: collect them and
        // make the clips once the ones expected are in (or the wait is over).
        let fresh = engine.finish_audio_recording();
        if !fresh.is_empty() {
            self.pending_audio.get_or_insert(PendingAudio { expected: fresh.len(), got: Vec::new(), over_since: None, looped: None }).got.extend(fresh);
        }
        if let Some(pending) = &mut self.pending_audio
            && !state.capturing
        {
            let over = state.takes_started == state.takes_finished;
            if over {
                pending.over_since.get_or_insert(now);
            }
            let complete = over && !pending.got.is_empty() && pending.got.len() >= pending.expected;
            let gave_up = pending.over_since.is_some_and(|t| now.saturating_duration_since(t) > AUDIO_FILES_WAIT);
            if complete || gave_up {
                let pending = self.pending_audio.take().expect("pending");
                if pending.got.len() < pending.expected {
                    poll.errors.push(format!("{} of {} audio recordings never arrived", pending.expected - pending.got.len(), pending.expected));
                }
                let (clips, errors) = self.commit_audio_takes(&pending.got, pending.looped);
                poll.audio_clips = clips;
                poll.errors.extend(errors);
                poll.changed = true;
            }
        }
        poll
    }

    /// Turn the files of a take into audio clips: one source, clip and
    /// placement per file, at the tick the take began, in one undo step
    /// ("Record audio") that replaces what the clips land on like any clip.
    /// A file that failed to write is reported (and used if any of it made it).
    /// A take of a track that has no input now is left out.
    pub fn commit_audio_takes(&mut self, takes: &[crate::bridge::AudioTake], looped: Option<(Tick, Tick)>) -> (usize, Vec<String>) {
        let (mut cmds, mut errors, mut clips) = (Vec::new(), Vec::new(), 0);
        let bpm = self.project.tempo.initial_bpm();
        for t in takes {
            let Some(track) = self.project.track(t.track).filter(|x| x.kind == TrackKind::Audio && x.input != InputSource::None) else { continue };
            let name = track.name.clone();
            let color = track.color;
            self.session_takes.insert(t.path.clone());
            if let Some(e) = &t.error {
                errors.push(format!("Recording on {name} failed: {e}"));
            }
            if t.frames == 0 {
                continue;
            }
            let file = AudioSource { path: t.path.clone(), hash: None, sample_rate: t.sample_rate, channels: t.channels, frames: t.frames, warp: Warp::default(), key: None };
            let file_ticks = file.length_ticks(bpm).max(PPQ);
            let (start, offset, length) = audio_take_span(t.start_tick.max(0), file_ticks, looped);
            let (source, clip, placement) = (self.project.alloc_source_id(), self.project.alloc_clip_id(), self.project.alloc_placement_id());
            let mut c = Clip::new("Recorded", color, source, length);
            c.offset = offset;
            cmds.push(Command::AddSource { id: source, source: Box::new(Source::Audio(file)) });
            // With the track's take lanes shown, every whole pass of a loop recording is a take lane.
            if let Some((ls, len, offsets)) = self.takes.shown.contains(&t.track).then(|| crate::take_lanes::audio_passes(t.start_tick.max(0), file_ticks, looped)).flatten() {
                cmds.extend(self.audio_pass_lanes(t.track, source, color, ls, len, &offsets));
            }
            cmds.push(Command::AddClip { id: clip, clip: Box::new(c) });
            cmds.push(Command::PlaceClip { track: t.track, placement: Placement { id: placement, clip, start, lane: 0 } });
            clips += 1;
        }
        if cmds.is_empty() {
            return (0, errors);
        }
        self.seal();
        let ok = self.edit("Record audio", cmds);
        self.seal();
        (if ok { clips } else { 0 }, errors)
    }

    /// Tell the engine each audio track's input and monitor mode where the
    /// project differs from what `sent` says it was told (a track just made,
    /// a project just opened, a mixer choice). Returns how many tracks it updated.
    pub fn sync_track_io(&self, sent: &mut BTreeMap<TrackId, (InputSource, Monitor)>, engine: &mut dyn crate::bridge::EngineBridge) -> usize {
        sent.retain(|id, _| self.project.track(*id).is_some_and(|t| t.kind == TrackKind::Audio));
        let mut n = 0;
        for t in self.project.tracks.iter().filter(|t| t.kind == TrackKind::Audio) {
            let now = (t.input, t.monitor);
            if sent.get(&t.id) != Some(&now) {
                engine.set_track_input(t.id, t.input);
                engine.set_track_monitor(t.id, t.monitor);
                sent.insert(t.id, now);
                n += 1;
            }
        }
        n
    }

    /// Turn a finished take into clips, one undo step for every armed track
    /// that played something. Without a loop each track gets a new clip from
    /// where the take began; looping, every pass lands in one clip over the
    /// loop, and if a clip already sits there (an earlier take) the notes are
    /// added to it: overdub. Returns how many tracks got notes.
    pub fn commit_take(&mut self, take: &crate::bridge::MidiTake) -> usize {
        self.commit_take_as(take, "Record MIDI")
    }

    /// [`commit_take`](Self::commit_take) with the undo step's name.
    pub fn commit_take_as(&mut self, take: &crate::bridge::MidiTake, label: &str) -> usize {
        // The engine reports every note at its place on the timeline, inside
        // the loop for each pass (`RecordedNote.pass` only says which pass it was
        // played in) and `passes` > 1 means the loop wrapped at least once. So
        // all the passes share one stretch: the loop, or from where the take
        // began when that was before the loop (an intro played into it).
        let looped = take.passes > 1 && self.project.loop_region.is_some();
        let (start, len) = match self.project.loop_region.filter(|_| looped) {
            Some((a, b)) => (take.start_tick.min(a), b - take.start_tick.min(a)),
            None => {
                let last = take.notes.iter().map(|n| n.start_tick + n.length_ticks).max().unwrap_or(0);
                (take.start_tick, (take.end_tick.max(last) - take.start_tick).max(PPQ))
            }
        };
        let mut by_track: BTreeMap<TrackId, Vec<&crate::bridge::RecNote>> = BTreeMap::new();
        for n in &take.notes {
            if self.project.track(n.track).is_some_and(|t| t.kind.has_clips()) {
                by_track.entry(n.track).or_default().push(n);
            }
        }
        let mut cmds = Vec::new();
        let tracks = by_track.len();
        for (track, played) in by_track {
            let mut notes_by_pass = Vec::new();
            for n in played {
                let at = (n.start_tick - start).clamp(0, len - 1);
                notes_by_pass.push((n.pass, Note::new(self.project.alloc_note_id(), n.key, at, n.length_ticks.clamp(1, (len - at).max(1)), n.velocity.clamp(0.01, 1.))));
            }
            let notes: Vec<Note> = notes_by_pass.iter().map(|n| n.1.clone()).collect();
            // With the track's take lanes shown, each pass of a loop recording is a take lane.
            if looped && self.takes.shown.contains(&track) {
                let mut passes: BTreeMap<u32, Vec<Note>> = BTreeMap::new();
                for n in &notes_by_pass {
                    passes.entry(n.0).or_default().push(n.1.clone());
                }
                if passes.len() >= 2 {
                    let color = self.project.track(track).map_or(Color(0), |t| t.color);
                    let passes: Vec<Vec<Note>> = passes.into_values().collect();
                    cmds.extend(self.midi_pass_lanes(track, color, start, len, &passes));
                    continue;
                }
            }
            // A clip over the same stretch, from an earlier take: add to it.
            let existing = looped
                .then(|| {
                    self.project.track(track)?.arrangement.iter().filter(|p| p.start == start && p.lane == 0).find_map(|p| {
                        let c = self.project.clips.get(&p.clip).filter(|c| c.length == len)?;
                        matches!(self.project.sources.get(&c.source), Some(Source::Notes(_))).then_some(c.source)
                    })
                })
                .flatten();
            if let Some(source) = existing {
                cmds.push(Command::AddNotes { source, notes });
                continue;
            }
            let Some(color) = self.project.track(track).map(|t| t.color) else { continue };
            let (source, clip, placement) = (self.project.alloc_source_id(), self.project.alloc_clip_id(), self.project.alloc_placement_id());
            let mut c = Clip::new("Recorded", color, source, len);
            c.looping = None;
            cmds.push(Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource { notes })) });
            cmds.push(Command::AddClip { id: clip, clip: Box::new(c) });
            cmds.push(Command::PlaceClip { track, placement: Placement { id: placement, clip, start, lane: 0 } });
        }
        if cmds.is_empty() {
            return 0;
        }
        self.seal();
        let ok = self.edit(label, cmds);
        self.seal();
        if ok { tracks } else { 0 }
    }

    /// Hand the engine what changed since `synced` (the revision it last got).
    /// A drag of knobs, faders, pans and sends goes through the engine's fast
    /// path (no project snapshot, no compile); anything else sends the project.
    /// The engine is then a revision behind until [`settle_engine`](Self::settle_engine).
    pub fn sync_engine(&mut self, engine: &mut dyn crate::bridge::EngineBridge, synced: &mut Option<u64>) -> Synced {
        let rev = self.revision();
        if *synced == Some(rev) {
            return Synced::Nothing;
        }
        if let Some(edits) = self.fast.since(*synced, rev)
            && edits.iter().all(|e| e.send(engine))
        {
            self.fast = FastLog::default();
            *synced = Some(rev);
            self.engine_behind = true;
            return Synced::Fast;
        }
        self.fast = FastLog::default();
        engine.update_project(&self.project);
        *synced = Some(rev);
        self.engine_behind = false;
        Synced::Full
    }

    /// A drag has settled: give the engine the project itself, so what it
    /// holds matches (this costs no compile either). `true` if it was behind.
    pub fn settle_engine(&mut self, engine: &mut dyn crate::bridge::EngineBridge) -> bool {
        std::mem::take(&mut self.engine_behind) && {
            engine.update_project(&self.project);
            true
        }
    }

    #[cfg(test)]
    pub fn engine_is_behind(&self) -> bool {
        self.engine_behind
    }

    /// Part of a continuous gesture: merges with the previous step of the same `key`.
    pub fn edit_coalesced(&mut self, key: &str, label: &str, commands: Vec<Command>) -> bool {
        self.note_landed(&commands);
        // Control moves while the transport rolls are kept for Capture Automation.
        if !commands.is_empty() {
            let now = std::time::Instant::now();
            for (track, target, value) in crate::capture::moves_of(&self.project, &commands) {
                self.capture.note_move(now, track, target, value);
            }
        }
        // A knob, fader, pan or send move can go straight to the engine ([`FastEdit`]).
        let (before, fast) = (self.revision(), FastEdit::of_all(&self.project, &commands));
        match self.history.apply_coalesced(&mut self.project, key, label, commands) {
            Ok(()) => {
                self.fast.note(before, self.revision(), fast);
                true
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
                false
            }
        }
    }

    /// End of a gesture: the clips it moved or made longer replace what they
    /// were left on (part of the gesture's undo step), and the next drag
    /// starts a fresh step.
    pub fn seal(&mut self) {
        let settle = self.settle_gesture();
        if let Err(e) = self.history.amend(&mut self.project, settle) {
            self.last_error = Some(e.to_string());
        }
        self.history.seal();
        self.tempo_base = None;
    }

    pub fn undo(&mut self) -> bool {
        self.ed.landed.clear();
        let done = self.history.undo(&mut self.project).unwrap_or(false);
        self.prune_selection();
        done
    }

    pub fn redo(&mut self) -> bool {
        self.ed.landed.clear();
        let done = self.history.redo(&mut self.project).unwrap_or(false);
        self.prune_selection();
        done
    }

    pub(crate) fn prune_selection(&mut self) {
        self.prune_edit_selection();
        if self.auto_sel.is_some_and(|r| self.project.track(r.track).is_none()) {
            self.auto_sel = None;
        }
        let p = &self.project;
        self.automation_shown.retain(|t| p.track(*t).is_some());
        self.auto_lanes.retain(|t, _| p.track(*t).is_some());
        self.prune_take_view();
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

    /// The scale tools and highlighting use for a clip: its own, else its track's, else the project's.
    pub fn clip_scale(&self, clip: Option<ClipId>) -> Option<Scale> {
        let track_scale = clip
            .and_then(|c| self.clip_track(c))
            .or(self.selected_track)
            .and_then(|id| self.project.track(id))
            .and_then(|t| t.scale);
        clip.and_then(|c| self.project.clips.get(&c))
            .and_then(|c| c.scale)
            .or(track_scale)
            .or(self.project.scale)
    }

    /// Active scale when Scale Fold mode is enabled in the Piano Roll (off on drum racks).
    pub fn scale_folded(&self, clip: Option<ClipId>) -> Option<Scale> {
        if !self.piano_scale_fold {
            return None;
        }
        let scale = self.clip_scale(clip)?;
        let drum = clip
            .and_then(|c| self.clip_track(c))
            .or(self.selected_track)
            .and_then(|id| self.project.track(id))
            .is_some_and(is_drum_track);
        (!drum).then_some(scale)
    }

    /// Descending list of MIDI keys rendered in the piano roll (only scale notes when Scale mode folds).
    pub fn piano_rows(&self, clip: Option<ClipId>) -> Vec<u8> {
        if let Some(scale) = self.scale_folded(clip) {
            (0..=127u8).rev().filter(|k| scale.contains(*k)).collect()
        } else {
            (0..=127u8).rev().collect()
        }
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
        let start = self.snap_floor(crate::editing::GridView::Piano, tick.max(0));
        self.ed.area = crate::editing::Area::Piano;
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
    #[cfg(test)]
    pub fn move_notes(&mut self, clip: ClipId, originals: &[Note], dt: Tick, dk: i32) -> bool {
        self.move_notes_scale(clip, originals, dt, dk, false)
    }

    /// Move `originals` by `dt` ticks and either `dsteps` scale degrees (when `diatonic` and a scale exists) or semitones.
    pub fn move_notes_scale(&mut self, clip: ClipId, originals: &[Note], dt: Tick, dsteps: i32, diatonic: bool) -> bool {
        let Some((source, _)) = self.clip_notes(clip) else { return false };
        let min_start = originals.iter().map(|n| n.start).min().unwrap_or(0);
        let dt = dt.max(-min_start);
        let scale = diatonic.then(|| self.clip_scale(Some(clip))).flatten();
        let notes = originals
            .iter()
            .map(|n| {
                let key = match scale {
                    Some(s) => step_note_in_scale(s, n.key, dsteps),
                    None => (n.key as i32 + dsteps).clamp(0, 127) as u8,
                };
                Note { start: n.start + dt, key, ..n.clone() }
            })
            .collect();
        self.edit_coalesced("notes-move", "Move notes", vec![Command::UpdateNotes { source, notes }])
    }

    /// Change the length of `originals` by `dl` ticks (one coalesced drag).
    pub fn resize_notes(&mut self, clip: ClipId, originals: &[Note], dl: Tick) -> bool {
        let Some((source, _)) = self.clip_notes(clip) else { return false };
        let min_len = self.grid().clamp(PPQ / 16, PPQ / 4);
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

    /// Set playback chance (probability 0..=1) of `originals` offset by `dp` (one coalesced drag).
    pub fn set_probability(&mut self, clip: ClipId, originals: &[Note], dp: f32) -> bool {
        let Some((source, _)) = self.clip_notes(clip) else { return false };
        if originals.is_empty() { return false }
        let dp = probability_delta(originals, dp);
        let notes = originals.iter().map(|n| Note { probability: (n.probability + dp).clamp(0., 1.), ..n.clone() }).collect();
        self.edit_coalesced("notes-chance", "Change chance", vec![Command::UpdateNotes { source, notes }])
    }

    /// Nudge probability of selected notes (or all notes in `clip`) by `delta`, in one undo step.
    pub fn nudge_probability(&mut self, clip: ClipId, delta: f32) -> bool {
        let Some((source, notes)) = self.tool_targets(clip) else { return false };
        if notes.is_empty() {
            return false;
        }
        let changed: Vec<Note> = notes.into_iter().map(|n| Note { probability: (n.probability + delta).clamp(0., 1.), ..n }).collect();
        self.seal();
        let ok = self.edit("Change chance", vec![Command::UpdateNotes { source, notes: changed }]);
        self.seal();
        ok
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
        let grid = self.grid();
        let s = strength.clamp(0., 1.);
        let changed: Vec<Note> = notes
            .into_iter()
            .filter_map(|n| {
                let target = (n.start + grid / 2).div_euclid(grid) * grid;
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

    /// Build scale-aware chords (`Triad`, `Seventh`, `Ninth`, `Sus2`, `Sus4`, `Power`) on the target notes (or at the marker if empty).
    pub fn build_chord(&mut self, clip: ClipId, kind: ChordKind) -> usize {
        let Some((source, src)) = self.clip_notes(clip) else { return 0 };
        let scale = self.clip_scale(Some(clip));
        let existing: BTreeSet<(Tick, u8)> = src.notes.iter().map(|n| (n.start, n.key)).collect();
        let anchors: Vec<Note> = {
            let mut base = if self.selected_notes.is_empty() {
                if src.notes.is_empty() {
                    let root_note = scale.map_or(60, |s| (s.root.0 + 60).min(127));
                    vec![Note::new(self.project.alloc_note_id(), root_note, self.ed.piano_marker.max(0), self.note_length.max(PPQ / 4), 0.85)]
                } else {
                    src.notes.clone()
                }
            } else {
                src.notes.iter().filter(|n| self.selected_notes.contains(&n.id)).cloned().collect()
            };
            // Keep one root anchor (lowest pitch) per unique start tick so applying twice doesn't explode stacks.
            base.sort_by_key(|n| (n.start, n.key));
            base.dedup_by_key(|n| n.start);
            base
        };
        let mut added = Vec::new();
        for anchor in &anchors {
            if !existing.contains(&(anchor.start, anchor.key)) {
                added.push(anchor.clone());
            }
            for &step in kind.steps(scale) {
                let key = match scale {
                    Some(s) => step_note_in_scale(s, anchor.key, step),
                    None => (anchor.key as i32 + step).clamp(0, 127) as u8,
                };
                if key == anchor.key
                    || existing.contains(&(anchor.start, key))
                    || added.iter().any(|n| n.start == anchor.start && n.key == key)
                {
                    continue;
                }
                added.push(Note {
                    id: self.project.alloc_note_id(),
                    key,
                    ..anchor.clone()
                });
            }
        }
        let count = added.len();
        if count > 0 {
            let new_ids: BTreeSet<NoteId> = added.iter().map(|n| n.id).collect();
            if self.edit(kind.edit_label(), vec![Command::AddNotes { source, notes: added }]) {
                self.selected_notes.extend(new_ids);
            }
        }
        count
    }

    /// Invert each chord (`dir > 0` moves its lowest note up an octave, `dir < 0` moves its highest note down an octave).
    pub fn invert_chords(&mut self, clip: ClipId, dir: i32) -> usize {
        let Some((source, notes)) = self.tool_targets(clip) else { return 0 };
        let mut by_start: BTreeMap<Tick, Vec<Note>> = BTreeMap::new();
        for n in notes {
            by_start.entry(n.start).or_default().push(n);
        }
        let mut changed = Vec::new();
        for (_, mut group) in by_start {
            if group.len() < 2 {
                continue;
            }
            group.sort_by_key(|n| n.key);
            if dir >= 0 {
                let lo = group.first().unwrap().clone();
                if lo.key <= 115 {
                    changed.push(Note { key: lo.key + 12, ..lo });
                }
            } else {
                let hi = group.last().unwrap().clone();
                if hi.key >= 12 {
                    changed.push(Note { key: hi.key - 12, ..hi });
                }
            }
        }
        let count = changed.len();
        if count > 0 {
            self.edit("Invert chord", vec![Command::UpdateNotes { source, notes: changed }]);
        }
        count
    }

    /// Generate a 4-bar diatonic chord progression using the clip/track/project key (defaults to C Minor if unset).
    pub fn generate_scale_progression(&mut self, clip: ClipId) -> usize {
        let Some((source, src)) = self.clip_notes(clip) else { return 0 };
        let scale = self.clip_scale(Some(clip)).unwrap_or(Scale::new(Key(0), ScaleKind::Minor));
        let bar = PPQ * 4;
        // Scale degrees (0-based): Minor-family -> i - VI - III - VII; Major-family -> I - V - vi - IV.
        let progression: [usize; 4] = match scale.kind {
            ScaleKind::Major | ScaleKind::Lydian | ScaleKind::Mixolydian | ScaleKind::MajorPentatonic => [0, 4, 5, 3],
            _ => [0, 5, 2, 6],
        };
        let old_ids: Vec<NoteId> = if self.selected_notes.is_empty() {
            Vec::new()
        } else {
            self.selected_notes.iter().copied().collect()
        };
        let mut cmds = Vec::new();
        if !old_ids.is_empty() {
            cmds.push(Command::RemoveNotes { source, ids: old_ids });
        }
        let start_tick = if src.notes.is_empty() { 0 } else { self.ed.piano_marker.max(0) };
        let existing: BTreeSet<(Tick, u8)> = src.notes.iter().map(|n| (n.start, n.key)).collect();
        let mut added = Vec::new();
        for (i, deg) in progression.into_iter().enumerate() {
            let root = scale.degree_to_note(deg as i32, 3);
            let tick = start_tick + i as Tick * bar;
            for step in [0, 2, 4] {
                let key = step_note_in_scale(scale, root, step);
                if existing.contains(&(tick, key)) {
                    continue;
                }
                added.push(Note::new(self.project.alloc_note_id(), key, tick, bar - PPQ / 16, 0.78));
            }
        }
        let count = added.len();
        let new_ids: BTreeSet<NoteId> = added.iter().map(|n| n.id).collect();
        if !added.is_empty() {
            cmds.push(Command::AddNotes { source, notes: added });
        }
        if !cmds.is_empty() && self.edit("Chord progression", cmds) {
            self.selected_notes = new_ids;
        }
        count
    }

    /// Turn target notes or chords into rhythmic arpeggio steps at the current snap grid.
    pub fn arpeggiate(&mut self, clip: ClipId, pattern: ArpPattern) -> usize {
        let Some((source, targets)) = self.tool_targets(clip) else { return 0 };
        if targets.is_empty() {
            return 0;
        }
        let step = self.grid().max(PPQ / 16);
        let scale = self.clip_scale(Some(clip));
        let mut by_start: BTreeMap<Tick, Vec<Note>> = BTreeMap::new();
        for n in &targets {
            by_start.entry(n.start).or_default().push(n.clone());
        }
        let mut added = Vec::new();
        for (start, mut chord) in by_start {
            chord.sort_by_key(|n| n.key);
            let total_len = chord.iter().map(|n| n.length).max().unwrap_or(step).max(step);
            let vel = chord.iter().map(|n| n.velocity).sum::<f32>() / chord.len() as f32;
            let muted = chord.iter().all(|n| n.muted);
            // If only a single note is in the group, build a 1-3-5-8 pattern in scale (or +4,+7,+12).
            let mut pitches: Vec<u8> = if chord.len() == 1 {
                let base = chord[0].key;
                match scale {
                    Some(s) => [0, 2, 4, 7].into_iter().map(|st| step_note_in_scale(s, base, st)).collect(),
                    None => [0, 4, 7, 12].into_iter().map(|st| (base as i32 + st).clamp(0, 127) as u8).collect(),
                }
            } else {
                chord.into_iter().map(|n| n.key).collect()
            };
            pitches.dedup();
            if pattern == ArpPattern::Down {
                pitches.reverse();
            } else if pattern == ArpPattern::UpDown && pitches.len() > 2 {
                let mut rev = pitches[1..pitches.len() - 1].to_vec();
                rev.reverse();
                pitches.extend(rev);
            }
            let steps = ((total_len + step - 1) / step).max(1) as usize;
            let mut tick = start;
            for idx in 0..steps {
                let end = (start + total_len).min(tick + step);
                let len = (end - tick).max(PPQ / 16);
                let key = pitches[idx % pitches.len()];
                added.push(Note { muted, ..Note::new(self.project.alloc_note_id(), key, tick, len, vel) });
                tick += step;
            }
        }
        let remove_ids: Vec<NoteId> = targets.into_iter().map(|n| n.id).collect();
        let new_ids: BTreeSet<NoteId> = added.iter().map(|n| n.id).collect();
        let count = added.len();
        if count > 0
            && self.edit(
                "Arpeggiate",
                vec![Command::RemoveNotes { source, ids: remove_ids }, Command::AddNotes { source, notes: added }],
            )
        {
            self.selected_notes = new_ids;
        }
        count
    }

    /// Offset notes in each chord sequentially (`PPQ / 32` per voice) to simulate a human strum.
    pub fn strum(&mut self, clip: ClipId, downward: bool) -> usize {
        let Some((source, targets)) = self.tool_targets(clip) else { return 0 };
        let mut by_start: BTreeMap<Tick, Vec<Note>> = BTreeMap::new();
        for n in targets {
            by_start.entry(n.start).or_default().push(n);
        }
        let offset_step = PPQ / 32;
        let min_len = PPQ / 16;
        let mut changed = Vec::new();
        for (_, mut chord) in by_start {
            if chord.len() < 2 {
                continue;
            }
            chord.sort_by_key(|n| n.key);
            if downward {
                chord.reverse();
            }
            for (i, n) in chord.into_iter().enumerate() {
                let shift = (i as Tick * offset_step).min(n.length.saturating_sub(min_len));
                if shift > 0 {
                    changed.push(Note { start: n.start + shift, length: n.length - shift, ..n });
                }
            }
        }
        let count = changed.len();
        if count > 0 {
            self.edit("Strum", vec![Command::UpdateNotes { source, notes: changed }]);
        }
        count
    }

    /// Slice each target note longer than the snap grid into repeated equal grid slices.
    pub fn chop(&mut self, clip: ClipId) -> usize {
        let Some((source, targets)) = self.tool_targets(clip) else { return 0 };
        let step = self.grid().max(PPQ / 16);
        let mut remove_ids = Vec::new();
        let mut added = Vec::new();
        for n in &targets {
            if n.length <= step {
                continue;
            }
            remove_ids.push(n.id);
            let mut tick = n.start;
            let end = n.end();
            while tick < end {
                let len = (end - tick).min(step).max(PPQ / 16);
                added.push(Note {
                    id: self.project.alloc_note_id(),
                    start: tick,
                    length: len,
                    ..n.clone()
                });
                tick += step;
            }
        }
        let count = added.len();
        if count > 0 {
            let new_ids: BTreeSet<NoteId> = added.iter().map(|n| n.id).collect();
            let had_sel = !self.selected_notes.is_empty();
            if self.edit("Chop", vec![Command::RemoveNotes { source, ids: remove_ids }, Command::AddNotes { source, notes: added }]) && had_sel {
                self.selected_notes = new_ids;
            }
        }
        count
    }

    /// Randomize pitch within the active scale (±3 steps) and humanize velocity cleanly.
    pub fn randomize_notes(&mut self, clip: ClipId, seed: u64) -> usize {
        let Some((source, targets)) = self.tool_targets(clip) else { return 0 };
        let scale = self.clip_scale(Some(clip));
        let mut changed = Vec::new();
        for (idx, n) in targets.into_iter().enumerate() {
            let h = n.id.0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(seed.wrapping_mul(1442695040888963407))
                .wrapping_add(idx as u64);
            let pitch_delta = ((h >> 16) % 7) as i32 - 3; // -3..=+3 steps
            let vel_jitter = (((h >> 32) % 37) as f32 - 18.0) / 100.0; // -0.18..=+0.18
            let key = match scale {
                Some(s) => step_note_in_scale(s, n.key, pitch_delta),
                None => (n.key as i32 + pitch_delta).clamp(0, 127) as u8,
            };
            let velocity = (n.velocity + vel_jitter).clamp(0.25, 1.0);
            if key != n.key || (velocity - n.velocity).abs() > 1e-4 {
                changed.push(Note { key, velocity, ..n });
            }
        }
        let count = changed.len();
        if count > 0 {
            self.edit("Randomize", vec![Command::UpdateNotes { source, notes: changed }]);
        }
        count
    }

    // ── Keyboard editing (the Live key commands) ──

    fn grid(&self) -> Tick {
        self.snap_step(crate::editing::GridView::Piano).unwrap_or(PPQ / 4).max(1)
    }

    /// The selected notes of the open clip, when the piano roll has the keys.
    fn key_notes(&self) -> Option<(ClipId, SourceId, Vec<Note>)> {
        let clip = self.selected_clip.filter(|_| self.ed.area == crate::editing::Area::Piano)?;
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
        // In the Session View the arrows walk the slots.
        if self.view == MainView::Session {
            return self.session_key(e);
        }
        // With an automation range selected the arrows move its envelope.
        if let Some(done) = self.key_edit_auto_range(e) {
            return done;
        }
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
                        KeyEdit::VelocityUp => {
                            if self.note_lane == NoteLane::Chance {
                                n.probability = (n.probability + 0.1).min(1.);
                            } else {
                                n.velocity = (n.velocity + 0.08).min(1.);
                            }
                        }
                        KeyEdit::VelocityDown => {
                            if self.note_lane == NoteLane::Chance {
                                n.probability = (n.probability - 0.1).max(0.);
                            } else {
                                n.velocity = (n.velocity - 0.08).max(0.01);
                            }
                        }
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
                KeyEdit::VelocityUp | KeyEdit::VelocityDown => if self.note_lane == NoteLane::Chance { "Change chance" } else { "Change velocity" },
                _ => "Transpose notes",
            };
            let ok = self.edit(label, vec![Command::UpdateNotes { source, notes }]);
            self.seal();
            return ok;
        }
        self.key_edit_arrangement(e)
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
        // In the Device View: the picked devices, right after themselves.
        if self.ed.area == crate::editing::Area::Devices {
            let ids = self.selected_devices();
            if !ids.is_empty() {
                return self.duplicate_devices(&ids);
            }
        }
        if let Some(done) = self.duplicate_auto_range() {
            return done;
        }
        if let Some(done) = self.duplicate_arrangement() {
            return done;
        }
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
        // No clip picked: the track headers have the key (Live's Cmd+D on a track).
        let Some((track, id)) = self.selected_placement else {
            if self.ed.area == crate::editing::Area::Arrange {
                let ids = self.selected_tracks();
                return self.duplicate_tracks(&ids);
            }
            return false;
        };
        let Some(place) = self.project.track(track).and_then(|t| t.arrangement.iter().find(|p| p.id == id)).cloned() else { return false };
        let Some(length) = self.project.clips.get(&place.clip).map(|c| c.length) else { return false };
        // An independent copy: MIDI clips get their own notes, audio shares the file.
        let Some((cmds, new_place)) = aitrack_model::ops::duplicate_placement(&mut self.project, track, id, place.start + length) else { return false };
        self.seal();
        let ok = self.edit("Duplicate clip", cmds);
        self.seal();
        if ok {
            self.selected_placement = Some((track, new_place));
        }
        ok
    }

    // ── Clips and tracks (the Live key commands) ──

    /// Ctrl+E and Split in the clip menu: cut at the insert marker, or at
    /// `playhead` (on the grid) while the transport is playing. Returns how
    /// many clips were split.
    pub fn split(&mut self, playhead: Option<Tick>) -> usize {
        match playhead {
            Some(at) => self.split_at(at),
            None => self.split_exact(self.ed.marker),
        }
    }

    /// Split at `tick`, on the grid. The selected arrangement clips the cut
    /// falls inside, otherwise every clip of the selected track under it.
    /// The parts before the cut are the selection afterwards. Returns how
    /// many clips were split.
    pub fn split_at(&mut self, tick: Tick) -> usize {
        let tick = self.snap_tick(crate::editing::GridView::Arrange, tick, false);
        self.split_exact(tick)
    }

    fn split_exact(&mut self, tick: Tick) -> usize {
        if let Some(cut) = self.split_time_selection() {
            return cut;
        }
        let spans = |p: &Project, pl: &Placement| p.clips.get(&pl.clip).is_some_and(|c| tick > pl.start && tick < pl.start + c.length);
        let mut targets: Vec<(TrackId, Placement)> =
            self.clip_selection().into_iter().filter(|(_, pl, _)| spans(&self.project, pl)).map(|(track, pl, _)| (track, pl)).collect();
        let selected = !targets.is_empty();
        if targets.is_empty()
            && let Some(track) = self.selected_track
            && let Some(t) = self.project.track(track)
        {
            targets.extend(t.arrangement.iter().filter(|pl| spans(&self.project, pl)).map(|pl| (track, pl.clone())));
        }
        let mut plan = crate::editing::Plan::new(self);
        let mut cut = 0usize;
        let mut last = None;
        for (track, pl) in &targets {
            // The model's split keeps the fades on the outer edges, divides the
            // volume envelope, gives a notes half its own notes and leaves a
            // clip that is used elsewhere as it is there. Running on a Plan
            // applies each split sequentially and keeps IDs/clips atomic.
            let Some(split) = aitrack_model::ops::split_placement(&mut plan.p, *track, pl.id, tick) else { continue };
            plan.run_all(split.commands);
            last = Some(pl.id);
            cut += 1;
        }
        if self.commit("Split clip", plan) {
            // The part before the cut keeps its placement and stays selected,
            // in the lane and in the Clip tab alike.
            let ids = if selected { self.selected_clip_ids() } else { targets.iter().map(|(_, pl)| pl.id).collect() };
            let last = self.selected_placement.map(|(_, id)| id).filter(|id| ids.contains(id)).or(last);
            self.select_clips(ids, last);
            cut
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
        let mut track = Track::new(id, kind, name, if self.color_prefs.auto_track { Color::palette(self.project.tracks.len()) } else { ColorPrefs::DEFAULT_TRACK });
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

    /// The audio clips a sample edit made on `clip` acts on: every selected
    /// audio clip when `clip` is one of them, else `clip` alone.
    pub fn warp_targets(&self, clip: ClipId) -> Vec<ClipId> {
        let audio = |c: &ClipId| self.project.clips.get(c).is_some_and(|c| matches!(self.project.sources.get(&c.source), Some(Source::Audio(_))));
        let selected: std::collections::BTreeSet<ClipId> = self.clip_selection().iter().map(|(_, pl, _)| pl.clip).filter(audio).collect();
        if selected.contains(&clip) { selected.into_iter().collect() } else { vec![clip] }
    }

    /// Edit the sample settings (transpose, detune, reverse, warp on/off) of
    /// `clip` and of the other selected audio clips with it; the rest stay as
    /// they are. A sample shared with clips outside the edit is copied for
    /// the edited ones first, so every clip keeps its own settings as in Live.
    /// Edits under the same `key` (one drag) are one undo step.
    pub fn set_clip_warp(&mut self, key: &str, clip: ClipId, f: impl Fn(&mut Warp)) -> bool {
        let targets = self.warp_targets(clip);
        self.set_warp_of(key, &targets, f)
    }

    /// Every audio clip that plays the same file as `clip` (whether it shares
    /// `clip`'s source or got a copy of its own when it was transposed
    /// alone), `clip` first. Empty when `clip` is not an audio clip.
    pub fn sample_targets(&self, clip: ClipId) -> Vec<ClipId> {
        let path_of = |c: &Clip| match self.project.sources.get(&c.source) {
            Some(Source::Audio(a)) => Some(a.path.clone()),
            _ => None,
        };
        let Some(path) = self.project.clips.get(&clip).and_then(path_of) else { return Vec::new() };
        let mut out = vec![clip];
        out.extend(self.project.clips.iter().filter(|(id, c)| **id != clip && path_of(c).as_ref() == Some(&path)).map(|(id, _)| *id));
        out
    }

    /// As [`Self::set_clip_warp`], on every clip of the sample `clip` plays
    /// ([`Self::sample_targets`]), selected or not: Cmd/Ctrl + a Transpose or
    /// Detune drag, and "Apply to all clips of this sample".
    pub fn set_sample_warp(&mut self, key: &str, clip: ClipId, f: impl Fn(&mut Warp)) -> bool {
        let targets = self.sample_targets(clip);
        self.set_warp_of(key, &targets, f)
    }

    /// Give every clip of the sample `clip` plays the transpose, detune and
    /// reverse `clip` has; each keeps its own warp and stretch. One undo step.
    pub fn match_sample_pitch(&mut self, clip: ClipId) -> bool {
        let Some(Source::Audio(a)) = self.project.clips.get(&clip).and_then(|c| self.project.sources.get(&c.source)) else { return false };
        let (transpose, detune, reverse) = (a.warp.transpose, a.warp.detune, a.warp.reverse);
        self.seal();
        let ok = self.set_sample_warp("sample-pitch", clip, |w| {
            w.transpose = transpose;
            w.detune = detune;
            w.reverse = reverse;
        });
        self.seal();
        ok
    }

    /// The sample edit of [`Self::set_clip_warp`] on exactly `targets`.
    pub(crate) fn set_warp_of(&mut self, key: &str, targets: &[ClipId], f: impl Fn(&mut Warp)) -> bool {
        let mut sources: Vec<SourceId> = Vec::new();
        for c in targets {
            if let Some(s) = self.project.clips.get(c).map(|c| c.source).filter(|s| !sources.contains(s)) {
                sources.push(s);
            }
        }
        let mut cmds = Vec::new();
        for src in sources {
            let Some(Source::Audio(a)) = self.project.sources.get(&src).cloned() else { continue };
            let mut warp = a.warp.clone();
            f(&mut warp);
            warp.transpose = warp.transpose.round().clamp(-48., 48.);
            warp.detune = warp.detune.round().clamp(-50., 50.);
            if warp == a.warp {
                continue;
            }
            let shared = self.project.clips.iter().any(|(id, c)| c.source == src && !targets.contains(id));
            if !shared {
                cmds.push(Command::SetWarp { source: src, warp: Box::new(warp) });
                continue;
            }
            let id = self.project.alloc_source_id();
            cmds.push(Command::AddSource { id, source: Box::new(Source::Audio(AudioSource { warp, ..a })) });
            for c in targets {
                if let Some(clip) = self.project.clips.get(c).filter(|cl| cl.source == src) {
                    cmds.push(Command::UpdateClip { id: *c, clip: Box::new(Clip { source: id, ..clip.clone() }) });
                }
            }
        }
        !cmds.is_empty() && self.edit_coalesced(key, "Edit sample", cmds)
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

    /// Snapshot the rack's current macro values as a new variation.
    pub fn save_rack_variation(&mut self, rack: DeviceId, name: Option<String>) -> bool {
        let Some(DeviceKind::Rack(r)) = self.project.device(rack).map(|d| &d.kind) else { return false };
        let mut variations = r.variations.clone();
        let next_num = variations.len() + 1;
        let var_name = name.unwrap_or_else(|| format!("Var {next_num}"));
        let values = r.macros.iter().map(|m| m.value).collect();
        variations.push(Variation { name: var_name, values });
        self.edit("Save macro variation", vec![Command::SetVariations { rack, variations }])
    }

    /// Remove a saved macro variation by index.
    pub fn delete_rack_variation(&mut self, rack: DeviceId, index: usize) -> bool {
        let Some(DeviceKind::Rack(r)) = self.project.device(rack).map(|d| &d.kind) else { return false };
        if index >= r.variations.len() {
            return false;
        }
        let mut variations = r.variations.clone();
        variations.remove(index);
        self.edit("Delete macro variation", vec![Command::SetVariations { rack, variations }])
    }

    pub fn toggle_device(&mut self, device: DeviceId) -> bool {
        let Some(enabled) = self.project.device(device).map(|d| d.enabled) else { return false };
        self.edit("Toggle device", vec![Command::SetDeviceEnabled { device, enabled: !enabled }])
    }

    pub fn add_stock_device(&mut self, track: TrackId, plugin_id: &str, name: &str) -> Option<DeviceId> {
        self.add_stock_device_in(track, None, plugin_id, name)
    }

    /// Where a new device of `track` goes: the end of `into` (a chain of a rack
    /// of that track, or a Drum Rack return) when given, else the end of the track's chain.
    fn new_device_slot(&self, track: TrackId, into: Option<racks::Container>) -> Option<DeviceSlot> {
        if let Some(c) = into
            && let racks::Container::Rack { rack, .. } | racks::Container::Return { rack, .. } = c
            && self.project.device_track(rack) == Some(track)
            && let Some(chain) = racks::chain(&self.project, c)
        {
            return Some(c.slot(chain.len()));
        }
        Some(DeviceSlot::Track { track, index: self.project.track(track)?.devices.len() })
    }

    /// Add a stock device at the end of `into` (see [`new_device_slot`](Self::new_device_slot)).
    pub fn add_stock_device_in(&mut self, track: TrackId, into: Option<racks::Container>, plugin_id: &str, name: &str) -> Option<DeviceId> {
        let mut slot = self.new_device_slot(track, into)?;
        // The engine runs MIDI effects before everything else, in the order they
        // were put in: a new one goes after the last MIDI effect of the chain, so
        // the chain reads as it runs.
        if (plugin_id.starts_with("midi.") || plugin_id == "rack.midi")
            && into.is_none()
            && let DeviceSlot::Track { index, .. } = &mut slot
        {
            let is_midi = |d: &Device| matches!(&d.kind, DeviceKind::Stock { plugin_id } if plugin_id.starts_with("midi.")) || matches!(&d.kind, DeviceKind::Rack(r) if r.kind == RackKind::Midi);
            *index = self.project.track(track)?.devices.iter().rposition(is_midi).map_or(0, |i| i + 1);
        }
        let id = self.project.alloc_device_id();
        let mut device = Device::stock(id, plugin_id, name);
        if let Some(kind) = match plugin_id {
            "rack.drum" => Some(RackKind::Drum),
            "rack.instrument" => Some(RackKind::Instrument),
            "rack.audio" => Some(RackKind::Audio),
            "rack.midi" => Some(RackKind::Midi),
            _ => None,
        } {
            device.kind = DeviceKind::Rack(Rack::new(kind));
        }
        self.edit("Add device", vec![Command::AddDevice { slot, device: Box::new(device) }]).then_some(id)
    }

    /// Add a third-party VST3/CLAP plugin to the end of a track's chain.
    pub fn add_plugin_device(&mut self, track: TrackId, plugin: &crate::bridge::ExternalPlugin) -> Option<DeviceId> {
        self.add_plugin_device_in(track, None, plugin)
    }

    pub fn add_plugin_device_in(&mut self, track: TrackId, into: Option<racks::Container>, plugin: &crate::bridge::ExternalPlugin) -> Option<DeviceId> {
        let slot = self.new_device_slot(track, into)?;
        let id = self.project.alloc_device_id();
        let mut device = Device::stock(id, "", plugin.name.clone());
        device.kind = DeviceKind::Plugin {
            format: plugin.format,
            uid: plugin.uid.clone(),
            path: plugin.path.clone(),
            vendor: plugin.vendor.clone(),
            is_instrument: plugin.is_instrument,
        };
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

    /// Delete several devices in one undo step. A device inside a rack that
    /// goes too is left to the rack.
    pub fn remove_devices(&mut self, ids: Vec<DeviceId>) -> bool {
        let inside = |id: DeviceId| {
            let mut found = false;
            for r in &ids {
                if let Some(Device {
                    kind: DeviceKind::Rack(_),
                    ..
                }) = self.project.device(*r)
                {
                    let mut hit = |d: &Device| found |= d.id == id && d.id != *r;
                    aitrack_model::visit_devices(
                        std::slice::from_ref(self.project.device(*r).unwrap()),
                        &mut hit,
                    );
                }
            }
            found
        };
        let cmds: Vec<Command> = ids
            .iter()
            .filter(|id| !inside(**id))
            .map(|&id| Command::RemoveDevice { id })
            .collect();
        let label = if cmds.len() > 1 {
            "Delete devices"
        } else {
            "Delete device"
        };
        let ok = self.edit(label, cmds);
        if ok {
            self.ed.devices.clear();
        }
        ok
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
        // A one-shot audio clip stops at the end of its file; a looping one repeats.
        let Some(cmd) = aitrack_model::ops::resize_end(&self.project, clip, length) else { return false };
        self.edit_coalesced("clip-resize", "Resize clip", vec![cmd])
    }

    pub fn remove_placement(&mut self, track: TrackId, id: PlacementId) -> bool {
        self.selected_placement = None;
        self.edit("Delete clip", vec![Command::RemovePlacement { track, id }])
    }

    /// Change the tempo. Samples that play in real time (audio clips without
    /// warp) keep their duration: their length in ticks follows
    /// ([`ops::set_tempo_keeping_samples`]), in the same undo step. During a
    /// drag every frame is worked out from the project as it was before the
    /// drag, so the clips neither drift nor get stuck on rounding.
    pub fn set_tempo(&mut self, bpm: f64) -> bool {
        if !bpm.is_finite() || bpm <= 0. {
            return false;
        }
        let bpm = (bpm * 100.).round() / 100.;
        let (base, touched) = match self.tempo_base.take() {
            Some((base, revision, touched)) if revision == self.revision() => (base, touched),
            _ => (self.project.clone(), BTreeSet::new()),
        };
        // `None` = the start tempo again: only the clips an earlier frame changed go back.
        let mut cmds = ops::set_tempo_keeping_samples(&base, bpm).unwrap_or_else(|| vec![Command::SetTempo { bpm }]);
        let changed: BTreeSet<ClipId> = cmds.iter().filter_map(|c| if let Command::UpdateClip { id, .. } = c { Some(*id) } else { None }).collect();
        for id in touched.difference(&changed) {
            if let Some(clip) = base.clips.get(id) {
                cmds.push(Command::UpdateClip { id: *id, clip: Box::new(clip.clone()) });
            }
        }
        let touched: BTreeSet<ClipId> = touched.union(&changed).copied().collect();
        let ok = self.edit_coalesced("tempo", "Change tempo", cmds);
        if ok {
            self.tempo_base = Some((base, self.revision(), touched));
        }
        ok
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

    pub fn set_track_scale(&mut self, track: TrackId, scale: Option<Scale>) -> bool {
        self.edit("Change track key", vec![Command::SetTrack { id: track, change: TrackProp::Scale(scale) }])
    }

    pub fn set_track_keyboard(&mut self, track: TrackId, on: bool) -> bool {
        self.edit("Toggle computer keyboard", vec![Command::SetTrack { id: track, change: TrackProp::KeyboardInput(on) }])
    }

    pub fn set_clip_scale(&mut self, clip: ClipId, scale: Option<Scale>) -> bool {
        let Some(c) = self.project.clips.get(&clip).cloned() else { return false };
        if c.scale == scale {
            return false;
        }
        self.edit("Change clip key", vec![Command::UpdateClip { id: clip, clip: Box::new(Clip { scale, ..c }) }])
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

    // ── Hosted plugins ──

    /// Parameter edits made in plugin windows (oldest first) become
    /// `SetParam` commands: each `Begin`…`End` gesture on one parameter is
    /// one undo step. A value the project already holds is skipped, so the
    /// engine never sends a plugin back what it just reported; values the
    /// host sets are not reported by plugins as edits, so nothing loops.
    ///
    /// As in Live, touching a control shows its lane when the track's
    /// automation is shown, and while recording with Automation Arm on
    /// (`record_at` is the playhead then) the gesture is written into that
    /// lane, overwriting what it passes over, in the same undo step.
    /// Returns whether the project changed.
    pub fn apply_plugin_edits(&mut self, edits: &[crate::bridge::PluginEdit], record_at: Option<Tick>) -> bool {
        use crate::bridge::PluginEditKind;
        let record_at = record_at.filter(|_| self.automation_arm).map(|t| t.max(0));
        let mut changed = false;
        for e in edits {
            let target = AutomationTarget::DeviceParam { device: e.device, param: e.param };
            let track = self.project.device_track(e.device);
            match e.kind {
                PluginEditKind::Begin => {
                    if let Some(track) = track
                        && (!self.automation_shown.contains(&track) || !self.shown_lanes(track).contains(&target))
                    {
                        self.show_automation(track, target);
                        changed = true;
                    }
                    if let Some(tick) = record_at {
                        self.auto_writes.insert((e.device, e.param), tick);
                    }
                }
                PluginEditKind::Value => {
                    let Some(d) = self.project.device(e.device) else { continue };
                    let value = e.value.clamp(0., 1.);
                    let same = d.params.get(&e.param).is_some_and(|v| (v - value).abs() < 1e-6);
                    let key = format!("plugin:{}:{}", e.device, e.param);
                    let mut cmds = Vec::new();
                    if !same {
                        cmds.push(Command::SetParam { device: e.device, param: e.param, value: Some(value) });
                    }
                    // Plugins that move a control without a Begin edit (or when
                    // automation is still hidden) should reveal its lane too.
                    if let Some(track) = track
                        && (!same || record_at.is_some())
                        && (!self.automation_shown.contains(&track) || !self.shown_lanes(track).contains(&target))
                    {
                        self.show_automation(track, target);
                        changed = true;
                    }
                    if let (Some(tick), Some(track)) = (record_at, track) {
                        let from = *self.auto_writes.get(&(e.device, e.param)).unwrap_or(&tick);
                        let points = aitrack_model::automation::write(&self.lane_points(track, target), from, tick, value);
                        self.auto_writes.insert((e.device, e.param), tick);
                        cmds.push(self.lane_command(track, target, points));
                    }
                    if !cmds.is_empty() {
                        changed |= self.edit_coalesced(&key, "Plugin parameter", cmds);
                    }
                }
                PluginEditKind::End => {
                    self.auto_writes.remove(&(e.device, e.param));
                    self.seal();
                }
            }
        }
        changed
    }

    // ── Automation lanes (Live: the header's automation toggle, the lane's
    //    chooser, "+" for another lane and "−" to hide one) ──

    /// The lanes shown under `track`, top to bottom; empty while its
    /// automation is hidden. Shown without a chosen lane, it is the first
    /// lane that has breakpoints, else Volume.
    pub fn shown_lanes(&self, track: TrackId) -> Vec<AutomationTarget> {
        if !self.automation_shown.contains(&track) {
            return Vec::new();
        }
        match self.auto_lanes.get(&track) {
            Some(l) if !l.is_empty() => l.clone(),
            _ => vec![self.default_lane(track)],
        }
    }

    fn default_lane(&self, track: TrackId) -> AutomationTarget {
        let t = self.project.track(track);
        t.and_then(|t| t.automation.iter().find(|l| !l.points.is_empty()).map(|l| l.target))
            .unwrap_or(if t.is_some_and(|t| t.kind.carries_audio()) { AutomationTarget::Volume } else { AutomationTarget::Tempo })
    }

    /// The header's automation button: show or hide the track's lanes.
    pub fn toggle_automation(&mut self, track: TrackId) {
        if !self.automation_shown.remove(&track) {
            self.automation_shown.insert(track);
            let lanes = self.shown_lanes(track);
            self.auto_lanes.insert(track, lanes);
        }
    }

    /// Show `track`'s lane for `target` (the mixer's and the device view's
    /// "Show automation", a touched plugin control). A lane with nothing
    /// drawn in it gives its place to it, as Live's chooser does; otherwise
    /// it is added under the others. Nothing is edited: the lane exists in
    /// the project once a breakpoint is drawn.
    pub fn show_automation(&mut self, track: TrackId, target: AutomationTarget) {
        if self.project.track(track).is_none() {
            return;
        }
        let mut lanes = if self.automation_shown.contains(&track) { self.shown_lanes(track) } else { self.auto_lanes.get(&track).cloned().unwrap_or_default() };
        if !lanes.contains(&target) {
            match lanes.iter().position(|t| self.lane_points(track, *t).is_empty()) {
                Some(i) => lanes[i] = target,
                None => lanes.push(target),
            }
        }
        self.auto_lanes.insert(track, lanes);
        self.automation_shown.insert(track);
        self.selected_track = Some(track);
    }

    /// "+": one more lane under the track's others, for the first target
    /// not shown yet (lanes with breakpoints first, then the mixer's).
    /// `None` when every one is already shown.
    pub fn add_auto_lane(&mut self, track: TrackId) -> Option<AutomationTarget> {
        let t = self.project.track(track)?;
        let shown = self.shown_lanes(track);
        let mixer = aitrack_model::automation::targets(t, |_| Vec::new()).swap_remove(0).targets;
        let next = t.automation.iter().filter(|l| !l.points.is_empty()).map(|l| l.target).chain(mixer).find(|c| !shown.contains(c))?;
        let mut lanes = shown;
        lanes.push(next);
        self.auto_lanes.insert(track, lanes);
        self.automation_shown.insert(track);
        Some(next)
    }

    /// The chooser of the lane at `index`: it shows `target` from now on (a
    /// lane already showing `target` elsewhere under the track is closed).
    pub fn set_lane_target(&mut self, track: TrackId, index: usize, target: AutomationTarget) {
        let mut lanes = self.shown_lanes(track);
        if index >= lanes.len() {
            return;
        }
        let old = lanes[index];
        lanes[index] = target;
        // Keep the lane just chosen; another one showing `target` goes.
        let mut i = 0;
        lanes.retain(|t| {
            i += 1;
            *t != target || i - 1 == index
        });
        if self.auto_sel.is_some_and(|r| r.track == track && r.target == old) {
            self.auto_sel = None;
        }
        self.auto_lanes.insert(track, lanes);
    }

    /// "−": hide one lane (its breakpoints stay); the last one hides the
    /// track's automation.
    pub fn hide_auto_lane(&mut self, track: TrackId, target: AutomationTarget) {
        let mut lanes = self.shown_lanes(track);
        lanes.retain(|t| *t != target);
        if self.auto_sel.is_some_and(|r| r.track == track && r.target == target) {
            self.auto_sel = None;
        }
        if lanes.is_empty() {
            self.automation_shown.remove(&track);
            self.auto_lanes.remove(&track);
        } else {
            self.auto_lanes.insert(track, lanes);
        }
    }

    /// Delete a lane's automation (one undo step); the lane stays shown,
    /// empty. `false` when it has none.
    pub fn delete_automation(&mut self, track: TrackId, target: AutomationTarget) -> bool {
        if !self.project.track(track).is_some_and(|t| t.automation.iter().any(|l| l.target == target)) {
            return false;
        }
        if self.auto_sel.is_some_and(|r| r.track == track && r.target == target) {
            self.auto_sel = None;
        }
        self.seal();
        let ok = self.edit("Delete automation", vec![Command::SetAutomation { track, target, lane: None }]);
        self.seal();
        ok
    }

    fn plugin_state_commands(&self, states: Vec<(DeviceId, Vec<u8>)>) -> Vec<Command> {
        states
            .into_iter()
            .filter(|(device, state)| self.project.device(*device).is_some_and(|d| d.state.as_deref() != Some(state.as_slice())))
            .map(|(device, state)| Command::SetDeviceState { device, state: Some(state), preset: None })
            .collect()
    }

    /// Write the states the live plugins report into the project **without an
    /// undo step** (before saving or exporting): they are the plugins' own
    /// state echoed back, not an edit by the user, so Cmd+Z after saving still
    /// undoes the user's last edit. The revision does not move either (the
    /// engine already has these states). Returns whether any changed.
    pub fn capture_plugin_states(&mut self, states: Vec<(DeviceId, Vec<u8>)>) -> bool {
        let cmds = self.plugin_state_commands(states);
        if cmds.is_empty() {
            return false;
        }
        match (Command::Batch { commands: cmds }).apply(&mut self.project) {
            Ok(_) => true,
            Err(e) => {
                self.last_error = Some(e.to_string());
                false
            }
        }
    }

    /// The project as the exporter must render it: the live plugin states
    /// captured first, so what the user hears in the plugin windows is what
    /// the offline render builds.
    pub fn export_snapshot(&mut self, engine: &mut dyn crate::bridge::EngineBridge) -> Project {
        let states = engine.plugin_states();
        self.capture_plugin_states(states);
        self.project.clone()
    }

    // ── Track management: delete, reorder, colour ──

    /// Delete `ids` (the Main track stays). A group goes alone: its tracks move
    /// up to its parent ([`groups::ungroup_commands`]). One undo step.
    pub fn delete_tracks(&mut self, ids: &[TrackId]) -> bool {
        let mut cmds = Vec::new();
        for &id in ids {
            let Some(t) = self.project.track(id) else { continue };
            match t.kind {
                TrackKind::Main => {}
                TrackKind::Group | TrackKind::Folder => cmds.extend(groups::ungroup_commands(&self.project, id).unwrap_or_default()),
                _ => cmds.push(Command::RemoveTrack { id }),
            }
        }
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit(if ids.len() == 1 { "Delete track" } else { "Delete tracks" }, cmds);
        self.seal();
        if ok {
            self.ed.tracks.retain(|t| self.project.track(*t).is_some());
            if self.selected_track.is_some_and(|t| self.project.track(t).is_none()) {
                self.selected_track = None;
            }
            self.prune_selection();
        }
        ok
    }

    /// Move track `id` (with the tracks of its group, if it is one) next to
    /// `target`: before it, or after it and its group's members, at its level
    /// (dropping next to a track of a group puts it in that group). Tracks and
    /// returns keep to their own sections, and Main never moves.
    pub fn move_track(&mut self, id: TrackId, target: TrackId, after: bool) -> bool {
        let Some(cmds) = self.move_track_commands(id, target, after) else { return false };
        self.seal();
        let ok = self.edit("Move track", cmds);
        self.seal();
        ok
    }

    fn move_track_commands(&self, id: TrackId, target: TrackId, after: bool) -> Option<Vec<Command>> {
        let p = &self.project;
        let (x, t) = (p.track(id)?, p.track(target)?);
        let section = |k: TrackKind| matches!(k, TrackKind::Return | TrackKind::Vca);
        if id == target || t.kind == TrackKind::Main || section(x.kind) != section(t.kind) {
            return None;
        }
        if !after {
            return tracks::move_track_before(p, id, target);
        }
        // After the target and its members: in front of the next track of its level.
        let last = std::iter::once(target).chain(groups::descendants(p, target)).filter_map(|m| p.track_index(m)).max()?;
        let next = p.tracks.iter().skip(last + 1).find(|n| n.parent == t.parent).map(|n| n.id);
        if next == Some(id) {
            return None;
        }
        tracks::move_track(p, id, next, t.parent)
    }

    /// Duplicate `ids` (devices) right after themselves in their chains, as one
    /// undo step; the copies become the picked devices.
    pub fn duplicate_devices(&mut self, ids: &[DeviceId]) -> bool {
        let mut scratch = self.project.clone();
        let Some(d) = tracks::duplicate_devices(&mut scratch, ids) else { return false };
        self.seal();
        let ok = self.edit(if d.devices.len() == 1 { "Duplicate device" } else { "Duplicate devices" }, d.commands);
        self.seal();
        if ok {
            self.ed.devices = d.devices;
        }
        ok
    }

    /// Duplicate `ids` (with their devices, clips and automation; a group with
    /// its tracks) right after themselves, as one undo step. The copies become
    /// the selection.
    pub fn duplicate_tracks(&mut self, ids: &[TrackId]) -> bool {
        // A track inside another selected one is copied with it.
        let roots: Vec<TrackId> = ids.iter().copied().filter(|&t| !groups::ancestors(&self.project, t).any(|a| ids.contains(&a))).collect();
        let mut scratch = self.project.clone();
        let (mut cmds, mut made) = (Vec::new(), Vec::new());
        for id in roots {
            let Some(d) = tracks::duplicate_track(&mut scratch, id) else { continue };
            if (Command::Batch { commands: d.commands.clone() }).apply(&mut scratch).is_err() {
                continue;
            }
            cmds.extend(d.commands);
            made.push(d.track);
        }
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit(if made.len() == 1 { "Duplicate track" } else { "Duplicate tracks" }, cmds);
        self.seal();
        if ok {
            self.ed.tracks = made.iter().copied().collect();
            self.selected_track = made.first().copied();
        }
        ok
    }

    /// Duplicate `ids` with their devices, routing and mixer state, but without
    /// clips or automation (Live's Duplicate Without Content). One undo step.
    pub fn duplicate_tracks_empty(&mut self, ids: &[TrackId]) -> bool {
        let roots: Vec<TrackId> = ids.iter().copied().filter(|&t| !groups::ancestors(&self.project, t).any(|a| ids.contains(&a))).collect();
        let mut scratch = self.project.clone();
        let (mut cmds, mut made) = (Vec::new(), Vec::new());
        for id in roots {
            let Some(d) = tracks::duplicate_track_empty(&mut scratch, id) else { continue };
            if (Command::Batch { commands: d.commands.clone() }).apply(&mut scratch).is_err() {
                continue;
            }
            cmds.extend(d.commands);
            made.push(d.track);
        }
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit(if made.len() == 1 { "Duplicate track without content" } else { "Duplicate tracks without content" }, cmds);
        self.seal();
        if ok {
            self.ed.tracks = made.iter().copied().collect();
            self.selected_track = made.first().copied();
        }
        ok
    }

    /// Clear Solo on every soloed track in one undo step. Returns how many tracks were unsoloed.
    pub fn unsolo_all(&mut self) -> usize {
        let cmds: Vec<Command> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.solo)
            .map(|t| Command::SetTrack { id: t.id, change: TrackProp::Solo(false) })
            .collect();
        let count = cmds.len();
        if count > 0 {
            self.seal();
            self.edit("Unsolo all", cmds);
            self.seal();
        }
        count
    }

    /// Give `ids` the palette colour `color`.
    pub fn set_tracks_color(&mut self, ids: &[TrackId], color: Color) -> bool {
        let pairs: Vec<(TrackId, Color)> = ids.iter().map(|id| (*id, color)).collect();
        self.recolor_tracks(&pairs, "Change track colour")
    }

    /// Give each track its colour, as one undo step. The clips that wore the
    /// old colour of their track follow it; a clip coloured by hand keeps its own.
    pub fn recolor_tracks(&mut self, pairs: &[(TrackId, Color)], label: &str) -> bool {
        let mut cmds = Vec::new();
        for &(id, color) in pairs {
            let Some(t) = self.project.track(id) else { continue };
            let Some(set) = tracks::set_track_color(&self.project, id, color) else { continue };
            cmds.push(set);
            for pl in &t.arrangement {
                if let Some(c) = self.project.clips.get(&pl.clip).filter(|c| c.color == t.color)
                    && !cmds.iter().any(|x| matches!(x, Command::UpdateClip { id, .. } if *id == pl.clip))
                {
                    cmds.push(Command::UpdateClip { id: pl.clip, clip: Box::new(Clip { color, ..c.clone() }) });
                }
            }
        }
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit(label, cmds);
        self.seal();
        ok
    }

    /// The colour a new clip on a track coloured `track` gets: the track's, or its own palette pick.
    pub fn clip_color_for(&self, track: Color) -> Color {
        if self.color_prefs.clips_take_track { track } else { Color::palette(self.project.clips.len()) }
    }

    /// Context action "Assign Track Color to Clips" for `tracks`.
    pub fn assign_track_color_to_clips(&mut self, tracks: &[TrackId]) -> bool {
        self.run_color_commands(colors::assign_track_color_to_clips(&self.project, tracks), "Assign track colour to clips")
    }

    /// Context action "Randomize Colors" for the selected tracks.
    pub fn randomize_track_colors(&mut self, tracks: &[TrackId], seed: u64) -> bool {
        let pairs = colors::randomize_tracks(&self.project, tracks, seed);
        self.recolor_tracks(&pairs, "Randomize track colours")
    }

    /// Context action "Randomize Colors" for the selected clips.
    pub fn randomize_clip_colors(&mut self, seed: u64) -> bool {
        let clips: Vec<ClipId> = self.clip_selection().iter().map(|(_, pl, _)| pl.clip).collect();
        self.run_color_commands(colors::randomize_clips(&self.project, &clips, seed), "Randomize clip colours")
    }

    fn run_color_commands(&mut self, cmds: Vec<Command>, label: &str) -> bool {
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit(label, cmds);
        self.seal();
        ok
    }

    /// Give the selected clips the colour `color`.
    pub fn set_clips_color(&mut self, color: Color) -> bool {
        let mut clips: Vec<ClipId> = self.clip_selection().iter().map(|(_, pl, _)| pl.clip).collect();
        clips.dedup();
        let cmds: Vec<Command> = clips
            .into_iter()
            .filter_map(|id| self.project.clips.get(&id).filter(|c| c.color != color).map(|c| Command::UpdateClip { id, clip: Box::new(Clip { color, ..c.clone() }) }))
            .collect();
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit("Change clip colour", cmds);
        self.seal();
        ok
    }

    // ── Automation ranges (Live: drag in an automation lane, then edit that range) ──

    fn lane_points(&self, track: TrackId, target: AutomationTarget) -> Vec<AutomationPoint> {
        self.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target)).map(|l| l.points.clone()).unwrap_or_default()
    }

    /// The command that gives the lane `points` (keeping its other settings).
    fn lane_command(&self, track: TrackId, target: AutomationTarget, points: Vec<AutomationPoint>) -> Command {
        let lane = self.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target).cloned()).unwrap_or_else(|| AutomationLane::new(target));
        Command::SetAutomation { track, target, lane: Some(AutomationLane { points, ..lane }) }
    }

    /// One sealed undo step that sets the lane's points; `false` when nothing changes.
    fn set_lane_points(&mut self, track: TrackId, target: AutomationTarget, label: &str, points: Vec<AutomationPoint>) -> bool {
        if points == self.lane_points(track, target) {
            return false;
        }
        let cmd = self.lane_command(track, target, points);
        self.seal();
        let ok = self.edit(label, vec![cmd]);
        self.seal();
        ok
    }

    /// Select `a..b` (either order) in an automation lane; the insert marker
    /// goes to its start.
    pub fn set_auto_range(&mut self, track: TrackId, target: AutomationTarget, a: Tick, b: Tick) {
        let (start, end) = (a.min(b).max(0), a.max(b).max(0));
        self.ed.area = crate::editing::Area::Arrange;
        self.ed.marker = start;
        self.selected_track = Some(track);
        self.auto_sel = Some(AutoRange { track, target, start, end });
    }

    /// The selected automation range, when the arrangement has the keys.
    fn active_auto_range(&self) -> Option<AutoRange> {
        self.auto_sel.filter(|_| self.ed.area == crate::editing::Area::Arrange)
    }

    /// Ctrl+C on an automation range. `None` when no automation range is selected.
    pub(crate) fn copy_auto_range(&mut self) -> Option<bool> {
        let r = self.active_auto_range()?;
        let points = aitrack_model::automation::slice(&self.lane_points(r.track, r.target), r.start, r.end);
        if r.is_empty() || points.is_empty() {
            return Some(false);
        }
        self.auto_clip = Some((points, r.end - r.start));
        Some(true)
    }

    /// Delete on an automation range: the breakpoints inside go and the
    /// envelope runs straight across it; the outside is unchanged.
    pub(crate) fn delete_auto_range(&mut self) -> Option<bool> {
        let r = self.active_auto_range()?;
        if r.is_empty() {
            return Some(false);
        }
        let points = aitrack_model::automation::clear(&self.lane_points(r.track, r.target), r.start, r.end);
        Some(self.set_lane_points(r.track, r.target, "Delete automation", points))
    }

    /// Ctrl+X on an automation range: Copy, then Delete.
    pub(crate) fn cut_auto_range(&mut self) -> Option<bool> {
        let r = self.active_auto_range()?;
        if self.copy_auto_range() != Some(true) {
            return Some(false);
        }
        let points = aitrack_model::automation::clear(&self.lane_points(r.track, r.target), r.start, r.end);
        // Cutting a range that was already flat still copied it.
        self.set_lane_points(r.track, r.target, "Cut automation", points);
        Some(true)
    }

    /// Ctrl+V into the selected automation lane, at the start of its range
    /// (or the caret), over as long as what was copied. The pasted range
    /// becomes the selection. `None` without an automation range or copy.
    pub(crate) fn paste_auto_range(&mut self) -> Option<bool> {
        let r = self.active_auto_range()?;
        let (clip, span) = self.auto_clip.clone()?;
        let points = aitrack_model::automation::paste(&self.lane_points(r.track, r.target), r.start, &clip, span);
        // Pasting what is already there changes nothing, which is still a paste.
        self.set_lane_points(r.track, r.target, "Paste automation", points);
        self.set_auto_range(r.track, r.target, r.start, r.start + span);
        Some(true)
    }

    /// Ctrl+D on an automation range: it again right after itself, and the
    /// copy becomes the selection, so repeating it tiles the range.
    pub(crate) fn duplicate_auto_range(&mut self) -> Option<bool> {
        let r = self.active_auto_range()?;
        if r.is_empty() {
            return Some(false);
        }
        let (points, (a, b)) = aitrack_model::automation::duplicate(&self.lane_points(r.track, r.target), r.start, r.end);
        self.set_lane_points(r.track, r.target, "Duplicate automation", points);
        self.set_auto_range(r.track, r.target, a, b);
        Some(true)
    }

    /// Drag of a selected automation range up or down: `original` moved by
    /// `delta` (normalised), clamped to the parameter's range. One coalesced
    /// undo step per drag (sealed on mouse-up).
    pub fn shift_auto_range(&mut self, range: AutoRange, original: &[AutomationPoint], delta: f32) -> bool {
        let points = aitrack_model::automation::shift(original, range.start, range.end, delta);
        let cmd = self.lane_command(range.track, range.target, points);
        self.edit_coalesced("automation-range", "Move automation", vec![cmd])
    }

    /// The tick step a wave takes as its period: the grid of the arrangement.
    pub fn auto_period(&self) -> Tick {
        self.grid_spec(crate::editing::GridView::Arrange).step.max(1)
    }

    /// "Insert Shape" over the selected range of an automation lane: `shape`
    /// replaces what is there, one cycle per grid step for the waves. One undo step.
    pub fn insert_auto_shape(&mut self, shape: aitrack_model::automation::Shape) -> bool {
        let Some(r) = self.active_auto_range().filter(|r| !r.is_empty()) else { return false };
        let points = aitrack_model::automation::shape(&self.lane_points(r.track, r.target), r.start, r.end, shape, self.auto_period());
        self.set_lane_points(r.track, r.target, &format!("Insert {}", shape.name().to_lowercase()), points)
    }

    /// Tolerance of "Simplify Envelope", of the normalised range: below what a
    /// lane a few dozen pixels tall can show.
    pub const SIMPLIFY_TOLERANCE: f32 = 0.004;

    /// "Simplify Envelope": fewer breakpoints for the same curve, in the
    /// selected range or, with none, the whole lane `track`/`target`.
    /// Returns how many breakpoints went (`None` when there is no lane).
    pub fn simplify_auto(&mut self, track: TrackId, target: AutomationTarget) -> Option<usize> {
        let points = self.lane_points(track, target);
        if points.is_empty() {
            return None;
        }
        let range = self.auto_sel.filter(|r| r.track == track && r.target == target && !r.is_empty());
        let out = match range {
            Some(r) => aitrack_model::automation::simplify_range(&points, r.start, r.end, Self::SIMPLIFY_TOLERANCE),
            None => aitrack_model::automation::simplify(&points, Self::SIMPLIFY_TOLERANCE),
        };
        let removed = points.len().saturating_sub(out.len());
        self.set_lane_points(track, target, "Simplify envelope", out);
        Some(removed)
    }

    /// A drag of the transform handles of the selected range: the lane is
    /// `original` with `xform` applied to `range`. One coalesced undo step
    /// per drag. A time stretch also moves the selection to the new range.
    pub fn xform_auto_range(&mut self, range: AutoRange, original: &[AutomationPoint], xform: AutoXform) -> bool {
        use aitrack_model::automation as a;
        let (points, label) = match xform {
            AutoXform::Scale { anchor, factor } => (a::scale(original, range.start, range.end, anchor, factor), "Scale automation"),
            AutoXform::Skew { left, right } => (a::skew(original, range.start, range.end, left, right), "Skew automation"),
            AutoXform::Stretch { start, end } => (a::stretch(original, range.start, range.end, start, end), "Stretch automation"),
        };
        if let AutoXform::Stretch { start, end } = xform
            && end > start
        {
            self.auto_sel = Some(AutoRange { start: start.max(0), end: end.max(0), ..range });
        }
        let cmd = self.lane_command(range.track, range.target, points);
        self.edit_coalesced("automation-xform", label, vec![cmd])
    }

    /// An arrow key with an automation range selected: ←/→ move its envelope
    /// by a grid step, ↑/↓ raise or lower it by 5 % of the range. Shift
    /// (the keys for a fine step) takes an eighth of the time step and 1 %.
    /// `None` when no range is selected or the key is not one of these.
    pub(crate) fn key_edit_auto_range(&mut self, e: KeyEdit) -> Option<bool> {
        let r = self.active_auto_range().filter(|r| !r.is_empty())?;
        let grid = self.auto_period();
        let fine_t = (grid / 8).max(1);
        let (dt, dv) = match e {
            KeyEdit::Left => (-grid, 0.),
            KeyEdit::Right => (grid, 0.),
            KeyEdit::Shorter => (-fine_t, 0.),
            KeyEdit::Longer => (fine_t, 0.),
            KeyEdit::Up => (0, 0.05),
            KeyEdit::Down => (0, -0.05),
            KeyEdit::OctaveUp => (0, 0.01),
            KeyEdit::OctaveDown => (0, -0.01),
            _ => return None,
        };
        let points = self.lane_points(r.track, r.target);
        if dt != 0 {
            // Not before tick 0.
            let dt = dt.max(-r.start);
            if dt == 0 {
                return Some(false);
            }
            let out = aitrack_model::automation::stretch(&points, r.start, r.end, r.start + dt, r.end + dt);
            let ok = self.set_lane_points(r.track, r.target, "Move automation", out);
            self.set_auto_range(r.track, r.target, r.start + dt, r.end + dt);
            return Some(ok);
        }
        let out = aitrack_model::automation::shift(&points, r.start, r.end, dv);
        Some(self.set_lane_points(r.track, r.target, "Move automation", out))
    }

    /// Capture Automation: the knob moves of the last minutes, written
    /// where they happened, over what the lanes held there, in one undo
    /// step. The lanes touched are shown. Returns how many lanes changed.
    pub fn capture_automation(&mut self) -> usize {
        let gestures = self.capture.gestures();
        let mut cmds = Vec::new();
        let mut shown = Vec::new();
        for (track, target, runs) in gestures {
            if self.project.track(track).is_none() {
                continue;
            }
            let mut points = self.lane_points(track, target);
            let before = points.clone();
            for run in runs {
                let (Some(first), Some(last)) = (run.first().copied(), run.last().copied()) else { continue };
                if run.len() == 1 || last.tick <= first.tick {
                    points = aitrack_model::automation::write(&points, first.tick, first.tick, first.value);
                } else {
                    let clip: Vec<AutomationPoint> = run.iter().map(|p| AutomationPoint { tick: p.tick - first.tick, ..*p }).collect();
                    points = aitrack_model::automation::paste(&points, first.tick, &clip, last.tick - first.tick);
                }
            }
            if points != before {
                cmds.push(self.lane_command(track, target, points));
                shown.push((track, target));
            }
        }
        if cmds.is_empty() {
            return 0;
        }
        self.seal();
        let n = cmds.len();
        let ok = self.edit("Capture automation", cmds);
        self.seal();
        if !ok {
            return 0;
        }
        for (track, target) in shown {
            self.show_automation(track, target);
        }
        self.capture.clear_moves();
        n
    }

    /// Capture MIDI: the notes played on armed tracks while the transport
    /// rolled, as a clip on each track at the position they were played,
    /// in one undo step. Returns how many tracks got a clip.
    pub fn capture_midi(&mut self) -> usize {
        let notes = self.capture.notes();
        let Some(first) = notes.iter().map(|n| n.start_tick).min() else { return 0 };
        let last = notes.iter().map(|n| n.start_tick + n.length_ticks).max().unwrap_or(first);
        // The clip starts on the beat the first note falls in.
        let start = first.max(0).div_euclid(PPQ) * PPQ;
        let take = crate::bridge::MidiTake { notes, start_tick: start, end_tick: last, passes: 1 };
        let n = self.commit_take_as(&take, "Capture MIDI");
        if n > 0 {
            self.capture.clear_notes();
        }
        n
    }

    /// Delete whatever is selected: notes in the piano roll, else the selected arrangement clip.
    pub fn delete_selection(&mut self) -> bool {
        if let Some(done) = self.delete_auto_range() {
            return done;
        }
        if let Some(done) = self.delete_arrangement() {
            return done;
        }
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
                clip: Box::new(Clip::new(name, color, src, length)),
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

pub fn probability_delta(originals: &[Note], delta: f32) -> f32 {
    if originals.is_empty() { return 0. }
    let lower = originals.iter().map(|n| 0. - n.probability).fold(f32::NEG_INFINITY, f32::max);
    let upper = originals.iter().map(|n| 1. - n.probability).fold(f32::INFINITY, f32::min);
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordKind {
    Triad,
    Seventh,
    Ninth,
    Sus2,
    Sus4,
    Power,
}

impl ChordKind {
    pub fn steps(self, scale: Option<Scale>) -> &'static [i32] {
        match (self, scale.is_some()) {
            (Self::Triad, true) => &[2, 4],
            (Self::Triad, false) => &[4, 7],
            (Self::Seventh, true) => &[2, 4, 6],
            (Self::Seventh, false) => &[4, 7, 11],
            (Self::Ninth, true) => &[2, 4, 6, 8],
            (Self::Ninth, false) => &[4, 7, 11, 14],
            (Self::Sus2, true) => &[1, 4],
            (Self::Sus2, false) => &[2, 7],
            (Self::Sus4, true) => &[3, 4],
            (Self::Sus4, false) => &[5, 7],
            (Self::Power, true) => &[4],
            (Self::Power, false) => &[7],
        }
    }

    pub fn edit_label(self) -> &'static str {
        match self {
            Self::Triad => "Build triad chord",
            Self::Seventh => "Build 7th chord",
            Self::Ninth => "Build 9th chord",
            Self::Sus2 => "Build sus2 chord",
            Self::Sus4 => "Build sus4 chord",
            Self::Power => "Build power chord",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArpPattern {
    Up,
    Down,
    UpDown,
}

/// Find the closest row index for `key` in a descending [`AppState::piano_rows`] list.
pub fn piano_nearest_row(rows: &[u8], key: u8) -> usize {
    rows.iter()
        .enumerate()
        .min_by_key(|(_, k)| (**k as i32 - key as i32).abs())
        .map_or(0, |(r, _)| r)
}

/// Step `key` by `steps` scale degrees (`steps > 0` moves up, `steps < 0` moves down).
pub fn step_note_in_scale(scale: Scale, key: u8, steps: i32) -> u8 {
    let mut k = key;
    let dir = steps.signum();
    for _ in 0..steps.abs() {
        k = step_in_scale(scale, k, dir);
    }
    k
}

/// Move `key` one step inside `scale` (`dir` is ±1).
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

    fn device_with_state(s: &mut AppState) -> (TrackId, DeviceId) {
        let track = s.add_track(TrackKind::Instrument).unwrap();
        let id = s.add_stock_device(track, "fx.eq", "EQ").unwrap();
        s.seal();
        (track, id)
    }

    #[test]
    fn saving_captures_plugin_states_without_an_undo_step() {
        let mut s = state();
        let (track, device) = device_with_state(&mut s);
        s.set_track(track, TrackProp::VolumeDb(-3.));
        s.seal();
        let (steps, revision, label) = (s.history.steps().count(), s.revision(), s.history.undo_label().map(str::to_owned));
        assert!(s.capture_plugin_states(vec![(device, vec![7, 7]), (DeviceId(u64::MAX), vec![1])]));
        assert_eq!(s.project.device(device).unwrap().state.as_deref(), Some(&[7u8, 7][..]));
        assert_eq!(s.history.steps().count(), steps, "no \"Plugin state\" step");
        assert_eq!(s.revision(), revision);
        assert_eq!(s.history.undo_label().map(str::to_owned), label);
        assert!(!s.capture_plugin_states(vec![(device, vec![7, 7])]), "unchanged: nothing to do");
        // Cmd+Z after saving undoes the user's last edit.
        s.undo();
        assert_eq!(s.project.track(track).unwrap().volume_db, 0.);
    }

    #[test]
    fn the_export_gets_the_live_unsaved_plugin_state() {
        let mut s = state();
        let (_, device) = device_with_state(&mut s);
        let mut engine = crate::bridge::StubEngine::default();
        engine.live_states = vec![(device, vec![9, 9, 9])];
        assert_eq!(s.project.device(device).unwrap().state, None, "nothing saved yet");
        let steps = s.history.steps().count();
        let exported = s.export_snapshot(&mut engine);
        assert_eq!(exported.device(device).unwrap().state.as_deref(), Some(&[9u8, 9, 9][..]));
        assert_eq!(s.history.steps().count(), steps, "exporting adds no undo step");
    }

    fn names(s: &AppState) -> Vec<String> {
        s.project.tracks.iter().map(|t| t.name.clone()).collect()
    }

    fn add_named(s: &mut AppState, kind: TrackKind, name: &str) -> TrackId {
        let id = s.add_track(kind).unwrap();
        s.set_track(id, TrackProp::Name(name.into()));
        s.seal();
        id
    }

    #[test]
    fn tracks_are_reordered_as_siblings_and_never_past_the_returns_or_main() {
        let mut s = AppState::new(Project::new("t"));
        let (a, b, c) = (add_named(&mut s, TrackKind::Audio, "A"), add_named(&mut s, TrackKind::Audio, "B"), add_named(&mut s, TrackKind::Audio, "C"));
        let r = add_named(&mut s, TrackKind::Return, "R");
        assert_eq!(names(&s), ["A", "B", "C", "R", "Main"]);
        // C before A.
        assert!(s.move_track(c, a, false));
        assert_eq!(names(&s)[..3], ["C", "A", "B"]);
        // A after B, which is the last of the tracks (still before the return).
        assert!(s.move_track(a, b, true));
        assert_eq!(names(&s)[..3], ["C", "B", "A"]);
        assert_eq!(s.project.tracks.iter().position(|t| t.id == r), Some(3));
        // A track cannot go among the returns, nor onto itself, nor move Main.
        assert!(!s.move_track(a, r, false));
        assert!(!s.move_track(a, a, true));
        let main = s.project.main_track().id;
        assert!(!s.move_track(main, a, false) && !s.move_track(a, main, false));
        // Already there: no step.
        let steps = s.history.steps().count();
        assert!(!s.move_track(c, b, false));
        assert_eq!(s.history.steps().count(), steps);
        // One undo step per move.
        s.undo();
        assert_eq!(names(&s)[..3], ["C", "A", "B"]);
    }

    #[test]
    fn a_group_moves_with_its_tracks() {
        let mut s = AppState::new(Project::new("t"));
        let (a, b, c) = (add_named(&mut s, TrackKind::Audio, "A"), add_named(&mut s, TrackKind::Audio, "B"), add_named(&mut s, TrackKind::Audio, "C"));
        let d = add_named(&mut s, TrackKind::Audio, "D");
        s.ed.tracks = [b, c].into();
        s.selected_track = Some(b);
        let group = s.group_tracks().expect("B and C are grouped");
        let order = |s: &AppState| names(s).into_iter().filter(|n| n != "Main").collect::<Vec<_>>();
        // The group goes after D: its members follow it.
        assert!(s.move_track(group, d, true));
        let o = order(&s);
        let at = |n: &str| o.iter().position(|x| x == n).unwrap();
        let g = at("Group");
        assert!(at("D") < g, "the group went after D: {o:?}");
        assert_eq!(at("B").min(at("C")), g + 1, "its members right after it, in order: {o:?}");
        assert_eq!(at("B").max(at("C")), g + 2, "{o:?}");
        // Dropped next to a track of a group, a track joins that group.
        let member = if at("B") < at("C") { "B" } else { "C" };
        let member = s.project.tracks.iter().find(|t| t.name == member).unwrap().id;
        assert!(s.move_track(a, member, false));
        assert_eq!(s.project.track(a).unwrap().parent, Some(group));
    }

    #[test]
    fn duplicating_tracks_copies_devices_and_clips_and_selects_the_copy() {
        let mut s = AppState::new(Project::new("t"));
        let t = add_named(&mut s, TrackKind::Instrument, "Keys");
        s.add_stock_device(t, "fx.eq", "EQ").unwrap();
        let (src, clip, pl) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
        let note = Note::new(s.project.alloc_note_id(), 60, 0, PPQ, 0.8);
        s.edit("clip", vec![
            Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes: vec![note] })) },
            Command::AddClip { id: clip, clip: Box::new(Clip::new("riff", Color::palette(1), src, PPQ)) },
            Command::PlaceClip { track: t, placement: Placement { id: pl, clip, start: 0, lane: 0 } },
        ]);
        s.seal();
        s.selected_track = Some(t);
        s.ed.area = crate::editing::Area::Arrange;
        s.selected_placement = None;
        let steps = s.history.steps().count();
        assert!(s.duplicate_selection(), "Cmd+D on a track header");
        assert_eq!(s.history.steps().count(), steps + 1, "one undo step");
        let copy = s.project.tracks.iter().find(|x| x.name == "Keys copy").expect("the copy");
        assert_eq!(copy.devices.len(), 1);
        assert_ne!(copy.devices[0].id, s.project.track(t).unwrap().devices[0].id, "its own device");
        assert_eq!(copy.arrangement.len(), 1);
        assert_ne!(copy.arrangement[0].clip, clip, "its own clip");
        assert_eq!(s.selected_track, Some(copy.id));
        s.undo();
        assert!(s.project.tracks.iter().all(|x| x.name != "Keys copy"));
        let main = s.project.main_track().id;
        assert!(!s.duplicate_tracks(&[main]), "Main cannot be duplicated");
    }

    #[test]
    fn a_tempo_drag_keeps_the_samples_as_long_as_they_are_in_seconds_in_one_undo_step() {
        let mut s = AppState::new(Project::new("t"));
        let t = add_named(&mut s, TrackKind::Audio, "Loop");
        let (src, clip, pl) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
        // A one-second file, two beats at 120 BPM, with a small clip next to it.
        let file = AudioSource { path: "x.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames: 48_000, warp: Warp::default(), key: None };
        s.edit("clip", vec![
            Command::AddSource { id: src, source: Box::new(Source::Audio(file)) },
            Command::AddClip { id: clip, clip: Box::new(Clip::new("hit", Color::palette(1), src, 2 * PPQ)) },
            Command::PlaceClip { track: t, placement: Placement { id: pl, clip, start: 0, lane: 0 } },
        ]);
        s.seal();
        let steps = s.history.steps().count();
        // Drag 120 -> 140 in small steps and let go.
        for i in 1..=200 {
            assert!(s.set_tempo(120. + i as f64 * 0.1));
        }
        s.seal();
        assert_eq!(s.project.tempo.initial_bpm(), 140.);
        // One second at 140 BPM is 2.333 beats: the clip is longer in ticks, to the tick.
        assert_eq!(s.project.clips[&clip].length, (PPQ as f64 * 2. * 140. / 120.).round() as Tick);
        assert_eq!(s.history.steps().count(), steps + 1, "the whole drag is one step");
        // Back to the start tempo in the same way: nothing drifted.
        for i in 1..=200 {
            s.set_tempo(140. - i as f64 * 0.1);
        }
        s.seal();
        assert_eq!(s.project.tempo.initial_bpm(), 120.);
        assert_eq!(s.project.clips[&clip].length, 2 * PPQ);
        s.undo();
        assert_eq!(s.project.tempo.initial_bpm(), 140., "undo goes back to the tempo before the second drag");
        assert_eq!(s.project.clips[&clip].length, (PPQ as f64 * 2. * 140. / 120.).round() as Tick);
        s.undo();
        assert_eq!((s.project.tempo.initial_bpm(), s.project.clips[&clip].length), (120., 2 * PPQ));
    }

    #[test]
    fn a_drag_of_knobs_faders_and_sends_never_sends_the_project() {
        use crate::bridge::{EngineBridge, StubEngine};
        let mut s = AppState::new(crate::demo::project());
        let mut engine = StubEngine::default();
        let mut synced = None;
        assert_eq!(s.sync_engine(&mut engine, &mut synced), Synced::Full, "the first sync sends the project");
        assert_eq!(s.sync_engine(&mut engine, &mut synced), Synced::Nothing);
        let (updates, compiles) = (engine.project_updates, engine.compile_count());
        let track = s.project.tracks.iter().find(|t| t.kind == TrackKind::Audio && !t.sends.is_empty()).map(|t| t.id).or_else(|| s.project.tracks.iter().find(|t| !t.sends.is_empty()).map(|t| t.id)).expect("a track with sends");
        let device = s.project.tracks.iter().flat_map(|t| &t.devices).next().map(|d| d.id).expect("a device");
        s.seal();
        // 60 frames: a fader, a send and a knob move together.
        for i in 0..60 {
            let v = i as f32 / 60.;
            s.set_track_coalesced(&format!("fader-{}", track.0), track, TrackProp::VolumeDb(-30. + v * 30.));
            s.set_send(track, 0, -40. + v * 30.);
            s.set_param(device, 3, v);
            assert_eq!(s.sync_engine(&mut engine, &mut synced), Synced::Fast, "frame {i}");
        }
        s.seal();
        assert_eq!(engine.project_updates, updates, "not one project snapshot during the drag");
        assert_eq!(engine.compile_count(), compiles, "and so no compile");
        assert_eq!(engine.fast_calls, 60 * 3);
        // The drag is one undo step per control, and the engine is told the project once it settles.
        assert!(s.engine_is_behind());
        assert!(s.settle_engine(&mut engine));
        assert_eq!(engine.project_updates, updates + 1);
        assert!(!s.settle_engine(&mut engine), "once");
        // Anything else sends the project, and so does an undo.
        s.edit("Rename", vec![Command::SetTrack { id: track, change: TrackProp::Name("Renamed".into()) }]);
        assert_eq!(s.sync_engine(&mut engine, &mut synced), Synced::Full);
        s.undo();
        assert_eq!(s.sync_engine(&mut engine, &mut synced), Synced::Full);
    }

    #[test]
    fn dragging_a_macro_moves_its_mapped_parameters_live_and_never_sends_the_project() {
        use crate::bridge::StubEngine;
        let mut s = AppState::new(Project::new("t"));
        let t = add_named(&mut s, TrackKind::Audio, "FX");
        let rack = s.add_stock_device(t, "rack.audio", "Rack").unwrap();
        let (a, b) = (s.project.alloc_device_id(), s.project.alloc_device_id());
        s.edit("chain", vec![
            Command::AddChain { rack, index: 0, chain: Box::new(Chain::new("A", Color::palette(1))) },
            Command::AddDevice { slot: DeviceSlot::Rack { rack, chain: 0, index: 0 }, device: Box::new(Device::stock(a, "fx.filter", "Filter")) },
            Command::AddDevice { slot: DeviceSlot::Rack { rack, chain: 0, index: 1 }, device: Box::new(Device::stock(b, "fx.eq", "EQ")) },
            Command::AddMapping { rack, macro_index: 0, index: 0, mapping: MacroMapping { device: a, param: 0, min: 0.2, max: 0.8 } },
            Command::AddMapping { rack, macro_index: 0, index: 1, mapping: MacroMapping { device: b, param: 1, min: 0.0, max: 1.0 } },
        ]);
        s.seal();
        let mut e = StubEngine::default();
        let mut synced = None;
        s.sync_engine(&mut e, &mut synced);
        let updates = e.project_updates;
        for i in 0..60 {
            s.set_macro(rack, 0, i as f32 / 59.);
            assert_eq!(s.sync_engine(&mut e, &mut synced), Synced::Fast, "frame {i}");
        }
        s.seal();
        assert_eq!(e.project_updates, updates, "no project snapshot while the macro moves");
        assert_eq!(e.fast_calls, 60 * 2, "each frame sets the two mapped parameters");
        // A macro with nothing mapped has nothing to tell the engine, and still costs no snapshot.
        let calls = e.fast_calls;
        s.set_macro(rack, 5, 0.5);
        assert_eq!(s.sync_engine(&mut e, &mut synced), Synced::Fast);
        assert_eq!((e.fast_calls, e.project_updates), (calls, updates));
    }

    #[test]
    fn a_move_that_is_not_a_plain_value_change_takes_the_slow_way() {
        use crate::bridge::StubEngine;
        let mut s = AppState::new(crate::demo::project());
        let mut engine = StubEngine::default();
        let mut synced = None;
        s.sync_engine(&mut engine, &mut synced);
        let track = s.project.tracks.iter().find(|t| !t.sends.is_empty()).map(|t| t.id).unwrap();
        // A send's pre/post switch is a routing change: the engine compiles.
        let mut sends = s.project.track(track).unwrap().sends.clone();
        sends[0].pre_fader = !sends[0].pre_fader;
        s.edit_coalesced("send", "Change sends", vec![Command::SetTrack { id: track, change: TrackProp::Sends(sends) }]);
        assert_eq!(s.sync_engine(&mut engine, &mut synced), Synced::Full);
        // A fader move after a different edit in between is also not covered by the log.
        s.set_track_coalesced("fader", track, TrackProp::VolumeDb(-5.));
        s.edit("Rename", vec![Command::SetTrack { id: track, change: TrackProp::Name("X".into()) }]);
        assert_eq!(s.sync_engine(&mut engine, &mut synced), Synced::Full);
    }

    #[test]
    fn cmd_d_in_the_device_view_duplicates_the_picked_devices_after_themselves() {
        let mut s = AppState::new(Project::new("t"));
        let t = add_named(&mut s, TrackKind::Instrument, "Keys");
        let eq = s.add_stock_device(t, "fx.eq", "EQ").unwrap();
        let comp = s.add_stock_device(t, "fx.compressor", "Comp").unwrap();
        s.seal();
        s.ed.area = crate::editing::Area::Devices;
        s.ed.devices = vec![eq];
        let steps = s.history.steps().count();
        assert!(s.duplicate_selection());
        assert_eq!(s.history.steps().count(), steps + 1, "one undo step");
        let chain: Vec<&str> = s.project.track(t).unwrap().devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(chain, ["EQ", "EQ", "Comp"], "the copy sits right after the original");
        let copy = s.project.track(t).unwrap().devices[1].id;
        assert!(copy != eq && copy != comp);
        assert_eq!(s.ed.devices, vec![copy], "the copy is the pick now");
        s.undo();
        assert_eq!(s.project.track(t).unwrap().devices.len(), 2);
    }

    fn armed_keys(s: &mut AppState) -> TrackId {
        let t = add_named(s, TrackKind::Instrument, "Keys");
        s.set_track(t, TrackProp::Arm(true));
        s.seal();
        t
    }

    fn rec_note(track: TrackId, key: u8, start: Tick, len: Tick) -> crate::bridge::RecNote {
        crate::bridge::RecNote { track, key, velocity: 0.8, start_tick: start, length_ticks: len, pass: 0 }
    }

    fn clips_of(s: &AppState, t: TrackId) -> Vec<(Tick, Tick, usize)> {
        let mut v: Vec<_> = s
            .project
            .track(t)
            .unwrap()
            .arrangement
            .iter()
            .map(|p| {
                let c = &s.project.clips[&p.clip];
                let n = match s.project.sources.get(&c.source) {
                    Some(Source::Notes(n)) => n.notes.len(),
                    _ => 0,
                };
                (p.start, c.length, n)
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_take_becomes_a_clip_with_its_notes_in_one_undo_step() {
        use crate::bridge::{EngineBridge, MidiTake, StubEngine};
        let mut s = AppState::new(Project::new("t"));
        let t = armed_keys(&mut s);
        let mut e = StubEngine::default();
        e.armed_midi = vec![t];
        e.toggle_record();
        e.play();
        let steps = s.history.steps().count();
        // While the take runs, finished notes are collected and the clip is shown growing.
        e.played = vec![rec_note(t, 60, 4 * PPQ, PPQ / 2)];
        let poll = s.poll_recording(&mut e);
        assert!(poll.changed && poll.committed == 0);
        assert_eq!(s.rec_view.as_ref().map(|v| v.tracks.clone()), Some(vec![t]));
        assert_eq!(s.rec_notes.len(), 1);
        assert_eq!(s.history.steps().count(), steps, "nothing in the project until the take ends");
        // Stop: the take ends with the notes not handed out yet.
        e.stop();
        e.rec_take = Some(MidiTake { notes: vec![rec_note(t, 64, 5 * PPQ, PPQ), rec_note(t, 67, 6 * PPQ, 2 * PPQ)], start_tick: 4 * PPQ, end_tick: 8 * PPQ, passes: 1 });
        let poll = s.poll_recording(&mut e);
        assert_eq!(poll.committed, 1);
        assert!(s.rec_view.is_none() && s.rec_notes.is_empty());
        assert_eq!(clips_of(&s, t), [(4 * PPQ, 4 * PPQ, 3)], "one clip from where the take began, to where it stopped, with every note");
        assert_eq!(s.history.steps().count(), steps + 1);
        let Some(Source::Notes(n)) = s.project.sources.values().find(|x| matches!(x, Source::Notes(_))) else { panic!() };
        let mut starts: Vec<_> = n.notes.iter().map(|n| n.start).collect();
        starts.sort();
        assert_eq!(starts, [0, PPQ, 2 * PPQ], "notes are relative to the clip: from tick 0");
        assert_eq!(s.history.undo_label(), Some("Record MIDI"));
        s.undo();
        assert!(clips_of(&s, t).is_empty());
        // Nothing played: no clip, no step.
        e.rec_take = Some(MidiTake { notes: vec![], start_tick: 0, end_tick: 4 * PPQ, passes: 1 });
        assert_eq!(s.poll_recording(&mut e).committed, 0);
        assert!(clips_of(&s, t).is_empty());
    }

    #[test]
    fn a_take_replaces_what_it_lands_on_like_any_clip() {
        use crate::bridge::{MidiTake, StubEngine};
        let mut s = AppState::new(Project::new("t"));
        let t = armed_keys(&mut s);
        let (src, clip, pl) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
        s.edit("old", vec![
            Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes: vec![] })) },
            Command::AddClip { id: clip, clip: Box::new(Clip::new("old", Color::palette(1), src, 8 * PPQ)) },
            Command::PlaceClip { track: t, placement: Placement { id: pl, clip, start: 0, lane: 0 } },
        ]);
        s.seal();
        let mut e = StubEngine::default();
        e.rec_take = Some(MidiTake { notes: vec![rec_note(t, 60, 2 * PPQ, PPQ)], start_tick: 2 * PPQ, end_tick: 4 * PPQ, passes: 1 });
        assert_eq!(s.poll_recording(&mut e).committed, 1);
        let spans: Vec<(Tick, Tick)> = clips_of(&s, t).into_iter().map(|c| (c.0, c.1)).collect();
        assert_eq!(spans, [(0, 2 * PPQ), (2 * PPQ, 2 * PPQ), (4 * PPQ, 4 * PPQ)], "the old clip keeps what is left either side");
    }

    #[test]
    fn recording_in_a_loop_overdubs_the_clip_that_is_already_there() {
        use crate::bridge::{MidiTake, StubEngine};
        let mut s = AppState::new(Project::new("t"));
        let t = armed_keys(&mut s);
        let other = add_named(&mut s, TrackKind::Instrument, "Other");
        s.set_track(other, TrackProp::Arm(true));
        s.edit("loop", vec![Command::SetLoop { region: Some((4 * PPQ, 8 * PPQ)) }]);
        s.seal();
        let mut e = StubEngine::default();
        // Two passes over the loop: notes of both land in one clip over the loop.
        e.rec_take = Some(MidiTake {
            notes: vec![rec_note(t, 60, 4 * PPQ, PPQ), crate::bridge::RecNote { pass: 1, ..rec_note(t, 62, 6 * PPQ, PPQ) }, rec_note(other, 48, 5 * PPQ, PPQ)],
            start_tick: 4 * PPQ,
            end_tick: 8 * PPQ,
            passes: 2,
        });
        assert_eq!(s.poll_recording(&mut e).committed, 2, "both armed tracks");
        assert_eq!(clips_of(&s, t), [(4 * PPQ, 4 * PPQ, 2)]);
        assert_eq!(clips_of(&s, other), [(4 * PPQ, 4 * PPQ, 1)]);
        // Another take over the same loop: the notes are added to the clip.
        e.rec_take = Some(MidiTake { notes: vec![rec_note(t, 65, 7 * PPQ, PPQ / 2)], start_tick: 4 * PPQ, end_tick: 8 * PPQ, passes: 2 });
        let steps = s.history.steps().count();
        assert_eq!(s.poll_recording(&mut e).committed, 1);
        assert_eq!(clips_of(&s, t), [(4 * PPQ, 4 * PPQ, 3)], "still one clip, one note more");
        assert_eq!(s.history.steps().count(), steps + 1);
        s.undo();
        assert_eq!(clips_of(&s, t), [(4 * PPQ, 4 * PPQ, 2)], "undo takes only the overdub back");
    }

    #[test]
    fn after_duplicating_a_track_with_devices_every_new_id_is_still_free() {
        let mut s = AppState::new(Project::new("t"));
        let t = add_named(&mut s, TrackKind::Instrument, "Keys");
        s.add_stock_device(t, "fx.eq", "EQ").unwrap();
        s.add_stock_device(t, "rack.audio", "Rack").unwrap();
        s.seal();
        s.selected_track = Some(t);
        assert!(s.duplicate_tracks(&[t]));
        // Devices, tracks, clips and notes added afterwards get ids nobody has.
        let again = s.add_stock_device(t, "fx.compressor", "Comp").expect("a device can be added after duplicating");
        let mut ids = vec![again];
        ids.extend(s.project.tracks.iter().flat_map(|x| x.devices.iter().map(|d| d.id)).filter(|d| *d != again));
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "no two devices share an id");
        let later = s.add_track(TrackKind::Audio).expect("and a track");
        assert!(s.project.tracks.iter().filter(|x| x.id == later).count() == 1);
        assert!(s.last_error.is_none(), "{:?}", s.last_error);
    }

    fn audio_track_with_input(s: &mut AppState) -> TrackId {
        let t = add_named(s, TrackKind::Audio, "Mic");
        s.set_track(t, TrackProp::Input(InputSource::Hardware { first: 0, stereo: false }));
        s.set_track(t, TrackProp::Arm(true));
        s.seal();
        t
    }

    fn take_of(track: TrackId, name: &str, start: Tick, frames: u64) -> crate::bridge::AudioTake {
        crate::bridge::AudioTake { track, path: format!("/rec/{name}.wav").into(), start_tick: start, frames, sample_rate: 48_000, channels: 1, error: None }
    }

    #[test]
    fn an_audio_take_becomes_a_clip_at_its_start_tick_in_one_undo_step() {
        use crate::bridge::{EngineBridge, StubEngine};
        let mut s = AppState::new(Project::new("t"));
        let (a, b) = (audio_track_with_input(&mut s), audio_track_with_input(&mut s));
        let mut e = StubEngine::default();
        e.armed_audio = vec![a, b];
        e.toggle_record();
        e.play();
        let now = std::time::Instant::now();
        assert_eq!(s.poll_recording_at(&mut e, now), RecPoll { changed: true, ..RecPoll::default() }, "capturing: nothing in the project yet");
        e.stop();
        // The files arrive after the take is over: one now, the other a moment later.
        e.audio_takes = vec![take_of(a, "a", 8 * PPQ, 96_000)];
        let steps = s.history.steps().count();
        let poll = s.poll_recording_at(&mut e, now);
        assert_eq!(poll.audio_clips, 0, "still waiting for the second file");
        assert_eq!(s.history.steps().count(), steps);
        e.audio_takes = vec![take_of(b, "b", 8 * PPQ, 96_000)];
        let poll = s.poll_recording_at(&mut e, now);
        assert_eq!(poll.audio_clips, 2);
        assert_eq!(s.history.steps().count(), steps + 1, "one undo step for both files");
        assert_eq!(s.history.undo_label(), Some("Record audio"));
        for t in [a, b] {
            let pl = &s.project.track(t).unwrap().arrangement;
            assert_eq!(pl.len(), 1);
            assert_eq!(pl[0].start, 8 * PPQ, "at the tick the take began");
            // Two seconds at 120 BPM: four beats.
            assert_eq!(s.project.clips[&pl[0].clip].length, 4 * PPQ);
        }
        s.undo();
        assert!(s.project.track(a).unwrap().arrangement.is_empty() && s.project.track(b).unwrap().arrangement.is_empty());
    }

    #[test]
    fn an_audio_take_replaces_what_it_lands_on() {
        use crate::bridge::{StubEngine};
        let mut s = AppState::new(Project::new("t"));
        let t = audio_track_with_input(&mut s);
        let (src, clip, pl) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
        s.edit("old", vec![
            Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes: vec![] })) },
            Command::AddClip { id: clip, clip: Box::new(Clip::new("old", Color::palette(1), src, 16 * PPQ)) },
            Command::PlaceClip { track: t, placement: Placement { id: pl, clip, start: 0, lane: 0 } },
        ]);
        s.seal();
        let mut e = StubEngine::default();
        e.audio_takes = vec![take_of(t, "over", 4 * PPQ, 48_000)];
        e.armed_audio = vec![t];
        assert_eq!(s.poll_recording(&mut e).audio_clips, 1);
        let spans: Vec<(Tick, Tick)> = clips_of(&s, t).into_iter().map(|c| (c.0, c.1)).collect();
        assert_eq!(spans, [(0, 4 * PPQ), (4 * PPQ, 2 * PPQ), (6 * PPQ, 10 * PPQ)], "the old clip keeps what is left either side");
    }

    #[test]
    fn a_failed_take_is_reported_and_a_missing_file_is_given_up_on() {
        use crate::bridge::{EngineBridge, StubEngine};
        let mut s = AppState::new(Project::new("t"));
        let (a, b) = (audio_track_with_input(&mut s), audio_track_with_input(&mut s));
        let mut e = StubEngine::default();
        e.armed_audio = vec![a, b];
        e.toggle_record();
        e.play();
        let t0 = std::time::Instant::now();
        s.poll_recording_at(&mut e, t0);
        e.stop();
        // One file failed to write after a few frames; the other never shows up.
        e.audio_takes = vec![crate::bridge::AudioTake { error: Some("disk full".into()), ..take_of(a, "a", 0, 4_800) }];
        let early = s.poll_recording_at(&mut e, t0);
        assert!(early.errors.is_empty() && early.audio_clips == 0, "still waiting for the other");
        let late = s.poll_recording_at(&mut e, t0 + std::time::Duration::from_secs(11));
        assert_eq!(late.audio_clips, 1, "what made it to the disk is kept");
        assert!(late.errors.iter().any(|m| m.contains("disk full") && m.contains("Mic")), "{:?}", late.errors);
        assert!(late.errors.iter().any(|m| m.contains("never arrived")), "{:?}", late.errors);
        // A take of a track whose input was switched off meanwhile is left out.
        s.set_track(b, TrackProp::Input(InputSource::None));
        assert_eq!(s.commit_audio_takes(&[take_of(b, "b", 0, 1_000)], None).0, 0);
    }

    #[test]
    fn the_engine_is_told_each_audio_tracks_input_and_monitor_when_they_change() {
        use crate::bridge::StubEngine;
        let mut s = AppState::new(Project::new("t"));
        let t = add_named(&mut s, TrackKind::Audio, "Mic");
        let keys = add_named(&mut s, TrackKind::Instrument, "Keys");
        let mut e = StubEngine::default();
        let mut sent = BTreeMap::new();
        assert_eq!(s.sync_track_io(&mut sent, &mut e), 1, "audio tracks only, once");
        assert_eq!(s.sync_track_io(&mut sent, &mut e), 0, "nothing changed");
        assert!(!e.track_io.contains_key(&keys));
        let pair = InputSource::Hardware { first: 2, stereo: true };
        s.set_track(t, TrackProp::Input(pair));
        s.set_track(t, TrackProp::Monitor(Monitor::In));
        assert_eq!(s.sync_track_io(&mut sent, &mut e), 1);
        assert_eq!(e.track_io[&t], (pair, Monitor::In));
        // A project opened later starts from nothing sent: everything is told again.
        sent.clear();
        assert_eq!(s.sync_track_io(&mut sent, &mut e), 1);
        // A deleted track is forgotten.
        s.delete_tracks(&[t]);
        s.sync_track_io(&mut sent, &mut e);
        assert!(sent.is_empty());
    }

    #[test]
    fn audio_takes_go_next_to_a_saved_project_or_under_the_settings() {
        let saved = std::env::temp_dir().join("song").join("Song.aitrack");
        let dir = recordings_dir(Some(&saved));
        assert!(dir.ends_with("Recordings") && dir.parent().is_some_and(|p| p.ends_with("song")), "{dir:?}");
        let unsaved = recordings_dir(None);
        assert!(unsaved.ends_with("Recordings") && unsaved.starts_with(crate::appearance::config_dir()));
    }

    #[test]
    fn loop_takes_keep_each_note_where_the_engine_put_it_whatever_pass_it_was_played_in() {
        use crate::bridge::{MidiTake, RecNote, StubEngine};
        let mut s = AppState::new(Project::new("t"));
        let t = armed_keys(&mut s);
        s.edit("loop", vec![Command::SetLoop { region: Some((4 * PPQ, 8 * PPQ)) }]);
        s.seal();
        let mut e = StubEngine::default();
        // Recording started in the middle of the loop (beat 6): the first pass
        // runs 6..8, the second 4..8. The engine reports in-loop ticks for both.
        let note = |key, at, pass| RecNote { pass, ..rec_note(t, key, at, PPQ / 2) };
        e.rec_take = Some(MidiTake { notes: vec![note(60, 6 * PPQ, 0), note(62, 4 * PPQ, 1), note(60, 6 * PPQ, 1), note(64, 7 * PPQ, 2)], start_tick: 6 * PPQ, end_tick: 7 * PPQ + PPQ / 2, passes: 3 });
        assert_eq!(s.poll_recording(&mut e).committed, 1);
        // One clip over the loop (not over the take), notes at their loop positions.
        assert_eq!(clips_of(&s, t), [(4 * PPQ, 4 * PPQ, 4)]);
        let Some(Source::Notes(n)) = s.project.sources.values().find(|x| matches!(x, Source::Notes(_))) else { panic!() };
        let mut at: Vec<(u8, Tick)> = n.notes.iter().map(|n| (n.key, n.start)).collect();
        at.sort();
        assert_eq!(at, [(60, 2 * PPQ), (60, 2 * PPQ), (62, 0), (64, 3 * PPQ)], "beat 6 of the song is beat 2 of the clip in every pass");
        s.undo();
        // A take that began before the loop (an intro) keeps its intro in the clip.
        e.rec_take = Some(MidiTake { notes: vec![note(60, 2 * PPQ, 0), note(62, 5 * PPQ, 1)], start_tick: 2 * PPQ, end_tick: 8 * PPQ, passes: 2 });
        assert_eq!(s.poll_recording(&mut e).committed, 1);
        assert_eq!(clips_of(&s, t), [(2 * PPQ, 6 * PPQ, 2)], "from where it began to the loop end");
        s.undo();
        // Looping on but the loop never wrapped: an ordinary take, relative to where it began.
        e.rec_take = Some(MidiTake { notes: vec![note(60, 5 * PPQ, 0)], start_tick: 5 * PPQ, end_tick: 7 * PPQ, passes: 1 });
        assert_eq!(s.poll_recording(&mut e).committed, 1);
        assert_eq!(clips_of(&s, t), [(5 * PPQ, 2 * PPQ, 1)]);
    }

    #[test]
    fn a_loop_recording_fills_only_the_loop_with_its_last_whole_pass() {
        let mut s = AppState::new(Project::new("t"));
        let t = audio_track_with_input(&mut s);
        let bar = 4 * PPQ;
        // Something after the loop that the recording must not touch.
        let (src, clip, pl) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
        s.edit("later", vec![
            Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes: vec![] })) },
            Command::AddClip { id: clip, clip: Box::new(Clip::new("later", Color::palette(1), src, 4 * bar)) },
            Command::PlaceClip { track: t, placement: Placement { id: pl, clip, start: 2 * bar, lane: 0 } },
        ]);
        s.seal();
        // Three passes over a one-bar loop at 120 BPM: 6 s of audio, 288 000 frames.
        let spans = |s: &AppState| -> Vec<(Tick, Tick, Tick)> {
            let mut v: Vec<_> = s.project.track(t).unwrap().arrangement.iter().map(|p| (p.start, s.project.clips[&p.clip].length, s.project.clips[&p.clip].offset)).collect();
            v.sort();
            v
        };
        let three = take_of(t, "loop", 0, 288_000);
        let (n, errors) = s.commit_audio_takes(&[three], Some((0, bar)));
        assert_eq!((n, errors.len()), (1, 0));
        assert_eq!(spans(&s), [(0, bar, 2 * bar), (2 * bar, 4 * bar, 0)], "one bar, showing the third pass; what follows is untouched");
        s.undo();
        // Started in the middle of the loop (beat 2): the first pass is partial.
        // 2 + 4 + 4 + 1 beats = 11 beats = 5.5 s.
        let mid = take_of(t, "mid", 2 * PPQ, 264_000);
        s.commit_audio_takes(&[mid], Some((0, bar)));
        assert_eq!(spans(&s)[0], (0, bar, 2 * PPQ + bar), "the last whole pass starts after the partial one and one whole pass");
        assert_eq!(spans(&s).len(), 2);
        s.undo();
        // Began before the loop (an intro): from where it began to the loop end.
        let intro = take_of(t, "intro", 0, 288_000);
        s.commit_audio_takes(&[intro], Some((bar, 2 * bar)));
        assert_eq!(spans(&s)[0], (0, 2 * bar, 0));
        s.undo();
        // No loop, or a take that never reached the loop end: one plain clip.
        s.commit_audio_takes(&[take_of(t, "plain", PPQ, 48_000)], Some((0, bar)));
        assert_eq!(spans(&s)[0], (PPQ, 2 * PPQ, 0));
        s.undo();
        s.commit_audio_takes(&[take_of(t, "free", 0, 288_000)], None);
        assert_eq!(spans(&s)[0].1, 12 * PPQ);
    }

    #[test]
    fn deleting_tracks_is_one_undo_step_and_leaves_the_main_track() {
        let mut s = AppState::new(Project::new("t"));
        let (a, b) = (add_named(&mut s, TrackKind::Audio, "A"), add_named(&mut s, TrackKind::Instrument, "B"));
        let main = s.project.main_track().id;
        assert!(!s.delete_tracks(&[main]));
        assert!(s.delete_tracks(&[a, b, main]));
        assert!(s.project.track(a).is_none() && s.project.track(b).is_none() && s.project.track(main).is_some());
        s.undo();
        assert!(s.project.track(a).is_some() && s.project.track(b).is_some(), "one undo brings both back");
    }

    #[test]
    fn colours_change_the_track_and_the_clips_that_followed_it() {
        let mut s = AppState::new(Project::new("t"));
        let t = add_named(&mut s, TrackKind::Instrument, "K");
        let old = s.project.track(t).unwrap().color;
        let (src, c1, c2, p1, p2) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id(), s.project.alloc_placement_id());
        let hand = Color::palette(9);
        assert_ne!(hand, old);
        s.edit("clips", vec![
            Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes: vec![] })) },
            Command::AddClip { id: c1, clip: Box::new(Clip::new("one", old, src, PPQ)) },
            Command::AddClip { id: c2, clip: Box::new(Clip::new("two", hand, src, PPQ)) },
            Command::PlaceClip { track: t, placement: Placement { id: p1, clip: c1, start: 0, lane: 0 } },
            Command::PlaceClip { track: t, placement: Placement { id: p2, clip: c2, start: 4 * PPQ, lane: 0 } },
        ]);
        let new = Color::palette(5);
        assert!(s.set_tracks_color(&[t], new));
        assert_eq!(s.project.track(t).unwrap().color, new);
        assert_eq!(s.project.clips[&c1].color, new, "followed the track");
        assert_eq!(s.project.clips[&c2].color, hand, "coloured by hand: kept");
        assert!(!s.set_tracks_color(&[t], new), "same colour: no step");
        s.undo();
        assert_eq!((s.project.track(t).unwrap().color, s.project.clips[&c1].color), (old, old));
        // The selected clips take a colour of their own.
        s.select_clips(vec![p2], Some(p2));
        assert!(s.set_clips_color(Color::palette(1)));
        assert_eq!(s.project.clips[&c2].color, Color::palette(1));
    }

    #[test]
    fn colour_actions_are_one_undo_step_and_the_colour_options_apply() {
        let mut s = AppState::new(Project::new("t"));
        let t = add_named(&mut s, TrackKind::Instrument, "K");
        let color = s.project.track(t).unwrap().color;
        let (src, c1, p1) = (s.project.alloc_source_id(), s.project.alloc_clip_id(), s.project.alloc_placement_id());
        let hand = Color::grid(2, 4);
        s.edit("clips", vec![
            Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource { notes: vec![] })) },
            Command::AddClip { id: c1, clip: Box::new(Clip::new("one", hand, src, PPQ)) },
            Command::PlaceClip { track: t, placement: Placement { id: p1, clip: c1, start: 0, lane: 0 } },
        ]);
        s.seal();
        // Assign Track Color to Clips.
        assert!(s.assign_track_color_to_clips(&[t]));
        assert_eq!(s.project.clips[&c1].color, color);
        assert!(!s.assign_track_color_to_clips(&[t]), "already the track's colour: no step");
        assert!(s.undo());
        assert_eq!(s.project.clips[&c1].color, hand, "one undo step");
        // Randomize Colors: tracks and the clips that followed them, in one step.
        assert!(s.randomize_track_colors(&[t], 99));
        let randomised = s.project.track(t).unwrap().color;
        assert_ne!(randomised, color);
        s.select_clips(vec![p1], Some(p1));
        assert!(s.randomize_clip_colors(5));
        assert!(s.undo() && s.undo());
        assert_eq!(s.project.track(t).unwrap().color, color);
        // Colour options: a neutral default instead of the next palette colour; clips keep their own colour.
        s.color_prefs = ColorPrefs { auto_track: false, clips_take_track: false };
        let u = s.add_track(TrackKind::Audio).unwrap();
        assert_eq!(s.project.track(u).unwrap().color, ColorPrefs::DEFAULT_TRACK);
        assert_ne!(s.clip_color_for(Color(0x123456)), Color(0x123456));
        s.color_prefs = ColorPrefs::default();
        assert_eq!(s.clip_color_for(Color(0x123456)), Color(0x123456));
    }

    #[test]
    fn midi_effects_are_put_before_the_other_devices_in_the_order_they_were_added() {
        let mut s = state();
        let track = s.add_track(TrackKind::Instrument).unwrap();
        for (id, name) in [("fx.eq", "EQ"), ("midi.arp", "Arp"), ("fx.reverb", "Reverb"), ("midi.chord", "Chord")] {
            s.add_stock_device(track, id, name).unwrap();
        }
        let chain: Vec<String> = s.project.track(track).unwrap().devices.iter().filter_map(|d| match &d.kind {
            DeviceKind::Stock { plugin_id } => Some(plugin_id.clone()),
            _ => None,
        }).collect();
        let midi: Vec<&String> = chain.iter().filter(|id| id.starts_with("midi.")).collect();
        assert_eq!(midi, ["midi.arp", "midi.chord"]);
        let last_midi = chain.iter().rposition(|id| id.starts_with("midi.")).unwrap();
        assert!(chain[..=last_midi].iter().all(|id| id.starts_with("midi.")), "{chain:?}");
    }

    #[test]
    fn warp_edits_clamp_and_a_drag_is_one_undo_step() {
        let mut s = state();
        let clip = *s.project.clips.iter().find(|(_, c)| matches!(s.project.sources.get(&c.source), Some(Source::Audio(_)))).unwrap().0;
        let warp = |s: &AppState| match s.project.sources.get(&s.project.clips[&clip].source) {
            Some(Source::Audio(a)) => a.warp.clone(),
            _ => unreachable!(),
        };
        let before = warp(&s);
        s.seal();
        for st in [3., 9., 60.] {
            s.set_clip_warp("transpose-drag", clip, |w| w.transpose = st);
        }
        s.seal();
        assert_eq!(warp(&s).transpose, 48.);
        assert!(s.set_clip_warp("detune", clip, |w| w.detune = -80.4));
        assert_eq!(warp(&s).detune, -50.);
        assert!(!s.set_clip_warp("detune", clip, |w| w.detune = -50.), "no change, no undo step");
        s.seal();
        s.undo();
        s.undo();
        assert_eq!(warp(&s), before);
    }

    #[test]
    fn sample_edits_follow_the_clip_selection_and_split_shared_samples() {
        let mut s = state();
        let (&src, _) = s.project.sources.iter().find(|(_, x)| matches!(x, Source::Audio(_))).unwrap();
        let track = s.add_track(TrackKind::Audio).unwrap();
        // Three clips on one sample, placed side by side.
        let mut ids = Vec::new();
        for i in 0..3 {
            let clip = s.project.alloc_clip_id();
            let pl = s.project.alloc_placement_id();
            s.edit("add", vec![
                Command::AddClip { id: clip, clip: Box::new(Clip::new("c", Color(0), src, PPQ)) },
                Command::PlaceClip { track, placement: Placement { id: pl, clip, start: i * 4 * PPQ, lane: 0 } },
            ]);
            ids.push((clip, pl));
        }
        let source_of = |s: &AppState, c: ClipId| s.project.clips[&c].source;
        let warp_of = |s: &AppState, c: ClipId| match &s.project.sources[&s.project.clips[&c].source] {
            Source::Audio(a) => a.warp.clone(),
            _ => unreachable!(),
        };
        // The first two selected: they get their own sample, the third keeps the old one.
        s.click_clip(ids[0].1, false);
        s.click_clip(ids[1].1, true);
        assert_eq!(s.warp_targets(ids[0].0), vec![ids[0].0, ids[1].0]);
        s.seal();
        assert!(s.set_clip_warp("t", ids[0].0, |w| w.transpose = 5.));
        assert!(s.set_clip_warp("t", ids[0].0, |w| w.transpose = 7.), "a drag goes on in the copy");
        s.seal();
        assert_eq!(source_of(&s, ids[0].0), source_of(&s, ids[1].0));
        assert_ne!(source_of(&s, ids[0].0), src);
        assert_eq!(source_of(&s, ids[2].0), src);
        assert_eq!((warp_of(&s, ids[0].0).transpose, warp_of(&s, ids[2].0).transpose), (7., 0.));
        // A clip outside the selection is edited alone.
        assert_eq!(s.warp_targets(ids[2].0), vec![ids[2].0]);
        // One undo step brings the shared sample back.
        s.undo();
        assert_eq!(source_of(&s, ids[0].0), src);
    }

    #[test]
    fn all_clips_of_a_sample_follow_a_cmd_drag_and_the_menu_item() {
        let mut s = state();
        let (&src, _) = s.project.sources.iter().find(|(_, x)| matches!(x, Source::Audio(_))).unwrap();
        let track = s.add_track(TrackKind::Audio).unwrap();
        let mut ids = Vec::new();
        for i in 0..3 {
            let clip = s.project.alloc_clip_id();
            let pl = s.project.alloc_placement_id();
            s.edit("add", vec![
                Command::AddClip { id: clip, clip: Box::new(Clip::new("c", Color(0), src, PPQ)) },
                Command::PlaceClip { track, placement: Placement { id: pl, clip, start: i * 4 * PPQ, lane: 0 } },
            ]);
            ids.push((clip, pl));
        }
        let warp_of = |s: &AppState, c: ClipId| match &s.project.sources[&s.project.clips[&c].source] {
            Source::Audio(a) => a.warp.clone(),
            _ => unreachable!(),
        };
        // The first clip alone transposed: it gets a copy of the file, the others keep theirs.
        s.click_clip(ids[0].1, false);
        s.seal();
        assert!(s.set_clip_warp("t", ids[0].0, |w| w.transpose = 5.));
        s.seal();
        assert_eq!((warp_of(&s, ids[0].0).transpose, warp_of(&s, ids[1].0).transpose), (5., 0.));
        // Copies or not, they are all clips of the same sample.
        let all = s.sample_targets(ids[1].0);
        assert_eq!(all[0], ids[1].0);
        for (c, _) in &ids {
            assert!(all.contains(c));
        }
        // Cmd + drag on the Transpose knob: every clip of the sample, selected or not.
        s.seal();
        assert!(s.set_sample_warp("transpose-x", ids[0].0, |w| w.transpose = -3.));
        assert!(s.set_sample_warp("transpose-x", ids[0].0, |w| w.transpose = -7.));
        s.seal();
        assert!(ids.iter().all(|(c, _)| warp_of(&s, *c).transpose == -7.));
        assert!(s.undo(), "one drag, one step");
        assert_eq!((warp_of(&s, ids[0].0).transpose, warp_of(&s, ids[2].0).transpose), (5., 0.));
        // "Apply to all clips of this sample": the first clip's pitch on every one, stretch kept.
        let stretched = s.project.clips[&ids[2].0].source;
        let warp = Warp { enabled: true, original_bpm: Some(60.), ..warp_of(&s, ids[2].0) };
        s.edit("stretch", vec![Command::SetWarp { source: stretched, warp: Box::new(warp) }]);
        assert!(s.set_clip_warp("d", ids[0].0, |w| w.detune = 20.));
        assert!(s.match_sample_pitch(ids[0].0));
        assert!(ids.iter().all(|(c, _)| { let w = warp_of(&s, *c); (w.transpose, w.detune) == (5., 20.) }));
        assert_eq!(warp_of(&s, ids[2].0).original_bpm, Some(60.), "each clip keeps its own stretch");
        assert!(!s.match_sample_pitch(ids[0].0), "already the same");
        // A notes clip has no sample.
        assert!(s.sample_targets(lead_clip(&s)).is_empty());
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
    fn matching_keys_are_the_same_key_or_its_relative() {
        use crate::workspace::keys_match;
        let key = |root: u8, kind: ScaleKind| Scale::new(Key(root), kind);
        let a_minor = key(9, ScaleKind::Minor);
        assert!(keys_match(a_minor, a_minor));
        assert!(keys_match(key(0, ScaleKind::Major), a_minor), "C major is the relative of A minor");
        assert!(!keys_match(key(9, ScaleKind::Major), a_minor), "A major is not");
        assert!(!keys_match(key(4, ScaleKind::Minor), a_minor), "a fifth away is close, but not the same key");
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
        let original = s.project.clone();
        s.selected_track = Some(track);
        s.selected_placement = None;
        let before = s.project.track(track).unwrap().arrangement.len();
        assert_eq!(s.split_at(place.start), 0, "the clip's own edge is not a cut");
        let cut = place.start + BAR;
        assert_eq!(s.split_at(cut + 7), 1, "the cut snaps to the grid");
        let t = s.project.track(track).unwrap();
        assert_eq!(t.arrangement.len(), before + 1);
        // The part before the cut keeps its placement.
        let left_clip = t.arrangement.iter().find(|p| p.id == place.id).expect("the placement stays").clip;
        let left = &s.project.clips[&left_clip];
        let right_place = t.arrangement.iter().find(|p| p.start == cut).expect("a clip starts at the cut");
        let right = &s.project.clips[&right_place.clip];
        assert_eq!(left.length, BAR);
        // The right half plays on from the cut, with notes of its own from its tick 0.
        assert_eq!(right.length, clip.length - BAR);
        assert_ne!(right.source, clip.source);
        let heard = |c: &Clip, src: &Project| {
            let Some(Source::Notes(n)) = src.sources.get(&c.source) else { panic!("a notes clip") };
            aitrack_model::ops::heard_notes(c, &n.notes).into_iter().map(|n| (n.start, n.key, n.length)).collect::<Vec<_>>()
        };
        let whole: Vec<_> = heard(&clip, &original).into_iter().filter(|(at, ..)| *at >= BAR).map(|(at, k, len)| (at - BAR, k, len)).collect();
        assert_eq!(heard(right, &s.project), whole);
        assert_eq!((s.selected_placement, s.selected_clip), (Some((track, place.id)), Some(left_clip)), "the left half is selected, in the lane and in the Clip tab");
        assert_eq!(s.selected_clip_ids(), [place.id].into());
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
        let group = s.group_tracks().unwrap();
        let b = s.project.track(bass).unwrap();
        assert_eq!((b.parent, b.output), (Some(group), Output::Track(group)));
        assert_eq!(s.project.track_index(group).map(|i| i + 1), s.project.track_index(bass), "the group sits right above");
        assert!(s.ungroup_tracks());
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

    #[test]
    fn scale_fold_rows_and_track_scale_resolution() {
        let mut s = state();
        let clip = lead_clip(&s);
        let track = s.clip_track(clip).unwrap();
        let track_scale = Scale::new(Key(0), ScaleKind::Major);
        assert!(s.set_track_scale(track, Some(track_scale)));
        assert_eq!(s.clip_scale(Some(clip)), Some(track_scale));
        let rows = s.piano_rows(Some(clip));
        assert!(rows.len() < 128);
        assert!(rows.iter().all(|k| track_scale.contains(*k)));
        s.piano_scale_fold = false;
        assert_eq!(s.piano_rows(Some(clip)).len(), 128);
    }

    #[test]
    fn scale_chord_arp_strum_chop_and_invert() {
        let mut s = state();
        let clip = lead_clip(&s);
        let source = s.clip_notes(clip).unwrap().0;
        let ids: Vec<_> = s.clip_notes(clip).unwrap().1.notes.iter().map(|n| n.id).collect();
        s.edit("Clear", vec![Command::RemoveNotes { source, ids }]);
        s.set_scale(Some(Scale::new(Key(0), ScaleKind::Major))); // C Major
        let root_id = s.draw_note(clip, 60, 0).unwrap();
        s.selected_notes = [root_id].into();
        // Triad from C4 in C Major -> C4 (60), E4 (64), G4 (67)
        assert_eq!(s.build_chord(clip, ChordKind::Triad), 2);
        let mut keys: Vec<u8> = s.clip_notes(clip).unwrap().1.notes.iter().map(|n| n.key).collect();
        keys.sort();
        assert_eq!(keys, vec![60, 64, 67]);
        // Diatonic transposition up 1 scale step -> D4 (62), F4 (65), A4 (69)
        let originals = s.clip_notes(clip).unwrap().1.notes.clone();
        s.move_notes_scale(clip, &originals, 0, 1, true);
        s.seal();
        let mut moved: Vec<u8> = s.clip_notes(clip).unwrap().1.notes.iter().map(|n| n.key).collect();
        moved.sort();
        assert_eq!(moved, vec![62, 65, 69]);
        // Invert up moves D4 (62) -> D5 (74)
        assert_eq!(s.invert_chords(clip, 1), 1);
        let mut inverted: Vec<u8> = s.clip_notes(clip).unwrap().1.notes.iter().map(|n| n.key).collect();
        inverted.sort();
        assert_eq!(inverted, vec![65, 69, 74]);
        // Strum shifts voices slightly
        assert!(s.strum(clip, false) > 0);
        // Arpeggiate replaces chord with sequential steps
        let count = s.arpeggiate(clip, ArpPattern::Up);
        assert!(count >= 2);
        // Chop splits long note at snap grid
        let note_id = s.draw_note(clip, 60, PPQ * 4).unwrap();
        s.selected_notes = [note_id].into();
        s.resize_notes(clip, &[s.clip_notes(clip).unwrap().1.notes.iter().find(|n| n.id == note_id).unwrap().clone()], PPQ * 2);
        s.seal();
        assert!(s.chop(clip) >= 2);
    }

    #[test]
    fn rack_macro_variations_save_recall_delete() {
        let mut s = state();
        let track = s.selected_track.unwrap();
        let rack = s.add_stock_device(track, "rack.instrument", "Inst Rack").unwrap();
        assert!(s.set_macro(rack, 0, 0.25));
        assert!(s.set_macro(rack, 3, 0.8));
        s.seal();
        assert!(s.save_rack_variation(rack, Some("Warm".into())));

        assert!(s.set_macro(rack, 0, 0.9));
        assert!(s.set_macro(rack, 3, 0.1));
        s.seal();
        assert!(s.save_rack_variation(rack, None));

        assert!(s.recall_variation_except(rack, 0, &Default::default()));
        let DeviceKind::Rack(r) = &s.project.device(rack).unwrap().kind else { panic!("expected rack") };
        assert!((r.macros[0].value - 0.25).abs() < 1e-6);
        assert!((r.macros[3].value - 0.8).abs() < 1e-6);
        assert_eq!(r.variations.len(), 2);
        assert_eq!(r.variations[1].name, "Var 2");

        assert!(s.delete_rack_variation(rack, 0));
        let DeviceKind::Rack(r) = &s.project.device(rack).unwrap().kind else { panic!("expected rack") };
        assert_eq!(r.variations.len(), 1);
        assert_eq!(r.variations[0].name, "Var 2");
    }
}
