//! Device selection in the Device View and racks, as in Live: a click on a
//! title bar picks a device, Cmd (macOS) / Ctrl (Windows) + click adds or
//! removes one, Shift+click picks the run from the last one picked; Cmd+G
//! puts the picked devices in a new rack and Cmd+Shift+G dissolves the
//! picked racks. Picks stay inside one chain. The commands are in
//! `aitrack_model::racks`.

use crate::state::AppState;
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
                if let Command::AddDevice { device, .. } = c {
                    freed.push(device.id);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
