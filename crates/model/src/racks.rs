//! Device racks as pure command builders: grouping a run of devices into a
//! new rack (Live's Cmd+G in the Device View) and dissolving a rack back
//! into the chain it sits in, plus the lookups the device view asks (where
//! a device sits, what a chain holds).
//!
//! A rack built by grouping has one chain holding the grouped devices in
//! their order. It is an instrument rack when one of them is an instrument,
//! a MIDI Effect Rack when all of them are MIDI effects, an audio rack otherwise.
//!
//! Both directions *move* the devices (`Command::MoveDevice`) instead of
//! taking them out and adding copies back: a move keeps every macro and
//! modulator mapping that still holds after it (those of a rack the devices
//! stay inside, and those of modulators on the same track), where a removal
//! would drop them.

use crate::command::{Command, DeviceSlot};
use crate::ids::DeviceId;
use crate::project::*;

/// Which chain a slot points into, without the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Track(crate::ids::TrackId),
    Rack { rack: DeviceId, chain: usize },
    /// A Drum Rack's internal return.
    Return { rack: DeviceId, index: usize },
}

impl Container {
    pub fn of(slot: DeviceSlot) -> Self {
        match slot {
            DeviceSlot::Track { track, .. } => Self::Track(track),
            DeviceSlot::Rack { rack, chain, .. } => Self::Rack { rack, chain },
            DeviceSlot::Return { rack, index, .. } => Self::Return { rack, index },
        }
    }

    pub fn slot(self, index: usize) -> DeviceSlot {
        match self {
            Self::Track(track) => DeviceSlot::Track { track, index },
            Self::Rack { rack, chain } => DeviceSlot::Rack { rack, chain, index },
            Self::Return { rack, index: ret } => DeviceSlot::Return { rack, index: ret, pos: index },
        }
    }
}

fn index_of(slot: DeviceSlot) -> usize {
    match slot {
        DeviceSlot::Track { index, .. } | DeviceSlot::Rack { index, .. } => index,
        DeviceSlot::Return { pos, .. } => pos,
    }
}

/// Where device `id` sits: its track's chain or a chain of a rack.
pub fn device_slot(p: &Project, id: DeviceId) -> Option<DeviceSlot> {
    fn find(chain: &[Device], id: DeviceId, here: &dyn Fn(usize) -> DeviceSlot) -> Option<DeviceSlot> {
        if let Some(i) = chain.iter().position(|d| d.id == id) {
            return Some(here(i));
        }
        chain.iter().find_map(|d| match &d.kind {
            DeviceKind::Rack(r) => {
                let rack = d.id;
                r.chains
                    .iter()
                    .enumerate()
                    .find_map(|(ci, c)| find(&c.devices, id, &move |index| DeviceSlot::Rack { rack, chain: ci, index }))
                    .or_else(|| {
                        r.returns.iter().enumerate().find_map(|(ri, c)| find(&c.devices, id, &move |pos| DeviceSlot::Return { rack, index: ri, pos }))
                    })
            }
            _ => None,
        })
    }
    p.tracks.iter().find_map(|t| {
        let track = t.id;
        find(&t.devices, id, &move |index| DeviceSlot::Track { track, index })
    })
}

/// The devices of a chain.
pub fn chain(p: &Project, c: Container) -> Option<&[Device]> {
    match c {
        Container::Track(t) => p.track(t).map(|t| t.devices.as_slice()),
        Container::Rack { rack, chain } => match &p.device(rack)?.kind {
            DeviceKind::Rack(r) => r.chains.get(chain).map(|c| c.devices.as_slice()),
            _ => None,
        },
        Container::Return { rack, index } => match &p.device(rack)?.kind {
            DeviceKind::Rack(r) => r.returns.get(index).map(|c| c.devices.as_slice()),
            _ => None,
        },
    }
}

/// A rack as it comes from the Add Device menu: one empty chain, "Chain 1",
/// so it takes devices at once. A Drum Rack has no pads yet (`Rack::new`,
/// which grouping and the tests use as it is).
pub fn fresh_rack(kind: RackKind) -> Rack {
    let mut r = Rack::new(kind);
    if kind != RackKind::Drum {
        r.chains.push(Chain::new("Chain 1", Color::palette(0)));
    }
    r
}

/// The commands that put `ids` in a new rack `rack` (Cmd+G on devices).
/// The devices must all sit in one chain; the rack takes the place of the
/// first of them and keeps their order. `None` when nothing can be grouped.
///
/// The rack is added with an empty chain, named after the first device, and
/// each device is then moved into it.
pub fn group_commands(p: &Project, ids: &[DeviceId], rack: DeviceId) -> Option<Vec<Command>> {
    let mut picked: Vec<(usize, DeviceId)> = Vec::new();
    let mut home = None;
    for &id in ids {
        let slot = device_slot(p, id)?;
        match home {
            None => home = Some(Container::of(slot)),
            Some(h) if h != Container::of(slot) => return None,
            _ => {}
        }
        if !picked.iter().any(|x| x.1 == id) {
            picked.push((index_of(slot), id));
        }
    }
    let home = home?;
    picked.sort();
    let devices: Vec<&Device> = picked.iter().map(|&(_, id)| p.device(id)).collect::<Option<_>>()?;
    let first = devices.first()?;
    let all_midi = devices.iter().all(|d| matches!(&d.kind, DeviceKind::Stock { plugin_id } if plugin_id.starts_with("midi.")));
    let kind = if devices.iter().any(|d| d.is_instrument()) {
        RackKind::Instrument
    } else if all_midi {
        RackKind::Midi
    } else {
        RackKind::Audio
    };
    let mut r = Rack::new(kind);
    r.chains.push(Chain::new(first.name.clone(), Color::palette(0)));
    let (name, plugin_id) = match kind {
        RackKind::Instrument => ("Instrument Rack", "rack.instrument"),
        RackKind::Midi => ("MIDI Effect Rack", "rack.midi"),
        _ => ("Audio Effect Rack", "rack.audio"),
    };
    let device = Device { kind: DeviceKind::Rack(r), ..Device::stock(rack, plugin_id, name) };
    let mut cmds = vec![Command::AddDevice { slot: home.slot(picked[0].0), device: Box::new(device) }];
    cmds.extend(picked.iter().enumerate().map(|(k, &(_, id))| Command::MoveDevice { id, to: DeviceSlot::Rack { rack, chain: 0, index: k } }));
    Some(cmds)
}

/// Whether `id` is a rack that [`ungroup_commands`] can dissolve: a
/// non-drum rack with a single chain (with more, Live greys it out too).
pub fn can_ungroup(p: &Project, id: DeviceId) -> bool {
    matches!(p.device(id).map(|d| &d.kind), Some(DeviceKind::Rack(r)) if r.kind != RackKind::Drum && r.chains.len() <= 1)
}

/// The commands that dissolve rack `id`: its chain's devices take its place.
/// Each one is moved to the chain the rack sits in, behind the rack, and the
/// rack, now empty, is removed.
pub fn ungroup_commands(p: &Project, id: DeviceId) -> Option<Vec<Command>> {
    if !can_ungroup(p, id) {
        return None;
    }
    let slot = device_slot(p, id)?;
    let DeviceKind::Rack(r) = &p.device(id)?.kind else { return None };
    let at = index_of(slot);
    let home = Container::of(slot);
    let inner = r.chains.first().map(|c| c.devices.as_slice()).unwrap_or_default();
    let mut cmds: Vec<Command> = inner.iter().enumerate().map(|(k, d)| Command::MoveDevice { id: d.id, to: home.slot(at + 1 + k) }).collect();
    cmds.push(Command::RemoveDevice { id });
    Some(cmds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TrackId;

    fn project() -> (Project, TrackId, Vec<DeviceId>) {
        let mut p = Project::new("t");
        let t = p.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(t, TrackKind::Audio, "A", Color::palette(0))), index: 0 }.apply(&mut p).unwrap();
        let mut ids = Vec::new();
        for (i, name) in ["EQ", "Comp", "Reverb", "Limiter"].into_iter().enumerate() {
            let id = p.alloc_device_id();
            let d = Device::stock(id, "fx.eq", name);
            Command::AddDevice { slot: DeviceSlot::Track { track: t, index: i }, device: Box::new(d) }.apply(&mut p).unwrap();
            ids.push(id);
        }
        (p, t, ids)
    }

    fn names(p: &Project, t: TrackId) -> Vec<String> {
        p.track(t).unwrap().devices.iter().map(|d| d.name.clone()).collect()
    }

    fn run(p: &mut Project, cmds: Vec<Command>) -> Vec<Command> {
        cmds.into_iter().map(|c| c.apply(p).unwrap()).collect()
    }

    #[test]
    fn grouping_puts_the_devices_in_one_chain_in_order_and_undoes() {
        let (mut p, t, ids) = project();
        let before = p.clone();
        let rack = p.alloc_device_id();
        // Picked out of order: the chain keeps signal order.
        let cmds = group_commands(&p, &[ids[2], ids[1]], rack).unwrap();
        let undo = run(&mut p, cmds);
        assert_eq!(names(&p, t), ["EQ", "Audio Effect Rack", "Limiter"]);
        let DeviceKind::Rack(r) = &p.device(rack).unwrap().kind else { panic!() };
        assert_eq!(r.kind, RackKind::Audio);
        assert_eq!(r.chains[0].devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Comp", "Reverb"]);
        assert_eq!(device_slot(&p, ids[2]), Some(DeviceSlot::Rack { rack, chain: 0, index: 1 }));
        for c in undo.into_iter().rev() {
            c.apply(&mut p).unwrap();
        }
        assert_eq!(p.tracks, before.tracks);
    }

    #[test]
    fn ungrouping_puts_the_chain_back_in_place() {
        let (mut p, t, ids) = project();
        let rack = p.alloc_device_id();
        let cmds = group_commands(&p, &[ids[1], ids[2]], rack).unwrap();
        run(&mut p, cmds);
        assert!(can_ungroup(&p, rack));
        let cmds = ungroup_commands(&p, rack).unwrap();
        run(&mut p, cmds);
        assert_eq!(names(&p, t), ["EQ", "Comp", "Reverb", "Limiter"]);
        assert!(!can_ungroup(&p, ids[0]));
    }

    #[test]
    fn grouping_and_ungrouping_inside_a_rack_keep_the_macro_and_modulator_links() {
        let (mut p, t, ids) = project(); // EQ, Comp, Reverb, Limiter
        let outer = p.alloc_device_id();
        let cmds = group_commands(&p, &[ids[1], ids[2]], outer).unwrap();
        run(&mut p, cmds);
        Command::AddMapping { rack: outer, macro_index: 0, index: 0, mapping: MacroMapping { device: ids[1], param: 1, min: 0.2, max: 0.8 } }.apply(&mut p).unwrap();
        let lfo = p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Track { track: t, index: 0 }, device: Box::new(Device::stock(lfo, "mod.lfo", "LFO")) }.apply(&mut p).unwrap();
        Command::AddModMapping { device: lfo, index: 0, mapping: ModMapping::bipolar(ModTarget::Param { device: ids[2], param: 2 }, 0.3) }.apply(&mut p).unwrap();
        let before = p.clone();
        let macro_links = |p: &Project| match &p.device(outer).unwrap().kind {
            DeviceKind::Rack(r) => r.macros[0].mappings.clone(),
            _ => unreachable!(),
        };
        // Group Comp + Reverb into a rack nested in `outer`.
        let inner = p.alloc_device_id();
        let cmds = group_commands(&p, &[ids[2], ids[1]], inner).unwrap();
        let undo = run(&mut p, cmds);
        assert_eq!(device_slot(&p, inner), Some(DeviceSlot::Rack { rack: outer, chain: 0, index: 0 }));
        assert_eq!(device_slot(&p, ids[2]), Some(DeviceSlot::Rack { rack: inner, chain: 0, index: 1 }), "signal order kept");
        assert_eq!(macro_links(&p), macro_links(&before), "the outer macro still drives the Comp");
        assert_eq!(p.device(lfo).unwrap().mods, before.device(lfo).unwrap().mods, "the LFO still drives the Reverb");
        // Undo of the grouping is exact.
        let mut q = p.clone();
        for c in undo.into_iter().rev() {
            c.apply(&mut q).unwrap();
        }
        q.ids = before.ids.clone();
        assert_eq!(q, before);
        // Ungroup puts them back where the rack was, links and all.
        let cmds = ungroup_commands(&p, inner).unwrap();
        run(&mut p, cmds);
        assert_eq!(chain(&p, Container::Rack { rack: outer, chain: 0 }).unwrap().iter().map(|d| d.id).collect::<Vec<_>>(), vec![ids[1], ids[2]]);
        assert_eq!(macro_links(&p), macro_links(&before));
        assert_eq!(p.device(lfo).unwrap().mods, before.device(lfo).unwrap().mods);
        assert_eq!(p.device(inner), None);
    }

    #[test]
    fn ungrouping_drops_only_the_dissolved_racks_own_macros_and_undo_restores_them() {
        let (mut p, t, ids) = project();
        let rack = p.alloc_device_id();
        let cmds = group_commands(&p, &[ids[1]], rack).unwrap();
        run(&mut p, cmds);
        Command::AddMapping { rack, macro_index: 3, index: 0, mapping: MacroMapping { device: ids[1], param: 0, min: 0.0, max: 1.0 } }.apply(&mut p).unwrap();
        let before = p.clone();
        let cmds = ungroup_commands(&p, rack).unwrap();
        let undo = run(&mut p, cmds);
        assert_eq!(names(&p, t), ["EQ", "Comp", "Reverb", "Limiter"]);
        for c in undo.into_iter().rev() {
            c.apply(&mut p).unwrap();
        }
        p.ids = before.ids.clone();
        assert_eq!(p, before, "undo brings the rack back with its macro mapping");
    }

    #[test]
    fn a_fresh_rack_has_one_empty_chain_but_a_drum_rack_has_no_pads() {
        for kind in [RackKind::Audio, RackKind::Instrument, RackKind::Midi] {
            let r = fresh_rack(kind);
            assert_eq!((r.kind, r.chains.len(), r.chains[0].devices.len(), r.chains[0].name.as_str()), (kind, 1, 0, "Chain 1"));
        }
        assert!(fresh_rack(RackKind::Drum).chains.is_empty());
    }

    #[test]
    fn devices_from_different_chains_do_not_group() {
        let (mut p, _, ids) = project();
        let rack = p.alloc_device_id();
        let cmds = group_commands(&p, &[ids[0]], rack).unwrap();
        run(&mut p, cmds);
        assert!(group_commands(&p, &[ids[0], ids[1]], DeviceId(9999)).is_none());
        // Inside the rack a device groups into a nested rack.
        let inner = p.alloc_device_id();
        let cmds = group_commands(&p, &[ids[0]], inner).unwrap();
        run(&mut p, cmds);
        assert_eq!(device_slot(&p, inner), Some(DeviceSlot::Rack { rack, chain: 0, index: 0 }));
    }

    #[test]
    fn midi_effects_make_a_midi_effect_rack() {
        let (mut p, t, _) = project();
        let (a, b) = (p.alloc_device_id(), p.alloc_device_id());
        for (i, (id, pid)) in [(a, "midi.arp"), (b, "midi.chord")].into_iter().enumerate() {
            Command::AddDevice { slot: DeviceSlot::Track { track: t, index: i }, device: Box::new(Device::stock(id, pid, pid)) }.apply(&mut p).unwrap();
        }
        let rack = p.alloc_device_id();
        let cmds = group_commands(&p, &[a, b], rack).unwrap();
        run(&mut p, cmds);
        let d = p.device(rack).unwrap();
        assert_eq!(d.name, "MIDI Effect Rack");
        assert!(matches!(&d.kind, DeviceKind::Rack(r) if r.kind == RackKind::Midi) && !d.is_instrument());
    }

    #[test]
    fn an_instrument_makes_an_instrument_rack() {
        let (mut p, t, ids) = project();
        let synth = p.alloc_device_id();
        Command::AddDevice { slot: DeviceSlot::Track { track: t, index: 0 }, device: Box::new(Device::stock(synth, "instrument.wavetable", "Synth")) }.apply(&mut p).unwrap();
        let rack = p.alloc_device_id();
        let cmds = group_commands(&p, &[synth, ids[0]], rack).unwrap();
        run(&mut p, cmds);
        assert!(p.device(rack).unwrap().is_instrument());
    }
}
