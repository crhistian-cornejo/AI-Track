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
}
