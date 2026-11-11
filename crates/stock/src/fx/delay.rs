//! `fx.delay` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.delay";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Delay", Category::Effect, &["delay", "echo"])
}
