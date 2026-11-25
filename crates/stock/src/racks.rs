//! Factory racks: `racks/<kind>/<Name>.aitrack-rack`, embedded at build time
//! next to the presets. Each file is the JSON of a `RackPreset` of
//! `aitrack-model` (`rack_preset`); this crate does not depend on the model,
//! so it only carries the text, and the app reads it with the model's reader.
//!
//! The files are made with `cargo run -p aitrack-stock --example racks_factory`
//! (see `examples/racks_factory.rs`): they are generated, not written by hand.

use std::path::PathBuf;

include!(concat!(env!("OUT_DIR"), "/factory_racks.rs"));

/// `(path relative to the racks folder, json)` of every factory rack, sorted
/// by path: `audio/Parallel Compression.aitrack-rack`. Empty without a
/// `racks/` folder.
pub fn factory() -> &'static [(&'static str, &'static str)] {
    FACTORY_RACKS
}

/// The folder of the factory racks in the source tree (`crates/stock/racks`),
/// where the generator writes them.
pub fn racks_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("racks")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_racks_sit_in_a_kind_folder() {
        for (rel, json) in factory() {
            let (kind, file) = rel.split_once('/').unwrap_or_else(|| panic!("{rel}"));
            assert!(["audio", "instrument", "midi", "drum"].contains(&kind), "{rel}");
            assert!(file.ends_with(".aitrack-rack") && !json.is_empty(), "{rel}");
        }
        assert!(racks_dir().ends_with("racks"));
    }

    #[test]
    fn every_factory_rack_loads_and_uses_real_stock_devices_and_params() {
        use aitrack_model::rack_preset::{RackPreset, kind_slug};
        use aitrack_model::{DeviceKind, visit_devices};
        let all = factory();
        assert_eq!(all.len(), 4);
        for (rel, json) in all {
            let preset = RackPreset::from_json(json).unwrap_or_else(|e| panic!("{rel}: {e}"));
            assert_eq!(*rel, format!("{}/{}.aitrack-rack", kind_slug(preset.kind), preset.name));
            assert!(preset.requires.plugins.is_empty() && preset.requires.samples.is_empty(), "{rel}");
            let mut inside = Vec::new();
            visit_devices(std::slice::from_ref(preset.device()), &mut |d| {
                inside.push(d.id);
                match &d.kind {
                    DeviceKind::Stock { plugin_id } => {
                        let proc = crate::create(plugin_id).unwrap_or_else(|| panic!("{rel}: no stock device {plugin_id}"));
                        for k in d.params.keys() {
                            assert!(proc.info().params.iter().any(|i| i.id == *k), "{rel}: {plugin_id} has no param {k}");
                        }
                    }
                    DeviceKind::Rack(_) => {}
                    DeviceKind::Plugin { .. } => panic!("{rel}: a factory rack uses only stock devices"),
                }
            });
            let DeviceKind::Rack(r) = &preset.device().kind else { panic!("{rel}") };
            let mapped: Vec<_> = r.macros.iter().flat_map(|m| &m.mappings).collect();
            assert!(!mapped.is_empty(), "{rel}: a factory rack has its macros mapped");
            assert!(mapped.iter().all(|m| inside.contains(&m.device) && (0.0..=1.0).contains(&m.min) && (0.0..=1.0).contains(&m.max)), "{rel}");
        }
    }
}
