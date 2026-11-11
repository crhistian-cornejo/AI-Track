//! `instrument.wavetable` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "instrument.wavetable";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Wavetable Synth", Category::Instrument, &["synth", "wavetable", "polyphonic"])
}
