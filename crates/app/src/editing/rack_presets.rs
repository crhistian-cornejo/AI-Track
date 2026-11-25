//! Saving a rack as a preset and adding one to a track. The file format and
//! the capture / instantiate rules are `aitrack_model::rack_preset`; here
//! are the project-side steps: store the live plugin states, and add the
//! rack as one undo step.

#![allow(dead_code)] // TODO(A3.5): drop once the views use it.

use crate::bridge::ExternalPlugin;
use crate::state::AppState;
use aitrack_model::rack_preset::{Missing, RackPreset};
use aitrack_model::*;
use std::path::Path;

/// A rack just added to a track, and what it needs that is not there.
pub struct RackLoad {
    pub device: DeviceId,
    /// In words: "plugin Acme Synth (VST3) is not installed", "sample kick.wav is missing".
    pub missing: Vec<String>,
}

impl AppState {
    /// Store `states` (what the live plugins report) without an undo step,
    /// then the rack as a preset named `name`. Also returns the names of the
    /// hosted plugins in the rack whose state was not among `states`.
    pub fn rack_preset_for_save(&mut self, rack: DeviceId, name: &str, states: Vec<(DeviceId, Vec<u8>)>) -> Result<(RackPreset, Vec<String>), String> {
        let captured: Vec<DeviceId> = states.iter().map(|(d, _)| *d).collect();
        self.capture_plugin_states(states);
        let preset = RackPreset::from_device(&self.project, rack, name, env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
        let mut uncaptured = Vec::new();
        visit_devices(std::slice::from_ref(preset.device()), &mut |d| {
            if matches!(d.kind, DeviceKind::Plugin { .. }) && !captured.contains(&d.id) {
                uncaptured.push(d.name.clone());
            }
        });
        Ok((preset, uncaptured))
    }

    /// Add `preset` at the end of `track`'s chain (a MIDI rack after the MIDI
    /// effects) as one sealed undo step "Add rack". `missing` says, in words,
    /// what it needs that is not installed (`installed`) or not on disk
    /// (`exists`).
    pub fn add_rack_preset(&mut self, track: TrackId, preset: &RackPreset, installed: &[ExternalPlugin], exists: &dyn Fn(&Path) -> bool) -> Option<RackLoad> {
        let plugin_id = if preset.kind == RackKind::Midi { "rack.midi" } else { "rack.audio" };
        let slot = self.insert_slot(track, None, plugin_id)?;
        let device = preset.instantiate(&mut self.project);
        let id = device.id;
        self.seal();
        let ok = self.edit("Add rack", vec![Command::AddDevice { slot, device: Box::new(device) }]);
        self.seal();
        if !ok {
            return None;
        }
        let missing = preset
            .missing(|n| installed.iter().any(|p| p.format == n.format && p.uid == n.uid), exists)
            .iter()
            .map(Missing::to_string)
            .collect();
        Some(RackLoad { device: id, missing })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::racks::Container;

    #[test]
    fn a_rack_saved_from_one_track_loads_on_another_in_one_undo_step_with_its_mapping() {
        let mut s = AppState::new(Project::new("t"));
        let a = s.add_track(TrackKind::Audio).unwrap();
        let rack = s.add_stock_device(a, "rack.audio", "Glue").unwrap();
        let comp = s.add_stock_device_in(a, Some(Container::Rack { rack, chain: 0 }), "fx.compressor", "Comp").unwrap();
        s.map_macro_param(rack, 0, comp, 0, 0.5).unwrap();
        let steps = s.history.steps().count();
        let (preset, uncaptured) = s.rack_preset_for_save(rack, "Glue", Vec::new()).unwrap();
        assert!(uncaptured.is_empty());
        assert_eq!(s.history.steps().count(), steps, "saving is not an edit");
        let b = s.add_track(TrackKind::Audio).unwrap();
        let steps = s.history.steps().count();
        let load = s.add_rack_preset(b, &preset, &[], &|_| true).unwrap();
        assert!(load.missing.is_empty());
        assert_eq!(s.history.steps().count(), steps + 1);
        assert_eq!(s.project.device_track(load.device), Some(b));
        let r = s.rack_of(load.device).unwrap();
        let new_comp = r.chains[0].devices[0].id;
        assert_ne!(new_comp, comp);
        assert_eq!(r.macros[0].mappings[0].device, new_comp);
        s.undo();
        assert!(s.project.device(load.device).is_none());
    }

    #[test]
    fn saving_names_the_plugins_whose_state_was_not_captured_and_loading_names_what_is_missing() {
        let mut s = AppState::new(Project::new("t"));
        let a = s.add_track(TrackKind::Instrument).unwrap();
        let rack = s.add_stock_device(a, "rack.instrument", "Keys").unwrap();
        let into = Some(Container::Rack { rack, chain: 0 });
        let synth = ExternalPlugin { uid: "ABCD".into(), name: "Acme Synth".into(), vendor: "Acme".into(), format: PluginFormat::Vst3, category: "Instrument".into(), path: "/x.vst3".into(), is_instrument: true };
        let plugin = s.add_plugin_device_in(a, into, &synth).unwrap();
        let sampler = s.add_stock_device_in(a, into, "instrument.sampler", "Sampler").unwrap();
        s.edit("state", vec![Command::SetDeviceState { device: sampler, state: Some(aitrack_model::slice::sampler_state(std::path::Path::new("/nowhere/kick.wav"))), preset: None }]);
        let (_, uncaptured) = s.rack_preset_for_save(rack, "Keys", Vec::new()).unwrap();
        assert_eq!(uncaptured, ["Acme Synth"]);
        let (preset, uncaptured) = s.rack_preset_for_save(rack, "Keys", vec![(plugin, vec![9])]).unwrap();
        assert!(uncaptured.is_empty());
        let DeviceKind::Rack(r) = &preset.device().kind else { panic!() };
        assert_eq!(r.chains[0].devices[0].state.as_deref(), Some(&[9u8][..]), "the captured state is in the file");
        let load = s.add_rack_preset(a, &preset, &[], &|_| false).unwrap();
        assert_eq!(load.missing, ["plugin Acme Synth (VST3) is not installed", "sample kick.wav is missing"]);
        assert!(s.add_rack_preset(a, &preset, &[synth], &|_| true).unwrap().missing.is_empty());
    }

    #[test]
    fn a_midi_rack_preset_goes_after_the_midi_effects() {
        let mut s = AppState::new(Project::new("t"));
        let a = s.add_track(TrackKind::Instrument).unwrap();
        let arp = s.add_stock_device(a, "midi.arp", "Arp").unwrap();
        let eq = s.add_stock_device(a, "fx.eq", "EQ").unwrap();
        let rack = s.add_stock_device(a, "rack.midi", "Chords").unwrap();
        let (preset, _) = s.rack_preset_for_save(rack, "Chords", Vec::new()).unwrap();
        let load = s.add_rack_preset(a, &preset, &[], &|_| true).unwrap();
        let order: Vec<DeviceId> = s.project.track(a).unwrap().devices.iter().map(|d| d.id).collect();
        assert_eq!(order.iter().position(|d| *d == load.device), Some(2), "after the arp and the first MIDI rack");
        assert!(order.iter().position(|d| *d == arp) < order.iter().position(|d| *d == load.device));
        assert!(order.iter().position(|d| *d == load.device) < order.iter().position(|d| *d == eq));
    }
}
