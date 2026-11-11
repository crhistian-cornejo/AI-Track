//! `instrument.drum` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "instrument.drum";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Drum Sampler", Category::Instrument, &["drums", "sampler"])
}
