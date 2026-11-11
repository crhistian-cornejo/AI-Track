//! `fx.gate` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.gate";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Gate", Category::Effect, &["dynamics", "gate", "expander"])
}
