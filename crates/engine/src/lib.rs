//! Real-time audio engine: graph, transport, audio and MIDI I/O.
//!
//! See `crates/engine/README.md` for the architecture and the public API.
//! In short: the control thread compiles a [`Project`](aitrack_model::Project)
//! snapshot into an immutable-shape [`CompiledGraph`] (topological order,
//! PDC, routing, solo/mute), sends it to the audio thread through a
//! wait-free queue, and drops retired graphs on a graveyard thread. Processor
//! instances survive recompiles through a pool keyed by device id. The audio
//! thread never allocates, locks or does I/O (a test checks it).

pub mod audio_io;
pub mod automation;
pub(crate) mod clip;
pub mod compile;
pub mod delay;
pub mod engine;
pub mod export;
pub mod factory;
pub mod fallback;
pub(crate) mod graph;
pub mod midi_io;
pub mod mp3;
pub mod rt;
pub mod sample_cache;
pub mod shared;
pub mod status;
pub mod tap;
pub mod thread_priority;

#[cfg(test)]
mod alloc_counter;
#[cfg(test)]
mod tests;

pub use audio_io::{ActiveAudio, AudioConfig, AudioDeviceInfo, ChannelSelection, ShareMode};
pub use compile::{compile, CompileContext, CompileError};
pub use engine::{Engine, EngineConfig, EngineHandle};
pub use export::{
    export_mp3, export_stems, export_wav, render, render_offline, render_offline_with, write_wav, Dither, Progress, RenderOptions,
    RenderRange, Rendered, WavFormat,
};
pub use factory::{FactoryStack, NullFactory, ProcessorFactory};
pub use graph::CompiledGraph;
pub use midi_io::MidiPortInfo;
pub use mp3::{Lame, Mp3Tags};
pub use rt::{AudioProcessor, MidiMessage};
pub use sample_cache::{DecodedAudio, SampleCache};
pub use shared::{ProcessorPool, SharedProcessor};
pub use status::{ClipSlotStatus, MeterReading, SlotState};
pub use tap::{AnalysisTap, DeviceMeter, TapReader, DEVICE_METER_LEN, TAP_FRAMES};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Compile(#[from] CompileError),
    #[error("audio device: {0}")]
    Audio(String),
    #[error("MIDI: {0}")]
    Midi(String),
    #[error("export: {0}")]
    Export(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("render cancelled")]
    Cancelled,
    #[error("the engine thread is not running")]
    Disconnected,
}
