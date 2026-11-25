//! Track management as pure command builders: duplicate a track (with its
//! devices, clips, notes and automation), reorder it, and recolour it. The
//! UI, scripts and agents share these rules; the builders that need fresh
//! ids take `&mut Project` only to allocate them.

use crate::command::{Command, DeviceSlot, TrackProp};
use crate::groups;
use crate::ids::*;
use crate::project::*;
use std::collections::HashMap;

/// The result of [`duplicate_track`].
#[derive(Clone, Debug, PartialEq)]
pub struct Duplicated {
    /// Apply these as one step (`History::apply`).
    pub commands: Vec<Command>,
    /// The copy of the track that was asked for.
    pub track: TrackId,
    /// Every track created, in list order: just `track`, or a group's copy
    /// followed by its members' copies.
    pub tracks: Vec<TrackId>,
}

/// The commands that copy track `id` right after it: its settings, a copy of
/// every device (with new ids, racks and their macros and the automation of
/// their parameters following the copies), the clips of its arrangement and
/// Session slots, and its automation. A group or folder is copied with all
/// the tracks inside it, which route into the copy.
///
/// Clips are copied too, so the copy is independent: notes clips get their
/// own notes (new ids); a clip placed twice stays one clip in the copy; audio
/// clips share their file. The copy is named "<name> copy" and is not armed.
/// Sends, sidechains and VCA membership keep pointing at the same tracks.
/// `None` for Main or an unknown track.
pub fn duplicate_track(p: &mut Project, id: TrackId) -> Option<Duplicated> {
    duplicate_track_inner(p, id, true)
}

/// Duplicate a track (with its devices, routing and mixer state) but without
/// clips, session slots or automation (Live's Duplicate Without Content).
pub fn duplicate_track_empty(p: &mut Project, id: TrackId) -> Option<Duplicated> {
    duplicate_track_inner(p, id, false)
}

fn duplicate_track_inner(p: &mut Project, id: TrackId, include_content: bool) -> Option<Duplicated> {
    p.track(id).filter(|t| t.kind != TrackKind::Main)?;
    let block: Vec<TrackId> = std::iter::once(id).chain(groups::descendants(p, id)).collect();
    let originals: Vec<Track> = block.iter().filter_map(|&t| p.track(t).cloned()).collect();
    let base = block.iter().filter_map(|&t| p.track_index(t)).max()? + 1;
    let track_map: HashMap<TrackId, TrackId> = block.iter().map(|&t| (t, p.alloc_track_id())).collect();
    let remap_track = |t: TrackId| track_map.get(&t).copied().unwrap_or(t);

    let mut clips = ClipCopier::default();
    let (mut add_tracks, mut place) = (Vec::new(), Vec::new());
    for (k, orig) in originals.iter().enumerate() {
        let new_id = track_map[&orig.id];
        let mut devices = HashMap::new();
        let mut copy = orig.clone();
        copy.id = new_id;
        if orig.id == id {
            copy.name = format!("{} copy", orig.name);
        } else {
            copy.parent = orig.parent.map(remap_track);
        }
        copy.arm = false;
        // The copy starts without the original's clips, so it cannot play their render.
        copy.freeze = None;
        copy.keyboard_input = false;
        copy.reported_latency = 0;
        copy.output = match orig.output {
            Output::Track(t) => Output::Track(remap_track(t)),
            other => other,
        };
        for s in &mut copy.sends {
            s.target = remap_track(s.target);
        }
        copy.devices = copy_devices(p, &orig.devices, &mut devices);
        relink_devices(&mut copy.devices, &devices, &remap_track);
        if include_content {
            for lane in &mut copy.automation {
                lane.target = match lane.target {
                    AutomationTarget::DeviceParam { device, param } => AutomationTarget::DeviceParam { device: devices.get(&device).copied().unwrap_or(device), param },
                    AutomationTarget::DeviceEnabled { device } => AutomationTarget::DeviceEnabled { device: devices.get(&device).copied().unwrap_or(device) },
                    AutomationTarget::Macro { device, index } => AutomationTarget::Macro { device: devices.get(&device).copied().unwrap_or(device), index },
                    other => other,
                };
            }
        } else {
            copy.automation.clear();
        }
        copy.arrangement.clear();
        copy.session_slots.clear();
        add_tracks.push(Command::AddTrack { track: Box::new(copy), index: base + k });
        if include_content {
            for pl in &orig.arrangement {
                let Some(clip) = clips.copy(p, pl.clip) else { continue };
                let placement = Placement { id: p.alloc_placement_id(), clip, start: pl.start, lane: pl.lane };
                place.push(Command::PlaceClip { track: new_id, placement });
            }
            for (&scene, &slot) in &orig.session_slots {
                let Some(clip) = clips.copy(p, slot) else { continue };
                place.push(Command::SetSessionSlot { track: new_id, scene, clip: Some(clip) });
            }
        }
    }
    let tracks: Vec<TrackId> = originals.iter().map(|t| track_map[&t.id]).collect();
    let mut commands = add_tracks;
    commands.extend(clips.commands);
    commands.extend(place);
    Some(Duplicated { commands, track: track_map[&id], tracks })
}

/// Copies clips (and the notes they own) once each, so clips that were one
/// clip stay one clip.
#[derive(Default)]
struct ClipCopier {
    clips: HashMap<ClipId, ClipId>,
    sources: HashMap<SourceId, SourceId>,
    commands: Vec<Command>,
}

impl ClipCopier {
    fn copy(&mut self, p: &mut Project, old: ClipId) -> Option<ClipId> {
        if let Some(&new) = self.clips.get(&old) {
            return Some(new);
        }
        let mut clip = p.clips.get(&old)?.clone();
        if let Some(Source::Notes(old_src)) = p.sources.get(&clip.source).cloned() {
            clip.source = match self.sources.get(&clip.source) {
                Some(&s) => s,
                None => {
                    let new = p.alloc_source_id();
                    let notes = old_src.notes.iter().map(|n| Note { id: p.alloc_note_id(), ..n.clone() }).collect();
                    self.commands.push(Command::AddSource { id: new, source: Box::new(Source::Notes(NoteSource { notes, pattern: old_src.pattern.clone() })) });
                    self.sources.insert(clip.source, new);
                    new
                }
            };
        }
        let new = p.alloc_clip_id();
        self.commands.push(Command::AddClip { id: new, clip: Box::new(clip) });
        self.clips.insert(old, new);
        Some(new)
    }
}

/// Copies of `devices` with new ids (racks included, all the way down);
/// `map` collects old id → new id.
fn copy_devices(p: &mut Project, devices: &[Device], map: &mut HashMap<DeviceId, DeviceId>) -> Vec<Device> {
    devices
        .iter()
        .map(|d| {
            let mut copy = d.clone();
            copy.id = p.alloc_device_id();
            map.insert(d.id, copy.id);
            if let DeviceKind::Rack(rack) = &mut copy.kind {
                for chain in rack.chains.iter_mut().chain(rack.returns.iter_mut()) {
                    chain.devices = copy_devices(p, &chain.devices, map);
                }
            }
            copy
        })
        .collect()
}

/// Point what the copies refer to at the copies: rack macros and modulator
/// targets at the copied devices, sidechains at the copied tracks (when the source was copied too).
fn relink_devices(devices: &mut [Device], map: &HashMap<DeviceId, DeviceId>, track: &impl Fn(TrackId) -> TrackId) {
    for d in devices {
        d.sidechain = d.sidechain.map(track);
        for m in &mut d.mods {
            if let ModTarget::Param { device, .. } = &mut m.target {
                *device = map.get(device).copied().unwrap_or(*device);
            }
        }
        if let DeviceKind::Rack(rack) = &mut d.kind {
            for m in &mut rack.macros {
                for mapping in &mut m.mappings {
                    mapping.device = map.get(&mapping.device).copied().unwrap_or(mapping.device);
                }
            }
            for chain in rack.chains.iter_mut().chain(rack.returns.iter_mut()) {
                relink_devices(&mut chain.devices, map, track);
            }
        }
    }
}

/// The result of [`duplicate_devices`].
#[derive(Clone, Debug, PartialEq)]
pub struct DuplicatedDevices {
    /// Apply these as one step.
    pub commands: Vec<Command>,
    /// The new devices, in the order of their originals in the chain.
    pub devices: Vec<DeviceId>,
}

/// Where device `id` sits (the slot has its index in its chain).
fn locate(p: &Project, id: DeviceId) -> Option<DeviceSlot> {
    fn find(chain: &[Device], id: DeviceId, here: &dyn Fn(usize) -> DeviceSlot) -> Option<DeviceSlot> {
        if let Some(i) = chain.iter().position(|d| d.id == id) {
            return Some(here(i));
        }
        for d in chain {
            if let DeviceKind::Rack(r) = &d.kind {
                let rack = d.id;
                for (ci, c) in r.chains.iter().enumerate() {
                    if let Some(found) = find(&c.devices, id, &move |index| DeviceSlot::Rack { rack, chain: ci, index }) {
                        return Some(found);
                    }
                }
                for (ri, c) in r.returns.iter().enumerate() {
                    if let Some(found) = find(&c.devices, id, &move |pos| DeviceSlot::Return { rack, index: ri, pos }) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }
    p.tracks.iter().find_map(|t| {
        let track = t.id;
        find(&t.devices, id, &move |index| DeviceSlot::Track { track, index })
    })
}

/// The commands that copy each of `ids` right after itself in its chain
/// (Cmd+D in the device view): a copy has new ids all the way down (a rack's
/// chains and nested devices too), keeps parameters, state, preset and
/// sidechain, and a rack's macros drive the copied devices. Devices inside a
/// selected rack ride along with it rather than being copied twice; unknown
/// ids are ignored. Automation is not copied. `None` when there is nothing
/// to copy.
pub fn duplicate_devices(p: &mut Project, ids: &[DeviceId]) -> Option<DuplicatedDevices> {
    let mut picked: Vec<(DeviceId, DeviceSlot)> = Vec::new();
    for &id in ids {
        if picked.iter().all(|(d, _)| *d != id)
            && let Some(slot) = locate(p, id)
        {
            picked.push((id, slot));
        }
    }
    let inside_another = |id: DeviceId| {
        picked.iter().any(|&(other, _)| {
            other != id && p.device(other).is_some_and(|d| {
                let mut found = false;
                visit_devices(std::slice::from_ref(d), &mut |x| found |= x.id == id && x.id != d.id);
                found
            })
        })
    };
    let mut picked: Vec<(DeviceId, DeviceSlot)> = picked.iter().copied().filter(|&(id, _)| !inside_another(id)).collect();
    // Chain by chain, in chain order, so each copy's index counts the copies before it.
    let key = |s: &DeviceSlot| match *s {
        DeviceSlot::Track { track, index } => (0, track.0, 0, index),
        DeviceSlot::Rack { rack, chain, index } => (1, rack.0, chain, index),
        DeviceSlot::Return { rack, index, pos } => (2, rack.0, index, pos),
    };
    picked.sort_by_key(|(_, s)| key(s));
    let mut commands = Vec::new();
    let mut devices = Vec::new();
    let mut earlier: HashMap<(u8, u64, usize), usize> = HashMap::new();
    for (id, slot) in picked {
        let original = p.device(id)?.clone();
        let mut map = HashMap::new();
        let mut copy = copy_devices(p, std::slice::from_ref(&original), &mut map);
        relink_devices(&mut copy, &map, &|t| t);
        let copy = copy.pop()?;
        let (k, owner, chain, index) = key(&slot);
        let shift = earlier.entry((k, owner, chain)).or_insert(0);
        let at = index + 1 + *shift;
        *shift += 1;
        let slot = match slot {
            DeviceSlot::Track { track, .. } => DeviceSlot::Track { track, index: at },
            DeviceSlot::Rack { rack, chain, .. } => DeviceSlot::Rack { rack, chain, index: at },
            DeviceSlot::Return { rack, index, .. } => DeviceSlot::Return { rack, index, pos: at },
        };
        devices.push(copy.id);
        commands.push(Command::AddDevice { slot, device: Box::new(copy) });
    }
    (!commands.is_empty()).then_some(DuplicatedDevices { commands, devices })
}

/// The commands that move track `id` (with the tracks inside it, if it is a
/// group) in front of `before`, as a member of group `into` (`None` = top
/// level). `before: None` puts it last: at the end of `into`, or before Main.
/// `before` must be a track of that same level (its parent is `into`).
///
/// A track that followed its group (it fed it, or fed Main) now feeds its new
/// group instead, or Main; one routed on its own keeps its route. `None`
/// when nothing would change, for Main or unknown tracks, for a group moved
/// into itself, for `into` that is not a group or folder, or when the
/// result would not be a valid project edit.
pub fn move_track(p: &Project, id: TrackId, before: Option<TrackId>, into: Option<TrackId>) -> Option<Vec<Command>> {
    let t = p.track(id).filter(|t| t.kind != TrackKind::Main)?;
    let block: Vec<TrackId> = std::iter::once(id).chain(groups::descendants(p, id)).collect();
    if let Some(g) = into {
        let gt = p.track(g)?;
        if !matches!(gt.kind, TrackKind::Group | TrackKind::Folder) || block.contains(&g) {
            return None;
        }
    }
    let rest: Vec<TrackId> = p.tracks.iter().map(|t| t.id).filter(|x| !block.contains(x)).collect();
    // The track the block lands in front of; `None` = at the very end.
    let anchor = match (before, into) {
        (Some(b), _) => {
            if block.contains(&b) || p.track(b)?.parent != into {
                return None;
            }
            Some(b)
        }
        (None, Some(g)) => {
            let last = rest.iter().rposition(|&x| groups::within(p, x, g))?;
            rest.get(last + 1).copied()
        }
        (None, None) => p.tracks.last().filter(|t| t.kind == TrackKind::Main).map(|t| t.id),
    };

    let mut scratch = p.clone();
    let mut cmds = Vec::new();
    for &b in &block {
        let from = scratch.track_index(b)?;
        let to = match anchor {
            Some(a) => {
                let at = scratch.track_index(a)?;
                if at > from { at - 1 } else { at }
            }
            None => scratch.tracks.len() - 1,
        };
        if from != to {
            let mv = Command::MoveTrack { id: b, to };
            mv.clone().apply(&mut scratch).ok()?;
            cmds.push(mv);
        }
    }
    if t.parent != into {
        cmds.push(Command::SetTrack { id, change: TrackProp::Parent(into) });
        if groups::follows_group(t.output, t.parent) {
            let output = groups::bus_output(p, into);
            if output != t.output {
                cmds.push(Command::SetTrack { id, change: TrackProp::Output(output) });
            }
        }
        for c in &cmds[cmds.len().saturating_sub(2)..] {
            if matches!(c, Command::SetTrack { .. }) {
                c.clone().apply(&mut scratch).ok()?;
            }
        }
    }
    (!cmds.is_empty()).then_some(cmds)
}

/// [`move_track`] in front of `before`, at its level (in its group, if it is in one).
pub fn move_track_before(p: &Project, id: TrackId, before: TrackId) -> Option<Vec<Command>> {
    move_track(p, id, Some(before), p.track(before)?.parent)
}

/// Give a track a new colour; `None` when it already has it or is unknown.
pub fn set_track_color(p: &Project, id: TrackId, color: Color) -> Option<Command> {
    (p.track(id)?.color != color).then_some(Command::SetTrack { id, change: TrackProp::Color(color) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, DeviceSlot, TrackProp};
    use crate::groups;
    use crate::history::{History, Origin};
    use crate::time::PPQ;
    use std::collections::BTreeSet;

    fn add(p: &mut Project, kind: TrackKind, name: &str) -> TrackId {
        let id = p.alloc_track_id();
        let at = p.tracks.len() - 1; // before Main
        Command::AddTrack { track: Box::new(Track::new(id, kind, name, Color::palette(3))), index: at }.apply(p).unwrap();
        id
    }

    fn names(p: &Project) -> Vec<String> {
        p.content_tracks().map(|t| format!("{}{}", "  ".repeat(groups::depth(p, t.id)), t.name)).collect()
    }

    /// An instrument track with everything a track can hold.
    struct Rich {
        p: Project,
        track: TrackId,
        key: TrackId,
        notes_source: SourceId,
        audio_source: SourceId,
        inner: DeviceId,
        comp: DeviceId,
    }

    fn rich() -> Rich {
        let mut p = Project::new("t");
        let track = add(&mut p, TrackKind::Instrument, "Lead");
        let ret = add(&mut p, TrackKind::Return, "Verb");
        let key = add(&mut p, TrackKind::Audio, "Kick");
        let set = |p: &mut Project, change| {
            Command::SetTrack { id: track, change }.apply(p).unwrap();
        };
        set(&mut p, TrackProp::Sends(vec![Send { target: ret, level_db: -8.0, pre_fader: false }]));
        set(&mut p, TrackProp::VolumeDb(-4.0));
        set(&mut p, TrackProp::Arm(true));
        // Devices: a rack with a nested EQ (macro mapped to it) and a compressor keyed from Kick.
        let (rack, inner, comp) = (p.alloc_device_id(), p.alloc_device_id(), p.alloc_device_id());
        let mut r = Rack::new(RackKind::Audio);
        let mut chain = Chain::new("A", Color::palette(2));
        let mut eq = Device::stock(inner, "fx.eq", "EQ");
        eq.params.insert(2, 0.75);
        chain.devices.push(eq);
        r.chains.push(chain);
        r.macros[0].mappings.push(MacroMapping { device: inner, param: 2, min: 0.0, max: 1.0 });
        let mut rd = Device::stock(rack, "rack.audio", "Rack");
        rd.kind = DeviceKind::Rack(r);
        let mut cd = Device::stock(comp, "fx.compressor", "Comp");
        cd.state = Some(vec![1, 2, 3]);
        for (i, d) in [rd, cd].into_iter().enumerate() {
            Command::AddDevice { slot: DeviceSlot::Track { track, index: i }, device: Box::new(d) }.apply(&mut p).unwrap();
        }
        Command::SetSidechain { device: comp, source: Some(key) }.apply(&mut p).unwrap();
        // Automation on a rack-nested parameter, the volume and a macro.
        let lane = |target| {
            let mut l = AutomationLane::new(target);
            l.insert(AutomationPoint { tick: 0, value: 0.2, curve: 0.0 });
            l
        };
        for target in [
            AutomationTarget::DeviceParam { device: inner, param: 2 },
            AutomationTarget::Macro { device: rack, index: 0 },
            AutomationTarget::Volume,
        ] {
            Command::SetAutomation { track, target, lane: Some(lane(target)) }.apply(&mut p).unwrap();
        }
        // Content: a notes source used by two clips (one placed twice, one in a Session slot)
        // and an audio source.
        let notes_source = p.alloc_source_id();
        let notes = (0..3).map(|i| Note::new(p.alloc_note_id(), 60 + i, i as i64 * PPQ, PPQ / 2, 0.8)).collect();
        Command::AddSource { id: notes_source, source: Box::new(Source::Notes(NoteSource { notes, ..Default::default() })) }.apply(&mut p).unwrap();
        let audio_source = p.alloc_source_id();
        let audio = AudioSource { path: "a.wav".into(), hash: None, sample_rate: 48_000, channels: 2, frames: 48_000, warp: Warp::default(), key: None };
        Command::AddSource { id: audio_source, source: Box::new(Source::Audio(audio)) }.apply(&mut p).unwrap();
        let (c1, c2, c3) = (p.alloc_clip_id(), p.alloc_clip_id(), p.alloc_clip_id());
        for (c, source, name) in [(c1, notes_source, "A"), (c2, notes_source, "B"), (c3, audio_source, "Smp")] {
            Command::AddClip { id: c, clip: Box::new(Clip::new(name, Color::palette(5), source, 2 * PPQ)) }.apply(&mut p).unwrap();
        }
        for (clip, start) in [(c1, 0), (c1, 4 * PPQ), (c3, 8 * PPQ)] {
            let id = p.alloc_placement_id();
            Command::PlaceClip { track, placement: Placement { id, clip, start, lane: 0 } }.apply(&mut p).unwrap();
        }
        let scene = p.scenes[0].id;
        Command::SetSessionSlot { track, scene, clip: Some(c2) }.apply(&mut p).unwrap();
        Rich { p, track, key, notes_source, audio_source, inner, comp }
    }

    /// Every id of any kind that appears inside a track (not the sources and clips it uses).
    fn track_ids(t: &Track) -> BTreeSet<u64> {
        let mut ids = BTreeSet::from([t.id.0]);
        visit_devices(&t.devices, &mut |d| {
            ids.insert(d.id.0);
        });
        ids.extend(t.arrangement.iter().map(|pl| pl.id.0));
        ids
    }

    #[test]
    fn duplicating_a_track_points_modulators_at_the_copied_devices() {
        let mut r = rich();
        let m = r.p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Track { track: r.track, index: 2 }, device: Box::new(Device::stock(m, "mod.lfo", "LFO")) }.apply(&mut r.p).unwrap();
        for target in [ModTarget::Param { device: r.comp, param: 1 }, ModTarget::Param { device: r.inner, param: 2 }, ModTarget::Pan] {
            Command::AddModMapping { device: m, index: 9, mapping: ModMapping { target, min: 0.0, max: 0.5 } }.apply(&mut r.p).unwrap();
        }
        let dup = duplicate_track(&mut r.p, r.track).expect("duplicates");
        let mut p = r.p.clone();
        Command::Batch { commands: dup.commands }.apply(&mut p).unwrap();
        let copy = p.track(dup.track).unwrap();
        let DeviceKind::Rack(rack) = &copy.devices[0].kind else { panic!("rack") };
        let (new_inner, new_comp) = (rack.chains[0].devices[0].id, copy.devices[1].id);
        let new_mod = copy.devices.iter().find(|d| d.is_modulator()).unwrap();
        let targets: Vec<ModTarget> = new_mod.mods.iter().map(|x| x.target).collect();
        assert_eq!(targets, vec![ModTarget::Param { device: new_comp, param: 1 }, ModTarget::Param { device: new_inner, param: 2 }, ModTarget::Pan]);
        assert_eq!(p.device(m).unwrap().mods[0].target, ModTarget::Param { device: r.comp, param: 1 }, "the original is untouched");
    }

    #[test]
    fn duplicating_a_track_copies_devices_clips_notes_and_automation_with_new_ids() {
        let mut r = rich();
        let before = r.p.clone();
        let dup = duplicate_track(&mut r.p, r.track).expect("duplicates");
        let mut h = History::default();
        let mut p = before.clone();
        h.apply(&mut p, "Duplicate track", Origin::User, dup.commands.clone()).unwrap();

        let orig = p.track(r.track).unwrap().clone();
        let copy = p.track(dup.track).expect("the copy exists").clone();
        assert_eq!(dup.tracks, vec![dup.track]);
        assert_ne!(copy.id, orig.id);
        // It sits right after the original, named as a copy, not armed.
        assert_eq!(p.track_index(copy.id), Some(p.track_index(orig.id).unwrap() + 1));
        assert_eq!(copy.name, "Lead copy");
        assert!(orig.arm && !copy.arm);
        assert_eq!((copy.volume_db, copy.kind, copy.color, copy.sends.clone()), (orig.volume_db, orig.kind, orig.color, orig.sends.clone()));
        // No id is shared between the two tracks.
        assert!(track_ids(&orig).is_disjoint(&track_ids(&copy)));
        // Devices: same shape and params, new ids, the rack's macro and the sidechain follow.
        assert_eq!(copy.devices.len(), 2);
        let (DeviceKind::Rack(or), DeviceKind::Rack(cr)) = (&orig.devices[0].kind, &copy.devices[0].kind) else { panic!("rack") };
        let new_inner = cr.chains[0].devices[0].id;
        assert_ne!(new_inner, r.inner);
        assert_eq!(cr.chains[0].devices[0].params, or.chains[0].devices[0].params);
        assert_eq!(cr.macros[0].mappings[0].device, new_inner, "the macro drives the copy's EQ");
        assert_eq!(or.macros[0].mappings[0].device, r.inner, "the original is untouched");
        assert_eq!(copy.devices[1].sidechain, Some(r.key), "still keyed from Kick");
        assert_eq!(copy.devices[1].state, Some(vec![1, 2, 3]));
        assert_ne!(copy.devices[1].id, r.comp);
        // Automation follows the copied devices.
        let targets: Vec<AutomationTarget> = copy.automation.iter().map(|l| l.target).collect();
        assert_eq!(
            targets,
            vec![
                AutomationTarget::DeviceParam { device: new_inner, param: 2 },
                AutomationTarget::Macro { device: copy.devices[0].id, index: 0 },
                AutomationTarget::Volume,
            ]
        );
        // Clips: three placements, the twice-placed clip is still one clip, nothing is shared with the original.
        assert_eq!(copy.arrangement.len(), 3);
        let (a, b, c) = (&copy.arrangement[0], &copy.arrangement[1], &copy.arrangement[2]);
        assert_eq!(a.clip, b.clip, "linked placements stay linked");
        assert_eq!([a.start, b.start, c.start], [0, 4 * PPQ, 8 * PPQ]);
        assert!(orig.arrangement.iter().all(|o| o.clip != a.clip && o.clip != c.clip));
        let slot = *copy.session_slots.values().next().expect("the Session slot is copied");
        assert!(orig.session_slots.values().all(|o| *o != slot));
        assert_eq!(p.clips[&slot].name, "B");
        // Notes: the copy's notes live in their own source (new note ids); audio shares its file.
        let new_notes = p.clips[&a.clip].source;
        assert_ne!(new_notes, r.notes_source);
        assert_eq!(p.clips[&slot].source, new_notes, "two clips on one notes source still share the copy");
        let (Source::Notes(n_old), Source::Notes(n_new)) = (p.sources[&r.notes_source].clone(), p.sources[&new_notes].clone()) else { panic!("notes") };
        assert_eq!(n_new.notes.len(), 3);
        assert!(n_old.notes.iter().all(|o| n_new.notes.iter().all(|n| n.id != o.id)));
        assert_eq!(n_new.notes.iter().map(|n| (n.key, n.start)).collect::<Vec<_>>(), n_old.notes.iter().map(|n| (n.key, n.start)).collect::<Vec<_>>());
        assert_eq!(p.clips[&c.clip].source, r.audio_source);
        // Editing the copy's notes leaves the original alone.
        let moved = Note { key: 90, ..n_new.notes[0].clone() };
        Command::UpdateNotes { source: new_notes, notes: vec![moved] }.apply(&mut p).unwrap();
        let Source::Notes(n_old2) = &p.sources[&r.notes_source] else { panic!() };
        assert_eq!(*n_old2, n_old);
        // Ids keep growing past everything the copy used.
        let max = p.alloc_track_id().0;
        assert!(max > copy.id.0 && max > new_inner.0);
        // One undo takes the whole copy back.
        let mut p = before.clone();
        let mut h = History::default();
        h.apply(&mut p, "Duplicate track", Origin::User, dup.commands).unwrap();
        h.undo(&mut p).unwrap();
        let mut expect = before;
        expect.ids = p.ids.clone();
        assert_eq!(p, expect);
    }

    #[test]
    fn duplicating_a_group_copies_its_members_and_keeps_them_inside_the_copy() {
        let mut p = Project::new("t");
        let a = add(&mut p, TrackKind::Audio, "A");
        let b = add(&mut p, TrackKind::Audio, "B");
        let _after = add(&mut p, TrackKind::Audio, "After");
        let g = p.alloc_track_id();
        for c in groups::group_commands(&p, &[a, b], g).unwrap() {
            c.apply(&mut p).unwrap();
        }
        assert_eq!(names(&p), ["Group", "  A", "  B", "After"]);
        let dup = duplicate_track(&mut p, g).unwrap();
        for c in dup.commands {
            c.apply(&mut p).unwrap();
        }
        assert_eq!(names(&p), ["Group", "  A", "  B", "Group copy", "  A", "  B", "After"]);
        assert_eq!(dup.tracks.len(), 3);
        let g2 = dup.track;
        let members: Vec<&Track> = p.tracks.iter().filter(|t| t.parent == Some(g2)).collect();
        assert_eq!(members.len(), 2);
        assert!(members.iter().all(|m| m.output == Output::Track(g2)), "members feed the copied group, not the original");
        assert_eq!(p.track(g2).unwrap().output, Output::Main);
        assert_eq!(groups::descendants(&p, g).len(), 2, "the original group keeps its own members");
        assert!(crate::routing::dangling_references(&p).is_empty());
    }

    #[test]
    fn a_member_copy_joins_the_same_group_and_main_cannot_be_duplicated() {
        let mut p = Project::new("t");
        let a = add(&mut p, TrackKind::Audio, "A");
        let g = p.alloc_track_id();
        for c in groups::group_commands(&p, &[a], g).unwrap() {
            c.apply(&mut p).unwrap();
        }
        let dup = duplicate_track(&mut p, a).unwrap();
        for c in dup.commands {
            c.apply(&mut p).unwrap();
        }
        assert_eq!(names(&p), ["Group", "  A", "  A copy"]);
        assert_eq!(p.track(dup.track).unwrap().output, Output::Track(g));
        let main = p.main_track().id;
        assert!(duplicate_track(&mut p, main).is_none());
        assert!(duplicate_track(&mut p, TrackId(999)).is_none());
    }

    fn flat() -> (Project, Vec<TrackId>) {
        let mut p = Project::new("t");
        let ids = ["A", "B", "C", "D"].map(|n| add(&mut p, TrackKind::Audio, n)).to_vec();
        (p, ids)
    }

    fn apply_all(p: &mut Project, cmds: Vec<Command>) {
        let mut h = History::default();
        h.apply(p, "Move", Origin::User, cmds).unwrap();
    }

    #[test]
    fn moving_a_track_reorders_and_undoes() {
        let (mut p, ids) = flat();
        let before = p.clone();
        let mut h = History::default();
        // D to the front, A to the end, B past C.
        let cmds = move_track(&p, ids[3], Some(ids[0]), None).unwrap();
        h.apply(&mut p, "Move", Origin::User, cmds).unwrap();
        assert_eq!(names(&p), ["D", "A", "B", "C"]);
        let cmds = move_track(&p, ids[0], None, None).unwrap();
        h.apply(&mut p, "Move", Origin::User, cmds).unwrap();
        assert_eq!(names(&p), ["D", "B", "C", "A"]);
        assert_eq!(p.main_track().id, before.main_track().id);
        assert_eq!(p.tracks.last().unwrap().kind, TrackKind::Main, "Main stays last");
        let cmds = move_track_before(&p, ids[1], ids[0]).unwrap();
        h.apply(&mut p, "Move", Origin::User, cmds).unwrap();
        assert_eq!(names(&p), ["D", "C", "B", "A"]);
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p.tracks, before.tracks);
        // Nothing to do: already there, Main, or an unknown track.
        assert!(move_track(&p, ids[0], Some(ids[1]), None).is_none(), "A is already before B");
        assert!(move_track(&p, ids[0], Some(ids[0]), None).is_none());
        assert!(move_track(&p, p.main_track().id, None, None).is_none());
        assert!(move_track(&p, TrackId(999), None, None).is_none());
    }

    #[test]
    fn moving_a_group_takes_its_members_along() {
        let (mut p, ids) = flat();
        let g = p.alloc_track_id();
        let cmds = groups::group_commands(&p, &[ids[0], ids[1]], g).unwrap();
        apply_all(&mut p, cmds);
        assert_eq!(names(&p), ["Group", "  A", "  B", "C", "D"]);
        let cmds = move_track(&p, g, None, None).unwrap();
        apply_all(&mut p, cmds);
        assert_eq!(names(&p), ["C", "D", "Group", "  A", "  B"]);
        let cmds = move_track(&p, g, Some(ids[3]), None).unwrap();
        apply_all(&mut p, cmds);
        assert_eq!(names(&p), ["C", "Group", "  A", "  B", "D"]);
    }

    #[test]
    fn moving_into_and_out_of_a_group_changes_parent_and_output() {
        let (mut p, ids) = flat();
        let g = p.alloc_track_id();
        let cmds = groups::group_commands(&p, &[ids[0], ids[1]], g).unwrap();
        apply_all(&mut p, cmds);
        // C into the group, between A and B.
        let cmds = move_track(&p, ids[2], Some(ids[1]), Some(g)).unwrap();
        apply_all(&mut p, cmds);
        assert_eq!(names(&p), ["Group", "  A", "  C", "  B", "D"]);
        let c = p.track(ids[2]).unwrap();
        assert_eq!((c.parent, c.output), (Some(g), Output::Track(g)));
        // D to the end of the group.
        let cmds = move_track(&p, ids[3], None, Some(g)).unwrap();
        apply_all(&mut p, cmds);
        assert_eq!(names(&p), ["Group", "  A", "  C", "  B", "  D"]);
        // A back out, to the top level; it follows the group no more.
        let cmds = move_track(&p, ids[0], None, None).unwrap();
        apply_all(&mut p, cmds);
        assert_eq!(names(&p), ["Group", "  C", "  B", "  D", "A"]);
        let a = p.track(ids[0]).unwrap();
        assert_eq!((a.parent, a.output), (None, Output::Main));
        // A track routed on its own keeps its route when it changes group.
        Command::SetTrack { id: ids[2], change: TrackProp::Output(Output::Hardware(2)) }.apply(&mut p).unwrap();
        let cmds = move_track(&p, ids[2], None, None).unwrap();
        apply_all(&mut p, cmds);
        assert_eq!(p.track(ids[2]).unwrap().output, Output::Hardware(2));
        assert!(crate::routing::dangling_references(&p).is_empty());
    }

    #[test]
    fn a_move_that_breaks_the_groups_is_refused() {
        let (mut p, ids) = flat();
        let g = p.alloc_track_id();
        let cmds = groups::group_commands(&p, &[ids[0], ids[1]], g).unwrap();
        apply_all(&mut p, cmds);
        let inner = p.alloc_track_id();
        let cmds = groups::group_commands(&p, &[ids[1]], inner).unwrap();
        apply_all(&mut p, cmds);
        assert_eq!(names(&p), ["Group", "  A", "  Group", "    B", "C", "D"]);
        // A group into itself or into something inside it.
        assert!(move_track(&p, g, None, Some(g)).is_none());
        assert!(move_track(&p, g, None, Some(inner)).is_none());
        // Into a track that is not a group; before a track that is not in that group.
        assert!(move_track(&p, ids[3], None, Some(ids[2])).is_none());
        assert!(move_track(&p, ids[3], Some(ids[0]), None).is_none(), "A is inside the group, not top level");
        assert!(move_track(&p, ids[3], Some(ids[2]), Some(g)).is_none());
        // Before one of its own members.
        assert!(move_track(&p, g, Some(ids[1]), None).is_none());
    }

    #[test]
    fn colours_come_as_commands_that_undo() {
        let (mut p, ids) = flat();
        let before = p.clone();
        let blue = Color(0x5B8DEF);
        let cmd = set_track_color(&p, ids[1], blue).unwrap();
        let mut h = History::default();
        h.apply(&mut p, "Track colour", Origin::User, vec![cmd]).unwrap();
        assert_eq!(p.track(ids[1]).unwrap().color, blue);
        assert!(set_track_color(&p, ids[1], blue).is_none(), "already that colour");
        assert!(set_track_color(&p, TrackId(999), blue).is_none());
        h.undo(&mut p).unwrap();
        assert_eq!(p.tracks, before.tracks);

        let r = rich();
        let clip = *r.p.clips.keys().next().unwrap();
        let mut p = r.p.clone();
        let cmd = crate::ops::set_clip_color(&p, clip, blue).unwrap();
        let undo = Command::Batch { commands: vec![cmd] }.apply(&mut p).unwrap();
        assert_eq!(p.clips[&clip].color, blue);
        assert!(crate::ops::set_clip_color(&p, clip, blue).is_none());
        assert!(crate::ops::set_clip_color(&p, ClipId(999), blue).is_none());
        undo.apply(&mut p).unwrap();
        assert_eq!(p, r.p);
    }

    // ── duplicate_devices ──

    struct Chains {
        p: Project,
        track: TrackId,
        key: TrackId,
        eq: DeviceId,
        rack: DeviceId,
        comp: DeviceId,
        filter: DeviceId,
        delay: DeviceId,
    }

    /// Track chain [EQ, Rack{A: [Comp keyed from Kick, Filter]}, Delay]; a macro drives the comp.
    fn chains() -> Chains {
        let mut p = Project::new("t");
        let track = add(&mut p, TrackKind::Audio, "T");
        let key = add(&mut p, TrackKind::Audio, "Kick");
        let (eq, rack, comp, filter, delay) = (p.alloc_device_id(), p.alloc_device_id(), p.alloc_device_id(), p.alloc_device_id(), p.alloc_device_id());
        let mut c = Device::stock(comp, "fx.compressor", "Comp");
        c.params.insert(1, 0.4);
        c.state = Some(vec![9, 9]);
        let mut chain = Chain::new("A", Color::palette(2));
        chain.devices = vec![c, Device::stock(filter, "fx.filter", "Filter")];
        let mut r = Rack::new(RackKind::Audio);
        r.chains.push(chain);
        r.macros[3].mappings.push(MacroMapping { device: comp, param: 1, min: 0.0, max: 1.0 });
        let mut rd = Device::stock(rack, "rack.audio", "Rack");
        rd.kind = DeviceKind::Rack(r);
        for (i, d) in [Device::stock(eq, "fx.eq", "EQ"), rd, Device::stock(delay, "fx.delay", "Delay")].into_iter().enumerate() {
            Command::AddDevice { slot: DeviceSlot::Track { track, index: i }, device: Box::new(d) }.apply(&mut p).unwrap();
        }
        Command::SetSidechain { device: comp, source: Some(key) }.apply(&mut p).unwrap();
        Chains { p, track, key, eq, rack, comp, filter, delay }
    }

    fn chain_ids(p: &Project, track: TrackId) -> Vec<DeviceId> {
        p.track(track).unwrap().devices.iter().map(|d| d.id).collect()
    }

    fn all_ids(devices: &[Device]) -> BTreeSet<u64> {
        let mut ids = BTreeSet::new();
        visit_devices(devices, &mut |d| {
            ids.insert(d.id.0);
        });
        ids
    }

    #[test]
    fn duplicating_a_rack_copies_chains_macros_and_nested_devices_with_new_ids() {
        let mut c = chains();
        let before = c.p.clone();
        let dup = duplicate_devices(&mut c.p, &[c.rack]).expect("duplicates");
        let mut p = before.clone();
        let mut h = History::default();
        h.apply(&mut p, "Duplicate device", Origin::User, dup.commands.clone()).unwrap();
        let ids = chain_ids(&p, c.track);
        assert_eq!(ids.len(), 4);
        assert_eq!((ids[0], ids[1], ids[3]), (c.eq, c.rack, c.delay));
        assert_eq!(dup.devices, vec![ids[2]], "the copy sits right after the original");
        let copy = p.track(c.track).unwrap().devices[2].clone();
        let orig = p.track(c.track).unwrap().devices[1].clone();
        assert!(all_ids(std::slice::from_ref(&copy)).is_disjoint(&all_ids(std::slice::from_ref(&orig))));
        let (DeviceKind::Rack(cr), DeviceKind::Rack(or)) = (&copy.kind, &orig.kind) else { panic!("rack") };
        let nested = &cr.chains[0].devices;
        assert_eq!((nested.len(), nested[0].name.as_str(), nested[0].params.get(&1)), (2, "Comp", Some(&0.4)));
        assert_eq!(nested[0].state, Some(vec![9, 9]));
        assert_eq!(cr.macros[3].mappings[0].device, nested[0].id, "the copy's macro drives the copy's comp");
        assert_eq!(or.macros[3].mappings[0].device, c.comp, "the original is untouched");
        assert_eq!(nested[0].sidechain, Some(c.key), "the sidechain is kept");
        assert_eq!(cr.chains[0].name, "A");
        // One undo takes it back; ids still grow.
        h.undo(&mut p).unwrap();
        let mut expect = before;
        expect.ids = p.ids.clone();
        assert_eq!(p, expect);
        assert!(p.alloc_device_id().0 > nested[1].id.0);
    }

    #[test]
    fn several_devices_each_get_their_copy_next_to_them() {
        let mut c = chains();
        let dup = duplicate_devices(&mut c.p, &[c.delay, c.eq]).unwrap();
        for cmd in dup.commands {
            cmd.apply(&mut c.p).unwrap();
        }
        let names: Vec<String> = c.p.track(c.track).unwrap().devices.iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, ["EQ", "EQ", "Rack", "Delay", "Delay"]);
        assert_eq!(dup.devices.len(), 2);
        assert_eq!(chain_ids(&c.p, c.track)[1], dup.devices[0], "listed in chain order: EQ first");
        assert_eq!(chain_ids(&c.p, c.track)[4], dup.devices[1]);
    }

    #[test]
    fn a_device_inside_a_rack_is_copied_into_the_same_chain() {
        let mut c = chains();
        let dup = duplicate_devices(&mut c.p, &[c.comp, c.rack, c.filter]).unwrap();
        // The rack is selected, so the devices inside it ride along instead of being copied twice.
        assert_eq!(dup.devices.len(), 1);
        let dup = duplicate_devices(&mut c.p, &[c.comp]).unwrap();
        for cmd in dup.commands {
            cmd.apply(&mut c.p).unwrap();
        }
        let DeviceKind::Rack(r) = &c.p.device(c.rack).unwrap().kind else { panic!() };
        let names: Vec<&str> = r.chains[0].devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Comp", "Comp", "Filter"]);
        assert_eq!(r.chains[0].devices[1].id, dup.devices[0]);
        assert_eq!(r.chains[0].devices[1].sidechain, Some(c.key));
    }

    #[test]
    fn nothing_to_duplicate_for_unknown_or_no_devices() {
        let mut c = chains();
        assert!(duplicate_devices(&mut c.p, &[]).is_none());
        assert!(duplicate_devices(&mut c.p, &[DeviceId(9999)]).is_none());
        let one = duplicate_devices(&mut c.p, &[DeviceId(9999), c.eq]).unwrap();
        assert_eq!(one.devices.len(), 1);
    }

    // ── ids introduced by commands ──

    /// Every id in use anywhere in the project.
    fn every_id(p: &Project) -> BTreeSet<u64> {
        let mut ids = BTreeSet::new();
        for t in &p.tracks {
            ids.extend(track_ids(t));
        }
        ids.extend(p.scenes.iter().map(|s| s.id.0));
        ids.extend(p.clips.keys().map(|c| c.0));
        for (id, s) in &p.sources {
            ids.insert(id.0);
            if let Source::Notes(n) = s {
                ids.extend(n.notes.iter().map(|n| n.id.0));
            }
        }
        ids
    }

    fn next_ids(p: &mut Project) -> Vec<u64> {
        vec![
            p.alloc_track_id().0,
            p.alloc_device_id().0,
            p.alloc_source_id().0,
            p.alloc_clip_id().0,
            p.alloc_note_id().0,
            p.alloc_placement_id().0,
            p.alloc_scene_id().0,
        ]
    }

    #[test]
    fn a_duplicate_worked_out_on_a_copy_keeps_the_real_allocator_ahead() {
        let r = rich();
        let mut scratch = r.p.clone();
        let dup = duplicate_track(&mut scratch, r.track).unwrap();
        // The real project never saw the allocations.
        let mut p = r.p.clone();
        let mut h = History::default();
        h.apply(&mut p, "Duplicate track", Origin::User, dup.commands).unwrap();
        let used = every_id(&p);
        let top = *used.iter().max().unwrap();
        for id in next_ids(&mut p) {
            assert!(id > top && !used.contains(&id), "{id} is already in use (top {top})");
        }
        // The next device goes in without a DuplicateId.
        let d = p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Track { track: dup.track, index: 0 }, device: Box::new(Device::stock(d, "fx.eq", "EQ")) }.apply(&mut p).unwrap();
        // Undo and redo never take the allocator back below what the copy used.
        h.undo(&mut p).unwrap();
        for id in next_ids(&mut p) {
            assert!(id > top, "{id} reuses an id of the undone copy");
        }
        h.redo(&mut p).unwrap();
        let used = every_id(&p);
        for id in next_ids(&mut p) {
            assert!(!used.contains(&id));
        }
    }

    #[test]
    fn a_group_duplicate_on_a_copy_is_safe_too() {
        let mut p = Project::new("t");
        let a = add(&mut p, TrackKind::Audio, "A");
        let g = p.alloc_track_id();
        let cmds = groups::group_commands(&p, &[a], g).unwrap();
        let mut h = History::default();
        h.apply(&mut p, "Group", Origin::User, cmds).unwrap();
        let d = p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Track { track: a, index: 0 }, device: Box::new(Device::stock(d, "fx.eq", "EQ")) }.apply(&mut p).unwrap();
        let mut scratch = p.clone();
        let dup = duplicate_track(&mut scratch, g).unwrap();
        h.apply(&mut p, "Duplicate", Origin::User, dup.commands).unwrap();
        let used = every_id(&p);
        let ids = next_ids(&mut p);
        assert!(ids.iter().all(|i| !used.contains(i)));
    }

    #[test]
    fn duplicating_a_device_on_a_copy_keeps_the_allocator_ahead() {
        let c = chains();
        let mut scratch = c.p.clone();
        let dup = duplicate_devices(&mut scratch, &[c.rack]).unwrap();
        let mut p = c.p.clone();
        let mut h = History::default();
        h.apply(&mut p, "Duplicate device", Origin::User, dup.commands).unwrap();
        let used = every_id(&p);
        for id in next_ids(&mut p) {
            assert!(!used.contains(&id), "{id} collides");
        }
        h.undo(&mut p).unwrap();
        h.redo(&mut p).unwrap();
        let used = every_id(&p);
        assert!(next_ids(&mut p).iter().all(|i| !used.contains(i)));
    }

    #[test]
    fn copies_of_a_rack_and_of_a_track_carry_their_return_devices_with_new_ids() {
        let mut c = chains();
        // Give the rack a Drum kind with a return holding a delay keyed from Kick and mapped by a macro.
        let delay = c.p.alloc_device_id();
        {
            let Some(Device { kind: DeviceKind::Rack(r), .. }) = c.p.device_mut(c.rack) else { panic!() };
            r.kind = RackKind::Drum;
            let mut ret = Chain::new("Verb", Color(1));
            ret.devices.push(Device::stock(delay, "fx.delay", "Delay"));
            r.returns.push(ret);
            r.macros[1].mappings.push(MacroMapping { device: delay, param: 0, min: 0.0, max: 1.0 });
        }
        Command::SetSidechain { device: delay, source: Some(c.key) }.apply(&mut c.p).unwrap();
        let ids_in = |p: &Project, d: DeviceId| -> BTreeSet<u64> {
            let mut s = BTreeSet::new();
            visit_devices(std::slice::from_ref(p.device(d).unwrap()), &mut |x| {
                s.insert(x.id.0);
            });
            s
        };
        // The device copy.
        let mut scratch = c.p.clone();
        let dup = duplicate_devices(&mut scratch, &[c.rack]).unwrap();
        let mut p = c.p.clone();
        let mut h = History::default();
        h.apply(&mut p, "Duplicate device", Origin::User, dup.commands).unwrap();
        let (orig, copy) = (ids_in(&p, c.rack), ids_in(&p, dup.devices[0]));
        assert_eq!(orig.len(), copy.len(), "returns' devices are copied too");
        assert!(orig.is_disjoint(&copy));
        let DeviceKind::Rack(cr) = &p.device(dup.devices[0]).unwrap().kind else { panic!() };
        let new_delay = cr.returns[0].devices[0].id;
        assert_eq!(cr.macros[1].mappings[0].device, new_delay, "the macro drives the copy");
        assert_eq!(cr.returns[0].devices[0].sidechain, Some(c.key));
        h.undo(&mut p).unwrap();
        let mut expect = c.p.clone();
        expect.ids = p.ids.clone();
        assert_eq!(p, expect);
        // A device inside a return is copied next to itself, in the return.
        let mut scratch = c.p.clone();
        let dup = duplicate_devices(&mut scratch, &[delay]).unwrap();
        let mut p = c.p.clone();
        for cmd in dup.commands {
            cmd.apply(&mut p).unwrap();
        }
        let DeviceKind::Rack(r) = &p.device(c.rack).unwrap().kind else { panic!() };
        assert_eq!(r.returns[0].devices.len(), 2);
        assert_eq!(r.returns[0].devices[1].id, dup.devices[0]);
        // The whole track.
        let mut scratch = c.p.clone();
        let dup = duplicate_track(&mut scratch, c.track).unwrap();
        let mut p = c.p.clone();
        for cmd in dup.commands {
            cmd.apply(&mut p).unwrap();
        }
        let before = every_id(&c.p);
        let after = track_ids(p.track(dup.track).unwrap());
        assert!(after.is_disjoint(&before));
        let all = every_id(&p);
        assert!(next_ids(&mut p).iter().all(|i| !all.contains(i)));
    }
}
