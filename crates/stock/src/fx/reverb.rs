//! `fx.reverb` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.reverb";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Reverb", Category::Effect, &["reverb", "space"])
}
