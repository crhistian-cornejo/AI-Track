//! DSP foundation shared by the engine, stock plugins and plugin hosts.
//!
//! The [`Processor`] trait is the one interface every audio node implements:
//! stock instruments and effects, hosted VST3/CLAP plugins (through an
//! adapter in `aitrack-host`) and the engine's own nodes. Processing is
//! block-based and in place on a planar [`AudioBuffer`], with sample-accurate
//! [`Event`]s.
//!
//! Real-time rules for `process`: no allocation, no locks, no I/O, no
//! panics on valid input. `prepare` runs off the audio thread and is where
//! buffers are allocated.

// `next()` on oscillators/envelopes is the DSP idiom; they are not iterators.
#![allow(clippy::should_implement_trait)]

pub mod buffer;
pub mod event;
pub mod param;
pub mod response;

// DSP primitives (allocation only in constructors / `prepare`).
pub mod biquad;
pub mod crossover;
pub mod delay;
pub mod dynamics;
pub mod envelope;
pub mod lfo;
pub mod loudness;
pub mod osc;
pub mod oversample;
pub mod shaper;
pub mod smooth;
pub mod spectrum;
pub mod svf;
pub mod wavetable;

pub use buffer::AudioBuffer;
pub use event::{Event, EventKind};
pub use param::{ParamInfo, ParamScale, ParamValues};

/// Musical transport state for the current block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transport {
    pub playing: bool,
    pub recording: bool,
    pub bpm: f64,
    pub time_sig: (u8, u8),
    /// Song position at the first sample of the block, in quarter notes.
    pub position_beats: f64,
    /// Song position at the first sample of the block, in samples.
    pub position_samples: i64,
    pub loop_range_beats: Option<(f64, f64)>,
}

impl Default for Transport {
    fn default() -> Self {
        Self { playing: false, recording: false, bpm: 120.0, time_sig: (4, 4), position_beats: 0.0, position_samples: 0, loop_range_beats: None }
    }
}

/// Everything a processor sees for one block.
pub struct ProcessContext<'a> {
    pub sample_rate: f64,
    /// Main in/out bus, processed in place. Instruments receive it cleared.
    pub buffer: &'a mut AudioBuffer,
    /// Optional sidechain input.
    pub sidechain: Option<&'a AudioBuffer>,
    /// Sorted by `offset`, all `< buffer.frames()`.
    pub events: &'a [Event],
    pub transport: &'a Transport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Instrument,
    Effect,
    Analyzer,
    Utility,
    MidiEffect,
}

/// Static description of a processor.
#[derive(Clone, Debug)]
pub struct PluginInfo {
    /// Stable id such as `"fx.eq"` or `"instrument.wavetable"`; stored in projects.
    pub id: &'static str,
    pub name: &'static str,
    pub vendor: &'static str,
    pub category: Category,
    /// Short tags for the browser ("eq", "dynamic", "filter"…).
    pub tags: &'static [&'static str],
    pub params: Vec<ParamInfo>,
    pub inputs: u16,
    pub outputs: u16,
    pub has_sidechain: bool,
}

pub trait Processor: Send {
    fn info(&self) -> &PluginInfo;

    /// Called off the audio thread before processing and whenever the sample
    /// rate or maximum block size changes. Allocate here.
    fn prepare(&mut self, sample_rate: f64, max_frames: usize);

    /// Clear internal state (delay lines, envelopes) without reallocating.
    fn reset(&mut self);

    /// Process one block in place.
    fn process(&mut self, ctx: &mut ProcessContext<'_>);

    /// Set a parameter from a normalised value 0..=1.
    fn set_param(&mut self, id: u32, normalized: f32);

    fn get_param(&self, id: u32) -> f32;

    /// Latency in samples introduced by this processor (for PDC).
    fn latency(&self) -> u32 {
        0
    }

    /// Tail length in samples after input stops (reverbs, delays).
    fn tail(&self) -> u32 {
        0
    }

    /// Serialise non-parameter state (sample paths, wavetables…).
    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }

    fn load_state(&mut self, _data: &[u8]) {}

    /// Optional display data for the UI (spectrum, gain reduction…), copied
    /// into a lock-free buffer by the engine. Default: none.
    fn meter(&self, _out: &mut [f32]) -> usize {
        0
    }
}

/// Decibels ↔ linear gain helpers.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -150.0 { 0.0 } else { 10f32.powf(db / 20.0) }
}

#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 1e-8 { -160.0 } else { 20.0 * gain.log10() }
}

/// Flush denormals on the current thread (call once at the top of the audio
/// callback and in each worker).
pub fn disable_denormals() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        #[allow(deprecated)]
        use std::arch::x86_64::{_MM_FLUSH_ZERO_ON, _mm_getcsr, _mm_setcsr};
        // FTZ (bit 15) | DAZ (bit 6)
        #[allow(deprecated)]
        _mm_setcsr(_mm_getcsr() | _MM_FLUSH_ZERO_ON | 0x0040);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let mut fpcr: u64;
        std::arch::asm!("mrs {}, fpcr", out(reg) fpcr);
        std::arch::asm!("msr fpcr, {}", in(reg) fpcr | (1 << 24));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_round_trip() {
        for db in [-60.0f32, -6.0, 0.0, 6.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-4);
        }
    }
}
