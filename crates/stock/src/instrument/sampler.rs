//! `instrument.sampler` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "instrument.sampler";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Sampler", Category::Instrument, &["sampler"])
}
