//! Generates the factory racks into `crates/stock/racks/<kind>/`.
//! `cargo run -p aitrack-stock --example racks_factory`
//!
//! Each rack is built in a throwaway project with `aitrack-model` commands
//! (the same ones the app uses), captured with `RackPreset::from_device` and
//! written as `.aitrack-rack`. Parameter values are plain (dB, ms, Hz…) and
//! are normalised with the device's own `ParamInfo`, so the files cannot
//! drift from the devices. Running it again changes nothing.

use aitrack_model::command::{ChainProp, DeviceSlot};
use aitrack_model::rack_preset::{RackPreset, kind_slug};
use aitrack_model::racks::fresh_rack;
use aitrack_model::*;
use aitrack_stock::presets::PresetBuilder;
use aitrack_stock::racks::racks_dir;

/// A stock device with plain parameter values.
struct Dev {
    plugin: &'static str,
    name: &'static str,
    values: &'static [(u32, f32)],
}

/// One macro: its name and the `(device index in the chain, param, plain min, plain max)` it sweeps.
struct Mac {
    name: &'static str,
    maps: &'static [(usize, u32, f32, f32)],
}

struct Chain1 {
    name: &'static str,
    volume_db: f32,
    devices: &'static [Dev],
}

struct Spec {
    name: &'static str,
    kind: RackKind,
    tags: &'static [&'static str],
    chains: &'static [Chain1],
    /// Macros map devices of the first chain that has any (index into it).
    macros: &'static [Mac],
}

use aitrack_stock::fx::{compressor, drumbus, filter, limiter};
use aitrack_stock::midi::arp;

const SPECS: &[Spec] = &[
    Spec {
        name: "Parallel Compression",
        kind: RackKind::Audio,
        tags: &["compression", "drums", "parallel"],
        chains: &[
            Chain1 { name: "Dry", volume_db: 0.0, devices: &[] },
            Chain1 {
                name: "Crushed",
                volume_db: -6.0,
                devices: &[Dev {
                    plugin: "fx.compressor",
                    name: "Compressor",
                    values: &[(compressor::p::THRESHOLD, -40.0), (compressor::p::RATIO, 10.0), (compressor::p::ATTACK, 1.0), (compressor::p::RELEASE, 80.0), (compressor::p::MAKEUP, 12.0)],
                }],
            },
        ],
        macros: &[
            Mac { name: "Squash", maps: &[(0, compressor::p::THRESHOLD, -20.0, -50.0)] },
            Mac { name: "Blend", maps: &[(0, compressor::p::OUTPUT, -24.0, 0.0)] },
        ],
    },
    Spec {
        name: "Drum Bus Glue",
        kind: RackKind::Audio,
        tags: &["drums", "bus", "glue", "saturation"],
        chains: &[Chain1 {
            name: "Bus",
            volume_db: 0.0,
            devices: &[
                Dev { plugin: "fx.drumbus", name: "Drum Bus", values: &[(drumbus::p::DRIVE, 30.0), (drumbus::p::CRUNCH, 10.0), (drumbus::p::BOOM_AMOUNT, 20.0), (drumbus::p::COMPRESS, 40.0)] },
                Dev { plugin: "fx.compressor", name: "Compressor", values: &[(compressor::p::THRESHOLD, -18.0), (compressor::p::RATIO, 2.0), (compressor::p::ATTACK, 10.0), (compressor::p::RELEASE, 120.0)] },
                Dev { plugin: "fx.limiter", name: "Limiter", values: &[(limiter::p::CEILING, -1.0)] },
            ],
        }],
        macros: &[
            Mac { name: "Drive", maps: &[(0, drumbus::p::DRIVE, 0.0, 80.0)] },
            Mac { name: "Boom", maps: &[(0, drumbus::p::BOOM_AMOUNT, 0.0, 60.0)] },
            Mac { name: "Crunch", maps: &[(0, drumbus::p::CRUNCH, 0.0, 60.0)] },
            Mac { name: "Glue", maps: &[(1, compressor::p::THRESHOLD, -6.0, -30.0)] },
        ],
    },
    Spec {
        name: "Filter Sweep",
        kind: RackKind::Audio,
        tags: &["filter", "sweep", "performance"],
        chains: &[Chain1 {
            name: "Filter",
            volume_db: 0.0,
            devices: &[Dev { plugin: "fx.filter", name: "Filter", values: &[(filter::p::TYPE, 0.0), (filter::p::CUTOFF, 20000.0), (filter::p::RESONANCE, 20.0)] }],
        }],
        macros: &[
            Mac { name: "Cutoff", maps: &[(0, filter::p::CUTOFF, 200.0, 20000.0)] },
            Mac { name: "Resonance", maps: &[(0, filter::p::RESONANCE, 0.0, 70.0)] },
        ],
    },
    Spec {
        name: "Chord Arp",
        kind: RackKind::Midi,
        tags: &["chords", "arpeggio", "midi"],
        chains: &[Chain1 {
            name: "Chords",
            volume_db: 0.0,
            devices: &[Dev { plugin: "midi.chord", name: "Chord", values: &[] }, Dev { plugin: "midi.arp", name: "Arpeggiator", values: &[(arp::p::GATE, 60.0)] }],
        }],
        macros: &[
            Mac { name: "Gate", maps: &[(1, arp::p::GATE, 10.0, 150.0)] },
            Mac { name: "Swing", maps: &[(1, arp::p::SWING, 0.0, 50.0)] },
            Mac { name: "Octaves", maps: &[(1, arp::p::OCTAVES, 1.0, 4.0)] },
        ],
    },
];

/// The normalised value of `plain` for `param` of the stock device `plugin`.
fn norm(plugin: &str, param: u32, plain: f32) -> f32 {
    let proc = aitrack_stock::create(plugin).unwrap_or_else(|| panic!("no stock device {plugin}"));
    let info = proc.info().params.iter().find(|i| i.id == param).unwrap_or_else(|| panic!("{plugin} has no param {param}")).clone();
    info.to_normalized(plain)
}

fn build(spec: &Spec) -> RackPreset {
    let mut p = Project::new("factory racks");
    let track = p.alloc_track_id();
    Command::AddTrack { track: Box::new(Track::new(track, TrackKind::Audio, "Rack", Color::palette(0))), index: 1 }.apply(&mut p).unwrap();
    let rack = p.alloc_device_id();
    let mut device = Device::stock(rack, format!("rack.{}", kind_slug(spec.kind)), spec.name);
    device.kind = DeviceKind::Rack(fresh_rack(spec.kind));
    Command::AddDevice { slot: DeviceSlot::Track { track, index: 0 }, device: Box::new(device) }.apply(&mut p).unwrap();
    // The rack starts with one empty chain: reuse it for the first.
    let mut ids: Vec<Vec<(DeviceId, &'static str)>> = Vec::new();
    for (ci, c) in spec.chains.iter().enumerate() {
        if ci > 0 {
            Command::AddChain { rack, index: ci, chain: Box::new(aitrack_model::Chain::new(c.name, Color::palette(ci))) }.apply(&mut p).unwrap();
        } else {
            Command::SetChainProp { rack, chain: 0, prop: ChainProp::Name(c.name.into()) }.apply(&mut p).unwrap();
        }
        if c.volume_db != 0.0 {
            Command::SetChainProp { rack, chain: ci, prop: ChainProp::VolumeDb(c.volume_db) }.apply(&mut p).unwrap();
        }
        let mut chain_ids = Vec::new();
        for (di, d) in c.devices.iter().enumerate() {
            let id = p.alloc_device_id();
            let mut b = PresetBuilder::new(d.plugin, "x", "y");
            for &(param, plain) in d.values {
                b = b.set(param, plain);
            }
            let (_, preset) = b.build();
            let mut dev = Device::stock(id, d.plugin, d.name);
            dev.params = preset.params.iter().map(|(k, v)| (k.parse::<u32>().unwrap(), *v)).collect();
            Command::AddDevice { slot: DeviceSlot::Rack { rack, chain: ci, index: di }, device: Box::new(dev) }.apply(&mut p).unwrap();
            chain_ids.push((id, d.plugin));
        }
        ids.push(chain_ids);
    }
    let first = ids.iter().find(|c| !c.is_empty()).expect("a rack with devices");
    for (mi, m) in spec.macros.iter().enumerate() {
        let mappings = m
            .maps
            .iter()
            .map(|&(di, param, lo, hi)| {
                let (device, plugin) = first[di];
                MacroMapping { device, param, min: norm(plugin, param, lo), max: norm(plugin, param, hi) }
            })
            .collect();
        Command::SetMacroDef { rack, index: mi, name: m.name.into(), mappings }.apply(&mut p).unwrap();
    }
    let mut preset = RackPreset::from_device(&p, rack, spec.name, env!("CARGO_PKG_VERSION")).unwrap();
    preset.author = "AI-Track".into();
    preset.tags = spec.tags.iter().map(|t| t.to_string()).collect();
    preset
}

fn main() {
    for spec in SPECS {
        let preset = build(spec);
        let dir = racks_dir().join(kind_slug(spec.kind));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.aitrack-rack", spec.name));
        std::fs::write(&path, preset.to_json().unwrap() + "\n").unwrap();
        println!("wrote {}", path.display());
    }
}
