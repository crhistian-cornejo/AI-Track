//! Device selection in the Device View and racks, as in Live: a click on a
//! title bar picks a device, Cmd (macOS) / Ctrl (Windows) + click adds or
//! removes one, Shift+click picks the run from the last one picked; Cmd+G
//! puts the picked devices in a new rack and Cmd+Shift+G dissolves the
//! picked racks. Picks stay inside one chain. The commands are in
//! `aitrack_model::racks`.

use crate::state::AppState;
use aitrack_model::command::ChainProp;
use aitrack_model::racks::{self, Container};
use aitrack_model::*;

impl AppState {
    /// The picked devices that still exist, in chain order.
    pub fn selected_devices(&self) -> Vec<DeviceId> {
        let Some(home) = self
            .ed
            .devices
            .first()
            .and_then(|d| racks::device_slot(&self.project, *d))
            .map(Container::of)
        else {
            return Vec::new();
        };
        let chain = racks::chain(&self.project, home).unwrap_or_default();
        chain
            .iter()
            .filter(|d| self.ed.devices.contains(&d.id))
            .map(|d| d.id)
            .collect()
    }

    pub fn is_device_selected(&self, id: DeviceId) -> bool {
        self.ed.devices.contains(&id)
    }

    /// A click on a device's title bar: `toggle` (Cmd/Ctrl) adds or removes
    /// it, `range` (Shift) picks the run from the last one picked. A device
    /// from another chain starts a new pick.
    pub fn click_device(&mut self, id: DeviceId, toggle: bool, range: bool) {
        self.ed.area = crate::editing::Area::Devices;
        let Some(home) = racks::device_slot(&self.project, id).map(Container::of) else {
            return;
        };
        let same = self
            .ed
            .devices
            .first()
            .and_then(|d| racks::device_slot(&self.project, *d))
            .map(Container::of)
            == Some(home);
        if !same {
            self.ed.devices = vec![id];
            return;
        }
        if range && let Some(&anchor) = self.ed.devices.last() {
            let chain: Vec<DeviceId> = racks::chain(&self.project, home)
                .unwrap_or_default()
                .iter()
                .map(|d| d.id)
                .collect();
            if let (Some(a), Some(b)) = (
                chain.iter().position(|d| *d == anchor),
                chain.iter().position(|d| *d == id),
            ) {
                let mut run: Vec<DeviceId> = chain[a.min(b)..=a.max(b)]
                    .iter()
                    .copied()
                    .filter(|d| *d != anchor)
                    .collect();
                // The anchor stays last, so the next Shift+click extends from it.
                run.push(anchor);
                self.ed.devices = run;
                return;
            }
        }
        if toggle {
            if let Some(i) = self.ed.devices.iter().position(|d| *d == id) {
                if self.ed.devices.len() > 1 {
                    self.ed.devices.remove(i);
                }
            } else {
                self.ed.devices.push(id);
            }
            return;
        }
        self.ed.devices = vec![id];
    }

    /// Cmd+G in the Device View: the picked devices go in a new rack, which
    /// is picked.
    pub fn group_devices(&mut self) -> Option<DeviceId> {
        let ids = self.selected_devices();
        if ids.is_empty() {
            return None;
        }
        let rack = self.project.alloc_device_id();
        let cmds = racks::group_commands(&self.project, &ids, rack)?;
        self.seal();
        let ok = self.edit("Group devices", cmds);
        self.seal();
        ok.then(|| {
            self.ed.devices = vec![rack];
            rack
        })
    }

    /// Fold or unfold a device: one sealed undo step. `false` if it does not
    /// exist.
    pub fn fold_device(&mut self, id: DeviceId) -> bool {
        let Some(d) = self.project.device(id) else {
            return false;
        };
        let collapsed = !d.collapsed;
        self.seal();
        let ok = self.edit(
            if collapsed { "Fold device" } else { "Show device" },
            vec![Command::SetDeviceCollapsed { device: id, collapsed }],
        );
        self.seal();
        ok
    }

    /// Whether Cmd+Shift+G has a rack to dissolve among the picked devices.
    pub fn can_ungroup_devices(&self) -> bool {
        self.selected_devices()
            .iter()
            .any(|d| racks::can_ungroup(&self.project, *d))
    }

    /// Cmd+Shift+G in the Device View: dissolve each picked rack that has a
    /// single chain. Its devices are picked afterwards.
    pub fn ungroup_devices(&mut self) -> bool {
        let targets: Vec<DeviceId> = self
            .selected_devices()
            .into_iter()
            .filter(|d| racks::can_ungroup(&self.project, *d))
            .collect();
        let mut scratch = self.project.clone();
        let mut cmds = Vec::new();
        let mut freed = Vec::new();
        for r in targets {
            let Some(step) = racks::ungroup_commands(&scratch, r) else {
                continue;
            };
            for c in &step {
                if let Command::MoveDevice { id, .. } = c {
                    freed.push(*id);
                }
                if c.clone().apply(&mut scratch).is_err() {
                    return false;
                }
            }
            cmds.extend(step);
        }
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit("Ungroup devices", cmds);
        self.seal();
        if ok {
            self.ed.devices = freed;
        }
        ok
    }
}

// ── Chains, mappings and macros of a rack (the rack's own panel) ──

/// A stable name for the kind of a chain property, to coalesce a drag of one
/// of them and nothing else.
fn prop_tag(prop: &ChainProp) -> String {
    match prop {
        ChainProp::Send(i, _) => format!("send{i}"),
        ChainProp::Name(_) => "name".into(),
        ChainProp::Color(_) => "color".into(),
        ChainProp::VolumeDb(_) => "volume".into(),
        ChainProp::Pan(_) => "pan".into(),
        ChainProp::Mute(_) => "mute".into(),
        ChainProp::Solo(_) => "solo".into(),
        ChainProp::KeyRange(_) => "key".into(),
        ChainProp::VelRange(_) => "vel".into(),
        ChainProp::SelRange(_) => "sel".into(),
        ChainProp::KeyFade(_) => "keyfade".into(),
        ChainProp::VelFade(_) => "velfade".into(),
        ChainProp::SelFade(_) => "selfade".into(),
        ChainProp::Choke(_) => "choke".into(),
        ChainProp::ReceiveNote(_) => "receive".into(),
        ChainProp::PlayNote(_) => "play".into(),
        ChainProp::Sends(_) => "sends".into(),
        ChainProp::Output(_) => "output".into(),
    }
}

impl AppState {
    pub fn rack_of(&self, rack: DeviceId) -> Option<&Rack> {
        match &self.project.device(rack)?.kind {
            DeviceKind::Rack(r) => Some(r),
            _ => None,
        }
    }

    /// One sealed undo step: a property of a chain (a click, a menu).
    pub fn set_chain_prop(&mut self, rack: DeviceId, chain: usize, prop: ChainProp) -> bool {
        self.seal();
        let ok = self.edit("Edit chain", vec![Command::SetChainProp { rack, chain, prop }]);
        self.seal();
        ok
    }

    /// Part of a drag of a chain's volume, pan, zone or send: merges with the
    /// previous frame (the drag ends with `seal`).
    pub fn set_chain_prop_gliding(&mut self, rack: DeviceId, chain: usize, prop: ChainProp) -> bool {
        let key = format!("chain-{}-{chain}-{}", rack.0, prop_tag(&prop));
        self.edit_coalesced(&key, "Edit chain", vec![Command::SetChainProp { rack, chain, prop }])
    }

    /// The same for one of a Drum Rack's returns.
    pub fn set_return_prop(&mut self, rack: DeviceId, index: usize, prop: ChainProp) -> bool {
        self.seal();
        let ok = self.edit("Edit return", vec![Command::SetReturnProp { rack, index, prop }]);
        self.seal();
        ok
    }

    pub fn set_return_prop_gliding(&mut self, rack: DeviceId, index: usize, prop: ChainProp) -> bool {
        let key = format!("return-{}-{index}-{}", rack.0, prop_tag(&prop));
        self.edit_coalesced(&key, "Edit return", vec![Command::SetReturnProp { rack, index, prop }])
    }

    pub fn toggle_chain_mute(&mut self, rack: DeviceId, chain: usize) -> bool {
        let Some(c) = self.rack_of(rack).and_then(|r| r.chains.get(chain)) else { return false };
        self.set_chain_prop(rack, chain, ChainProp::Mute(!c.mute))
    }

    pub fn toggle_chain_solo(&mut self, rack: DeviceId, chain: usize) -> bool {
        let Some(c) = self.rack_of(rack).and_then(|r| r.chains.get(chain)) else { return false };
        self.set_chain_prop(rack, chain, ChainProp::Solo(!c.solo))
    }

    /// A drag of a rack's chain selector (0..=127); the drag ends with `seal`.
    pub fn set_rack_selector(&mut self, rack: DeviceId, value: u8) -> bool {
        self.edit_coalesced(&format!("selector-{}", rack.0), "Chain selector", vec![Command::SetRackSelector { rack, value: value.min(127) }])
    }

    /// Delete a chain (a pad) with its devices: one undo step.
    pub fn remove_chain(&mut self, rack: DeviceId, chain: usize) -> bool {
        if self.rack_of(rack).is_none_or(|r| chain >= r.chains.len()) {
            return false;
        }
        self.seal();
        let ok = self.edit("Delete chain", vec![Command::RemoveChain { rack, index: chain }]);
        self.seal();
        ok
    }

    /// Add an empty return bus to a Drum Rack (at most [`MAX_RETURNS`]).
    pub fn add_return(&mut self, rack: DeviceId) -> bool {
        let Some(r) = self.rack_of(rack).filter(|r| r.kind == RackKind::Drum && r.returns.len() < MAX_RETURNS) else { return false };
        let n = r.returns.len();
        let chain = Chain::new(format!("Return {}", (b'A' + n as u8) as char), Color::palette(n + 5));
        self.seal();
        let ok = self.edit("Add return", vec![Command::AddReturn { rack, index: n, chain: Box::new(chain) }]);
        self.seal();
        ok
    }

    pub fn remove_return(&mut self, rack: DeviceId, index: usize) -> bool {
        if self.rack_of(rack).is_none_or(|r| index >= r.returns.len()) {
            return false;
        }
        self.seal();
        let ok = self.edit("Delete return", vec![Command::RemoveReturn { rack, index }]);
        self.seal();
        ok
    }

    /// Map parameter `param` of `device` to macro `macro_index` of `rack`,
    /// sweeping from where the parameter is now (`current`, normalised) up to
    /// the top (or down to the bottom when it is already near it): one undo
    /// step. `None` when the device is not inside the rack or the parameter is
    /// already mapped to a macro of it. Returns the mapping's index.
    pub fn map_macro_param(&mut self, rack: DeviceId, macro_index: usize, device: DeviceId, param: u32, current: f32) -> Option<usize> {
        let r = self.rack_of(rack)?;
        let m = r.macros.get(macro_index)?;
        if r.macros.iter().any(|m| m.mappings.iter().any(|x| x.device == device && x.param == param)) {
            return None;
        }
        let current = current.clamp(0., 1.);
        let (min, max) = if current > 0.95 { (0., current) } else { (current, 1.) };
        let index = m.mappings.len();
        self.seal();
        let ok = self.edit("Map macro", vec![Command::AddMapping { rack, macro_index, index, mapping: MacroMapping { device, param, min, max } }]);
        self.seal();
        ok.then_some(index)
    }

    pub fn unmap_macro(&mut self, rack: DeviceId, macro_index: usize, index: usize) -> bool {
        self.seal();
        let ok = self.edit("Unmap macro", vec![Command::RemoveMapping { rack, macro_index, index }]);
        self.seal();
        ok
    }

    /// A drag of a mapping's range handles (`min > max` inverts it).
    pub fn set_mapping_range(&mut self, rack: DeviceId, macro_index: usize, index: usize, min: f32, max: f32) -> bool {
        let key = format!("map-{}-{macro_index}-{index}", rack.0);
        self.edit_coalesced(&key, "Macro range", vec![Command::SetMappingRange { rack, macro_index, index, min: min.clamp(0., 1.), max: max.clamp(0., 1.) }])
    }

    /// Set every macro that is not `locked` to a random value, as one undo
    /// step (`seed` makes it repeatable). `false` when there is nothing to change.
    pub fn randomize_macros(&mut self, rack: DeviceId, locked: &std::collections::BTreeSet<usize>, seed: u64) -> bool {
        let Some(r) = self.rack_of(rack) else { return false };
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = || {
            // xorshift64*: plenty for a "surprise me" button.
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
        };
        let cmds: Vec<Command> = (0..r.macros.len()).filter(|i| !locked.contains(i)).map(|index| Command::SetMacro { rack, index, value: next() }).collect();
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit("Randomize macros", cmds);
        self.seal();
        ok
    }

    /// Recall a saved variation, leaving the macros in `kept` as they are.
    pub fn recall_variation_except(&mut self, rack: DeviceId, index: usize, kept: &std::collections::BTreeSet<usize>) -> bool {
        let Some(r) = self.rack_of(rack) else { return false };
        let Some(var) = r.variations.get(index) else { return false };
        let cmds: Vec<Command> = var
            .values
            .iter()
            .enumerate()
            .take(r.macros.len())
            .filter(|(i, _)| !kept.contains(i))
            .map(|(i, &value)| Command::SetMacro { rack, index: i, value })
            .collect();
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit("Recall macro variation", cmds);
        self.seal();
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::command::{ChainProp, DeviceSlot};

    fn state() -> (AppState, Vec<DeviceId>) {
        let mut s = AppState::new(Project::new("t"));
        let t = s.add_track(TrackKind::Audio).unwrap();
        let ids = ["fx.eq", "fx.compressor", "fx.reverb"]
            .iter()
            .filter_map(|id| s.add_stock_device(t, id, id))
            .collect();
        (s, ids)
    }

    #[test]
    fn clicks_pick_devices_and_cmd_g_makes_a_rack() {
        let (mut s, ids) = state();
        s.click_device(ids[2], false, false);
        s.click_device(ids[0], true, false);
        assert_eq!(s.selected_devices(), vec![ids[0], ids[2]]);
        s.click_device(ids[0], false, false);
        s.click_device(ids[1], false, true);
        assert_eq!(s.selected_devices(), vec![ids[0], ids[1]]);
        let before = s.project.tracks.clone();
        let rack = s.group_devices().unwrap();
        assert_eq!(s.selected_devices(), vec![rack]);
        assert!(s.can_ungroup_devices());
        assert!(s.ungroup_devices());
        assert_eq!(s.selected_devices(), vec![ids[0], ids[1]]);
        s.undo();
        s.undo();
        assert_eq!(s.project.tracks, before);
    }

    #[test]
    fn group_fold_and_ungroup_keep_a_macro_mapping_through_undo_and_redo() {
        let (mut s, rack, filter) = rack_with_filter();
        s.map_macro_param(rack, 0, filter, 0, 0.5).unwrap();
        let mapped = |s: &AppState| s.rack_of(rack).unwrap().macros[0].mappings.iter().any(|m| m.device == filter);
        let steps = s.history.steps().count();
        s.click_device(filter, false, false);
        let inner = s.group_devices().unwrap();
        assert!(mapped(&s), "grouping inside the rack keeps the mapping");
        assert!(s.fold_device(inner));
        assert!(s.project.device(inner).unwrap().collapsed && mapped(&s));
        assert!(s.ungroup_devices());
        assert_eq!(s.selected_devices(), vec![filter]);
        assert!(mapped(&s));
        assert_eq!(s.history.steps().count(), steps + 3, "group, fold and ungroup are a step each");
        for _ in 0..3 {
            s.undo();
            assert!(mapped(&s));
        }
        for _ in 0..3 {
            s.redo();
        }
        assert!(mapped(&s) && s.project.device(inner).is_none());
        assert!(!s.fold_device(DeviceId(9999)));
    }

    /// A track with an audio rack of two chains (the fresh one and B), a filter in the first.
    fn rack_with_filter() -> (AppState, DeviceId, DeviceId) {
        let mut s = AppState::new(Project::new("t"));
        let t = s.add_track(TrackKind::Audio).unwrap();
        let rack = s.add_stock_device(t, "rack.audio", "Rack").unwrap();
        let filter = s.project.alloc_device_id();
        s.edit("chains", vec![
            Command::AddChain { rack, index: 1, chain: Box::new(Chain::new("B", Color::palette(2))) },
            Command::AddDevice { slot: DeviceSlot::Rack { rack, chain: 0, index: 0 }, device: Box::new(Device::stock(filter, "fx.filter", "Filter")) },
        ]);
        s.seal();
        (s, rack, filter)
    }

    fn macro_value(s: &AppState, rack: DeviceId, i: usize) -> f32 {
        s.rack_of(rack).unwrap().macros[i].value
    }

    #[test]
    fn a_filter_cutoff_mapped_to_a_macro_sweeps_exactly_the_range_that_was_set_and_undoes_in_one_step() {
        let (mut s, rack, filter) = rack_with_filter();
        let steps = s.history.steps().count();
        // Map the cutoff (param 0, now at 0.5) to macro 1, then set the range 20 %..80 %.
        assert_eq!(s.map_macro_param(rack, 0, filter, 0, 0.5), Some(0));
        assert_eq!(s.history.steps().count(), steps + 1, "mapping is one undo step");
        for (min, max) in [(0.5, 1.0), (0.35, 0.9), (0.2, 0.8)] {
            assert!(s.set_mapping_range(rack, 0, 0, min, max));
        }
        s.seal();
        assert_eq!(s.history.steps().count(), steps + 2, "the drag of the range handles is one step");
        let m = s.rack_of(rack).unwrap().macros[0].mappings[0].clone();
        assert_eq!((m.min, m.max), (0.2, 0.8));
        // What the engine does with a macro value (`min + (max - min) * v`): the macro sweeps 20 %..80 %.
        let swept = |v: f32| m.min + (m.max - m.min) * v;
        assert!((swept(0.0) - 0.2).abs() < 1e-6 && (swept(1.0) - 0.8).abs() < 1e-6 && (swept(0.5) - 0.5).abs() < 1e-6);
        // The same parameter cannot be mapped twice, and a stranger is refused.
        assert_eq!(s.map_macro_param(rack, 1, filter, 0, 0.5), None);
        assert_eq!(s.map_macro_param(rack, 0, DeviceId(9999), 0, 0.5), None);
        // Undo goes back through the range, then the mapping.
        s.undo();
        let m = s.rack_of(rack).unwrap().macros[0].mappings[0].clone();
        assert_eq!((m.min, m.max), (0.5, 1.0));
        s.undo();
        assert!(s.rack_of(rack).unwrap().macros[0].mappings.is_empty());
    }

    #[test]
    fn dragging_one_handle_past_the_other_inverts_the_range_and_a_mapping_can_be_deleted() {
        let (mut s, rack, filter) = rack_with_filter();
        s.map_macro_param(rack, 2, filter, 3, 0.1).unwrap();
        s.set_mapping_range(rack, 2, 0, 0.9, 0.1);
        s.seal();
        let m = s.rack_of(rack).unwrap().macros[2].mappings[0].clone();
        assert!(m.is_inverted() && (m.min + (m.max - m.min) - 0.1).abs() < 1e-6, "macro up, parameter down");
        assert!(s.unmap_macro(rack, 2, 0));
        assert!(s.rack_of(rack).unwrap().macros[2].mappings.is_empty());
        s.undo();
        assert_eq!(s.rack_of(rack).unwrap().macros[2].mappings.len(), 1, "undo brings the mapping back with its range");
    }

    #[test]
    fn rand_sets_the_macros_that_are_not_locked_in_one_undo_step() {
        let (mut s, rack, _) = rack_with_filter();
        let locked: std::collections::BTreeSet<usize> = [0, 5].into();
        s.set_macro(rack, 0, 0.25);
        s.set_macro(rack, 5, 0.75);
        s.seal();
        let before: Vec<f32> = (0..16).map(|i| macro_value(&s, rack, i)).collect();
        let steps = s.history.steps().count();
        assert!(s.randomize_macros(rack, &locked, 42));
        assert_eq!(s.history.steps().count(), steps + 1, "one step for all of them");
        let after: Vec<f32> = (0..16).map(|i| macro_value(&s, rack, i)).collect();
        assert_eq!((after[0], after[5]), (0.25, 0.75), "locked macros stay");
        assert!(after.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!((0..16).filter(|i| !locked.contains(i)).filter(|&i| after[i] != before[i]).count() >= 12, "the rest move");
        // The seed makes it repeatable; another seed gives something else.
        s.undo();
        assert_eq!((0..16).map(|i| macro_value(&s, rack, i)).collect::<Vec<_>>(), before);
        s.randomize_macros(rack, &locked, 42);
        assert_eq!((0..16).map(|i| macro_value(&s, rack, i)).collect::<Vec<_>>(), after);
        s.undo();
        s.randomize_macros(rack, &locked, 43);
        assert_ne!((0..16).map(|i| macro_value(&s, rack, i)).collect::<Vec<_>>(), after);
        let all: std::collections::BTreeSet<usize> = (0..16).collect();
        assert!(!s.randomize_macros(rack, &all, 1), "everything locked: nothing to do");
    }

    #[test]
    fn a_variation_recalls_all_the_macros_but_the_ones_kept() {
        let (mut s, rack, _) = rack_with_filter();
        for (i, v) in [(0, 0.1), (1, 0.2), (2, 0.3)] {
            s.set_macro(rack, i, v);
        }
        s.seal();
        assert!(s.save_rack_variation(rack, None));
        for i in 0..3 {
            s.set_macro(rack, i, 0.9);
        }
        s.seal();
        let kept = [1].into();
        assert!(s.recall_variation_except(rack, 0, &kept));
        assert_eq!((macro_value(&s, rack, 0), macro_value(&s, rack, 1), macro_value(&s, rack, 2)), (0.1, 0.9, 0.3));
        s.undo();
        assert_eq!(macro_value(&s, rack, 0), 0.9, "one undo step");
    }

    #[test]
    fn chain_volume_pan_mute_and_solo_are_commands_with_undo() {
        let (mut s, rack, _) = rack_with_filter();
        let chain = |s: &AppState| s.rack_of(rack).unwrap().chains[1].clone();
        let steps = s.history.steps().count();
        for db in [-3.0, -6.0, -9.0] {
            assert!(s.set_chain_prop_gliding(rack, 1, ChainProp::VolumeDb(db)));
        }
        s.seal();
        assert_eq!((chain(&s).volume_db, s.history.steps().count()), (-9.0, steps + 1), "a drag is one step");
        assert!(s.set_chain_prop(rack, 1, ChainProp::Pan(-0.5)));
        assert!(s.toggle_chain_mute(rack, 1));
        assert!(s.toggle_chain_solo(rack, 1));
        let c = chain(&s);
        assert!(c.mute && c.solo && c.pan == -0.5);
        s.undo();
        s.undo();
        assert!(!chain(&s).mute && !chain(&s).solo);
        s.undo();
        s.undo();
        assert_eq!(chain(&s).volume_db, 0.0);
        assert!(!s.toggle_chain_mute(rack, 9), "no such chain");
    }

    #[test]
    fn a_chain_can_be_deleted_and_a_drum_rack_has_up_to_six_returns() {
        let (mut s, rack, _) = rack_with_filter();
        assert!(s.remove_chain(rack, 1));
        assert_eq!(s.rack_of(rack).unwrap().chains.len(), 1);
        s.undo();
        assert_eq!(s.rack_of(rack).unwrap().chains.len(), 2);
        assert!(!s.add_return(rack), "only a Drum Rack has returns");
        let t = s.add_track(TrackKind::Instrument).unwrap();
        let drum = s.add_stock_device(t, "rack.drum", "Drum").unwrap();
        for i in 0..MAX_RETURNS {
            assert!(s.add_return(drum), "return {i}");
        }
        assert!(!s.add_return(drum));
        assert_eq!(s.rack_of(drum).unwrap().returns[1].name, "Return B");
        assert!(s.remove_return(drum, 0));
        assert_eq!(s.rack_of(drum).unwrap().returns.len(), MAX_RETURNS - 1);
        s.undo();
        assert_eq!(s.rack_of(drum).unwrap().returns.len(), MAX_RETURNS);
    }

    #[test]
    fn dragging_the_chain_selector_is_one_undo_step_and_goes_to_the_engine_live() {
        use crate::bridge::StubEngine;
        let (mut s, rack, _) = rack_with_filter();
        let mut e = StubEngine::default();
        let mut synced = None;
        s.sync_engine(&mut e, &mut synced);
        let (updates, steps) = (e.project_updates, s.history.steps().count());
        for v in 0..=60u8 {
            assert!(s.set_rack_selector(rack, v * 2));
            assert_eq!(s.sync_engine(&mut e, &mut synced), crate::state::Synced::Fast, "frame {v}");
        }
        s.seal();
        assert_eq!(s.rack_of(rack).unwrap().chain_selector, 120);
        assert_eq!((e.project_updates, e.fast_calls), (updates, 61), "the engine gets each value without a project snapshot");
        assert_eq!(s.history.steps().count(), steps + 1, "one undo step for the whole drag");
        s.undo();
        assert_eq!(s.rack_of(rack).unwrap().chain_selector, 0);
        assert!(s.set_rack_selector(rack, 200));
        assert_eq!(s.rack_of(rack).unwrap().chain_selector, 127, "capped at 127");
    }

    #[test]
    fn devices_can_be_added_moved_and_removed_inside_a_drum_rack_return() {
        let mut s = AppState::new(Project::new("t"));
        let t = s.add_track(TrackKind::Instrument).unwrap();
        let drum = s.add_stock_device(t, "rack.drum", "Drum").unwrap();
        assert!(s.add_return(drum));
        let ret = Container::Return { rack: drum, index: 0 };
        let reverb = s.add_stock_device_in(t, Some(ret), "fx.reverb", "Reverb").expect("added to the return");
        let eq = s.add_stock_device_in(t, Some(ret), "fx.eq", "EQ").unwrap();
        let names = |s: &AppState| s.rack_of(drum).unwrap().returns[0].devices.iter().map(|d| d.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&s), ["Reverb", "EQ"]);
        assert_eq!(racks::device_slot(&s.project, eq), Some(DeviceSlot::Return { rack: drum, index: 0, pos: 1 }));
        // Another track's chain is not a place for this track's menu: it falls back to the track.
        let other = s.add_track(TrackKind::Audio).unwrap();
        let stray = s.add_stock_device_in(other, Some(ret), "fx.eq", "Stray").unwrap();
        assert_eq!(s.project.device_track(stray), Some(other));
        // Move EQ in front of the reverb, then a track device into the return, then remove.
        assert!(s.edit("move", vec![Command::MoveDevice { id: eq, to: ret.slot(0) }]));
        assert_eq!(names(&s), ["EQ", "Reverb"]);
        let comp = s.add_stock_device(t, "fx.compressor", "Comp").unwrap();
        assert!(s.edit("move", vec![Command::MoveDevice { id: comp, to: ret.slot(2) }]));
        assert_eq!(names(&s), ["EQ", "Reverb", "Comp"]);
        assert!(s.edit("remove", vec![Command::RemoveDevice { id: reverb }]));
        assert_eq!(names(&s), ["EQ", "Comp"]);
        // Edits inside the return are ordinary commands: a parameter, and undo.
        assert!(s.set_param(eq, 1, 0.3));
        s.seal();
        assert_eq!(s.project.device(eq).unwrap().params.get(&1), Some(&0.3));
        s.undo();
        assert_eq!(s.project.device(eq).unwrap().params.get(&1), None);
        // Cmd+D in the Device View copies a device of a return right after itself.
        s.ed.area = crate::editing::Area::Devices;
        s.ed.devices = vec![eq];
        assert!(s.duplicate_selection());
        assert_eq!(names(&s), ["EQ", "EQ", "Comp"]);
    }
}
