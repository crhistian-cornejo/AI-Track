//! `fx.transient` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.transient";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Transient Shaper", Category::Effect, &["dynamics", "transient"])
}
