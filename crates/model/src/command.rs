//! The Command API: every edit to a [`Project`] is a serialisable
//! [`Command`]. Applying a command returns its exact inverse, which is what
//! undo stores. Commands carry ids allocated up front (`Project::alloc_*`),
//! so redo replays the same ids and later commands keep pointing at the
//! right objects.
//!
//! The same enum is the wire format for scripts and AI agents (via MCP), so
//! variants are explicit and serialise as tagged JSON.

use crate::groove::{Groove, GrooveSettings, PoolGroove, MAX_GROOVE_AMOUNT};
use crate::ids::*;
use crate::project::*;
use crate::routing;
use crate::takes;
use crate::scale::Scale;
use crate::time::{Tick, TempoMap};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CommandError {
    #[error("track {0} not found")]
    TrackNotFound(TrackId),
    #[error("clip {0} not found")]
    ClipNotFound(ClipId),
    #[error("source {0} not found")]
    SourceNotFound(SourceId),
    #[error("source {0} is not a note source")]
    NotNotes(SourceId),
    #[error("source {0} is not an audio source")]
    NotAudio(SourceId),
    #[error("device {0} not found")]
    DeviceNotFound(DeviceId),
    #[error("device {0} is not a rack")]
    NotARack(DeviceId),
    #[error("placement {0} not found")]
    PlacementNotFound(PlacementId),
    #[error("scene {0} not found")]
    SceneNotFound(SceneId),
    #[error("id already in use: {0}")]
    DuplicateId(String),
    #[error("the Main track cannot be removed or moved")]
    MainTrack,
    #[error("index {0} out of range")]
    OutOfRange(usize),
    /// Routing `from` to `to` would feed `from` with its own signal.
    #[error("routing {from} to {to} would make a loop")]
    RoutingCycle { from: TrackId, to: TrackId },
    /// The command would edit the clips or devices of a frozen track.
    #[error("the track is frozen: unfreeze it to edit its clips and devices")]
    Frozen(TrackId),
    #[error("{0}")]
    Invalid(String),
}

/// A single track property change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "prop", content = "value")]
pub enum TrackProp {
    Name(String),
    Color(Color),
    VolumeDb(f32),
    Pan(f32),
    Mute(bool),
    Solo(bool),
    Arm(bool),
    Monitor(Monitor),
    Input(InputSource),
    Output(Output),
    Sends(Vec<Send>),
    Parent(Option<TrackId>),
    Vca(Option<TrackId>),
    Collapsed(bool),
    Height(f32),
    Scale(Option<Scale>),
    KeyboardInput(bool),
}

/// A single rack-chain property change (see `Command::SetChainProp`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "prop", content = "value")]
pub enum ChainProp {
    Name(String),
    Color(Color),
    /// -150..=12 dB.
    VolumeDb(f32),
    /// -1..=1.
    Pan(f32),
    Mute(bool),
    Solo(bool),
    /// Zones: (low, high) 0..=127, put in order when set.
    KeyRange((u8, u8)),
    VelRange((u8, u8)),
    SelRange((u8, u8)),
    /// Fade widths at the (low, high) end of each zone, 0..=127.
    KeyFade((u8, u8)),
    VelFade((u8, u8)),
    SelFade((u8, u8)),
    /// Drum Rack choke group 0..=16 (0 = none).
    Choke(u8),
    /// Drum Rack: the note that hits the pad / the note its devices get.
    ReceiveNote(Option<u8>),
    PlayNote(Option<u8>),
    /// All send levels (dB) at once; at most one per return of the rack.
    Sends(Vec<f32>),
    /// The send into return `index`, in dB ([`SEND_OFF_DB`]..=[`SEND_MAX_DB`]).
    Send(usize, f32),
    Output(ChainOut),
}

/// Where a device is inserted: a track's chain or a chain inside a rack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "in")]
pub enum DeviceSlot {
    Track { track: TrackId, index: usize },
    Rack { rack: DeviceId, chain: usize, index: usize },
    /// A device of a Drum Rack's internal return `index`, at position `pos`.
    Return { rack: DeviceId, index: usize, pos: usize },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum Command {
    RenameProject { name: String },
    SetTempo { bpm: f64 },
    SetTempoMap { map: TempoMap },
    SetScale { scale: Option<Scale> },
    SetLoop { region: Option<(Tick, Tick)> },

    AddTrack { track: Box<Track>, index: usize },
    RemoveTrack { id: TrackId },
    MoveTrack { id: TrackId, to: usize },
    SetTrack { id: TrackId, change: TrackProp },

    AddScene { scene: Scene, index: usize },
    RemoveScene { id: SceneId },
    RenameScene { id: SceneId, name: String },

    AddSource { id: SourceId, source: Box<Source> },
    RemoveSource { id: SourceId },
    /// Point an audio source at another file (relinking a lost one, collecting
    /// into the project folder). Every clip playing the source follows.
    SetSourceFile { source: SourceId, path: std::path::PathBuf, hash: Option<String> },
    AddClip { id: ClipId, clip: Box<Clip> },
    RemoveClip { id: ClipId },
    UpdateClip { id: ClipId, clip: Box<Clip> },

    PlaceClip { track: TrackId, placement: Placement },
    RemovePlacement { track: TrackId, id: PlacementId },
    MovePlacement { track: TrackId, id: PlacementId, to_track: TrackId, start: Tick, lane: u16 },
    SetSessionSlot { track: TrackId, scene: SceneId, clip: Option<ClipId> },

    /// Add a take lane to a track at `index` among its take lanes (comping,
    /// see [`crate::takes`]). The placements of the lanes after it keep their lane.
    AddTakeLane { track: TrackId, index: usize, name: String },
    /// Delete take lane `index` with the takes on it; undo brings them back.
    RemoveTakeLane { track: TrackId, index: usize },
    RenameTakeLane { track: TrackId, index: usize, name: String },
    /// Move take lane `from` to position `to` (the takes follow it).
    MoveTakeLane { track: TrackId, from: usize, to: usize },
    /// Replace which stretches of the main lane were promoted from which take
    /// lane (what the arrangement highlights).
    SetComp { track: TrackId, spans: Vec<CompSpan> },

    AddNotes { source: SourceId, notes: Vec<Note> },
    RemoveNotes { source: SourceId, ids: Vec<NoteId> },
    /// Replace notes with the same ids (move, resize, velocity…).
    UpdateNotes { source: SourceId, notes: Vec<Note> },
    /// Replace an audio source's warp settings (transpose, detune, reverse,
    /// warp on/off and mode). Every clip playing the source follows.
    SetWarp { source: SourceId, warp: Box<Warp> },

    AddDevice { slot: DeviceSlot, device: Box<Device> },
    RemoveDevice { id: DeviceId },
    MoveDevice { id: DeviceId, to: DeviceSlot },
    SetParam { device: DeviceId, param: u32, value: Option<f32> },
    SetDeviceEnabled { device: DeviceId, enabled: bool },
    /// Connect a device's sidechain input to a track (or disconnect it).
    SetSidechain { device: DeviceId, source: Option<TrackId> },
    SetDeviceState { device: DeviceId, state: Option<Vec<u8>>, preset: Option<String> },
    RenameDevice { device: DeviceId, name: String },

    AddChain { rack: DeviceId, index: usize, chain: Box<Chain> },
    RemoveChain { rack: DeviceId, index: usize },
    UpdateChain { rack: DeviceId, index: usize, chain: Box<Chain> },
    /// Change one property of chain `chain` of a rack; the granular edit for
    /// zones, faders, choke groups and sends (coalesces like `SetParam`).
    SetChainProp { rack: DeviceId, chain: usize, prop: ChainProp },
    /// The same for return `index` of a Drum Rack.
    SetReturnProp { rack: DeviceId, index: usize, prop: ChainProp },
    /// Add an internal return bus to a Drum Rack (at most [`MAX_RETURNS`]).
    /// Chains' sends and outputs at or after `index` move up to match.
    AddReturn { rack: DeviceId, index: usize, chain: Box<Chain> },
    /// Remove return `index`: the chains lose their send to it, and the ones
    /// routed into it go back to the rack output.
    RemoveReturn { rack: DeviceId, index: usize },
    /// Map a parameter to macro `macro_index` of a rack, at `index` in its
    /// mappings. The device must be inside the rack.
    AddMapping { rack: DeviceId, macro_index: usize, index: usize, mapping: MacroMapping },
    RemoveMapping { rack: DeviceId, macro_index: usize, index: usize },
    /// Change a mapping's range; `min > max` inverts it.
    SetMappingRange { rack: DeviceId, macro_index: usize, index: usize, min: f32, max: f32 },
    /// Map a modulator (`mod.*` device) to a target on its own track, at
    /// `index` in its mappings (at most [`MAX_MOD_MAPPINGS`], one per target).
    AddModMapping { device: DeviceId, index: usize, mapping: ModMapping },
    RemoveModMapping { device: DeviceId, index: usize },
    /// Change the offset range of a modulator mapping (`-1..=1`; `min > max` inverts).
    SetModRange { device: DeviceId, index: usize, min: f32, max: f32 },
    /// Set a rack's chain selector (0..=127, `Rack::chain_selector`).
    SetRackSelector { rack: DeviceId, value: u8 },
    SetMacro { rack: DeviceId, index: usize, value: f32 },
    SetMacroDef { rack: DeviceId, index: usize, name: String, mappings: Vec<MacroMapping> },
    SetVariations { rack: DeviceId, variations: Vec<Variation> },

    /// Set (or with `None` remove) the automation lane for `target`.
    SetAutomation { track: TrackId, target: AutomationTarget, lane: Option<AutomationLane> },

    /// Put a groove in the project's pool at `index` (ids come from
    /// `Project::alloc_groove_id`). The groove is brought into range.
    AddGroove { entry: Box<PoolGroove>, index: usize },
    /// Take a groove out of the pool; clips that used it go back to none.
    RemoveGroove { id: GrooveId },
    /// Replace a pool groove's template (rename, new base, edited cells).
    UpdateGroove { id: GrooveId, groove: Box<Groove> },
    /// Change how strongly a pool groove acts (Quantize, Timing, Random,
    /// Velocity); coalesces like a drag.
    SetGrooveSettings { id: GrooveId, settings: GrooveSettings },
    /// Point a notes clip at a pool groove (`None`: play the notes as they are).
    SetClipGroove { clip: ClipId, groove: Option<GrooveId> },
    /// The global groove amount, 0..=1.3 (Live's Global Groove Amount).
    SetGrooveAmount { amount: f32 },

    /// Freeze an Instrument or Audio track: from now on the engine plays
    /// `file` (the track's clips and device chain rendered offline from tick
    /// 0) and runs none of its devices. Mixer and mixer automation stay live.
    FreezeTrack { track: TrackId, file: AudioSource },
    /// Drop the frozen render; the track plays live again.
    UnfreezeTrack { track: TrackId },
    /// Turn a frozen track into a plain Audio track that holds the render as
    /// one clip at tick 0: devices, device automation and the old clips go.
    /// Ids come from `Project::alloc_*`.
    FlattenTrack { track: TrackId, source: SourceId, clip: ClipId, placement: PlacementId },
    /// The inverse of `FlattenTrack`: put the saved track back and drop the
    /// source and clip the flatten made.
    RestoreFlattened { track: Box<Track>, source: SourceId, clip: ClipId },

    Batch { commands: Vec<Command> },
}

type R = Result<Command, CommandError>;

impl Command {
    /// Short human label for undo menus and agent proposals.
    pub fn label(&self) -> &'static str {
        use Command::*;
        match self {
            RenameProject { .. } => "Rename project",
            SetTempo { .. } | SetTempoMap { .. } => "Change tempo",
            SetScale { .. } => "Change scale",
            SetLoop { .. } => "Set loop",
            AddTrack { .. } => "Add track",
            RemoveTrack { .. } => "Delete track",
            MoveTrack { .. } => "Move track",
            SetTrack { change, .. } => match change {
                TrackProp::Name(_) => "Rename track",
                TrackProp::Color(_) => "Change track color",
                TrackProp::VolumeDb(_) => "Change volume",
                TrackProp::Pan(_) => "Change pan",
                TrackProp::Mute(_) => "Mute",
                TrackProp::Solo(_) => "Solo",
                TrackProp::Sends(_) => "Change sends",
                TrackProp::Input(_) => "Change input",
                TrackProp::Monitor(_) => "Change monitor",
                TrackProp::Output(_) => "Change routing",
                TrackProp::Scale(_) => "Change track key",
                TrackProp::KeyboardInput(_) => "Toggle computer keyboard",
                _ => "Change track",
            },
            AddScene { .. } => "Add scene",
            RemoveScene { .. } => "Delete scene",
            RenameScene { .. } => "Rename scene",
            AddSource { .. } | AddClip { .. } => "Add clip",
            RemoveSource { .. } | RemoveClip { .. } => "Delete clip",
            SetSourceFile { .. } => "Relink file",
            UpdateClip { .. } => "Edit clip",
            AddGroove { .. } => "Add groove",
            RemoveGroove { .. } => "Delete groove",
            UpdateGroove { .. } => "Edit groove",
            SetGrooveSettings { .. } => "Change groove",
            SetClipGroove { .. } => "Set clip groove",
            SetGrooveAmount { .. } => "Change groove amount",
            SetWarp { .. } => "Edit sample",
            PlaceClip { .. } => "Place clip",
            RemovePlacement { .. } => "Delete clip",
            MovePlacement { .. } => "Move clip",
            SetSessionSlot { .. } => "Set clip slot",
            AddTakeLane { .. } => "Add take lane",
            RemoveTakeLane { .. } => "Delete take lane",
            RenameTakeLane { .. } => "Rename take lane",
            MoveTakeLane { .. } => "Move take lane",
            SetComp { .. } => "Comp",
            AddNotes { .. } => "Add notes",
            RemoveNotes { .. } => "Delete notes",
            UpdateNotes { .. } => "Edit notes",
            AddDevice { .. } => "Add device",
            RemoveDevice { .. } => "Delete device",
            MoveDevice { .. } => "Move device",
            SetParam { .. } => "Change parameter",
            SetDeviceEnabled { .. } => "Toggle device",
            SetSidechain { .. } => "Route sidechain",
            SetDeviceState { .. } => "Load preset",
            RenameDevice { .. } => "Rename device",
            AddChain { .. } => "Add chain",
            RemoveChain { .. } => "Delete chain",
            UpdateChain { .. } => "Edit chain",
            SetChainProp { .. } | SetReturnProp { .. } => "Edit chain",
            AddReturn { .. } => "Add return",
            RemoveReturn { .. } => "Delete return",
            AddMapping { .. } | SetMappingRange { .. } => "Map macro",
            RemoveMapping { .. } => "Unmap macro",
            AddModMapping { .. } | SetModRange { .. } => "Map modulator",
            RemoveModMapping { .. } => "Unmap modulator",
            SetRackSelector { .. } => "Chain selector",
            SetMacro { .. } => "Change macro",
            SetMacroDef { .. } => "Map macro",
            SetVariations { .. } => "Store variation",
            SetAutomation { .. } => "Edit automation",
            FreezeTrack { .. } => "Freeze track",
            UnfreezeTrack { .. } => "Unfreeze track",
            FlattenTrack { .. } => "Flatten track",
            RestoreFlattened { .. } => "Undo flatten",
            Batch { .. } => "Edit",
        }
    }

    /// True for commands that delete user content (agents must ask first).
    pub fn is_destructive(&self) -> bool {
        use Command::*;
        match self {
            RemoveTrack { .. } | RemoveScene { .. } | RemoveSource { .. } | RemoveClip { .. }
            | RemovePlacement { .. } | RemoveTakeLane { .. } | RemoveNotes { .. } | RemoveGroove { .. } | RemoveDevice { .. } | RemoveChain { .. } | RemoveReturn { .. } | RemoveMapping { .. } | RemoveModMapping { .. } => true,
            Batch { commands } => commands.iter().any(Command::is_destructive),
            _ => false,
        }
    }

    /// The frozen track whose clips, notes, devices or arming this command
    /// would edit, if any: a frozen track plays its render, so those edits
    /// would do nothing until it is unfrozen (the mixer stays editable).
    /// `History` refuses such commands.
    pub fn frozen_target(&self, p: &Project) -> Option<TrackId> {
        use Command::*;
        if !p.tracks.iter().any(Track::is_frozen) && !matches!(self, Batch { .. }) {
            return None;
        }
        let frozen = |t: TrackId| p.track(t).is_some_and(Track::is_frozen).then_some(t);
        let device = |d: DeviceId| p.device_track(d).and_then(frozen);
        // The frozen track playing a clip / a source.
        let clip_track = |c: ClipId| {
            p.tracks.iter().filter(|t| t.is_frozen()).find(|t| t.arrangement.iter().any(|pl| pl.clip == c) || t.session_slots.values().any(|&s| s == c)).map(|t| t.id)
        };
        let source_track = |s: SourceId| p.clips.iter().filter(|(_, c)| c.source == s).find_map(|(&id, _)| clip_track(id));
        match self {
            PlaceClip { track, .. } | RemovePlacement { track, .. } | SetSessionSlot { track, .. } => frozen(*track),
            MovePlacement { track, to_track, .. } => frozen(*track).or_else(|| frozen(*to_track)),
            UpdateClip { id, .. } => clip_track(*id),
            SetClipGroove { clip, .. } => clip_track(*clip),
            AddNotes { source, .. } | RemoveNotes { source, .. } | UpdateNotes { source, .. } | SetWarp { source, .. } => source_track(*source),
            AddDevice { slot, .. } | MoveDevice { to: slot, .. } => match slot {
                DeviceSlot::Track { track, .. } => frozen(*track),
                DeviceSlot::Rack { rack, .. } | DeviceSlot::Return { rack, .. } => device(*rack),
            },
            RemoveDevice { id } => device(*id),
            AddModMapping { device: d, .. } | RemoveModMapping { device: d, .. } | SetModRange { device: d, .. } => device(*d),
            SetParam { device: d, .. } | SetDeviceEnabled { device: d, .. } | SetSidechain { device: d, .. } | SetDeviceState { device: d, .. } | RenameDevice { device: d, .. } => device(*d),
            AddChain { rack, .. } | RemoveChain { rack, .. } | UpdateChain { rack, .. } | SetChainProp { rack, .. } | SetReturnProp { rack, .. } | AddReturn { rack, .. } | RemoveReturn { rack, .. }
            | AddMapping { rack, .. } | RemoveMapping { rack, .. } | SetMappingRange { rack, .. } | SetRackSelector { rack, .. } | SetMacro { rack, .. } | SetMacroDef { rack, .. }
            | SetVariations { rack, .. } => device(*rack),
            SetAutomation { track, target, .. } => match target {
                AutomationTarget::DeviceParam { .. } | AutomationTarget::DeviceEnabled { .. } | AutomationTarget::Macro { .. } => frozen(*track),
                _ => None,
            },
            SetTrack { id, change: TrackProp::Arm(true) | TrackProp::KeyboardInput(true) } => frozen(*id),
            Batch { commands } => {
                // A batch that changes the freeze state itself is checked as it applies:
                // `[Unfreeze, SetParam]` edits an unfrozen track.
                if commands.iter().any(|c| matches!(c, FreezeTrack { .. } | UnfreezeTrack { .. } | FlattenTrack { .. } | RestoreFlattened { .. })) {
                    let mut sim = p.clone();
                    for c in commands {
                        if let Some(t) = c.frozen_target(&sim) {
                            return Some(t);
                        }
                        if c.clone().apply(&mut sim).is_err() {
                            break;
                        }
                    }
                    return None;
                }
                commands.iter().find_map(|c| c.frozen_target(p))
            }
            _ => None,
        }
    }

    /// Apply to `p` and return the inverse command.
    pub fn apply(self, p: &mut Project) -> R {
        use Command::*;
        match self {
            RenameProject { name } => Ok(RenameProject { name: std::mem::replace(&mut p.name, name) }),
            SetTempo { bpm } => {
                let old = p.tempo.initial_bpm();
                p.tempo.set_initial_bpm(bpm);
                Ok(SetTempo { bpm: old })
            }
            SetTempoMap { map } => Ok(SetTempoMap { map: std::mem::replace(&mut p.tempo, map) }),
            SetScale { scale } => Ok(SetScale { scale: std::mem::replace(&mut p.scale, scale) }),
            SetLoop { region } => {
                let region = region.map(|(a, b)| (a.min(b), a.max(b)));
                Ok(SetLoop { region: std::mem::replace(&mut p.loop_region, region) })
            }

            AddTrack { track, index } => {
                if p.track(track.id).is_some() {
                    return Err(CommandError::DuplicateId(track.id.to_string()));
                }
                if track.kind == TrackKind::Main {
                    return Err(CommandError::Invalid("only one Main track".into()));
                }
                observe_track(p, &track);
                let id = track.id;
                let index = index.min(p.tracks.len());
                p.tracks.insert(index, *track);
                Ok(RemoveTrack { id })
            }
            RemoveTrack { id } => {
                let index = p.track_index(id).ok_or(CommandError::TrackNotFound(id))?;
                if p.tracks[index].kind == TrackKind::Main {
                    return Err(CommandError::MainTrack);
                }
                let track = p.tracks.remove(index);
                let mut restores = unlink_track(p, &track);
                restores.insert(0, AddTrack { track: Box::new(track), index });
                Ok(Batch { commands: restores })
            }
            MoveTrack { id, to } => {
                let from = p.track_index(id).ok_or(CommandError::TrackNotFound(id))?;
                if p.tracks[from].kind == TrackKind::Main {
                    return Err(CommandError::MainTrack);
                }
                let t = p.tracks.remove(from);
                p.tracks.insert(to.min(p.tracks.len()), t);
                Ok(MoveTrack { id, to: from })
            }
            SetTrack { id, change } => {
                match &change {
                    TrackProp::Output(output) => routing::check_output(p, id, *output)?,
                    TrackProp::Sends(sends) => routing::check_sends(p, id, sends)?,
                    TrackProp::Parent(Some(group)) => check_parent(p, id, *group)?,
                    TrackProp::Input(input) if *input != InputSource::None => {
                        let t = p.track(id).ok_or(CommandError::TrackNotFound(id))?;
                        if t.kind != TrackKind::Audio {
                            return Err(CommandError::Invalid("only Audio tracks record from an input".into()));
                        }
                    }
                    _ => {}
                }
                let t = p.track_mut(id).ok_or(CommandError::TrackNotFound(id))?;
                let old = set_track_prop(t, change);
                Ok(SetTrack { id, change: old })
            }

            AddScene { scene, index } => {
                p.ids.observe(scene.id.0);
                let id = scene.id;
                p.scenes.insert(index.min(p.scenes.len()), scene);
                Ok(RemoveScene { id })
            }
            RemoveScene { id } => {
                let index = p.scenes.iter().position(|s| s.id == id).ok_or(CommandError::SceneNotFound(id))?;
                // Clear slots for this scene so the inverse can restore them.
                let mut restores = Vec::new();
                for t in &mut p.tracks {
                    if let Some(clip) = t.session_slots.remove(&id) {
                        restores.push(SetSessionSlot { track: t.id, scene: id, clip: Some(clip) });
                    }
                }
                let scene = p.scenes.remove(index);
                let mut commands = vec![AddScene { scene, index }];
                commands.extend(restores);
                Ok(Batch { commands })
            }
            RenameScene { id, name } => {
                let s = p.scenes.iter_mut().find(|s| s.id == id).ok_or(CommandError::SceneNotFound(id))?;
                Ok(RenameScene { id, name: std::mem::replace(&mut s.name, name) })
            }

            AddSource { id, source } => {
                if p.sources.contains_key(&id) {
                    return Err(CommandError::DuplicateId(id.to_string()));
                }
                p.ids.observe(id.0);
                if let Source::Notes(n) = source.as_ref() {
                    for note in &n.notes {
                        p.ids.observe(note.id.0);
                    }
                }
                p.sources.insert(id, *source);
                Ok(RemoveSource { id })
            }
            RemoveSource { id } => {
                let source = p.sources.remove(&id).ok_or(CommandError::SourceNotFound(id))?;
                Ok(AddSource { id, source: Box::new(source) })
            }
            SetSourceFile { source, path, hash } => match p.sources.get_mut(&source) {
                Some(Source::Audio(a)) => {
                    let old_path = std::mem::replace(&mut a.path, path);
                    let old_hash = std::mem::replace(&mut a.hash, hash);
                    Ok(SetSourceFile { source, path: old_path, hash: old_hash })
                }
                Some(_) => Err(CommandError::NotAudio(source)),
                None => Err(CommandError::SourceNotFound(source)),
            },
            AddClip { id, clip } => {
                if p.clips.contains_key(&id) {
                    return Err(CommandError::DuplicateId(id.to_string()));
                }
                if !p.sources.contains_key(&clip.source) {
                    return Err(CommandError::SourceNotFound(clip.source));
                }
                p.ids.observe(id.0);
                p.clips.insert(id, *clip);
                Ok(RemoveClip { id })
            }
            RemoveClip { id } => {
                let clip = p.clips.remove(&id).ok_or(CommandError::ClipNotFound(id))?;
                Ok(AddClip { id, clip: Box::new(clip) })
            }
            UpdateClip { id, clip } => {
                let slot = p.clips.get_mut(&id).ok_or(CommandError::ClipNotFound(id))?;
                Ok(UpdateClip { id, clip: Box::new(std::mem::replace(slot, *clip)) })
            }

            AddGroove { entry, index } => {
                if p.groove(entry.id).is_some() {
                    return Err(CommandError::DuplicateId(entry.id.to_string()));
                }
                p.ids.observe(entry.id.0);
                let mut entry = *entry;
                entry.groove.normalize();
                entry.settings = entry.settings.normalized();
                let id = entry.id;
                let i = index.min(p.grooves.len());
                p.grooves.insert(i, entry);
                Ok(RemoveGroove { id })
            }
            RemoveGroove { id } => {
                let i = p.grooves.iter().position(|g| g.id == id).ok_or_else(|| CommandError::Invalid(format!("groove {id} not found")))?;
                let entry = p.grooves.remove(i);
                let mut restores = Vec::new();
                let users: Vec<ClipId> = p.clip_grooves.iter().filter(|(_, g)| **g == id).map(|(c, _)| *c).collect();
                for cid in users {
                    p.clip_grooves.remove(&cid);
                    restores.push(SetClipGroove { clip: cid, groove: Some(id) });
                }
                Ok(with_restores(AddGroove { entry: Box::new(entry), index: i }, restores))
            }
            UpdateGroove { id, groove } => {
                let mut groove = *groove;
                groove.normalize();
                let slot = p.grooves.iter_mut().find(|g| g.id == id).ok_or_else(|| CommandError::Invalid(format!("groove {id} not found")))?;
                Ok(UpdateGroove { id, groove: Box::new(std::mem::replace(&mut slot.groove, groove)) })
            }
            SetGrooveSettings { id, settings } => {
                let slot = p.grooves.iter_mut().find(|g| g.id == id).ok_or_else(|| CommandError::Invalid(format!("groove {id} not found")))?;
                Ok(SetGrooveSettings { id, settings: std::mem::replace(&mut slot.settings, settings.normalized()) })
            }
            SetClipGroove { clip, groove } => {
                if let Some(g) = groove
                    && p.groove(g).is_none()
                {
                    return Err(CommandError::Invalid(format!("groove {g} not found")));
                }
                if !p.clips.contains_key(&clip) {
                    return Err(CommandError::ClipNotFound(clip));
                }
                let old = match groove {
                    Some(g) => p.clip_grooves.insert(clip, g),
                    None => p.clip_grooves.remove(&clip),
                };
                Ok(SetClipGroove { clip, groove: old })
            }
            SetGrooveAmount { amount } => {
                let amount = if amount.is_finite() { amount.clamp(0.0, MAX_GROOVE_AMOUNT) } else { 1.0 };
                Ok(SetGrooveAmount { amount: std::mem::replace(&mut p.groove_amount, amount) })
            }

            PlaceClip { track, placement } => {
                if !p.clips.contains_key(&placement.clip) {
                    return Err(CommandError::ClipNotFound(placement.clip));
                }
                p.ids.observe(placement.id.0);
                let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
                let id = placement.id;
                let mut placement = placement;
                // A take lane the track does not have would hide the clip from every lane.
                if usize::from(placement.lane) > t.take_lanes.len() {
                    placement.lane = 0;
                }
                let i = t.arrangement.partition_point(|x| x.start <= placement.start);
                t.arrangement.insert(i, placement);
                Ok(RemovePlacement { track, id })
            }
            RemovePlacement { track, id } => {
                let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
                let i = t.arrangement.iter().position(|x| x.id == id).ok_or(CommandError::PlacementNotFound(id))?;
                let placement = t.arrangement.remove(i);
                Ok(PlaceClip { track, placement })
            }
            MovePlacement { track, id, to_track, start, lane } => {
                let Some(dest_lanes) = p.track(to_track).map(|t| t.take_lanes.len()) else {
                    return Err(CommandError::TrackNotFound(to_track));
                };
                let lane = if usize::from(lane) > dest_lanes { 0 } else { lane };
                let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
                let i = t.arrangement.iter().position(|x| x.id == id).ok_or(CommandError::PlacementNotFound(id))?;
                let mut placement = t.arrangement.remove(i);
                let inverse = MovePlacement { track: to_track, id, to_track: track, start: placement.start, lane: placement.lane };
                placement.start = start;
                placement.lane = lane;
                let dest = p.track_mut(to_track).expect("checked above");
                let j = dest.arrangement.partition_point(|x| x.start <= start);
                dest.arrangement.insert(j, placement);
                Ok(inverse)
            }
            AddTakeLane { track, index, name } => takes::add_lane(p, track, index, name),
            RemoveTakeLane { track, index } => takes::remove_lane(p, track, index),
            RenameTakeLane { track, index, name } => takes::rename_lane(p, track, index, name),
            MoveTakeLane { track, from, to } => takes::move_lane(p, track, from, to),
            SetComp { track, spans } => takes::set_comp(p, track, spans),
            SetSessionSlot { track, scene, clip } => {
                if !p.scenes.iter().any(|s| s.id == scene) {
                    return Err(CommandError::SceneNotFound(scene));
                }
                if let Some(c) = clip
                    && !p.clips.contains_key(&c)
                {
                    return Err(CommandError::ClipNotFound(c));
                }
                let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
                let old = match clip {
                    Some(c) => t.session_slots.insert(scene, c),
                    None => t.session_slots.remove(&scene),
                };
                Ok(SetSessionSlot { track, scene, clip: old })
            }

            AddNotes { source, notes } => {
                for n in &notes {
                    p.ids.observe(n.id.0);
                }
                let src = note_source(p, source)?;
                let ids = notes.iter().map(|n| n.id).collect();
                src.notes.extend(notes);
                src.sort();
                Ok(RemoveNotes { source, ids })
            }
            RemoveNotes { source, ids } => {
                let src = note_source(p, source)?;
                let mut removed = Vec::with_capacity(ids.len());
                src.notes.retain(|n| {
                    if ids.contains(&n.id) {
                        removed.push(n.clone());
                        false
                    } else {
                        true
                    }
                });
                Ok(AddNotes { source, notes: removed })
            }
            UpdateNotes { source, notes } => {
                let src = note_source(p, source)?;
                let mut old = Vec::with_capacity(notes.len());
                for new in notes {
                    if let Some(slot) = src.notes.iter_mut().find(|n| n.id == new.id) {
                        old.push(std::mem::replace(slot, new));
                    }
                }
                src.sort();
                Ok(UpdateNotes { source, notes: old })
            }
            SetWarp { source, warp } => match p.sources.get_mut(&source) {
                Some(Source::Audio(a)) => Ok(SetWarp { source, warp: Box::new(std::mem::replace(&mut a.warp, *warp)) }),
                Some(_) => Err(CommandError::NotAudio(source)),
                None => Err(CommandError::SourceNotFound(source)),
            },

            AddDevice { slot, device } => {
                if p.device(device.id).is_some() {
                    return Err(CommandError::DuplicateId(device.id.to_string()));
                }
                observe_devices(p, std::slice::from_ref(device.as_ref()));
                let id = device.id;
                let chain = chain_for_slot(p, slot)?;
                let index = slot_index(slot).min(chain.len());
                chain.insert(index, *device);
                Ok(RemoveDevice { id })
            }
            RemoveDevice { id } => {
                let links = valid_links(p);
                let mod_links = valid_mod_links(p);
                let (slot, device) = take_device(p, id)?;
                let mut restores = drop_broken_mappings(p, &links);
                restores.extend(drop_broken_mod_mappings(p, &mod_links));
                Ok(with_restores(AddDevice { slot, device: Box::new(device) }, restores))
            }
            MoveDevice { id, to } => {
                let links = valid_links(p);
                let mod_links = valid_mod_links(p);
                let (from, device) = take_device(p, id)?;
                let chain = match chain_for_slot(p, to) {
                    Ok(c) => c,
                    Err(e) => {
                        // Put it back where it was before failing.
                        let back = chain_for_slot(p, from).expect("origin chain exists");
                        back.insert(slot_index(from).min(back.len()), device);
                        return Err(e);
                    }
                };
                let index = slot_index(to).min(chain.len());
                chain.insert(index, device);
                let mut restores = drop_broken_mappings(p, &links);
                restores.extend(drop_broken_mod_mappings(p, &mod_links));
                Ok(with_restores(MoveDevice { id, to: from }, restores))
            }
            SetParam { device, param, value } => {
                let d = p.device_mut(device).ok_or(CommandError::DeviceNotFound(device))?;
                let old = match value {
                    Some(v) => d.params.insert(param, v.clamp(0.0, 1.0)),
                    None => d.params.remove(&param),
                };
                Ok(SetParam { device, param, value: old })
            }
            SetSidechain { device, source } => {
                if let Some(t) = source {
                    if p.track(t).is_none() {
                        return Err(CommandError::TrackNotFound(t));
                    }
                    if p.device_track(device) == Some(t) {
                        return Err(CommandError::Invalid("a device cannot be sidechained from its own track".into()));
                    }
                }
                let d = p.device_mut(device).ok_or(CommandError::DeviceNotFound(device))?;
                Ok(SetSidechain { device, source: std::mem::replace(&mut d.sidechain, source) })
            }
            SetDeviceEnabled { device, enabled } => {
                let d = p.device_mut(device).ok_or(CommandError::DeviceNotFound(device))?;
                Ok(SetDeviceEnabled { device, enabled: std::mem::replace(&mut d.enabled, enabled) })
            }
            SetDeviceState { device, state, preset } => {
                let d = p.device_mut(device).ok_or(CommandError::DeviceNotFound(device))?;
                Ok(SetDeviceState {
                    device,
                    state: std::mem::replace(&mut d.state, state),
                    preset: std::mem::replace(&mut d.preset, preset),
                })
            }
            RenameDevice { device, name } => {
                let d = p.device_mut(device).ok_or(CommandError::DeviceNotFound(device))?;
                Ok(RenameDevice { device, name: std::mem::replace(&mut d.name, name) })
            }

            AddChain { rack, index, chain } => {
                observe_devices(p, &chain.devices);
                let r = rack_mut(p, rack)?;
                let index = index.min(r.chains.len());
                r.chains.insert(index, *chain);
                Ok(RemoveChain { rack, index })
            }
            RemoveChain { rack, index } => {
                let links = valid_links(p);
                let r = rack_mut(p, rack)?;
                if index >= r.chains.len() {
                    return Err(CommandError::OutOfRange(index));
                }
                let chain = r.chains.remove(index);
                let restores = drop_broken_mappings(p, &links);
                Ok(with_restores(AddChain { rack, index, chain: Box::new(chain) }, restores))
            }
            UpdateChain { rack, index, chain } => {
                let links = valid_links(p);
                observe_devices(p, &chain.devices);
                let r = rack_mut(p, rack)?;
                let slot = r.chains.get_mut(index).ok_or(CommandError::OutOfRange(index))?;
                let old = std::mem::replace(slot, *chain);
                let restores = drop_broken_mappings(p, &links);
                Ok(with_restores(UpdateChain { rack, index, chain: Box::new(old) }, restores))
            }
            SetChainProp { rack, chain, prop } => {
                let r = rack_mut(p, rack)?;
                let returns = r.returns.len();
                let c = r.chains.get_mut(chain).ok_or(CommandError::OutOfRange(chain))?;
                Ok(SetChainProp { rack, chain, prop: set_chain_prop(c, prop, returns)? })
            }
            SetReturnProp { rack, index, prop } => {
                let r = rack_mut(p, rack)?;
                let returns = r.returns.len();
                let c = r.returns.get_mut(index).ok_or(CommandError::OutOfRange(index))?;
                Ok(SetReturnProp { rack, index, prop: set_chain_prop(c, prop, returns)? })
            }
            AddReturn { rack, index, chain } => {
                observe_devices(p, &chain.devices);
                let r = rack_mut(p, rack)?;
                if r.kind != RackKind::Drum {
                    return Err(CommandError::Invalid("only a Drum Rack has returns".into()));
                }
                if r.returns.len() >= MAX_RETURNS {
                    return Err(CommandError::Invalid(format!("a rack has at most {MAX_RETURNS} returns")));
                }
                let index = index.min(r.returns.len());
                for c in &mut r.chains {
                    if index < c.sends.len() {
                        c.sends.insert(index, SEND_OFF_DB);
                    }
                    if let ChainOut::Return(n) = c.output
                        && usize::from(n) >= index
                    {
                        c.output = ChainOut::Return(n + 1);
                    }
                }
                r.returns.insert(index, *chain);
                Ok(RemoveReturn { rack, index })
            }
            RemoveReturn { rack, index } => {
                let links = valid_links(p);
                let r = rack_mut(p, rack)?;
                if index >= r.returns.len() {
                    return Err(CommandError::OutOfRange(index));
                }
                let removed = r.returns.remove(index);
                let mut commands = vec![AddReturn { rack, index, chain: Box::new(removed) }];
                for (ci, c) in r.chains.iter_mut().enumerate() {
                    let old = (c.sends.clone(), c.output);
                    if index < c.sends.len() {
                        c.sends.remove(index);
                    }
                    c.output = match c.output {
                        ChainOut::Return(n) if usize::from(n) == index => ChainOut::Rack,
                        ChainOut::Return(n) if usize::from(n) > index => ChainOut::Return(n - 1),
                        other => other,
                    };
                    if c.sends != old.0 {
                        commands.push(SetChainProp { rack, chain: ci, prop: ChainProp::Sends(old.0) });
                    }
                    if c.output != old.1 {
                        commands.push(SetChainProp { rack, chain: ci, prop: ChainProp::Output(old.1) });
                    }
                }
                commands.extend(drop_broken_mappings(p, &links));
                Ok(Batch { commands })
            }
            AddMapping { rack, macro_index, index, mapping } => {
                let r = rack_ref(p, rack)?;
                let m = r.macros.get(macro_index).ok_or(CommandError::OutOfRange(macro_index))?;
                if p.device(mapping.device).is_none() {
                    return Err(CommandError::DeviceNotFound(mapping.device));
                }
                let inside = r.chains.iter().chain(&r.returns).any(|c| find_device(&c.devices, mapping.device).is_some());
                if !inside {
                    return Err(CommandError::Invalid("a macro can only drive devices inside its own rack".into()));
                }
                if m.mappings.iter().any(|x| x.device == mapping.device && x.param == mapping.param) {
                    return Err(CommandError::Invalid("that parameter is already mapped to this macro".into()));
                }
                let mapping = MacroMapping { min: mapping.min.clamp(0.0, 1.0), max: mapping.max.clamp(0.0, 1.0), ..mapping };
                let m = &mut rack_mut(p, rack)?.macros[macro_index];
                let index = index.min(m.mappings.len());
                m.mappings.insert(index, mapping);
                Ok(RemoveMapping { rack, macro_index, index })
            }
            RemoveMapping { rack, macro_index, index } => {
                let m = rack_mut(p, rack)?.macros.get_mut(macro_index).ok_or(CommandError::OutOfRange(macro_index))?;
                if index >= m.mappings.len() {
                    return Err(CommandError::OutOfRange(index));
                }
                let mapping = m.mappings.remove(index);
                Ok(AddMapping { rack, macro_index, index, mapping })
            }
            SetMappingRange { rack, macro_index, index, min, max } => {
                let m = rack_mut(p, rack)?.macros.get_mut(macro_index).ok_or(CommandError::OutOfRange(macro_index))?;
                let mapping = m.mappings.get_mut(index).ok_or(CommandError::OutOfRange(index))?;
                let (old_min, old_max) = (mapping.min, mapping.max);
                mapping.min = min.clamp(0.0, 1.0);
                mapping.max = max.clamp(0.0, 1.0);
                Ok(SetMappingRange { rack, macro_index, index, min: old_min, max: old_max })
            }
            AddModMapping { device, index, mapping } => {
                let track = p.device_track(device).ok_or(CommandError::DeviceNotFound(device))?;
                if !p.device(device).is_some_and(Device::is_modulator) {
                    return Err(CommandError::Invalid("only a modulator device can be mapped".into()));
                }
                if let ModTarget::Param { device: target, .. } = mapping.target {
                    if target == device {
                        return Err(CommandError::Invalid("a modulator cannot drive itself".into()));
                    }
                    if p.device(target).is_none() {
                        return Err(CommandError::DeviceNotFound(target));
                    }
                    if p.device_track(target) != Some(track) {
                        return Err(CommandError::Invalid("a modulator can only drive devices on its own track".into()));
                    }
                }
                let d = p.device_mut(device).ok_or(CommandError::DeviceNotFound(device))?;
                if d.mods.iter().any(|m| m.target == mapping.target) {
                    return Err(CommandError::Invalid("that target is already mapped to this modulator".into()));
                }
                if d.mods.len() >= MAX_MOD_MAPPINGS {
                    return Err(CommandError::Invalid(format!("a modulator drives at most {MAX_MOD_MAPPINGS} targets")));
                }
                let mapping = ModMapping { min: mapping.min.clamp(-1.0, 1.0), max: mapping.max.clamp(-1.0, 1.0), ..mapping };
                let index = index.min(d.mods.len());
                d.mods.insert(index, mapping);
                Ok(RemoveModMapping { device, index })
            }
            RemoveModMapping { device, index } => {
                let d = p.device_mut(device).ok_or(CommandError::DeviceNotFound(device))?;
                if index >= d.mods.len() {
                    return Err(CommandError::OutOfRange(index));
                }
                let mapping = d.mods.remove(index);
                Ok(AddModMapping { device, index, mapping })
            }
            SetModRange { device, index, min, max } => {
                let d = p.device_mut(device).ok_or(CommandError::DeviceNotFound(device))?;
                let m = d.mods.get_mut(index).ok_or(CommandError::OutOfRange(index))?;
                let (old_min, old_max) = (m.min, m.max);
                m.min = min.clamp(-1.0, 1.0);
                m.max = max.clamp(-1.0, 1.0);
                Ok(SetModRange { device, index, min: old_min, max: old_max })
            }
            SetRackSelector { rack, value } => {
                let r = rack_mut(p, rack)?;
                Ok(SetRackSelector { rack, value: std::mem::replace(&mut r.chain_selector, value.min(127)) })
            }
            SetMacro { rack, index, value } => {
                let r = rack_mut(p, rack)?;
                let m = r.macros.get_mut(index).ok_or(CommandError::OutOfRange(index))?;
                Ok(SetMacro { rack, index, value: std::mem::replace(&mut m.value, value.clamp(0.0, 1.0)) })
            }
            SetMacroDef { rack, index, name, mappings } => {
                let r = rack_mut(p, rack)?;
                let m = r.macros.get_mut(index).ok_or(CommandError::OutOfRange(index))?;
                Ok(SetMacroDef {
                    rack,
                    index,
                    name: std::mem::replace(&mut m.name, name),
                    mappings: std::mem::replace(&mut m.mappings, mappings),
                })
            }
            SetVariations { rack, variations } => {
                let r = rack_mut(p, rack)?;
                Ok(SetVariations { rack, variations: std::mem::replace(&mut r.variations, variations) })
            }

            SetAutomation { track, target, lane } => {
                let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
                let pos = t.automation.iter().position(|l| l.target == target);
                let old = match (pos, lane) {
                    (Some(i), Some(lane)) => Some(std::mem::replace(&mut t.automation[i], lane)),
                    (Some(i), None) => Some(t.automation.remove(i)),
                    (None, Some(lane)) => {
                        t.automation.push(lane);
                        None
                    }
                    (None, None) => None,
                };
                Ok(SetAutomation { track, target, lane: old })
            }

            FreezeTrack { track, file } => {
                let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
                if !t.can_freeze() {
                    return Err(CommandError::Invalid("only Instrument and Audio tracks can be frozen".into()));
                }
                if t.is_frozen() {
                    return Err(CommandError::Invalid("the track is already frozen".into()));
                }
                t.freeze = Some(Freeze { file });
                // A frozen track plays its render and records nothing; undo gives the arming back.
                let was_armed = std::mem::replace(&mut t.arm, false);
                let unfreeze = UnfreezeTrack { track };
                Ok(if was_armed { Batch { commands: vec![unfreeze, SetTrack { id: track, change: TrackProp::Arm(true) }] } } else { unfreeze })
            }
            UnfreezeTrack { track } => {
                let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
                let frozen = t.freeze.take().ok_or_else(|| CommandError::Invalid("the track is not frozen".into()))?;
                Ok(FreezeTrack { track, file: frozen.file })
            }
            FlattenTrack { track, source, clip, placement } => {
                if p.sources.contains_key(&source) {
                    return Err(CommandError::DuplicateId(source.to_string()));
                }
                if p.clips.contains_key(&clip) {
                    return Err(CommandError::DuplicateId(clip.to_string()));
                }
                let t = p.track(track).ok_or(CommandError::TrackNotFound(track))?;
                let file = t.freeze.as_ref().ok_or_else(|| CommandError::Invalid("only a frozen track can be flattened".into()))?.file.clone();
                if t.arrangement.iter().any(|pl| pl.id == placement) {
                    return Err(CommandError::DuplicateId(placement.to_string()));
                }
                let saved = t.clone();
                let length = file.length_ticks(p.tempo.initial_bpm()).max(1);
                p.ids.observe(source.0);
                p.ids.observe(clip.0);
                p.ids.observe(placement.0);
                p.sources.insert(source, Source::Audio(file));
                let name = saved.name.clone();
                p.clips.insert(clip, Clip::new(name, saved.color, source, length));
                let t = p.track_mut(track).expect("checked above");
                t.freeze = None;
                if t.kind == TrackKind::Instrument {
                    t.kind = TrackKind::Audio;
                    t.session_slots.clear();
                    t.keyboard_input = false;
                    t.scale = None;
                }
                t.arm = false;
                // The takes and the comp described the clips that are gone.
                t.take_lanes.clear();
                t.comp.clear();
                t.devices.clear();
                t.automation.retain(|l| !matches!(l.target, AutomationTarget::DeviceParam { .. } | AutomationTarget::DeviceEnabled { .. } | AutomationTarget::Macro { .. }));
                t.arrangement = vec![Placement { id: placement, clip, start: 0, lane: 0 }];
                Ok(RestoreFlattened { track: Box::new(saved), source, clip })
            }
            RestoreFlattened { track, source, clip } => {
                let id = track.id;
                let index = p.track_index(id).ok_or(CommandError::TrackNotFound(id))?;
                let flattened = std::mem::replace(&mut p.tracks[index], *track);
                p.sources.remove(&source);
                p.clips.remove(&clip);
                let placement = flattened.arrangement.first().map_or(PlacementId(0), |pl| pl.id);
                Ok(FlattenTrack { track: id, source, clip, placement })
            }

            Batch { commands } => {
                let mut inverses = Vec::with_capacity(commands.len());
                for c in commands {
                    match c.apply(p) {
                        Ok(inv) => inverses.push(inv),
                        Err(e) => {
                            // Roll back what was applied so the batch is atomic.
                            for inv in inverses.into_iter().rev() {
                                inv.apply(p).expect("inverse of an applied command must apply");
                            }
                            return Err(e);
                        }
                    }
                }
                inverses.reverse();
                Ok(Batch { commands: inverses })
            }
        }
    }
}

/// Make future ids come after the ids of `devices` and of every device
/// nested in their racks.
fn observe_devices(p: &mut Project, devices: &[Device]) {
    let mut max = 0;
    visit_devices(devices, &mut |d| max = max.max(d.id.0));
    p.ids.observe(max);
}

/// Make future ids come after every id a track brings along: its own, its
/// devices' (racks included) and its placements'. A command can carry ids
/// worked out on another copy of the project, so the allocator must not rely
/// on having handed them out.
fn observe_track(p: &mut Project, t: &Track) {
    p.ids.observe(t.id.0);
    observe_devices(p, &t.devices);
    for pl in &t.arrangement {
        p.ids.observe(pl.id.0);
    }
}

/// A group or folder `group` may take `track` as a member when it exists and
/// is not the track itself or inside it (a track cannot be its own ancestor).
fn check_parent(p: &Project, track: TrackId, group: TrackId) -> Result<(), CommandError> {
    let t = p.track(track).ok_or(CommandError::TrackNotFound(track))?;
    if t.parent == Some(group) {
        return Ok(());
    }
    let g = p.track(group).ok_or(CommandError::TrackNotFound(group))?;
    if !matches!(g.kind, TrackKind::Group | TrackKind::Folder) {
        return Err(CommandError::Invalid(format!("{group} is not a group or folder")));
    }
    if crate::groups::within(p, group, track) {
        return Err(CommandError::Invalid("a group cannot be moved into itself".into()));
    }
    Ok(())
}

/// Which `(rack, mapped device)` pairs are sound now: the rack holds the
/// device, at any depth, in a chain or a return.
fn valid_links(p: &Project) -> std::collections::HashSet<(DeviceId, DeviceId)> {
    let mut out = std::collections::HashSet::new();
    for t in &p.tracks {
        visit_devices(&t.devices, &mut |d| {
            if let DeviceKind::Rack(rack) = &d.kind {
                let mut inside = Vec::new();
                for c in rack.chains.iter().chain(&rack.returns) {
                    visit_devices(&c.devices, &mut |x| inside.push(x.id));
                }
                for m in &rack.macros {
                    out.extend(m.mappings.iter().filter(|x| inside.contains(&x.device)).map(|x| (d.id, x.device)));
                }
            }
        });
    }
    out
}

/// After a device left a rack (deleted, or moved out), drop the macro
/// mappings that were sound in `before` ([`valid_links`]) and no longer are,
/// and return the `AddMapping`s that put them back, in the order that
/// restores their positions. A mapping that was already dangling is left alone.
fn drop_broken_mappings(p: &mut Project, before: &std::collections::HashSet<(DeviceId, DeviceId)>) -> Vec<Command> {
    let mut restores = Vec::new();
    if before.is_empty() {
        return restores;
    }
    for t in &mut p.tracks {
        visit_devices_mut(&mut t.devices, &mut |d| {
            let rack_id = d.id;
            let DeviceKind::Rack(rack) = &mut d.kind else { return };
            let mut inside = Vec::new();
            for c in rack.chains.iter().chain(&rack.returns) {
                visit_devices(&c.devices, &mut |x| inside.push(x.id));
            }
            for (mi, m) in rack.macros.iter_mut().enumerate() {
                let mut kept = Vec::with_capacity(m.mappings.len());
                for (i, mapping) in std::mem::take(&mut m.mappings).into_iter().enumerate() {
                    if before.contains(&(rack_id, mapping.device)) && !inside.contains(&mapping.device) {
                        restores.push(Command::AddMapping { rack: rack_id, macro_index: mi, index: i, mapping });
                    } else {
                        kept.push(mapping);
                    }
                }
                m.mappings = kept;
            }
        });
    }
    restores
}

/// Which `(modulator, target device)` pairs are sound now: the target exists
/// and sits on the modulator's own track.
fn valid_mod_links(p: &Project) -> std::collections::HashSet<(DeviceId, DeviceId)> {
    let mut out = std::collections::HashSet::new();
    for t in &p.tracks {
        visit_devices(&t.devices, &mut |d| {
            for m in &d.mods {
                if let ModTarget::Param { device, .. } = m.target
                    && p.device_track(device) == Some(t.id)
                {
                    out.insert((d.id, device));
                }
            }
        });
    }
    out
}

/// After a device was deleted or moved, drop the modulator mappings that were
/// sound in `before` ([`valid_mod_links`]) and no longer are (the target is
/// gone, or left the modulator's track, or the modulator did), and return the
/// `AddModMapping`s that put them back, in the order that restores their
/// positions.
fn drop_broken_mod_mappings(p: &mut Project, before: &std::collections::HashSet<(DeviceId, DeviceId)>) -> Vec<Command> {
    let mut restores = Vec::new();
    if before.is_empty() {
        return restores;
    }
    let sound: Vec<(DeviceId, DeviceId)> = before.iter().copied().filter(|&(m, t)| matches!((p.device_track(m), p.device_track(t)), (Some(a), Some(b)) if a == b)).collect();
    let mut tracks = std::mem::take(&mut p.tracks);
    for t in &mut tracks {
        visit_devices_mut(&mut t.devices, &mut |d| {
            let modulator = d.id;
            if d.mods.is_empty() {
                return;
            }
            let mut kept = Vec::with_capacity(d.mods.len());
            for (i, mapping) in std::mem::take(&mut d.mods).into_iter().enumerate() {
                let broken = matches!(mapping.target, ModTarget::Param { device, .. } if before.contains(&(modulator, device)) && !sound.contains(&(modulator, device)));
                if broken {
                    restores.push(Command::AddModMapping { device: modulator, index: i, mapping });
                } else {
                    kept.push(mapping);
                }
            }
            d.mods = kept;
        });
    }
    p.tracks = tracks;
    restores
}

/// `undo`, followed by `restores` when there are any.
fn with_restores(undo: Command, mut restores: Vec<Command>) -> Command {
    if restores.is_empty() {
        return undo;
    }
    restores.insert(0, undo);
    Command::Batch { commands: restores }
}

fn visit_devices_mut(chain: &mut [Device], f: &mut impl FnMut(&mut Device)) {
    for d in chain {
        f(d);
        if let DeviceKind::Rack(rack) = &mut d.kind {
            for c in rack.chains.iter_mut().chain(rack.returns.iter_mut()) {
                visit_devices_mut(&mut c.devices, f);
            }
        }
    }
}

/// Make everything that pointed at the just-removed `gone` point nowhere, so
/// no output, send, group, VCA or sidechain dangles, and return the
/// commands that put each of them back (to run after `gone` is re-added).
///
/// * Tracks that routed into `gone` go to Main (or, for its members, to the
///   group around it, as when ungrouping), unless that would make a loop.
/// * Its members move up to its own group; sends to it are dropped (with
///   their automation, and the later sends' lanes renumbered); VCA members
///   are freed; devices listening to it are disconnected.
fn unlink_track(p: &mut Project, gone: &Track) -> Vec<Command> {
    let id = gone.id;
    let mut restores = Vec::new();
    // Members first: the output replacement below reads the new parents.
    let members: Vec<TrackId> = p.tracks.iter().filter(|t| t.parent == Some(id)).map(|t| t.id).collect();
    let mut restore_parent = Vec::new();
    for &m in &members {
        let t = p.track_mut(m).expect("listed above");
        t.parent = gone.parent;
        restore_parent.push(Command::SetTrack { id: m, change: TrackProp::Parent(Some(id)) });
    }
    let mut restore_output = Vec::new();
    let mut restore_sends = Vec::new();
    let mut restore_vca = Vec::new();
    let mut restore_lanes = Vec::new();
    let ids: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
    for t_id in ids {
        let was_member = members.contains(&t_id);
        let replacement = if was_member { crate::groups::bus_output(p, gone.parent) } else { Output::Main };
        let t = p.track_mut(t_id).expect("listed above");
        let old_output = t.output;
        if old_output == Output::Track(id) {
            t.output = replacement;
        }
        // Moving a member up changes where "Main" leads; never close a loop by it.
        if (was_member || old_output == Output::Track(id)) && routing::reaches(p, t_id, t_id) {
            p.track_mut(t_id).expect("listed above").output = Output::None;
        }
        let t = p.track_mut(t_id).expect("listed above");
        if t.output != old_output {
            restore_output.push(Command::SetTrack { id: t_id, change: TrackProp::Output(old_output) });
        }
        let t = p.track_mut(t_id).expect("listed above");
        if t.vca == Some(id) {
            t.vca = None;
            restore_vca.push(Command::SetTrack { id: t_id, change: TrackProp::Vca(Some(id)) });
        }
        if t.sends.iter().any(|s| s.target == id) {
            let old_sends = t.sends.clone();
            let old_lanes = t.automation.clone();
            let kept: Vec<usize> = (0..old_sends.len()).filter(|&i| old_sends[i].target != id).collect();
            t.sends = kept.iter().map(|&i| old_sends[i].clone()).collect();
            t.automation = old_lanes
                .iter()
                .filter_map(|lane| match lane.target {
                    AutomationTarget::Send { index } => {
                        let new = kept.iter().position(|&k| k == usize::from(index))?;
                        Some(AutomationLane { target: AutomationTarget::Send { index: new as u16 }, ..lane.clone() })
                    }
                    _ => Some(lane.clone()),
                })
                .collect();
            restore_sends.push(Command::SetTrack { id: t_id, change: TrackProp::Sends(old_sends) });
            if t.automation != old_lanes {
                // Take every lane out and put the old ones back one by one: that
                // is the only way to get the order back (a lane is added last).
                for lane in &t.automation {
                    restore_lanes.push(Command::SetAutomation { track: t_id, target: lane.target, lane: None });
                }
                for lane in &old_lanes {
                    restore_lanes.push(Command::SetAutomation { track: t_id, target: lane.target, lane: Some(lane.clone()) });
                }
            }
        }
    }
    let mut restore_side = Vec::new();
    for t in &mut p.tracks {
        visit_devices_mut(&mut t.devices, &mut |d| {
            if d.sidechain == Some(id) {
                d.sidechain = None;
                restore_side.push(Command::SetSidechain { device: d.id, source: Some(id) });
            }
        });
    }
    restores.extend(restore_parent);
    restores.extend(restore_output);
    restores.extend(restore_sends);
    restores.extend(restore_lanes);
    restores.extend(restore_vca);
    restores.extend(restore_side);
    restores
}

fn set_track_prop(t: &mut Track, change: TrackProp) -> TrackProp {
    use std::mem::replace;
    match change {
        TrackProp::Name(v) => TrackProp::Name(replace(&mut t.name, v)),
        TrackProp::Color(v) => TrackProp::Color(replace(&mut t.color, v)),
        TrackProp::VolumeDb(v) => TrackProp::VolumeDb(replace(&mut t.volume_db, v.clamp(-150.0, 12.0))),
        TrackProp::Pan(v) => TrackProp::Pan(replace(&mut t.pan, v.clamp(-1.0, 1.0))),
        TrackProp::Mute(v) => TrackProp::Mute(replace(&mut t.mute, v)),
        TrackProp::Solo(v) => TrackProp::Solo(replace(&mut t.solo, v)),
        TrackProp::Arm(v) => TrackProp::Arm(replace(&mut t.arm, v)),
        TrackProp::Monitor(v) => TrackProp::Monitor(replace(&mut t.monitor, v)),
        TrackProp::Input(v) => TrackProp::Input(replace(&mut t.input, v)),
        TrackProp::Output(v) => TrackProp::Output(replace(&mut t.output, v)),
        TrackProp::Sends(v) => TrackProp::Sends(replace(&mut t.sends, v)),
        TrackProp::Parent(v) => TrackProp::Parent(replace(&mut t.parent, v)),
        TrackProp::Vca(v) => TrackProp::Vca(replace(&mut t.vca, v)),
        TrackProp::Collapsed(v) => TrackProp::Collapsed(replace(&mut t.collapsed, v)),
        TrackProp::Height(v) => TrackProp::Height(replace(&mut t.height, v.clamp(24.0, 400.0))),
        TrackProp::Scale(v) => TrackProp::Scale(replace(&mut t.scale, v)),
        TrackProp::KeyboardInput(v) => TrackProp::KeyboardInput(replace(&mut t.keyboard_input, v)),
    }
}

fn note_source(p: &mut Project, id: SourceId) -> Result<&mut NoteSource, CommandError> {
    match p.sources.get_mut(&id) {
        Some(Source::Notes(n)) => Ok(n),
        Some(_) => Err(CommandError::NotNotes(id)),
        None => Err(CommandError::SourceNotFound(id)),
    }
}

fn rack_ref(p: &Project, id: DeviceId) -> Result<&Rack, CommandError> {
    match p.device(id) {
        Some(Device { kind: DeviceKind::Rack(r), .. }) => Ok(r),
        Some(_) => Err(CommandError::NotARack(id)),
        None => Err(CommandError::DeviceNotFound(id)),
    }
}

/// Set one property of a chain and return the property that undoes it.
/// `returns` is how many returns the rack has (sends and outputs may only
/// name those).
fn set_chain_prop(c: &mut Chain, prop: ChainProp, returns: usize) -> Result<ChainProp, CommandError> {
    use std::mem::replace;
    let zone = |(a, b): (u8, u8)| (a.min(b).min(127), a.max(b).min(127));
    let fade = |(a, b): (u8, u8)| (a.min(127), b.min(127));
    let send = |db: f32| if db.is_nan() { SEND_OFF_DB } else { db.clamp(SEND_OFF_DB, SEND_MAX_DB) };
    Ok(match prop {
        ChainProp::Name(v) => ChainProp::Name(replace(&mut c.name, v)),
        ChainProp::Color(v) => ChainProp::Color(replace(&mut c.color, v)),
        ChainProp::VolumeDb(v) => ChainProp::VolumeDb(replace(&mut c.volume_db, v.clamp(-150.0, 12.0))),
        ChainProp::Pan(v) => ChainProp::Pan(replace(&mut c.pan, v.clamp(-1.0, 1.0))),
        ChainProp::Mute(v) => ChainProp::Mute(replace(&mut c.mute, v)),
        ChainProp::Solo(v) => ChainProp::Solo(replace(&mut c.solo, v)),
        ChainProp::KeyRange(v) => ChainProp::KeyRange(replace(&mut c.key_range, zone(v))),
        ChainProp::VelRange(v) => ChainProp::VelRange(replace(&mut c.velocity_range, zone(v))),
        ChainProp::SelRange(v) => ChainProp::SelRange(replace(&mut c.selector_range, zone(v))),
        ChainProp::KeyFade(v) => ChainProp::KeyFade(replace(&mut c.key_fade, fade(v))),
        ChainProp::VelFade(v) => ChainProp::VelFade(replace(&mut c.vel_fade, fade(v))),
        ChainProp::SelFade(v) => ChainProp::SelFade(replace(&mut c.sel_fade, fade(v))),
        ChainProp::Choke(v) => ChainProp::Choke(replace(&mut c.choke, v.min(16))),
        ChainProp::ReceiveNote(v) => ChainProp::ReceiveNote(replace(&mut c.note, v.map(|n| n.min(127)))),
        ChainProp::PlayNote(v) => ChainProp::PlayNote(replace(&mut c.play_note, v.map(|n| n.min(127)))),
        ChainProp::Sends(v) => {
            if v.len() > returns {
                return Err(CommandError::Invalid(format!("{} sends for {returns} returns", v.len())));
            }
            ChainProp::Sends(replace(&mut c.sends, v.into_iter().map(send).collect()))
        }
        ChainProp::Send(i, db) => {
            if i >= returns {
                return Err(CommandError::Invalid(format!("send {i} but the rack has {returns} returns")));
            }
            if i < c.sends.len() {
                ChainProp::Send(i, replace(&mut c.sends[i], send(db)))
            } else {
                let old = c.sends.clone();
                c.sends.resize(i + 1, SEND_OFF_DB);
                c.sends[i] = send(db);
                ChainProp::Sends(old)
            }
        }
        ChainProp::Output(v) => {
            if let ChainOut::Return(n) = v
                && usize::from(n) >= returns
            {
                return Err(CommandError::Invalid(format!("return {n} but the rack has {returns} returns")));
            }
            ChainProp::Output(replace(&mut c.output, v))
        }
    })
}

fn rack_mut(p: &mut Project, id: DeviceId) -> Result<&mut Rack, CommandError> {
    match p.device_mut(id) {
        Some(Device { kind: DeviceKind::Rack(r), .. }) => Ok(r),
        Some(_) => Err(CommandError::NotARack(id)),
        None => Err(CommandError::DeviceNotFound(id)),
    }
}

fn slot_index(slot: DeviceSlot) -> usize {
    match slot {
        DeviceSlot::Track { index, .. } | DeviceSlot::Rack { index, .. } => index,
        DeviceSlot::Return { pos, .. } => pos,
    }
}

fn chain_for_slot(p: &mut Project, slot: DeviceSlot) -> Result<&mut Vec<Device>, CommandError> {
    match slot {
        DeviceSlot::Track { track, .. } => Ok(&mut p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?.devices),
        DeviceSlot::Rack { rack, chain, .. } => {
            let r = rack_mut(p, rack)?;
            Ok(&mut r.chains.get_mut(chain).ok_or(CommandError::OutOfRange(chain))?.devices)
        }
        DeviceSlot::Return { rack, index, .. } => {
            let r = rack_mut(p, rack)?;
            Ok(&mut r.returns.get_mut(index).ok_or(CommandError::OutOfRange(index))?.devices)
        }
    }
}

/// Remove a device wherever it lives and report the slot it came from.
fn take_device(p: &mut Project, id: DeviceId) -> Result<(DeviceSlot, Device), CommandError> {
    fn take_in(chain: &mut Vec<Device>, id: DeviceId, here: &dyn Fn(usize) -> DeviceSlot) -> Option<(DeviceSlot, Device)> {
        if let Some(i) = chain.iter().position(|d| d.id == id) {
            return Some((here(i), chain.remove(i)));
        }
        for d in chain.iter_mut() {
            let rack_id = d.id;
            if let DeviceKind::Rack(r) = &mut d.kind {
                for (ci, c) in r.chains.iter_mut().enumerate() {
                    let slot = move |index| DeviceSlot::Rack { rack: rack_id, chain: ci, index };
                    if let Some(found) = take_in(&mut c.devices, id, &slot) {
                        return Some(found);
                    }
                }
                for (ri, c) in r.returns.iter_mut().enumerate() {
                    let slot = move |pos| DeviceSlot::Return { rack: rack_id, index: ri, pos };
                    if let Some(found) = take_in(&mut c.devices, id, &slot) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }
    for t in &mut p.tracks {
        let track = t.id;
        let here = move |index| DeviceSlot::Track { track, index };
        if let Some(found) = take_in(&mut t.devices, id, &here) {
            return Ok(found);
        }
    }
    Err(CommandError::DeviceNotFound(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::PPQ;

    fn project_with_track() -> (Project, TrackId) {
        let mut p = Project::new("t");
        let id = p.alloc_track_id();
        let track = Track::new(id, TrackKind::Instrument, "Lead", Color::palette(3));
        Command::AddTrack { track: Box::new(track), index: 1 }.apply(&mut p).unwrap();
        (p, id)
    }

    #[test]
    fn every_command_round_trips() {
        let (mut p, track) = project_with_track();
        let before = p.clone();
        let src = p.alloc_source_id();
        let clip = p.alloc_clip_id();
        let n1 = p.alloc_note_id();
        let dev = p.alloc_device_id();
        let rack = p.alloc_device_id();
        let pl = p.alloc_placement_id();
        let scene = p.scenes[0].id;

        let mut r = Device::stock(rack, "rack", "Rack");
        r.kind = DeviceKind::Rack(Rack::new(RackKind::Audio));
        let script = vec![
            Command::SetTempo { bpm: 128.0 },
            Command::SetScale { scale: Some(crate::Scale::new(crate::Key(9), crate::ScaleKind::Minor)) },
            Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource::default())) },
            Command::AddClip {
                id: clip,
                clip: Box::new(Clip::new("c", Color::palette(1), src, 4 * PPQ)),
            },
            Command::AddNotes { source: src, notes: vec![Note::new(n1, 60, 0, PPQ, 0.8)] },
            Command::UpdateNotes { source: src, notes: vec![Note::new(n1, 62, PPQ, PPQ, 0.5)] },
            Command::PlaceClip { track, placement: Placement { id: pl, clip, start: 0, lane: 0 } },
            Command::MovePlacement { track, id: pl, to_track: track, start: 8 * PPQ, lane: 0 },
            Command::SetSessionSlot { track, scene, clip: Some(clip) },
            Command::AddDevice { slot: DeviceSlot::Track { track, index: 0 }, device: Box::new(r) },
            Command::AddChain { rack, index: 0, chain: Box::new(Chain::new("A", Color::palette(2))) },
            Command::AddDevice {
                slot: DeviceSlot::Rack { rack, chain: 0, index: 0 },
                device: Box::new(Device::stock(dev, "fx.eq", "EQ")),
            },
            Command::SetParam { device: dev, param: 3, value: Some(0.25) },
            Command::MoveDevice { id: dev, to: DeviceSlot::Track { track, index: 0 } },
            Command::SetMacro { rack, index: 2, value: 0.7 },
            Command::SetTrack { id: track, change: TrackProp::VolumeDb(-6.0) },
            Command::SetAutomation { track, target: AutomationTarget::Volume, lane: Some(AutomationLane::new(AutomationTarget::Volume)) },
            Command::RemoveNotes { source: src, ids: vec![n1] },
        ];

        let mut inverses = Vec::new();
        for c in script {
            let json = serde_json::to_string(&c).unwrap();
            let c: Command = serde_json::from_str(&json).unwrap();
            inverses.push(c.apply(&mut p).expect("command applies"));
        }
        assert_eq!(p.tempo.initial_bpm(), 128.0);
        for inv in inverses.into_iter().rev() {
            inv.apply(&mut p).unwrap();
        }
        // Ids allocated during the script are kept, everything else is restored.
        p.ids = before.ids.clone();
        assert_eq!(p, before);
    }

    #[test]
    fn set_warp_swaps_and_undoes() {
        let mut p = Project::new("t");
        let src = p.alloc_source_id();
        let audio = AudioSource { path: "a.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames: 10, warp: Warp::default(), key: None };
        Command::AddSource { id: src, source: Box::new(Source::Audio(audio)) }.apply(&mut p).unwrap();
        let warp = Warp { transpose: -7.0, detune: 25.0, reverse: true, ..Warp::default() };
        let undo = Command::SetWarp { source: src, warp: Box::new(warp.clone()) }.apply(&mut p).unwrap();
        let Some(Source::Audio(a)) = p.sources.get(&src) else { panic!() };
        assert_eq!(a.warp, warp);
        assert!((a.warp.pitch() + 6.75).abs() < 1e-6);
        undo.apply(&mut p).unwrap();
        let Some(Source::Audio(a)) = p.sources.get(&src) else { panic!() };
        assert_eq!(a.warp, Warp::default());
        // Old projects without detune / reverse still load.
        let old: Warp = serde_json::from_str(r#"{"enabled":false,"mode":"beats","original_bpm":null,"transpose":3.0,"markers":[]}"#).unwrap();
        assert_eq!((old.transpose, old.detune, old.reverse), (3.0, 0.0, false));
        // A note source has no warp.
        let notes = p.alloc_source_id();
        Command::AddSource { id: notes, source: Box::new(Source::Notes(NoteSource::default())) }.apply(&mut p).unwrap();
        assert_eq!(Command::SetWarp { source: notes, warp: Box::new(Warp::default()) }.apply(&mut p), Err(CommandError::NotAudio(notes)));
    }

    #[test]
    fn failing_batch_is_atomic() {
        let (mut p, track) = project_with_track();
        let before = p.clone();
        let err = Command::Batch {
            commands: vec![
                Command::SetTrack { id: track, change: TrackProp::Mute(true) },
                Command::RemoveTrack { id: TrackId(9999) },
            ],
        }
        .apply(&mut p);
        assert_eq!(err, Err(CommandError::TrackNotFound(TrackId(9999))));
        assert_eq!(p, before);
    }

    #[test]
    fn main_track_is_protected() {
        let mut p = Project::new("t");
        let main = p.main_track().id;
        assert_eq!(Command::RemoveTrack { id: main }.apply(&mut p), Err(CommandError::MainTrack));
    }

    /// Tracks of every kind wired to each other, to try deleting each one.
    struct Rig {
        p: Project,
        src: TrackId,
        bus: TrackId,
        ret: TrackId,
        vca: TrackId,
        group: TrackId,
        member_out: TrackId,
        member_main: TrackId,
        keyed: DeviceId,
    }

    fn rig() -> Rig {
        let mut p = Project::new("t");
        let add = |p: &mut Project, kind, name: &str| {
            let id = p.alloc_track_id();
            let at = p.tracks.len() - 1;
            Command::AddTrack { track: Box::new(Track::new(id, kind, name, Color::palette(1))), index: at }.apply(p).unwrap();
            id
        };
        let src = add(&mut p, TrackKind::Audio, "src");
        let bus = add(&mut p, TrackKind::Audio, "bus");
        let ret = add(&mut p, TrackKind::Return, "ret");
        let vca = add(&mut p, TrackKind::Vca, "vca");
        let group = add(&mut p, TrackKind::Group, "group");
        let member_out = add(&mut p, TrackKind::Audio, "m1");
        let member_main = add(&mut p, TrackKind::Audio, "m2");
        let other = add(&mut p, TrackKind::Audio, "other");
        let set = |p: &mut Project, id, change| {
            Command::SetTrack { id, change }.apply(p).unwrap();
        };
        set(&mut p, src, TrackProp::Output(Output::Track(bus)));
        set(&mut p, src, TrackProp::Sends(vec![
            Send { target: ret, level_db: -6.0, pre_fader: false },
            Send { target: bus, level_db: -9.0, pre_fader: true },
        ]));
        set(&mut p, src, TrackProp::Vca(Some(vca)));
        set(&mut p, other, TrackProp::Sends(vec![Send { target: ret, level_db: -3.0, pre_fader: false }]));
        for m in [member_out, member_main] {
            set(&mut p, m, TrackProp::Parent(Some(group)));
        }
        set(&mut p, member_out, TrackProp::Output(Output::Track(group)));
        let keyed = p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Track { track: other, index: 0 }, device: Box::new(Device::stock(keyed, "fx.compressor", "Comp")) }
            .apply(&mut p)
            .unwrap();
        Command::SetSidechain { device: keyed, source: Some(bus) }.apply(&mut p).unwrap();
        // The other track also rides the send-1 level with an automation lane.
        let mut lane = AutomationLane::new(AutomationTarget::Send { index: 1 });
        lane.insert(AutomationPoint { tick: 0, value: 0.5, curve: 0.0 });
        Command::SetAutomation { track: src, target: AutomationTarget::Send { index: 1 }, lane: Some(lane) }.apply(&mut p).unwrap();
        Rig { p, src, bus, ret, vca, group, member_out, member_main, keyed }
    }

    #[test]
    fn removing_a_track_other_tracks_point_at_cleans_them_and_undo_restores_exactly() {
        let r = rig();
        for victim in [r.bus, r.ret, r.vca, r.group] {
            let mut p = r.p.clone();
            let undo = Command::RemoveTrack { id: victim }.apply(&mut p).unwrap();
            assert!(crate::routing::dangling_references(&p).is_empty(), "{victim}: {:?}", crate::routing::dangling_references(&p));
            undo.apply(&mut p).unwrap();
            assert_eq!(p, r.p, "undo of deleting {victim} restores everything");
        }
        // What the cleanup leaves behind.
        let mut p = r.p.clone();
        Command::RemoveTrack { id: r.bus }.apply(&mut p).unwrap();
        assert_eq!(p.track(r.src).unwrap().output, Output::Main, "its output goes to Main");
        assert_eq!(p.track(r.src).unwrap().sends.iter().map(|s| s.target).collect::<Vec<_>>(), vec![r.ret], "the send to it is dropped");
        assert!(p.track(r.src).unwrap().automation.is_empty(), "the lane of the dropped send goes with it");
        assert_eq!(p.device(r.keyed).unwrap().sidechain, None, "the sidechain is disconnected");
        let mut p = r.p.clone();
        Command::RemoveTrack { id: r.vca }.apply(&mut p).unwrap();
        assert_eq!(p.track(r.src).unwrap().vca, None);
        // Deleting a group frees its members to the group's parent, as ungrouping does.
        let mut p = r.p.clone();
        Command::RemoveTrack { id: r.group }.apply(&mut p).unwrap();
        for m in [r.member_out, r.member_main] {
            assert_eq!(p.track(m).unwrap().parent, None);
        }
        assert_eq!(p.track(r.member_out).unwrap().output, Output::Main);
    }

    #[test]
    fn removing_a_send_target_renumbers_the_automation_of_later_sends() {
        let r = rig();
        let mut p = r.p.clone();
        Command::RemoveTrack { id: r.ret }.apply(&mut p).unwrap();
        let src = p.track(r.src).unwrap();
        assert_eq!(src.sends.len(), 1);
        assert_eq!(src.automation.len(), 1);
        assert_eq!(src.automation[0].target, AutomationTarget::Send { index: 0 }, "the bus is send 0 now");
        // A lane on the dropped send goes with it.
        let mut p = r.p.clone();
        let lane = AutomationLane::new(AutomationTarget::Send { index: 0 });
        Command::SetAutomation { track: r.src, target: lane.target, lane: Some(lane) }.apply(&mut p).unwrap();
        let before = p.clone();
        let undo = Command::RemoveTrack { id: r.ret }.apply(&mut p).unwrap();
        assert_eq!(p.track(r.src).unwrap().automation.len(), 1);
        undo.apply(&mut p).unwrap();
        assert_eq!(p, before, "lanes come back in their order");
    }

    #[test]
    fn cleaning_up_never_leaves_a_loop() {
        // group G holds R; M sits in R and R ends in hardware. G feeds Z, Z feeds M.
        // Deleting R frees M into G, where "Main" would mean G: a loop, so M gets no output.
        let mut p = Project::new("t");
        let add = |p: &mut Project, kind| {
            let id = p.alloc_track_id();
            let at = p.tracks.len() - 1;
            Command::AddTrack { track: Box::new(Track::new(id, kind, "t", Color::palette(1))), index: at }.apply(p).unwrap();
            id
        };
        let (g, r, m, z) = (add(&mut p, TrackKind::Group), add(&mut p, TrackKind::Group), add(&mut p, TrackKind::Audio), add(&mut p, TrackKind::Audio));
        let set = |p: &mut Project, id, change| {
            Command::SetTrack { id, change }.apply(p).unwrap();
        };
        set(&mut p, r, TrackProp::Parent(Some(g)));
        set(&mut p, m, TrackProp::Parent(Some(r)));
        set(&mut p, r, TrackProp::Output(Output::Hardware(0)));
        set(&mut p, g, TrackProp::Output(Output::Track(z)));
        set(&mut p, z, TrackProp::Output(Output::Track(m)));
        let before = p.clone();
        let undo = Command::RemoveTrack { id: r }.apply(&mut p).unwrap();
        assert_eq!(p.track(m).unwrap().parent, Some(g));
        assert_eq!(p.track(m).unwrap().output, Output::None);
        assert!(!routing::reaches(&p, m, m));
        undo.apply(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn commands_that_bring_ids_move_the_allocator_past_all_of_them() {
        let fresh = || Project::new("t");
        let nested = |top: u64| {
            let mut inner = Chain::new("A", Color::palette(1));
            inner.devices.push(Device::stock(DeviceId(top), "fx.eq", "EQ"));
            let mut rack = Rack::new(RackKind::Audio);
            rack.chains.push(inner);
            let mut d = Device::stock(DeviceId(top - 1), "rack.audio", "Rack");
            d.kind = DeviceKind::Rack(rack);
            d
        };
        let check = |p: &mut Project, top: u64, what: &str| {
            for id in [p.alloc_device_id().0, p.alloc_track_id().0, p.alloc_placement_id().0, p.alloc_clip_id().0] {
                assert!(id > top, "{what}: next id {id} <= {top}");
            }
        };
        // A track carrying a rack with a nested device, and placements.
        let mut p = fresh();
        let mut t = Track::new(TrackId(100), TrackKind::Audio, "T", Color(0));
        t.devices.push(nested(300));
        t.arrangement.push(Placement { id: PlacementId(400), clip: ClipId(1), start: 0, lane: 0 });
        Command::AddTrack { track: Box::new(t), index: 1 }.apply(&mut p).unwrap();
        check(&mut p, 400, "AddTrack");
        // A device with a nested one.
        let mut p = fresh();
        let track = p.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Audio, "T", Color(0))), index: 1 }.apply(&mut p).unwrap();
        Command::AddDevice { slot: DeviceSlot::Track { track, index: 0 }, device: Box::new(nested(500)) }.apply(&mut p).unwrap();
        check(&mut p, 500, "AddDevice");
        let rack = DeviceId(499);
        // Chains with nested devices, added and updated.
        let mut chain = Chain::new("B", Color(0));
        chain.devices.push(nested(700));
        Command::AddChain { rack, index: 1, chain: Box::new(chain.clone()) }.apply(&mut p).unwrap();
        check(&mut p, 700, "AddChain");
        chain.devices.push(nested(900));
        Command::UpdateChain { rack, index: 1, chain: Box::new(chain) }.apply(&mut p).unwrap();
        check(&mut p, 900, "UpdateChain");
        // Scene, source with notes, clip and placement ids.
        let mut p = fresh();
        Command::AddScene { scene: Scene { id: SceneId(1200), name: "x".into(), tempo: None }, index: 0 }.apply(&mut p).unwrap();
        check(&mut p, 1200, "AddScene");
        Command::AddSource { id: SourceId(1300), source: Box::new(Source::Notes(NoteSource { notes: vec![Note::new(NoteId(1350), 60, 0, 1, 1.0)] })) }.apply(&mut p).unwrap();
        check(&mut p, 1350, "AddSource");
        Command::AddClip { id: ClipId(1400), clip: Box::new(Clip::new("c", Color(0), SourceId(1300), 10)) }.apply(&mut p).unwrap();
        check(&mut p, 1400, "AddClip");
        let track = p.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Audio, "T", Color(0))), index: 1 }.apply(&mut p).unwrap();
        Command::PlaceClip { track, placement: Placement { id: PlacementId(1500), clip: ClipId(1400), start: 0, lane: 0 } }.apply(&mut p).unwrap();
        check(&mut p, 1500, "PlaceClip");
        Command::AddNotes { source: SourceId(1300), notes: vec![Note::new(NoteId(1600), 61, 0, 1, 1.0)] }.apply(&mut p).unwrap();
        check(&mut p, 1600, "AddNotes");
        // Inside a batch, and a project opened from a file whose allocator is behind.
        let mut p = fresh();
        let mut t = Track::new(TrackId(2000), TrackKind::Audio, "T", Color(0));
        t.devices.push(nested(2100));
        Command::Batch { commands: vec![Command::AddTrack { track: Box::new(t), index: 1 }] }.apply(&mut p).unwrap();
        check(&mut p, 2100, "Batch");
        let mut stale: serde_json::Value = serde_json::from_str(&p.to_json().unwrap()).unwrap();
        stale["ids"] = serde_json::json!({ "next": 3 });
        let mut opened = Project::open_json(&stale.to_string()).unwrap();
        check(&mut opened, 2100, "open");
    }

    /// Delete and undo must give back the very same project, lane order included.
    fn removal_round_trips(p: &Project, victims: &[TrackId]) {
        for &victim in victims {
            let mut q = p.clone();
            let undo = Command::RemoveTrack { id: victim }.apply(&mut q).unwrap();
            assert!(crate::routing::dangling_references(&q).is_empty(), "{victim}: {:?}", crate::routing::dangling_references(&q));
            undo.apply(&mut q).unwrap();
            assert_eq!(&q, p, "undo of deleting {victim} is not exact");
        }
    }

    #[test]
    fn undoing_a_delete_restores_the_automation_lane_order() {
        let mut r = rig();
        let more = {
            let id = r.p.alloc_track_id();
            let at = r.p.tracks.len() - 1;
            Command::AddTrack { track: Box::new(Track::new(id, TrackKind::Return, "ret2", Color::palette(2))), index: at }.apply(&mut r.p).unwrap();
            id
        };
        let lane = |target| {
            let mut l = AutomationLane::new(target);
            l.insert(AutomationPoint { tick: 0, value: 0.3, curve: 0.0 });
            l
        };
        // sends [ret, bus, ret2]; lanes in a deliberate order with sends in between.
        let src = r.src;
        Command::SetTrack { id: src, change: TrackProp::Sends(vec![
            Send { target: r.ret, level_db: -6.0, pre_fader: false },
            Send { target: r.bus, level_db: -9.0, pre_fader: true },
            Send { target: more, level_db: -3.0, pre_fader: false },
        ]) }.apply(&mut r.p).unwrap();
        let t = r.p.track_mut(src).unwrap();
        t.automation = [AutomationTarget::Send { index: 2 }, AutomationTarget::Volume, AutomationTarget::Send { index: 1 }, AutomationTarget::Pan, AutomationTarget::Send { index: 0 }, AutomationTarget::Mute]
            .into_iter()
            .map(lane)
            .collect();
        removal_round_trips(&r.p, &[r.ret, r.bus, more]);
        // The shape after deleting the first send's target: renumbered, order otherwise kept.
        let mut q = r.p.clone();
        Command::RemoveTrack { id: r.ret }.apply(&mut q).unwrap();
        let targets: Vec<AutomationTarget> = q.track(src).unwrap().automation.iter().map(|l| l.target).collect();
        assert_eq!(targets, [AutomationTarget::Send { index: 1 }, AutomationTarget::Volume, AutomationTarget::Send { index: 0 }, AutomationTarget::Pan, AutomationTarget::Mute]);
        // The case reported: [Send1, Volume] and the return of send 0.
        let mut s = rig();
        let t = s.p.track_mut(s.src).unwrap();
        t.automation = [AutomationTarget::Send { index: 1 }, AutomationTarget::Volume].into_iter().map(lane).collect();
        removal_round_trips(&s.p, &[s.ret]);
        // Through history, with redo.
        let mut p = s.p.clone();
        let mut h = crate::History::default();
        h.apply(&mut p, "Delete", crate::Origin::User, vec![Command::RemoveTrack { id: s.ret }]).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p, s.p);
        h.redo(&mut p).unwrap();
        assert!(p.track(s.ret).is_none());
    }

    #[test]
    fn undoing_a_delete_is_exact_for_nested_groups() {
        let mut p = Project::new("t");
        let add = |p: &mut Project, kind, name: &str| {
            let id = p.alloc_track_id();
            let at = p.tracks.len() - 1;
            Command::AddTrack { track: Box::new(Track::new(id, kind, name, Color::palette(1))), index: at }.apply(p).unwrap();
            id
        };
        let (outer, inner, a, b, c, ret, side) = (
            add(&mut p, TrackKind::Group, "outer"),
            add(&mut p, TrackKind::Group, "inner"),
            add(&mut p, TrackKind::Audio, "a"),
            add(&mut p, TrackKind::Audio, "b"),
            add(&mut p, TrackKind::Audio, "c"),
            add(&mut p, TrackKind::Return, "ret"),
            add(&mut p, TrackKind::Audio, "side"),
        );
        let set = |p: &mut Project, id, change| {
            Command::SetTrack { id, change }.apply(p).unwrap();
        };
        set(&mut p, inner, TrackProp::Parent(Some(outer)));
        set(&mut p, inner, TrackProp::Output(Output::Track(outer)));
        for (m, out) in [(a, Output::Track(inner)), (b, Output::Main), (c, Output::Hardware(1))] {
            set(&mut p, m, TrackProp::Parent(Some(inner)));
            set(&mut p, m, TrackProp::Output(out));
        }
        set(&mut p, a, TrackProp::Sends(vec![Send { target: inner, level_db: -3.0, pre_fader: false }, Send { target: ret, level_db: -6.0, pre_fader: false }]));
        set(&mut p, b, TrackProp::Sends(vec![Send { target: ret, level_db: -1.0, pre_fader: true }, Send { target: outer, level_db: -2.0, pre_fader: false }]));
        for (track, lanes) in [
            (a, vec![AutomationTarget::Volume, AutomationTarget::Send { index: 1 }, AutomationTarget::Send { index: 0 }, AutomationTarget::Pan]),
            (b, vec![AutomationTarget::Send { index: 1 }, AutomationTarget::Mute, AutomationTarget::Send { index: 0 }]),
        ] {
            p.track_mut(track).unwrap().automation = lanes
                .into_iter()
                .map(|t| {
                    let mut l = AutomationLane::new(t);
                    l.insert(AutomationPoint { tick: 0, value: 0.5, curve: 0.0 });
                    l
                })
                .collect();
        }
        let d = p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Track { track: side, index: 0 }, device: Box::new(Device::stock(d, "fx.compressor", "Comp")) }.apply(&mut p).unwrap();
        Command::SetSidechain { device: d, source: Some(inner) }.apply(&mut p).unwrap();
        set(&mut p, c, TrackProp::Vca(None));
        removal_round_trips(&p, &[inner, outer, ret, a, b, side]);
        // Deleting the outer group and then the inner one, undone in reverse, comes back whole.
        let mut q = p.clone();
        let mut h = crate::History::default();
        h.apply(&mut q, "1", crate::Origin::User, vec![Command::RemoveTrack { id: outer }]).unwrap();
        h.apply(&mut q, "2", crate::Origin::User, vec![Command::RemoveTrack { id: inner }]).unwrap();
        assert!(crate::routing::dangling_references(&q).is_empty());
        h.undo(&mut q).unwrap();
        h.undo(&mut q).unwrap();
        assert_eq!(q, p);
    }

    // ── granular rack commands ──

    struct RackRig {
        p: Project,
        track: TrackId,
        rack: DeviceId,
        eq: DeviceId,
        comp: DeviceId,
    }

    /// A drum rack with two pads (one holding an EQ, one a compressor), a
    /// macro mapped to the EQ, and two internal returns.
    fn rack_rig() -> RackRig {
        let mut p = Project::new("t");
        let track = p.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Instrument, "Drums", Color::palette(1))), index: 1 }.apply(&mut p).unwrap();
        let (rack, eq, comp) = (p.alloc_device_id(), p.alloc_device_id(), p.alloc_device_id());
        let mut r = Rack::new(RackKind::Drum);
        for (name, note, dev) in [("Kick", 36, Device::stock(eq, "fx.eq", "EQ")), ("Snare", 38, Device::stock(comp, "fx.compressor", "Comp"))] {
            let mut c = Chain::new(name, Color::palette(2));
            c.note = Some(note);
            c.devices.push(dev);
            r.chains.push(c);
        }
        r.macros[0].mappings.push(MacroMapping { device: eq, param: 1, min: 0.0, max: 1.0 });
        let mut d = Device::stock(rack, "rack.drum", "Drum Rack");
        d.kind = DeviceKind::Rack(r);
        Command::AddDevice { slot: DeviceSlot::Track { track, index: 0 }, device: Box::new(d) }.apply(&mut p).unwrap();
        for (i, name) in ["Reverb", "Delay"].into_iter().enumerate() {
            Command::AddReturn { rack, index: i, chain: Box::new(Chain::new(name, Color::palette(4))) }.apply(&mut p).unwrap();
        }
        RackRig { p, track, rack, eq, comp }
    }

    fn rack_of(p: &Project, rack: DeviceId) -> &Rack {
        let Some(Device { kind: DeviceKind::Rack(r), .. }) = p.device(rack) else { panic!("rack") };
        r
    }

    /// Apply, check it changed, apply the inverse and check the project is identical.
    fn round_trip(p: &Project, cmd: Command) -> Project {
        let mut q = p.clone();
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd, "travels as JSON");
        let inverse = cmd.clone().apply(&mut q).unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
        assert_ne!(&q, p, "{cmd:?} changed nothing");
        let after = q.clone();
        inverse.apply(&mut q).unwrap();
        q.ids = p.ids.clone(); // the allocator only ever moves up
        assert_eq!(&q, p, "inverse of {cmd:?}");
        after
    }

    #[test]
    fn every_chain_property_applies_and_undoes_exactly() {
        let r = rack_rig();
        let set = |chain, prop| Command::SetChainProp { rack: r.rack, chain, prop };
        let chain0 = |p: &Project| rack_of(p, r.rack).chains[0].clone();
        let q = round_trip(&r.p, set(0, ChainProp::Name("Kick 2".into())));
        assert_eq!(chain0(&q).name, "Kick 2");
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::Color(Color(7))))).color, Color(7));
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::VolumeDb(-6.0)))).volume_db, -6.0);
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::VolumeDb(99.0)))).volume_db, 12.0, "clamped like a track fader");
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::Pan(-3.0)))).pan, -1.0);
        assert!(chain0(&round_trip(&r.p, set(0, ChainProp::Mute(true)))).mute);
        assert!(chain0(&round_trip(&r.p, set(0, ChainProp::Solo(true)))).solo);
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::KeyRange((60, 72))))).key_range, (60, 72));
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::KeyRange((90, 20))))).key_range, (20, 90), "low end first");
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::KeyRange((10, 200))))).key_range, (10, 127));
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::VelRange((10, 100))))).velocity_range, (10, 100));
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::SelRange((5, 50))))).selector_range, (5, 50));
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::KeyFade((4, 8))))).key_fade, (4, 8));
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::VelFade((1, 2))))).vel_fade, (1, 2));
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::SelFade((9, 9))))).sel_fade, (9, 9));
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::Choke(3)))).choke, 3);
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::Choke(99)))).choke, 16);
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::ReceiveNote(Some(40))))).note, Some(40));
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::ReceiveNote(None)))).note, None);
        assert_eq!(chain0(&round_trip(&r.p, set(0, ChainProp::PlayNote(Some(60))))).play_note, Some(60));
        // Sends to the rack's internal returns, one by one or all together.
        let q = round_trip(&r.p, set(1, ChainProp::Send(1, -12.0)));
        assert_eq!(rack_of(&q, r.rack).chains[1].sends, vec![SEND_OFF_DB, -12.0], "the send before it stays off");
        let q = round_trip(&r.p, set(1, ChainProp::Sends(vec![-3.0, -6.0])));
        assert_eq!(rack_of(&q, r.rack).chains[1].sends, vec![-3.0, -6.0]);
        assert_eq!(rack_of(&round_trip(&r.p, set(1, ChainProp::Send(0, 40.0))).clone(), r.rack).chains[1].sends[0], SEND_MAX_DB);
        assert_eq!(rack_of(&round_trip(&r.p, set(1, ChainProp::Output(ChainOut::Return(0)))), r.rack).chains[1].output, ChainOut::Return(0));
        // A chain edited twice goes back through both steps.
        let mut q = q;
        let first = q.clone();
        let undo = set(1, ChainProp::Send(0, 0.0)).apply(&mut q).unwrap();
        assert_ne!(q, first);
        undo.apply(&mut q).unwrap();
        assert_eq!(q, first);
        // Return chains take the same properties.
        let ret = Command::SetReturnProp { rack: r.rack, index: 1, prop: ChainProp::VolumeDb(-9.0) };
        let q = round_trip(&r.p, ret);
        assert_eq!(rack_of(&q, r.rack).returns[1].volume_db, -9.0);
        // Nothing touches the chain's devices, or the other chain.
        assert_eq!(rack_of(&q, r.rack).chains, rack_of(&r.p, r.rack).chains);
    }

    #[test]
    fn chain_commands_reject_what_does_not_exist() {
        let mut r = rack_rig();
        let rack = r.rack;
        let err = |p: &mut Project, c: Command| c.apply(p).unwrap_err();
        assert_eq!(err(&mut r.p, Command::SetChainProp { rack, chain: 5, prop: ChainProp::Mute(true) }), CommandError::OutOfRange(5));
        assert_eq!(err(&mut r.p, Command::SetChainProp { rack: r.eq, chain: 0, prop: ChainProp::Mute(true) }), CommandError::NotARack(r.eq));
        assert_eq!(err(&mut r.p, Command::SetChainProp { rack: DeviceId(999), chain: 0, prop: ChainProp::Mute(true) }), CommandError::DeviceNotFound(DeviceId(999)));
        assert_eq!(err(&mut r.p, Command::SetReturnProp { rack, index: 2, prop: ChainProp::Mute(true) }), CommandError::OutOfRange(2));
        // Only two returns: no third send, no output to a missing return.
        assert!(matches!(err(&mut r.p, Command::SetChainProp { rack, chain: 0, prop: ChainProp::Send(2, 0.0) }), CommandError::Invalid(_)));
        assert!(matches!(err(&mut r.p, Command::SetChainProp { rack, chain: 0, prop: ChainProp::Sends(vec![0.0; 3]) }), CommandError::Invalid(_)));
        assert!(matches!(err(&mut r.p, Command::SetChainProp { rack, chain: 0, prop: ChainProp::Output(ChainOut::Return(2)) }), CommandError::Invalid(_)));
    }

    #[test]
    fn returns_are_added_removed_and_the_chains_that_used_them_are_cleaned() {
        let mut r = rack_rig();
        let rack = r.rack;
        for cmd in [
            Command::SetChainProp { rack, chain: 0, prop: ChainProp::Sends(vec![-3.0, -9.0]) },
            Command::SetChainProp { rack, chain: 1, prop: ChainProp::Output(ChainOut::Return(1)) },
            Command::SetChainProp { rack, chain: 0, prop: ChainProp::Output(ChainOut::Return(0)) },
        ] {
            cmd.apply(&mut r.p).unwrap();
        }
        // Removing the first return drops its send and renumbers the rest.
        let mut q = r.p.clone();
        let undo = Command::RemoveReturn { rack, index: 0 }.apply(&mut q).unwrap();
        let rr = rack_of(&q, rack);
        assert_eq!(rr.returns.len(), 1);
        assert_eq!(rr.returns[0].name, "Delay");
        assert_eq!(rr.chains[0].sends, vec![-9.0]);
        assert_eq!(rr.chains[0].output, ChainOut::Rack, "its output went with the return");
        assert_eq!(rr.chains[1].output, ChainOut::Return(0), "the other one follows its return");
        undo.apply(&mut q).unwrap();
        assert_eq!(q, r.p, "undo restores sends, outputs and the return");
        // Adding in the middle shifts the sends and outputs after it, and undoes.
        let mut chain = Chain::new("Chorus", Color(1));
        chain.devices.push(Device::stock(DeviceId(700), "fx.chorus", "Chorus"));
        let add = Command::AddReturn { rack, index: 1, chain: Box::new(chain) };
        let q = round_trip(&r.p, add);
        assert_eq!(rack_of(&q, rack).returns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Reverb", "Chorus", "Delay"]);
        let mut q = q;
        assert!(q.alloc_device_id().0 > 700, "ids inside the added return are accounted for");
        // Six at most; only a Drum Rack has them; the destructive one is flagged.
        let mut p = r.p.clone();
        for i in 2..6 {
            Command::AddReturn { rack, index: i, chain: Box::new(Chain::new("R", Color(1))) }.apply(&mut p).unwrap();
        }
        let e = Command::AddReturn { rack, index: 0, chain: Box::new(Chain::new("R", Color(1))) }.apply(&mut p).unwrap_err();
        assert!(matches!(e, CommandError::Invalid(_)));
        let audio = p.alloc_device_id();
        let mut ar = Device::stock(audio, "rack.audio", "Rack");
        ar.kind = DeviceKind::Rack(Rack::new(RackKind::Audio));
        Command::AddDevice { slot: DeviceSlot::Track { track: r.track, index: 1 }, device: Box::new(ar) }.apply(&mut p).unwrap();
        let e = Command::AddReturn { rack: audio, index: 0, chain: Box::new(Chain::new("R", Color(1))) }.apply(&mut p).unwrap_err();
        assert!(matches!(e, CommandError::Invalid(_)));
        assert!(Command::RemoveReturn { rack, index: 0 }.is_destructive());
        assert_eq!(Command::RemoveReturn { rack, index: 9 }.apply(&mut p), Err(CommandError::OutOfRange(9)));
    }

    #[test]
    fn mappings_are_added_removed_and_ranged_one_at_a_time() {
        let r = rack_rig();
        let rack = r.rack;
        let mapping = MacroMapping { device: r.comp, param: 4, min: 0.2, max: 0.8 };
        let q = round_trip(&r.p, Command::AddMapping { rack, macro_index: 0, index: 0, mapping: mapping.clone() });
        let m = &rack_of(&q, rack).macros[0].mappings;
        assert_eq!((m.len(), m[0].clone(), m[1].device), (2, mapping.clone(), r.eq), "inserted where asked");
        // Append, remove the first one, and give back the one that was removed.
        let appended = round_trip(&r.p, Command::AddMapping { rack, macro_index: 0, index: 99, mapping: mapping.clone() });
        assert_eq!(rack_of(&appended, rack).macros[0].mappings[1], mapping);
        let removed = round_trip(&r.p, Command::RemoveMapping { rack, macro_index: 0, index: 0 });
        assert!(rack_of(&removed, rack).macros[0].mappings.is_empty());
        // The range, and inverting by swapping the ends.
        let ranged = round_trip(&r.p, Command::SetMappingRange { rack, macro_index: 0, index: 0, min: 0.25, max: 0.75 });
        let m = &rack_of(&ranged, rack).macros[0].mappings[0];
        assert_eq!((m.min, m.max), (0.25, 0.75));
        assert!(!m.is_inverted());
        let inverted = round_trip(&ranged, Command::SetMappingRange { rack, macro_index: 0, index: 0, min: 0.75, max: 0.25 });
        assert!(rack_of(&inverted, rack).macros[0].mappings[0].is_inverted());
        let clamped = round_trip(&r.p, Command::SetMappingRange { rack, macro_index: 0, index: 0, min: -1.0, max: 0.5 });
        let m = &rack_of(&clamped, rack).macros[0].mappings[0];
        assert_eq!((m.min, m.max), (0.0, 0.5));
        // The macro value and the other macros are left alone.
        assert_eq!(rack_of(&ranged, rack).macros[1..], rack_of(&r.p, rack).macros[1..]);
        assert_eq!(rack_of(&ranged, rack).macros[0].value, rack_of(&r.p, rack).macros[0].value);
    }

    #[test]
    fn modulator_mappings_are_commands_with_undo() {
        let mut r = rack_rig();
        let m = r.p.alloc_device_id();
        let (other_track, foreign) = (r.p.alloc_track_id(), r.p.alloc_device_id());
        Command::AddTrack { track: Box::new(Track::new(other_track, TrackKind::Audio, "B", Color::palette(2))), index: 2 }.apply(&mut r.p).unwrap();
        Command::AddDevice { slot: DeviceSlot::Track { track: other_track, index: 0 }, device: Box::new(Device::stock(foreign, "fx.eq", "EQ")) }.apply(&mut r.p).unwrap();
        Command::AddDevice { slot: DeviceSlot::Track { track: r.track, index: 1 }, device: Box::new(Device::stock(m, "mod.lfo", "LFO")) }.apply(&mut r.p).unwrap();
        let on = |t: ModTarget| ModMapping { target: t, min: -0.25, max: 0.25 };
        let eq1 = ModTarget::Param { device: r.eq, param: 1 };
        let q = round_trip(&r.p, Command::AddModMapping { device: m, index: 0, mapping: on(eq1) });
        assert_eq!(q.device(m).unwrap().mods, vec![on(eq1)]);
        // Volume and pan are targets too; a device inside a rack on the same track counts.
        let mut q = q;
        Command::AddModMapping { device: m, index: 9, mapping: on(ModTarget::Volume) }.apply(&mut q).unwrap();
        let q2 = round_trip(&q, Command::SetModRange { device: m, index: 1, min: 2.0, max: -0.5 });
        assert_eq!((q2.device(m).unwrap().mods[1].min, q2.device(m).unwrap().mods[1].max), (1.0, -0.5));
        let q3 = round_trip(&q, Command::RemoveModMapping { device: m, index: 0 });
        assert_eq!(q3.device(m).unwrap().mods.len(), 1);
        assert!(Command::RemoveModMapping { device: m, index: 0 }.is_destructive());
        let err = |p: &mut Project, c: Command| c.apply(p).unwrap_err();
        assert!(matches!(err(&mut q, Command::AddModMapping { device: m, index: 0, mapping: on(eq1) }), CommandError::Invalid(_)), "twice");
        assert!(matches!(err(&mut q, Command::AddModMapping { device: m, index: 0, mapping: on(ModTarget::Param { device: foreign, param: 0 }) }), CommandError::Invalid(_)), "other track");
        assert!(matches!(err(&mut q, Command::AddModMapping { device: m, index: 0, mapping: on(ModTarget::Param { device: m, param: 0 }) }), CommandError::Invalid(_)), "itself");
        assert!(matches!(err(&mut q, Command::AddModMapping { device: r.eq, index: 0, mapping: on(ModTarget::Pan) }), CommandError::Invalid(_)), "not a modulator");
        assert_eq!(err(&mut q, Command::SetModRange { device: m, index: 5, min: 0.0, max: 1.0 }), CommandError::OutOfRange(5));
        // At most eight targets.
        let mut full = r.p.clone();
        for param in 0..MAX_MOD_MAPPINGS as u32 {
            Command::AddModMapping { device: m, index: 0, mapping: on(ModTarget::Param { device: r.eq, param }) }.apply(&mut full).unwrap();
        }
        assert!(matches!(err(&mut full, Command::AddModMapping { device: m, index: 0, mapping: on(ModTarget::Pan) }), CommandError::Invalid(_)));
        // They survive a save / load.
        let json = serde_json::to_string(&q).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap().device(m).unwrap().mods, q.device(m).unwrap().mods);
    }

    #[test]
    fn removing_or_moving_a_modulated_device_drops_its_modulation_and_undo_restores_it() {
        let mut r = rack_rig();
        let (m, other_track) = (r.p.alloc_device_id(), r.p.alloc_track_id());
        Command::AddTrack { track: Box::new(Track::new(other_track, TrackKind::Audio, "B", Color::palette(2))), index: 2 }.apply(&mut r.p).unwrap();
        Command::AddDevice { slot: DeviceSlot::Track { track: r.track, index: 1 }, device: Box::new(Device::stock(m, "mod.lfo", "LFO")) }.apply(&mut r.p).unwrap();
        let on = |t: ModTarget| ModMapping { target: t, min: -0.25, max: 0.25 };
        for t in [ModTarget::Pan, ModTarget::Param { device: r.eq, param: 1 }, ModTarget::Param { device: r.eq, param: 2 }, ModTarget::Volume] {
            Command::AddModMapping { device: m, index: 9, mapping: on(t) }.apply(&mut r.p).unwrap();
        }
        let before = r.p.clone();
        let check = |p: &Project, want: usize| {
            let mods = &p.device(m).unwrap().mods;
            assert_eq!(mods.len(), want, "{mods:?}");
            assert!(mods.iter().all(|x| !matches!(x.target, ModTarget::Param { device, .. } if p.device_track(device) != p.device_track(m))));
        };
        // Removing the target drops the mappings to it, and only those.
        let mut p = r.p.clone();
        let undo = Command::RemoveDevice { id: r.eq }.apply(&mut p).unwrap();
        check(&p, 2);
        undo.apply(&mut p).unwrap();
        assert_eq!(p, before, "undo restores the mappings in order");
        // Moving the target to another track leaves the modulator without it.
        let undo = Command::MoveDevice { id: r.eq, to: DeviceSlot::Track { track: other_track, index: 0 } }.apply(&mut p).unwrap();
        check(&p, 2);
        undo.apply(&mut p).unwrap();
        assert_eq!(p, before);
        // Moving the modulator away takes its parameter targets out but keeps the fader ones.
        let undo = Command::MoveDevice { id: m, to: DeviceSlot::Track { track: other_track, index: 0 } }.apply(&mut p).unwrap();
        check(&p, 2);
        undo.apply(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn placements_cannot_land_on_a_take_lane_the_track_does_not_have() {
        let (mut p, a) = project_with_track();
        let b = p.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(b, TrackKind::Instrument, "B", Color::palette(1))), index: 2 }.apply(&mut p).unwrap();
        Command::AddTakeLane { track: a, index: 0, name: "Take 1".into() }.apply(&mut p).unwrap();
        let (src, clip) = (p.alloc_source_id(), p.alloc_clip_id());
        Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource::default())) }.apply(&mut p).unwrap();
        Command::AddClip { id: clip, clip: Box::new(Clip::new("c", Color(0), src, PPQ)) }.apply(&mut p).unwrap();
        let lane_of = |p: &Project, t: TrackId, id: PlacementId| p.track(t).unwrap().arrangement.iter().find(|x| x.id == id).map(|x| x.lane);
        let (ok, bad) = (p.alloc_placement_id(), p.alloc_placement_id());
        Command::PlaceClip { track: a, placement: Placement { id: ok, clip, start: 0, lane: 1 } }.apply(&mut p).unwrap();
        Command::PlaceClip { track: a, placement: Placement { id: bad, clip, start: PPQ, lane: 2 } }.apply(&mut p).unwrap();
        assert_eq!((lane_of(&p, a, ok), lane_of(&p, a, bad)), (Some(1), Some(0)), "lane 2 does not exist; lane 1 does");
        // Moving to a track without take lanes puts the clip in its main lane; undo gives the lane back.
        let before = p.clone();
        let undo = Command::MovePlacement { track: a, id: ok, to_track: b, start: 0, lane: 1 }.apply(&mut p).unwrap();
        assert_eq!(lane_of(&p, b, ok), Some(0));
        undo.apply(&mut p).unwrap();
        assert_eq!(p, before);
        Command::MovePlacement { track: a, id: ok, to_track: a, start: 0, lane: 7 }.apply(&mut p).unwrap();
        assert_eq!(lane_of(&p, a, ok), Some(0));
    }

    #[test]
    fn mapping_commands_reject_bad_targets() {
        let mut r = rack_rig();
        let rack = r.rack;
        let ok = MacroMapping { device: r.comp, param: 4, min: 0.0, max: 1.0 };
        let err = |p: &mut Project, c: Command| c.apply(p).unwrap_err();
        assert_eq!(err(&mut r.p, Command::AddMapping { rack, macro_index: 16, index: 0, mapping: ok.clone() }), CommandError::OutOfRange(16));
        assert_eq!(err(&mut r.p, Command::RemoveMapping { rack, macro_index: 0, index: 3 }), CommandError::OutOfRange(3));
        assert_eq!(err(&mut r.p, Command::SetMappingRange { rack, macro_index: 0, index: 3, min: 0.0, max: 1.0 }), CommandError::OutOfRange(3));
        assert_eq!(err(&mut r.p, Command::AddMapping { rack, macro_index: 0, index: 0, mapping: MacroMapping { device: DeviceId(999), ..ok.clone() } }), CommandError::DeviceNotFound(DeviceId(999)));
        // A device outside the rack cannot be driven by it.
        let outside = r.p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Track { track: r.track, index: 1 }, device: Box::new(Device::stock(outside, "fx.eq", "EQ")) }.apply(&mut r.p).unwrap();
        assert!(matches!(err(&mut r.p, Command::AddMapping { rack, macro_index: 0, index: 0, mapping: MacroMapping { device: outside, ..ok.clone() } }), CommandError::Invalid(_)));
        // The same parameter twice on one macro is refused; on another macro it is fine.
        let again = MacroMapping { device: r.eq, param: 1, min: 0.0, max: 0.5 };
        assert!(matches!(err(&mut r.p, Command::AddMapping { rack, macro_index: 0, index: 0, mapping: again.clone() }), CommandError::Invalid(_)));
        Command::AddMapping { rack, macro_index: 1, index: 0, mapping: again }.apply(&mut r.p).unwrap();
    }

    #[test]
    fn rack_command_drags_coalesce_into_one_step() {
        let r = rack_rig();
        let mut p = r.p.clone();
        let mut h = crate::History::default();
        for v in [-1.0, -2.0, -3.0] {
            h.apply_coalesced(&mut p, "chain-vol", "Chain volume", vec![Command::SetChainProp { rack: r.rack, chain: 0, prop: ChainProp::VolumeDb(v) }]).unwrap();
        }
        for v in [0.3, 0.4] {
            h.apply_coalesced(&mut p, "map-range", "Map macro", vec![Command::SetMappingRange { rack: r.rack, macro_index: 0, index: 0, min: v, max: 1.0 }]).unwrap();
        }
        assert_eq!(h.steps().count(), 2, "one step per drag");
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p, r.p);
        h.redo(&mut p).unwrap();
        h.redo(&mut p).unwrap();
        assert_eq!(rack_of(&p, r.rack).chains[0].volume_db, -3.0);
        assert_eq!(rack_of(&p, r.rack).macros[0].mappings[0].min, 0.4);
    }

    #[test]
    fn projects_saved_before_these_fields_still_open() {
        let r = rack_rig();
        let mut value: serde_json::Value = serde_json::from_str(&r.p.to_json().unwrap()).unwrap();
        fn strip(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Object(o) => {
                    if o.contains_key("key_range") {
                        for k in ["key_fade", "vel_fade", "sel_fade", "choke", "play_note", "sends", "output"] {
                            o.remove(k);
                        }
                    }
                    o.remove("returns");
                    o.values_mut().for_each(strip);
                }
                serde_json::Value::Array(a) => a.iter_mut().for_each(strip),
                _ => {}
            }
        }
        // Tracks have their own "sends" and "output": only chains are stripped, so work on the racks.
        let racks: Vec<&mut serde_json::Value> = value["tracks"].as_array_mut().unwrap().iter_mut().flat_map(|t| t["devices"].as_array_mut().unwrap().iter_mut()).collect();
        for d in racks {
            strip(d);
        }
        let old = Project::open_json(&value.to_string()).unwrap();
        let rr = rack_of(&old, r.rack);
        assert!(rr.returns.is_empty());
        assert_eq!(rr.chains[0].name, "Kick");
        assert_eq!((rr.chains[0].key_fade, rr.chains[0].choke, rr.chains[0].play_note, rr.chains[0].output), ((0, 0), 0, None, ChainOut::Rack));
        assert!(rr.chains[0].sends.is_empty());
    }

    // ── devices inside a Drum Rack's returns, and the rack selector ──

    fn ret_slot(r: &RackRig, index: usize, pos: usize) -> DeviceSlot {
        DeviceSlot::Return { rack: r.rack, index, pos }
    }

    /// Run `cmds` one after the other; each inverse must restore the project exactly.
    fn undo_each(p: &Project, cmds: Vec<Command>) -> Project {
        let mut q = p.clone();
        let mut steps = Vec::new();
        for c in cmds {
            let before = q.clone();
            let inv = c.apply(&mut q).unwrap();
            steps.push((before, inv));
        }
        let end = q.clone();
        for (before, inv) in steps.into_iter().rev() {
            inv.apply(&mut q).unwrap();
            q.ids = before.ids.clone();
            assert_eq!(q, before);
        }
        end
    }

    #[test]
    fn devices_are_added_moved_and_removed_inside_a_return() {
        let mut r = rack_rig();
        let (fx, fx2) = (r.p.alloc_device_id(), r.p.alloc_device_id());
        let add = |id, slot| Command::AddDevice { slot, device: Box::new(Device::stock(id, "fx.reverb", "Verb")) };
        let end = undo_each(
            &r.p,
            vec![
                add(fx, ret_slot(&r, 0, 0)),
                add(fx2, ret_slot(&r, 0, 1)),
                // From a pad's chain into the second return, then within it, then back to a pad.
                Command::MoveDevice { id: r.eq, to: ret_slot(&r, 1, 0) },
                Command::MoveDevice { id: fx, to: ret_slot(&r, 0, 1) },
                Command::MoveDevice { id: fx2, to: DeviceSlot::Rack { rack: r.rack, chain: 1, index: 0 } },
                Command::RemoveDevice { id: r.eq },
            ],
        );
        let rr = rack_of(&end, r.rack);
        assert_eq!(rr.returns[0].devices.iter().map(|d| d.id).collect::<Vec<_>>(), vec![fx]);
        assert!(rr.returns[1].devices.is_empty());
        assert_eq!(rr.chains[1].devices.iter().map(|d| d.id).collect::<Vec<_>>(), vec![fx2, r.comp]);
        // A return that is not there, or a position past the end (clamped like a chain).
        assert_eq!(add(fx, ret_slot(&r, 2, 0)).apply(&mut r.p), Err(CommandError::OutOfRange(2)));
        let id = r.p.alloc_device_id();
        add(id, ret_slot(&r, 0, 99)).apply(&mut r.p).unwrap();
        assert_eq!(rack_of(&r.p, r.rack).returns[0].devices[0].id, id);
        // A failed move puts the device back where it was.
        let before = r.p.clone();
        assert!(Command::MoveDevice { id, to: ret_slot(&r, 5, 0) }.apply(&mut r.p).is_err());
        assert_eq!(r.p, before);
    }

    #[test]
    fn return_devices_are_found_like_any_other() {
        let mut r = rack_rig();
        let fx = r.p.alloc_device_id();
        Command::AddDevice { slot: ret_slot(&r, 1, 0), device: Box::new(Device::stock(fx, "fx.delay", "Delay")) }.apply(&mut r.p).unwrap();
        assert_eq!(r.p.device(fx).map(|d| d.name.as_str()), Some("Delay"));
        assert_eq!(r.p.device_track(fx), Some(r.track));
        r.p.device_mut(fx).unwrap().enabled = false;
        assert_eq!(crate::racks::device_slot(&r.p, fx), Some(ret_slot(&r, 1, 0)));
        assert_eq!(crate::racks::Container::of(ret_slot(&r, 1, 0)), crate::racks::Container::Return { rack: r.rack, index: 1 });
        assert_eq!(crate::racks::Container::Return { rack: r.rack, index: 1 }.slot(3), ret_slot(&r, 1, 3));
        assert_eq!(crate::racks::chain(&r.p, crate::racks::Container::Return { rack: r.rack, index: 1 }).map(|c| c.len()), Some(1));
        let mut seen = Vec::new();
        visit_devices(&r.p.track(r.track).unwrap().devices, &mut |d| seen.push(d.id));
        assert!(seen.contains(&fx));
        // The slot is plain data.
        let slot = ret_slot(&r, 1, 0);
        assert_eq!(serde_json::from_str::<DeviceSlot>(&serde_json::to_string(&slot).unwrap()).unwrap(), slot);
        // A reopened project keeps its allocator above ids that only live in returns.
        let mut value: serde_json::Value = serde_json::from_str(&r.p.to_json().unwrap()).unwrap();
        value["ids"] = serde_json::json!({ "next": 3 });
        let mut q = Project::open_json(&value.to_string()).unwrap();
        assert!(q.alloc_device_id().0 > fx.0);
        // Deleting a track that a return device listens to disconnects it, and undo reconnects it.
        let src = q.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(src, TrackKind::Audio, "Kick", Color(0))), index: 1 }.apply(&mut q).unwrap();
        Command::SetSidechain { device: fx, source: Some(src) }.apply(&mut q).unwrap();
        let before = q.clone();
        let undo = Command::RemoveTrack { id: src }.apply(&mut q).unwrap();
        assert_eq!(q.device(fx).unwrap().sidechain, None);
        undo.apply(&mut q).unwrap();
        assert_eq!(q, before);
        // Macros can drive a return device, and the automation chooser lists it.
        Command::AddMapping { rack: r.rack, macro_index: 2, index: 0, mapping: MacroMapping { device: fx, param: 0, min: 0.0, max: 1.0 } }.apply(&mut q).unwrap();
        let listed: Vec<DeviceId> = crate::automation::chain_devices(&q.track(r.track).unwrap().devices).iter().map(|(d, _)| d.id).collect();
        assert!(listed.contains(&fx));
    }

    #[test]
    fn the_rack_selector_is_one_absolute_setter() {
        let r = rack_rig();
        let rack = r.rack;
        let q = round_trip(&r.p, Command::SetRackSelector { rack, value: 64 });
        assert_eq!(rack_of(&q, rack).chain_selector, 64);
        assert_eq!(rack_of(&round_trip(&r.p, Command::SetRackSelector { rack, value: 200 }), rack).chain_selector, 127);
        assert_eq!(Command::SetRackSelector { rack: r.eq, value: 1 }.clone().apply(&mut r.p.clone()), Err(CommandError::NotARack(r.eq)));
        let mut p = r.p.clone();
        let mut h = crate::History::default();
        for v in [10, 20, 30] {
            h.apply_coalesced(&mut p, "selector", "Chain selector", vec![Command::SetRackSelector { rack, value: v }]).unwrap();
        }
        assert_eq!(h.steps().count(), 1);
        h.undo(&mut p).unwrap();
        assert_eq!(p, r.p);
        h.redo(&mut p).unwrap();
        assert_eq!(rack_of(&p, rack).chain_selector, 30);
        // Old projects that never wrote the field open at 0.
        let mut value: serde_json::Value = serde_json::from_str(&r.p.to_json().unwrap()).unwrap();
        fn strip(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Object(o) => {
                    o.remove("chain_selector");
                    o.values_mut().for_each(strip);
                }
                serde_json::Value::Array(a) => a.iter_mut().for_each(strip),
                _ => {}
            }
        }
        strip(&mut value["tracks"]);
        assert_eq!(rack_of(&Project::open_json(&value.to_string()).unwrap(), rack).chain_selector, 0);
    }

    // ── macro mappings follow their devices ──

    fn mappings(p: &Project, rack: DeviceId, m: usize) -> Vec<(DeviceId, u32)> {
        rack_of(p, rack).macros[m].mappings.iter().map(|x| (x.device, x.param)).collect()
    }

    /// Rig with macro 0 mapped to the EQ (pad 0), the compressor (pad 1) and a delay (return 0),
    /// in that order, plus a legacy mapping to a device that never existed.
    fn mapped_rig() -> (RackRig, DeviceId) {
        let mut r = rack_rig();
        let delay = r.p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Return { rack: r.rack, index: 0, pos: 0 }, device: Box::new(Device::stock(delay, "fx.delay", "Delay")) }
            .apply(&mut r.p)
            .unwrap();
        for (i, (device, param)) in [(r.comp, 2), (delay, 3)].into_iter().enumerate() {
            Command::AddMapping { rack: r.rack, macro_index: 0, index: i + 1, mapping: MacroMapping { device, param, min: 0.0, max: 1.0 } }.apply(&mut r.p).unwrap();
        }
        (r, delay)
    }

    /// Apply `cmd` through history; undo must give back the exact project, redo the same result.
    fn through_history(p: &Project, cmd: Command) -> Project {
        let mut q = p.clone();
        let mut h = crate::History::default();
        h.apply(&mut q, "edit", crate::Origin::User, vec![cmd]).unwrap();
        let after = q.clone();
        h.undo(&mut q).unwrap();
        assert_eq!(&q, p, "undo is not exact");
        h.redo(&mut q).unwrap();
        assert_eq!(q, after);
        let broken: Vec<String> = crate::routing::dangling_references(&q).into_iter().filter(|m| !m.contains("#9999")).collect();
        assert!(broken.is_empty(), "{broken:?}");
        after
    }

    #[test]
    fn deleting_a_mapped_device_drops_its_mappings_and_undo_puts_them_back() {
        let (r, delay) = mapped_rig();
        let before = mappings(&r.p, r.rack, 0);
        assert_eq!(before, vec![(r.eq, 1), (r.comp, 2), (delay, 3)]);
        // The middle one (pad 1's compressor), the first, and the one in a return.
        let q = through_history(&r.p, Command::RemoveDevice { id: r.comp });
        assert_eq!(mappings(&q, r.rack, 0), vec![(r.eq, 1), (delay, 3)]);
        let q = through_history(&r.p, Command::RemoveDevice { id: r.eq });
        assert_eq!(mappings(&q, r.rack, 0), vec![(r.comp, 2), (delay, 3)]);
        let q = through_history(&r.p, Command::RemoveDevice { id: delay });
        assert_eq!(mappings(&q, r.rack, 0), vec![(r.eq, 1), (r.comp, 2)]);
        // Deleting a device nothing maps to leaves the macros alone.
        let other = {
            let mut p = r.p.clone();
            let id = p.alloc_device_id();
            Command::AddDevice { slot: DeviceSlot::Track { track: r.track, index: 1 }, device: Box::new(Device::stock(id, "fx.eq", "EQ")) }.apply(&mut p).unwrap();
            (p, id)
        };
        let q = through_history(&other.0, Command::RemoveDevice { id: other.1 });
        assert_eq!(mappings(&q, r.rack, 0), before);
    }

    #[test]
    fn moving_a_device_out_of_its_rack_unmaps_it_but_inside_the_rack_it_keeps_its_mappings() {
        let (r, delay) = mapped_rig();
        let before = mappings(&r.p, r.rack, 0);
        // Out to the track's own chain: the macro must stop driving it.
        let q = through_history(&r.p, Command::MoveDevice { id: r.eq, to: DeviceSlot::Track { track: r.track, index: 1 } });
        assert_eq!(mappings(&q, r.rack, 0), vec![(r.comp, 2), (delay, 3)]);
        assert!(q.device(r.eq).is_some());
        // Out from a return, and to another track.
        let q = through_history(&r.p, Command::MoveDevice { id: delay, to: DeviceSlot::Track { track: r.track, index: 0 } });
        assert_eq!(mappings(&q, r.rack, 0), vec![(r.eq, 1), (r.comp, 2)]);
        let mut with_track = r.p.clone();
        let t2 = with_track.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(t2, TrackKind::Audio, "B", Color(0))), index: 1 }.apply(&mut with_track).unwrap();
        let q = through_history(&with_track, Command::MoveDevice { id: r.comp, to: DeviceSlot::Track { track: t2, index: 0 } });
        assert_eq!(mappings(&q, r.rack, 0), vec![(r.eq, 1), (delay, 3)]);
        // Inside the same rack (another pad, a return, back) every mapping stays.
        for to in [
            DeviceSlot::Rack { rack: r.rack, chain: 1, index: 0 },
            DeviceSlot::Return { rack: r.rack, index: 1, pos: 0 },
            DeviceSlot::Rack { rack: r.rack, chain: 0, index: 1 },
        ] {
            for id in [r.eq, r.comp, delay] {
                let q = through_history(&r.p, Command::MoveDevice { id, to });
                assert_eq!(mappings(&q, r.rack, 0), before, "{id} to {to:?}");
            }
        }
    }

    #[test]
    fn nested_racks_keep_the_mappings_of_the_outer_rack() {
        let (mut r, delay) = mapped_rig();
        // An inner audio rack on pad 0, with its own macro mapped to nothing yet.
        let inner = r.p.alloc_device_id();
        let mut rd = Device::stock(inner, "rack.audio", "Inner");
        let mut ir = Rack::new(RackKind::Audio);
        ir.chains.push(Chain::new("A", Color(1)));
        rd.kind = DeviceKind::Rack(ir);
        Command::AddDevice { slot: DeviceSlot::Rack { rack: r.rack, chain: 0, index: 1 }, device: Box::new(rd) }.apply(&mut r.p).unwrap();
        Command::AddMapping { rack: inner, macro_index: 0, index: 0, mapping: MacroMapping { device: r.eq, param: 5, min: 0.0, max: 1.0 } }.clone().apply(&mut r.p).unwrap_err();
        let before = mappings(&r.p, r.rack, 0);
        // Into the inner rack: still inside the outer one.
        let into_inner = DeviceSlot::Rack { rack: inner, chain: 0, index: 0 };
        let q = through_history(&r.p, Command::MoveDevice { id: r.comp, to: into_inner });
        assert_eq!(mappings(&q, r.rack, 0), before);
        // Map it from the inner rack too, then move it out of the inner rack but not the outer one:
        // the outer mapping stays, the inner one goes.
        let mut q = q;
        Command::AddMapping { rack: inner, macro_index: 0, index: 0, mapping: MacroMapping { device: r.comp, param: 7, min: 0.0, max: 1.0 } }.apply(&mut q).unwrap();
        let q2 = through_history(&q, Command::MoveDevice { id: r.comp, to: DeviceSlot::Rack { rack: r.rack, chain: 1, index: 0 } });
        assert_eq!(mappings(&q2, r.rack, 0), before);
        assert!(mappings(&q2, inner, 0).is_empty());
        // Deleting it removes both.
        let q3 = through_history(&q, Command::RemoveDevice { id: r.comp });
        assert!(mappings(&q3, inner, 0).is_empty());
        assert_eq!(mappings(&q3, r.rack, 0), vec![(r.eq, 1), (delay, 3)]);
        // Deleting the inner rack takes the devices inside it, and the mappings to them.
        let q4 = through_history(&q, Command::RemoveDevice { id: inner });
        assert_eq!(mappings(&q4, r.rack, 0), vec![(r.eq, 1), (delay, 3)]);
    }

    #[test]
    fn moving_between_two_racks_unmaps_from_the_first() {
        let (mut r, _) = mapped_rig();
        let second = r.p.alloc_device_id();
        let mut rd = Device::stock(second, "rack.audio", "Other");
        let mut rr = Rack::new(RackKind::Audio);
        rr.chains.push(Chain::new("A", Color(1)));
        rd.kind = DeviceKind::Rack(rr);
        Command::AddDevice { slot: DeviceSlot::Track { track: r.track, index: 1 }, device: Box::new(rd) }.apply(&mut r.p).unwrap();
        let q = through_history(&r.p, Command::MoveDevice { id: r.eq, to: DeviceSlot::Rack { rack: second, chain: 0, index: 0 } });
        assert!(!mappings(&q, r.rack, 0).contains(&(r.eq, 1)));
    }

    #[test]
    fn deleting_or_replacing_a_chain_a_return_or_the_track_cleans_up_too() {
        let (r, delay) = mapped_rig();
        let rack = r.rack;
        let q = through_history(&r.p, Command::RemoveChain { rack, index: 0 });
        assert_eq!(mappings(&q, rack, 0), vec![(r.comp, 2), (delay, 3)]);
        let q = through_history(&r.p, Command::RemoveReturn { rack, index: 0 });
        assert_eq!(mappings(&q, rack, 0), vec![(r.eq, 1), (r.comp, 2)]);
        // UpdateChain that drops the compressor from pad 1 (and keeps the other pad's mappings).
        let mut empty = rack_of(&r.p, rack).chains[1].clone();
        empty.devices.clear();
        let q = through_history(&r.p, Command::UpdateChain { rack, index: 1, chain: Box::new(empty) });
        assert_eq!(mappings(&q, rack, 0), vec![(r.eq, 1), (delay, 3)]);
        // One that renames it keeps everything.
        let mut renamed = rack_of(&r.p, rack).chains[1].clone();
        renamed.name = "Snare 2".into();
        let q = through_history(&r.p, Command::UpdateChain { rack, index: 1, chain: Box::new(renamed) });
        assert_eq!(mappings(&q, rack, 0).len(), 3);
        // The whole track.
        let q = through_history(&r.p, Command::RemoveTrack { id: r.track });
        assert!(q.track(r.track).is_none());
    }

    #[test]
    fn a_mapping_that_was_already_dangling_does_not_break_undo() {
        let (mut r, delay) = mapped_rig();
        // A project from before this check: a mapping to a device that is gone.
        let Some(Device { kind: DeviceKind::Rack(rack), .. }) = r.p.device_mut(r.rack) else { panic!() };
        rack.macros[3].mappings.push(MacroMapping { device: DeviceId(9999), param: 0, min: 0.0, max: 1.0 });
        let q = through_history(&r.p, Command::RemoveDevice { id: delay });
        assert_eq!(mappings(&q, r.rack, 3), vec![(DeviceId(9999), 0)], "left alone");
        through_history(&r.p, Command::MoveDevice { id: r.eq, to: DeviceSlot::Track { track: r.track, index: 1 } });
    }

    #[test]
    fn removing_a_track_through_history_is_one_undo_step() {
        let r = rig();
        let mut p = r.p.clone();
        let mut h = crate::History::default();
        h.apply(&mut p, "Delete track", crate::Origin::User, vec![Command::RemoveTrack { id: r.bus }]).unwrap();
        assert!(h.undo(&mut p).unwrap());
        assert_eq!(p, r.p);
        assert!(h.redo(&mut p).unwrap());
        assert!(p.track(r.bus).is_none());
        assert!(crate::routing::dangling_references(&p).is_empty());
    }

    #[test]
    fn outputs_and_sends_reject_routing_cycles() {
        let mut r = rig();
        let p = &mut r.p;
        // src -> bus already; bus -> src would close the loop.
        let before = p.clone();
        let err = Command::SetTrack { id: r.bus, change: TrackProp::Output(Output::Track(r.src)) }.apply(p);
        assert_eq!(err, Err(CommandError::RoutingCycle { from: r.bus, to: r.src }));
        assert_eq!(*p, before, "a rejected edit changes nothing");
        // Itself, a track that is not there, and one that carries no audio.
        for bad in [r.bus, TrackId(9999), r.vca] {
            assert!(Command::SetTrack { id: r.bus, change: TrackProp::Output(Output::Track(bad)) }.apply(p).is_err(), "{bad}");
        }
        // Sends: ret -> src would loop through src's send to ret; so would bus -> src.
        let send_to = |target| vec![Send { target, level_db: 0.0, pre_fader: false }];
        assert_eq!(
            Command::SetTrack { id: r.ret, change: TrackProp::Sends(send_to(r.src)) }.apply(p),
            Err(CommandError::RoutingCycle { from: r.ret, to: r.src })
        );
        assert!(Command::SetTrack { id: r.bus, change: TrackProp::Sends(send_to(r.src)) }.apply(p).is_err());
        assert!(Command::SetTrack { id: r.src, change: TrackProp::Sends(send_to(r.src)) }.apply(p).is_err(), "no send to itself");
        assert!(Command::SetTrack { id: r.src, change: TrackProp::Sends(send_to(TrackId(9999))) }.apply(p).is_err());
        // A group: its member routes into it, so the group cannot route into the member,
        // whether the member's output is explicit or Main read as "the enclosing group".
        for member in [r.member_out, r.member_main] {
            assert_eq!(
                Command::SetTrack { id: r.group, change: TrackProp::Output(Output::Track(member)) }.apply(p),
                Err(CommandError::RoutingCycle { from: r.group, to: member })
            );
        }
        // Legal rerouting still works and undoes; an unchanged route is never judged.
        let undo = Command::SetTrack { id: r.bus, change: TrackProp::Output(Output::Track(r.ret)) }.apply(p).unwrap();
        assert_eq!(p.track(r.bus).unwrap().output, Output::Track(r.ret));
        undo.apply(p).unwrap();
        Command::SetTrack { id: r.src, change: TrackProp::Output(Output::Track(r.bus)) }.apply(p).unwrap();
        let mut sends = p.track(r.src).unwrap().sends.clone();
        sends[0].level_db = -12.0;
        Command::SetTrack { id: r.src, change: TrackProp::Sends(sends) }.apply(p).unwrap();
        Command::SetTrack { id: r.src, change: TrackProp::Output(Output::Hardware(2)) }.apply(p).unwrap();
        Command::SetTrack { id: r.bus, change: TrackProp::Output(Output::None) }.apply(p).unwrap();
    }

    #[test]
    fn record_input_and_monitor_undo_redo_and_old_projects_open() {
        let mut p = Project::new("t");
        let audio = {
            let id = p.alloc_track_id();
            Command::AddTrack { track: Box::new(Track::new(id, TrackKind::Audio, "Vox", Color::palette(1))), index: 1 }.apply(&mut p).unwrap();
            id
        };
        assert_eq!(p.track(audio).unwrap().input, InputSource::None);
        assert_eq!(p.track(audio).unwrap().monitor, Monitor::Auto);
        let before = p.clone();
        let mut h = crate::History::default();
        let pair = InputSource::Hardware { first: 2, stereo: true };
        h.apply(&mut p, "Input", crate::Origin::User, vec![Command::SetTrack { id: audio, change: TrackProp::Input(pair) }]).unwrap();
        h.apply(&mut p, "Monitor", crate::Origin::User, vec![Command::SetTrack { id: audio, change: TrackProp::Monitor(Monitor::In) }]).unwrap();
        let t = p.track(audio).unwrap();
        assert_eq!((t.input, t.monitor), (pair, Monitor::In));
        assert_eq!(pair.channel_span(), Some((2, 2)));
        assert_eq!(InputSource::Hardware { first: 0, stereo: false }.channel_span(), Some((0, 1)));
        assert_eq!(InputSource::None.channel_span(), None);
        h.undo(&mut p).unwrap();
        assert_eq!(p.track(audio).unwrap().monitor, Monitor::Auto);
        assert_eq!(p.track(audio).unwrap().input, pair);
        h.undo(&mut p).unwrap();
        assert_eq!(p, before);
        h.redo(&mut p).unwrap();
        h.redo(&mut p).unwrap();
        assert_eq!((p.track(audio).unwrap().input, p.track(audio).unwrap().monitor), (pair, Monitor::In));
        // Both survive a save and reopen, and travel as tagged JSON commands.
        let back = Project::open_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(back.track(audio).unwrap().input, pair);
        let json = serde_json::to_string(&Command::SetTrack { id: audio, change: TrackProp::Input(pair) }).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), Command::SetTrack { id: audio, change: TrackProp::Input(pair) });
        // A project saved before tracks had an input opens with none.
        let mut value: serde_json::Value = serde_json::from_str(&before.to_json().unwrap()).unwrap();
        for t in value["tracks"].as_array_mut().unwrap() {
            t.as_object_mut().unwrap().remove("input");
        }
        let old = Project::open_json(&value.to_string()).unwrap();
        assert_eq!(old, before);
        // Only audio tracks record; "none" is always fine.
        let main = p.main_track().id;
        assert!(Command::SetTrack { id: main, change: TrackProp::Input(pair) }.apply(&mut p).is_err());
        assert!(Command::SetTrack { id: main, change: TrackProp::Input(InputSource::None) }.apply(&mut p).is_ok());
    }

    #[test]
    fn track_scale_and_keyboard_input_undo_cleanly() {
        let (mut p, track) = project_with_track();
        let scale = Some(Scale { root: crate::Key(2), kind: crate::ScaleKind::Minor });
        let undo_scale = Command::SetTrack { id: track, change: TrackProp::Scale(scale) }.apply(&mut p).unwrap();
        assert_eq!(p.track(track).unwrap().scale, scale);
        undo_scale.apply(&mut p).unwrap();
        assert_eq!(p.track(track).unwrap().scale, None);

        let undo_kbd = Command::SetTrack { id: track, change: TrackProp::KeyboardInput(true) }.apply(&mut p).unwrap();
        assert!(p.track(track).unwrap().keyboard_input);
        undo_kbd.apply(&mut p).unwrap();
        assert!(!p.track(track).unwrap().keyboard_input);
    }
    fn frozen_file() -> AudioSource {
        AudioSource { path: "/tmp/Lead Frozen.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames: 96_000, warp: Warp::default(), key: None }
    }

    #[test]
    fn freeze_and_unfreeze_undo_cleanly() {
        let (mut p, track) = project_with_track();
        let before = p.clone();
        let undo = Command::FreezeTrack { track, file: frozen_file() }.apply(&mut p).unwrap();
        assert!(p.track(track).unwrap().is_frozen());
        assert!(Command::FreezeTrack { track, file: frozen_file() }.apply(&mut p).is_err(), "already frozen");
        let redo = undo.apply(&mut p).unwrap();
        assert_eq!(p, before);
        redo.apply(&mut p).unwrap();
        let undo_unfreeze = Command::UnfreezeTrack { track }.apply(&mut p).unwrap();
        assert!(!p.track(track).unwrap().is_frozen());
        assert!(Command::UnfreezeTrack { track }.apply(&mut p).is_err(), "not frozen");
        undo_unfreeze.apply(&mut p).unwrap();
        assert_eq!(p.track(track).unwrap().freeze.as_ref().map(|f| f.file.clone()), Some(frozen_file()));
        let main = p.main_track().id;
        assert!(Command::FreezeTrack { track: main, file: frozen_file() }.apply(&mut p).is_err(), "Main cannot be frozen");
    }

    #[test]
    fn undoing_a_freeze_gives_the_arming_back() {
        let (mut p, track) = project_with_track();
        p.track_mut(track).unwrap().arm = true;
        let before = p.clone();
        let undo = Command::FreezeTrack { track, file: frozen_file() }.apply(&mut p).unwrap();
        assert!(!p.track(track).unwrap().arm);
        let redo = undo.apply(&mut p).unwrap();
        assert!(!p.track(track).unwrap().is_frozen() && p.track(track).unwrap().arm, "undo restores the arm");
        assert_eq!(p, before);
        let undo = redo.apply(&mut p).unwrap();
        assert!(p.track(track).unwrap().is_frozen() && !p.track(track).unwrap().arm, "redo freezes again");
        undo.apply(&mut p).unwrap();
        assert_eq!(p, before);
    }

    #[test]
    fn flatten_turns_the_frozen_track_into_an_audio_track_and_undoes() {
        let (mut p, track) = project_with_track();
        let dev = p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Track { track, index: 0 }, device: Box::new(Device::stock(dev, "fx.eq", "EQ")) }.apply(&mut p).unwrap();
        let mut param = AutomationLane::new(AutomationTarget::DeviceParam { device: dev, param: 0 });
        param.insert(AutomationPoint { tick: 0, value: 0.5, curve: 0.0 });
        let mut volume = AutomationLane::new(AutomationTarget::Volume);
        volume.insert(AutomationPoint { tick: 0, value: 0.5, curve: 0.0 });
        Command::SetAutomation { track, target: param.target, lane: Some(param) }.apply(&mut p).unwrap();
        Command::SetAutomation { track, target: volume.target, lane: Some(volume) }.apply(&mut p).unwrap();
        let (source, clip, placement) = (p.alloc_source_id(), p.alloc_clip_id(), p.alloc_placement_id());
        let flatten = Command::FlattenTrack { track, source, clip, placement };
        assert!(flatten.clone().apply(&mut p).is_err(), "only a frozen track can be flattened");

        Command::FreezeTrack { track, file: frozen_file() }.apply(&mut p).unwrap();
        let frozen = p.clone();
        let undo = flatten.apply(&mut p).unwrap();
        let t = p.track(track).unwrap();
        assert_eq!((t.kind, t.is_frozen(), t.devices.len()), (TrackKind::Audio, false, 0));
        assert_eq!(t.arrangement.len(), 1);
        assert_eq!(t.automation.len(), 1, "the mixer lane stays, the device lane goes");
        assert_eq!(t.arrangement[0].start, 0);
        let c = &p.clips[&t.arrangement[0].clip];
        assert_eq!(c.length, 4 * PPQ,"two seconds at 120 BPM are four beats");
        assert!(matches!(p.sources.get(&c.source), Some(Source::Audio(a)) if *a == frozen_file()));

        let redo = undo.apply(&mut p).unwrap();
        assert_eq!(p, frozen, "undo gives back the frozen track exactly");
        redo.apply(&mut p).unwrap();
        assert_eq!(p.track(track).unwrap().kind, TrackKind::Audio);
    }

    #[test]
    fn flatten_clears_the_takes_and_the_comp_and_undoes() {
        let (mut p, track) = project_with_track();
        Command::AddTakeLane { track, index: 0, name: "Take 1".into() }.apply(&mut p).unwrap();
        p.track_mut(track).unwrap().comp.push(CompSpan { start: 0, end: PPQ, lane: 1 });
        Command::FreezeTrack { track, file: frozen_file() }.apply(&mut p).unwrap();
        let (source, clip, placement) = (p.alloc_source_id(), p.alloc_clip_id(), p.alloc_placement_id());
        let frozen = p.clone();
        let undo = Command::FlattenTrack { track, source, clip, placement }.apply(&mut p).unwrap();
        let t = p.track(track).unwrap();
        assert!(t.take_lanes.is_empty() && t.comp.is_empty(), "the takes and comp described the clips that are gone");
        undo.apply(&mut p).unwrap();
        assert_eq!(p, frozen);
        assert_eq!(p.track(track).unwrap().take_lanes.len(), 1);
    }

    #[test]
    fn a_batch_that_unfreezes_may_edit_the_track_after_it() {
        let (mut p, track) = project_with_track();
        let dev = p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Track { track, index: 0 }, device: Box::new(Device::stock(dev, "fx.eq", "EQ")) }.apply(&mut p).unwrap();
        Command::FreezeTrack { track, file: frozen_file() }.apply(&mut p).unwrap();
        let set = Command::SetParam { device: dev, param: 0, value: Some(0.5) };
        let unfreeze = Command::UnfreezeTrack { track };
        let after = Command::Batch { commands: vec![unfreeze.clone(), set.clone()] };
        assert_eq!(after.frozen_target(&p), None, "the track is live by the time the parameter changes");
        let before = Command::Batch { commands: vec![set.clone(), unfreeze] };
        assert_eq!(before.frozen_target(&p), Some(track));
        let refreeze = Command::Batch { commands: vec![Command::UnfreezeTrack { track }, Command::FreezeTrack { track, file: frozen_file() }, set] };
        assert_eq!(refreeze.frozen_target(&p), Some(track));
    }

    #[test]
    fn a_frozen_track_refuses_clip_and_device_edits_but_not_the_mixer() {
        use crate::history::{History, Origin};
        let (mut p, track) = project_with_track();
        let (src, clip, pl, dev) = (p.alloc_source_id(), p.alloc_clip_id(), p.alloc_placement_id(), p.alloc_device_id());
        let mut h = History::default();
        let setup = vec![
            Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource::default())) },
            Command::AddClip { id: clip, clip: Box::new(Clip::new("c", Color(0), src, PPQ)) },
            Command::PlaceClip { track, placement: Placement { id: pl, clip, start: 0, lane: 0 } },
            Command::AddDevice { slot: DeviceSlot::Track { track, index: 0 }, device: Box::new(Device::stock(dev, "fx.eq", "EQ")) },
        ];
        h.apply(&mut p, "setup", Origin::User, setup).unwrap();
        h.apply(&mut p, "freeze", Origin::User, vec![Command::FreezeTrack { track, file: frozen_file() }]).unwrap();
        let n = p.alloc_note_id();
        for refused in [
            Command::SetParam { device: dev, param: 0, value: Some(0.5) },
            Command::RemoveDevice { id: dev },
            Command::AddNotes { source: src, notes: vec![Note::new(n, 60, 0, PPQ, 1.0)] },
            Command::RemovePlacement { track, id: pl },
            Command::MovePlacement { track, id: pl, to_track: track, start: PPQ, lane: 0 },
            Command::SetTrack { id: track, change: TrackProp::Arm(true) },
            Command::SetAutomation { track, target: AutomationTarget::DeviceParam { device: dev, param: 0 }, lane: Some(AutomationLane::new(AutomationTarget::DeviceParam { device: dev, param: 0 })) },
            Command::Batch { commands: vec![Command::SetParam { device: dev, param: 0, value: None }] },
        ] {
            assert_eq!(h.apply(&mut p, "edit", Origin::User, vec![refused]), Err(CommandError::Frozen(track)));
        }
        for allowed in [
            Command::SetTrack { id: track, change: TrackProp::VolumeDb(-3.0) },
            Command::SetTrack { id: track, change: TrackProp::Mute(true) },
            Command::SetTrack { id: track, change: TrackProp::Arm(false) },
            Command::SetAutomation { track, target: AutomationTarget::Volume, lane: Some(AutomationLane::new(AutomationTarget::Volume)) },
        ] {
            h.apply(&mut p, "mix", Origin::User, vec![allowed]).unwrap();
        }
        h.apply(&mut p, "unfreeze", Origin::User, vec![Command::UnfreezeTrack { track }]).unwrap();
        h.apply(&mut p, "edit", Origin::User, vec![Command::SetParam { device: dev, param: 0, value: Some(0.5) }]).unwrap();
    }

    #[test]
    fn a_frozen_track_survives_json() {
        let (mut p, track) = project_with_track();
        Command::FreezeTrack { track, file: frozen_file() }.apply(&mut p).unwrap();
        let back = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(back.tracks, p.tracks);
        // An old file without the field loads unfrozen.
        let mut val = serde_json::to_value(p.track(track).unwrap()).unwrap();
        val.as_object_mut().unwrap().remove("freeze");
        assert!(serde_json::from_value::<Track>(val).unwrap().freeze.is_none());
    }
}
