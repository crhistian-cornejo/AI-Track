//! `fx.compressor` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.compressor";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Compressor", Category::Effect, &["dynamics", "compressor"])
}
