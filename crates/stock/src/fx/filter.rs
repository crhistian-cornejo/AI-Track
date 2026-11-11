//! `fx.filter` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.filter";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Filter", Category::Effect, &["filter", "modulation"])
}
