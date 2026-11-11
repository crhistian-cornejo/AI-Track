//! `fx.saturator` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.saturator";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Saturator", Category::Effect, &["distortion", "saturation", "multiband"])
}
