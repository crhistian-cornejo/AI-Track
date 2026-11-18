//! Mapping a modulator (LFO, Envelope Follower) to the parameters of its
//! track: each change is a command of `aitrack_model` with its own undo step,
//! the range of a dragged handle merging into one.

use crate::state::AppState;
use aitrack_model::*;

/// The range a new mapping starts with: a quarter of the parameter's span
/// either side of the knob.
pub const DEFAULT_RANGE: f32 = 0.25;

impl AppState {
    /// Why `target` cannot be mapped to modulator `device` right now (the
    /// words for a toast), `None` when it can.
    pub fn mod_map_error(&self, device: DeviceId, target: ModTarget) -> Option<&'static str> {
        let Some(d) = self.project.device(device).filter(|d| d.is_modulator()) else { return Some("That device is not a modulator") };
        if d.mods.iter().any(|m| m.target == target) {
            return Some("That is already mapped to this modulator");
        }
        if d.mods.len() >= MAX_MOD_MAPPINGS {
            return Some("A modulator drives at most 8 targets");
        }
        if let ModTarget::Param { device: t, .. } = target {
            if t == device {
                return Some("A modulator cannot drive itself");
            }
            if self.project.device(t).is_none() || self.project.device_track(t) != self.project.device_track(device) {
                return Some("A modulator drives the devices of its own track");
            }
        }
        None
    }

    /// Map `target` to modulator `device` with the default range: one undo
    /// step. Returns the mapping's index, or the reason it was refused.
    pub fn map_modulator(&mut self, device: DeviceId, target: ModTarget) -> Result<usize, &'static str> {
        if let Some(why) = self.mod_map_error(device, target) {
            return Err(why);
        }
        let index = self.project.device(device).map_or(0, |d| d.mods.len());
        self.seal();
        let ok = self.edit("Map modulator", vec![Command::AddModMapping { device, index, mapping: ModMapping::bipolar(target, DEFAULT_RANGE) }]);
        self.seal();
        if ok { Ok(index) } else { Err("The mapping could not be added") }
    }

    pub fn unmap_modulator(&mut self, device: DeviceId, index: usize) -> bool {
        self.seal();
        let ok = self.edit("Unmap modulator", vec![Command::RemoveModMapping { device, index }]);
        self.seal();
        ok
    }

    /// A drag of a mapping's range handles (`min > max` inverts it).
    pub fn set_mod_range(&mut self, device: DeviceId, index: usize, min: f32, max: f32) -> bool {
        let key = format!("modrange-{}-{index}", device.0);
        self.edit_coalesced(&key, "Modulator range", vec![Command::SetModRange { device, index, min: min.clamp(-1., 1.), max: max.clamp(-1., 1.) }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A track with an EQ and an LFO, and another track with an EQ.
    fn rig() -> (AppState, DeviceId, DeviceId, DeviceId) {
        let mut s = AppState::new(Project::new("t"));
        let (a, b) = (s.add_track(TrackKind::Audio).unwrap(), s.add_track(TrackKind::Audio).unwrap());
        let eq = s.add_stock_device(a, "fx.eq", "EQ").unwrap();
        let lfo = s.add_stock_device(a, "mod.lfo", "LFO").unwrap();
        let foreign = s.add_stock_device(b, "fx.eq", "EQ").unwrap();
        (s, eq, lfo, foreign)
    }

    #[test]
    fn mapping_is_one_undo_step_each() {
        let (mut s, eq, lfo, foreign) = rig();
        let target = ModTarget::Param { device: eq, param: 3 };
        assert_eq!(s.map_modulator(lfo, target), Ok(0));
        assert_eq!(s.map_modulator(lfo, ModTarget::Volume), Ok(1));
        assert_eq!(s.project.device(lfo).unwrap().mods.len(), 2);
        assert_eq!(s.map_modulator(lfo, target), Err("That is already mapped to this modulator"));
        assert_eq!(s.map_modulator(lfo, ModTarget::Param { device: foreign, param: 0 }), Err("A modulator drives the devices of its own track"));
        assert_eq!(s.map_modulator(eq, ModTarget::Pan), Err("That device is not a modulator"));
        // The range drag merges into a single undo step.
        s.set_mod_range(lfo, 0, -0.5, 0.5);
        s.set_mod_range(lfo, 0, -0.6, 0.7);
        s.seal();
        assert_eq!((s.project.device(lfo).unwrap().mods[0].min, s.project.device(lfo).unwrap().mods[0].max), (-0.6, 0.7));
        assert!(s.undo());
        assert_eq!((s.project.device(lfo).unwrap().mods[0].min, s.project.device(lfo).unwrap().mods[0].max), (-DEFAULT_RANGE, DEFAULT_RANGE));
        assert!(s.undo());
        assert_eq!(s.project.device(lfo).unwrap().mods.len(), 1, "the second mapping is undone");
        assert!(s.unmap_modulator(lfo, 0));
        assert!(s.project.device(lfo).unwrap().mods.is_empty());
        assert!(s.undo());
        assert_eq!(s.project.device(lfo).unwrap().mods.len(), 1, "unmapping can be undone");
    }

    #[test]
    fn at_most_eight_targets() {
        let (mut s, eq, lfo, _) = rig();
        for param in 0..MAX_MOD_MAPPINGS as u32 {
            assert!(s.map_modulator(lfo, ModTarget::Param { device: eq, param }).is_ok());
        }
        assert_eq!(s.map_modulator(lfo, ModTarget::Pan), Err("A modulator drives at most 8 targets"));
    }
}
