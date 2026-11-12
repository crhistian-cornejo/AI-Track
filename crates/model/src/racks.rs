//! Device racks as pure command builders: grouping a run of devices into a
//! new rack (Live's Cmd+G in the Device View) and dissolving a rack back
//! into the chain it sits in, plus the lookups the device view asks (where
//! a device sits, what a chain holds).
//!
//! A rack built by grouping has one chain holding the grouped devices in
//! their order. It is an instrument rack when one of them is an instrument,
//! an audio rack otherwise.

use crate::command::{Command, DeviceSlot};
use crate::ids::DeviceId;
use crate::project::*;

/// Which chain a slot points into, without the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Track(crate::ids::TrackId),
    Rack { rack: DeviceId, chain: usize },
}

impl Container {
    pub fn of(slot: DeviceSlot) -> Self {
        match slot {
            DeviceSlot::Track { track, .. } => Self::Track(track),
            DeviceSlot::Rack { rack, chain, .. } => Self::Rack { rack, chain },
        }
    }

    pub fn slot(self, index: usize) -> DeviceSlot {
        match self {
            Self::Track(track) => DeviceSlot::Track { track, index },
            Self::Rack { rack, chain } => DeviceSlot::Rack { rack, chain, index },
        }
    }
}

fn index_of(slot: DeviceSlot) -> usize {
    match slot {
        DeviceSlot::Track { index, .. } | DeviceSlot::Rack { index, .. } => index,
    }
}

/// Where device `id` sits: its track's chain or a chain of a rack.
pub fn device_slot(p: &Project, id: DeviceId) -> Option<DeviceSlot> {
    fn find(chain: &[Device], id: DeviceId, here: &dyn Fn(usize) -> DeviceSlot) -> Option<DeviceSlot> {
        if let Some(i) = chain.iter().position(|d| d.id == id) {
            return Some(here(i));
        }
        chain.iter().find_map(|d| match &d.kind {
            DeviceKind::Rack(r) => r.chains.iter().enumerate().find_map(|(ci, c)| {
                let rack = d.id;
                find(&c.devices, id, &move |index| DeviceSlot::Rack { rack, chain: ci, index })
            }),
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
    }
}

/// The commands that put `ids` in a new rack `rack` (Cmd+G on devices).
/// The devices must all sit in one chain; the rack takes the place of the
/// first of them and keeps their order. `None` when nothing can be grouped.
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
    let devices: Vec<Device> = picked.iter().map(|&(_, id)| p.device(id).cloned()).collect::<Option<_>>()?;
    let first = devices.first()?;
    let kind = if devices.iter().any(Device::is_instrument) { RackKind::Instrument } else { RackKind::Audio };
    let mut r = Rack::new(kind);
    let mut chain = Chain::new(first.name.clone(), Color::palette(0));
    chain.devices = devices;
    r.chains.push(chain);
    let name = if kind == RackKind::Instrument { "Instrument Rack" } else { "Audio Effect Rack" };
    let plugin_id = if kind == RackKind::Instrument { "rack.instrument" } else { "rack.audio" };
    let device = Device { kind: DeviceKind::Rack(r), ..Device::stock(rack, plugin_id, name) };
    // Removed from the back so the indices before them hold.
    let mut cmds: Vec<Command> = picked.iter().rev().map(|&(_, id)| Command::RemoveDevice { id }).collect();
    cmds.push(Command::AddDevice { slot: home.slot(picked[0].0), device: Box::new(device) });
    Some(cmds)
}

/// Whether `id` is a rack that [`ungroup_commands`] can dissolve: a
/// non-drum rack with a single chain (with more, Live greys it out too).
pub fn can_ungroup(p: &Project, id: DeviceId) -> bool {
    matches!(p.device(id).map(|d| &d.kind), Some(DeviceKind::Rack(r)) if r.kind != RackKind::Drum && r.chains.len() <= 1)
}

/// The commands that dissolve rack `id`: its chain's devices take its place.
pub fn ungroup_commands(p: &Project, id: DeviceId) -> Option<Vec<Command>> {
    if !can_ungroup(p, id) {
        return None;
    }
    let slot = device_slot(p, id)?;
    let DeviceKind::Rack(r) = &p.device(id)?.kind else { return None };
    let at = index_of(slot);
    let home = Container::of(slot);
    let mut cmds = vec![Command::RemoveDevice { id }];
    let inner = r.chains.first().map(|c| c.devices.as_slice()).unwrap_or_default();
    for (i, d) in inner.iter().enumerate() {
        cmds.push(Command::AddDevice { slot: home.slot(at + i), device: Box::new(d.clone()) });
    }
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
