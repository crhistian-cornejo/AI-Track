//! `fx.chorus` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.chorus";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Chorus", Category::Effect, &["modulation", "chorus", "flanger", "phaser"])
}
