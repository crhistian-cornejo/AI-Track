//! `fx.analyzer` — placeholder until the real implementation lands.

use aitrack_dsp::{Category, Processor};

pub const ID: &str = "fx.analyzer";

pub fn create() -> Box<dyn Processor> {
    crate::stub::stub(ID, "Analyzer", Category::Analyzer, &["analyzer", "meter", "loudness"])
}
