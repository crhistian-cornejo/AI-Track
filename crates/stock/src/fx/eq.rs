//! `fx.eq` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.eq";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "EQ", Category::Effect, &["eq", "filter", "dynamic"])
}
