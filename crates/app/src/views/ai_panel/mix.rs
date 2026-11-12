//! A mix plan as `Command`s on one track: fader and pan, the stock EQ (low
//! cut in band 1, the moves in bands 2–6) and the stock compressor. Devices
//! already on the track are adjusted; missing ones are added at the end of
//! the chain. The result goes to `History::propose`, never straight to the
//! project.

use aitrack_ai::llm::mix::{BandKind, MixPlan};
use aitrack_model::command::DeviceSlot;
use aitrack_model::{Command, Device, DeviceId, DeviceKind, Project, TrackId, TrackProp};
use aitrack_stock::fx::{compressor, eq};

/// Normalise a plain value through the plugin's own parameter info.
fn normalized(plugin: &str, param: u32, plain: f32) -> Option<f32> {
    let proc = aitrack_stock::create(plugin)?;
    proc.info().params.iter().find(|p| p.id == param).map(|p| p.to_normalized(plain))
}

fn find_stock(p: &Project, track: TrackId, plugin: &str) -> Option<DeviceId> {
    p.track(track)?.devices.iter().find(|d| matches!(&d.kind, DeviceKind::Stock { plugin_id } if plugin_id == plugin)).map(|d| d.id)
}

/// Set `values` on the existing `plugin` device, or add one carrying them.
fn device_moves(p: &mut Project, track: TrackId, plugin: &str, name: &str, values: &[(u32, f32)], out: &mut Vec<Command>) {
    let values: Vec<(u32, f32)> = values.iter().filter_map(|&(id, plain)| normalized(plugin, id, plain).map(|n| (id, n))).collect();
    if values.is_empty() {
        return;
    }
    match find_stock(p, track, plugin) {
        Some(device) => out.extend(values.into_iter().map(|(param, v)| Command::SetParam { device, param, value: Some(v) })),
        None => {
            let index = p.track(track).map_or(0, |t| t.devices.len()) + out.iter().filter(|c| matches!(c, Command::AddDevice { .. })).count();
            let mut d = Device::stock(p.alloc_device_id(), plugin, name);
            d.params.extend(values);
            out.push(Command::AddDevice { slot: DeviceSlot::Track { track, index }, device: Box::new(d) });
        }
    }
}

/// The commands for `plan` on `track` (empty if the plan changes nothing).
pub fn commands(p: &mut Project, track: TrackId, plan: &MixPlan) -> Vec<Command> {
    let mut out = Vec::new();
    if p.track(track).is_none() {
        return out;
    }
    if let Some(v) = plan.volume_db {
        out.push(Command::SetTrack { id: track, change: TrackProp::VolumeDb(v) });
    }
    if let Some(v) = plan.pan {
        out.push(Command::SetTrack { id: track, change: TrackProp::Pan(v) });
    }
    let mut eq_values = Vec::new();
    if let Some(hz) = plan.high_pass_hz {
        eq_values.extend([(eq::p::band(0, eq::p::ON), 1.), (eq::p::band(0, eq::p::TYPE), eq::ty::LOW_CUT), (eq::p::band(0, eq::p::FREQ), hz)]);
    }
    for (i, m) in plan.eq.iter().take(5).enumerate() {
        let b = i + 1;
        let ty = match m.kind {
            BandKind::Bell => eq::ty::BELL,
            BandKind::LowShelf => eq::ty::LOW_SHELF,
            BandKind::HighShelf => eq::ty::HIGH_SHELF,
        };
        eq_values.extend([
            (eq::p::band(b, eq::p::ON), 1.),
            (eq::p::band(b, eq::p::TYPE), ty),
            (eq::p::band(b, eq::p::FREQ), m.freq_hz),
            (eq::p::band(b, eq::p::GAIN), m.gain_db),
            (eq::p::band(b, eq::p::Q), m.q),
        ]);
    }
    device_moves(p, track, eq::ID, "EQ", &eq_values, &mut out);
    if let Some(c) = &plan.compressor {
        let mut v = vec![(compressor::p::THRESHOLD, c.threshold_db), (compressor::p::RATIO, c.ratio)];
        v.extend(c.attack_ms.map(|a| (compressor::p::ATTACK, a)));
        v.extend(c.release_ms.map(|r| (compressor::p::RELEASE, r)));
        device_moves(p, track, compressor::ID, "Compressor", &v, &mut out);
    }
    out
}

/// Short lines for the proposal card.
pub fn summary(plan: &MixPlan) -> Vec<String> {
    let mut s = Vec::new();
    if let Some(v) = plan.volume_db {
        s.push(format!("Fader to {v:+.1} dB"));
    }
    if let Some(v) = plan.pan {
        s.push(if v.abs() < 0.01 { "Pan centre".to_owned() } else { format!("Pan {:.0}{}", v.abs() * 100., if v < 0. { "L" } else { "R" }) });
    }
    if let Some(hz) = plan.high_pass_hz {
        s.push(format!("Low cut at {hz:.0} Hz"));
    }
    for m in &plan.eq {
        let kind = match m.kind {
            BandKind::Bell => "Bell",
            BandKind::LowShelf => "Low shelf",
            BandKind::HighShelf => "High shelf",
        };
        s.push(format!("{kind} {:+.1} dB at {}", m.gain_db, hz(m.freq_hz)));
    }
    if let Some(c) = &plan.compressor {
        s.push(format!("Compress {:.1}:1 from {:.0} dB", c.ratio, c.threshold_db));
    }
    s
}

fn hz(f: f32) -> String {
    if f >= 1_000. { format!("{:.1} kHz", f / 1_000.) } else { format!("{f:.0} Hz") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_ai::llm::mix::{CompMove, EqMove};
    use aitrack_model::{Color, History, Origin, Track, TrackKind};

    fn project() -> (Project, TrackId) {
        let mut p = Project::new("t");
        let id = p.alloc_track_id();
        p.tracks.insert(0, Track::new(id, TrackKind::Audio, "Vox", Color::palette(0)));
        (p, id)
    }

    fn plan() -> MixPlan {
        MixPlan {
            volume_db: Some(-6.),
            pan: Some(0.2),
            high_pass_hz: Some(90.),
            eq: vec![EqMove { kind: BandKind::Bell, freq_hz: 300., gain_db: -2., q: 1.2 }],
            compressor: Some(CompMove { threshold_db: -18., ratio: 3., attack_ms: Some(5.), release_ms: None }),
            notes: vec![],
        }
    }

    #[test]
    fn a_plan_is_one_proposal_that_leaves_the_project_until_accepted() {
        let (mut p, track) = project();
        let cmds = commands(&mut p, track, &plan());
        assert_eq!(cmds.iter().filter(|c| matches!(c, Command::AddDevice { .. })).count(), 2, "EQ and compressor added");
        let mut h = History::default();
        let id = h.propose(&p, "Mix Vox", Origin::Feature("mix".into()), cmds).unwrap();
        assert!(p.track(track).unwrap().devices.is_empty(), "proposing changes nothing");
        assert!(h.accept(&mut p, id).unwrap());
        let t = p.track(track).unwrap();
        assert_eq!((t.volume_db, t.pan), (-6., 0.2));
        assert_eq!(t.devices.len(), 2);
        let eq_dev = &t.devices[0];
        let on = eq_dev.params[&eq::p::band(0, eq::p::ON)];
        assert_eq!(on, 1.);
        let freq = normalized(eq::ID, eq::p::band(0, eq::p::FREQ), 90.).unwrap();
        assert!((eq_dev.params[&eq::p::band(0, eq::p::FREQ)] - freq).abs() < 1e-6);
    }

    #[test]
    fn existing_devices_are_adjusted_not_duplicated() {
        let (mut p, track) = project();
        let dev = p.alloc_device_id();
        p.track_mut(track).unwrap().devices.push(Device::stock(dev, eq::ID, "EQ"));
        let cmds = commands(&mut p, track, &MixPlan { high_pass_hz: Some(40.), ..Default::default() });
        assert!(cmds.iter().all(|c| matches!(c, Command::SetParam { device, .. } if *device == dev)));
        assert_eq!(summary(&plan())[0], "Fader to -6.0 dB");
        assert!(summary(&plan()).contains(&"Bell -2.0 dB at 300 Hz".to_owned()));
    }
}
