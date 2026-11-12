//! The Command API: every edit to a [`Project`] is a serialisable
//! [`Command`]. Applying a command returns its exact inverse, which is what
//! undo stores. Commands carry ids allocated up front (`Project::alloc_*`),
//! so redo replays the same ids and later commands keep pointing at the
//! right objects.
//!
//! The same enum is the wire format for scripts and AI agents (via MCP), so
//! variants are explicit and serialise as tagged JSON.

use crate::ids::*;
use crate::project::*;
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
    Output(Output),
    Sends(Vec<Send>),
    Parent(Option<TrackId>),
    Vca(Option<TrackId>),
    Collapsed(bool),
    Height(f32),
}

/// Where a device is inserted: a track's chain or a chain inside a rack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "in")]
pub enum DeviceSlot {
    Track { track: TrackId, index: usize },
    Rack { rack: DeviceId, chain: usize, index: usize },
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
    AddClip { id: ClipId, clip: Box<Clip> },
    RemoveClip { id: ClipId },
    UpdateClip { id: ClipId, clip: Box<Clip> },

    PlaceClip { track: TrackId, placement: Placement },
    RemovePlacement { track: TrackId, id: PlacementId },
    MovePlacement { track: TrackId, id: PlacementId, to_track: TrackId, start: Tick, lane: u16 },
    SetSessionSlot { track: TrackId, scene: SceneId, clip: Option<ClipId> },

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
    SetMacro { rack: DeviceId, index: usize, value: f32 },
    SetMacroDef { rack: DeviceId, index: usize, name: String, mappings: Vec<MacroMapping> },
    SetVariations { rack: DeviceId, variations: Vec<Variation> },

    /// Set (or with `None` remove) the automation lane for `target`.
    SetAutomation { track: TrackId, target: AutomationTarget, lane: Option<AutomationLane> },

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
                TrackProp::VolumeDb(_) => "Change volume",
                TrackProp::Pan(_) => "Change pan",
                TrackProp::Mute(_) => "Mute",
                TrackProp::Solo(_) => "Solo",
                TrackProp::Sends(_) => "Change sends",
                TrackProp::Output(_) => "Change routing",
                _ => "Change track",
            },
            AddScene { .. } => "Add scene",
            RemoveScene { .. } => "Delete scene",
            RenameScene { .. } => "Rename scene",
            AddSource { .. } | AddClip { .. } => "Add clip",
            RemoveSource { .. } | RemoveClip { .. } => "Delete clip",
            UpdateClip { .. } => "Edit clip",
            SetWarp { .. } => "Edit sample",
            PlaceClip { .. } => "Place clip",
            RemovePlacement { .. } => "Delete clip",
            MovePlacement { .. } => "Move clip",
            SetSessionSlot { .. } => "Set clip slot",
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
            SetMacro { .. } => "Change macro",
            SetMacroDef { .. } => "Map macro",
            SetVariations { .. } => "Store variation",
            SetAutomation { .. } => "Edit automation",
            Batch { .. } => "Edit",
        }
    }

    /// True for commands that delete user content (agents must ask first).
    pub fn is_destructive(&self) -> bool {
        use Command::*;
        match self {
            RemoveTrack { .. } | RemoveScene { .. } | RemoveSource { .. } | RemoveClip { .. }
            | RemovePlacement { .. } | RemoveNotes { .. } | RemoveDevice { .. } | RemoveChain { .. } => true,
            Batch { commands } => commands.iter().any(Command::is_destructive),
            _ => false,
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
                p.ids.observe(track.id.0);
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
                Ok(AddTrack { track: Box::new(track), index })
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

            PlaceClip { track, placement } => {
                if !p.clips.contains_key(&placement.clip) {
                    return Err(CommandError::ClipNotFound(placement.clip));
                }
                p.ids.observe(placement.id.0);
                let t = p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?;
                let id = placement.id;
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
                if p.track(to_track).is_none() {
                    return Err(CommandError::TrackNotFound(to_track));
                }
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
                let mut max = 0;
                visit_devices(std::slice::from_ref(device.as_ref()), &mut |d| max = max.max(d.id.0));
                p.ids.observe(max);
                let id = device.id;
                let chain = chain_for_slot(p, slot)?;
                let index = slot_index(slot).min(chain.len());
                chain.insert(index, *device);
                Ok(RemoveDevice { id })
            }
            RemoveDevice { id } => {
                let (slot, device) = take_device(p, id)?;
                Ok(AddDevice { slot, device: Box::new(device) })
            }
            MoveDevice { id, to } => {
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
                Ok(MoveDevice { id, to: from })
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
                let r = rack_mut(p, rack)?;
                let index = index.min(r.chains.len());
                r.chains.insert(index, *chain);
                Ok(RemoveChain { rack, index })
            }
            RemoveChain { rack, index } => {
                let r = rack_mut(p, rack)?;
                if index >= r.chains.len() {
                    return Err(CommandError::OutOfRange(index));
                }
                let chain = r.chains.remove(index);
                Ok(AddChain { rack, index, chain: Box::new(chain) })
            }
            UpdateChain { rack, index, chain } => {
                let r = rack_mut(p, rack)?;
                let slot = r.chains.get_mut(index).ok_or(CommandError::OutOfRange(index))?;
                Ok(UpdateChain { rack, index, chain: Box::new(std::mem::replace(slot, *chain)) })
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
        TrackProp::Output(v) => TrackProp::Output(replace(&mut t.output, v)),
        TrackProp::Sends(v) => TrackProp::Sends(replace(&mut t.sends, v)),
        TrackProp::Parent(v) => TrackProp::Parent(replace(&mut t.parent, v)),
        TrackProp::Vca(v) => TrackProp::Vca(replace(&mut t.vca, v)),
        TrackProp::Collapsed(v) => TrackProp::Collapsed(replace(&mut t.collapsed, v)),
        TrackProp::Height(v) => TrackProp::Height(replace(&mut t.height, v.clamp(24.0, 400.0))),
    }
}

fn note_source(p: &mut Project, id: SourceId) -> Result<&mut NoteSource, CommandError> {
    match p.sources.get_mut(&id) {
        Some(Source::Notes(n)) => Ok(n),
        Some(_) => Err(CommandError::NotNotes(id)),
        None => Err(CommandError::SourceNotFound(id)),
    }
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
    }
}

fn chain_for_slot(p: &mut Project, slot: DeviceSlot) -> Result<&mut Vec<Device>, CommandError> {
    match slot {
        DeviceSlot::Track { track, .. } => Ok(&mut p.track_mut(track).ok_or(CommandError::TrackNotFound(track))?.devices),
        DeviceSlot::Rack { rack, chain, .. } => {
            let r = rack_mut(p, rack)?;
            Ok(&mut r.chains.get_mut(chain).ok_or(CommandError::OutOfRange(chain))?.devices)
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
}
