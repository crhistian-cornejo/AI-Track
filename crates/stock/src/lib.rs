//! Stock instruments and effects with factory presets.
//!
//! Every plugin implements [`aitrack_dsp::Processor`]: stable parameter ids,
//! latency/tail reporting, JSON state and optional `meter()` data for the
//! UI. Use [`registry`] to list them and [`create`] to instantiate one by
//! id. Factory presets live in [`presets`].

// Index loops over parallel channel/voice arrays are the clearest DSP idiom.
#![allow(clippy::should_implement_trait, clippy::needless_range_loop)]

pub mod common;
pub mod fx;
pub mod instrument;
#[cfg(test)]
mod measure;
pub mod midi;
pub mod presets;
mod stub;
#[doc(hidden)]
pub mod test_util;

use aitrack_dsp::{Category, Processor};

/// A registry entry.
#[derive(Clone, Copy)]
pub struct StockPlugin {
    pub id: &'static str,
    pub name: &'static str,
    pub category: Category,
    pub tags: &'static [&'static str],
    pub create: fn() -> Box<dyn Processor>,
}

impl std::fmt::Debug for StockPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StockPlugin").field("id", &self.id).field("name", &self.name).field("category", &self.category).finish()
    }
}

static REGISTRY: &[StockPlugin] = &[
    StockPlugin { id: fx::eq::ID, name: "EQ", category: Category::Effect, tags: &["eq", "filter", "dynamic", "mid-side"], create: fx::eq::create },
    StockPlugin { id: fx::compressor::ID, name: "Compressor", category: Category::Effect, tags: &["dynamics", "compressor", "sidechain"], create: fx::compressor::create },
    StockPlugin { id: fx::buscomp::ID, name: "Bus Compressor", category: Category::Effect, tags: &["dynamics", "compressor", "bus", "glue", "master"], create: fx::buscomp::create },
    StockPlugin { id: fx::drumbus::ID, name: "Drum Bus", category: Category::Effect, tags: &["drums", "bus", "drive", "compressor", "transient", "boom", "sub"], create: fx::drumbus::create },
    StockPlugin { id: fx::limiter::ID, name: "Limiter", category: Category::Effect, tags: &["dynamics", "limiter", "true peak", "master"], create: fx::limiter::create },
    StockPlugin { id: fx::saturator::ID, name: "Saturator", category: Category::Effect, tags: &["distortion", "saturation", "multiband"], create: fx::saturator::create },
    StockPlugin { id: fx::saturator::tape::ID, name: "Tape", category: Category::Effect, tags: &["saturation", "tape", "hysteresis", "wow", "flutter", "vintage"], create: fx::saturator::tape::create },
    StockPlugin { id: fx::saturator::amp::ID, name: "Amp", category: Category::Effect, tags: &["distortion", "overdrive", "fuzz", "amp", "guitar", "cabinet"], create: fx::saturator::amp::create },
    StockPlugin { id: fx::saturator::console::ID, name: "Console", category: Category::Effect, tags: &["saturation", "console", "summing", "bus", "glue", "mixing"], create: fx::saturator::console::create },
    StockPlugin { id: fx::clipper::ID, name: "Clipper", category: Category::Effect, tags: &["dynamics", "clipper", "distortion", "master"], create: fx::clipper::create },
    StockPlugin { id: fx::multiband::ID, name: "Multiband", category: Category::Effect, tags: &["multiband", "crossover", "utility", "master"], create: fx::multiband::create },
    StockPlugin { id: fx::mbdyn::ID, name: "Multiband Dynamics", category: Category::Effect, tags: &["dynamics", "multiband", "upward", "compressor", "bass", "edm"], create: fx::mbdyn::create },
    StockPlugin { id: fx::midside::ID, name: "Mid/Side", category: Category::Utility, tags: &["mid-side", "stereo", "width", "utility"], create: fx::midside::create },
    StockPlugin { id: fx::reverb::ID, name: "Reverb", category: Category::Effect, tags: &["reverb", "space", "plate", "hall"], create: fx::reverb::create },
    StockPlugin { id: fx::delay::ID, name: "Delay", category: Category::Effect, tags: &["delay", "echo", "ping-pong"], create: fx::delay::create },
    StockPlugin { id: fx::filter::ID, name: "Filter", category: Category::Effect, tags: &["filter", "modulation", "lfo"], create: fx::filter::create },
    StockPlugin { id: fx::gate::ID, name: "Gate", category: Category::Effect, tags: &["dynamics", "gate", "expander"], create: fx::gate::create },
    StockPlugin { id: fx::transient::ID, name: "Transient Shaper", category: Category::Effect, tags: &["dynamics", "transient", "attack"], create: fx::transient::create },
    StockPlugin { id: fx::chorus::ID, name: "Chorus", category: Category::Effect, tags: &["modulation", "chorus", "flanger", "phaser"], create: fx::chorus::create },
    StockPlugin { id: fx::autopan::ID, name: "Auto Pan", category: Category::Effect, tags: &["modulation", "pan", "tremolo", "lfo", "autopan"], create: fx::autopan::create },
    StockPlugin { id: fx::crusher::ID, name: "Crusher", category: Category::Effect, tags: &["distortion", "bitcrusher", "lo-fi", "downsample", "crusher"], create: fx::crusher::create },
    StockPlugin { id: fx::shaper::ID, name: "Shaper", category: Category::Effect, tags: &["dynamics", "sidechain", "pump", "gate", "tremolo", "volume"], create: fx::shaper::create },
    StockPlugin { id: fx::stutter::ID, name: "Stutter", category: Category::Effect, tags: &["glitch", "stutter", "beat repeat", "tape stop", "half-time", "rhythm"], create: fx::stutter::create },
    StockPlugin { id: fx::pitchshift::ID, name: "Pitch Shifter", category: Category::Effect, tags: &["pitch", "transpose", "shifter", "harmony", "shimmer"], create: fx::pitchshift::create },
    StockPlugin { id: fx::freqshift::ID, name: "Frequency Shifter", category: Category::Effect, tags: &["pitch", "frequency shifter", "ring modulator", "single sideband", "inharmonic", "barber pole"], create: fx::freqshift::create },
    StockPlugin { id: fx::vocoder::ID, name: "Vocoder", category: Category::Effect, tags: &["vocoder", "voice", "robot", "talkbox", "sidechain", "formant", "pitch"], create: fx::vocoder::create },
    StockPlugin { id: fx::utility::ID, name: "Utility", category: Category::Utility, tags: &["utility", "gain", "pan", "width", "mono"], create: fx::utility::create },
    StockPlugin { id: fx::lfo::ID, name: "LFO", category: Category::Utility, tags: &["modulation", "lfo", "modulator", "mapping"], create: fx::lfo::create },
    StockPlugin { id: fx::envfollow::ID, name: "Envelope Follower", category: Category::Utility, tags: &["modulation", "envelope", "follower", "modulator", "mapping", "peak controller"], create: fx::envfollow::create },
    StockPlugin { id: fx::analyzer::ID, name: "Analyzer", category: Category::Analyzer, tags: &["analyzer", "spectrum", "loudness", "meter"], create: fx::analyzer::create },
    StockPlugin { id: fx::drum_extract::ID, name: "Drum Extractor", category: Category::Analyzer, tags: &["drums", "oneshot", "loop", "extractor", "de-bleed", "analyzer"], create: fx::drum_extract::create },
    StockPlugin { id: instrument::wavetable::ID, name: "Wavetable Synth", category: Category::Instrument, tags: &["synth", "wavetable", "polyphonic"], create: instrument::wavetable::create },
    StockPlugin { id: instrument::vault::ID, name: "Vault", category: Category::Instrument, tags: &["synth", "analog", "fm", "electric piano", "organ", "strings", "presets", "macros"], create: instrument::vault::create },
    StockPlugin { id: instrument::acid303::ID, name: "Acid 303", category: Category::Instrument, tags: &["synth", "acid", "303", "bass", "mono", "sequencer"], create: instrument::acid303::create },
    StockPlugin { id: instrument::kick808::ID, name: "Kick 808", category: Category::Instrument, tags: &["808", "kick", "sub", "bass", "drums"], create: instrument::kick808::create },
    StockPlugin { id: instrument::sampler::ID, name: "Sampler", category: Category::Instrument, tags: &["sampler", "one-shot", "loop"], create: instrument::sampler::create },
    StockPlugin { id: instrument::drum::ID, name: "Drum Sampler", category: Category::Instrument, tags: &["drums", "sampler", "pads"], create: instrument::drum::create },
    StockPlugin { id: instrument::granular::ID, name: "Granular", category: Category::Instrument, tags: &["granular", "texture", "pad", "freeze", "live", "sampler"], create: instrument::granular::create },
    StockPlugin { id: midi::arp::ID, name: "Arpeggiator", category: Category::MidiEffect, tags: &["midi", "arpeggiator", "pattern", "sequencer"], create: midi::arp::create },
    StockPlugin { id: midi::chord::ID, name: "Chord", category: Category::MidiEffect, tags: &["midi", "chord", "harmony"], create: midi::chord::create },
    StockPlugin { id: midi::scale::ID, name: "Scale", category: Category::MidiEffect, tags: &["midi", "scale", "key", "quantize", "harmony"], create: midi::scale::create },
    StockPlugin { id: midi::velocity::ID, name: "Velocity", category: Category::MidiEffect, tags: &["midi", "velocity", "dynamics", "humanize"], create: midi::velocity::create },
    StockPlugin { id: midi::echo::ID, name: "Note Echo", category: Category::MidiEffect, tags: &["midi", "echo", "delay", "repeat"], create: midi::echo::create },
    StockPlugin { id: midi::pitch::ID, name: "Pitch", category: Category::MidiEffect, tags: &["midi", "transpose", "pitch", "octave"], create: midi::pitch::create },
    StockPlugin { id: midi::length::ID, name: "Note Length", category: Category::MidiEffect, tags: &["midi", "length", "gate", "legato"], create: midi::length::create },
    StockPlugin { id: midi::random::ID, name: "Random", category: Category::MidiEffect, tags: &["midi", "random", "humanize", "variation"], create: midi::random::create },
];

/// Every stock plugin.
pub fn registry() -> &'static [StockPlugin] {
    REGISTRY
}

/// Instantiate a stock plugin by id (`"fx.eq"`, `"instrument.wavetable"`…).
pub fn create(id: &str) -> Option<Box<dyn Processor>> {
    REGISTRY.iter().find(|p| p.id == id).map(|p| (p.create)())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_ids_match_infos() {
        for p in registry() {
            let proc = (p.create)();
            assert_eq!(proc.info().id, p.id);
            assert_eq!(proc.info().category, p.category);
            let mut ids: Vec<u32> = proc.info().params.iter().map(|i| i.id).collect();
            let n = ids.len();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), n, "{}: duplicate param ids", p.id);
        }
        assert!(create("fx.nope").is_none());
    }

    /// `cargo test -p aitrack-stock --lib summary -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn summary() {
        for p in registry() {
            let mut proc = (p.create)();
            proc.prepare(48_000.0, 128);
            println!("{:<22} {:>3} params  latency {:>4}  presets {:>3}", p.id, proc.info().params.len(), proc.latency(), presets::list(p.id).len());
        }
    }
}
