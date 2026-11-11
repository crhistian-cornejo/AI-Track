//! `fx.limiter` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.limiter";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Limiter", Category::Effect, &["dynamics", "limiter", "master"])
}
