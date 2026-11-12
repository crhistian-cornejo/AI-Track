//! The AI-Track project model.
//!
//! Everything here is plain data plus the [`Command`] API that edits it. The
//! UI, scripts and AI agents never mutate a [`Project`] directly: they build
//! commands and hand them to a [`History`], which applies them, records the
//! inverse for undo, and can stage them as a reviewable [`Proposal`].
//!
//! The model has no I/O, no threads and no UI dependency, so the engine can
//! compile a snapshot of it off the audio thread and tests can drive it
//! headlessly.

pub mod command;
pub mod groups;
pub mod history;
pub mod ids;
pub mod ops;
pub mod project;
pub mod scale;
pub mod time;

pub use command::{Command, CommandError, TrackProp};
pub use history::{History, Origin, Proposal, Transaction};
pub use ids::*;
pub use project::*;
pub use scale::{Key, Scale, ScaleKind};
pub use time::{Tick, TempoMap, TimeSignature, PPQ};
